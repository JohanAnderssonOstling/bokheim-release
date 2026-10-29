#[cfg(all(test, not(target_arch = "wasm32")))]
pub(super) use super::data::AppDataLocation;
pub(super) use super::data::{BackendContext, LibraryEntry, LibraryRegistry};
use crate::executor::{BackendExecutor, BackendOutput, BackendSend, BackendTask};
use crate::host::startup_profile::StartupProfile;
use crate::AppStartupState;
use client_platform_runtime::transfers;
use library_backend::library::LibraryDirectory;
use serde::Serialize;
pub(super) use std::collections::HashSet;
pub(super) use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(all(test, not(target_arch = "wasm32")))]
use std::time::Duration;
pub(super) use sync_common::LibraryId;
use tokio::sync::watch;

pub(super) use crate::{BackendError, Failure};
pub(super) use crate::DeviceAccountSession;
pub(super) use library_backend::{LibrarySyncState, LibrarySyncStatus};

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct LibraryListState {
    pub libraries: Vec<LibraryEntry>,
    pub sync_statuses: Vec<LibrarySyncStatus>,
}

pub(crate) struct AppState {
    pub(super) executor: BackendExecutor,
    pub(super) registry: LibraryRegistry,
    pub(super) cloud: cloud_storage::connections::CloudConnections,
    pub(super) storage_copy_lock: tokio::sync::Mutex<()>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) cloud_logins: std::sync::Mutex<std::collections::HashMap<uuid::Uuid, super::cloud::CloudLoginReceiver>>,
    /// Durable registry/account policy remain app-owned. The library module
    /// owns the in-memory directory of resource-bearing sessions.
    pub(super) library_directory: Arc<LibraryDirectory>,
    pub(super) account: DeviceAccountSession,
    library_list_changed: watch::Sender<()>,
    /// Coalesces requests for a full account-maintenance pass.
    pub(super) account_wake: watch::Sender<()>,
    account_task: Option<BackendTask<()>>,
    #[cfg(not(target_arch = "wasm32"))]
    root_task: Option<BackendTask<()>>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) root_failures: async_channel::Sender<LibraryId>,
    pub(super) notification_interest: crate::notification_interest::NotificationInterest,
}

impl Drop for AppState {
    fn drop(&mut self) {
        self.account_task.take();
        #[cfg(not(target_arch = "wasm32"))]
        self.root_task.take();
        self.library_directory.shutdown();
    }
}

#[derive(Clone)]
pub(crate) struct AppBackend {
    pub(super) state: Arc<AppState>,
}

impl AppBackend {
    /// The storage owner applies registry policy and the corresponding
    /// directory or account changes together, before publishing the result.
    pub(super) fn with_storage<T: BackendOutput>(&self, operation: impl FnOnce(&Self) -> Result<T, BackendError> + BackendSend + 'static) -> Result<T, BackendError> {
        use client_platform_runtime::storage_queue::{self, Priority};
        let this = self.clone();
        storage_queue::run_sync(Priority::Interactive, move || operation(&this)).map_err(BackendError::message)?
    }

    pub(crate) fn startup_state(&self) -> Result<AppStartupState, BackendError> {
        Ok(AppStartupState {
            libraries: self.libraries().map_err(BackendError::operation)?,
            browsing_preferences: self.browsing_preferences().map_err(BackendError::operation)?,
            account_status: self.account_status().map_err(BackendError::operation)?,
            reader_preferences: self.reader_preferences().map_err(BackendError::operation)?,
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn run_native_worker(self, worker: crate::runtime::native::AppWorker) {
        let executor = self.state.executor.clone();
        executor.block_on(worker.serve(self));
    }

    pub fn new(context: BackendContext) -> Result<Self, BackendError> {
        let core = Self::new_without_libraries(context)?;
        core.open_startup_libraries()?;
        Ok(core)
    }

    pub(crate) fn new_without_libraries(context: BackendContext) -> Result<Self, BackendError> {
        let mut profile = StartupProfile::new("backend_construct");
        let executor = BackendExecutor::new().map_err(BackendError::operation)?;
        profile.mark("executor");
        let notification_interest = crate::notification_interest::NotificationInterest::default();
        let library_directory = Arc::new(LibraryDirectory::default());
        let (account_wake, account_requests) = watch::channel(());
        let api_server = crate::api_server_url();
        let account = DeviceAccountSession::load(context.clone(), api_server).map_err(BackendError::operation)?;
        profile.mark("account_load");
        let cloud = super::cloud::connections(context.registry.clone())?;
        #[cfg(not(target_arch = "wasm32"))]
        let (root_failures, root_requests) = async_channel::unbounded();
        let state = Arc::new_cyclic(|weak| AppState {
            account_task: Some(super::account::start(&executor, weak, account.clone(), account_wake.clone(), account_requests, notification_interest.clone(), library_directory.clone())),
            #[cfg(not(target_arch = "wasm32"))]
            root_task: Some(super::root_recovery::start(&executor, weak.clone(), root_requests)),
            #[cfg(not(target_arch = "wasm32"))]
            root_failures,
            executor: executor.clone(),
            registry: context.registry,
            cloud,
            storage_copy_lock: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            cloud_logins: Default::default(),
            library_directory,
            account: account.clone(),
            library_list_changed: watch::channel(()).0,
            account_wake,
            notification_interest: notification_interest.clone(),
        });
        let core = Self { state };
        profile.mark("state_and_account_task");
        let allow_metered = core.browsing_preferences()?.allow_metered_transfers();
        #[cfg(all(not(target_arch = "wasm32"), not(feature = "kobo")))]
        transfers::initialize(&context.app_data_dir, allow_metered).map_err(BackendError::operation)?;
        #[cfg(all(not(target_arch = "wasm32"), feature = "kobo"))]
        transfers::initialize_device(&context.app_data_dir, allow_metered).map_err(BackendError::operation)?;
        #[cfg(target_arch = "wasm32")]
        transfers::POLICY.set_allow_metered(allow_metered);
        profile.mark("transfer_policy");
        // Catalog queries share one disk database; activate it before libraries open.
        super::catalog::start(&context.app_data_dir, &executor, core.state.library_directory.clone());
        profile.mark("catalog_activate");
        Ok(core)
    }

    pub(crate) fn open_startup_libraries(&self) -> Result<(), BackendError> {
        let mut profile = StartupProfile::new("library_startup");
        self.open_registered_libraries()?;
        profile.mark("libraries_open");
        self.start_pending_local_purges();
        self.state.account_wake.send_replace(());
        profile.mark("maintenance_schedule");
        Ok(())
    }

    pub(crate) fn library_list_changes(&self) -> watch::Receiver<()> {
        self.state.library_list_changed.subscribe()
    }

    pub(crate) fn notification_interest(&self, browsing: bool) -> async_channel::Receiver<()> {
        let (sender, receiver) = async_channel::bounded(1);
        let interest = self.state.notification_interest.view(browsing);
        self.state.executor.spawn_detached(async move {
            let _interest = interest;
            sender.closed().await;
        });
        receiver
    }

    pub(super) fn send_library_list_changed(&self) {
        self.state.library_list_changed.send_replace(());
    }

    /// Runtime routing may resolve an already-open actor here. It cannot open
    /// or configure libraries; that remains the explicit lifecycle path.
    pub(crate) fn library_directory(&self) -> &Arc<LibraryDirectory> {
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
    async fn library_list_watch_tracks_lifecycle_for_independent_subscribers() {
        let root = tempfile::tempdir().unwrap();
        let backend = AppBackend::new(BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap()).unwrap();
        let (client, worker) = crate::runtime::native::channel(&backend);
        let exercise = async move {
            let mut changes = client.library_list_changes().await.unwrap();
            let mut other = client.library_list_changes().await.unwrap();
            assert!(!changes.has_changed().unwrap());

            let entry = client.create_library_with_asset_storage("Books".to_owned(), false).await.unwrap();
            let id = *entry.library_id();
            changes.changed().await.unwrap();
            assert!(other.has_changed().unwrap());
            other.changed().await.unwrap();
            assert_eq!(client.libraries().await.unwrap().len(), 1);

            client.rename_library(id, "First name".to_owned()).await.unwrap();
            client.rename_library(id, "Final name".to_owned()).await.unwrap();
            changes.changed().await.unwrap();
            assert!(!changes.has_changed().unwrap(), "unread changes should coalesce");
            assert!(other.has_changed().unwrap(), "each subscriber tracks changes independently");
            assert_eq!(client.libraries().await.unwrap()[0].library_name(), "Final name");

            drop(other);
            client.delete_library(id).await.unwrap();
            changes.changed().await.unwrap();
            assert!(client.libraries().await.unwrap().is_empty());
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(worker.serve(backend), exercise);
        })
        .await
        .expect("library list subscriptions must remain live through creation, rename, and deletion");
    }

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
        backend.library_directory().retire(&id).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async { while updates.recv().await.is_ok() {} }).await.expect("retirement must close subscriptions even while the old client is retained");
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
    async fn concurrent_imports_choose_one_destination() {
        let root = tempfile::tempdir().unwrap();
        let backend = AppBackend::new(BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap()).unwrap();
        let start = Arc::new(std::sync::Barrier::new(6));
        let clients = (0..6)
            .map(|_| {
                let backend = backend.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    *backend.ensure_import_library().unwrap().library_id()
                })
            })
            .collect::<Vec<_>>();
        let ids = clients.into_iter().map(|client| client.join().unwrap()).collect::<Vec<_>>();
        assert!(ids.iter().all(|id| id == &ids[0]));
        assert_eq!(backend.libraries().unwrap().len(), 1);
        assert_eq!(backend.library_directory().all().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn shutdown_closes_libraries_while_transport_survives() {
        let root = tempfile::tempdir().unwrap();
        let backend = AppBackend::new(BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap()).unwrap();
        let id = *backend.ensure_import_library().unwrap().library_id();
        let (client, worker) = crate::runtime::native::channel(&backend);
        let library = client.library(id);
        let (updates, _) = library.updates().await.unwrap();
        drop(worker);
        drop(backend);
        tokio::time::timeout(Duration::from_secs(5), async { while updates.recv().await.is_ok() {} }).await.expect("application shutdown must close libraries even while a transport retains the directory");
        assert!(library.book_count().await.is_err());
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
