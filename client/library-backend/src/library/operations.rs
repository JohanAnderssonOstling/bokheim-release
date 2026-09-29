//! Multi-resource library workflows implemented on the public library handle.
use super::filenames;
use super::LibrarySession;
use crate::asset_workflow::AssetPlacementWorkflow;
use crate::BackendError;
use crate::{BlobKind, ContentHash, DownloadState};
use book_model::BookFormat;
use library_model::Dir;
use std::collections::HashMap;
use std::io::Read;
use sync_common::DirId;

pub(super) struct ValidatedImport {
    pub(super) parent_id: DirId,
    pub(super) file_name: String,
    pub(super) format: BookFormat,
}

fn validate_import_destination(db: &library_database::Database, parent_id: &DirId) -> Result<(), BackendError> {
    if !db.is_live_destination(parent_id).map_err(BackendError::operation)? {
        return Err(format!("library folder does not exist: {parent_id}").into());
    }
    Ok(())
}

impl LibrarySession {
    pub(super) fn persistent_download_state(&self, content_hash: ContentHash) -> Result<DownloadState, BackendError> {
        let bytes_present = self.assets.exists(BlobKind::Book, &content_hash).map_err(BackendError::operation)?;
        self.db.transfer_download_state(&content_hash, bytes_present).map_err(BackendError::operation)
    }

    pub(crate) async fn thumbnail(&self, content_hash: ContentHash, resolution: crate::ThumbnailResolution) -> Result<Option<Vec<u8>>, BackendError> {
        let bytes = match resolution {
            crate::ThumbnailResolution::Browse => self.assets.read_thumbnail_variant(&content_hash, resolution.width()).await.map_err(BackendError::operation)?,
            crate::ThumbnailResolution::HighDensity => self.assets.read_bytes(BlobKind::Thumbnail, &content_hash).await.map_err(BackendError::operation)?,
        };
        if bytes.is_none() && self.assets.exists(BlobKind::Book, &content_hash).map_err(BackendError::operation)? && self.db.thumbnail_book_format(content_hash).map_err(BackendError::operation)?.is_some() {
            self.db.request_missing_thumbnail(&content_hash).map_err(BackendError::operation)?;
        }
        Ok(bytes)
    }

    /// Evicts downloaded book files without deleting the library book,
    /// its placements, reading state, or relationships.
    pub(crate) fn remove_local_book_copy(&self, content_hash: ContentHash) -> Result<DownloadState, BackendError> {
        self.assets.evict_local_book(&self.db, &content_hash).map_err(BackendError::operation)?;
        let state = self.persistent_download_state(content_hash)?;
        self.emit(super::events::LibraryEvent::DownloadStatusChanged { content_hash, state: state.clone() });
        self.notify_contents_changed();
        Ok(state)
    }

    pub fn create_directory(&self, parent_id: &DirId, name: &str) -> Result<Dir, BackendError> {
        let name = filenames::validate_component(name.trim())?;
        #[cfg(not(target_arch = "wasm32"))]
        let created = if self.assets.local_access_enabled() {
            let id = DirId::new_v4();
            let (_, path) = self.db.directory_creation_path(parent_id, &name)?;
            let chosen = self.assets.placement().create_directory(&path, id).map_err(BackendError::operation)?;
            let parent = path.rsplit_once('/').map(|(parent, _)| parent);
            let path = parent.map_or_else(|| chosen.clone(), |parent| format!("{parent}/{chosen}"));
            self.db.register_created_directory(&id, parent_id, &chosen, &path)?
        } else {
            self.db.create_directory(parent_id, &name)?
        };
        #[cfg(target_arch = "wasm32")]
        let created = self.db.create_directory(parent_id, &name)?;
        self.notify_contents_changed();
        Ok(created)
    }

    pub(crate) async fn prepare_directory_import(&self, parent_id: &DirId, name: Option<String>, directories: Vec<Vec<String>>) -> Result<super::PreparedDirectoryImport, BackendError> {
        self.prepare_directory_import_job(parent_id, name, directories, None).await
    }

    pub(crate) async fn prepare_directory_import_job(&self, parent_id: &DirId, name: Option<String>, mut directories: Vec<Vec<String>>, job: Option<String>) -> Result<super::PreparedDirectoryImport, BackendError> {
        validate_import_destination(&self.db, parent_id)?;
        if let Some(job) = &job {
            uuid::Uuid::parse_str(job).map_err(BackendError::operation)?;
        }
        let create = |parent: &DirId, name: &str, path: &[String]| -> Result<Dir, BackendError> {
            if let Some(job) = &job {
                let hash = blake3::hash(&serde_json::to_vec(&(job, path)).map_err(BackendError::operation)?);
                let id = DirId::from_bytes(hash.as_bytes()[..16].try_into().unwrap());
                let name = filenames::validate_component(name)?;
                Ok(self.db.create_directory_with_id(&id, parent, &name).map_err(BackendError::operation)?)
            } else {
                self.create_directory(parent, name)
            }
        };
        // Let queued import status reach subscribers before doing SQLite work;
        // large directory trees must not monopolize the executor.
        crate::executor::sleep(std::time::Duration::ZERO).await;
        let root_id = match name {
            Some(name) => create(parent_id, &name, &[])?.id,
            None => *parent_id,
        };
        let mut imported_directories = HashMap::from([(Vec::<String>::new(), root_id)]);
        let mut failures = Vec::new();

        // A parent path sorts before every path that extends it.
        directories.sort_unstable();
        directories.dedup();
        let mut slice_started = web_time::Instant::now();
        for path in directories {
            if slice_started.elapsed() >= std::time::Duration::from_millis(8) {
                crate::executor::sleep(std::time::Duration::ZERO).await;
                slice_started = web_time::Instant::now();
            }
            let Some((name, parent_path)) = path.split_last() else { continue };
            let Some(parent_id) = imported_directories.get(parent_path) else {
                failures.push(import_pipeline::import_workflow::folder_failure(&path, "parent folder could not be imported"));
                continue;
            };
            match create(parent_id, name, &path) {
                Ok(directory) => {
                    imported_directories.insert(path, directory.id);
                }
                Err(error) => failures.push(import_pipeline::import_workflow::folder_failure(&path, &error.to_string())),
            }
        }

        Ok(super::PreparedDirectoryImport { directories: imported_directories.into_iter().collect(), failures })
    }

    pub(super) fn validate_import(&self, parent_id: &DirId, file_name: &str) -> Result<ValidatedImport, BackendError> {
        let file_name = filenames::validate_component(file_name)?;
        let extension = file_name.rsplit_once('.').map(|(_, extension)| extension).unwrap_or_default();
        let format = BookFormat::from_import_extension(extension).ok_or("only EPUB, PDF, DRM-free MOBI/AZW/AZW3, and M4B files can be added to a library")?;
        validate_import_destination(&self.db, parent_id)?;
        Ok(ValidatedImport { parent_id: *parent_id, file_name, format })
    }

    pub(crate) async fn import_book_reader(&self, parent_id: &DirId, source: Box<dyn Read + Send>, file_name: String) -> Result<ContentHash, BackendError> {
        let import = self.validate_import(parent_id, &file_name)?;
        let staged = self.assets.stage_import(&self.db, *parent_id, import.file_name.clone(), source).await.map_err(BackendError::operation)?;
        self.finish_import(import, staged).await
    }

    pub(super) async fn finish_import(&self, import: ValidatedImport, staged: crate::asset_store::StagedBook) -> Result<ContentHash, BackendError> {
        self.finish_import_with_progress(import, staged, None).await
    }

    pub(super) async fn finish_import_with_progress(&self, import: ValidatedImport, staged: crate::asset_store::StagedBook, progress: Option<crate::asset_store::ImportProgressObserver>) -> Result<ContentHash, BackendError> {
        use library_model::ImportFileStage;
        let report = |stage| {
            if let Some(progress) = &progress {
                progress(stage);
            }
        };
        report(ImportFileStage::ReadingMetadata);
        let started = web_time::Instant::now();
        let reuse = import.format != BookFormat::Pdf && self.db.has_import_inspection(&staged.content_hash(), import.format.canonical_extension(), book_inspection::inspection_version(import.format)).map_err(BackendError::operation)?;
        let inspection = if reuse {
            None
        } else {
            let reader = staged.open_reader().map_err(BackendError::operation)?;
            Some(
                cpu_host::submit_book(&*self.cpu, cpu_host::Inspect { source_name: import.file_name.clone(), format: import.format, checksum: Some(staged.checksum()) }, cpu_host::as_cpu_reader(reader))
                    .await
                    .map_err(|error| BackendError::message(format!("invalid {} book: {error}", import.format.canonical_extension().to_ascii_uppercase())))?,
            )
        };
        let content_hash = staged.content_hash();
        log::info!(target: "import_timing", "inspection_finished reused={reuse} ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        let checksum = staged.checksum();
        let size_bytes = staged.size_bytes();
        // Inspection may overlap a delete/move. Do not publish into a folder
        // that became unavailable while CPU or source I/O was in flight.
        validate_import_destination(&self.db, &import.parent_id)?;
        report(ImportFileStage::Publishing);
        let started = web_time::Instant::now();
        let _lease = self.assets.lease_book(&content_hash).await.map_err(|error| BackendError::message(format!("could not lock the imported book: {error}")))?;
        log::info!(target: "import_timing", "book_lease_acquired ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        let started = web_time::Instant::now();
        let prepared = staged.prepare_import().await.map_err(BackendError::operation)?;

        // Book precedes the metadata commit. On a database failure the
        // library file remains discoverable by the next scan.
        let published = self.publish_import(&import, prepared).await?;
        let request_thumbnail = !self.assets.thumbnail_set_exists(&content_hash).map_err(BackendError::operation)?;
        let restore_paths = self.import_restore_paths(&content_hash)?;
        log::info!(target: "import_timing", "publication_finished ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        report(ImportFileStage::AddingToLibrary);
        let started = web_time::Instant::now();
        self.commit_import(import, content_hash, checksum, size_bytes, inspection, published, request_thumbnail, restore_paths)?;
        log::info!(target: "import_timing", "database_commit_finished ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        drop(_lease);
        Ok(content_hash)
    }

    /// Filesystem book from database snapshots, without holding the
    /// writer. Staging already prepared the destination once; this second
    /// pass covers a concurrent rename before the bytes move.
    #[cfg(not(target_arch = "wasm32"))]
    async fn publish_import(&self, import: &ValidatedImport, prepared: crate::asset_store::PreparedImport) -> Result<library_database::PublishedImport, BackendError> {
        self.assets.placement().prepare_directory_with_database(&self.db, import.parent_id).await.map_err(|error| BackendError::message(format!("could not prepare the destination library folder for book: {error}")))?;
        let relative = self.db.native_book_path(import.parent_id, &import.file_name).map_err(BackendError::operation)?.ok_or_else(|| BackendError::message("prepared directory no longer exists"))?;
        let occupied = self.db.shared_occupied_file_names(&import.parent_id).map_err(BackendError::operation)?;
        let staged = prepared.publish_snapshot(&self.assets, &relative, &occupied).map_err(|error| BackendError::message(format!("could not publish the imported book: {error}")))?;
        Ok(library_database::PublishedImport { name: staged.name, relative_path: staged.relative_path, published: staged.published, fingerprint: staged.fingerprint })
    }

    /// Browser imports commit bytes during preparation; there is no placement
    /// filesystem to publish to, so the commit records the validated name.
    #[cfg(target_arch = "wasm32")]
    async fn publish_import(&self, import: &ValidatedImport, prepared: crate::asset_store::PreparedImport) -> Result<library_database::PublishedImport, BackendError> {
        let _ = prepared;
        Ok(library_database::PublishedImport { name: import.file_name.clone(), relative_path: String::new(), published: true, fingerprint: None })
    }

    /// Placement rows whose remembered path disagrees with storage, resolved
    /// from a snapshot so no filesystem check holds the writer.
    #[cfg(not(target_arch = "wasm32"))]
    fn import_restore_paths(&self, content_hash: &ContentHash) -> Result<Vec<String>, BackendError> {
        let candidates = self.db.import_restore_candidates(content_hash).map_err(BackendError::operation)?;
        Ok(candidates.into_iter().filter(|(path, materialized)| materialized.as_deref() != Some(path.as_str()) || !self.assets.placement_file_exists(path)).map(|(path, _)| path).collect())
    }

    #[cfg(target_arch = "wasm32")]
    fn import_restore_paths(&self, _content_hash: &ContentHash) -> Result<Vec<String>, BackendError> {
        Ok(Vec::new())
    }

    /// Committing the metadata transaction is the import's success point.
    /// Resolve all fallible result data before it, and notify before optional work.
    fn commit_import(
        &self, import: ValidatedImport, content_hash: ContentHash, checksum: ContentHash, size_bytes: u64, inspection: Option<book_metadata::InspectedBook>, published: library_database::PublishedImport, request_thumbnail: bool,
        restore_paths: Vec<String>,
    ) -> Result<(), BackendError> {
        let ValidatedImport { parent_id, file_name, format } = import;
        self.db
            .commit_import(library_database::ImportCommit {
                parent_id,
                file_name,
                format,
                content_hash,
                checksum,
                size_bytes,
                inspection_version: book_inspection::inspection_version(format),
                inspection,
                published,
                request_thumbnail,
                restore_paths,
            })
            .map_err(BackendError::operation)?;
        self.notify_contents_changed();
        self.notify_storage_changed();
        Ok(())
    }
}
