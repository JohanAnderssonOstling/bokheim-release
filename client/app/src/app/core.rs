pub(super) use super::data::{AppDataLocation, BackendContext, LibraryEntry, LibraryRegistry};
use crate::executor::{BackendExecutor, BackendTask};
pub(super) use crate::LibraryId;
use serde::Serialize;
pub(super) use std::collections::HashSet;
pub(super) use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(super) const BACKGROUND_SYNC_RETRY_INTERVAL: Duration = Duration::from_secs(30);

pub(super) use crate::library::{LibrarySyncState, LibrarySyncStatus};
pub(super) use crate::BackendError;
pub(super) use crate::DeviceAccountSession;

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct LibraryListState {
    pub libraries: Vec<LibraryEntry>,
    pub sync_statuses: Vec<LibrarySyncStatus>,
}

pub(super) fn subscribe_event(subscribers: &mut Vec<async_channel::Sender<()>>) -> async_channel::Receiver<()> {
    let (sender, receiver) = async_channel::bounded(1);
    subscribers.push(sender);
    receiver
}

/// One unread notification is sufficient. A full channel remains subscribed.
pub(super) fn invalidate_events(subscribers: &mut Vec<async_channel::Sender<()>>) {
    subscribers.retain(|sender| !matches!(sender.try_send(()), Err(async_channel::TrySendError::Closed(()))));
}

pub(crate) struct AppState {
    pub(super) executor: BackendExecutor,
    pub(super) registry: LibraryRegistry,
    /// Durable registry/account policy remain app-owned. The library module
    /// owns the in-memory directory of resource-bearing sessions.
    pub(super) library_directory: Arc<crate::library::LibraryDirectory>,
    pub(super) account: DeviceAccountSession,
    pub(super) library_list_subscribers: std::sync::Mutex<Vec<async_channel::Sender<()>>>,
    pub(super) account_scheduler: SyncScheduler,
    pub(super) remote_changes: crate::notification_interest::RemoteChanges,
    notification_task: Option<BackendTask<()>>,
    pub(super) notification_interest: crate::notification_interest::NotificationInterest,
}

pub(super) use crate::sync::SyncScheduler;

impl Drop for AppState {
    fn drop(&mut self) {
        self.account_scheduler.stop();
        self.notification_task.take();
        self.library_directory.shutdown_all();
    }
}

#[derive(Clone)]
pub(crate) struct AppBackend {
    pub(super) state: Arc<AppState>,
}

impl AppBackend {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn run_native_worker(self, worker: crate::runtime::native::AppWorker) {
        let executor = self.state.executor.clone();
        executor.block_on(worker.serve(self));
    }

    pub fn initialize(location: AppDataLocation) -> Result<Self, BackendError> {
        Self::new(BackendContext::initialize(location).map_err(BackendError::message)?)
    }

    pub fn new(context: BackendContext) -> Result<Self, BackendError> {
        let executor = BackendExecutor::new().map_err(BackendError::operation)?;
        let notification_interest = crate::notification_interest::NotificationInterest::default();
        let remote_changes = crate::notification_interest::RemoteChanges::default();
        let (account_scheduler, account_wake_rx) = SyncScheduler::new();
        let api_server = crate::api_server_url();
        let account = DeviceAccountSession::load(context.clone(), api_server).map_err(BackendError::operation)?;
        let notification_task = executor.spawn(super::notifications::run(account.clone(), account_scheduler.clone(), notification_interest.clone(), remote_changes.clone()));
        let state = Arc::new(AppState {
            executor: executor.clone(),
            registry: context.registry,
            library_directory: Arc::new(crate::library::LibraryDirectory::default()),
            account: account.clone(),
            library_list_subscribers: std::sync::Mutex::new(Vec::new()),
            account_scheduler: account_scheduler.clone(),
            remote_changes: remote_changes.clone(),
            notification_task: Some(notification_task),
            notification_interest: notification_interest.clone(),
        });
        let weak = Arc::downgrade(&state);
        super::account::tasks::start_maintenance(&executor, &weak, (account_scheduler.clone(), account_wake_rx));
        let core = Self { state };
        core.open_registered_libraries()?;
        core.start_pending_local_purges();
        core.state.account_scheduler.request_remote_refresh();
        Ok(core)
    }

    pub(crate) fn library_list_changes(&self) -> async_channel::Receiver<()> {
        subscribe_event(&mut self.state.library_list_subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    pub(super) fn send_library_list_changed(&self) {
        invalidate_events(&mut self.state.library_list_subscribers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
    }

    /// Runtime routing may resolve an already-open actor here. It cannot open
    /// or configure libraries; that remains the explicit lifecycle path.
    pub(crate) fn library_directory(&self) -> &Arc<crate::library::LibraryDirectory> {
        &self.state.library_directory
    }

    pub(super) fn library_entry(&self, library_id: &LibraryId) -> Result<LibraryEntry, BackendError> {
        self.state.registry.entry(library_id).map_err(BackendError::operation)?.ok_or_else(|| BackendError::message(format!("library not found: {library_id}")))
    }
}

pub(super) fn library_name_for_folder(path: &Path) -> String {
    path.file_name().and_then(|name| name.to_str()).filter(|name| !name.trim().is_empty()).map(str::to_owned).unwrap_or_else(|| path.display().to_string())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod library_client_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn library_calls_bypass_the_app_worker_and_retired_clients_stay_retired() {
        let root = tempfile::tempdir().unwrap();
        let backend = AppBackend::new(BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap()).unwrap();
        let id = *backend.ensure_import_library().unwrap().library_id();
        let (client, worker) = crate::runtime::native::channel(&backend);
        // The app worker is deliberately never served. Library requests must
        // go straight to the already-running library actor.
        let old = client.library(id);
        assert_eq!(tokio::time::timeout(Duration::from_secs(5), old.book_count()).await.unwrap().unwrap(), 0);
        let (updates, _) = old.updates().await.unwrap();
        let hash = crate::ContentHash::new(&format!("{:064x}", 1));
        let (downloads, _) = old.download_changes(hash).await.unwrap();
        backend.library_directory().retire(&id).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while updates.recv().await.is_ok() {}
            while downloads.recv().await.is_ok() {}
        })
        .await
        .expect("retirement must close subscriptions even while the old client is retained");
        assert!(tokio::time::timeout(Duration::from_secs(5), old.book_count()).await.unwrap().is_err());
        backend.open_registered_library(&id).unwrap();
        let fresh = client.library(id);
        assert_eq!(fresh.book_count().await.unwrap(), 0);
        assert!(old.book_count().await.is_err(), "an old client must not retarget to a new actor generation");
        drop(worker);
    }

    #[tokio::test]
    async fn import_destination_skips_a_registered_library_that_cannot_open() {
        let root = tempfile::tempdir().unwrap();
        let context = BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap();
        let unavailable = context.registry.create("Unavailable", None).unwrap();
        let database = context.registry.library_database_locator(unavailable.library_id(), unavailable.storage_locator());
        std::fs::create_dir_all(&database).unwrap();
        let backend = AppBackend::new(context).unwrap();

        let destination = backend.ensure_import_library().unwrap();
        assert_ne!(destination.library_id(), unavailable.library_id());
        assert!(backend.library_directory().get(destination.library_id()).is_ok());
        assert_eq!(backend.libraries().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn failed_local_cleanup_keeps_deleted_library_hidden_and_retryable() {
        let root = tempfile::tempdir().unwrap();
        let context = BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap();
        let entry = context.registry.create("Books", None).unwrap();
        let id = *entry.library_id();
        let blocked_file = std::path::Path::new(entry.storage_locator()).join(".bokheim/library.db");
        std::fs::create_dir(&blocked_file).unwrap();
        let backend = AppBackend::new(context).unwrap();
        assert!(backend.library_directory().get(&id).is_ok());

        assert!(backend.delete_library(&id).await.is_err());
        assert!(backend.libraries().unwrap().is_empty());
        assert!(backend.library_directory().get(&id).is_err());
        assert_eq!(backend.state.registry.pending_local_purges().unwrap(), [id]);

        std::fs::remove_dir(&blocked_file).unwrap();
        backend.state.registry.complete_local_purge(&id).unwrap();
        assert!(backend.state.registry.pending_local_purges().unwrap().is_empty());
    }
}
