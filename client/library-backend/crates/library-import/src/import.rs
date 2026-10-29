//! The shared import path: validate the destination, prepare folders,
//! inspect the book, publish its bytes, and commit the addition.

use book_model::BookFormat;
use client_runtime::BackendError;
use library_database::ContributorNameMatcher;
use library_files::asset_store::CopiedBook;
use library_replica::filenames;
use sync_common::{BookId, DirId};

use crate::LibraryImport;

pub(crate) struct ValidatedImport {
    pub(crate) parent_id: DirId,
    pub(crate) file_name: String,
    pub(crate) format: BookFormat,
}

pub(crate) fn validate_import_destination(db: &library_database::Database, parent_id: &DirId) -> Result<(), BackendError> {
    if !db.is_live_destination(parent_id).map_err(BackendError::operation)? {
        return Err(format!("library folder does not exist: {parent_id}").into());
    }
    Ok(())
}

impl LibraryImport {
    pub(crate) fn validate_import(&self, parent_id: &DirId, file_name: &str) -> Result<ValidatedImport, BackendError> {
        #[cfg(target_os = "android")]
        if self.database.storage_policy().map_err(BackendError::operation)?.external().is_some_and(|root| root.scope.provider == sync_common::storage::CloudProvider::GoogleDrive) {
            cloud_storage::require_google_drive().map_err(BackendError::operation)?;
        }
        let file_name = filenames::validate_component(file_name)?;
        let format = BookFormat::from_file_name(&file_name).ok_or("only EPUB, PDF, DRM-free MOBI/AZW/AZW3, FB2, CBZ, and M4B files can be added to a library")?;
        validate_import_destination(&self.database, parent_id)?;
        Ok(ValidatedImport { parent_id: *parent_id, file_name, format })
    }

    pub(crate) async fn finish_import(&self, import: ValidatedImport, staged: CopiedBook) -> Result<BookId, BackendError> {
        #[cfg(not(target_arch = "wasm32"))]
        let (staged, inspection) = {
            let ingested = filesystem_scanner::ingestion::ingest_file(staged.path().to_owned(), import.file_name.clone(), &*self.cpu)
                .await
                .map_err(|error| BackendError::operation(error).context(format!("invalid {} book", import.format.canonical_extension().to_ascii_uppercase())))?;
            let inspection = Some(ingested.inspection);
            (staged.into_staged(ingested.content_hash, ingested.size_bytes), inspection)
        };
        #[cfg(target_arch = "wasm32")]
        let inspection = {
            let reuse = self.database.storage_policy().map_err(BackendError::operation)?.external().is_none()
                && import.format != BookFormat::Pdf
                && self.database.has_import_inspection(&staged.content_hash(), import.format, book_inspection::inspection_version(import.format)).map_err(BackendError::operation)?;
            if reuse {
                None
            } else {
                let reader = staged.open_reader().map_err(BackendError::operation)?;
                Some(
                    cpu_host::submit_book(&*self.cpu, cpu_host::Inspect { source_name: import.file_name.clone(), format: import.format, content_hash: Some(staged.content_hash()) }, cpu_host::as_cpu_reader(reader))
                        .await
                        .map_err(|error| error.context(format!("invalid {} book", import.format.canonical_extension().to_ascii_uppercase())))?,
                )
            }
        };
        if self.database.storage_policy().map_err(BackendError::operation)?.external().is_some() {
            return self.finish_provider_import(import, staged, inspection.ok_or_else(|| BackendError::message("provider import inspection is unavailable"))?).await;
        }
        let content_hash = staged.content_hash();
        let size_bytes = staged.size_bytes();
        // Inspection may overlap a delete/move. Do not publish into a folder
        // that became unavailable while CPU or source I/O was in flight.
        validate_import_destination(&self.database, &import.parent_id)?;
        let _lease = self.assets.lease_book(&content_hash).await.map_err(|error| BackendError::operation(error).context("could not lock the imported book"))?;

        // Book precedes the metadata commit. On a database failure the
        // library file remains discoverable by the next scan.
        let published = self.publish_import(&import, staged).await?;
        self.commit_import(import, content_hash, size_bytes, inspection, published)?;
        drop(_lease);
        Ok(content_hash)
    }

    pub(crate) async fn publish_import(&self, import: &ValidatedImport, staged: library_files::asset_store::StagedBook) -> Result<library_model::PublishedStagedBook, BackendError> {
        library_placement::publish_import(&self.assets, &self.database, import.parent_id, &import.file_name, staged, import.format == BookFormat::Mp3Folder).await.map_err(|error| error.context("could not publish the imported book"))
    }

    /// Placement rows whose remembered path disagrees with storage, resolved
    /// from a snapshot so no filesystem check holds the writer.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_restore_paths(&self, content_hash: &BookId) -> Result<Vec<String>, BackendError> {
        let candidates = self.database.import_restore_candidates(content_hash).map_err(BackendError::operation)?;
        Ok(candidates.into_iter().filter(|(path, materialized)| materialized.as_deref() != Some(path.as_str()) || !self.assets.placement_file_exists(path)).map(|(path, _)| path.to_string()).collect())
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn import_restore_paths(&self, _content_hash: &BookId) -> Result<Vec<String>, BackendError> {
        Ok(Vec::new())
    }

    /// Committing the metadata transaction is the import's success point.
    /// Resolve all fallible result data before it, and notify before optional work.
    pub(crate) fn commit_import(&self, import: ValidatedImport, content_hash: BookId, size_bytes: u64, inspection: Option<book_inspection::InspectedBook>, published: library_model::PublishedStagedBook) -> Result<(), BackendError> {
        let request_thumbnail = !self.assets.thumbnail_set_exists(&content_hash).map_err(BackendError::operation)?;
        let restore_paths = self.import_restore_paths(&content_hash)?;
        let ValidatedImport { parent_id, file_name, format } = import;
        let catalog_record = library_database::catalog::import_record(content_hash, format).map_err(BackendError::operation)?;
        self.database
            .commit_import_with_catalog_answer(
                library_database::ImportCommit { parent_id, file_name, format, content_hash, size_bytes, inspection_version: book_inspection::inspection_version(format), inspection, published, request_thumbnail, restore_paths },
                Some(ContributorNameMatcher::Normalized),
                catalog_record,
            )
            .map_err(BackendError::operation)?;
        self.events.notify_contents_changed();
        Ok(())
    }
}
