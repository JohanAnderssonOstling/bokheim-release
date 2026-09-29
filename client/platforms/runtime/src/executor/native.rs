use super::task::{owned_local_task, owned_task, BackendTask};
use super::{BackendExecutorError, BackendFuture, BackendOutput, BackendSend, BackendTimeout};
use std::future::Future;
use std::sync::Arc;

// Startup trials may inspect local state, but background sync/import must not
// publish side effects before the host commits the new generation.
static STARTUP_GATE: std::sync::Mutex<Option<tokio::sync::watch::Receiver<bool>>> = std::sync::Mutex::new(None);

pub struct StartupPermit(tokio::sync::watch::Sender<bool>);
impl StartupPermit {
    pub fn release(self) {
        *STARTUP_GATE.lock().expect("startup gate poisoned") = None;
        self.0.send_replace(true);
    }
}
pub fn defer_background_work() -> StartupPermit {
    let (sender, receiver) = tokio::sync::watch::channel(false);
    let mut gate = STARTUP_GATE.lock().expect("startup gate poisoned");
    assert!(gate.is_none(), "startup work is already deferred");
    *gate = Some(receiver);
    StartupPermit(sender)
}
async fn startup_ready() {
    let receiver = STARTUP_GATE.lock().expect("startup gate poisoned").clone();
    if let Some(mut receiver) = receiver {
        loop {
            let ready = *receiver.borrow_and_update();
            if ready {
                break;
            }
            if receiver.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Native async-I/O execution boundary owned by the backend.
#[derive(Clone)]
pub struct BackendExecutor {
    inner: Arc<BackendExecutorInner>,
}

struct BackendExecutorInner {
    handle: tokio::runtime::Handle,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for BackendExecutorInner {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl BackendExecutor {
    pub fn new() -> Result<Self, BackendExecutorError> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("bokheim-async").enable_all().build().map_err(|error| BackendExecutorError(error.to_string()))?;
        let handle = runtime.handle().clone();
        Ok(Self { inner: Arc::new(BackendExecutorInner { handle, runtime: Some(runtime) }) })
    }

    pub fn spawn<T: BackendOutput>(&self, future: impl BackendFuture<T>) -> BackendTask<T> {
        let (task, runner) = owned_task(future);
        self.inner.handle.spawn(async move {
            startup_ready().await;
            runner.await
        });
        task
    }

    pub fn spawn_detached(&self, future: impl BackendFuture<()>) {
        self.inner.handle.spawn(async move {
            startup_ready().await;
            future.await
        });
    }

    pub fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        // Synchronous backend entry points bridge into the backend-owned runtime.
        self.inner.handle.block_on(future)
    }
}

/// One dedicated OS thread running a single-threaded Tokio `LocalSet`, owned
/// for the lifetime of one open library.
///
/// A library's database owns exactly one `rusqlite::Connection`, which is
/// `Send` but not `Sync`. Rather than making it `Sync` — a mutex around a
/// connection that is, by construction, only ever touched by its one owner —
/// every task that touches it runs here instead, on the one thread that owns
/// it. Nothing scheduled through this executor needs `Send`. Each open
/// library gets its own, so a slow query in one library never blocks another.
#[derive(Clone)]
pub struct LibraryExecutor {
    submit: tokio::sync::mpsc::UnboundedSender<LocalJob>,
}

type LocalJob = Box<dyn FnOnce() + Send>;

impl LibraryExecutor {
    pub fn new() -> Result<Self, BackendExecutorError> {
        let (submit, mut jobs) = tokio::sync::mpsc::unbounded_channel::<LocalJob>();
        std::thread::Builder::new()
            .name("bokheim-library".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("library executor thread failed to start: {error}");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, async move {
                    while let Some(job) = jobs.recv().await {
                        job();
                    }
                });
            })
            .map_err(|error| BackendExecutorError(error.to_string()))?;
        Ok(Self { submit })
    }

    /// Runs `job` on this thread. `job` must be `Send` to cross onto it, but
    /// because it then executes inside this thread's `LocalSet`, it may call
    /// `tokio::task::spawn_local` (or [`Self::spawn_local`]) from there to
    /// start non-`Send` background work — including anything touching this
    /// library's owned database connection.
    pub fn run(&self, job: impl FnOnce() + Send + 'static) {
        let _ = self.submit.send(Box::new(job));
    }

    /// Runs `job` on this thread and blocks the calling thread for its
    /// result. For synchronous, non-runtime call sites (library open) that
    /// need the outcome before continuing.
    pub fn run_blocking<T: Send + 'static>(&self, job: impl FnOnce() -> T + Send + 'static) -> T {
        let (reply, result) = std::sync::mpsc::channel();
        self.run(move || {
            let _ = reply.send(job());
        });
        result.recv().expect("library executor thread is running")
    }

    /// Spawns a non-`Send` future on this thread and returns a handle with
    /// the same cancel-on-drop semantics as [`BackendExecutor::spawn`]. Only
    /// for use by code already running on this thread (inside a job passed
    /// to [`Self::run`] or [`Self::run_blocking`]) — it does not hop threads.
    pub fn spawn_local<T: 'static>(future: impl Future<Output = T> + 'static) -> BackendTask<T> {
        let (task, runner) = owned_local_task(future);
        tokio::task::spawn_local(async move {
            startup_ready().await;
            runner.await
        });
        task
    }

    /// Same as [`Self::spawn_local`], but fire-and-forget: dropping the
    /// caller's side does not cancel it.
    pub fn spawn_local_detached(future: impl Future<Output = ()> + 'static) {
        tokio::task::spawn_local(async move {
            startup_ready().await;
            future.await
        });
    }
}

pub async fn run_blocking<T: BackendOutput>(operation: impl FnOnce() -> T + BackendSend + 'static) -> Result<T, BackendExecutorError> {
    tokio::task::spawn_blocking(operation).await.map_err(|error| BackendExecutorError(format!("backend blocking task failed: {error}")))
}

pub fn spawn_detached(future: impl BackendFuture<()>) {
    tokio::spawn(async move {
        startup_ready().await;
        future.await
    });
}

pub async fn sleep(duration: std::time::Duration) {
    tokio::time::sleep(duration).await;
}

pub async fn timeout<T>(duration: std::time::Duration, future: impl Future<Output = T>) -> Result<T, BackendTimeout> {
    tokio::time::timeout(duration, future).await.map_err(|_| BackendTimeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn startup_gate_prevents_background_side_effects_until_commit() {
        let permit = defer_background_work();
        let executor = BackendExecutor::new().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        executor.spawn_detached(async move {
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
        permit.release();
        received.recv_timeout(Duration::from_secs(2)).expect("background work resumes after commit");
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn executor_reuses_one_runtime_for_spawn_and_block_on() {
        let executor = BackendExecutor::new().unwrap();
        assert_send::<BackendTask<()>>();
        let first = executor.spawn(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            41_u8
        });
        assert_eq!(executor.block_on(first).unwrap(), 41);
        assert_eq!(
            executor.block_on(async {
                tokio::time::sleep(Duration::from_millis(1)).await;
                42_u8
            }),
            42
        );
    }

    #[test]
    fn dropping_backend_task_cancels_its_future() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let executor = BackendExecutor::new().unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = DropFlag(dropped.clone());
        let task = executor.spawn(async move {
            let _flag = flag;
            std::future::pending::<()>().await
        });
        std::thread::sleep(Duration::from_millis(10));
        drop(task);
        for _ in 0..100 {
            if dropped.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("cancelled backend future was not dropped");
    }

    #[test]
    fn task_panics_become_errors_at_the_host_boundary() {
        let executor = BackendExecutor::new().unwrap();
        let task = executor.spawn(async { panic!("broken backend operation") });
        let error = executor.block_on(task).unwrap_err();
        assert!(error.to_string().contains("panicked"));
    }
}

/// Report available CPU capacity; callers choose their own concurrency limits.
pub fn available_parallelism() -> usize {
    std::thread::available_parallelism().map(|count| count.get()).unwrap_or(1)
}
