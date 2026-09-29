//! Transfer execution on snapshots, recording durable effects for commit.
//!
//! Nothing here touches storage: every method reads its inputs from the
//! drain snapshot and pushes writes into the outcome the driver commits.
//! Network negotiation stays pure data; guards re-validate at commit time.
use super::worker::TransferWorker;
use crate::runner::{DroppedUpload, PreparedUploads};
use crate::ClaimedTransferJob;
use crate::{TransferError, TransferOperation, TransferOrigin};
use library_database::{BookPlacement, BookUploadIntent, TransferOutcome, TransferSnapshot, TransferWrite};
use library_model::DownloadState;
#[cfg(target_arch = "wasm32")]
use library_replica::BlobKind;
use library_replica::TransferJobKind;
use library_runtime::events::LibraryEvent;
use sync_common::ContentHash;

impl TransferWorker {
    pub(super) async fn prepare_book_uploads_with_refresh(&self, intents: &[BookUploadIntent], out: &mut TransferOutcome) -> Result<PreparedUploads, TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(PreparedUploads { jobs: Vec::new(), dropped: Vec::new() });
        }
        let guard = self.refresh_guard(super::RefreshScope::Planned)?;
        let result = self.prepare_book_uploads(intents, out).await;
        if !matches!(result, Err(TransferError::AuthenticationRequired)) {
            return result;
        }
        self.refresh_after_auth(guard).await?;
        self.prepare_book_uploads(intents, out).await
    }

    /// Negotiate one bounded planner page over pure intent data, then record
    /// the manifest outcome for commit. Job selection below mirrors the
    /// commit's guarded settle: present remotely completes, rejected
    /// completes with its reason, everything else transfers.
    pub(super) async fn prepare_book_uploads(&self, intents: &[BookUploadIntent], out: &mut TransferOutcome) -> Result<PreparedUploads, TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(PreparedUploads { jobs: Vec::new(), dropped: Vec::new() });
        }
        let credentials = self.credentials()?;
        let planner = self.transfer_planner();
        let negotiation = planner.negotiate_book_uploads(intents).await?;
        planner.check_transfer_account(&credentials)?;
        out.record(TransferWrite::SettleManifest { intents: intents.to_vec(), present_remotely: negotiation.present_remotely.clone(), rejected: negotiation.rejected.iter().map(|(hash, error)| (*hash, error.to_string())).collect() });
        let mut jobs = Vec::new();
        let mut dropped = Vec::new();
        for intent in intents {
            if negotiation.present_remotely.contains(&intent.content_hash) {
                dropped.push(DroppedUpload::Completed { hash: intent.content_hash });
            } else if let Some((_, reason)) = negotiation.rejected.iter().find(|(hash, _)| *hash == intent.content_hash) {
                dropped.push(DroppedUpload::Rejected { hash: intent.content_hash, reason: reason.to_string() });
            } else {
                jobs.push(crate::TransferJob::upload_book_intent(intent.clone(), negotiation.upload_required.contains(&intent.content_hash), TransferOrigin::Background));
            }
        }
        Ok(PreparedUploads { jobs, dropped })
    }

    pub async fn prepare_thumbnail_uploads_with_refresh(&self, snapshot: &TransferSnapshot, hashes: &[ContentHash], out: &mut TransferOutcome) -> Result<Vec<crate::TransferJob>, TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(Vec::new());
        }
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let guard = self.refresh_guard(super::RefreshScope::Planned)?;
        let result = self.prepare_thumbnail_uploads(snapshot, hashes, out).await;
        if !matches!(result, Err(TransferError::AuthenticationRequired)) {
            return result;
        }
        self.refresh_after_auth(guard).await?;
        self.prepare_thumbnail_uploads(snapshot, hashes, out).await
    }

    /// Check remote availability before publishing upload activity. The
    /// existing bounded thumbnail endpoint also works with older servers.
    ///
    /// Snapshot in, commit out: availability is negotiated over the network
    /// as pure data, then committed once. Upload eligibility below mirrors
    /// the commit's guarded settle over the same snapshot.
    pub(super) async fn prepare_thumbnail_uploads(&self, snapshot: &TransferSnapshot, hashes: &[ContentHash], out: &mut TransferOutcome) -> Result<Vec<crate::TransferJob>, TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(Vec::new());
        }
        let credentials = self.credentials()?;
        let planner = self.transfer_planner();
        let negotiation = planner.negotiate_thumbnail_availability(hashes).await?;
        planner.check_transfer_account(&credentials)?;
        out.record(TransferWrite::SettleAvailability { available: negotiation.available.clone(), missing: negotiation.missing.clone() });
        Ok(negotiation.missing.iter().filter(|hash| snapshot.asset_for(hash).is_some_and(|asset| asset.thumbnail_allowed)).map(|hash| crate::TransferJob::upload_thumbnail(*hash, TransferOrigin::Background)).collect())
    }

    pub(super) fn resolve_rejected_job(&self, job: &crate::TransferJob, reason: &str, out: &mut TransferOutcome) {
        match &job.operation {
            TransferOperation::DownloadBlob { content_hash, .. } => {
                out.record(TransferWrite::CompleteDownload { hash: *content_hash });
            }
            TransferOperation::UploadBlob { content_hash, intent, .. } => {
                out.record(TransferWrite::SettleRejectedUpload { hash: *content_hash, intent: intent.clone(), reason: reason.to_owned() });
            }
            TransferOperation::UploadThumbnail { thumbnail_hash } => {
                out.record(TransferWrite::SettleRejectedThumbnail { hash: *thumbnail_hash, reason: reason.to_owned() });
            }
            TransferOperation::DownloadThumbnail { .. } => {}
        }
    }

    pub(super) async fn handle_claimed_batch(&self, snapshot: &TransferSnapshot, claimed: &[ClaimedTransferJob], out: &mut TransferOutcome) -> Result<(), TransferError> {
        #[cfg(target_arch = "wasm32")]
        if let Some(item) = claimed.first().filter(|item| matches!(item.job.operation, TransferOperation::DownloadBlob { .. })) {
            // Scheduler-owned downloads retain cross-tab ownership and guarded commits.
            return self.handle_job(item.job.clone()).await;
        }
        match claimed.first().map(|item| item.job.batch_kind()) {
            Some(Some(TransferJobKind::DownloadThumbnail)) => self.download_thumbnail_batch(snapshot, claimed, out).await,
            Some(Some(TransferJobKind::UploadThumbnail)) => self.upload_thumbnail_batch(snapshot, claimed, out).await,
            Some(_) => self.execute_job(snapshot, claimed[0].job.clone(), out).await,
            None => Ok(()),
        }
    }

    pub(super) async fn execute_job(&self, snapshot: &TransferSnapshot, job: crate::TransferJob, out: &mut TransferOutcome) -> Result<(), TransferError> {
        match job.operation {
            TransferOperation::UploadBlob { content_hash, intent, negotiated } => self.upload_blob_version(snapshot, &content_hash, intent.as_ref(), negotiated, out).await,
            TransferOperation::DownloadBlob { content_hash, placement } => {
                // A hash-addressed store can satisfy another placement from
                // retained bytes. Native stores must materialize the
                // requested filesystem placement instead, so only the
                // browser path completes here; anything else downloads.
                // Currency re-validates when this record commits.
                #[cfg(target_arch = "wasm32")]
                if placement.content_hash == content_hash && self.assets.exists(BlobKind::Book, &content_hash)? {
                    if snapshot.placement_for(&content_hash, placement.rel_path.as_str()).is_some_and(|entry| entry.placement_current) {
                        out.record(TransferWrite::CompleteRetainedDownload { hash: content_hash, rel_path: placement.rel_path.as_str().to_owned() });
                        return Ok(());
                    }
                }
                self.download_blob_to_placement(snapshot, &content_hash, &placement, out).await
            }
            TransferOperation::UploadThumbnail { thumbnail_hash, .. } => self.upload_thumbnail(snapshot, &thumbnail_hash, out).await,
            TransferOperation::DownloadThumbnail { .. } => Err(TransferError::rejected("thumbnail download job bypassed batch execution")),
        }
    }

    pub(super) fn credentials(&self) -> Result<sync_transport::SyncCredentials, TransferError> {
        self.credentials.credentials().ok_or(TransferError::AuthenticationRequired)
    }

    pub(super) fn check_transfer_account(&self, credentials: &sync_transport::SyncCredentials) -> Result<(), TransferError> {
        crate::books::validate_account(credentials, || self.credentials())
    }

    async fn upload_thumbnail(&self, snapshot: &TransferSnapshot, thumbnail_hash: &ContentHash, out: &mut TransferOutcome) -> Result<(), TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(());
        }
        self.thumbnail_transfers().upload_thumbnail(snapshot, thumbnail_hash, out).await
    }

    async fn download_thumbnail_batch(&self, snapshot: &TransferSnapshot, claimed: &[ClaimedTransferJob], out: &mut TransferOutcome) -> Result<(), TransferError> {
        self.thumbnail_transfers().download_thumbnail_batch(snapshot, claimed, out).await
    }

    async fn upload_thumbnail_batch(&self, snapshot: &TransferSnapshot, claimed: &[ClaimedTransferJob], out: &mut TransferOutcome) -> Result<(), TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(());
        }
        self.thumbnail_transfers().upload_thumbnail_batch(snapshot, claimed, out).await
    }

    fn transfer_planner(&self) -> crate::planning::TransferPlanner<impl Fn() -> Result<sync_transport::SyncCredentials, TransferError> + Sync + '_> {
        // Borrow only the credentials field: the closure must stay Sync, and
        // storage never crosses into it.
        let credentials = &self.credentials;
        crate::planning::TransferPlanner { http_client: self.http_client.clone(), library_id: self.library_id, credentials: move || credentials.credentials().ok_or(TransferError::AuthenticationRequired) }
    }
    fn thumbnail_transfers(
        &self,
    ) -> crate::thumbnails::ThumbnailTransfers<
        impl Fn() -> Result<sync_transport::SyncCredentials, TransferError> + Sync + '_,
        impl Fn(Vec<u8>, u32) -> client_platform_runtime::executor::BoxedBackendFuture<'static, Result<Vec<u8>, client_runtime::BackendError>> + Sync,
        impl Fn(ContentHash) + Sync + '_,
    > {
        // Borrow only shareable fields: these closures must stay Sync, and
        // storage never crosses into them.
        let credentials = &self.credentials;
        let event_tx = &self.event_tx;
        let cpu = self.cpu.clone();
        crate::thumbnails::ThumbnailTransfers {
            assets: self.assets.clone(),
            http_client: self.http_client.clone(),
            library_id: self.library_id,
            credentials: move || credentials.credentials().ok_or(TransferError::AuthenticationRequired),
            resize: move |bytes, width| -> client_platform_runtime::executor::BoxedBackendFuture<'static, Result<Vec<u8>, client_runtime::BackendError>> {
                let cpu = cpu.clone();
                Box::pin(async move { cpu_host::resize(&*cpu, bytes, width).await })
            },
            ready: move |content_hash| {
                if let Some(tx) = event_tx {
                    let _ = tx.try_send(LibraryEvent::ThumbnailGenerated { content_hash });
                }
            },
        }
    }

    pub(super) fn book_transfers(&self) -> crate::books::BookTransfers<impl Fn() -> Result<sync_transport::SyncCredentials, TransferError> + Sync + '_, impl Fn(ContentHash, DownloadState) + Sync + '_> {
        // Borrow only shareable fields: these closures must stay Sync, and
        // storage never crosses into them.
        let credentials = &self.credentials;
        let availability_changed = &self.availability_changed;
        let event_tx = &self.event_tx;
        #[cfg(target_arch = "wasm32")]
        let progress_key = &self.progress_key;
        crate::books::BookTransfers {
            assets: self.assets.clone(),
            http_client: self.http_client.clone(),
            library_id: self.library_id,
            credentials: move || credentials.credentials().ok_or(TransferError::AuthenticationRequired),
            status: move |hash, state| {
                #[cfg(target_arch = "wasm32")]
                if let (Some(key), DownloadState::Downloading(progress)) = (progress_key.as_deref(), &state) {
                    client_platform_web::transport::bridge::report_transfer_progress(key, progress.value() as f64);
                }
                Self::report_download_status(availability_changed, event_tx, hash, state);
            },
            diagnostic: |_| {},
        }
    }

    /// Test helper for transfer execution and guarded commits.
    #[cfg(test)]
    pub(super) async fn download_blob(&self, content_hash: &ContentHash, placement: &BookPlacement) -> Result<(), TransferError> {
        self.execute_owned_job(crate::TransferJob::download_blob(*content_hash, placement.clone(), crate::TransferOrigin::Background)).await
    }

    /// Single-book upload outside the queue, committing its outcome before
    /// returning. Test and explicit-request flows use this directly.
    pub(super) async fn upload_blob(&self, content_hash: &ContentHash) -> Result<(), TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(());
        }
        let database = self.commit_connection()?;
        let snapshot = database.transfer_snapshot(&[*content_hash])?;
        let mut outcome = TransferOutcome::default();
        let result = self.book_transfers().upload_blob(&snapshot, content_hash, &mut outcome).await;
        let commit = database.commit_transfer_outcome(&outcome).map_err(TransferError::from).and_then(|current| if current { Ok(()) } else { Err(TransferError::retryable("local bytes changed before the upload could be confirmed")) });
        result.and(commit)
    }

    async fn upload_blob_version(&self, snapshot: &TransferSnapshot, content_hash: &ContentHash, planned: Option<&library_database::BookUploadIntent>, negotiated: bool, out: &mut TransferOutcome) -> Result<(), TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(());
        }
        self.book_transfers().upload_blob_version(snapshot, content_hash, planned, negotiated, out).await
    }

    async fn download_blob_to_placement(&self, snapshot: &TransferSnapshot, content_hash: &ContentHash, placement: &BookPlacement, out: &mut TransferOutcome) -> Result<(), TransferError> {
        let result = self.book_transfers().download_blob(snapshot, content_hash, placement, out).await;
        if result.is_err() {
            self.send_download_status(*content_hash, DownloadState::Queued);
        }
        result
    }

    /// Reports one book's availability to whoever is showing that book.
    ///
    /// Downloaded and cleared books also change the counts folders display, but
    /// that is a property of the batch rather than of each book in it: a run
    /// that fetches four hundred books changes those counts once. The batch
    /// boundary announces it once after the queue drains.
    pub(super) fn send_download_status(&self, content_hash: ContentHash, state: DownloadState) {
        Self::report_download_status(&self.availability_changed, &self.event_tx, content_hash, state);
    }

    /// Shareable reporting for transfer callbacks: touches only atomics and
    /// channels so the closure stays Sync without storage crossing into it.
    fn report_download_status(availability_changed: &std::sync::atomic::AtomicBool, event_tx: &Option<library_runtime::events::LibraryEventSender>, content_hash: ContentHash, state: DownloadState) {
        if matches!(state, DownloadState::Downloaded | DownloadState::NotDownloaded) {
            availability_changed.store(true, std::sync::atomic::Ordering::Release);
        }
        if let Some(tx) = event_tx {
            let _ = tx.try_send(LibraryEvent::DownloadStatusChanged { content_hash, state });
        }
    }
}
