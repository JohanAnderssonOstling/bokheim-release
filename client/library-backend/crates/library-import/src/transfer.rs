//! Verified imports preserve reader state across storage identities.
use crate::LibraryImport;
use book_model::BookFormat;
use client_runtime::BackendError;
use library_database::transfer::{BookTransferState, ImportedBook};
#[cfg(not(target_arch = "wasm32"))]
use library_placement::stage_import;
use sync_common::DirId;

// A metadata-only placement may still be named "Untitled". Keep its known
// format recognizable when publishing it as a file in provider storage.
fn transfer_file_name(name: &str, format: BookFormat) -> Result<String, BackendError> {
    let name = library_replica::filenames::validate_component(name)?;
    if format == BookFormat::Mp3Folder || BookFormat::from_file_name(&name) == Some(format) {
        return Ok(name);
    }
    let requested = format!("{}.{}", name, format.canonical_extension());
    Ok(library_replica::filenames::validate_component(&requested).unwrap_or_else(|_| library_replica::numbered_file_name(&requested, 1)))
}

impl LibraryImport {
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn import_transferred_book(&self, parent_id: DirId, file_name: String, reader: library_files::BoxedBookReader, state: BookTransferState) -> Result<ImportedBook, BackendError> {
        state.validate_source().map_err(BackendError::operation)?;
        let format = state.addition.facts.format;
        let mut import = self.validate_import(&parent_id, &format!("book.{}", format.canonical_extension()))?;
        let file_name = self.database.transfer_destination_name(&state.content_hash, &parent_id).map_err(BackendError::operation)?.unwrap_or(file_name);
        import.file_name = transfer_file_name(&file_name, format)?;
        let (name, copied) = stage_import(&self.assets, &self.database, parent_id, import.file_name.clone(), reader, Default::default()).await.map_err(BackendError::operation)?;
        import.file_name = name;
        let checksum = state.expected_checksum();
        let expected_size = state.provider_file.as_ref().map(|file| file.size_bytes);
        let staged = copied.verify_transfer_bytes(checksum, expected_size).await.map_err(BackendError::operation)?;
        Box::pin(self.finish_transferred_book(import, staged, state)).await
    }

    async fn finish_transferred_book(&self, mut import: crate::import::ValidatedImport, staged: library_files::asset_store::StagedBook, mut state: BookTransferState) -> Result<ImportedBook, BackendError> {
        let parent_id = import.parent_id;
        let format = state.addition.facts.format;
        state.byte_checksum = staged.content_hash().content_hash();
        if self.database.storage_policy().map_err(BackendError::operation)?.external().is_some() {
            let content_hash = self.finish_provider_transfer(import, staged, state).await?;
            return Ok(ImportedBook { content_hash, created: true });
        }
        let content_hash = staged.content_hash();
        let state = state.retarget(content_hash).map_err(BackendError::operation)?;
        if let Some(name) = self.database.transfer_destination_name(&content_hash, &parent_id).map_err(BackendError::operation)? {
            import.file_name = name;
        }
        let size_bytes = staged.size_bytes();
        crate::import::validate_import_destination(&self.database, &parent_id)?;
        let _lease = self.assets.lease_book(&content_hash).await.map_err(BackendError::operation)?;
        let published = self.publish_import(&import, staged).await?;
        let request_thumbnail = !self.assets.thumbnail_set_exists(&content_hash).map_err(BackendError::operation)?;
        let restore_paths = self.import_restore_paths(&content_hash)?;
        let created = self
            .database
            .commit_transfer_import(
                library_database::ImportCommit {
                    parent_id,
                    file_name: import.file_name,
                    format: state.addition.facts.format,
                    content_hash,
                    size_bytes,
                    inspection_version: book_inspection::inspection_version(format),
                    inspection: None,
                    published,
                    request_thumbnail,
                    restore_paths,
                },
                state,
            )
            .map_err(BackendError::operation)?;
        self.events.notify_contents_changed();
        Ok(ImportedBook { content_hash, created })
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn import_transferred_staged_book(&self, parent_id: DirId, file_name: String, staged: library_files::asset_store::StagedBook, state: BookTransferState) -> Result<ImportedBook, BackendError> {
        state.validate_source().map_err(BackendError::operation)?;
        if state.expected_checksum().is_some_and(|expected| staged.content_hash().content_hash() != Some(expected)) || state.provider_file.as_ref().is_some_and(|file| file.size_bytes != staged.size_bytes()) {
            return Err(BackendError::message("transferred book failed content verification"));
        }
        let mut import = self.validate_import(&parent_id, &format!("book.{}", state.addition.facts.format.canonical_extension()))?;
        import.file_name = transfer_file_name(&file_name, state.addition.facts.format)?;
        Box::pin(self.finish_transferred_book(import, staged, state)).await
    }
}
