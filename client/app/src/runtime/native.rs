use crate::app::AppBackend;
use crate::host::startup_profile::StartupProfile;
use crate::BackendError;
use client_platform_runtime::storage_queue::{self, Priority};
use library_backend::{library::LibraryDirectory, LibraryHandle};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::watch;
#[derive(Clone)]
pub(crate) struct Transport {
    jobs: async_channel::Sender<BackendJob>,
    libraries: Arc<LibraryDirectory>,
    library_startup: watch::Receiver<Option<Result<(), BackendError>>>,
}

use super::*;
use futures_util::{FutureExt, StreamExt};
use std::panic::AssertUnwindSafe;

fn request_panicked(_: Box<dyn std::any::Any + Send>) -> BackendError {
    log::error!("backend request panicked; request failed without stopping the worker");
    BackendError::message("backend request failed unexpectedly")
}

#[cfg(test)]
mod panic_tests {
    use super::*;

    #[tokio::test]
    async fn request_panics_preserve_worker_and_other_pending_requests() {
        let root = tempfile::tempdir().unwrap();
        let context = crate::BackendContext::initialize(crate::AppDataLocation::native_path(root.path())).unwrap();
        let backend = AppBackend::new(context).unwrap();
        let (client, worker) = channel(&backend);
        let exercise = async move {
            let (started, ready) = oneshot::channel();
            let (release, resume) = oneshot::channel();
            let pending = client.transport.run(move |_| async move {
                started.send(()).unwrap();
                resume.await.unwrap();
                Ok(42)
            });
            let failures = async {
                ready.await.unwrap();
                let construction = client.transport.run::<(), _, std::future::Ready<Result<(), BackendError>>>(|_| panic!("construction failure")).await;
                assert_eq!(construction.unwrap_err().to_string(), "backend request failed unexpectedly");
                let polling = client
                    .transport
                    .run::<(), _, _>(|_| async {
                        tokio::task::yield_now().await;
                        panic!("polling failure");
                    })
                    .await;
                assert_eq!(polling.unwrap_err().to_string(), "backend request failed unexpectedly");
                assert_eq!(client.transport.run_ordered::<(), _>(|_| panic!("ordered failure")).await.unwrap_err().to_string(), "backend request failed unexpectedly");
                assert_eq!(client.transport.run_ordered(|_| Ok(7)).await.unwrap(), 7);
                assert_eq!(client.transport.run::<(), _, _>(|_| async { Err("ordinary error".into()) }).await.unwrap_err().to_string(), "ordinary error");
                release.send(()).unwrap();
            };
            let (result, ()) = tokio::join!(pending, failures);
            assert_eq!(result.unwrap(), 42);
            drop(client);
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(worker.serve(backend), exercise);
        })
        .await
        .expect("worker must survive failures and drain on shutdown");
    }
}

type NativeJobFuture = std::pin::Pin<Box<dyn Future<Output = ()>>>;

pub(crate) enum BackendJob {
    Run(Box<dyn FnOnce(AppBackend) -> NativeJobFuture + Send>),
    RunOrdered(Box<dyn FnOnce(&AppBackend) + Send>),
}

pub(crate) struct AppWorker {
    jobs: async_channel::Receiver<BackendJob>,
    library_startup: watch::Sender<Option<Result<(), BackendError>>>,
    initialize_libraries: bool,
}

pub(crate) fn channel(backend: &AppBackend) -> (AppClient, AppWorker) {
    channel_with_startup(backend, false)
}

fn channel_with_startup(backend: &AppBackend, initialize_libraries: bool) -> (AppClient, AppWorker) {
    let (jobs, incoming) = async_channel::bounded(64);
    let (library_startup, ready) = watch::channel((!initialize_libraries).then_some(Ok(())));
    (AppClient { transport: Transport { jobs, libraries: backend.library_directory().clone(), library_startup: ready } }, AppWorker { jobs: incoming, library_startup, initialize_libraries })
}

impl AppWorker {
    /// Deferred startup opens every library before serving queued commands.
    /// Settings then run in receive order; waiting operations run concurrently.
    pub async fn serve(self, backend: AppBackend) {
        if self.initialize_libraries {
            crate::executor::library_maintenance_ready().await;
            let initializer = backend.clone();
            let result = storage_queue::run(Priority::Background, move || initializer.open_startup_libraries()).await.map_err(BackendError::message).and_then(|result| result);
            self.library_startup.send_replace(Some(result.clone()));
            if let Err(error) = result {
                log::error!("could not open startup libraries: {error}");
                return;
            }
        }
        let mut running = futures_util::stream::FuturesUnordered::new();
        loop {
            tokio::select! {
                job = self.jobs.recv() => match job {
                    Ok(BackendJob::RunOrdered(operation)) => operation(&backend),
                    Ok(BackendJob::Run(operation)) => {
                        let backend = backend.clone();
                        running.push(async move {
                            operation(backend).await;
                        });
                    }
                    Err(_) => break,
                },
                Some(()) = running.next(), if !running.is_empty() => {},
            }
        }
        while running.next().await.is_some() {}
    }
}

impl Transport {
    pub(crate) async fn library_handle(&self, library_id: LibraryId) -> Result<LibraryHandle, BackendError> {
        let mut profile = StartupProfile::new("book_lookup_transport");
        self.wait_for_library_startup().await?;
        profile.mark("startup_wait");
        let libraries = self.libraries.clone();
        let (handle, mut profile) = storage_queue::run(Priority::Interactive, move || {
            profile.mark("storage_dispatch");
            let handle = libraries.get(&library_id);
            profile.mark("directory_get");
            (handle, profile)
        }).await.map_err(BackendError::message)?;
        profile.mark("storage_return");
        handle
    }

    async fn wait_for_library_startup(&self) -> Result<(), BackendError> {
        let mut ready = self.library_startup.clone();
        loop {
            let result = ready.borrow_and_update().clone();
            if let Some(result) = result {
                return result;
            }
            ready.changed().await.map_err(|_| BackendError::message("backend stopped during library startup"))?;
        }
    }

    pub(crate) async fn run<T, F, Fut>(&self, operation: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce(AppBackend) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, BackendError>> + 'static,
    {
        let (response, result) = oneshot::channel();
        let operation = Box::new(move |backend| {
            Box::pin(async move {
                // Keep construction and polling inside the boundary, while
                // retaining the reply sender outside it. Never retry a write
                // automatically: it may have committed before panicking.
                let result = AssertUnwindSafe(async move { operation(backend).await }).catch_unwind().await.map_err(request_panicked).and_then(|result| result);
                let _ = response.send(result);
            }) as NativeJobFuture
        });
        self.jobs.send(BackendJob::Run(operation)).await.map_err(|_| "backend worker stopped during request".to_owned())?;
        result.await.map_err(|_| "backend worker stopped before replying".to_owned())?
    }

    pub(crate) async fn run_ordered<T, F>(&self, operation: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce(&AppBackend) -> Result<T, BackendError> + Send + 'static,
    {
        let (response, result) = oneshot::channel();
        self.jobs
            .send(BackendJob::RunOrdered(Box::new(move |backend| {
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| operation(backend))).map_err(request_panicked).and_then(|result| result);
                let _ = response.send(result);
            })))
            .await
            .map_err(|_| "backend worker stopped during request".to_owned())?;
        result.await.map_err(|_| "backend worker stopped before replying".to_owned())?
    }
}

impl Transport {
    pub(crate) async fn request<T: LibraryReply>(&self, command: AppCommand) -> Result<T, BackendError> {
        if command.is_settings_update() {
            return self.run_ordered(move |backend| super::requests::execute_ordered(backend, command)).await.and_then(client_runtime::reply::Reply::take);
        }
        self.run(move |backend| async move {
            match super::requests::dispatch_request(backend, command).await? {
                WorkerDispatchResult::Response(value) => Ok(value),
                WorkerDispatchResult::Subscription { .. } => Err("backend returned a subscription for a request".into()),
            }
        })
        .await
        .and_then(client_runtime::reply::Reply::take)
    }

    async fn subscribe<T: LibraryReply>(&self, command: AppCommand) -> Result<(WorkerSubscription, T), BackendError> {
        self.run(move |backend| async move {
            match super::requests::dispatch_request(backend, command).await? {
                WorkerDispatchResult::Subscription { initial, events } => Ok((events, initial)),
                WorkerDispatchResult::Response(_) => Err("backend returned a response for a subscription".into()),
            }
        })
        .await
        .and_then(|(events, initial)| Ok((events, initial.take()?)))
    }

    pub(super) async fn unit_subscription(&self, command: AppCommand) -> Result<async_channel::Receiver<()>, BackendError> {
        let (WorkerSubscription::Unit(events), ()) = self.subscribe::<()>(command).await? else { return Err("unexpected subscription type".into()) };
        Ok(events)
    }

    pub(crate) async fn library_list_changes_transport(&self) -> Result<watch::Receiver<()>, BackendError> {
        let (WorkerSubscription::LibraryList(events), ()) = self.subscribe::<()>(AppCommand::SubscribeLibraryList).await? else { return Err("unexpected subscription type".into()) };
        Ok(events)
    }
}

impl AppClient {
    /// Loads startup metadata and starts the application-owned backend thread.
    /// Hosts may defer the library-opening phase until their first frame.
    pub fn start_native(launch: crate::api::BackendLaunch) -> Result<(Self, AppStartupState), BackendError> {
        let mut profile = StartupProfile::new("native_backend");
        let defer_libraries = launch.libraries_after_first_frame;
        let backend_context = launch.into_context()?;
        profile.mark("registry_open");
        let backend = if defer_libraries { AppBackend::new_without_libraries(backend_context) } else { AppBackend::new(backend_context) }.map_err(BackendError::operation)?;
        profile.mark("backend_construct");
        let startup = backend.startup_state()?;
        profile.mark("startup_snapshot");
        let (client, worker) = if defer_libraries { channel_with_startup(&backend, true) } else { channel(&backend) };
        std::thread::Builder::new().name("bokheim-app-worker".to_owned()).spawn(move || backend.run_native_worker(worker)).map_err(|error| format!("failed to start application backend worker: {error}"))?;
        profile.mark("worker_start");
        Ok((client, startup))
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;
    use app_preferences::{ReaderPreferencePatch, ReaderPreferences};

    #[tokio::test]
    async fn settings_patches_are_ordered_by_the_worker() {
        let root = tempfile::tempdir().unwrap();
        let context = crate::app::BackendContext::initialize(crate::app::AppDataLocation::native_path(root.path())).unwrap();
        let (client, _) = AppClient::start_native(crate::BackendLaunch::initialize(crate::AppDataLocation::native_path(root.path())).unwrap()).unwrap();

        let first = client.update_reader_preferences(vec![ReaderPreferencePatch::LineHeight(1.8)]);
        let second = client.update_reader_preferences(vec![ReaderPreferencePatch::LineHeight(2.0), ReaderPreferencePatch::Zoom(1.7)]);
        let (first, second) = futures_util::future::join(first, second).await;
        first.unwrap();
        second.unwrap();

        let preferences = serde_json::from_slice::<ReaderPreferences>(&context.registry.read_value("reader_preferences.json").unwrap().unwrap()).unwrap();
        assert_eq!(preferences.line_height(), 2.0);
        assert_eq!(preferences.zoom(), 1.7);
    }
}
