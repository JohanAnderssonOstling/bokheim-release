//! Provider downloads settle against their captured file revision and request.
//! No completion records Bokheim asset presence or creates an upload intent.

use super::*;
use crate::shared_sql::SharedSql;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderDownloadSnapshot {
    pub file: ProviderFile,
    requested_at: i64,
}

impl Database {
    pub fn provider_download_snapshot(&self, id: BookId) -> Result<Option<ProviderDownloadSnapshot>, DatabaseError> {
        let Some(file) = self.provider_file(&id)? else { return Ok(None) };
        let storage = self.storage_policy()?;
        if storage.external().is_none_or(|root| root.scope != file.identity.scope) || file.identity.book_id() != id {
            return Err(DatabaseError::message("provider download belongs to another storage account"));
        }
        let requested_at = self.connection.query_row(
            "SELECT t.book_download_at FROM book_transfer t JOIN book b ON b.content_hash=t.content_hash WHERE t.content_hash=?1 AND t.book_download IS NOT NULL AND b.deleted_at IS NULL AND EXISTS (SELECT 1 FROM book_dir p JOIN dir d ON d.id=p.dir_id WHERE p.book_row_id=b.row_id AND p.deleted_at IS NULL AND d.deleted_at IS NULL)",
            [id], |row| row.get::<_, i64>(0),
        ).optional()?;
        Ok(requested_at.map(|requested_at| ProviderDownloadSnapshot { file, requested_at }))
    }

    /// The caller has committed verified bytes into its browser store. Return
    /// false if the book, provider revision, or request became obsolete; the
    /// caller then discards only the generation it just published.
    pub fn complete_provider_download(&self, snapshot: &ProviderDownloadSnapshot) -> Result<bool, DatabaseError> {
        self.write(|tx| {
            let id = snapshot.file.identity.book_id();
            if self.provider_download_snapshot(id)?.as_ref() != Some(snapshot) {
                return Ok(false);
            }
            let size = i64::try_from(snapshot.file.size_bytes).map_err(DatabaseError::operation)?;
            let updated = tx.execute(
                "UPDATE book_dir SET is_downloaded=1,size_bytes=?1 WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?2 AND deleted_at IS NULL) AND deleted_at IS NULL AND dir_id IN (SELECT id FROM dir WHERE deleted_at IS NULL)",
                params![size, id],
            )?;
            if updated == 0 {
                return Ok(false);
            }
            tx.shared_transfer_complete_download(&id)?;
            Ok(true)
        })
    }
}
