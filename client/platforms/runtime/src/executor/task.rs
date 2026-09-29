//! Task ownership and cancellation are identical on every executor.
use super::{BackendExecutorError, BackendFuture, BackendOutput};
use futures_util::future::{AbortHandle, Abortable};
use futures_util::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::oneshot;

/// An owned task. Dropping it cancels the future at its next polling boundary.
/// Synchronous work already executing cannot be interrupted.
#[must_use = "dropping the task cancels it; use spawn_detached for background work"]
pub struct BackendTask<T> {
    receiver: oneshot::Receiver<Result<T, BackendExecutorError>>,
    abort: AbortHandle,
}

impl<T> Future for BackendTask<T> {
    type Output = Result<T, BackendExecutorError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().receiver).poll(context).map(|result| result.unwrap_or_else(|_| Err(BackendExecutorError("backend task was cancelled".to_owned()))))
    }
}

impl<T> Drop for BackendTask<T> {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

pub(super) fn owned_task<T: BackendOutput>(future: impl BackendFuture<T>) -> (BackendTask<T>, impl BackendFuture<()>) {
    let (sender, receiver) = oneshot::channel();
    let (abort, registration) = AbortHandle::new_pair();
    let runner = async move {
        if let Ok(result) = Abortable::new(std::panic::AssertUnwindSafe(future).catch_unwind(), registration).await {
            let _ = sender.send(result.map_err(|_| BackendExecutorError("backend task panicked".to_owned())));
        }
    };
    (BackendTask { receiver, abort }, runner)
}

/// Same handle and cancel-on-drop semantics as [`owned_task`], but the future
/// need not be `Send` — for work confined to one dedicated thread, such as a
/// library's owned database connection.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn owned_local_task<T: 'static>(future: impl Future<Output = T> + 'static) -> (BackendTask<T>, impl Future<Output = ()> + 'static) {
    let (sender, receiver) = oneshot::channel();
    let (abort, registration) = AbortHandle::new_pair();
    let runner = async move {
        if let Ok(result) = Abortable::new(std::panic::AssertUnwindSafe(future).catch_unwind(), registration).await {
            let _ = sender.send(result.map_err(|_| BackendExecutorError("backend task panicked".to_owned())));
        }
    };
    (BackendTask { receiver, abort }, runner)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use futures_util::task::LocalSpawnExt;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct Released(Arc<AtomicBool>);
    impl Drop for Released {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn cancellation_releases_resources_before_or_after_first_poll() {
        for started in [false, true] {
            let mut pool = futures_executor::LocalPool::new();
            let released = Arc::new(AtomicBool::new(false));
            let resource = Released(released.clone());
            let (task, runner) = owned_task(async move {
                let _resource = resource;
                std::future::pending::<()>().await;
            });
            pool.spawner().spawn_local(runner).unwrap();
            if started {
                pool.run_until_stalled();
            }
            drop(task);
            pool.run_until_stalled();
            assert!(released.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn cancellation_prevents_work_after_a_suspension() {
        let mut pool = futures_executor::LocalPool::new();
        let (resume, wait) = oneshot::channel();
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let (task, runner) = owned_task(async move {
            let _ = wait.await;
            flag.store(true, Ordering::SeqCst);
        });
        pool.spawner().spawn_local(runner).unwrap();
        pool.run_until_stalled();
        drop(task);
        let _ = resume.send(());
        pool.run_until_stalled();
        assert!(!ran.load(Ordering::SeqCst));
    }
}
