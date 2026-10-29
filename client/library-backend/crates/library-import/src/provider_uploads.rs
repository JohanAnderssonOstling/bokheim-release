//! Upload first, then publish the provider identity. The retained source and
//! device queue survive restarts independently of Bokheim metadata sync.

use crate::{LibraryImport, import::ValidatedImport};
use book_inspection::InspectedBook;
use client_platform_runtime::executor::run_blocking;
use client_runtime::BackendError;
use cloud_storage::{
    CloudDirectoryIdentity, CloudEntry, CloudError, MAX_RANGE_BYTES,
    connections::ConnectedDrive,
    google_drive::{DriveUpload, UploadProgress},
};
#[cfg(not(target_arch = "wasm32"))]
use library_database::NativeBookCompletion;
use library_database::provider::{ProviderUpload, ProviderUploadDraft};
use library_database::transfer::BookTransferState;
#[cfg(not(target_arch = "wasm32"))]
use library_files::AssetStoreError;
use library_files::asset_store::StagedBook;
use library_model::TransferOrigin;
#[cfg(target_arch = "wasm32")]
use std::io::{Read, Seek};
use std::{collections::HashSet, io::SeekFrom};
use sync_common::{
    BookId, DirId,
    storage::{CloudLibraryRoot, ProviderFile, ProviderId},
};
#[cfg(not(target_arch = "wasm32"))]
use tokio::io::{AsyncReadExt, AsyncSeekExt};
#[cfg(not(target_arch = "wasm32"))]
type UploadSource = tokio::fs::File;
#[cfg(target_arch = "wasm32")]
type UploadSource = client_platform_web::web_storage::FileReader;
use uuid::Uuid;

impl LibraryImport {
    pub(crate) async fn finish_provider_import(&self, import: ValidatedImport, staged: StagedBook, inspection: InspectedBook) -> Result<BookId, BackendError> {
        // Resumable uploads must not inflate the browser worker's inline
        // import future beyond its bounded WebAssembly stack.
        Box::pin(self.queue_provider_import(import, staged, Some(inspection), None)).await
    }

    pub(crate) async fn finish_provider_transfer(&self, import: ValidatedImport, staged: StagedBook, source: BookTransferState) -> Result<BookId, BackendError> {
        Box::pin(self.queue_provider_import(import, staged, None, Some(source))).await
    }

    async fn queue_provider_import(&self, import: ValidatedImport, staged: StagedBook, inspection: Option<InspectedBook>, transfer: Option<BookTransferState>) -> Result<BookId, BackendError> {
        let _guard = self.provider_upload_lock.lock().await;
        let root = self.database.storage_policy().map_err(BackendError::operation)?.external().cloned().ok_or_else(|| BackendError::message("library does not use a cloud provider"))?;
        let checksum = staged.content_hash();
        let id = ProviderUploadDraft::identity(import.parent_id, &import.file_name, checksum);
        let draft = ProviderUploadDraft { id, library_id: self.library_id, root, parent: import.parent_id, file_name: import.file_name, format: import.format, checksum, size_bytes: staged.size_bytes(), inspection, transfer };
        draft.validate().map_err(BackendError::operation)?;
        let store = self.assets.clone();
        let draft = run_blocking(move || -> Result<ProviderUploadDraft, BackendError> {
            let mut refreshed = draft;
            if let Some(previous) = store.provider_upload_metadata(id).map_err(BackendError::operation)? {
                let retained: ProviderUploadDraft = serde_json::from_slice(&previous).map_err(BackendError::operation)?;
                // Reinspection creates fresh contributor IDs even for identical
                // bytes. Keep the retained inspection when retrying that source.
                if retained.inspection.is_some() && refreshed.inspection.is_some() {
                    refreshed.inspection = retained.inspection.clone();
                }
                if !retained.same_source(&refreshed).map_err(BackendError::operation)? {
                    return Err(BackendError::message("pending provider import differs from its retained source"));
                }
                let metadata = serde_json::to_vec(&refreshed).map_err(BackendError::operation)?;
                if previous != metadata {
                    store.replace_provider_upload_metadata(id, &previous, &metadata).map_err(BackendError::operation)?;
                }
            }
            Ok(refreshed)
        })
        .await
        .map_err(BackendError::operation)??;
        staged.retain_provider_upload(self.assets.clone(), id, serde_json::to_vec(&draft).map_err(BackendError::operation)?).await.map_err(BackendError::operation)?;
        self.database.queue_provider_upload(&draft).map_err(BackendError::operation)?;
        self.events.notify_contents_changed();
        self.events.assets.wake();
        Box::pin(self.run_provider_upload(id)).await.map_err(|error| error.context("Book saved locally for cloud upload; retry after resolving the storage error"))
    }

    pub async fn run_provider_uploads(&self) -> Result<(), BackendError> {
        if !client_platform_runtime::transfers::allowed() {
            return Ok(());
        }
        let _guard = self.provider_upload_lock.lock().await;
        let store = self.assets.clone();
        let mut failure = None;
        for (id, bytes) in run_blocking(move || store.retained_provider_uploads()).await.map_err(BackendError::operation)?.map_err(BackendError::operation)? {
            let recovered = (|| {
                let draft: ProviderUploadDraft = serde_json::from_slice(&bytes).map_err(BackendError::operation)?;
                if draft.id != id || draft.library_id != self.library_id {
                    return Err(BackendError::message("pending import belongs to another library"));
                }
                self.database.queue_provider_upload(&draft).map_err(BackendError::operation)
            })();
            if let Err(error) = recovered {
                log::warn!("could not recover pending provider import {id}: {error}");
                failure.get_or_insert(error);
            }
        }
        for id in self.database.provider_upload_ids().map_err(BackendError::operation)? {
            if let Err(error) = Box::pin(self.run_provider_upload(id)).await {
                log::warn!("provider import {id} will retry: {error}");
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), |error| Err(error.context("Some imports are still waiting for cloud upload")))
    }

    async fn run_provider_upload(&self, id: Uuid) -> Result<BookId, BackendError> {
        let upload = self.database.provider_upload(id).map_err(BackendError::operation)?.ok_or_else(|| BackendError::message("pending cloud import is unavailable"))?;
        upload.draft.validate().map_err(BackendError::operation)?;
        if upload.draft.library_id != self.library_id || self.database.storage_policy().map_err(BackendError::operation)?.external() != Some(&upload.draft.root) {
            return Err(BackendError::message("pending import belongs to another library or cloud folder"));
        }
        let connection = self.cloud.google_drive(upload.draft.root.scope.clone()).map_err(BackendError::operation)?;
        self.run_provider_upload_connected(upload, &connection).await
    }

    async fn run_provider_upload_connected(&self, mut upload: ProviderUpload, connection: &ConnectedDrive) -> Result<BookId, BackendError> {
        let draft = &upload.draft;
        crate::import::validate_import_destination(&self.database, &draft.parent)?;
        if connection.scope() != &draft.root.scope {
            return Err(BackendError::message("upload connection belongs to another provider account"));
        }
        if upload.confirmed_file.is_none() {
            let mut source = self.verified_provider_source(draft).await?;
            if upload.file_id.is_none() {
                let parent = self.provider_upload_folder(connection, &draft.root, draft.parent).await?;
                let file = connection.allocate_file_id().await.map_err(BackendError::operation)?;
                if !self.database.allocate_provider_upload(draft.id, &file, &parent).map_err(BackendError::operation)? {
                    return Err(BackendError::message("pending upload changed before file allocation"));
                }
                upload.file_id = Some(file);
                upload.parent_id = Some(parent);
            }
            let file_id = upload.file_id.as_ref().expect("allocated file ID");
            let parent = upload.parent_id.as_ref().ok_or_else(|| BackendError::message("pending cloud upload has no folder"))?;
            self.check_provider_upload_folder(connection, &draft.root, parent).await?;
            let mut session: Option<DriveUpload> = upload.session.as_deref().map(serde_json::from_str).transpose().map_err(|_| BackendError::message("the saved cloud upload session could not be read"))?;
            if session.as_ref().is_some_and(|session| !session.matches_file(&draft.root.scope, file_id, draft.size_bytes)) {
                return Err(BackendError::message("upload session differs from its reserved file"));
            }
            let mut progress = if let Some(session) = &session {
                match connection.upload_status(session).await {
                    Ok(progress) => Some(progress),
                    Err(CloudError::NotFound | CloudError::CursorExpired) => {
                        self.database.set_provider_upload_session(draft.id, file_id, None).map_err(BackendError::operation)?;
                        None
                    }
                    Err(error) => return Err(BackendError::operation(error)),
                }
            } else {
                None
            };
            if progress.is_none() {
                // A final response may have been lost before its durable commit.
                // Verify a recovered object; a matching size alone is insufficient.
                progress = self.recover_provider_upload(connection, draft, file_id, parent).await?.map(UploadProgress::Complete);
                if progress.is_none() {
                    let name = if draft.format == book_model::BookFormat::Mp3Folder && !draft.file_name.to_ascii_lowercase().ends_with(".mp3folder") { format!("{}.mp3folder", draft.file_name) } else { draft.file_name.clone() };
                    let created = connection.begin_upload(parent, file_id, &name, draft.size_bytes).await.map_err(BackendError::operation)?;
                    let json = serde_json::to_string(&created).map_err(BackendError::operation)?;
                    if !self.database.set_provider_upload_session(draft.id, file_id, Some(&json)).map_err(BackendError::operation)? {
                        return Err(BackendError::message("pending upload changed before its session was saved"));
                    }
                    session = Some(created);
                    progress = Some(UploadProgress::Incomplete { confirmed_bytes: 0 });
                }
            }
            let mut file = loop {
                match progress.take().expect("upload progress") {
                    UploadProgress::Complete(file) => break file,
                    UploadProgress::Incomplete { confirmed_bytes: offset } => {
                        let length = (draft.size_bytes - offset).min(MAX_RANGE_BYTES as u64) as usize;
                        let bytes = read_upload_chunk(&mut source, offset, length).await?;
                        let next = connection.upload_chunk(session.as_ref().expect("saved upload session"), offset, bytes).await.map_err(BackendError::operation)?;
                        if matches!(next, UploadProgress::Incomplete { confirmed_bytes } if confirmed_bytes <= offset) {
                            return Err(BackendError::message("cloud upload made no progress; its acknowledged offset will be checked on retry"));
                        }
                        progress = Some(next);
                    }
                }
            };
            file.content_hash = draft.checksum.content_hash();
            if !self.database.confirm_provider_upload(draft.id, &file).map_err(BackendError::operation)? {
                return Err(BackendError::message("pending upload changed before completion"));
            }
            upload.confirmed_file = Some(file);
        }
        let file = upload.confirmed_file.expect("confirmed provider object");
        let id = file.identity.book_id();
        crate::import::validate_import_destination(&self.database, &draft.parent)?;
        let current = self.database.provider_file(&id).map_err(BackendError::operation)?;
        if current.as_ref() != Some(&file) {
            let CloudEntry::File(remote) = connection.describe(&file.identity.file_id).await.map_err(BackendError::operation)? else {
                return Err(BackendError::message("uploaded cloud book is no longer available"));
            };
            if remote.file.revision != file.revision {
                if current.as_ref().is_some_and(|current| current.revision == remote.file.revision) {
                    if draft.transfer.is_some() {
                        return Err(BackendError::message("destination provider file changed; the source selection has been kept"));
                    }
                    // A later scan already admitted the provider replacement.
                    // Never publish the old inspection or bytes over it.
                    self.assets.remove_provider_upload(draft.id).map_err(BackendError::operation)?;
                    self.database.finish_provider_upload(draft.id).map_err(BackendError::operation)?;
                    self.events.notify_contents_changed();
                    return Ok(id);
                }
                return Err(BackendError::message("uploaded cloud book changed; its new revision must be scanned before the import can finish"));
            }
            let parent = upload.parent_id.as_ref().ok_or_else(|| BackendError::message("pending upload has no provider folder"))?;
            if !remote.parent_ids.contains(parent) {
                return Err(BackendError::message("uploaded book moved from its selected cloud folder"));
            }
            self.check_provider_upload_folder(connection, &draft.root, parent).await?;
            if let Some(source) = &draft.transfer {
                self.database.admit_transferred_provider_book(draft.parent, file.clone(), source.clone()).map_err(BackendError::operation)?;
            } else {
                let inspection = draft.inspection.clone().ok_or_else(|| BackendError::message("cloud import has no inspected metadata"))?;
                self.database.admit_inspected_provider_book(draft.parent, file.clone(), draft.format, inspection).map_err(BackendError::operation)?;
            }
        }
        if let Some(source) = &draft.transfer {
            self.database.merge_transferred_reader_state(id, source.clone()).map_err(BackendError::operation)?;
        }
        self.events.sync.wake();
        self.publish_provider_cache(draft, &file, id).await?;
        self.assets.remove_provider_upload(draft.id).map_err(BackendError::operation)?;
        self.database.finish_provider_upload(draft.id).map_err(BackendError::operation)?;
        self.events.notify_contents_changed();
        Ok(id)
    }

    async fn verified_provider_source(&self, draft: &ProviderUploadDraft) -> Result<UploadSource, BackendError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let store = self.assets.clone();
            let (id, checksum, size) = (draft.id, draft.checksum, draft.size_bytes);
            let source = run_blocking(move || {
                let mut source = store.open_provider_upload(id)?;
                if source.metadata()?.len() != size || BookId::from(content_address::hash_file(&mut source).map_err(AssetStoreError::operation)?) != checksum {
                    return Err(AssetStoreError::operation("pending cloud import bytes changed"));
                }
                Ok(source)
            })
            .await
            .map_err(BackendError::operation)?
            .map_err(BackendError::operation)?;
            Ok(tokio::fs::File::from_std(source))
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.assets.verified_provider_upload(draft.id, draft.checksum, draft.size_bytes).await.map_err(BackendError::operation)
        }
    }

    async fn publish_provider_cache(&self, draft: &ProviderUploadDraft, file: &ProviderFile, id: BookId) -> Result<(), BackendError> {
        let _lease = self.assets.lease_book(&id).await.map_err(BackendError::operation)?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let snapshot = self.database.transfer_snapshot(&[id]).map_err(BackendError::operation)?;
            let book = snapshot.placements.iter().find_map(|entry| entry.book.clone().filter(|book| book.directory == draft.parent)).ok_or_else(|| BackendError::message("cloud import has no live local placement"))?;
            if let Some(staged) = self.assets.stage_retained_provider_upload(draft.id, id, draft.size_bytes).map_err(BackendError::operation)? {
                let requested = book.relative_path.with_file_name(&draft.file_name).map_err(BackendError::operation)?;
                self.assets.ensure_placement_parent(&requested).map_err(BackendError::operation)?;
                let publication =
                    staged.publish_snapshot_with_track_titles(&self.assets, &requested, &book.occupied_names, draft.format == book_model::BookFormat::Mp3Folder, &book.audiobook_track_titles).map_err(BackendError::operation)?;
                self.database.complete_downloaded_book(NativeBookCompletion { snapshot: book, publication, size_bytes: draft.size_bytes, provider_file: Some(file.clone()) }, false).map_err(BackendError::operation)?;
            } else if !self.database.is_book_downloaded(&id).map_err(BackendError::operation)? {
                self.database.sync_request_book_downloads(&[id], TransferOrigin::Background).map_err(BackendError::operation)?;
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.database.sync_request_book_downloads(&[id], TransferOrigin::Background).map_err(BackendError::operation)?;
            let snapshot = self.database.provider_download_snapshot(id).map_err(BackendError::operation)?.ok_or_else(|| BackendError::message("cloud import has no download snapshot"))?;
            if &snapshot.file != file {
                return Err(BackendError::message("uploaded provider file changed before local publication"));
            }
            if self.assets.promote_provider_upload(draft.id, draft.checksum, id, draft.size_bytes).map_err(BackendError::operation)? {
                if !self.database.complete_provider_download(&snapshot).map_err(BackendError::operation)? {
                    return Err(BackendError::message("cloud import changed before local completion"));
                }
            }
        }
        Ok(())
    }

    async fn recover_provider_upload(&self, connection: &ConnectedDrive, draft: &ProviderUploadDraft, id: &ProviderId, parent: &ProviderId) -> Result<Option<ProviderFile>, BackendError> {
        let entry = match connection.describe(id).await {
            Ok(entry) => entry,
            Err(CloudError::NotFound) => return Ok(None),
            Err(error) => return Err(BackendError::operation(error)),
        };
        let CloudEntry::File(entry) = entry else { return Err(BackendError::message("reserved upload ID names a folder")) };
        if entry.file.size_bytes != draft.size_bytes || !entry.parent_ids.contains(parent) {
            return Err(BackendError::message("previous cloud upload changed size or folder"));
        }
        let mut hash = blake3::Hasher::new();
        let mut offset = 0;
        while offset < entry.file.size_bytes {
            let length = (entry.file.size_bytes - offset).min(MAX_RANGE_BYTES as u64) as usize;
            let bytes = connection.read_range(&entry.file, offset, length).await.map_err(BackendError::operation)?;
            offset += bytes.len() as u64;
            hash = run_blocking(move || {
                hash.update(&bytes);
                hash
            })
            .await
            .map_err(BackendError::operation)?;
        }
        if BookId::from(hash.finalize()) != draft.checksum {
            return Err(BackendError::message("previous cloud upload differs from the retained book"));
        }
        Ok(Some(entry.file))
    }

    async fn provider_upload_folder(&self, connection: &ConnectedDrive, root: &CloudLibraryRoot, parent: DirId) -> Result<ProviderId, BackendError> {
        crate::import::validate_import_destination(&self.database, &parent)?;
        let mut cloud_parent = root.folder_id.clone();
        for component in self.database.dir_path_components(&parent).map_err(BackendError::operation)? {
            let remembered = self.database.provider_directory(component.id).map_err(BackendError::operation)?;
            let found = connection
                .find_folder(&cloud_parent, |folder| {
                    remembered.as_ref() == Some(&folder.id)
                        || folder.directory_hint.as_ref().is_some_and(|hint| hint.library_id == self.library_id && hint.directory_id == component.id)
                        || DirId::new_v5(&self.library_id, format!("provider-folder/{}", folder.id.as_str()).as_bytes()) == component.id
                })
                .await
                .map_err(BackendError::operation)?;
            let folder = match found {
                Some(folder) => folder,
                None => connection.create_library_folder(&cloud_parent, component.name.as_str(), &CloudDirectoryIdentity { library_id: self.library_id, directory_id: component.id }).await.map_err(BackendError::operation)?,
            };
            self.database.remember_provider_directory(component.id, &folder.id).map_err(BackendError::operation)?;
            cloud_parent = folder.id;
        }
        Ok(cloud_parent)
    }

    async fn check_provider_upload_folder(&self, connection: &ConnectedDrive, root: &CloudLibraryRoot, parent: &ProviderId) -> Result<(), BackendError> {
        let mut pending = vec![parent.clone()];
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop() {
            if id == root.folder_id {
                return Ok(());
            }
            if !visited.insert(id.clone()) {
                continue;
            }
            if visited.len() > 1024 {
                return Err(BackendError::message("cloud upload folder exceeds the supported depth"));
            }
            let CloudEntry::Folder(folder) = connection.describe(&id).await.map_err(BackendError::operation)? else {
                return Err(BackendError::message("cloud upload folder is unavailable"));
            };
            pending.extend(folder.parent_ids);
        }
        Err(BackendError::message("upload folder moved outside the selected cloud library folder"))
    }
}

#[cfg(all(test, feature = "google-drive", not(target_arch = "wasm32")))]
mod tests;

async fn read_upload_chunk(source: &mut UploadSource, offset: u64, length: usize) -> Result<Vec<u8>, BackendError> {
    let mut bytes = vec![0; length];
    #[cfg(not(target_arch = "wasm32"))]
    {
        source.seek(SeekFrom::Start(offset)).await?;
        source.read_exact(&mut bytes).await?;
    }
    #[cfg(target_arch = "wasm32")]
    {
        source.seek(SeekFrom::Start(offset))?;
        source.read_exact(&mut bytes)?;
    }
    Ok(bytes)
}
