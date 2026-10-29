//! Durable device-only import state. A resumable session is an access
//! capability, so these types deliberately do not implement Debug.

use super::*;
use book_inspection::InspectedBook;
use book_model::BookFormat;
use library_replica::filenames;
use serde::{Deserialize, Serialize};
use sync_common::storage::{CloudLibraryRoot, ProviderId};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderUploadDraft {
    pub id: Uuid,
    pub library_id: sync_common::LibraryId,
    pub root: CloudLibraryRoot,
    pub parent: DirId,
    pub file_name: String,
    pub format: BookFormat,
    pub checksum: BookId,
    pub size_bytes: u64,
    #[serde(default)]
    pub inspection: Option<InspectedBook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<crate::transfer::BookTransferState>,
}

impl ProviderUploadDraft {
    pub fn validate(&self) -> Result<(), DatabaseError> {
        filenames::validate_component(&self.file_name).map_err(DatabaseError::operation)?;
        if self.checksum.content_hash().is_none() || self.size_bytes == 0 || self.size_bytes > 9_007_199_254_740_991 {
            return Err(DatabaseError::message("invalid pending provider upload size or checksum"));
        }
        if self.format != BookFormat::Mp3Folder && BookFormat::from_file_name(&self.file_name) != Some(self.format) {
            return Err(DatabaseError::message("pending provider upload format differs from its filename"));
        }
        if self.id != Self::identity(self.parent, &self.file_name, self.checksum) {
            return Err(DatabaseError::message("pending provider upload identity differs from its source"));
        }
        if self.inspection.is_none() && self.transfer.is_none() {
            return Err(DatabaseError::message("pending provider upload has no inspected metadata"));
        }
        if let Some(source) = &self.transfer {
            source.validate_source()?;
            if source.addition.facts.format != self.format
                || source.expected_checksum().is_some_and(|checksum| self.checksum.content_hash() != Some(checksum))
                || source.provider_file.as_ref().is_some_and(|file| file.size_bytes != self.size_bytes)
            {
                return Err(DatabaseError::message("pending provider transfer does not match its verified source"));
            }
        }
        Ok(())
    }

    pub fn identity(parent: DirId, name: &str, checksum: BookId) -> Uuid {
        Uuid::new_v5(&parent, format!("provider-upload/{checksum}/{name}").as_bytes())
    }

    /// Reader edits may advance while an upload is pending. They do not change
    /// the verified source bytes, destination, or resumable upload capability.
    pub fn same_source(&self, other: &Self) -> Result<bool, DatabaseError> {
        let mut previous = self.clone();
        if let (Some(previous), Some(current)) = (&mut previous.transfer, &other.transfer) {
            previous.reading = current.reading.clone();
            previous.chapter_read = current.chapter_read.clone();
            previous.annotations = current.annotations.clone();
        }
        Ok(serde_json::to_vec(&previous).map_err(DatabaseError::operation)? == serde_json::to_vec(other).map_err(DatabaseError::operation)?)
    }
}

pub struct ProviderUpload {
    pub draft: ProviderUploadDraft,
    pub file_id: Option<ProviderId>,
    pub parent_id: Option<ProviderId>,
    pub session: Option<String>,
    pub confirmed_file: Option<ProviderFile>,
}

impl Database {
    pub fn pending_provider_upload_count(&self) -> Result<u64, DatabaseError> {
        let count = self.connection.query_row("SELECT COUNT(*) FROM provider_upload", [], |row| row.get::<_, i64>(0))?;
        u64::try_from(count).map_err(DatabaseError::operation)
    }

    pub fn queue_provider_upload(&self, draft: &ProviderUploadDraft) -> Result<(), DatabaseError> {
        draft.validate()?;
        if self.storage_policy()?.external() != Some(&draft.root) {
            return Err(DatabaseError::message("pending upload belongs to another storage folder"));
        }
        let json = serde_json::to_string(draft).map_err(DatabaseError::operation)?;
        self.write(|tx| {
            let previous: Option<String> = tx.query_row("SELECT draft FROM provider_upload WHERE id=?1", [draft.id.to_string()], |row| row.get(0)).optional()?;
            if let Some(previous) = previous {
                if previous != json {
                    let previous: ProviderUploadDraft = serde_json::from_str(&previous).map_err(DatabaseError::operation)?;
                    if !previous.same_source(draft)? {
                        return Err(DatabaseError::message("pending upload was already queued with different source metadata"));
                    }
                    tx.execute("UPDATE provider_upload SET draft=?1 WHERE id=?2", params![json, draft.id.to_string()])?;
                }
                return Ok(());
            }
            tx.execute("INSERT INTO provider_upload(id,draft) VALUES(?1,?2)", params![draft.id.to_string(), json])?;
            Ok(())
        })
    }

    pub fn provider_upload(&self, id: Uuid) -> Result<Option<ProviderUpload>, DatabaseError> {
        let row: Option<(String, Option<String>, Option<String>, Option<String>, Option<String>)> =
            self.connection.query_row("SELECT draft,file_id,parent_id,session,confirmed_file FROM provider_upload WHERE id=?1", [id.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
        row.map(|(draft, file, parent, session, confirmed)| {
            Ok(ProviderUpload {
                draft: serde_json::from_str(&draft).map_err(DatabaseError::operation)?,
                file_id: file.map(ProviderId::new).transpose().map_err(DatabaseError::operation)?,
                parent_id: parent.map(ProviderId::new).transpose().map_err(DatabaseError::operation)?,
                session,
                confirmed_file: confirmed.map(|json| serde_json::from_str(&json)).transpose().map_err(DatabaseError::operation)?,
            })
        })
        .transpose()
    }

    pub fn provider_upload_ids(&self) -> Result<Vec<Uuid>, DatabaseError> {
        let mut statement = self.connection.prepare("SELECT id FROM provider_upload ORDER BY id")?;
        statement.query_map([], |row| row.get::<_, String>(0))?.map(|row| row?.parse().map_err(DatabaseError::operation)).collect()
    }

    /// Reserve the generated file ID once, before creating a remote object.
    pub fn allocate_provider_upload(&self, id: Uuid, file: &ProviderId, parent: &ProviderId) -> Result<bool, DatabaseError> {
        Ok(self.connection.execute("UPDATE provider_upload SET file_id=?1,parent_id=?2 WHERE id=?3 AND file_id IS NULL AND confirmed_file IS NULL", params![file.as_str(), parent.as_str(), id.to_string()])? == 1)
    }

    pub fn set_provider_upload_session(&self, id: Uuid, file: &ProviderId, session: Option<&str>) -> Result<bool, DatabaseError> {
        Ok(self.connection.execute("UPDATE provider_upload SET session=?1 WHERE id=?2 AND file_id=?3 AND confirmed_file IS NULL", params![session, id.to_string(), file.as_str()])? == 1)
    }

    pub fn confirm_provider_upload(&self, id: Uuid, file: &ProviderFile) -> Result<bool, DatabaseError> {
        file.validate().map_err(DatabaseError::operation)?;
        self.write(|tx| {
            let row: Option<(String, Option<String>)> = tx.query_row("SELECT draft,file_id FROM provider_upload WHERE id=?1", [id.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            let Some((draft, expected_id)) = row else { return Ok(false) };
            let draft: ProviderUploadDraft = serde_json::from_str(&draft).map_err(DatabaseError::operation)?;
            if expected_id.as_deref() != Some(file.identity.file_id.as_str()) || file.identity.scope != draft.root.scope || file.size_bytes != draft.size_bytes || file.content_hash != draft.checksum.content_hash() {
                return Err(DatabaseError::message("provider upload completion differs from its reserved file"));
            }
            tx.execute("UPDATE provider_upload SET confirmed_file=?1,session=NULL WHERE id=?2", params![serde_json::to_string(file).map_err(DatabaseError::operation)?, id.to_string()])?;
            Ok(true)
        })
    }

    pub fn finish_provider_upload(&self, id: Uuid) -> Result<(), DatabaseError> {
        self.connection.execute("DELETE FROM provider_upload WHERE id=?1 AND confirmed_file IS NOT NULL", [id.to_string()])?;
        Ok(())
    }
}
