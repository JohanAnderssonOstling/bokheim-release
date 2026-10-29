//! Provider selection and confirmed cloud objects, separate from Bokheim asset
//! presence. Credentials and resumable-upload capabilities never enter sync.

use crate::import::IngestionSql;
use crate::sync::apply::registers::{canonical_body, record_local};
use crate::{Database, DatabaseError};
use content_address::BookId;
use library_replica::{BookLifecycleState, ExternalBook, LibraryStateEntry};
use rusqlite::{OptionalExtension, params};
use sync_common::storage::{LibraryStorage, ProviderFile, ProviderId, ProviderThumbnails};
use sync_common::{DirId, StateKey};
mod covers;
mod downloads;
pub use downloads::ProviderDownloadSnapshot;
mod scans;
mod uploads;
pub use scans::ProviderScan;
pub use uploads::{ProviderUpload, ProviderUploadDraft};

impl Database {
    pub fn admit_transferred_provider_book(&self, parent: DirId, file: ProviderFile, source: crate::transfer::BookTransferState) -> Result<BookId, DatabaseError> {
        if source.expected_checksum().is_none() || file.content_hash != source.expected_checksum() {
            return Err(DatabaseError::message("provider transfer has no matching verified byte checksum"));
        }
        let id = file.identity.book_id();
        let mut source = source.retarget(id)?;
        if source.addition.enrichment.is_none() {
            source.addition.enrichment = Some(library_replica::EnrichmentResult::NoMatch);
        }
        let thumbnails = if self.provider_file(&id)?.is_some_and(|previous| previous.revision == file.revision) { self.provider_thumbnails(&id)? } else { None };
        self.admit_external_book(parent, ExternalBook { file, addition: source.addition, thumbnails })
    }
    pub fn remember_provider_directory(&self, directory: DirId, folder: &ProviderId) -> Result<(), DatabaseError> {
        self.connection.execute("INSERT INTO provider_directory(folder_id,directory_id) VALUES(?1,?2) ON CONFLICT(folder_id) DO UPDATE SET directory_id=excluded.directory_id", params![folder.as_str(), directory.to_string()])?;
        Ok(())
    }

    pub fn provider_directory(&self, directory: DirId) -> Result<Option<ProviderId>, DatabaseError> {
        let id: Option<String> = self.connection.query_row("SELECT folder_id FROM provider_directory WHERE directory_id=?1 ORDER BY folder_id LIMIT 1", [directory.to_string()], |row| row.get(0)).optional()?;
        id.map(ProviderId::new).transpose().map_err(DatabaseError::operation)
    }

    pub fn admit_inspected_provider_book(&self, parent: DirId, mut file: ProviderFile, format: book_model::BookFormat, mut inspection: book_inspection::InspectedBook) -> Result<BookId, DatabaseError> {
        let id = file.identity.book_id();
        if let Some(pdf) = &mut inspection.pdf {
            if let Ok(checksum) = pdf.content_hash.parse() {
                file.content_hash = Some(checksum);
            }
            pdf.content_hash = id.to_hex();
        }
        if let Some(audio) = &mut inspection.audiobook {
            for track in &mut audio.tracks {
                track.archive_hash = Some(id);
            }
        }
        let prepared = crate::import::prepare_inspection(&inspection, &id, format)?;
        let added_at: i64 = self.connection.ingestion_added_at(&id, &crate::sync::unix_millis()?, |row| row.get(0)).optional()?.unwrap_or(crate::sync::unix_millis()?);
        let addition = crate::import::provider_addition(prepared, format, added_at)?;
        let thumbnails = if self.provider_file(&id)?.is_some_and(|previous| previous.revision == file.revision) { self.provider_thumbnails(&id)? } else { None };
        self.admit_external_book(parent, ExternalBook { file, addition, thumbnails })
    }
    pub fn provider_remote_books(&self) -> Result<Vec<(BookId, ProviderFile)>, DatabaseError> {
        let mut query = self.connection.prepare("SELECT book_id,file FROM provider_book")?;
        let rows = query.query_map([], |row| Ok((row.get::<_, BookId>(0)?, row.get::<_, String>(1)?)))?;
        rows.map(|row| {
            let (id, json) = row?;
            Ok((id, serde_json::from_str(&json).map_err(DatabaseError::operation)?))
        })
        .collect()
    }

    pub fn provider_cover_books(&self) -> Result<Vec<ProviderFile>, DatabaseError> {
        let mut query = self.connection.prepare("SELECT p.file FROM provider_book p JOIN book b ON b.content_hash=p.book_id WHERE p.available AND EXISTS (SELECT 1 FROM book_dir d WHERE d.book_row_id=b.row_id AND d.deleted_at IS NULL)")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| serde_json::from_str(&row?).map_err(DatabaseError::operation)).collect()
    }

    pub fn mark_provider_unavailable(&self, book: &BookId) -> Result<(), DatabaseError> {
        self.connection.execute("UPDATE provider_book SET available=0 WHERE book_id=?1", [book])?;
        Ok(())
    }

    pub fn confirm_provider_available(&self, book: &BookId) -> Result<(), DatabaseError> {
        self.connection.execute("UPDATE provider_book SET available=1 WHERE book_id=?1 AND NOT available", [book])?;
        Ok(())
    }

    pub fn place_provider_book(&self, book: BookId, parent: DirId) -> Result<(), DatabaseError> {
        self.write(|tx| {
            let mut query = tx.prepare("SELECT d.dir_id FROM book_dir d JOIN book b ON b.row_id=d.book_row_id WHERE b.content_hash=?1 AND d.deleted_at IS NULL")?;
            let previous = query.query_map([&book], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            if previous.iter().any(|directory| directory == &parent.to_string()) && previous.len() == 1 {
                return Ok(());
            }
            for directory in previous {
                let directory: DirId = directory.parse().map_err(DatabaseError::operation)?;
                if directory != parent {
                    record_local(tx, LibraryStateEntry::Placement { dir_id: directory, content_hash: book, present: false, origin_folder_id: None })?;
                }
            }
            record_local(tx, LibraryStateEntry::Placement { dir_id: parent, content_hash: book, present: true, origin_folder_id: None })
        })
    }

    pub fn upsert_provider_directory(&self, id: DirId, parent: DirId, name: String) -> Result<bool, DatabaseError> {
        self.write(|tx| {
            if let Some((bytes, _)) = canonical_body(tx, "directory_lifecycle", &StateKey::Text(id.to_string()), "")? {
                let state: LibraryStateEntry = sync_common::wire::decode(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?;
                if !matches!(state, LibraryStateEntry::DirectoryLifecycle { value: library_replica::DirectoryLifecycleState::Present, .. }) {
                    return Ok(false);
                }
            }
            record_local(tx, LibraryStateEntry::DirectoryName { dir_id: id, name })?;
            record_local(tx, LibraryStateEntry::DirectoryParent { dir_id: id, parent_id: parent })?;
            record_local(tx, LibraryStateEntry::DirectoryLifecycle { dir_id: id, value: library_replica::DirectoryLifecycleState::Present })?;
            Ok(true)
        })
    }
    pub fn storage_policy(&self) -> Result<LibraryStorage, DatabaseError> {
        let json: Option<String> = self.connection.query_row("SELECT configuration FROM library_storage WHERE singleton=1", [], |row| row.get(0)).optional()?;
        if let Some(json) = json {
            return serde_json::from_str(&json).map_err(DatabaseError::operation);
        }
        Ok(if self.sync_metadata("cloud_storage_enabled")?.as_deref() == Some("0") { LibraryStorage::Local } else { LibraryStorage::Bokheim })
    }

    pub fn provider_file(&self, book: &BookId) -> Result<Option<ProviderFile>, DatabaseError> {
        let json: Option<String> = self.connection.query_row("SELECT file FROM provider_book WHERE book_id=?1", [book], |row| row.get(0)).optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(DatabaseError::operation)).transpose()
    }

    pub fn provider_thumbnails(&self, book: &BookId) -> Result<Option<ProviderThumbnails>, DatabaseError> {
        let json: Option<String> = self.connection.query_row("SELECT thumbnails FROM provider_book WHERE book_id=?1", [book], |row| row.get(0)).optional()?.flatten();
        json.map(|json| serde_json::from_str(&json).map_err(DatabaseError::operation)).transpose()
    }

    /// Called only with a complete provider object: either a scanned file or a
    /// successfully committed upload. File bytes are never sent to Bokheim.
    pub fn admit_external_book(&self, parent: DirId, value: ExternalBook) -> Result<BookId, DatabaseError> {
        let storage = self.storage_policy()?;
        let root = storage.external().ok_or_else(|| DatabaseError::message("library does not use external storage"))?;
        if value.file.identity.scope != root.scope {
            return Err(DatabaseError::message("cloud file belongs to another provider account or drive"));
        }
        let book = value.file.identity.book_id();
        self.write(|tx| {
            if crate::placements::directory_relative_path(tx, &parent)?.is_none() {
                return Err(DatabaseError::message("library folder is unavailable"));
            }
            tx.ingestion_ensure_book(&book)?;
            if canonical_body(tx, "book_lifecycle", &StateKey::Book(book), "")?.is_none() {
                record_local(tx, LibraryStateEntry::BookLifecycle { content_hash: book, value: BookLifecycleState::Present })?;
            }
            record_local(tx, LibraryStateEntry::ExternalBook { content_hash: book, value })?;
            if canonical_body(tx, "placement", &StateKey::Book(book), &parent.to_string())?.is_none() {
                record_local(tx, LibraryStateEntry::Placement { dir_id: parent, content_hash: book, present: true, origin_folder_id: None })?;
            }
            Ok(())
        })?;
        Ok(book)
    }
}

pub(crate) fn project_provider_file(tx: &rusqlite::Transaction<'_>, book: &BookId, file: &ProviderFile, thumbnails: Option<&ProviderThumbnails>) -> Result<(), DatabaseError> {
    file.validate().map_err(DatabaseError::operation)?;
    if let Some(thumbnails) = thumbnails {
        thumbnails.validate_for(&file.identity.scope).map_err(DatabaseError::operation)?;
    }
    let previous: Option<String> = tx.query_row("SELECT file FROM provider_book WHERE book_id=?1", [book], |row| row.get(0)).optional()?;
    if let Some(previous) = previous {
        let previous: ProviderFile = serde_json::from_str(&previous).map_err(DatabaseError::operation)?;
        if previous.revision != file.revision {
            tx.execute("UPDATE book_dir SET is_downloaded=0 WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", [book])?;
        }
    }
    let file = serde_json::to_string(file).map_err(DatabaseError::operation)?;
    let thumbnails = thumbnails.map(serde_json::to_string).transpose().map_err(DatabaseError::operation)?;
    tx.execute("INSERT INTO provider_book(book_id,file,thumbnails) VALUES(?1,?2,?3) ON CONFLICT(book_id) DO UPDATE SET file=excluded.file,thumbnails=excluded.thumbnails,available=1", params![book, file, thumbnails])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::{BookFormat, BookRecord};
    use library_replica::{BookAddition, BookFacts, EnrichmentResult};
    use sync_common::{
        ROOT_DIR_ID,
        storage::{CloudLibraryRoot, CloudProvider, ProviderFileIdentity, ProviderId, ProviderScope},
    };

    fn external() -> (LibraryStorage, ExternalBook) {
        let scope = ProviderScope { provider: CloudProvider::GoogleDrive, account_id: ProviderId::new("account").unwrap(), container_id: ProviderId::new("drive").unwrap() };
        let root = CloudLibraryRoot { scope: scope.clone(), folder_id: ProviderId::new("folder").unwrap() };
        let file = ProviderFile { identity: ProviderFileIdentity { scope, file_id: ProviderId::new("audio").unwrap() }, revision: ProviderId::new("1").unwrap(), size_bytes: 4096, content_hash: None };
        let addition = BookAddition {
            facts: BookFacts { added_at: 1, format: BookFormat::M4b },
            record: BookRecord { metadata: Default::default(), description: String::new() },
            navigation: None,
            pdf: None,
            recording: None,
            enrichment: Some(EnrichmentResult::NoMatch),
        };
        (LibraryStorage::External(root), ExternalBook { file, addition, thumbnails: None })
    }

    #[test]
    fn confirmed_provider_files_publish_without_bokheim_uploads_or_presence() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let id = db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        assert!(id.is_external());
        assert_eq!(db.provider_file(&id).unwrap(), Some(value.file));
        assert!(db.pending_book_uploads(0, 100).unwrap().is_empty());
        assert!(db.remote_asset_hashes(library_model::BlobKind::Book).unwrap().contains(&id));
        assert_eq!(db.connection.query_row("SELECT COALESCE(SUM(server_has_book),0) FROM book_transfer", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        let publications = db.sync_publishable_mutations().unwrap();
        assert!(publications.iter().any(|publication| matches!(publication.state.value, LibraryStateEntry::ExternalBook { .. })));
        assert!(db.set_asset_storage_policy(true, false).is_err());
        assert_eq!(db.storage_policy().unwrap(), storage);
    }

    #[test]
    fn wrong_account_is_rejected_and_file_revision_updates_preserve_identity() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let id = db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        let mut changed = value.clone();
        changed.file.revision = ProviderId::new("2").unwrap();
        assert_eq!(db.admit_external_book(ROOT_DIR_ID, changed.clone()).unwrap(), id);
        assert_eq!(db.provider_file(&id).unwrap(), Some(changed.file));
        let mut other = value;
        other.file.identity.scope.account_id = ProviderId::new("other").unwrap();
        assert!(db.admit_external_book(ROOT_DIR_ID, other).is_err());
    }

    #[test]
    fn transfers_refuse_to_remove_a_replaced_provider_book_or_its_folder() {
        use crate::transfer::TransferSelection;
        for nested in [false, true] {
            let db = Database::open(":memory:").unwrap();
            db.initialize_library().unwrap();
            let (storage, mut value) = external();
            db.set_storage_policy(&storage, false).unwrap();
            let parent = if nested {
                let id = DirId::new_v4();
                db.upsert_provider_directory(id, ROOT_DIR_ID, "Books".into()).unwrap();
                id
            } else {
                ROOT_DIR_ID
            };
            let id = db.admit_external_book(parent, value.clone()).unwrap();
            let selection = if nested { TransferSelection { folders: vec![parent], books: Vec::new(), source: None } } else { TransferSelection { folders: Vec::new(), books: vec![id], source: Some(parent) } };
            let plan = db.transfer_plan(selection.clone()).unwrap();
            value.file.revision = ProviderId::new("replacement").unwrap();
            db.admit_external_book(parent, value).unwrap();
            assert!(db.remove_transferred_selection(&plan).is_err());
            assert!(db.transfer_destination_name(&id, &parent).unwrap().is_some());
            assert!(db.is_live_destination(&parent).unwrap());
            assert!(db.library_trash().unwrap().books.is_empty());
            let current = db.transfer_plan(selection).unwrap();
            db.remove_transferred_selection(&current).unwrap();
            assert!(db.transfer_destination_name(&id, &parent).unwrap().is_none());
        }
    }

    #[test]
    fn browser_provider_download_requires_a_live_matching_request_and_never_records_bokheim_presence() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let id = db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        assert!(db.provider_download_snapshot(id).unwrap().is_none());
        db.sync_request_book_downloads(&[id], library_model::TransferOrigin::UserInitiated).unwrap();
        let snapshot = db.provider_download_snapshot(id).unwrap().unwrap();
        assert!(db.complete_provider_download(&snapshot).unwrap());
        assert!(db.is_book_downloaded(&id).unwrap());
        assert!(db.book_download_requests(-1, None, 10).unwrap().is_empty());
        assert!(!db.complete_provider_download(&snapshot).unwrap());
        assert!(db.pending_book_uploads(0, 10).unwrap().is_empty());
        assert_eq!(db.connection.query_row("SELECT server_has_book FROM book_transfer WHERE content_hash=?1", [id], |row| row.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn obsolete_browser_download_cannot_replace_a_new_revision_or_settle_a_new_request() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, mut value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let id = db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        db.sync_request_book_downloads(&[id], library_model::TransferOrigin::UserInitiated).unwrap();
        let snapshot = db.provider_download_snapshot(id).unwrap().unwrap();
        value.file.revision = ProviderId::new("new-revision").unwrap();
        db.admit_external_book(ROOT_DIR_ID, value).unwrap();
        assert!(!db.complete_provider_download(&snapshot).unwrap());
        assert!(!db.is_book_downloaded(&id).unwrap());
        let current = db.provider_download_snapshot(id).unwrap().unwrap();
        db.cancel_download(&id).unwrap();
        assert!(!db.complete_provider_download(&current).unwrap());
        db.sync_request_book_downloads(&[id], library_model::TransferOrigin::UserInitiated).unwrap();
        db.connection.execute("UPDATE book_transfer SET book_download_at=book_download_at+100 WHERE content_hash=?1", [id]).unwrap();
        assert!(!db.complete_provider_download(&current).unwrap());
        let current = db.provider_download_snapshot(id).unwrap().unwrap();
        db.trash_book(&id).unwrap();
        assert!(!db.complete_provider_download(&current).unwrap());
        assert!(!db.is_book_downloaded(&id).unwrap());
    }

    #[test]
    fn a_late_provider_download_cannot_mark_a_newer_revision_downloaded() {
        use crate::{NativeBookAcknowledgement, NativeBookCompletion};
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, mut value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let id = db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        let snapshot = db.transfer_snapshot(&[id]).unwrap().placements.into_iter().find_map(|entry| entry.book).unwrap();
        let old = value.file.clone();
        value.file.revision = ProviderId::new("2").unwrap();
        db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        let mut completion = NativeBookCompletion {
            publication: library_model::PublishedStagedBook { name: snapshot.name.clone(), relative_path: snapshot.relative_path.to_string(), published: true, fingerprint: Some("123".into()) },
            snapshot,
            size_bytes: old.size_bytes,
            provider_file: Some(old),
        };
        assert_eq!(db.complete_downloaded_book(completion.clone(), false).unwrap(), NativeBookAcknowledgement::Obsolete);
        assert!(!db.is_book_downloaded(&id).unwrap());
        completion.provider_file = None;
        assert_eq!(db.complete_downloaded_book(completion.clone(), false).unwrap(), NativeBookAcknowledgement::Obsolete);
        completion.provider_file = Some(value.file);
        assert_eq!(db.complete_downloaded_book(completion, false).unwrap(), NativeBookAcknowledgement::Applied);
        assert!(db.is_book_downloaded(&id).unwrap());
    }

    fn covers(file: &ProviderFile) -> ProviderThumbnails {
        let mut full = file.clone();
        full.identity.file_id = ProviderId::new("full-cover").unwrap();
        full.size_bytes = 128;
        let mut browse = full.clone();
        browse.identity.file_id = ProviderId::new("browse-cover").unwrap();
        ProviderThumbnails { full, browse }
    }

    #[test]
    fn thumbnail_pair_publication_preserves_book_and_invalidates_old_cache_generation() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let id = db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        assert_eq!(db.provider_cover_cached(&id).unwrap(), None);
        assert!(db.complete_provider_cover_cache(&value.file, None, true).unwrap());
        assert_eq!(db.provider_cover_cached(&id).unwrap(), Some(true));
        let pair = covers(&value.file);
        assert!(db.publish_provider_thumbnails(&value.file, pair.clone()).unwrap());
        assert_eq!(db.provider_thumbnails(&id).unwrap(), Some(pair.clone()));
        assert_eq!(db.provider_cover_cached(&id).unwrap(), None);
        assert!(!db.complete_provider_cover_cache(&value.file, None, true).unwrap());
        assert!(db.complete_provider_cover_cache(&value.file, Some(&pair), true).unwrap());
        assert!(!db.publish_provider_thumbnails(&value.file, pair.clone()).unwrap());
        assert_eq!(db.provider_file(&id).unwrap(), Some(value.file.clone()));
        assert!(!db.server_has_cover(&id).unwrap());
        let publication = db
            .sync_publishable_mutations()
            .unwrap()
            .into_iter()
            .find_map(|publication| match publication.state.value {
                LibraryStateEntry::ExternalBook { value, .. } => Some(value),
                _ => None,
            })
            .unwrap();
        assert_eq!(publication.addition, value.addition);
        assert_eq!(publication.thumbnails, Some(pair));
    }

    #[test]
    fn obsolete_book_revision_cannot_publish_covers_or_cache_absence() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let id = db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        assert!(db.complete_provider_cover_cache(&value.file, None, false).unwrap());
        assert_eq!(db.provider_cover_cached(&id).unwrap(), Some(false));
        let mut changed = value.clone();
        changed.file.revision = ProviderId::new("new-revision").unwrap();
        db.admit_external_book(ROOT_DIR_ID, changed).unwrap();
        assert_eq!(db.provider_cover_cached(&id).unwrap(), None);
        assert!(!db.complete_provider_cover_cache(&value.file, None, true).unwrap());
        assert!(!db.publish_provider_thumbnails(&value.file, covers(&value.file)).unwrap());
        assert_eq!(db.provider_thumbnails(&id).unwrap(), None);
    }

    #[test]
    fn thumbnail_pair_rejects_cross_account_and_reused_variant_id() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
        let mut pair = covers(&value.file);
        pair.browse.identity.scope.account_id = ProviderId::new("other").unwrap();
        assert!(db.publish_provider_thumbnails(&value.file, pair).is_err());
        let mut pair = covers(&value.file);
        pair.browse.identity = pair.full.identity.clone();
        assert!(db.publish_provider_thumbnails(&value.file, pair).is_err());
    }

    #[test]
    fn provider_pdf_identity_does_not_replace_a_confirmed_byte_checksum() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, mut value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let checksum = blake3::hash(b"provider PDF bytes").into();
        value.file.content_hash = Some(checksum);
        let id = value.file.identity.book_id();
        let inspection = book_inspection::InspectedBook {
            pdf: Some(pdf_view_common::PdfReaderMetadata {
                version: 1,
                content_hash: id.to_hex(),
                pages: vec![pdf_view_common::PdfStoredPage { width: 600.0, height: 800.0, rotation: 0 }],
                skippable_pages: None,
                annotations: Vec::new(),
                dependencies: None,
            }),
            metadata: value.addition.record,
            audiobook: None,
            toc: Vec::new(),
        };
        db.admit_inspected_provider_book(ROOT_DIR_ID, value.file, BookFormat::Pdf, inspection).unwrap();
        assert_eq!(db.provider_file(&id).unwrap().unwrap().content_hash, Some(checksum));
        assert!(db.book_transfer_state(id).unwrap().addition.pdf.unwrap().valid_for(&id.to_hex()));
    }

    #[test]
    fn pending_transfer_checks_provider_digest_even_without_an_explicit_byte_checksum() {
        let (storage, mut value) = external();
        let checksum = BookId::from(blake3::hash(b"source bytes"));
        value.file.content_hash = checksum.content_hash();
        let mut draft = ProviderUploadDraft {
            id: ProviderUploadDraft::identity(ROOT_DIR_ID, "Book.m4b", checksum),
            library_id: DirId::new_v4(),
            root: storage.external().unwrap().clone(),
            parent: ROOT_DIR_ID,
            file_name: "Book.m4b".into(),
            format: BookFormat::M4b,
            checksum,
            size_bytes: value.file.size_bytes,
            inspection: None,
            transfer: Some(crate::transfer::BookTransferState {
                content_hash: value.file.identity.book_id(),
                byte_checksum: None,
                provider_file: Some(value.file),
                addition: value.addition,
                reading: None,
                chapter_read: None,
                annotations: Vec::new(),
            }),
        };
        draft.validate().unwrap();
        draft.checksum = BookId::from(blake3::hash(b"different bytes"));
        draft.id = ProviderUploadDraft::identity(draft.parent, &draft.file_name, draft.checksum);
        assert!(draft.validate().is_err());
        draft.checksum = checksum;
        draft.id = ProviderUploadDraft::identity(draft.parent, &draft.file_name, draft.checksum);
        draft.size_bytes += 1;
        assert!(draft.validate().is_err());
    }

    #[test]
    fn pending_upload_caps_and_metadata_stay_local_until_provider_commit() {
        use super::ProviderUploadDraft;
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let (storage, mut value) = external();
        db.set_storage_policy(&storage, false).unwrap();
        let checksum = BookId::from(blake3::hash(b"local source"));
        let draft = ProviderUploadDraft {
            id: ProviderUploadDraft::identity(ROOT_DIR_ID, "Book.m4b", checksum),
            library_id: DirId::new_v4(),
            root: storage.external().unwrap().clone(),
            parent: ROOT_DIR_ID,
            file_name: "Book.m4b".into(),
            format: BookFormat::M4b,
            checksum,
            size_bytes: value.file.size_bytes,
            inspection: Some(book_inspection::InspectedBook { pdf: None, metadata: value.addition.record.clone(), audiobook: None, toc: Vec::new() }),
            transfer: None,
        };
        db.queue_provider_upload(&draft).unwrap();
        db.queue_provider_upload(&draft).unwrap();
        assert_eq!(db.pending_provider_upload_count().unwrap(), 1);
        let parent = &draft.root.folder_id;
        let file_id = &value.file.identity.file_id;
        assert!(db.allocate_provider_upload(draft.id, file_id, parent).unwrap());
        assert!(!db.allocate_provider_upload(draft.id, &ProviderId::new("different").unwrap(), parent).unwrap());
        assert!(db.set_provider_upload_session(draft.id, file_id, Some("private-upload-capability")).unwrap());
        assert!(db.provider_remote_books().unwrap().is_empty());
        assert!(!db.sync_publishable_mutations().unwrap().iter().any(|publication| publication.state.value.book().is_some()));
        db.finish_provider_upload(draft.id).unwrap();
        assert!(db.provider_upload(draft.id).unwrap().is_some());
        assert!(db.confirm_provider_upload(draft.id, &value.file).is_err());
        value.file.content_hash = checksum.content_hash();
        assert!(db.confirm_provider_upload(draft.id, &value.file).unwrap());
        assert!(db.provider_upload(draft.id).unwrap().unwrap().session.is_none());
        assert!(db.provider_remote_books().unwrap().is_empty());
        db.admit_external_book(ROOT_DIR_ID, value).unwrap();
        assert!(db.sync_publishable_mutations().unwrap().iter().any(|publication| matches!(publication.state.value, LibraryStateEntry::ExternalBook { .. })));
        db.finish_provider_upload(draft.id).unwrap();
        assert!(db.provider_upload(draft.id).unwrap().is_none());
        assert_eq!(db.pending_provider_upload_count().unwrap(), 0);
    }
}
