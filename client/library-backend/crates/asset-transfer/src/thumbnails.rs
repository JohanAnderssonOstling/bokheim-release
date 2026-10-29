//! Thumbnail transfer execution with host-supplied resizing and notifications.
use crate::deadline::transfer_deadline;
use crate::policy::SMALL_REQUEST_TIMEOUT;
use crate::TransferError;
use cpu_host::{resize, CpuHost};
use library_database::Database;
use library_files::AssetStore;
use library_model::BlobKind;
use library_runtime::account::SharedSyncCredentials;
use library_runtime::events::{LibraryEvent, LibraryEventRouter};
use std::{collections::HashSet, sync::Arc};
use sync_common::{BookId, LibraryId};
use sync_transport::SyncCredentials;
const MAX_THUMBNAIL_BYTES: usize = sync_common::MAX_THUMBNAIL_BYTES as usize;
use sync_common::api::assets::ThumbnailBatchEntry;

/// Owns thumbnail byte handling and persistence; the host supplies CPU work and events.
pub struct ThumbnailTransfers {
    pub assets: AssetStore,
    pub database: Arc<Database>,
    pub http_client: reqwest::Client,
    pub library_id: LibraryId,
    pub credentials: SharedSyncCredentials,
    pub cpu: Arc<dyn CpuHost>,
    pub events: Option<LibraryEventRouter>,
}
impl ThumbnailTransfers {
    fn credentials(&self) -> Result<SyncCredentials, TransferError> {
        if self.database.storage_policy()?.external().is_some() { return Err(TransferError::rejected("external library thumbnails belong to the selected storage provider")); }
        self.credentials.credentials().ok_or(TransferError::AuthenticationRequired)
    }
    fn check_transfer_account(&self, credentials: &SyncCredentials) -> Result<(), TransferError> {
        if self.credentials()?.server_url() != credentials.server_url() {
            return Err(TransferError::retryable("credentials changed during asset transfer"));
        }
        Ok(())
    }
    async fn upload_pair(&self, hash: BookId) -> Result<Option<ThumbnailBatchEntry>, TransferError> {
        // Writers hold book leases while replacing local variants. An exclusive
        // lease keeps this upload snapshot from mixing two cover generations.
        let _lease = self.assets.try_exclusive_lease(&hash)?.ok_or_else(|| TransferError::retryable("thumbnail is being updated"))?;
        let Some(bytes) = self.assets.read_bytes(BlobKind::Thumbnail, &hash).await? else { return Ok(None) };
        if bytes.len() > MAX_THUMBNAIL_BYTES || !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            self.discard_invalid_thumbnail(hash)?;
            return Ok(None);
        }
        let browse_bytes = match self.assets.read_named(library_files::asset_store::thumbnail_variant_name(&hash, thumbnail::BROWSE_THUMBNAIL_WIDTH)).await? {
            Some(bytes) => bytes,
            None => resize(&*self.cpu, bytes.clone(), thumbnail::BROWSE_THUMBNAIL_WIDTH).await.map_err(TransferError::retryable)?,
        };
        if browse_bytes.len() > MAX_THUMBNAIL_BYTES || !browse_bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            self.discard_invalid_thumbnail(hash)?;
            return Ok(None);
        }
        Ok(Some(ThumbnailBatchEntry { content_hash: hash, bytes, browse_bytes }))
    }

    /// Upload a bounded page in one request, enforcing its aggregate byte bound.
    pub async fn upload_thumbnail_batch(&self, hashes: &[BookId]) -> Result<(), TransferError> {
        let mut thumbnails = Vec::with_capacity(hashes.len());
        let mut total_bytes = 0usize;
        for hash in hashes {
            let Some(pair) = self.upload_pair(*hash).await? else { continue };
            total_bytes = total_bytes.checked_add(pair.bytes.len() + pair.browse_bytes.len()).ok_or_else(|| TransferError::rejected("thumbnail batch exceeds byte limit"))?;
            if total_bytes > sync_common::api::assets::MAX_THUMBNAIL_BATCH_BYTES {
                return Err(TransferError::rejected("thumbnail batch exceeds byte limit"));
            }
            thumbnails.push(pair);
        }
        if thumbnails.is_empty() {
            return Ok(());
        }
        let credentials = self.credentials()?;
        let uploaded = thumbnails.iter().map(|thumbnail| thumbnail.content_hash).collect::<Vec<_>>();
        let response = transfer_deadline(SMALL_REQUEST_TIMEOUT, crate::upload_thumbnail_batch(&self.http_client, credentials.server_url(), credentials.access_token(), &self.library_id, thumbnails)).await?;
        self.check_transfer_account(&credentials)?;
        response?;
        self.database.record_server_covers(&uploaded)?;
        Ok(())
    }
    /// Covers that are ready, received or missing are written once for the
    /// batch, also when it fails partway: what was stored still counts.
    pub async fn download_thumbnail_batch(&self, hashes: &[BookId]) -> Result<(), TransferError> {
        let mut ready = Vec::new();
        let mut received = Vec::new();
        let mut missing = Vec::new();
        let result: Result<(), TransferError> = async {
            let mut requested = HashSet::<BookId>::new();
            for thumbnail_hash in hashes {
                let is_local = self.assets.thumbnail_set_exists(thumbnail_hash)?;
                if is_local {
                    ready.push(*thumbnail_hash);
                    continue;
                }
                // Fetch missing variants from the server instead of rebuilding
                // them from a previously downloaded master on this device.
                requested.insert(*thumbnail_hash);
            }
            if requested.is_empty() {
                return Ok(());
            }
            let credentials = self.credentials()?;
            let response = transfer_deadline(SMALL_REQUEST_TIMEOUT, crate::download_thumbnail_batch(&self.http_client, credentials.server_url(), credentials.access_token(), &self.library_id, requested.iter().copied().collect())).await?;
            self.check_transfer_account(&credentials)?;
            let response = response?;
            let mut answered = HashSet::new();
            for thumbnail in response.thumbnails {
                if !requested.contains(&thumbnail.content_hash) {
                    return Err(TransferError::retryable("thumbnail batch returned an unrequested hash"));
                }
                if !answered.insert(thumbnail.content_hash) || thumbnail.bytes.len() > MAX_THUMBNAIL_BYTES || !thumbnail.bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                    return Err(TransferError::retryable("thumbnail batch returned invalid bytes"));
                }
                if thumbnail.browse_bytes.len() > MAX_THUMBNAIL_BYTES || !thumbnail.browse_bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                    return Err(TransferError::retryable("thumbnail batch returned invalid browse bytes"));
                }
                // Server artwork is already finished. The transfer supplies a
                // browse JPEG and a master; use the master for dense cards too,
                // preserving both images without cropping or re-encoding.
                self.assets.write_thumbnail_set(&thumbnail.content_hash, &thumbnail.bytes, &thumbnail.browse_bytes, &thumbnail.bytes).await?;
                self.check_transfer_account(&credentials)?;
                received.push(thumbnail.content_hash);
                ready.push(thumbnail.content_hash);
            }
            self.check_transfer_account(&credentials)?;
            for hash in response.missing {
                if !requested.contains(&hash) || !answered.insert(hash) {
                    return Err(TransferError::retryable("thumbnail batch returned an invalid missing hash"));
                }
                missing.push(hash);
            }
            if answered.len() != requested.len() {
                return Err(TransferError::retryable("thumbnail batch omitted requested hashes"));
            }
            Ok(())
        }
        .await;
        self.database.complete_cover_downloads(&ready, &received, &missing)?;
        for hash in ready {
            if let Some(events) = &self.events {
                events.emit(LibraryEvent::ThumbnailGenerated { content_hash: hash });
            }
        }
        result
    }
    fn discard_invalid_thumbnail(&self, content_hash: BookId) -> Result<(), TransferError> {
        self.assets.remove(BlobKind::Thumbnail, &content_hash)?;
        self.database.request_missing_thumbnail(&content_hash)?;
        Ok(())
    }
}
