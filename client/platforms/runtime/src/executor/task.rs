//! Task ownership and cancellation are identical on every executor.

use super::BackendExecutorError;
use futures_util::future::{AbortHandle, Abortable};
use futures_util::FutureExt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
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

// Send requirements belong to the spawning executor. The returned future is
// Send whenever its input and output are, so local tasks use the same runner.
pub(super) fn owned_task<T: 'static>(future: impl Future<Output = T> + 'static) -> (BackendTask<T>, impl Future<Output = ()> + 'static) {
    let (sender, receiver) = oneshot::channel();
    let (abort, registration) = AbortHandle::new_pair();
    let runner = async move {
        if let Ok(result) = Abortable::new(AssertUnwindSafe(future).catch_unwind(), registration).await {
            let _ = sender.send(result.map_err(|_| BackendExecutorError("backend task panicked".to_owned())));
        }
    };
    (BackendTask { receiver, abort }, runner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, future::pending, rc::Rc};

    #[tokio::test]
    async fn local_tasks_can_hold_and_return_non_send_values() {
        let value = Rc::new(42);
        let owned = value.clone();
        let (task, runner) = owned_task(async move {
            tokio::task::yield_now().await;
            owned
        });
        runner.await;
        assert!(Rc::ptr_eq(&task.await.unwrap(), &value));
    }

    #[tokio::test]
    async fn panics_settle_the_owned_task_with_an_error() {
        let (task, runner) = owned_task(async { panic!("failed task") });
        runner.await;
        assert_eq!(task.await.unwrap_err().to_string(), "backend task panicked");
    }

    #[tokio::test]
    async fn dropping_the_handle_releases_unstarted_and_active_work() {
        for start in [false, true] {
            let started = Rc::new(Cell::new(false));
            let owned = started.clone();
            let (task, runner) = owned_task(async move {
                owned.set(true);
                pending::<()>().await;
                drop(owned);
            });
            let mut runner = Box::pin(runner);
            if start {
                assert!(futures_util::poll!(&mut runner).is_pending());
            }
            drop(task);
            runner.await;
            assert_eq!(started.get(), start);
            assert_eq!(Rc::strong_count(&started), 1);
        }
    }
}
