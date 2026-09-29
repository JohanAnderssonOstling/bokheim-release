//! Verified book upload/download workflows with host-supplied resources.
#[cfg(not(target_arch = "wasm32"))]
mod batch;
use super::deadline::transfer_deadline;
use crate::policy::{download_progress, upload_timeout, SMALL_REQUEST_TIMEOUT};
use crate::TransferError;
use futures_util::StreamExt;
use library_database::{BookPlacement, NativeBookCompletion};
use library_files::AssetStore;
use library_model::{DownloadProgress, DownloadState};
use library_replica::BlobKind;
use sync_common::ContentHash;

#[cfg(target_arch = "wasm32")]
pub mod web_upload;

/// Credentials and notifications are supplied by the host; execution owns the
/// byte lifecycle, revision checks, account checks, and conditional book.
pub struct BookTransfers<C, S> {
    pub assets: AssetStore,
    pub http_client: reqwest::Client,
    pub library_id: sync_common::LibraryId,
    pub credentials: C,
    pub status: S,
    pub diagnostic: fn(String),
}
impl<C, S> BookTransfers<C, S>
where
    C: Fn() -> Result<sync_transport::SyncCredentials, TransferError> + Sync,
    S: Fn(ContentHash, DownloadState) + Sync,
{
    fn credentials(&self) -> Result<sync_transport::SyncCredentials, TransferError> {
        (self.credentials)()
    }
    fn check_transfer_account(&self, credentials: &sync_transport::SyncCredentials) -> Result<(), TransferError> {
        if self.credentials()?.server_url() != credentials.server_url() {
            return Err(TransferError::retryable("credentials changed during asset transfer"));
        }
        Ok(())
    }
    fn send_download_status(&self, hash: ContentHash, state: DownloadState) {
        (self.status)(hash, state);
    }

    fn book_label(versions: &[(String, String, String)]) -> String {
        versions.first().map(|(_, file_name, _)| file_name.as_str()).unwrap_or("unknown file").to_owned()
    }

    pub async fn upload_blob(&self, snapshot: &library_database::TransferSnapshot, content_hash: &ContentHash, out: &mut library_database::TransferOutcome) -> Result<(), TransferError> {
        self.upload_blob_version(snapshot, content_hash, None, false, out).await
    }

    pub async fn upload_blob_version(
        &self, snapshot: &library_database::TransferSnapshot, content_hash: &ContentHash, planned: Option<&library_database::BookUploadIntent>, negotiated: bool, out: &mut library_database::TransferOutcome,
    ) -> Result<(), TransferError> {
        let mut trace = sync_transport::PerformanceTrace::new("book_upload", "read_intent");
        log::debug!(target: "sync_performance", "trace_id={} library_id={}", trace.id(), self.library_id);
        let credentials = self.credentials()?;
        let asset = snapshot.asset_for(content_hash).cloned().ok_or_else(|| TransferError::retryable("transfer snapshot changed during drain"))?;
        let local_versions = asset.upload.local_versions;
        let intent = asset.upload.intent;
        if intent.is_none() {
            trace.finish(true);
            return Ok(());
        }
        if planned.is_some() && intent.as_ref() != planned {
            trace.finish(true);
            return Ok(());
        }
        let file_name = Self::book_label(&local_versions);
        trace.phase("verify_local_file");
        let paths = asset.upload_paths;
        let Some(book) = self.assets.verified_book_version_reader_at_paths(*content_hash, intent.as_ref().map(|job| job.checksum), paths).await? else {
            // Verification rejects missing or mismatching bytes without deleting
            // other local revisions. Retrying that same job forever would
            // pin the queue behind one pathological file.
            return Err(TransferError::rejected(format!("local book is unavailable or invalid: {file_name}")));
        };
        if book.length == 0 {
            return Err(TransferError::rejected(format!("local book is empty: {file_name}")));
        }
        if intent.as_ref().is_some_and(|job| job.checksum != book.checksum || job.size_bytes != book.length) {
            return Err(TransferError::retryable("local bytes do not match queued book version"));
        }
        log::debug!(target: "sync_performance", "trace_id={} bytes={} negotiated={negotiated}", trace.id(), book.length);
        trace.phase("negotiate");
        let upload_required = if negotiated {
            true
        } else {
            let negotiated = transfer_deadline(
                SMALL_REQUEST_TIMEOUT,
                crate::negotiate_blobs(&self.http_client, &credentials, &self.library_id, vec![sync_common::api::assets::BlobManifestEntry { content_hash: *content_hash, checksum: book.checksum, size_bytes: book.length }]),
            )
            .await?;
            self.check_transfer_account(&credentials)?;
            let negotiated = negotiated.map_err(|error| match error {
                sync_transport::SyncRequestError::AuthenticationRequired => TransferError::AuthenticationRequired,
                error => TransferError::retryable(error),
            })?;
            if let Some(rejected) = negotiated.rejected.first() {
                return Err(TransferError::from_upload_admission(rejected.reason));
            }
            // Transport validates that every requested hash has one outcome.
            !negotiated.upload.is_empty()
        };
        trace.phase("upload_http");
        let response = if upload_required {
            #[cfg(not(target_arch = "wasm32"))]
            let response = {
                let content_length = book.length;
                let checksum = book.checksum;
                let body = book.into_upload_body()?;
                transfer_deadline(upload_timeout(content_length), crate::upload_book_revision_stream(&self.http_client, credentials.server_url(), credentials.access_token(), &self.library_id, content_hash, &checksum, content_length, body))
                    .await?
                    .map_err(TransferError::from)
            };
            #[cfg(target_arch = "wasm32")]
            let response = transfer_deadline(upload_timeout(book.length), web_upload::upload(&book, &sync_transport::blob_endpoint(credentials.server_url(), &self.library_id, content_hash), credentials.access_token())).await?;
            response
        } else {
            Ok(())
        };
        trace.phase("record_local_completion");
        response?;
        // Currency is re-validated when the orchestrator commits this record;
        // a placement that changed mid-drain is rejected there and the next
        // planning pass retries the current version.
        out.record(library_database::TransferWrite::CompleteUploadedBook { hash: *content_hash, local_versions, intent });
        trace.finish(true);
        Ok(())
    }

    pub async fn download_blob(&self, snapshot: &library_database::TransferSnapshot, content_hash: &ContentHash, placement: &BookPlacement, out: &mut library_database::TransferOutcome) -> Result<(), TransferError> {
        // No live placement expects this download: the placement went stale
        // (or never named this hash), so there is nothing to publish. This
        // mirrors the currency skip below; the queue job still completes.
        let Some(entry) = snapshot.placement_for(content_hash, placement.rel_path.as_str()) else {
            return Ok(());
        };
        let credentials = self.credentials()?;
        self.send_download_status(*content_hash, DownloadState::Downloading(DownloadProgress::ZERO));
        (self.diagnostic)(format!("resolve_book phase=http_request content_hash={content_hash}"));
        let response = client_platform_runtime::executor::timeout(std::time::Duration::from_secs(20), crate::download_blob_stream(&self.http_client, credentials.server_url(), credentials.access_token(), &self.library_id, content_hash))
            .await
            .map_err(|_| TransferError::retryable("stage=http_headers: book download timed out after 20 seconds"))?;
        self.check_transfer_account(&credentials)?;
        let response = match response? {
            crate::BlobDownloadResponse::Missing => {
                self.check_transfer_account(&credentials)?;
                out.record(library_database::TransferWrite::CancelDownload { hash: *content_hash });
                return Ok(());
            }
            crate::BlobDownloadResponse::Content(response) => response,
        };
        (self.diagnostic)(format!("resolve_book phase=http_headers content_hash={content_hash} content_length={:?}", response.content_length()));
        let total_bytes = response.content_length();
        let transfer_checksum = response
            .headers()
            .get("x-bokheim-content-checksum")
            .map(|value| value.to_str().ok().and_then(|value| value.parse::<ContentHash>().ok()).ok_or_else(|| TransferError::rejected("invalid book transfer checksum")))
            .transpose()?
            .ok_or_else(|| TransferError::rejected("book response is missing its transfer checksum"))?;
        self.assets.check_write_capacity(total_bytes.unwrap_or(1))?;
        #[cfg(target_arch = "wasm32")]
        let mut destination = self.assets.begin_stream_write(BlobKind::Book, content_hash).await?;
        #[cfg(not(target_arch = "wasm32"))]
        let mut destination = {
            if entry.download_directory.is_none() {
                return Ok(());
            }
            self.assets.ensure_placement_parent(placement.rel_path.as_str())?;
            if !entry.placement_current {
                return Ok(());
            }
            self.assets.begin_book_write(placement.rel_path.as_str(), content_hash).await?
        };
        destination.set_transfer_checksum(transfer_checksum);
        let mut completed_bytes = 0_u64;
        let mut last_progress = web_time::Instant::now();
        let mut stream = response.bytes_stream();
        loop {
            let next = client_platform_runtime::executor::timeout(std::time::Duration::from_secs(20), stream.next()).await.map_err(|_| TransferError::retryable("stage=http_body: book download stalled for 20 seconds"))?;
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(TransferError::retryable)?;
            completed_bytes = completed_bytes.saturating_add(chunk.len() as u64);
            destination.write_all(&chunk).await?;
            if let Some(progress) = download_progress(completed_bytes, total_bytes, &mut last_progress, web_time::Instant::now()) {
                self.send_download_status(*content_hash, DownloadState::Downloading(progress));
            }
        }
        (self.diagnostic)(format!("resolve_book phase=body_complete content_hash={content_hash} bytes={completed_bytes}"));
        if let Err(error) = destination.verify().await {
            self.check_transfer_account(&credentials)?;
            out.record(library_database::TransferWrite::CancelDownload { hash: *content_hash });
            return Err(TransferError::rejected(error.to_string()));
        }
        (self.diagnostic)(format!("resolve_book phase=hash_verified content_hash={content_hash}"));
        self.check_transfer_account(&credentials)?;
        if !entry.placement_current {
            out.record(library_database::TransferWrite::CancelDownload { hash: *content_hash });
            return Ok(());
        }

        let _lease = self.assets.lease_book(content_hash).await?;
        self.check_transfer_account(&credentials)?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            // A content mismatch here is the same permanent failure the
            // checksum verify above rejects; retrying would just download the
            // same wrong bytes again.
            let destination = match destination.finish_book().await {
                Ok(destination) => destination,
                Err(error) => {
                    self.check_transfer_account(&credentials)?;
                    out.record(library_database::TransferWrite::CancelDownload { hash: *content_hash });
                    return Err(TransferError::rejected(error.to_string()));
                }
            };
            let Some(book) = entry.book.clone() else {
                out.record(library_database::TransferWrite::CancelDownload { hash: *content_hash });
                return Ok(());
            };
            let published = destination.publish_snapshot(&self.assets, &book.relative_path, &book.occupied_names)?;
            self.check_transfer_account(&credentials)?;
            // The commit re-validates the snapshot triple and re-checks name
            // conflicts; an obsolete placement is rejected there. The event
            // below is optimistic: a rejected commit leaves the placement
            // untouched and the next planning pass retries.
            out.record(library_database::TransferWrite::CompleteDownloadedBook {
                completion: NativeBookCompletion { snapshot: book, name: published.name, relative_path: published.relative_path, published: published.published, fingerprint: published.fingerprint },
                thumbnail_set_exists: self.assets.thumbnail_set_exists(content_hash)?,
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            destination.commit().await?;
            out.record(library_database::TransferWrite::CompleteLocalBookRequest { hash: *content_hash });
        }
        Ok(())
    }
}

/// Validate identity using the connection that will commit the result. Token
/// refresh for the same account does not discard otherwise valid work.
pub fn validate_account(original: &sync_transport::SyncCredentials, current: impl FnOnce() -> Result<sync_transport::SyncCredentials, TransferError>) -> Result<(), TransferError> {
    if current()?.server_url() != original.server_url() {
        return Err(TransferError::retryable("credentials changed during asset transfer"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sync_transport::SyncCredentials;

    #[test]
    fn account_validation_preserves_refresh_and_rejects_server_switch_before_credentials() {
        let server = "https://sync.example.test".parse().unwrap();
        let original = SyncCredentials::new(server, "old-token");

        // A refreshed token for the same server preserves the work.
        validate_account(&original, || Ok(SyncCredentials::new(original.server_url().as_str().parse().unwrap(), "new-token"))).unwrap();
        assert!(matches!(validate_account(&original, || Err(TransferError::AuthenticationRequired)), Err(TransferError::AuthenticationRequired)));
        assert!(matches!(validate_account(&original, || Ok(SyncCredentials::new("https://other.example.test".parse().unwrap(), "new-token"))), Err(TransferError::Retryable(_))));
    }
}
