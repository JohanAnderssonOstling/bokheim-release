//! The library's workers. Each is a `run_timed` loop that asks the database
//! for its work; events published through `LibraryEventRouter` wake them.
//! They all run inside the actor's root future and end with it.

use std::future::Future;
use std::time::Duration;

use super::LibrarySession;
use super::events::{LibraryEvent, LibraryEventRouter};
use crate::BackendError;
use client_platform_runtime::transfers;
#[cfg(all(feature = "kobo", target_os = "linux"))]
use client_platform_kobo::device::wifi::{self, Phase as WifiPhase};
#[cfg(not(target_arch = "wasm32"))]
use client_platform_runtime::startup_profile::StartupProfile;
use futures_util::FutureExt;
use library_database::Database;
use library_runtime::scheduler::{CycleInput, SyncRequest};
use library_sync::LibrarySync;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;

/// Wake receivers, one per worker; the router holds the senders.
#[derive(Clone)]
pub(crate) struct WorkerRequests {
    pub(crate) sync: async_channel::Receiver<SyncRequest>,
    pub(crate) assets: async_channel::Receiver<SyncRequest>,
    pub(crate) files: async_channel::Receiver<SyncRequest>,
    pub(crate) enrichment: async_channel::Receiver<SyncRequest>,
}

/// Subscribes immediately, so an account change between opening
/// and the first poll is not missed.
pub(super) fn observe_account(account: crate::integration::LibraryAccount, events: LibraryEventRouter) -> impl Future<Output = ()> {
    let mut account_changed = account.subscribe();
    account_changed.borrow_and_update();
    async move {
        while account_changed.changed().await.is_ok() {
            events.emit(LibraryEvent::ContentsChanged);
            events.sync.request_remote_refresh();
            events.assets.request_remote_refresh();
        }
    }
}

/// Runs every worker until dropped; none of them ends on its own.
pub(super) async fn run_workers(session: Rc<LibrarySession>, requests: WorkerRequests, observers: impl Future<Output = ()>) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut profile = StartupProfile::for_library("library_maintenance", session.id);
        client_platform_runtime::executor::library_maintenance_ready().await;
        profile.mark("ready");
    }
    let session = &*session;
    // Keep each scoped worker on the heap: their state machines contain large
    // parser/network futures, and the owning root must fit a normal thread stack.
    let observers = Box::pin(observers);
    let network = Box::pin(observe_network(session.events.clone()));
    let mut transfer_changes = transfers::POLICY.subscribe();
    let transfer_policy = async move {
        while transfer_changes.changed().await.is_ok() {
            session.events.assets.wake();
            if transfers::allowed() {
                session.events.notify_contents_changed();
            }
        }
    };
    let sync = Box::pin(run_timed("synchronization", move |input| sync_pass(session, input), requests.sync));
    let assets = Box::pin(run_timed("asset transfers", move |_| asset_pass(session), requests.assets));
    let files = Box::pin(run_timed("file replay", move |_| replay_and_admit(session), requests.files));
    let enrichment = Box::pin(run_timed("enrichment", move |_| session.enrichment.run(), requests.enrichment));
    #[cfg(not(target_arch = "wasm32"))]
    let scan = Box::pin(run_scan_worker(session));
    #[cfg(target_arch = "wasm32")]
    let scan = async {};
    #[cfg(not(target_arch = "wasm32"))]
    let torrents = Box::pin(session.torrents.run(session));
    #[cfg(target_arch = "wasm32")]
    let torrents = async {};
    futures_util::join!(observers, network, transfer_policy, sync, assets, files, enrichment, scan, torrents);
}

async fn observe_network(_events: LibraryEventRouter) {
    #[cfg(all(feature = "kobo", target_os = "linux"))]
    {
        let Ok(mut changes) = wifi::subscribe() else { return; };
        let mut was_connected = false;
        loop {
            let connected = changes.borrow_and_update().phase == WifiPhase::Connected;
            if connected && !was_connected {
                _events.sync.request_remote_refresh();
                _events.assets.request_remote_refresh();
            }
            was_connected = connected;
            if changes.changed().await.is_err() { break; }
        }
    }
}

/// Wakes within 100 ms coalesce. Kobo backs repeated failures off to 15 minutes.
async fn run_timed<F: Future<Output = Result<(), ()>>>(name: &str, mut pass: impl FnMut(CycleInput) -> F, requests: async_channel::Receiver<SyncRequest>) {
    #[cfg(feature = "kobo")]
    library_runtime::scheduler::run_timed_with_backoff(async move |input| recover_pass(name, async { pass(input).await }).await, requests, Duration::from_secs(30), Duration::from_secs(15 * 60), Duration::from_millis(100)).await;
    #[cfg(not(feature = "kobo"))]
    library_runtime::scheduler::run_timed(async move |input| recover_pass(name, async { pass(input).await }).await, requests, Duration::from_secs(30), Duration::from_millis(100)).await;
}

async fn wait_for_network() -> Result<(), ()> {
    #[cfg(all(feature = "kobo", target_os = "linux"))]
    wifi::wait_for_connection().await.map_err(|error| log::warn!("could not observe network availability: {error}"))?;
    Ok(())
}

/// These passes reconstruct their work from committed database state. Drop
/// the failed future and its activity guards before scheduling another pass.
/// Panic recovery applies when the target supports unwinding.
async fn recover_pass(name: &str, pass: impl Future<Output = Result<(), ()>>) -> Result<(), ()> {
    AssertUnwindSafe(pass).catch_unwind().await.unwrap_or_else(|_| {
        log::error!("library {name} panicked; queued work will retry");
        Err(())
    })
}

/// Create the remote library once for the current server and user.
async fn ensure_remote_library(sync: &LibrarySync, database: &Database) -> Result<(), BackendError> {
    let account = sync.account().ok_or_else(|| BackendError::message("library has no account capability"))?;
    let session = account.require().map_err(crate::BackendError::operation)?;
    let identity = format!("{}|{}", session.server_url(), session.user_id());
    if database.sync_metadata("server_library_created")?.as_deref() == Some(identity.as_str()) {
        return Ok(());
    }
    let name = database.sync_metadata("remote_library_name")?.ok_or_else(|| BackendError::message("library has no remote name"))?;
    account.create_library(sync.library_id, name, database.storage_policy()?).await.map_err(crate::BackendError::operation)?;
    database.set_sync_metadata("server_library_created", &identity)?;
    Ok(())
}

async fn sync_pass(session: &LibrarySession, input: CycleInput) -> Result<(), ()> {
    wait_for_network().await?;
    let sync = &session.sync;
    let result = async {
        if !sync.bind_current_account().map_err(crate::BackendError::operation)? {
            // Account discovery can lag behind library startup.
            // This is a deferred pass, not a successful sync:
            // returning an error preserves the scheduler retry
            // until the account observer wakes us on sign-in.
            return Err(BackendError::message("waiting for account session"));
        }
        ensure_remote_library(sync, &session.db).await?;
        if input.remote { sync.synchronize_state_mode(false).await.map(|_| ()).map_err(crate::BackendError::operation) } else { sync.drain_outbox().await.map(|_| ()).map_err(crate::BackendError::operation) }
    }
    .await;
    // Unrelated sync work may run before a reading position's quiet period ends.
    // Arrange another pass even if no further page turn or event arrives.
    match session.db.sync_queued_reading_positions() {
        Ok(queued) => {
            let policy = session.events.reading_positions.borrow().clone();
            let discarded = policy.no_longer_queued(&queued);
            if !discarded.is_empty() {
                session.events.emit(LibraryEvent::ReadingPositionsSelected(discarded));
            }
            if let Some(delay) = policy.delay(web_time::Instant::now(), &queued) {
                session.events.sync.wake_after(delay);
            }
        }
        Err(error) => {
            log::warn!("could not schedule reading-position sync; retrying: {error}");
            // A successful network pass must not suppress recovery of the
            // pending reading-position deadlines after a storage failure.
            session.events.sync.wake_after(Duration::from_secs(30));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    if result.is_ok() {
        let _torrents = if session.db.native_directory_replay_work().map_err(|error| log::warn!("could not read directory moves: {error}"))?.is_empty() {
            None
        } else {
            Some(session.torrents.before_moves().await.map_err(|error| log::warn!("could not release torrent files: {error}"))?)
        };
        library_placement::replay_directories(&session.db, &session.assets).map_err(|error| log::warn!("directory rename will retry from the durable queue: {error}"))?;
    }
    result.map_err(|error| log::warn!("library synchronization will resume: {error}"))
}

async fn asset_pass(session: &LibrarySession) -> Result<(), ()> {
    wait_for_network().await?;
    if session.db.storage_policy().map_err(|error| log::warn!("could not read storage provider: {error}"))?.external().is_some() {
        return session.run_cloud_pass().await.map_err(|error| log::warn!("cloud library pass will retry: {error}"));
    }
    if !session.sync.bind_current_account().map_err(|error| log::warn!("could not bind asset account: {error}"))? {
        return Ok(());
    }
    match session.sync.run_asset_pass().await {
        Ok(()) | Err(asset_transfer::TransferError::AuthenticationRequired) => Ok(()),
        Err(error) => {
            log::warn!("asset pass will retry: {error}");
            Err(())
        }
    }
}

/// File replay, then upload admission. A scan or folder import owns the
/// files meanwhile; their completion events wake this worker again.
async fn replay_and_admit(session: &LibrarySession) -> Result<(), ()> {
    if session.scanner_running() {
        return Ok(());
    }
    let mut failed = false;
    if session.assets.local_access_enabled() {
        let result = AssertUnwindSafe(async {
            let Some(_files) = session.runtime.begin_local_work() else {
                return Err(BackendError::message("local file preparation is already running"));
            };
            session.run_file_jobs().await
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(BackendError::message("library file replay panicked")));
        if let Err(error) = &result {
            log::warn!("library file replay will resume: {error}");
        }
        // A lease can leave durable work queued even after a successful pass.
        failed = result.is_err() || session.db.has_pending_file_work().unwrap_or(true) || session.db.has_pending_purge_work().unwrap_or(true);
    }
    // Admission never waits for a failed replay of other books.
    if session.db.storage_policy().map_err(|error| log::warn!("could not read storage provider: {error}"))?.external().is_none() {
        if let Err(error) = session.admit_uploads() {
            log::warn!("upload admission will resume: {error}");
            return Err(());
        }
    }
    session.emit(LibraryEvent::FilesPrepared);
    if failed { Err(()) } else { Ok(()) }
}

#[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
type FolderWatcher = notify::RecommendedWatcher;
#[cfg(all(not(feature = "watcher"), not(target_arch = "wasm32")))]
type FolderWatcher = ();

/// Scans on open, on folder changes, and after failures.
#[cfg(not(target_arch = "wasm32"))]
async fn run_scan_worker(session: &LibrarySession) {
    if session.db.storage_policy().is_ok_and(|storage| storage.external().is_some()) { return; }
    let (wake, requests) = library_runtime::scheduler::SyncScheduler::new();
    let mut watcher = None;
    #[cfg(feature = "watcher")]
    if session.assets.local_access_enabled() {
        watcher = super::handle::start_filesystem_watcher(session._scanner.root(), wake.clone()).map_err(|error| log::warn!("could not watch library folder {}: {error}", session._scanner.root())).ok();
    }
    wake.wake();
    let wake = &wake;
    // The debounce is the quiet period while a file manager is still copying.
    library_runtime::scheduler::run_timed(async move |_| recover_pass("filesystem scan", async { scan_and_watch(session, wake, &mut watcher).await }).await, requests, Duration::from_secs(30), Duration::from_millis(500)).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn scan_and_watch(session: &LibrarySession, wake: &library_runtime::scheduler::SyncScheduler, watcher: &mut Option<FolderWatcher>) -> Result<(), ()> {
    // Explicit imports own their in-progress files, including external moves.
    if session.directory_imports_active() {
        return Ok(());
    }
    match session.scan_folder().await {
        Ok(super::handle::FolderScan::Complete) => {},
        // File replay owns excluded paths. Release the scan's busy state so
        // replay can finish, then verify those files even without a watch event.
        Ok(super::handle::FolderScan::Deferred) => wake.wake_after(Duration::from_secs(1)),
        Err(error) => {
            log::warn!("filesystem scan failed; retrying in 30 seconds: {error}");
            #[cfg(feature = "watcher")]
            if !std::path::Path::new(session._scanner.root()).is_dir() {
                *watcher = None;
            }
            return Err(());
        }
    }
    session.torrents.wake();
    #[cfg(feature = "watcher")]
    if watcher.is_none() && session.assets.local_access_enabled() {
        let started = super::handle::start_filesystem_watcher(session._scanner.root(), wake.clone()).map_err(|error| log::warn!("could not watch library folder {}: {error}", session._scanner.root())).ok();
        // Recheck files that may have arrived before registration.
        wake.wake_after(if started.is_some() { Duration::from_millis(250) } else { Duration::from_secs(30) });
        *watcher = started;
    }
    #[cfg(not(feature = "watcher"))]
    let _ = (wake, watcher);
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::library::session::LibraryRuntime;
    use library_runtime::scheduler::SyncScheduler;

    #[tokio::test]
    async fn worker_retries_factory_and_poll_panics_and_releases_activity() {
        let runtime = Rc::new(LibraryRuntime::default());
        let (scheduler, requests) = SyncScheduler::new();
        let (completed, results) = async_channel::unbounded();
        let mut attempts = 0;
        scheduler.wake();
        let worker = run_timed("recovery test", move |_| {
            attempts += 1;
            let attempt = attempts;
            let activity = runtime.begin_local_work().expect("failed passes must release their lane");
            if attempt == 1 {
                completed.try_send(attempt).unwrap();
                panic!("failed while creating the pass");
            }
            let completed = completed.clone();
            async move {
                let _activity = activity;
                tokio::task::yield_now().await;
                completed.try_send(attempt).unwrap();
                if attempt == 2 {
                    panic!("failed while polling the pass");
                }
                Ok(())
            }
        }, requests);
        let exercise = async {
            for attempt in 1..=3 {
                assert_eq!(results.recv().await.unwrap(), attempt);
                if attempt < 3 {
                    // A fresh refresh bypasses the normal retry delay.
                    scheduler.request_remote_refresh();
                }
            }
            scheduler.stop();
        };
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(worker, exercise) }).await.expect("a failed pass must not end or strand the worker");
    }
}
