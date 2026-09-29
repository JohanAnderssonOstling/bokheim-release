use super::error::SyncError;
use super::error::SyncResult;
use account_client::ServerUrl;
use asset_transfer::{TransferOrigin, TransferQueue};
use library_database::Database;
use library_files::asset_store::AssetStore;
use library_replica::TransferJobKind;
use library_runtime::events::{LibraryEvent, LibraryEventSender};
use library_runtime::interest::NotificationInterest;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use sync_common::ContentHash;
use sync_common::{LibraryId, ReplicaId};
use sync_engine::SyncOutcome;

#[cfg(not(target_arch = "wasm32"))]
#[path = "library_sync/native.rs"]
mod host;
#[cfg(target_arch = "wasm32")]
#[path = "library_sync/web.rs"]
mod host;

#[derive(Clone, Copy)]
enum SyncMode {
    Full,
    Outbox,
    Repair,
}
struct SyncExchange {
    outcome: SyncOutcome,
    inventory_checked: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PersistedSyncWork {
    pub(crate) assets: bool,
}

pub struct LibrarySyncConfig {
    pub(crate) database_path: std::path::PathBuf,
    pub(crate) assets: AssetStore,
    pub(crate) replica_id: ReplicaId,
    pub(crate) server_url: ServerUrl,
    pub(crate) library_id: LibraryId,
    pub(crate) event_tx: Option<LibraryEventSender>,
    pub(crate) transfer_queue: Arc<TransferQueue>,
    pub(crate) credentials: library_runtime::account::SharedSyncCredentials,
    pub(crate) account: Option<library_runtime::account::LibraryAccount>,
    pub(crate) notification_interest: NotificationInterest,
    pub(crate) cpu: std::sync::Arc<dyn cpu_host::CpuHost>,
}

pub struct LibrarySync {
    pub(super) asset_storage_enabled: Arc<AtomicBool>,
    pub(super) assets: AssetStore,
    db_path: std::path::PathBuf,
    replica_id: ReplicaId,
    server_url: ServerUrl,
    pub(super) http_client: reqwest::Client,
    pub(super) library_id: LibraryId,
    pub(super) event_tx: Option<LibraryEventSender>,
    pub(super) credentials: library_runtime::account::SharedSyncCredentials,
    pub(super) account: Option<library_runtime::account::LibraryAccount>,
    pub(super) notification_interest: NotificationInterest,
    pub(super) transfer_queue: Arc<TransferQueue>,
    pub(super) cpu: std::sync::Arc<dyn cpu_host::CpuHost>,
    sync_gate: tokio::sync::Mutex<()>,
    asset_gate: tokio::sync::Mutex<()>,
    pub(crate) registration_gate: tokio::sync::Mutex<()>,
    inventory_checked: AtomicBool,
    metadata_activity: AtomicUsize,
    asset_activity: AtomicUsize,
    pub(super) availability_changed: Arc<AtomicBool>,
}

impl Drop for LibrarySync {
    fn drop(&mut self) {
        // A detached transfer worker may still own the queue. Persist completed
        // history before this library manager can be closed and reopened.
        if let Err(error) = self.transfer_queue.flush_history() {
            log::warn!("Could not flush transfer history while closing library {}: {error}", self.library_id);
        }
    }
}

/// Counts overlapping work and clears activity on completion, error or cancellation.
struct SyncActivity<'a> {
    count: &'a AtomicUsize,
    events: Option<&'a LibraryEventSender>,
}
impl<'a> SyncActivity<'a> {
    fn begin(count: &'a AtomicUsize, events: Option<&'a LibraryEventSender>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        if let Some(events) = events {
            let _ = events.try_send(LibraryEvent::TransferStatusChanged);
        }
        Self { count, events }
    }
}
impl Drop for SyncActivity<'_> {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
        if let Some(events) = self.events {
            let _ = events.try_send(LibraryEvent::TransferStatusChanged);
        }
    }
}

impl LibrarySync {
    pub(crate) fn metadata_sync_running(&self) -> bool {
        self.metadata_activity.load(Ordering::Acquire) > 0
    }
    pub(crate) fn preparing_transfers(&self) -> bool {
        self.asset_activity.load(Ordering::Acquire) > 0
    }

    pub(super) fn notify_remote_page_applied(&self) {
        if let Some(events) = &self.event_tx {
            let _ = events.try_send(LibraryEvent::ContentsChanged);
        }
    }

    pub(crate) fn new(config: LibrarySyncConfig) -> Self {
        let http_client = reqwest::Client::new();
        let transfer_queue = config.transfer_queue;
        Self {
            asset_storage_enabled: Arc::new(AtomicBool::new(true)),
            assets: config.assets,
            db_path: config.database_path,
            replica_id: config.replica_id,
            server_url: config.server_url,
            http_client,
            library_id: config.library_id,
            event_tx: config.event_tx,
            credentials: config.credentials,
            account: config.account,
            notification_interest: config.notification_interest,
            transfer_queue,
            cpu: config.cpu,
            sync_gate: tokio::sync::Mutex::new(()),
            asset_gate: tokio::sync::Mutex::new(()),
            registration_gate: tokio::sync::Mutex::new(()),
            inventory_checked: AtomicBool::new(false),
            metadata_activity: AtomicUsize::new(0),
            asset_activity: AtomicUsize::new(0),
            availability_changed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// One commit connection per call, opened inline from the stored library
    /// path. Sync never receives storage handles; SQLite serializes the
    /// writers and every commit replays guarded, idempotent writes.
    pub(super) fn database(&self) -> SyncResult<Database> {
        Database::open(&self.db_path).map_err(|error| SyncError::failed(error.to_string()))
    }

    /// Reconstruct work from durable state. Scheduler messages only wake the
    /// worker; losing a notification cannot lose a mutation or asset request.
    #[cfg(test)]
    pub(crate) async fn pending_work(&self) -> SyncResult<Option<PersistedSyncWork>> {
        let database = self.database()?;
        let state = database.stored_sync_status().map_err(|error| SyncError::failed(error.to_string()))?.has_unsynced_changes;
        let assets = database.has_pending_asset_work().map_err(|error| SyncError::failed(error.to_string()))?;
        Ok((state || assets).then_some(PersistedSyncWork { assets }))
    }

    pub(crate) fn has_pending_asset_work(&self) -> SyncResult<bool> {
        self.database()?.has_pending_asset_work().map_err(|error| SyncError::failed(error.to_string()))
    }

    pub fn is_signed_in(&self) -> bool {
        self.credentials.credentials().is_some()
    }

    /// The signed-in user at call time. Nothing is stored: the local library
    /// is authoritative and sync state survives account switches.
    pub fn live_user_id(&self) -> Option<String> {
        self.account.as_ref()?.current().ok()??.user_id().to_owned().into()
    }

    /// Validates that the current session targets this library's server.
    /// Account switches are non-events, so no binding is stored or cleared.
    pub fn bind_account(&self, server_url: &str) -> Result<(), String> {
        if server_url != self.server_url.as_str() {
            return Err(format!("account belongs to {server_url}, but this library syncs with {}", self.server_url));
        }
        Ok(())
    }

    /// This handle observes the app-global account state. Workers adopt the
    /// current identity at execution time, avoiding account broadcasts.
    pub(crate) fn bind_current_account(&self) -> Result<bool, String> {
        let Some(account) = &self.account else {
            return Ok(self.is_signed_in());
        };
        let Some(session) = account.current().map_err(|error| error.to_string())? else {
            return Ok(false);
        };
        self.bind_account(session.server_url().as_str())?;
        Ok(true)
    }

    pub(crate) fn account(&self) -> Option<library_runtime::account::LibraryAccount> {
        self.account.clone()
    }

    /// A library worker, rather than the application registry, reacts to
    /// account changes.  The account object remains device-global, but this
    /// subscription is deliberately consumed only by the library that owns
    /// the corresponding durable work.
    pub(crate) fn subscribe_account_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.account.as_ref().map(|account| account.subscribe())
    }

    /// The remote counterpart belongs to this concrete library. Its durable
    /// name and registration marker are stored alongside the library data.
    pub(crate) async fn ensure_server_library(&self) -> Result<(), String> {
        let account = self.account().ok_or("library has no account capability")?;
        let session = account.require().map_err(|error| error.to_string())?;
        let identity = format!("{}|{}", session.server_url(), session.user_id());
        let database = self.database().map_err(|error| error.to_string())?;
        let Some(name) = database.remote_library_name_to_create(&identity).map_err(|error| error.to_string())? else {
            return Ok(());
        };
        let id = self.library_id;
        account.create_library(id, name).await.map_err(|error| error.to_string())?;
        database.record_remote_library_created(&identity).map_err(|error| error.to_string())?;
        Ok(())
    }

    fn sync_identity(&self) -> SyncResult<(Option<String>, Option<sync_transport::SyncCredentials>)> {
        // Capture identity and token together from the live session.
        let credentials = if let Some(account) = &self.account {
            let session = account.current().map_err(SyncError::failed)?.ok_or(SyncError::AuthenticationRequired)?;
            if session.server_url() != &self.server_url {
                return Err(SyncError::failed("account belongs to another server"));
            }
            Some(sync_transport::SyncCredentials::new(session.server_url().clone(), session.token()))
        } else {
            self.credentials.credentials()
        };
        let user = self.live_user_id();
        Ok((user, credentials))
    }

    pub(crate) async fn repair_inventory(&self) -> SyncResult<()> {
        // Inventory repair must not block foreground metadata synchronization.
        if !self.is_signed_in() {
            return Ok(());
        }
        self.database()?.begin_sync_operation().map_err(SyncError::failed)?;
        let _activity = SyncActivity::begin(&self.metadata_activity, self.event_tx.as_ref());
        let (user, credentials) = self.sync_identity()?;
        let Some(exchange) = host::exchange(self, &user, credentials, SyncMode::Repair).await? else { return Ok(()) };
        // Repair finishes its own workflow but is not a full-sync checkpoint.
        // Any mutations it queued still keep the workflow pending.
        self.database()?.finish_state_sync(None).map_err(SyncError::failed)?;
        if exchange.inventory_checked {
            self.inventory_checked.store(true, Ordering::Release);
        }
        Ok(())
    }

    pub(crate) async fn synchronize_state(&self) -> SyncResult<SyncOutcome> {
        self.synchronize_state_mode(false).await
    }

    pub(crate) async fn drain_outbox(&self) -> SyncResult<SyncOutcome> {
        self.synchronize_state_mode(true).await
    }

    async fn synchronize_state_mode(&self, outbox_only: bool) -> SyncResult<SyncOutcome> {
        let _sync_guard = host::lock(self).await;
        if !self.is_signed_in() {
            return Ok(SyncOutcome::default());
        }
        let _activity = SyncActivity::begin(&self.metadata_activity, self.event_tx.as_ref());
        let _interest = self.notification_interest.work();
        let (user, credentials) = self.sync_identity()?;
        let mode = if outbox_only { SyncMode::Outbox } else { SyncMode::Full };
        let Some(exchange) = host::exchange(self, &user, credentials, mode).await? else { return Ok(SyncOutcome::default()) };
        // This checkpoint covers publishable state. Books awaiting upload keep
        // their mutations in the outbox; upload completion wakes another pass.
        let completed_at = u64::try_from(web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH)?.as_millis())?;
        self.database()?.finish_state_sync(if outbox_only { None } else { Some(completed_at) }).map_err(SyncError::failed)?;
        if exchange.inventory_checked {
            self.inventory_checked.store(true, Ordering::Release);
        }
        Ok(exchange.outcome)
    }

    pub(crate) fn set_asset_storage_enabled(&self, enabled: bool) {
        self.asset_storage_enabled.store(enabled, Ordering::Release);
    }

    pub(crate) fn asset_storage_enabled(&self) -> bool {
        self.asset_storage_enabled.load(Ordering::Acquire)
    }

    pub(crate) fn prepare_asset_storage_sync(&self) -> SyncResult<()> {
        self.database()?.begin_sync_operation().map_err(SyncError::failed)?;
        Ok(())
    }

    pub(crate) async fn reconcile_assets(&self, targeted: Vec<ContentHash>) -> SyncResult<()> {
        let _asset_guard = self.asset_gate.lock().await;
        if !self.is_signed_in() {
            return Ok(());
        }
        let _activity = SyncActivity::begin(&self.asset_activity, self.event_tx.as_ref());
        let result = self.sync_assets(targeted).await;
        result
    }

    #[cfg(test)]
    async fn sync_once(&self) -> SyncResult<()> {
        self.synchronize_state().await?;
        self.reconcile_assets(Vec::new()).await
    }

    pub(crate) fn begin_thumbnail_upload_batch(&self) -> asset_transfer::TransferProducer {
        self.transfer_queue.begin_producer(TransferJobKind::UploadThumbnail)
    }

    pub(crate) fn download_error(&self, hash: ContentHash) -> Option<String> {
        self.transfer_queue.download_error(hash)
    }

    pub fn transfer_statuses(&self) -> Vec<crate::TransferStatus> {
        self.transfer_queue.statuses()
    }

    /// Flush accepted history snapshots before the application releases its libraries.
    pub(crate) fn flush_transfer_history(&self) -> std::io::Result<()> {
        self.transfer_queue.flush_history()
    }

    pub fn transfer_history(&self) -> Vec<crate::TransferStatus> {
        self.transfer_queue.history()
    }

    pub fn request_download(&self, content_hash: ContentHash) -> SyncResult<()> {
        self.request_downloads(std::slice::from_ref(&content_hash))
    }

    /// Session hands the worker its library file: after this call every
    /// transfer runs on snapshots and commits, never on the session handle.
    pub(crate) fn transfer_worker(&self) -> asset_transfer::transfer::TransferWorker {
        asset_transfer::transfer::TransferWorker {
            asset_storage_enabled: self.asset_storage_enabled.clone(),
            assets: self.assets.clone(),
            http_client: self.http_client.clone(),
            library_id: self.library_id,
            event_tx: self.event_tx.clone(),
            queue: self.transfer_queue.clone(),
            credentials: self.credentials.clone(),
            account: self.account.clone(),
            notification_interest: self.notification_interest.clone(),
            availability_changed: self.availability_changed.clone(),
            db_path: self.db_path.clone(),
            cpu: self.cpu.clone(),
            #[cfg(target_arch = "wasm32")]
            progress_key: None,
        }
    }

    pub fn request_downloads(&self, content_hashes: &[ContentHash]) -> SyncResult<()> {
        self.database()?.sync_request_book_downloads(content_hashes, TransferOrigin::UserInitiated)?;
        // Feed an already-running driver immediately, including while it backs off.
        // The durable requests remain authoritative after interruption or restart.
        let jobs = content_hashes.iter().map(|hash| self.prepare_requested_download(hash)).collect::<SyncResult<Vec<_>>>()?;
        self.transfer_queue.enqueue(jobs.into_iter().flatten().collect());
        if let Some(tx) = &self.event_tx {
            for content_hash in content_hashes {
                let _ = tx.try_send(LibraryEvent::DownloadStatusChanged { content_hash: *content_hash, state: crate::DownloadState::Queued });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
