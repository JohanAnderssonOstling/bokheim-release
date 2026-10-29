//! Public handle for operations scoped to one concrete library.

use crate::executor::BackendExecutor;
use crate::library::ResolvedBook;
use crate::{DirectoryImport, ImportFailure, LibrarySyncState, LibrarySyncStatus};
#[cfg(not(target_arch = "wasm32"))]
use client_platform_runtime::pending::OnDrop;
use client_platform_runtime::startup_profile::StartupProfile;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::{future::Future, pin::Pin};
pub(crate) use std::sync::Arc;

use super::subscriptions::LibraryUpdates;
pub(crate) use crate::asset_store::AssetStore;
pub(crate) use crate::library::commands::LibraryCommand;
pub(crate) use crate::library::discovery::LibraryScanner;
pub(crate) use crate::library::events::{LibraryEvent, LibraryEventRouter, LibraryEventState};
pub(crate) use crate::{BookId, DirId, DownloadState, LibraryId};
use library_database::ContributorNameMatcher;
pub(crate) use library_database::Database;

pub(crate) use super::commands::PreparedRequest;
pub(crate) use super::pending::{Inbox, OperationKind, PendingOperations, PendingReply};
pub(crate) use crate::BackendError;

/// All operations whose meaning is scoped to one concrete library.
///
/// Application workflows supply policy and coordinate networking; this handle
/// has no reference back to the application.
pub struct LibrarySession {
    pub(crate) id: LibraryId,
    pub(crate) db: std::sync::Arc<Database>,
    pub(crate) assets: AssetStore,
    pub(crate) enrichment: library_enrichment::LibraryEnrichment,
    pub(crate) placement: Arc<library_placement::LibraryPlacement>,
    pub(crate) import: library_import::LibraryImport,
    pub(crate) sync: Arc<library_sync::LibrarySync>,
    /// Publishes events and wakes the workers that follow them.
    pub(crate) events: LibraryEventRouter,
    /// Moved into the actor before workers can borrow this session.
    pub(crate) initial_coordination: Option<LibraryEventState>,
    /// Task-owned guards keep worker admission synchronous while announcements queue.
    pub(crate) directory_imports: Cell<usize>,
    /// Taken once by the actor, whose root future runs the workers.
    pub(crate) requests: Option<super::workers::WorkerRequests>,
    pub(crate) runtime: Rc<crate::library::session::LibraryRuntime>,
    pub(crate) account: crate::integration::LibraryAccount,
    pub(crate) cloud: cloud_storage::connections::CloudConnections,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) root_failures: Option<async_channel::Sender<LibraryId>>,
    /// Kept only for library-owned accounting. The app registry never opens a
    /// library path merely to calculate a settings total.
    // Its Drop implementation stops and joins the scanner threads.
    pub(crate) _scanner: Arc<LibraryScanner>,
    pub(crate) cpu: std::sync::Arc<dyn cpu_host::CpuHost>,
    /// Filesystem or provider discovery, including inspection and admission.
    pub(crate) scanning: Cell<bool>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) torrents: super::torrents::TorrentService,
}

pub(crate) enum OperationCompletion {
    Reply(Box<dyn FnOnce()>),
    #[cfg(target_arch = "wasm32")]
    Book {
        reply: PendingReply<(client_runtime::reply::Reply, u64)>,
        result: Result<super::book::Opened, BackendError>,
    },
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum FolderScan {
    Complete,
    Deferred,
}

#[cfg(all(feature = "watcher", not(target_arch = "wasm32")))]
pub(crate) fn start_filesystem_watcher(root: &str, scan: library_runtime::scheduler::SyncScheduler) -> notify::Result<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let root = PathBuf::from(root);
    let watched_root = root.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                log::warn!("library filesystem watcher error: {error}");
                scan.wake();
                return;
            }
        };
        if event.kind.is_access() {
            return;
        }
        if event.need_rescan() || event.paths.is_empty() {
            scan.wake();
            return;
        }
        for path in event.paths {
            let Ok(relative) = path.strip_prefix(&watched_root) else { continue };
            let hidden = relative.components().any(|component| component.as_os_str().to_string_lossy().starts_with('.'));
            if hidden {
                continue;
            }
            scan.wake();
            return;
        }
    })?;
    watcher.watch(&root, notify::RecursiveMode::Recursive)?;
    Ok(watcher)
}

pub(crate) struct LibraryActor {
    pub(crate) coordination: LibraryEventState,
    pub(crate) operations: PendingOperations<OperationCompletion>,
    pub(crate) session: Rc<LibrarySession>,
    requests: Option<super::workers::WorkerRequests>,
    #[cfg(not(target_arch = "wasm32"))]
    restart: Option<(super::LibraryOpenConfig, async_channel::Sender<Result<(), BackendError>>)>,
    /// Browser book readers and Blob URLs are thread-local resources.
    /// The actor retains them for the lifetime of the page subscription; only
    /// a Send cancellation guard leaves the actor.
    #[cfg(target_arch = "wasm32")]
    pub(crate) books: super::book::browser::BookResources,
}

impl Drop for LibraryActor {
    fn drop(&mut self) {
        self.coordination.close();
    }
}

impl LibraryActor {
    pub(crate) fn new(mut session: LibrarySession) -> Self {
        let requests = session.requests.take();
        let coordination = session.initial_coordination.take().expect("coordination belongs to one actor");
        // Replay whatever an earlier run left queued; admission then wakes enrichment.
        session.events.files.wake();
        Self {
            coordination,
            session: Rc::new(session),
            operations: Default::default(),
            requests,
            #[cfg(not(target_arch = "wasm32"))]
            restart: None,
            #[cfg(target_arch = "wasm32")]
            books: Default::default(),
        }
    }

    /// The library's root future: commands and every worker. When the
    /// command loop ends, the workers are dropped with it.
    pub(crate) async fn run(mut self, commands: async_channel::Receiver<LibraryActorCommand>, observers: impl Future<Output = ()> + 'static) {
        let commands = Inbox(commands);
        let mut observers: Pin<Box<dyn Future<Output = ()>>> = Box::pin(observers);
        loop {
            {
                let workers = super::workers::run_workers(self.session.clone(), self.requests.as_ref().expect("actor worker requests").clone(), &mut observers);
                tokio::select! {
                    () = workers => {}
                    () = self.serve_inbox(&commands) => {}
                }
            }
            #[cfg(target_arch = "wasm32")]
            break;
            #[cfg(not(target_arch = "wasm32"))]
            {
                let Some((config, reply)) = self.restart.take() else { break };
                // Drop workers and old requests before rebuilding storage. Keep
                // this actor's command channel and UI subscriptions alive.
                self.operations = Default::default();
                let spec = super::LibraryRuntimeSpec {
                    config,
                    account: self.session.account.clone(),
                    cloud: self.session.cloud.clone(),
                    notification_interest: self.session.sync.notification_interest.clone(),
                    root_failures: self.session.root_failures.clone(),
                };
                let result = LibrarySession::recover(spec).map(|mut session| {
                    self.coordination.restart(session.initial_coordination.take().expect("new session coordination"));
                    self.requests = session.requests.take();
                    observers = Box::pin(super::workers::observe_account(session.account.clone(), session.events.clone()));
                    session.events.sync.request_remote_refresh();
                    session.events.assets.wake();
                    session.events.files.wake();
                    self.session = Rc::new(session);
                });
                let _ = reply.try_send(result);
            }
        }
    }

    // Commands never suspend this loop. It owns pending operations and polls
    // their completions beside commands.
    #[cfg(test)]
    async fn serve(&mut self, commands: async_channel::Receiver<LibraryActorCommand>) {
        self.serve_inbox(&Inbox(commands)).await;
    }

    async fn serve_inbox(&mut self, commands: &Inbox<LibraryActorCommand>) {
        let events = self.coordination.receiver();
        loop {
            // Retirement closes admission immediately; do not execute a
            // backlog of commands against an already-retired generation.
            if commands.0.is_closed() {
                break;
            }
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) if !commands.0.is_closed() => self.coordination.apply(event, &self.session.events),
                    _ => break,
                },
                Some(completion) = self.operations.next(), if !self.operations.is_empty() => self.complete_operation(completion),
                command = commands.0.recv() => match command {
                    Ok(command) => {
                        if !self.handle_command(command) {
                            break;
                        }
                    }
                    Err(_) => break,
                },
            }
        }
    }

    /// Applies one command in receive order. Returns false on shutdown.
    fn handle_command(&mut self, command: LibraryActorCommand) -> bool {
        self.coordination.drain(&self.session.events);
        match command {
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::RecoverRoot { config, reply } => {
                self.restart = Some((config, reply));
                return false;
            }
            LibraryActorCommand::SubscribeUpdates { reply } => {
                if !reply.is_closed() {
                    // Registration and the initial snapshot are one synchronous
                    // actor operation, so no event can fall between them.
                    let updates = self.coordination.subscribe_updates();
                    let scanning = self.session.scanner_running();
                    let _ = reply.try_send(Ok((updates, scanning)));
                }
            }
            LibraryActorCommand::Ui { command, reply } => {
                let reply = PendingReply::new(reply);
                match super::commands::prepare_library_request(self, command) {
                    Ok(PreparedRequest::Ready(value)) => {
                        self.coordination.drain(&self.session.events);
                        reply.finish(Ok(value));
                    }
                    Ok(PreparedRequest::Pending(operation)) => self.submit(operation.kind, reply, operation.future),
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
                drop(self.books.take(id));
            }
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::ResolveBook { content_hash, reply } => {
                let session = self.session.clone();
                let mut profile = StartupProfile::new("book_lookup_actor");
                profile.mark("received");
                self.submit(OperationKind::Read, PendingReply::new(reply), async move {
                    profile.mark("operation_start");
                    let result = session.prepare_book_for_reader(content_hash).await;
                    profile.mark("operation_complete");
                    result
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::ResolveAudiobookTrack { content_hash, index, reply } => {
                let session = self.session.clone();
                self.submit(OperationKind::Read, PendingReply::new(reply), async move { session.prepare_audiobook_track(content_hash, index).await });
            }
            LibraryActorCommand::ExportTransferBook { content_hash, reply } => {
                let session = self.session.clone();
                self.submit(OperationKind::Read, PendingReply::new(reply), async move { session.export_transfer_book(content_hash).await });
            }
            LibraryActorCommand::ImportTransferBook { parent_id, file_name, resource, reply } => {
                let session = self.session.clone();
                self.submit(OperationKind::Import, PendingReply::new(reply), async move {
                    let _activity = session.directory_import_activity();
                    session.import_transfer_resource(parent_id, file_name, resource).await
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::ImportDirectory { parent_id, directory, create_root, reply } => {
                let session = self.session.clone();
                let cancelled = reply.clone();
                self.submit(OperationKind::Import, PendingReply::new(reply), async move { session.import_selected_directory(parent_id, directory, create_root, &|| cancelled.is_closed()).await });
            }
            LibraryActorCommand::Control(LibraryControl::SetAssetStorage { enabled, reset_cloud_presence }) => {
                if let Err(error) = self.session.db.set_asset_storage_policy(enabled, reset_cloud_presence) {
                    log::warn!("could not update cloud-storage policy for {}: {error}", self.session.id);
                } else {
                    self.session.sync.clone().set_asset_storage_enabled(enabled);
                    self.session.events.assets.wake();
                    self.session.events.sync.request_remote_refresh();
                }
            }
            LibraryActorCommand::Control(LibraryControl::Rename { name, refresh_remote }) => {
                if let Err(error) = self.session.db.set_remote_library_name(&name) {
                    log::warn!("could not update the remote name for {}: {error}", self.session.id);
                } else if refresh_remote {
                    self.session.events.sync.request_remote_refresh();
                }
            }
            LibraryActorCommand::Control(LibraryControl::RefreshRemote) => self.session.events.sync.request_remote_refresh(),
            LibraryActorCommand::Control(LibraryControl::CloudConnectionChanged) => self.session.events.assets.wake(),
            #[cfg(not(target_arch = "wasm32"))]
            LibraryActorCommand::Control(LibraryControl::RootUnavailable) => {
                if !self.session.assets.root_available() {
                    match self.session.db.reset_local_storage() {
                        Ok(hashes) => {
                            for hash in hashes {
                                self.coordination.reset_download(&hash);
                                self.session.events.emit(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::NotDownloaded });
                            }
                            self.session.events.publish_update(library_model::LibraryUpdate::Contents);
                        }
                        Err(error) => log::warn!("could not clear unavailable folder state: {error}"),
                    }
                }
            }
            LibraryActorCommand::Control(LibraryControl::RefreshCatalog) => self.session.events.publish_update(library_model::LibraryUpdate::Contents),
            LibraryActorCommand::Control(LibraryControl::Shutdown) => return false,
        }
        true
    }

    fn complete_operation(&mut self, completion: OperationCompletion) {
        self.coordination.drain(&self.session.events);
        match completion {
            OperationCompletion::Reply(reply) => reply(),
            #[cfg(target_arch = "wasm32")]
            OperationCompletion::Book { reply, result } => {
                if reply.is_closed() {
                    return;
                }
                // Both registration and reply occur synchronously on the
                // browser thread, so cancellation cannot interleave.
                reply.finish(result.and_then(|(book, lease)| {
                    let id = self.books.next_id().map_err(BackendError::message)?;
                    self.books.insert(id, lease);
                    Ok((client_runtime::reply::Reply::new(book), id))
                }));
            }
        }
    }

    pub(crate) fn submit<T: 'static>(&mut self, kind: OperationKind, reply: PendingReply<T>, operation: impl std::future::Future<Output = Result<T, BackendError>> + 'static) {
        self.operations.submit(kind, reply, operation, |reply, result| OperationCompletion::Reply(Box::new(move || reply.finish(result))));
    }
}

/// The only app-facing control surface for a running library.  It deliberately
/// exposes no database, asset store, or mutable worker state.
#[derive(Clone)]
pub struct LibraryHandle {
    pub(crate) id: LibraryId,
    pub commands: async_channel::Sender<LibraryActorCommand>,
}

#[derive(Clone, Debug)]
pub enum LibraryControl {
    #[cfg(not(target_arch = "wasm32"))]
    RootUnavailable,
    SetAssetStorage { enabled: bool, reset_cloud_presence: bool },
    Rename { name: String, refresh_remote: bool },
    RefreshRemote,
    RefreshCatalog,
    Shutdown,
    CloudConnectionChanged,
}

pub enum LibraryActorCommand {
    #[cfg(not(target_arch = "wasm32"))]
    RecoverRoot {
        config: super::LibraryOpenConfig,
        reply: async_channel::Sender<Result<(), BackendError>>,
    },
    SubscribeUpdates {
        reply: async_channel::Sender<Result<(async_channel::Receiver<library_model::LibraryUpdate>, bool), BackendError>>,
    },
    Control(LibraryControl),
    Ui {
        command: LibraryCommand,
        reply: async_channel::Sender<Result<client_runtime::reply::Reply, BackendError>>,
    },
    #[cfg(target_arch = "wasm32")]
    SubscribeBook {
        content_hash: BookId,
        reply: async_channel::Sender<Result<(client_runtime::reply::Reply, u64), BackendError>>,
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
        content_hash: BookId,
        reply: async_channel::Sender<Result<ResolvedBook, BackendError>>,
    },
    #[cfg(not(target_arch = "wasm32"))]
    ResolveAudiobookTrack {
        content_hash: BookId,
        index: usize,
        reply: async_channel::Sender<Result<ResolvedBook, BackendError>>,
    },
    ExportTransferBook {
        content_hash: BookId,
        reply: async_channel::Sender<Result<super::transfer::TransferResource, BackendError>>,
    },
    ImportTransferBook {
        parent_id: DirId,
        file_name: String,
        resource: super::transfer::TransferResource,
        reply: async_channel::Sender<Result<library_database::transfer::ImportedBook, BackendError>>,
    },
    #[cfg(not(target_arch = "wasm32"))]
    ImportDirectory {
        parent_id: DirId,
        directory: DirectoryImport,
        create_root: bool,
        reply: async_channel::Sender<Result<Vec<ImportFailure>, BackendError>>,
    },
}

/// A Send-only token for a browser book held by the library actor.
/// Dropping the worker subscription releases its actor-owned resource.
#[cfg(target_arch = "wasm32")]
pub struct BookSubscription {
    pub commands: async_channel::Sender<LibraryActorCommand>,
    pub(crate) id: u64,
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
    pub fn open(spec: super::LibraryRuntimeSpec, _executor: &BackendExecutor) -> Result<Self, BackendError> {
        Self::open_with(spec, LibrarySession::open)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_recovered(spec: super::LibraryRuntimeSpec, _executor: &BackendExecutor) -> Result<Self, BackendError> {
        Self::open_with(spec, LibrarySession::recover)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_with(spec: super::LibraryRuntimeSpec, open: fn(super::LibraryRuntimeSpec) -> Result<LibrarySession, BackendError>) -> Result<Self, BackendError> {
        let mut profile = StartupProfile::for_library("library_handle", spec.config.id);
        let library_executor = crate::executor::LibraryExecutor::new().map_err(BackendError::operation)?;
        profile.mark("thread_spawn");
        let executor_for_actor = library_executor.clone();
        let handle = library_executor
            .run_blocking(move || -> Result<LibraryHandle, BackendError> {
                profile.mark("thread_ready");
                let session = open(spec)?;
                profile.mark("session_open");
                session.events.sync.request_remote_refresh();
                session.events.assets.wake();
                let handle = session.spawn_actor(executor_for_actor);
                profile.mark("actor_schedule");
                Ok(handle)
            })
            .map_err(BackendError::operation)??;
        Ok(handle)
    }
    #[cfg(target_arch = "wasm32")]
    pub fn open(spec: super::LibraryRuntimeSpec, executor: &BackendExecutor) -> Result<Self, BackendError> {
        let session = LibrarySession::open(spec)?;
        session.events.sync.request_remote_refresh();
        session.events.assets.wake();
        Ok(session.spawn_actor(executor))
    }

    pub fn id(&self) -> LibraryId {
        self.id
    }

    /// Ask this library's actor to schedule a remote sync pass.
    pub fn request_remote_refresh(&self) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::RefreshRemote));
    }

    pub fn cloud_connection_changed(&self) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::CloudConnectionChanged));
    }

    pub fn refresh_catalog(&self) {
        let _ = self.commands.try_send(LibraryActorCommand::Control(LibraryControl::RefreshCatalog));
    }

    pub fn shutdown(&self) {
        // Channel closure rejects admission and terminates update receivers
        // immediately, even before the owning actor is next polled.
        self.commands.close();
    }

    pub async fn subscribe_updates(&self) -> Result<(LibraryUpdates, bool), BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::SubscribeUpdates { reply }).await.map_err(|_| BackendError::message("library actor is no longer running"))?;
        let (updates, scanning) = tokio::select! {
            biased;
            _ = self.commands.closed() => return Err(BackendError::message("library actor is no longer running")),
            result = result.recv() => result.map_err(|_| BackendError::message("library actor stopped before subscribing"))??,
        };
        if self.commands.is_closed() {
            return Err(BackendError::message("library actor is no longer running"));
        }
        Ok((LibraryUpdates::new(updates, self.commands.clone()), scanning))
    }

    /// Delivers a command to the owning actor and waits for its reply.
    pub async fn dispatch(&self, command: LibraryCommand) -> Result<client_runtime::reply::Reply, BackendError> {
        if self.commands.is_closed() {
            return Err("library actor is no longer running".into());
        }
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::Ui { command, reply }).await.map_err(|_| "library actor is no longer running".to_owned())?;
        result.recv().await.map_err(|_| "library actor stopped before replying".to_owned())?
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn subscribe_book(&self, content_hash: BookId) -> Result<(client_runtime::reply::Reply, BookSubscription), BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::SubscribeBook { content_hash, reply }).await.map_err(|_| "library actor is no longer running".to_owned())?;
        let (initial, id) = result.recv().await.map_err(|_| "library actor stopped before replying".to_owned())??;
        Ok((initial, BookSubscription { commands: self.commands.clone(), id }))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn resolve_book(&self, content_hash: BookId) -> Result<ResolvedBook, BackendError> {
        let mut profile = StartupProfile::new("book_lookup_handle");
        let (reply, result) = async_channel::bounded(1);
        profile.mark("request_ready");
        self.commands.send(LibraryActorCommand::ResolveBook { content_hash, reply }).await.map_err(|_| BackendError::message("library actor is no longer running"))?;
        profile.mark("command_send");
        let result = result.recv().await.map_err(|_| BackendError::message("library actor stopped before replying"))?;
        profile.mark("reply_wait");
        result
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn resolve_audiobook_track(&self, content_hash: BookId, index: usize) -> Result<ResolvedBook, BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::ResolveAudiobookTrack { content_hash, index, reply }).await.map_err(|_| BackendError::message("library actor is no longer running"))?;
        result.recv().await.map_err(|_| BackendError::message("library actor stopped before replying"))?
    }

    pub(crate) async fn export_transfer_book(&self, content_hash: BookId) -> Result<super::transfer::TransferResource, BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::ExportTransferBook { content_hash, reply }).await.map_err(|_| BackendError::message("source library is no longer running"))?;
        result.recv().await.map_err(|_| BackendError::message("source library stopped before replying"))?
    }
    pub(crate) async fn import_transfer_book(&self, parent_id: DirId, file_name: String, resource: super::transfer::TransferResource) -> Result<library_database::transfer::ImportedBook, BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::ImportTransferBook { parent_id, file_name, resource, reply }).await.map_err(|_| BackendError::message("destination library is no longer running"))?;
        result.recv().await.map_err(|_| BackendError::message("destination library stopped before replying"))?
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn import_selected_directory(&self, parent_id: DirId, directory: DirectoryImport, create_root: bool) -> Result<Vec<ImportFailure>, BackendError> {
        let (reply, result) = async_channel::bounded(1);
        self.commands.send(LibraryActorCommand::ImportDirectory { parent_id, directory, create_root, reply }).await.map_err(|_| "library actor is no longer running".to_owned())?;
        result.recv().await.map_err(|_| "library actor stopped before replying".to_owned())?
    }
}

impl LibrarySession {
    /// Combines library-owned durable sync details with the application-owned
    /// account/discovery snapshot for settings presentation.
    pub(crate) fn sync_status(&self, signed_in: bool, remote_present: Option<bool>) -> Result<LibrarySyncStatus, BackendError> {
        let sync_details = self.db.stored_sync_status()?;
        let state = match signed_in {
            false => LibrarySyncState::LocalOnly,
            true => {
                if remote_present == Some(false) {
                    LibrarySyncState::RemoteMissing
                } else if !sync_details.has_unsynced_changes && sync_details.last_synced_at.is_none() {
                    if remote_present.is_some() {
                        LibrarySyncState::Receiving
                    } else {
                        LibrarySyncState::Checking
                    }
                } else if sync_details.has_unsynced_changes {
                    LibrarySyncState::SyncPending
                } else {
                    LibrarySyncState::Synced
                }
            }
        };
        Ok(LibrarySyncStatus {
            asset_storage_enabled: self.sync.clone().asset_storage_enabled(),
            storage: self.db.storage_policy()?,
            pending_provider_uploads: self.db.pending_provider_upload_count()?,
            library_id: self.id,
            state,
            has_unsynced_changes: sync_details.has_unsynced_changes,
            last_synced_at: sync_details.last_synced_at,
            issues: sync_details.issues,
        })
    }

    pub(crate) async fn local_storage_usage(&self) -> Result<Option<u64>, BackendError> {
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
    pub(crate) fn spawn_actor(self, #[cfg(not(target_arch = "wasm32"))] library_executor: crate::executor::LibraryExecutor, #[cfg(target_arch = "wasm32")] executor: &BackendExecutor) -> LibraryHandle {
        let (commands, receiver) = async_channel::unbounded();
        let handle = LibraryHandle { id: self.id, commands };
        // Subscribe before spawning so startup cannot miss an account change.
        let observers = super::workers::observe_account(self.account.clone(), self.events.clone());
        let library = LibraryActor::new(self).run(receiver, observers);
        #[cfg(not(target_arch = "wasm32"))]
        crate::executor::LibraryExecutor::spawn_local_detached(async move {
            // Keep the dedicated executor alive until the library retires.
            let _executor = library_executor;
            library.await;
        });
        #[cfg(target_arch = "wasm32")]
        executor.spawn_detached(library);
        handle
    }

    pub(crate) fn emit(&self, event: LibraryEvent) {
        self.events.emit(event);
    }

    pub(crate) fn directory_imports_active(&self) -> bool {
        self.directory_imports.get() != 0
    }

    pub fn id(&self) -> &LibraryId {
        &self.id
    }

    pub fn scanner_running(&self) -> bool {
        self.scanning.get() || self.directory_imports_active()
    }

    /// Walks the library folder and applies what it finds, walking again while
    /// the database changed underneath. Errors and cancellation release the
    /// scanning flag too, so file replay can continue between scan attempts.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) async fn scan_folder(&self) -> Result<FolderScan, BackendError> {
        if self.db.storage_policy()?.external().is_some() {
            return Ok(FolderScan::Complete);
        }
        if !self.assets.local_access_enabled() || !self._scanner.enabled() {
            self.scanning.set(false);
            return Ok(FolderScan::Complete);
        }
        if !self.assets.root_available() {
            return Err(BackendError::classified(client_runtime::Failure::LibraryFolderUnavailable, "library folder identity is unavailable"));
        }
        self.scanning.set(true);
        let scanning = OnDrop::new(|| {
            self.scanning.set(false);
            self.emit(LibraryEvent::ScanComplete { busy: self.scanner_running() });
        });
        let outcome = loop {
            let snapshot = self.db.scan_snapshot()?;
            let outcome = if snapshot.excluded_paths.is_empty() { FolderScan::Complete } else { FolderScan::Deferred };
            self.emit(LibraryEvent::ScanStarted);
            match futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(self.scan_once(snapshot))).await.unwrap_or_else(|_| Err(BackendError::message("library scan panicked")))? {
                library_database::ScanApplyResult::Applied => break outcome,
                // Coalesce changes while the actor keeps serving requests.
                library_database::ScanApplyResult::Stale => crate::executor::sleep(std::time::Duration::from_millis(250)).await.map_err(BackendError::operation)?,
            }
        };
        self.emit(LibraryEvent::ContentsChanged);
        drop(scanning);
        self.db.bind_torrent_books()?;
        // A completed scan can leave lifecycle and metadata mutations
        // that need a sync pass before dependent asset work is ready.
        self.events.sync.wake();
        Ok(outcome)
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn scan_once(&self, snapshot: library_database::ScanSnapshot) -> Result<library_database::ScanApplyResult, BackendError> {
        // Walking directories and fingerprinting files is blocking I/O.
        // Keep it off the library thread.
        let (mut snapshot, mut discovery) = crate::executor::run_blocking({
            let scanner = self._scanner.clone();
            move || {
                let discovery = scanner.discover(&snapshot);
                (snapshot, discovery)
            }
        })
        .await
        .map_err(BackendError::operation)?;
        let mut readable = discovery.readable;
        for files in discovery.files.chunks(filesystem_scanner::filesystem_scan::PUBLISH_BATCH_SIZE) {
            let chunk = library_database::FilesystemScan { files: files.to_vec(), readable: discovery.readable, ..Default::default() };
            let inspected = self._scanner.inspect(&chunk, &snapshot, &self.cpu).await;
            if !self.assets.root_available() {
                return Err(BackendError::message("library folder changed during scanning"));
            }
            readable &= inspected.readable;
            if !inspected.books.is_empty() {
                let result = self.db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &inspected, false, Some(ContributorNameMatcher::Normalized)).map_err(BackendError::operation)?;
                if result == library_database::ScanApplyResult::Stale {
                    return Ok(result);
                }
                discovery.directories.clear();
                self.emit(LibraryEvent::ContentsChanged);
                self.events.sync.wake();
            }
            // Deliver queries and partial contents before inspecting another batch.
            tokio::task::yield_now().await;
        }
        if !self.assets.root_available() {
            return Err(BackendError::message("library folder changed during scanning"));
        }
        let result =
            self.db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &library_database::InspectedFilesystemScan { readable, books: vec![] }, true, Some(ContributorNameMatcher::Normalized)).map_err(BackendError::operation)?;
        if result == library_database::ScanApplyResult::Applied && !discovery.readable {
            return Err(BackendError::message("filesystem scan was incomplete"));
        }
        Ok(result)
    }

    pub(crate) fn download_state(&self, coordination: &LibraryEventState, content_hash: BookId) -> Result<DownloadState, BackendError> {
        let durable = self.persistent_download_state(content_hash)?;
        Ok(match (&durable, coordination.download_state(&content_hash).cloned()) {
            (DownloadState::Queued, Some(state @ DownloadState::Downloading(_))) => state,
            _ => durable,
        })
    }

    pub(crate) fn queue_download(&self, coordination: &mut LibraryEventState, content_hash: BookId) -> Result<DownloadState, BackendError> {
        let state = self.download_state(coordination, content_hash)?;
        if matches!(state, DownloadState::Downloaded | DownloadState::Downloading(_)) {
            return Ok(state);
        }
        let sync = self.sync.clone();
        if self.db.storage_policy()?.external().is_none() && !sync.is_signed_in() {
            return Err(BackendError::message("sign in before downloading this book"));
        }
        {
            coordination.reset_download(&content_hash);
        }
        self.request_downloads(&[content_hash])?;
        Ok(DownloadState::Queued)
    }

    pub(crate) fn request_downloads(&self, hashes: &[BookId]) -> Result<(), BackendError> {
        if hashes.is_empty() {
            return Ok(());
        }
        #[cfg(target_os = "android")]
        if self.db.storage_policy()?.external().is_some_and(|root| root.scope.provider == sync_common::storage::CloudProvider::GoogleDrive) {
            cloud_storage::require_google_drive().map_err(BackendError::operation)?;
        }
        #[cfg(target_arch = "wasm32")]
        if self.db.storage_policy()?.external().is_none() {
            return Err(BackendError::message("this library streams books in the browser"));
        }
        self.sync.clone().request_downloads(hashes).map_err(BackendError::operation)?;
        self.events.assets.wake();
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "concurrency_tests.rs"]
pub(super) mod concurrency_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "transfer_tests.rs"]
mod transfer_tests;

#[cfg(all(test, feature = "google-drive", not(target_arch = "wasm32")))]
#[path = "cloud/transfer_tests.rs"]
mod provider_transfer_tests;

#[cfg(all(test, feature = "watcher", not(target_arch = "wasm32")))]
mod watcher_recovery_tests {
    use super::*;

    #[test]
    fn watcher_can_register_after_library_folder_appears() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        let (scan, _requests) = library_runtime::scheduler::SyncScheduler::new();
        assert!(start_filesystem_watcher(root.to_str().unwrap(), scan.clone()).is_err());
        std::fs::create_dir(&root).unwrap();
        assert!(start_filesystem_watcher(root.to_str().unwrap(), scan).is_ok());
    }
}
