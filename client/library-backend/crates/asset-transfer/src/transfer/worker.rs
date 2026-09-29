//! Session-owned transfer execution behind a snapshot-in/outcome-out boundary.
//!
//! The worker carries everything a drain needs except storage: queue,
//! credentials, account, assets, and events. Durable effects never cross as
//! connections or callbacks. Each driver opens its own commit connection to
//! the library file from the stored path, snapshots the due hashes, runs one
//! claimed batch, and commits its outcome; SQLite serializes the writers and
//! every replayed write is guarded and idempotent.
//!
//! The worker is `'static`, so planning pages can detach a driver and keep
//! metadata exchange moving while slow asset HTTP is still in flight.
use super::TransferRunResult;
use crate::runner::{TransferHost, TransferRunner};
use crate::{ClaimedTransferJob, TransferJob, TransferQueue};
use crate::{TransferError, TransferOperation};
use futures_util::future::LocalBoxFuture;
use library_database::{BookUploadIntent, Database, TransferOutcome, TransferSnapshot, TransferWrite};
use library_files::asset_store::AssetStore;
use library_replica::BlobKind;
use library_runtime::account::{LibraryAccount, SharedSyncCredentials};
use library_runtime::events::{LibraryEvent, LibraryEventSender};
use library_runtime::interest::NotificationInterest;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use sync_common::{ContentHash, LibraryId};

#[derive(Clone)]
pub struct TransferWorker {
    pub asset_storage_enabled: Arc<AtomicBool>,
    pub assets: AssetStore,
    pub http_client: reqwest::Client,
    pub library_id: LibraryId,
    pub event_tx: Option<LibraryEventSender>,
    pub queue: Arc<TransferQueue>,
    pub credentials: SharedSyncCredentials,
    pub account: Option<LibraryAccount>,
    pub notification_interest: NotificationInterest,
    pub availability_changed: Arc<AtomicBool>,
    pub db_path: std::path::PathBuf,
    pub cpu: std::sync::Arc<dyn cpu_host::CpuHost>,
    #[cfg(target_arch = "wasm32")]
    pub progress_key: Option<String>,
}

impl TransferWorker {
    /// One commit connection per driver: sequential use on one task, so the
    /// non-`Sync` handle never crosses threads while held.
    pub(super) fn commit_connection(&self) -> Result<Database, TransferError> {
        let database = Database::open(&self.db_path)?;
        database.initialize_library()?;
        Ok(database)
    }

    pub(crate) fn asset_storage_enabled(&self) -> bool {
        self.asset_storage_enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(super) fn live_user_id(&self) -> Option<String> {
        self.account.as_ref()?.current().ok()??.user_id().to_owned().into()
    }

    fn runner(&self) -> TransferRunner<'_, Self> {
        TransferRunner { queue: &self.queue, host: self }
    }

    /// Snapshot, drain, and commit one claimed batch.
    /// The outcome commits even when the drain reports an error; records are
    /// idempotent replays of the guarded commits.
    async fn drain_pass(&self) -> TransferRunResult {
        let database = self.commit_connection()?;
        let snapshot = database.transfer_snapshot(&self.queue.due_hashes())?;
        let mut outcome = TransferOutcome::default();
        let drain = self.runner().drain_one_batch(&snapshot, &mut outcome).await;
        // A stale member's next planning pass simply re-queues it; this batch
        // path only needs the commit to have happened, not stayed current.
        match database.commit_transfer_outcome(&outcome) {
            Ok(_) => {
                let result = drain.committed(&self.queue);
                self.send_transfer_status_changed();
                let published = self.publish_committed_downloads(&database, &outcome);
                self.flush_availability_changes();
                result.and(published)
            }
            Err(error) => {
                let error = TransferError::from(error);
                drain.commit_failed(&self.queue, &error);
                self.send_transfer_status_changed();
                Err(error)
            }
        }
    }

    /// Final availability is announced only after its SQLite outcome commits.
    /// Progress is transient, but completion must agree with a new subscriber's
    /// durable snapshot and with refreshed book rows.
    fn publish_committed_downloads(&self, database: &Database, outcome: &TransferOutcome) -> Result<(), TransferError> {
        let mut hashes = std::collections::HashSet::<ContentHash>::new();
        for write in &outcome.writes {
            match write {
                TransferWrite::CompleteDownload { hash } | TransferWrite::CancelDownload { hash } | TransferWrite::CompleteRetainedDownload { hash, .. } | TransferWrite::CompleteLocalBookRequest { hash } => {
                    hashes.insert(*hash);
                }
                TransferWrite::CompleteDownloadedBook { completion, .. } => {
                    hashes.insert(completion.snapshot.content_hash);
                }
                _ => {}
            }
        }
        for hash in hashes {
            let present = self.assets.exists(BlobKind::Book, &hash)?;
            let state = database.transfer_download_state(&hash, present)?;
            self.send_download_status(hash, state);
        }
        Ok(())
    }

    /// Drive due batches until the queue idles, holding the scheduler while
    /// work remains. Authentication pauses the queue; anything else is
    /// already settled per job, so the driver keeps going.
    pub async fn drive(&self) {
        self.send_transfer_status_changed();
        if !self.queue.try_start_scheduler() {
            return;
        }
        loop {
            if let Err(TransferError::AuthenticationRequired) = self.drain_pass().await {
                self.queue.pause_authentication();
                return;
            }
            let Some(delay) = self.queue.next_due_in() else {
                if self.queue.keep_scheduler_or_stop() {
                    continue;
                }
                return;
            };
            self.queue.wait_for_wake(delay).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn run(&self, jobs: Vec<TransferJob>) -> TransferRunResult {
        self.queue.reconcile(jobs);
        self.queue.resume_authentication();
        if !self.queue.try_start_scheduler() {
            return Ok(());
        }
        let result = self.drain_pass().await;
        if result.is_err() {
            self.queue.pause_authentication();
        }
        result
    }

    /// Reconcile one planning page and detach its driver. Slow asset HTTP
    /// must never hold the metadata gate behind it.
    pub fn run_page(&self, generation: u64, jobs: Vec<TransferJob>) {
        self.queue.reconcile_page(generation, jobs);
        Self::ensure_driver(self.clone());
    }

    /// Close a planning generation and detach its driver, same contract as
    /// [`Self::run_page`].
    pub fn finish_plan(&self, generation: u64) {
        self.queue.finish_reconcile(generation);
        Self::ensure_driver(self.clone());
    }

    fn ensure_driver(worker: Self) {
        if !worker.queue.try_start_scheduler() {
            return;
        }
        // The host works in local futures, so the driver is not `Send` and
        // cannot move onto the shared runtime. It gets its own thread with a
        // current-thread runtime instead; the queue rendezvous across it.
        #[cfg(target_arch = "wasm32")]
        client_platform_runtime::executor::spawn_detached(async move { worker.drive_detached().await });
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("transfer driver runtime");
            runtime.block_on(worker.drive_detached());
        });
    }

    /// Scheduler already acquired by [`Self::ensure_driver`]; same loop as
    /// [`Self::drive`] without re-acquiring it.
    async fn drive_detached(&self) {
        loop {
            if let Err(TransferError::AuthenticationRequired) = self.drain_pass().await {
                self.queue.pause_authentication();
                return;
            }
            let Some(delay) = self.queue.next_due_in() else {
                if self.queue.keep_scheduler_or_stop() {
                    continue;
                }
                return;
            };
            self.queue.wait_for_wake(delay).await;
        }
    }

    async fn handle_batch_with_refresh(&self, snapshot: &TransferSnapshot, claimed: &[ClaimedTransferJob], out: &mut TransferOutcome) -> TransferRunResult {
        let guard = self.refresh_guard(super::RefreshScope::Queued)?;
        let result = self.handle_claimed_batch(snapshot, claimed, out).await;
        if !matches!(result, Err(TransferError::AuthenticationRequired)) {
            return result;
        }
        self.refresh_after_auth(guard).await?;
        self.handle_claimed_batch(snapshot, claimed, out).await
    }

    /// Execute one explicitly requested job outside the queue, committing its
    /// outcome before returning.
    pub(super) async fn execute_owned_job(&self, job: TransferJob) -> Result<(), TransferError> {
        let hash = *job.status_fields().1;
        let download = matches!(&job.operation, TransferOperation::DownloadBlob { .. });
        let database = self.commit_connection()?;
        let snapshot = database.transfer_snapshot(&[hash])?;
        let mut outcome = TransferOutcome::default();
        let result = self.execute_job(&snapshot, job, &mut outcome).await;
        let commit = database.commit_transfer_outcome(&outcome).map_err(TransferError::from).and_then(|current| {
            if current {
                self.publish_committed_downloads(&database, &outcome)
            } else {
                Err(TransferError::retryable("local bytes changed before the job could be confirmed"))
            }
        });
        let result = result.and(commit);
        if download && result.is_err() {
            if let Ok(present) = self.assets.exists(BlobKind::Book, &hash) {
                if let Ok(state) = database.transfer_download_state(&hash, present) {
                    self.send_download_status(hash, state);
                }
            }
        }
        self.flush_availability_changes();
        result
    }

    /// Announces, once per drained queue, that the set of locally available
    /// books changed. Individual books already reported themselves.
    pub(crate) fn flush_availability_changes(&self) {
        if !self.availability_changed.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(LibraryEvent::ContentsChanged);
        }
    }

    fn send_transfer_status_changed(&self) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(LibraryEvent::TransferStatusChanged);
        }
    }
}

impl TransferHost for TransferWorker {
    fn prepare_uploads<'a>(&'a self, intents: &'a [BookUploadIntent], out: &'a mut TransferOutcome) -> LocalBoxFuture<'a, Result<crate::runner::PreparedUploads, TransferError>> {
        Box::pin(self.prepare_book_uploads_with_refresh(intents, out))
    }
    fn execute<'a>(&'a self, snapshot: &'a TransferSnapshot, claimed: &'a [ClaimedTransferJob], out: &'a mut TransferOutcome) -> LocalBoxFuture<'a, TransferRunResult> {
        Box::pin(async move {
            let _interest = self.notification_interest.work();
            self.handle_batch_with_refresh(snapshot, claimed, out).await
        })
    }
    fn execute_uploads<'a>(&'a self, snapshot: &'a TransferSnapshot, claimed: &'a [ClaimedTransferJob], out: &'a mut TransferOutcome) -> LocalBoxFuture<'a, Result<Vec<TransferRunResult>, TransferError>> {
        Box::pin(self.upload_claimed_books_with_refresh(snapshot, claimed, out))
    }
    fn settle_rejection(&self, job: &TransferJob, reason: &str, out: &mut TransferOutcome) {
        self.resolve_rejected_job(job, reason, out);
    }
    fn changed(&self) {
        self.send_transfer_status_changed();
    }
    fn flush(&self) {
        // Availability is published by the commit owner, after durable state.
    }
}
