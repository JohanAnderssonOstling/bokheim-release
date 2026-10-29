use super::startup::startup_ready;
use super::task::{owned_task, BackendTask};
use super::{BackendExecutorError, BackendFuture, BackendOutput, BackendSend, BackendTimeout};
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::watch;

// Foreground library commands remain available while the initial UI is built.
// Only scan/sync/import maintenance waits for the host's first presented frame.
// Hosts can also opt into opening their startup libraries after this gate.
static LIBRARY_MAINTENANCE_GATE: Mutex<Option<watch::Receiver<bool>>> = Mutex::new(None);

/// Releases startup maintenance when the host drops the guard.
pub struct LibraryMaintenancePermit(watch::Sender<bool>);

impl Drop for LibraryMaintenancePermit {
    fn drop(&mut self) {
        *LIBRARY_MAINTENANCE_GATE.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.0.send_replace(true);
    }
}

/// Hold library maintenance until this guard is dropped. This does not gate
/// the library actor's command loop or the update trial's side-effect barrier.
pub fn defer_library_maintenance() -> LibraryMaintenancePermit {
    let (sender, receiver) = watch::channel(false);
    let mut gate = LIBRARY_MAINTENANCE_GATE.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(gate.is_none(), "library maintenance is already deferred");
    *gate = Some(receiver);
    LibraryMaintenancePermit(sender)
}

/// Waits for the host's first-frame gate without gating open actors' commands.
pub async fn library_maintenance_ready() {
    let receiver = LIBRARY_MAINTENANCE_GATE.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(mut receiver) = receiver {
        let _ = receiver.wait_for(|ready| *ready).await;
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
                        // A failed job drops its reply without terminating the
                        // owner thread and stranding unrelated library work.
                        let _ = catch_unwind(AssertUnwindSafe(job));
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
    pub fn run_blocking<T: Send + 'static>(&self, job: impl FnOnce() -> T + Send + 'static) -> Result<T, BackendExecutorError> {
        let (reply, result) = std::sync::mpsc::channel();
        self.run(move || {
            let _ = reply.send(job());
        });
        result.recv().map_err(|_| BackendExecutorError("library executor stopped before returning a result".into()))
    }

    /// Spawns a non-`Send` future on this thread and returns a handle with
    /// the same cancel-on-drop semantics as [`BackendExecutor::spawn`]. Only
    /// for use by code already running on this thread (inside a job passed
    /// to [`Self::run`] or [`Self::run_blocking`]) — it does not hop threads.
    pub fn spawn_local<T: 'static>(future: impl Future<Output = T> + 'static) -> BackendTask<T> {
        let (task, runner) = owned_task(future);
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

pub async fn sleep(duration: std::time::Duration) -> Result<(), BackendExecutorError> {
    tokio::time::sleep(duration).await;
    Ok(())
}

pub async fn timeout<T>(duration: std::time::Duration, future: impl Future<Output = T>) -> Result<T, BackendTimeout> {
    tokio::time::timeout(duration, future).await.map_err(|_| BackendTimeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::defer_background_work;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn failed_library_job_returns_an_error_and_preserves_the_executor() {
        let executor = LibraryExecutor::new().unwrap();
        assert!(executor.run_blocking(|| panic!("failed library open")).is_err());
        assert_eq!(executor.run_blocking(|| 42).unwrap(), 42);
    }

    #[test]
    fn stopped_library_executor_returns_an_error() {
        let (submit, jobs) = tokio::sync::mpsc::unbounded_channel();
        drop(jobs);
        let executor = LibraryExecutor { submit };
        assert!(executor.run_blocking(|| 42).is_err());
    }

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

    #[test]
    fn maintenance_waits_for_first_frame_without_blocking_foreground_jobs() {
        let permit = defer_library_maintenance();
        let executor = LibraryExecutor::new().unwrap();
        let (started, waiting) = std::sync::mpsc::channel();
        let (finished, resumed) = std::sync::mpsc::channel();
        executor.run(move || {
            tokio::task::spawn_local(async move {
                started.send(()).unwrap();
                library_maintenance_ready().await;
                finished.send(()).unwrap();
            });
        });
        waiting.recv_timeout(Duration::from_secs(2)).expect("maintenance future started");
        let (foreground, reply) = std::sync::mpsc::channel();
        executor.run(move || foreground.send(42).unwrap());
        assert_eq!(reply.recv_timeout(Duration::from_secs(2)).unwrap(), 42, "foreground jobs remain available");
        assert!(resumed.recv_timeout(Duration::from_millis(30)).is_err());
        drop(permit);
        resumed.recv_timeout(Duration::from_secs(2)).expect("maintenance resumes when the frame releases its guard");
        let (late, reply) = std::sync::mpsc::channel();
        executor.run(move || {
            tokio::task::spawn_local(async move {
                library_maintenance_ready().await;
                late.send(()).unwrap();
            });
        });
        reply.recv_timeout(Duration::from_secs(2)).expect("later maintenance starts without waiting");
    }


}

/// Report available CPU capacity; callers choose their own concurrency limits.
pub fn available_parallelism() -> usize {
    std::thread::available_parallelism().map(|count| count.get()).unwrap_or(1)
}
