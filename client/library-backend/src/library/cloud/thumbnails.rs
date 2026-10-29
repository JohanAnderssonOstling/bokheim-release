//! Covers live beside provider-owned books. Only complete pairs enter sync;
//! device cache generations prevent a replaced book from showing an old cover.

use super::*;
use cloud_storage::{google_drive::UploadProgress, CloudError};
use library_model::BlobKind;
use sync_common::storage::ProviderThumbnails;

impl LibrarySession {
    pub(super) async fn cloud_thumbnails(&self, connection: &ConnectedDrive, root: &CloudLibraryRoot) -> Result<(), BackendError> {
        // Automatic cover extraction can read substantial parts of an archive.
        if !client_platform_runtime::transfers::allowed() {
            return Ok(());
        }
        let mut failed = false;
        for file in self.db.provider_cover_books()? {
            let id = file.identity.book_id();
            if let Err(error) = self.cloud_thumbnail(connection, root, &file).await {
                log::warn!("could not prepare cloud cover {id}: {error}");
                failed = true;
            }
            tokio::task::yield_now().await;
        }
        if failed {
            Err(BackendError::message("some cloud covers still require preparation"))
        } else {
            Ok(())
        }
    }

    async fn cloud_thumbnail(&self, connection: &ConnectedDrive, root: &CloudLibraryRoot, file: &ProviderFile) -> Result<(), BackendError> {
        let id = file.identity.book_id();
        let thumbnails = self.db.provider_thumbnails(&id)?;
        let cached = self.db.provider_cover_cached(&id)?;
        let local = cached == Some(true) && self.assets.thumbnail_set_exists(&id).map_err(BackendError::operation)?;
        if !local {
            if cached == Some(false) {
                return Ok(());
            }
            let versions = if let Some(pair) = &thumbnails {
                pair.validate_for(&root.scope).map_err(BackendError::operation)?;
                let bytes = read_cover(connection, &pair.full).await.map_err(BackendError::operation)?;
                cpu_host::cover(&*self.cpu, bytes).await.map(Some)?
            } else {
                self.generate_cloud_thumbnail(connection.clone(), file.clone()).await?
            };
            // Don't expose a generated cover for a revision replaced mid-read.
            if self.db.provider_file(&id)?.as_ref() != Some(file) || self.db.provider_thumbnails(&id)? != thumbnails {
                return Ok(());
            }
            if let Some(versions) = versions {
                self.assets.write_thumbnail_set(&id, &versions.high_density, &versions.browse, &versions.card_high_density).await.map_err(BackendError::operation)?;
                if !self.db.complete_provider_cover_cache(file, thumbnails.as_ref(), true)? {
                    return Ok(());
                }
                self.emit(LibraryEvent::ThumbnailGenerated { content_hash: id });
            } else {
                self.db.complete_provider_cover_cache(file, thumbnails.as_ref(), false)?;
                return Ok(());
            }
        }
        if thumbnails.is_some() {
            return Ok(());
        }
        // Read an immutable local pair under the same lease as ordinary uploads.
        // Release it before writing any cache files.
        let (full, browse) = {
            let _lease = self.assets.try_exclusive_lease(&id).map_err(BackendError::operation)?.ok_or_else(|| BackendError::message("thumbnail is being updated"))?;
            let full = self.assets.read_bytes(BlobKind::Thumbnail, &id).await.map_err(BackendError::operation)?.ok_or_else(|| BackendError::message("cloud cover cache was evicted"))?;
            let browse = self
                .assets
                .read_named(library_files::asset_store::thumbnail_variant_name(&id, thumbnail::BROWSE_THUMBNAIL_WIDTH))
                .await
                .map_err(BackendError::operation)?
                .ok_or_else(|| BackendError::message("cloud cover cache was evicted"))?;
            (full, browse)
        };
        let upload = async {
            let metadata = ensure_folder(connection, &root.folder_id, ".bokheim").await?;
            let directory = ensure_folder(connection, &metadata, "thumbnails").await?;
            let directory = ensure_folder(connection, &directory, &id.to_hex()).await?;
            let full = upload_cover(connection, &directory, "full", full).await?;
            let browse = upload_cover(connection, &directory, "browse", browse).await?;
            Ok::<_, CloudError>(ProviderThumbnails { full, browse })
        }
        .await;
        let pair = match upload {
            Ok(pair) => pair,
            // Reading a library must remain useful when writes are forbidden.
            Err(CloudError::PermissionDenied) => return Ok(()),
            Err(error) => return Err(BackendError::operation(error)),
        };
        if self.db.publish_provider_thumbnails(file, pair.clone())? {
            self.db.complete_provider_cover_cache(file, Some(&pair), true)?;
            self.events.sync.wake();
        }
        Ok(())
    }
}

async fn ensure_folder(connection: &ConnectedDrive, parent: &ProviderId, name: &str) -> Result<ProviderId, CloudError> {
    match connection.find_folder(parent, |folder| folder.name == name).await? {
        Some(folder) => Ok(folder.id),
        None => Ok(connection.create_folder(parent, name).await?.id),
    }
}

async fn read_cover(connection: &ConnectedDrive, file: &ProviderFile) -> Result<Vec<u8>, CloudError> {
    file.validate().map_err(|error| CloudError::InvalidResponse(error.to_string()))?;
    if file.size_bytes > sync_common::MAX_THUMBNAIL_BYTES {
        return Err(CloudError::InvalidResponse("cloud cover exceeds its size limit".into()));
    }
    let bytes = connection.read_range(file, 0, file.size_bytes as usize).await?;
    if let Some(expected) = file.content_hash {
        if blake3::hash(&bytes).as_bytes() != expected.as_bytes() {
            return Err(CloudError::InvalidResponse("cloud cover checksum differs".into()));
        }
    }
    Ok(bytes)
}

async fn upload_cover(connection: &ConnectedDrive, parent: &ProviderId, variant: &str, bytes: Vec<u8>) -> Result<ProviderFile, CloudError> {
    if bytes.is_empty() || bytes.len() as u64 > sync_common::MAX_THUMBNAIL_BYTES {
        return Err(CloudError::InvalidRequest("cloud cover exceeds its size limit".into()));
    }
    let digest = blake3::hash(&bytes);
    let name = format!("{variant}-{}.jpg", digest.to_hex());
    // A retry after a lost response can reuse a committed variant. Validate its
    // bytes, since a filename alone is never evidence of a completed upload.
    let mut page = None;
    loop {
        let listing = connection.list_folder(parent, page.as_deref(), false).await?;
        for entry in listing.entries {
            if let CloudEntry::File(entry) = entry {
                if entry.name == name && entry.file.size_bytes == bytes.len() as u64 && entry.parent_ids.contains(parent) {
                    match read_cover(connection, &entry.file).await {
                        Ok(existing) if existing == bytes => {
                            let mut file = entry.file;
                            file.content_hash = Some(digest.into());
                            return Ok(file);
                        }
                        Ok(_) | Err(CloudError::NotFound | CloudError::RevisionChanged) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        page = listing.next_page;
        if page.is_none() {
            break;
        }
    }
    let id = connection.allocate_file_id().await?;
    let upload = connection.begin_upload(parent, &id, &name, bytes.len() as u64).await?;
    let mut file = match connection.upload_chunk(&upload, 0, bytes).await? {
        UploadProgress::Complete(file) => file,
        UploadProgress::Incomplete { .. } => match connection.upload_status(&upload).await? {
            UploadProgress::Complete(file) => file,
            UploadProgress::Incomplete { .. } => return Err(CloudError::Retryable("cloud cover upload has not committed".into())),
        },
    };
    file.content_hash = Some(digest.into());
    Ok(file)
}
