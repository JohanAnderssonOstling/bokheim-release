//! Public handle for operations scoped to one concrete library.

use std::sync::Arc;

use crate::asset_store::AssetStore;
use crate::library::commands::LibraryCommand;
use crate::library::discovery::LibraryScanner;
use crate::library::events::{LibraryEvent, LibraryEventReceiver, LibraryEventSender, LibraryEventSubscriptions};
use crate::library::session::LibraryActivity;
use crate::{ContentHash, DirId, DownloadState, LibraryId};
use library_database::Database;

use super::commands::PreparedRequest;
use super::pending::{BackgroundJob, Inbox, OperationKind, PendingOperations, PendingReply};
use crate::BackendError;
use futures_util::FutureExt;

/// All operations whose meaning is scoped to one concrete library.
///
/// Application workflows supply policy and coordinate networking; this handle
/// has no reference back to the application.
pub struct LibrarySession {
    pub(super) id: LibraryId,
    pub(super) db: std::sync::Arc<Database>,
    pub(super) assets: AssetStore,
    metadata: metadata_client::MetadataClient,
    sync: Option<Arc<crate::sync::LibrarySync>>,
    event_tx: LibraryEventSender,
    event_rx: Option<LibraryEventReceiver>,
    subscriptions: LibraryEventSubscriptions,
    runtime: Arc<crate::library::session::LibraryRuntime>,
    /// All worker lifetimes are owned by this one concrete session.
    workers: super::workers::LibraryWorkers,
    pub(super) sync_wake: library_runtime::scheduler::SyncScheduler,
    pub(super) sync_requests: Option<async_channel::Receiver<library_runtime::scheduler::SyncRequest>>,
    pub(super) asset_wake: library_runtime::scheduler::SyncScheduler,
    pub(super) asset_requests: Option<async_channel::Receiver<library_runtime::scheduler::SyncRequest>>,
    remote_changes: Option<crate::notification_interest::RemoteChanges>,
    /// Kept only for library-owned accounting. The app registry never opens a
    /// library path merely to calculate a settings total.
    // Its Drop implementation stops and joins the scanner threads.
    pub(super) _scanner: Arc<LibraryScanner>,
    pub(super) cpu: std::sync::Arc<dyn cpu_host::CpuHost>,
    filesystem_scanning: std::cell::Cell<bool>,
    pub(super) file_work_error: std::cell::RefCell<Option<String>>,
}

#[cfg(not(target_arch = "wasm32"))]
type ScanResult = Result<Option<library_database::ScanApplyResult>, crate::BackendError>;

#[cfg(not(target_arch = "wasm32"))]
async fn publish_scan_batches(session: std::rc::Rc<LibrarySession>, mut snapshot: library_database::ScanSnapshot, mut discovery: library_database::FilesystemScan) -> ScanResult {
    let mut readable = discovery.readable;
    for files in discovery.files.chunks(filesystem_scanner::filesystem_scan::PUBLISH_BATCH_SIZE) {
        let chunk = library_database::FilesystemScan { files: files.to_vec(), readable: discovery.readable, ..Default::default() };
        let inspected = session._scanner.inspect(&chunk, &snapshot, &session.cpu).await;
        readable &= inspected.readable;
        if !inspected.books.is_empty() || !inspected.failures.is_empty() {
            let result = session.db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &inspected, false).map_err(crate::BackendError::operation)?;
            if result == library_database::ScanApplyResult::Stale {
                return Ok(Some(result));
            }
            discovery.directories.clear();
            session.emit(LibraryEvent::ContentsChanged);
        }
        // Deliver queries and partial contents before inspecting another batch.
        tokio::task::yield_now().await;
    }
    let result = session.db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &library_database::InspectedFilesystemScan { failures: vec![], readable, books: vec![] }, true).map_err(crate::BackendError::operation)?;
    if result == library_database::ScanApplyResult::Applied && !discovery.readable {
        return Err(crate::BackendError::message("filesystem scan was incomplete"));
    }
    Ok(Some(result))
}

enum OperationCompletion {
    Done,
    #[cfg(target_arch = "wasm32")]
    Book {
        reply: PendingReply<(client_runtime::reply::Reply, u64)>,
        result: Result<super::book::Opened, BackendError>,
    },
}

#[cfg(target_arch = "wasm32")]
type ScanResult = Result<(), BackendError>;

#[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
fn filesystem_event_needs_scan(event: &notify::Event, root: &std::path::Path) -> bool {
    if event.kind.is_access() {
        return false;
    }
    if event.need_rescan() || event.paths.is_empty() {
        return true;
    }
    event.paths.iter().any(|path| {
        path.strip_prefix(root).ok().is_some_and(|relative| {
            relative.components().all(|component| !component.as_os_str().to_string_lossy().starts_with('.'))
        })
    })
}

#[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
fn start_filesystem_watcher(root: &str, sender: async_channel::Sender<()>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let root = std::path::PathBuf::from(root);
    let watched_root = root.clone();
    let mut watcher = match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        match event {
            Ok(event) if filesystem_event_needs_scan(&event, &watched_root) => { let _ = sender.try_send(()); }
            Err(error) => {
                log::warn!("library filesystem watcher error: {error}");
                let _ = sender.try_send(());
            }
            _ => {}
        }
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            log::warn!("could not start library filesystem watcher: {error}");
            return None;
        }
    };
    if let Err(error) = watcher.watch(&root, notify::RecursiveMode::Recursive) {
        log::warn!("could not watch library folder {}: {error}", root.display());
        return None;
    }
    Some(watcher)
}

struct LibraryActor {
    operations: PendingOperations<OperationCompletion>,
    session: std::rc::Rc<LibrarySession>,
    // Poll preparation beside commands/events, and keep filesystem replay
    // independent so network enrichment cannot hold up physical folder moves.
    enrichment_job: BackgroundJob<Result<(), BackendError>>,
    file_job: BackgroundJob<Result<(), BackendError>>,
    local_retry: BackgroundJob<()>,
    enrichment_retry: BackgroundJob<()>,
    scan_job: BackgroundJob<ScanResult>,
    watch_debounce: BackgroundJob<()>,
    watch_events: async_channel::Receiver<()>,
    _watch_events_sender: async_channel::Sender<()>,
    #[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
    _watcher: Option<notify::RecommendedWatcher>,
    /// Browser book readers and Blob URLs are thread-local resources.
    /// The actor retains them for the lifetime of the page subscription; only
    /// a Send cancellation guard leaves the actor.
    #[cfg(target_arch = "wasm32")]
    books: super::book::browser::BookResources,
}

impl Drop for LibraryActor {
    fn drop(&mut self) {
        self.session.subscriptions.close();
    }
}

impl LibraryActor {
    fn new(session: LibrarySession) -> Self {
        let (watch_events_sender, watch_events) = async_channel::bounded(1);
        #[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
        let watcher = if session.assets.local_access_enabled() {
            start_filesystem_watcher(session._scanner.root(), watch_events_sender.clone())
        } else {
            None
        };
        Self {
            session: std::rc::Rc::new(session),
            operations: Default::default(),
            scan_job: Default::default(),
            watch_debounce: Default::default(),
            watch_events,
            _watch_events_sender: watch_events_sender,
            #[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
            _watcher: watcher,
            enrichment_job: Default::default(),
            file_job: Default::default(),
            local_retry: Default::default(),
            enrichment_retry: Default::default(),
            #[cfg(target_arch = "wasm32")]
            books: Default::default(),
        }
    }

    // Commands never suspend this loop. It owns pending operations and polls
    // their completions beside events, scans, and preparation.
    async fn run(mut self, commands: async_channel::Receiver<LibraryActorCommand>, events: LibraryEventReceiver, event_commands: async_channel::Sender<LibraryActorCommand>) {
        let commands = Inbox(commands);
        let sync_wake = self.session.sync_wake.clone();
        let asset_wake = self.session.asset_wake.clone();
        loop {
            // Retirement closes admission immediately; do not execute a
            // backlog of commands against an already-retired generation.
            if commands.0.is_closed() {
                break;
            }
            tokio::select! {
                event = self.watch_events.recv() => if event.is_ok() {
                    // Reset the quiet period while a file manager is still copying.
                    self.watch_debounce.task = Some(Box::pin(crate::executor::sleep(std::time::Duration::from_millis(500))));
                },
                _ = self.watch_debounce.next() => {
                    self.watch_debounce.finish();
                    self.begin_scan();
                },
                Some(completion) = self.operations.next(), if !self.operations.is_empty() => self.complete_operation(completion),
                _ = self.local_retry.next() => { self.local_retry.finish(); self.begin_file_work(); },
                _ = self.enrichment_retry.next() => { self.enrichment_retry.finish(); self.begin_enrichment(); },
                result = self.file_job.next() => self.finish_file_work(result),
                result = self.enrichment_job.next() => self.finish_enrichment(result),
                result = self.scan_job.next() => self.finish_scan(result),
                event = events.recv() => match event {
                    Ok(event) => self.publish_event(event, &event_commands),
                    Err(_) => break,
                },
                command = commands.0.recv() => match command {
                    Ok(command) => {
                        if matches!(&command, LibraryActorCommand::Ui { command: LibraryCommand::DownloadState { .. }, .. }) {
                            // A transfer may have committed while its earlier
                            // queued/progress events still sit in this inbox.
                            // Publish those before taking a subscription
                            // baseline, so they cannot replay after it.
                            while let Ok(event) = events.try_recv() {
                                self.publish_event(event, &event_commands);
                            }
                        }
                        if !self.handle_command(command, &sync_wake, &asset_wake) {
                            break;
                        }
                    }
                    Err(_) => break,
                },
            }
        }
    }

    fn submit<T: 'static>(&mut self, kind: OperationKind, reply: PendingReply<T>, operation: impl std::future::Future<Output = Result<T, BackendError>> + 'static) {
        self.operations.submit(kind, reply, operation, |reply, result| {
            reply.finish(result);
            OperationCompletion::Done
        });
    }

    fn complete_operation(&mut self, completion: OperationCompletion) {
        match completion {
            OperationCompletion::Done => {}
            #[cfg(target_arch = "wasm32")]
            OperationCompletion::Book { reply, result } => {
                if reply.is_closed() {
                    return;
                }
                match result {
                    Ok((book, lease)) => {
                        // Both registration and reply occur synchronously on the
                        // browser thread, so cancellation cannot interleave.
                        let id = self.books.retain(lease);
                        reply.finish(Ok((client_runtime::reply::Reply::new(book), id)));
                    }
                    Err(error) => reply.finish(Err(error)),
                }
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn begin_scan(&mut self) {
        if self.scan_job.task.is_some() {
            self.scan_job.pending = true;
            return;
        }
        self.session.filesystem_scanning.set(true);
        let session = self.session.clone();
        self.scan_job.task = Some(Box::pin(async move { std::panic::AssertUnwindSafe(session.start_scanner()).catch_unwind().await.map_err(|_| BackendError::message("library scan panicked")) }));
    }

    #[cfg(target_arch = "wasm32")]
    fn finish_scan(&mut self, result: ScanResult) {
        self.scan_job.finish();
        self.session.filesystem_scanning.set(false);
        if let Err(error) = result {
            log::warn!("library scan failed: {error}");
        }
        self.session.emit(LibraryEvent::ScanComplete);
    }

    fn begin_file_work(&mut self) {
        if !self.session.assets.local_access_enabled() {
            self.admit_and_enrich();
            return;
        }
        if self.session.scanner_running() {
            self.local_retry.retry_after(30);
            return;
        }
        if !self.file_job.begin() {
            return;
        }
        let session = self.session.clone();
        self.file_job.task = Some(Box::pin(async move {
            std::panic::AssertUnwindSafe(async {
                // A scan can start after this job was queued but before polling.
                if session.scanner_running() {
                    return Err(BackendError::message("Waiting for library scan"));
                }
                let Some(_files) = session.begin_local_work() else {
                    return Err(BackendError::message("local file preparation is already running"));
                };
                session.run_file_jobs().await
            })
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(BackendError::message("library file replay panicked")))
        }));
    }

    fn finish_file_work(&mut self, result: Result<(), BackendError>) {
        let pending = self.file_job.finish();
        *self.session.file_work_error.borrow_mut() = result.as_ref().err().map(ToString::to_string);
        self.session.emit(LibraryEvent::TransferStatusChanged);
        if let Err(error) = result {
            log::warn!("library file replay will resume: {error}");
            self.local_retry.retry_after(30);
        }
        // A lease can leave durable work queued even after a successful pass.
        if self.session.pending_file_jobs().unwrap_or(true) {
            self.local_retry.retry_after(30);
        }
        // Enrichment follows local replay, but never prevents a later replay.
        self.admit_and_enrich();
        if pending {
            self.begin_file_work();
        }
        // File replay can be the final durable dependency of an import. The
        // completion record belongs to this worker lifecycle, not a future UI
        // transfer-status poll.
        if let Err(error) = self.session.db.reconcile_operation(library_database::OperationActivity::default()) {
            log::warn!("could not reconcile library operation after file replay: {error}");
        }
    }

    fn admit_and_enrich(&mut self) {
        if let Err(error) = self.session.admit_uploads() {
            log::warn!("upload admission will resume: {error}");
            self.local_retry.retry_after(30);
            return;
        }
        self.begin_enrichment();
    }

    fn begin_enrichment(&mut self) {
        if !self.enrichment_job.begin() {
            return;
        }
        let session = self.session.clone();
        self.enrichment_job.task = Some(Box::pin(async move { std::panic::AssertUnwindSafe(session.run_enrichment()).catch_unwind().await.unwrap_or_else(|_| Err(BackendError::message("library enrichment panicked"))) }));
    }

    fn finish_enrichment(&mut self, result: Result<(), BackendError>) {
        let pending = self.enrichment_job.finish();
        if let Err(error) = &result {
            log::warn!("library enrichment will resume: {error}");
        }
        if result.is_err() || self.session.assets.local_access_enabled() && self.session.pending_thumbnail_jobs().unwrap_or(true) {
            self.enrichment_retry.retry_after(30);
        } else if self.session.db.pending_audible_job_count().unwrap_or(1) > 0 {
            self.enrichment_retry.retry_after(300);
        }
        if pending {
            self.begin_enrichment();
        }
        if let Err(error) = self.session.db.reconcile_operation(library_database::OperationActivity::default()) {
            log::warn!("could not reconcile library operation after enrichment: {error}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn begin_scan(&mut self) {
        if self.scan_job.task.is_some() {
            self.scan_job.pending = true;
            return;
        }
        if !self.session.assets.local_access_enabled() || !self.session._scanner.enabled() {
            self.session.filesystem_scanning.set(false);
            return;
        }
        self.session.filesystem_scanning.set(true);
        let snapshot = match self.session.db.scan_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                log::warn!("could not create filesystem scan snapshot; retrying in 30 seconds: {error}");
                self.retry_scan(std::time::Duration::from_secs(30));
                return;
            }
        };
        self.session.emit(LibraryEvent::ScanStarted);
        let scanner = self.session._scanner.clone();
        let session = self.session.clone();
        self.scan_job.task = Some(Box::pin(async move {
            // Walking directories and fingerprinting files is blocking I/O.
            // Keep it off the library actor's thread as well as its command loop.
            let (snapshot, discovery) = crate::executor::run_blocking({
                let scanner = scanner.clone();
                move || {
                    let discovery = scanner.discover(&snapshot);
                    (snapshot, discovery)
                }
            })
            .await
            .map_err(crate::BackendError::operation)?;
            std::panic::AssertUnwindSafe(publish_scan_batches(session, snapshot, discovery)).catch_unwind().await.unwrap_or_else(|_| Err(BackendError::message("library scan panicked")))
        }));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn retry_scan(&mut self, delay: std::time::Duration) {
        self.scan_job.task = Some(Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(None)
        }));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn finish_scan(&mut self, result: ScanResult) {
        let pending = self.scan_job.finish();
        let result = match result {
            Ok(None) => {
                self.begin_scan();
                return;
            }
            Ok(Some(result)) => Ok(result),
            Err(error) => Err(error),
        };
        match result {
            Ok(library_database::ScanApplyResult::Applied) => {
                self.session.filesystem_scanning.set(false);
                self.session.emit(LibraryEvent::ContentsChanged);
                self.session.emit(LibraryEvent::ScanComplete);
                let mut next_scan = pending.then_some(std::time::Duration::from_millis(250));
                #[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
                if self._watcher.is_none() && self.session.assets.local_access_enabled() {
                    self._watcher = start_filesystem_watcher(self.session._scanner.root(), self._watch_events_sender.clone());
                    // Recheck files that may have arrived before registration.
                    next_scan = Some(if self._watcher.is_some() { std::time::Duration::from_millis(250) } else { std::time::Duration::from_secs(30) });
                }
                if let Some(delay) = next_scan {
                    self.retry_scan(delay);
                }
            }
            Ok(library_database::ScanApplyResult::Stale) => {
                // Coalesce changes while allowing the actor to serve requests.
                self.retry_scan(std::time::Duration::from_millis(250));
            }
            Err(error) => {
                log::warn!("filesystem scan failed; retrying in 30 seconds: {error}");
                #[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
                if !std::path::Path::new(self.session._scanner.root()).is_dir() {
                    self._watcher = None;
                }
                self.retry_scan(std::time::Duration::from_secs(30));
            }
        }
    }

    /// Event book is part of the library actor, not a second worker
    /// that borrows session state through shared runtime objects.
    fn publish_event(&self, event: LibraryEvent, commands: &async_channel::Sender<LibraryActorCommand>) {
        let busy = self.session.runtime.has_directory_imports() || self.session.scanner_running();
        self.session.subscriptions.publish(event.clone(), busy);
        if matches!(event, LibraryEvent::ContentsChanged | LibraryEvent::ImportComplete | LibraryEvent::ScanComplete | LibraryEvent::DownloadStatusChanged { state: crate::DownloadState::Downloaded, .. }) {
            let hashes = match &event {
                LibraryEvent::DownloadStatusChanged { content_hash, .. } => vec![*content_hash],
                _ => Vec::new(),
            };
            let _ = commands.try_send(LibraryActorCommand::Control(LibraryControl::WakeAssets(hashes)));
        }
        // A scan can publish batches, but its final completion can still
        // leave durable lifecycle and metadata mutations that need a sync
        // pass before dependent asset work (such as cover uploads) is ready.
        if matches!(event, LibraryEvent::ContentsChanged | LibraryEvent::ImportComplete | LibraryEvent::ScanComplete) {
            let _ = commands.try_send(LibraryActorCommand::Control(LibraryControl::WakeSync { refresh_remote: false }));
        }
    }

    /// Returns false only when the session has been asked to shut down.
    fn handle_command(&mut self, command: LibraryActorCommand, sync_wake: &library_runtime::scheduler::SyncScheduler, asset_wake: &library_runtime::scheduler::SyncScheduler) -> bool {
        match command {
            LibraryActorCommand::Ui { command, reply } => {
                let reply = PendingReply::new(reply);
                match super::commands::prepare_library_request(self.session.clone(), command) {
                    Ok(PreparedRequest::Ready(value)) => reply.finish(Ok(value)),
                    Ok(PreparedRequest::Pending(kind, operation)) => self.submit(kind, reply, operation),
                    Err(error) => reply.finish(Err(error)),
                }
            }
            #[cfg(target_arch = "wasm32")]
            LibraryActorCommand::SubscribeBook { content_hash, reply } => {
                let session = self.session.clone();
                self.operations.submit(OperationKind::Read, PendingReply::new(reply), async move { session.prepare_book_for_reader(content_hash).await }, |reply, result| OperationCompletion::Book { reply, result });
            }
            #[cfg(target_arch = "wasm32")]
            LibraryActorCommand::ReleaseBook { id } => {
                self.books.release(id);
            }
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::ResolveBook { content_hash, reply } => {
                let session = self.session.clone();
                self.submit(OperationKind::Read, PendingReply::new(reply), async move { session.prepare_book_for_reader(content_hash).await });
            }
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::ResolveAudiobookTrack { content_hash, index, reply } => {
                let session = self.session.clone();
                self.submit(OperationKind::Read, PendingReply::new(reply), async move { session.prepare_audiobook_track(content_hash, index).await });
            }
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::ImportDirectory { parent_id, directory, create_root, activity, reply } => {
                let queued = std::time::Instant::now();
                log::info!(target: "import_timing", "import_command_received");
                let session = self.session.clone();
                let cancelled = reply.clone();
                self.submit(OperationKind::Import, PendingReply::new(reply), async move {
                    log::info!(target: "import_timing", "import_admitted queue_ms={:.3}", queued.elapsed().as_secs_f64() * 1000.0);
                    session.import_selected_directory(parent_id, directory, create_root, activity, &|| cancelled.is_closed()).await
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::ImportBookPath { parent_id, file_name, path, reply } => {
                let session = self.session.clone();
                self.submit(OperationKind::Import, PendingReply::new(reply), async move { session.import_book_path(&parent_id, file_name, &path).await });
            }
            LibraryActorCommand::Control(LibraryControl::WakeSync { refresh_remote: true }) => sync_wake.request_remote_refresh(),
            LibraryActorCommand::Control(LibraryControl::WakeSync { refresh_remote: false }) => sync_wake.wake(),
            LibraryActorCommand::Control(LibraryControl::WakeAssets(hashes)) => {
                if hashes.is_empty() {
                    asset_wake.wake();
                } else {
                    asset_wake.wake_hashes(hashes);
                }
                self.begin_file_work();
            }
            LibraryActorCommand::Control(LibraryControl::StartScanner) => {
                self.begin_scan();
            }
            LibraryActorCommand::Control(LibraryControl::SetAssetStorage { enabled, reset_cloud_presence }) => {
                let result = (|| -> Result<(), BackendError> {
                    self.session.db.set_asset_storage_policy(enabled, reset_cloud_presence)?;
                    self.session.sync().ok_or_else(|| BackendError::message("library sync is unavailable"))?.set_asset_storage_enabled(enabled);
                    Ok(())
                })();
                if let Err(error) = result {
                    log::warn!("could not update cloud-storage policy for {}: {error}", self.session.id);
                } else {
                    asset_wake.wake();
                    sync_wake.request_remote_refresh();
                }
            }
            LibraryActorCommand::Control(LibraryControl::RepairInventory) => {
                if let Some(sync) = self.session.sync() {
                    let (reply, _ignored) = async_channel::bounded(1);
                    self.operations.submit(OperationKind::Maintenance, PendingReply::new(reply), async move { sync.repair_inventory().await.map_err(BackendError::operation) }, |reply, result| {
                        if let Err(error) = &result {
                            log::warn!("library inventory repair will resume: {error}");
                        }
                        reply.finish(result);
                        OperationCompletion::Done
                    });
                }
            }
            LibraryActorCommand::Control(LibraryControl::Rename { name, refresh_remote }) => {
                if let Err(error) = self.session.db.set_remote_library_name(&name) {
                    log::warn!("could not update the remote name for {}: {error}", self.session.id);
                } else if refresh_remote {
                    sync_wake.request_remote_refresh();
                }
            }
            LibraryActorCommand::Control(LibraryControl::Shutdown) => return false,
        }
        true
    }
}

/// The only app-facing control surface for a running library.  It deliberately
/// exposes no database, asset store, or mutable worker state.
#[derive(Clone)]
pub struct LibraryHandle {
    id: LibraryId,
    commands: async_channel::Sender<LibraryActorCommand>,
    subscriptions: LibraryEventSubscriptions,
}

#[derive(Clone, Debug)]
enum LibraryControl {
    WakeSync { refresh_remote: bool },
    WakeAssets(Vec<ContentHash>),
    StartScanner,
    SetAssetStorage { enabled: bool, reset_cloud_presence: bool },
    RepairInventory,
    Rename { name: String, refresh_remote: bool },
    Shutdown,
}

enum LibraryActorCommand {
    Control(LibraryControl),
    Ui {
        command: LibraryCommand,
        reply: async_channel::Sender<Result<client_runtime::reply::Reply, crate::BackendError>>,
    },
    #[cfg(target_arch = "wasm32")]
    SubscribeBook {
        content_hash: ContentHash,
        reply: async_channel::Sender<Result<(client_runtime::reply::Reply, u64), crate::BackendError>>,
    },
    #[cfg(target_arch = "wasm32")]
    ReleaseBook {
        id: u64,
    },
    /// Reader resources and filesystem imports are host-shaped. Browsers open
    /// books through their own page-side adapter, and the resources they
    /// produce are not `Send`, so these never cross the browser actor channel.
    #[cfg(not(target_arch = "wasm32"))]
    ResolveBook {
        content_hash: ContentHash,
        reply: async_channel::Sender<Result<crate::library::ResolvedBook, BackendError>>,
    },
    #[cfg(not(target_arch = "wasm32"))]
    ResolveAudiobookTrack {
        content_hash: ContentHash,
        index: usize,
        reply: async_channel::Sender<Result<crate::library::ResolvedBook, BackendError>>,
    },
    #[cfg(not(target_arch = "wasm32"))]
    ImportDirectory {
        parent_id: DirId,
        directory: crate::DirectoryImport,
        create_root: bool,
        activity: Option<uuid::Uuid>,
        reply: async_channel::Sender<Result<Vec<crate::ImportFailure>, crate::BackendError>>,
    },
    #[cfg(not(target_arch = "wasm32"))]
    ImportBookPath {
        parent_id: DirId,
        file_name: String,
        path: std::path::PathBuf,
        reply: async_channel::Sender<Result<ContentHash, BackendError>>,
    },
}

/// A Send-only token for a browser book held by the library actor.
/// Dropping the worker subscription releases its actor-owned resource.
#[cfg(target_arch = "wasm32")]
pub struct BookSubscription {
    commands: async_channel::Sender<LibraryActorCommand>,
    id: u64,
}

#[cfg(target_arch = "wasm32")]
impl Drop for BookSubscription {
    fn drop(&mut self) {
        let _ = self.commands.try_send(LibraryActorCommand::ReleaseBook { id: self.id });
    }
}

impl LibraryHandle {
    /// Open one complete library runtime.  The returned handle is the only
    /// object the application may retain; resource setup and worker startup
    /// are entirely session-owned.
    ///
    /// Native opens the database connection, and builds everything that
    /// references it (including the sync worker's own connection), entirely
    /// on the library's dedicated thread — the connection is `Send` but not
    /// `Sync`, so it must never exist anywhere else, even transiently on the
    /// calling thread. `run_blocking` hands `spec` over and waits for the
    /// finished handle, which holds no database reference and is safe to
    /// return.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(spec: super::LibraryRuntimeSpec, _executor: &crate::executor::BackendExecutor) -> Result<Self, BackendError> {
        let library_executor = crate::executor::LibraryExecutor::new().map_err(BackendError::operation)?;
        let executor_for_actor = library_executor.clone();
        let handle = library_executor.run_blocking(move || -> Result<LibraryHandle, BackendError> {
            let session = LibrarySession::open(spec)?;
            // Recover completion records written by older builds, or by a
            // process that ended after its queues drained. This is durable
            // worker lifecycle reconciliation, not a Settings UI read.
            session.db.reconcile_operation(library_database::OperationActivity::default()).map_err(BackendError::operation)?;
            Ok(session.spawn_actor(executor_for_actor))
        })?;
        handle.wake_sync(true);
        handle.wake_assets();
        // Reconcile files on every native open, including startup and newly
        // registered folders. Queue after spawning the actor so async book
        // inspection runs on the library's executor.
        handle.start_scanner();
        Ok(handle)
    }
    #[cfg(target_arch = "wasm32")]
    pub fn open(spec: super::LibraryRuntimeSpec, executor: &crate::executor::BackendExecutor) -> Result<Self, BackendError> {
        let session = LibrarySession::open(spec)?;
        let needs_scan = session.db.needs_scan()?;
        let handle = session.spawn_actor(executor);
        handle.wake_sync(true);
        handle.wake_assets();
        if needs_scan {
            handle.start_scanner();
        }
        Ok(handle)
    }

    pub fn id(&self) -> LibraryId {
        self.id
    }

    pub fn wake_sync(&self, refresh_remote: bool) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::WakeSync { refresh_remote }));
    }

    pub fn wake_assets(&self) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::WakeAssets(Vec::new())));
    }

    pub fn start_scanner(&self) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::StartScanner));
    }

    /// Applies a registry-selected cloud policy inside the owning session.
    pub fn set_asset_storage(&self, enabled: bool, reset_cloud_presence: bool) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::SetAssetStorage { enabled, reset_cloud_presence }));
    }

    pub fn repair_inventory(&self) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::RepairInventory));
    }

    /// Applies a name discovered remotely without creating a new rename
    /// mutation to send back to that same remote.
    pub fn set_display_name(&self, name: String) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::Rename { name, refresh_remote: false }));
    }

    pub fn rename(&self, name: String) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::Rename { name, refresh_remote: true }));
    }

    pub fn shutdown(&self) {
        self.subscriptions.close();
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::Shutdown));
        self.commands.close();
    }

    /// Delivers the concrete UI protocol command to its owning actor.
    pub async fn dispatch(&self, command: LibraryCommand) -> Result<client_runtime::reply::Reply, crate::BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::Ui { command, reply }).await.map_err(|_| "library actor is no longer running".to_owned())?;
        result.recv().await.map_err(|_| "library actor stopped before replying".to_owned())?
    }

    pub async fn scanning(&self) -> Result<bool, BackendError> {
        self.dispatch(LibraryCommand::Scanning).await?.take()
    }

    pub async fn download_state(&self, content_hash: ContentHash) -> Result<DownloadState, BackendError> {
        self.dispatch(LibraryCommand::DownloadState { content_hash }).await?.take()
    }

    pub async fn transfer_snapshot(&self) -> Result<crate::LibraryTransfers, BackendError> {
        self.dispatch(LibraryCommand::TransferSnapshot).await?.take()
    }

    /// Reads sync state through the session actor. The application supplies
    /// account discovery context, but never opens the library database itself.
    pub async fn sync_status(&self, account: Option<account_client::Session>, remote_present: Option<bool>) -> Result<crate::LibrarySyncStatus, BackendError> {
        self.dispatch(LibraryCommand::SyncStatus { signed_in: account.is_some(), remote_present }).await?.take()
    }

    pub async fn local_storage_usage(&self) -> Result<Option<u64>, BackendError> {
        self.dispatch(LibraryCommand::LocalStorageUsage).await?.take()
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn subscribe_book(&self, content_hash: ContentHash) -> Result<(client_runtime::reply::Reply, BookSubscription), crate::BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::SubscribeBook { content_hash, reply }).await.map_err(|_| "library actor is no longer running".to_owned())?;
        let (initial, id) = result.recv().await.map_err(|_| "library actor stopped before replying".to_owned())??;
        Ok((initial, BookSubscription { commands: self.commands.clone(), id }))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn resolve_book(&self, content_hash: ContentHash) -> Result<crate::library::ResolvedBook, BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::ResolveBook { content_hash, reply }).await.map_err(|_| BackendError::message("library actor is no longer running"))?;
        result.recv().await.map_err(|_| BackendError::message("library actor stopped before replying"))?
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn resolve_audiobook_track(&self, content_hash: ContentHash, index: usize) -> Result<crate::library::ResolvedBook, BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::ResolveAudiobookTrack { content_hash, index, reply }).await.map_err(|_| BackendError::message("library actor is no longer running"))?;
        result.recv().await.map_err(|_| BackendError::message("library actor stopped before replying"))?
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn import_selected_directory(&self, parent_id: DirId, directory: crate::DirectoryImport, create_root: bool, activity: Option<uuid::Uuid>) -> Result<Vec<crate::ImportFailure>, crate::BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::ImportDirectory { parent_id, directory, create_root, activity, reply }).await.map_err(|_| "library actor is no longer running".to_owned())?;
        result.recv().await.map_err(|_| "library actor stopped before replying".to_owned())?
    }

    pub async fn directory_import_progress_for(&self, activity: uuid::Uuid) -> Result<Option<library_model::DirectoryImportProgress>, BackendError> {
        self.dispatch(LibraryCommand::ImportProgress { activity }).await?.take()
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn import_book_path(&self, parent_id: DirId, file_name: String, path: std::path::PathBuf) -> Result<ContentHash, BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::ImportBookPath { parent_id, file_name, path, reply }).await.map_err(|_| BackendError::message("library actor is no longer running"))?;
        result.recv().await.map_err(|_| BackendError::message("library actor stopped before replying"))?
    }

    pub fn subscribe_updates(&self) -> async_channel::Receiver<library_model::LibraryUpdate> {
        self.subscriptions.subscribe_updates()
    }

    pub fn subscribe_download(&self, content_hash: ContentHash) -> async_channel::Receiver<DownloadState> {
        self.subscriptions.subscribe_download(content_hash)
    }

    pub async fn download_changes(&self, content_hash: ContentHash) -> Result<(async_channel::Receiver<DownloadState>, DownloadState), BackendError> {
        let durable = self.download_state(content_hash).await?;
        Ok(self.subscriptions.subscribe_download_with_initial(content_hash, durable))
    }
}

impl LibrarySession {
    /// Combines library-owned durable sync details with the application-owned
    /// account/discovery snapshot for settings presentation.
    pub(super) fn sync_status(&self, signed_in: bool, remote_present: Option<bool>) -> Result<crate::LibrarySyncStatus, BackendError> {
        let sync_details = self.db.stored_sync_status()?;
        let state = match signed_in {
            false => crate::LibrarySyncState::LocalOnly,
            true => {
                if remote_present == Some(false) {
                    crate::LibrarySyncState::RemoteMissing
                } else if !sync_details.has_unsynced_changes && sync_details.last_synced_at.is_none() {
                    if remote_present.is_some() {
                        crate::LibrarySyncState::Receiving
                    } else {
                        crate::LibrarySyncState::Checking
                    }
                } else if sync_details.has_unsynced_changes {
                    crate::LibrarySyncState::SyncPending
                } else {
                    crate::LibrarySyncState::Synced
                }
            }
        };
        Ok(crate::LibrarySyncStatus {
            asset_storage_enabled: self.sync().is_some_and(|sync| sync.asset_storage_enabled()),
            library_id: self.id,
            state,
            has_unsynced_changes: sync_details.has_unsynced_changes,
            last_synced_at: sync_details.last_synced_at,
        })
    }

    /// Announces that the set of books or folders changed.
    ///
    /// A write site cannot tell whether it is alone or the four-thousandth
    /// commit of an import, so it must not decide how often this is worth
    /// saying. An import in flight already brackets itself and announces its
    /// result once at [`Self::finish_directory_import`]; every commit inside
    /// that bracket is the same announcement repeated, and each one costs every
    /// open view a full reload.
    pub(super) fn notify_contents_changed(&self) {
        if !self.runtime.has_directory_imports() {
            let _ = self.event_tx.try_send(LibraryEvent::ContentsChanged);
        }
    }

    pub(super) fn notify_replayed_contents(&self) {
        if !self.runtime.has_directory_imports() {
            self.publish_update(library_model::LibraryUpdate::Contents);
            self.sync_wake.wake();
        }
    }

    pub(super) fn notify_storage_changed(&self) {
        // Folder imports publish their totals once through ImportComplete.
        if !self.runtime.has_directory_imports() {
            let _ = self.event_tx.try_send(LibraryEvent::StorageChanged);
        }
    }

    pub(crate) fn begin_directory_import(&self, id: uuid::Uuid) {
        if self.runtime.begin_directory_import(id) {
            let _ = self.event_tx.try_send(LibraryEvent::ImportStarted);
        }
    }

    pub(crate) fn advance_directory_import(&self, id: uuid::Uuid, total: u64, succeeded: u64, failed: u64) {
        if self.runtime.advance_directory_import(id, total, succeeded, failed) {
            let _ = self.event_tx.try_send(LibraryEvent::TransferStatusChanged);
        }
    }

    pub(crate) fn scan_progress(&self) -> Option<crate::ScanProgress> {
        if let Some(progress) = self.directory_import_progress() {
            return Some(progress);
        }
        None
    }

    pub(crate) fn directory_import_details(&self) -> Option<library_model::DirectoryImportProgress> {
        self.runtime.directory_import_details()
    }

    pub(crate) fn directory_import_details_for(&self, id: uuid::Uuid) -> Option<library_model::DirectoryImportProgress> {
        self.runtime.directory_import_details_for(Some(id))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn start_import_file(&self, id: uuid::Uuid, name: String, total_bytes: Option<u64>) -> crate::asset_store::ImportProgressObserver {
        let progress = self.runtime.start_import_file(id, name, total_bytes);
        std::sync::Arc::new(move |stage| {
            progress.lock().unwrap_or_else(|e| e.into_inner()).stage = stage;
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn finish_import_file(&self, id: uuid::Uuid) {
        self.runtime.finish_import_file(id);
    }

    pub(crate) fn directory_import_progress(&self) -> Option<crate::ScanProgress> {
        self.runtime.directory_import_progress()
    }

    pub(crate) fn finish_directory_import(&self, id: uuid::Uuid) {
        // Duplicate and late replies cannot complete another import.
        if self.runtime.finish_directory_import(id) {
            let _ = self.event_tx.try_send(LibraryEvent::ImportComplete);
        }
    }

    pub(crate) fn from_parts(
        id: LibraryId, db: std::sync::Arc<Database>, assets: AssetStore, metadata: metadata_client::MetadataClient, event_tx: LibraryEventSender, event_rx: LibraryEventReceiver, scanner: LibraryScanner,
        cpu: std::sync::Arc<dyn cpu_host::CpuHost>,
    ) -> Self {
        let (sync_wake, sync_requests) = library_runtime::scheduler::SyncScheduler::new();
        let (asset_wake, asset_requests) = library_runtime::scheduler::SyncScheduler::new();
        Self {
            id,
            db,
            assets,
            metadata,
            sync: None,
            event_tx,
            event_rx: Some(event_rx),
            subscriptions: LibraryEventSubscriptions::default(),
            runtime: Arc::new(crate::library::session::LibraryRuntime::default()),
            workers: super::workers::LibraryWorkers::default(),
            sync_wake,
            sync_requests: Some(sync_requests),
            asset_wake,
            asset_requests: Some(asset_requests),
            remote_changes: None,
            _scanner: Arc::new(scanner),
            cpu,
            filesystem_scanning: std::cell::Cell::new(false),
            file_work_error: Default::default(),
        }
    }

    pub(crate) fn with_sync(mut self, sync: Arc<crate::sync::LibrarySync>) -> Self {
        self.sync = Some(sync);
        self
    }

    pub(crate) fn with_remote_changes(mut self, remote_changes: crate::notification_interest::RemoteChanges) -> Self {
        self.remote_changes = Some(remote_changes);
        self
    }

    pub(super) async fn local_storage_usage(&self) -> Result<Option<u64>, BackendError> {
        if !self.assets.local_access_enabled() {
            return Ok(None);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let paths = self.db.local_book_paths()?;
            self.assets.book_bytes_at_paths(paths).await.map(Some).map_err(BackendError::operation)
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.assets.local_book_bytes().await.map(Some).map_err(BackendError::operation)
        }
    }

    pub(crate) fn metadata(&self) -> &metadata_client::MetadataClient {
        &self.metadata
    }

    pub(crate) fn sync(&self) -> Option<Arc<crate::sync::LibrarySync>> {
        self.sync.clone()
    }

    pub(crate) fn subscribe_updates(&self) -> async_channel::Receiver<library_model::LibraryUpdate> {
        self.subscriptions.subscribe_updates()
    }

    /// Transfers sole ownership of this session to its long-lived actor.
    /// Callers retain only the returned handle, never a resource-bearing
    /// session clone.
    ///
    /// Native gives every opened library its own dedicated OS thread (see
    /// [`crate::executor::LibraryExecutor`]): the session's database
    /// connection is `Send` but not `Sync`, so the actor loop and its
    /// workers run there instead of on the shared multi-threaded executor,
    /// where holding it across an `.await` would not type-check. Wasm has
    /// only one thread to begin with, so it keeps using the shared executor
    /// directly — the same one the browser's own worker boundary already
    /// requires this handle's command protocol for.
    /// Called only from inside a job already running on `library_executor`'s
    /// dedicated thread (see [`Self::open`]) — everything here executes
    /// there, so nothing it touches needs to be `Send`.
    pub(crate) fn spawn_actor(mut self, #[cfg(not(target_arch = "wasm32"))] library_executor: crate::executor::LibraryExecutor, #[cfg(target_arch = "wasm32")] executor: &crate::executor::BackendExecutor) -> LibraryHandle {
        let (commands, receiver) = async_channel::unbounded();
        let handle = LibraryHandle { id: self.id, commands, subscriptions: self.subscriptions.clone() };
        self.start_account_observer(
            #[cfg(target_arch = "wasm32")]
            executor,
            handle.commands.clone(),
        );
        self.start_remote_observer(
            #[cfg(target_arch = "wasm32")]
            executor,
            handle.commands.clone(),
        );
        self.start_sync(
            #[cfg(target_arch = "wasm32")]
            executor,
        );
        self.start_assets(
            #[cfg(target_arch = "wasm32")]
            executor,
        );
        let events = self.event_rx.take().expect("a library session has one event receiver");
        let event_commands = handle.commands.clone();
        let run = async move {
            // The actor owns its native executor lifetime, including retirement.
            #[cfg(not(target_arch = "wasm32"))]
            let _executor = library_executor;
            LibraryActor::new(self).run(receiver, events, event_commands).await;
        };
        #[cfg(not(target_arch = "wasm32"))]
        crate::executor::LibraryExecutor::spawn_local_detached(run);
        #[cfg(target_arch = "wasm32")]
        executor.spawn_detached(run);
        handle
    }

    /// Starts the durable sync worker owned by this session.
    pub(crate) fn start_sync(&mut self, #[cfg(target_arch = "wasm32")] executor: &crate::executor::BackendExecutor) {
        let Some(requests) = self.sync_requests.take() else {
            return;
        };
        let scheduler = self.sync_wake.clone();
        let Some(sync) = self.sync() else {
            return;
        };
        #[cfg(not(target_arch = "wasm32"))]
        let database = self.db.clone();
        #[cfg(not(target_arch = "wasm32"))]
        let assets = self.assets.clone();
        let task = scheduler.run(
            move |input| {
                let sync = sync.clone();
                #[cfg(not(target_arch = "wasm32"))]
                let database = database.clone();
                #[cfg(not(target_arch = "wasm32"))]
                let assets = assets.clone();
                async move {
                    let result = async {
                        if !sync.bind_current_account().map_err(crate::BackendError::operation)? {
                            // Account discovery can lag behind library startup.
                            // This is a deferred pass, not a successful sync:
                            // returning an error preserves the scheduler retry
                            // until the account observer wakes us on sign-in.
                            return Err(BackendError::message("waiting for account session"));
                        }
                        sync.ensure_server_library().await?;
                        if input.remote {
                            sync.synchronize_state().await.map(|_| ()).map_err(crate::BackendError::operation)
                        } else {
                            sync.drain_outbox().await.map(|_| ()).map_err(crate::BackendError::operation)
                        }
                    }
                    .await;
                    // Sync can't replay this itself (SyncStore futures must
                    // stay Send); the orchestrator owns local state instead.
                    #[cfg(not(target_arch = "wasm32"))]
                    if result.is_ok() {
                        if let Err(error) = assets.run_directory_work(&database).await {
                            log::warn!("directory rename will retry from the durable queue: {error}");
                        }
                    }
                    result.map_err(|error| log::warn!("library synchronization will resume: {error}"))
                }
            },
            requests,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(100),
        );
        #[cfg(not(target_arch = "wasm32"))]
        let worker = crate::executor::LibraryExecutor::spawn_local(task);
        #[cfg(target_arch = "wasm32")]
        let worker = executor.spawn(task);
        self.workers.set_sync(worker);
    }

    /// Account identity is global, but its consequences are local: each
    /// opened library resumes its own durable queues and republishes its own
    /// views.  There is intentionally no AppState -> library fan-out.
    fn start_account_observer(&mut self, #[cfg(target_arch = "wasm32")] executor: &crate::executor::BackendExecutor, commands: async_channel::Sender<LibraryActorCommand>) {
        let Some(sync) = self.sync() else {
            return;
        };
        let Some(mut changed) = sync.subscribe_account_changes() else {
            return;
        };
        changed.borrow_and_update();
        let events = self.event_tx.clone();
        let task = async move {
            while changed.changed().await.is_ok() {
                let _ = events.try_send(LibraryEvent::ContentsChanged);
                let _ = commands.try_send(LibraryActorCommand::Control(LibraryControl::WakeSync { refresh_remote: true }));
                let _ = commands.try_send(LibraryActorCommand::Control(LibraryControl::WakeAssets(Vec::new())));
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        let worker = crate::executor::LibraryExecutor::spawn_local(task);
        #[cfg(target_arch = "wasm32")]
        let worker = executor.spawn(task);
        self.workers.set_account_observer(worker);
    }

    /// Server notifications are global transport input, but reconciliation is
    /// owned by each concrete library.  The signal is intentionally lossy:
    /// durable sync state makes a single remote refresh sufficient.
    fn start_remote_observer(&mut self, #[cfg(target_arch = "wasm32")] executor: &crate::executor::BackendExecutor, commands: async_channel::Sender<LibraryActorCommand>) {
        let Some(remote_changes) = &self.remote_changes else {
            return;
        };
        let mut changed = remote_changes.subscribe();
        changed.borrow_and_update();
        let task = async move {
            while changed.changed().await.is_ok() {
                let _ = commands.try_send(LibraryActorCommand::Control(LibraryControl::WakeSync { refresh_remote: true }));
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        let worker = crate::executor::LibraryExecutor::spawn_local(task);
        #[cfg(target_arch = "wasm32")]
        let worker = executor.spawn(task);
        self.workers.set_remote(worker);
    }

    /// Starts this library's durable asset worker.
    pub(crate) fn start_assets(&mut self, #[cfg(target_arch = "wasm32")] executor: &crate::executor::BackendExecutor) {
        let Some(requests) = self.asset_requests.take() else {
            return;
        };
        let scheduler = self.asset_wake.clone();
        let Some(sync) = self.sync() else {
            return;
        };
        let task = scheduler.run(
            move |input| {
                let sync = sync.clone();
                async move {
                    if !sync.bind_current_account().map_err(|error| log::warn!("could not bind account before asset reconciliation: {error}"))? {
                        // Do not consume a durable asset wake while account
                        // discovery is still in progress. The scheduler will
                        // retry, and the account observer wakes it immediately
                        // when a usable session arrives.
                        return Err(());
                    }
                    sync.reconcile_assets(input.hashes).await.map_err(|error| log::warn!("asset reconciliation will resume: {error}"))?;
                    // A scheduler wake is only a hint; the database owns the
                    // actual queue. `sync_assets` drains eligible paged work
                    // directly; remaining work here is blocked and should
                    // retain the normal retry policy.
                    if sync.has_pending_asset_work().map_err(|error| log::warn!("could not check remaining asset work: {error}"))? {
                        return Err(());
                    }
                    Ok(())
                }
            },
            requests,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(100),
        );
        #[cfg(not(target_arch = "wasm32"))]
        let worker = crate::executor::LibraryExecutor::spawn_local(task);
        #[cfg(target_arch = "wasm32")]
        let worker = executor.spawn(task);
        self.workers.set_assets(worker);
    }

    pub(crate) fn subscribe_download(&self, content_hash: ContentHash) -> async_channel::Receiver<DownloadState> {
        self.subscriptions.subscribe_download(content_hash)
    }

    pub(crate) fn publish_update(&self, update: library_model::LibraryUpdate) {
        self.subscriptions.publish_update(update);
    }

    pub(super) fn emit(&self, event: LibraryEvent) {
        let _ = self.event_tx.try_send(event);
    }

    pub fn id(&self) -> &LibraryId {
        &self.id
    }

    pub(crate) fn thumbnail_work_running(&self) -> bool {
        self.runtime.thumbnail_work_running()
    }

    pub(crate) fn thumbnail_batch_progress(&self) -> Option<crate::ScanProgress> {
        self.runtime.thumbnail_batch_progress()
    }

    pub(crate) fn thumbnail_batch_waiting(&self) -> bool {
        self.runtime.thumbnail_batch_waiting()
    }

    pub(super) fn prepare_thumbnail_batch(&self, pending: u64) {
        self.runtime.prepare_thumbnail_batch(pending);
    }

    pub(super) fn complete_thumbnail_batch_item(&self) {
        self.runtime.complete_thumbnail_batch_item();
        let _ = self.event_tx.try_send(LibraryEvent::TransferStatusChanged);
    }

    pub(super) fn begin_thumbnail_generation(&self) -> crate::library::session::ThumbnailActivity {
        self.runtime.begin_thumbnail_generation(self.event_tx.clone())
    }

    pub(crate) fn begin_thumbnail_work(&self) -> Option<LibraryActivity> {
        self.runtime.begin_thumbnail_work(self.event_tx.clone())
    }

    pub(crate) fn begin_local_work(&self) -> Option<LibraryActivity> {
        self.runtime.begin_local_work(self.event_tx.clone())
    }

    /// True while the local filesystem scanner is scheduled or actively
    /// walking this library.
    pub(crate) fn discovery_enabled(&self) -> bool {
        self._scanner.enabled()
    }

    pub fn scanner_running(&self) -> bool {
        self.filesystem_scanning.get() || self.runtime.has_directory_imports()
    }

    #[cfg(all(test, feature = "scanner", not(target_os = "android")))]
    pub(crate) fn shares_scanner_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self._scanner, &other._scanner)
    }

    /// Parked scanners have queued work but are not running user operations.
    pub(crate) fn operation_scanning(&self) -> bool {
        self.filesystem_scanning.get()
    }

    /// Enables the initial filesystem scan for a handle opened in deferred mode.
    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn start_scanner(&self) {
        if self.assets.local_access_enabled() {
            self.run_filesystem_scan().await;
        }
    }

    /// The session, not the filesystem scanner, owns the scan protocol and
    /// its application events. The scanner sees only immutable input and
    /// returns data; this method decides whether and how to apply that data.
    #[cfg(target_arch = "wasm32")]
    async fn run_filesystem_scan(&self) {
        let snapshot = match self.db.scan_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                log::warn!("could not create filesystem scan snapshot: {error}");
                return;
            }
        };
        self.emit(LibraryEvent::ScanStarted);
        let discovery = self._scanner.discover(&snapshot);
        let inspected = self._scanner.inspect(&discovery, &snapshot, &self.cpu).await;
        match self.db.apply_filesystem_scan(&snapshot, &discovery, &inspected) {
            Ok(library_database::ScanApplyResult::Applied) => self.emit(LibraryEvent::ContentsChanged),
            Ok(library_database::ScanApplyResult::Stale) => log::debug!("filesystem scan became stale before apply"),
            Err(error) => log::warn!("could not apply filesystem scan: {error}"),
        }
        self.emit(LibraryEvent::ScanComplete);
    }

    pub async fn import_book_bytes(&self, parent_id: &DirId, file_name: String, bytes: Vec<u8>) -> Result<ContentHash, BackendError> {
        self.import_book_reader(parent_id, Box::new(std::io::Cursor::new(bytes)), file_name).await
    }

    pub(crate) fn download_state(&self, content_hash: ContentHash) -> Result<DownloadState, BackendError> {
        let durable = self.persistent_download_state(content_hash)?;
        Ok(match (&durable, self.subscriptions.current_download(&content_hash)) {
            (DownloadState::Queued, Some(state @ DownloadState::Downloading(_))) => state,
            _ => durable,
        })
    }

    pub(crate) fn queue_download(&self, content_hash: ContentHash) -> Result<DownloadState, BackendError> {
        let state = self.download_state(content_hash)?;
        if matches!(state, DownloadState::Downloaded | DownloadState::Downloading(_)) {
            return Ok(state);
        }
        let sync = self.sync().ok_or_else(|| BackendError::message("library sync is unavailable"))?;
        if !sync.is_signed_in() {
            return Err(BackendError::message("sign in before downloading this book"));
        }
        self.subscriptions.forget_terminal_download(&content_hash);
        self.request_downloads(&[content_hash])?;
        Ok(DownloadState::Queued)
    }

    /// Readers wait on scheduler-owned work; cancelling a reader leaves the durable request intact.
    pub(crate) async fn request_download(&self, content_hash: ContentHash) -> Result<DownloadState, BackendError> {
        let changes = self.subscribe_download(content_hash);
        self.queue_download(content_hash)?;
        loop {
            let state = self.download_state(content_hash)?;
            if matches!(state, DownloadState::Downloaded | DownloadState::NotDownloaded) {
                return Ok(state);
            }
            let sync = self.sync().ok_or_else(|| BackendError::message("library sync is unavailable"))?;
            if !sync.is_signed_in() {
                return Err(BackendError::message("sign in before downloading this book"));
            }
            if let Some(error) = sync.download_error(content_hash) {
                return Err(BackendError::message(error));
            }
            // Recheck durable state even when another browser owns the transfer,
            // or an already-local request settles without a progress event.
            if let Ok(result) = crate::executor::timeout(std::time::Duration::from_millis(250), changes.recv()).await {
                result.map_err(|_| BackendError::message("library stopped while waiting for download"))?;
            }
        }
    }

    pub(crate) fn request_downloads(&self, hashes: &[ContentHash]) -> Result<(), BackendError> {
        if hashes.is_empty() {
            return Ok(());
        }
        self.sync().ok_or_else(|| BackendError::message("library sync is unavailable"))?.request_downloads(hashes).map_err(BackendError::operation)?;
        // Enqueue and wake together: the next pass plans exactly these
        // hashes first instead of discovering them in the drain.
        self.asset_wake.wake_hashes(hashes.to_vec());
        Ok(())
    }
}

pub use library_database::{LibraryAuthorsView, LibraryFolderEntry, LibraryFolderView, LibraryHierarchyEntry, LibraryHierarchyView, LibrarySubjectEntry, LibrarySubjectView};

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "concurrency_tests.rs"]
mod concurrency_tests;

#[cfg(all(test, feature = "scanner", not(target_arch = "wasm32")))]
mod background_scan_tests {
    use super::*;

    fn actor(root: &std::path::Path) -> LibraryActor {
        let database = Database::open(root.join("library.sqlite")).unwrap();
        database.initialize_library().unwrap();
        let (events, receiver) = crate::library::events::library_event_channel();
        let session = LibrarySession::from_parts(
            LibraryId::new_v4(),
            Arc::new(database),
            AssetStore::open(root.to_str().unwrap()).unwrap(),
            metadata_client::MetadataClient::new("http://127.0.0.1:1").unwrap(),
            events,
            receiver,
            LibraryScanner::from_root(root.to_str().unwrap()).unwrap(),
            crate::default_cpu_host(),
        );
        LibraryActor::new(session)
    }

    #[tokio::test]
    async fn background_scan_leaves_commands_responsive_and_coalesces_requests() {
        let root = tempfile::tempdir().unwrap();
        let mut actor = actor(root.path());
        actor.begin_scan();
        assert!(actor.session.scanner_running());
        let scan_id: i64 = crate::test_database::raw(&actor.session.db).query_row("SELECT scan_seq FROM sync_metadata", [], |row| row.get(0)).unwrap();
        actor.begin_scan();
        assert_eq!(crate::test_database::raw(&actor.session.db).query_row("SELECT scan_seq FROM sync_metadata", [], |row| row.get::<_, i64>(0)).unwrap(), scan_id, "duplicate start must not take another snapshot");
        assert!(actor.scan_job.pending, "a change during a scan needs another pass");
        let (reply, result) = async_channel::bounded(1);
        let sync = actor.session.sync_wake.clone();
        let assets = actor.session.asset_wake.clone();
        // The scan future has not been polled, but the actor can serve a query.
        assert!(actor.handle_command(LibraryActorCommand::Ui { command: LibraryCommand::Scanning, reply }, &sync, &assets));
        assert!(result.recv().await.unwrap().unwrap().take::<bool>().unwrap());
        let output = actor.scan_job.task.as_mut().unwrap().await;
        actor.finish_scan(output);
        assert!(!actor.session.scanner_running());
        assert!(actor.scan_job.task.is_some(), "a follow-up scan is queued");
    }

    #[cfg(feature = "watcher")]
    #[tokio::test]
    async fn external_folder_creation_wakes_library_watcher() {
        let root = tempfile::tempdir().unwrap();
        let actor = actor(root.path());
        assert!(actor._watcher.is_some());
        std::fs::create_dir(root.path().join("Added externally")).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), actor.watch_events.recv()).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stale_scan_and_database_failure_keep_a_retry_pending() {
        let root = tempfile::tempdir().unwrap();
        let mut actor = actor(root.path());
        actor.begin_scan();
        actor.session.db.queue_directory_work(&sync_common::ROOT_DIR_ID).unwrap();
        let output = actor.scan_job.task.as_mut().unwrap().await;
        actor.finish_scan(output);
        assert!(actor.scan_job.task.is_some());
        assert!(actor.session.scanner_running());
        actor.finish_scan(Err("database is busy".into()));
        assert!(actor.scan_job.task.is_some());
        assert!(actor.session.scanner_running());
    }

    #[tokio::test]
    async fn pending_preparation_does_not_block_queries_or_scan_completion() {
        let root = tempfile::tempdir().unwrap();
        let mut actor = actor(root.path());
        let sync = actor.session.sync_wake.clone();
        let assets = actor.session.asset_wake.clone();
        let updates = actor.session.subscribe_updates();
        // Model a metadata request that never responds. Wakes must coalesce
        // without replacing or awaiting the active preparation batch.
        actor.enrichment_job.task = Some(Box::pin(std::future::pending()));
        for _ in 0..3 {
            assert!(actor.handle_command(LibraryActorCommand::Control(LibraryControl::WakeAssets(vec![])), &sync, &assets));
        }
        assert!(actor.file_job.pending);
        let output = actor.file_job.task.as_mut().unwrap().await;
        actor.finish_file_work(output);
        assert!(actor.enrichment_job.pending);
        actor.begin_scan();
        let output = actor.scan_job.task.as_mut().unwrap().await;
        actor.finish_scan(output);
        let (reply, result) = async_channel::bounded(1);
        assert!(actor.handle_command(LibraryActorCommand::Ui { command: LibraryCommand::Scanning, reply }, &sync, &assets));
        assert!(!result.recv().await.unwrap().unwrap().take::<bool>().unwrap());
        let (commands, _) = async_channel::unbounded();
        actor.publish_event(LibraryEvent::ScanComplete, &commands);
        assert!(matches!(updates.recv().await.unwrap(), library_model::LibraryUpdate::Scanning(false)));
        assert!(matches!(updates.recv().await.unwrap(), library_model::LibraryUpdate::Contents));
        actor.finish_enrichment(Ok(()));
        assert!(actor.enrichment_job.task.is_some(), "coalesced work gets a follow-up pass");
        assert!(!actor.enrichment_job.pending);
        actor.finish_enrichment(Ok(()));
        assert!(actor.enrichment_job.task.is_none());
    }

    #[tokio::test]
    async fn initial_scan_exposes_first_batch_before_inspecting_all_files() {
        use std::io::Write;
        let root = tempfile::tempdir().unwrap();
        for index in 0..33 {
            let file = std::fs::File::create(root.path().join(format!("book-{index}.epub"))).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            let package = format!(
                r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">urn:test:{index}</dc:identifier><dc:title>Book {index}</dc:title><dc:language>en</dc:language></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#
            );
            for (name, bytes) in [
                ("mimetype", "application/epub+zip"),
                (
                    "META-INF/container.xml",
                    r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
                ),
                ("book.opf", package.as_str()),
                ("chapter.xhtml", r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title></head><body><p>Text</p></body></html>"#),
            ] {
                zip.start_file(name, options).unwrap();
                zip.write_all(bytes.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let mut actor = actor(root.path());
        let events = actor.session.event_rx.as_ref().unwrap().clone();
        actor.begin_scan();
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                tokio::select! {
                    biased;
                    event = events.recv() => if matches!(event.unwrap(), LibraryEvent::ContentsChanged) { break; },
                    result = actor.scan_job.task.as_mut().unwrap() => panic!("scan ended before publishing its first batch: {result:?}"),
                }
            }
        })
        .await
        .unwrap();
        let (reply, result) = async_channel::bounded(1);
        let sync = actor.session.sync_wake.clone();
        let assets = actor.session.asset_wake.clone();
        actor.handle_command(LibraryActorCommand::Ui { command: LibraryCommand::BookCount, reply }, &sync, &assets);
        let count: usize = result.recv().await.unwrap().unwrap().take().unwrap();
        assert_eq!(count, filesystem_scanner::filesystem_scan::PUBLISH_BATCH_SIZE);
        assert!(actor.session.scanner_running());
        let result = actor.scan_job.task.as_mut().unwrap().await;
        actor.finish_scan(result);
        assert!(!actor.session.scanner_running());
        assert_eq!(actor.session.book_count().unwrap(), 33);
    }

    #[tokio::test]
    async fn existing_books_remain_queryable_during_scan_and_preparation() {
        let root = tempfile::tempdir().unwrap();
        let mut actor = actor(root.path());
        crate::test_database::raw(&actor.session.db).execute("INSERT INTO book(content_hash,title,added_at,format) VALUES (?1,'Existing book',1,'epub')", ["a".repeat(64)]).unwrap();
        crate::test_database::raw(&actor.session.db).execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,is_downloaded,local_hash,last_scan) SELECT ?1,row_id,'existing.epub',1,'',0 FROM book", [sync_common::ROOT_DIR_ID.to_string()]).unwrap();
        actor.begin_scan();
        actor.enrichment_job.task = Some(Box::pin(std::future::pending()));
        let sync = actor.session.sync_wake.clone();
        let assets = actor.session.asset_wake.clone();
        let (reply, result) = async_channel::bounded(1);
        assert!(actor.handle_command(LibraryActorCommand::Ui { command: LibraryCommand::BookCount, reply }, &sync, &assets));
        let payload = result.recv().await.unwrap().unwrap();
        let count: usize = payload.take().unwrap();
        assert_eq!(count, 1);
        assert!(actor.session.scanner_running());
        assert!(actor.enrichment_job.task.is_some());
    }
    #[tokio::test]
    async fn subject_compaction_reaches_client_responses_and_reacts_to_finished_filter() {
        use library_database::LibraryBrowseData;
        use library_model::{LibraryBrowseQuery, LibraryFileTypeFilter};
        let root = tempfile::tempdir().unwrap();
        #[derive(Clone, Debug)]
        struct SignedOut;
        impl account_client::SessionPersistence for SignedOut {
            fn load_account_session(&self) -> std::io::Result<Option<account_client::Session>> {
                Ok(None)
            }
            fn save_account_session(&self, _: &account_client::Session) -> std::io::Result<()> {
                unreachable!("signed-out fixture")
            }
            fn clear_account_session(&self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let server: account_client::ServerUrl = "http://127.0.0.1:1".parse().unwrap();
        let account = library_runtime::account::LibraryAccountSession::load(SignedOut, server.clone()).unwrap();
        let session = LibrarySession::open(crate::library::LibraryRuntimeSpec {
            config: crate::library::LibraryOpenConfig {
                id: LibraryId::new_v4(),
                display_name: "Fixture".into(),
                root: root.path().to_string_lossy().into_owned(),
                database_locator: root.path().join("library.sqlite").to_string_lossy().into_owned(),
                default_sync_server_url: server,
                metadata_server_url: "http://127.0.0.1:1".into(),
                asset_storage_enabled: true,
                reset_cloud_presence: false,
            },
            account: account.library_account(),
            notification_interest: Default::default(),
            remote_changes: Default::default(),
        })
        .unwrap();
        let session = std::rc::Rc::new(session);
        let library = &session;
        async fn wire_value<T: serde::de::DeserializeOwned>(library: &std::rc::Rc<LibrarySession>, command: LibraryCommand) -> T {
            let reply = crate::library::commands::dispatch_library_request_in_session(library.clone(), command).await.unwrap();
            client_runtime::wire::decode_worker_payload(reply.into_wire().unwrap()).unwrap()
        }
        library_database::execute_owner_fixture_sql(
            &library.db,
            "DETACH curated; ATTACH ':memory:' AS curated;
         CREATE TABLE curated.concept(concept_id INTEGER PRIMARY KEY,preferred_label TEXT NOT NULL);
         INSERT INTO curated.concept VALUES(920001,'History'),(920002,'U.S.'),(920003,'Period');
         CREATE TABLE curated.unified_concept_route(route_id INTEGER PRIMARY KEY,concept_id INTEGER NOT NULL,parent_route_id INTEGER NOT NULL);
         INSERT INTO curated.unified_concept_route VALUES(1,920001,0),(2,920002,1),(3,920003,2);
         INSERT INTO book(content_hash,title,format,read_progress) VALUES
            (printf('%064x',911),'General history','epub',100),
            (printf('%064x',912),'Specific history','epub',0);
         INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded)
            SELECT '00000000-0000-0000-0000-000000000000',row_id,title,title,1 FROM book;
         INSERT INTO book_unified_concept(book_row_id,concept_id,mapper_version)
            SELECT row_id,CASE title WHEN 'General history' THEN 920002 ELSE 920003 END,1 FROM book;",
        )
        .unwrap();
        let mut query = LibraryBrowseQuery {
            location: String::new(),
            search: String::new(),
            file_types: vec![LibraryFileTypeFilter::All],
            chip_sort: Default::default(),
            book_sort: Default::default(),
            languages: Vec::new(),
            hide_finished: false,
            include_direct_child_books: false,
        };
        let unfiltered: LibraryBrowseData = wire_value(library, LibraryCommand::SubjectContents { query: query.clone() }).await;
        assert!(unfiltered.contents.children.is_empty(), "small subject subtrees are compacted before filtering");
        assert_eq!(unfiltered.contents.books.iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["General history", "Specific history"]);
        query.hide_finished = true;
        let filtered: LibraryBrowseData = wire_value(library, LibraryCommand::SubjectContents { query: query.clone() }).await;
        assert!(filtered.contents.children.is_empty());
        assert_eq!(filtered.contents.books[0].title, "Specific history", "the finished filter hides only the completed book");
        assert_eq!(filtered.format_counts, unfiltered.format_counts, "facets still describe all books in the location");
        assert_eq!(library.subject_contents(&query, true).unwrap(), filtered.contents);
        query.location = "History / U.S.".into();
        let collapsed: LibraryBrowseData = wire_value(library, LibraryCommand::SubjectContents { query: query.clone() }).await;
        assert!(collapsed.contents.children.is_empty() && collapsed.contents.books.is_empty(), "the original narrow route is no longer browsable");
        query.location.clear();
        let grouped: LibraryBrowseData = wire_value(library, LibraryCommand::SubjectContents { query: query.clone() }).await;
        assert!(grouped.chip_groups.is_empty());
        assert!(grouped.contents.children.is_empty());
        assert_eq!(grouped.contents.books.iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["Specific history"], "small subtrees also compact at the root");
        query.location = sync_common::ROOT_DIR_ID.to_string();
        let folder: LibraryBrowseData = wire_value(library, LibraryCommand::FolderContents { query }).await;
        assert_eq!(folder.contents.books.iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["Specific history"]);
        assert_eq!(folder.format_counts, unfiltered.format_counts, "folder facets also retain finished books");
    }
}

#[cfg(test)]
mod activity_subscription_tests {
    use super::*;

    #[tokio::test]
    async fn subscription_reports_thumbnail_cancellation_and_overlapping_work() {
        let runtime = Arc::new(crate::library::session::LibraryRuntime::default());
        let (events, changes) = crate::library::events::library_event_channel();
        let first = runtime.begin_thumbnail_generation(events.clone());
        assert!(matches!(changes.try_recv(), Ok(LibraryEvent::TransferStatusChanged)));
        let mut work = Box::pin(async {
            let _activity = runtime.begin_thumbnail_generation(events);
            std::future::pending::<()>().await;
        });
        assert!(futures_util::poll!(work.as_mut()).is_pending());
        assert!(matches!(changes.try_recv(), Ok(LibraryEvent::TransferStatusChanged)));
        drop(work);
        assert!(matches!(changes.try_recv(), Ok(LibraryEvent::TransferStatusChanged)));
        assert_eq!(runtime.thumbnail_generations_running(), 1);
        drop(first);
        assert!(matches!(changes.try_recv(), Ok(LibraryEvent::TransferStatusChanged)));
        assert_eq!(runtime.thumbnail_generations_running(), 0);
        assert!(changes.try_recv().is_err());
    }
}

#[cfg(all(test, feature = "watcher", not(target_arch = "wasm32")))]
mod watcher_recovery_tests {
    use super::*;

    #[test]
    fn watcher_can_register_after_library_folder_appears() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        let (sender, _receiver) = async_channel::bounded(1);
        assert!(start_filesystem_watcher(root.to_str().unwrap(), sender.clone()).is_none());
        std::fs::create_dir(&root).unwrap();
        assert!(start_filesystem_watcher(root.to_str().unwrap(), sender).is_some());
    }
}
