//! Owned request work. Admission never waits in the command loop.
//!
//! Cancellation policy belongs to the workflow: accepted writes run
//! until completion or library retirement; disposable reads stop with their
//! caller. Directory imports finish the current file and inspect caller
//! cancellation between files. No database transaction may span an await.
use crate::BackendError;
use futures_util::{future::LocalBoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use std::{future::Future, rc::Rc};

#[derive(Clone, Copy)]
pub(super) enum OperationKind {
    Read,
    Import,
    Maintenance,
}

/// A reply is resolved even if admission fails, a future panics, or its owner
/// is dropped before the future is first polled.
pub(super) struct PendingReply<T>(Option<async_channel::Sender<Result<T, BackendError>>>);

impl<T> PendingReply<T> {
    pub fn new(sender: async_channel::Sender<Result<T, BackendError>>) -> Self {
        Self(Some(sender))
    }
    #[cfg(target_arch = "wasm32")]
    pub fn is_closed(&self) -> bool {
        self.0.as_ref().is_none_or(|sender| sender.is_closed())
    }
    pub fn sender(&self) -> async_channel::Sender<Result<T, BackendError>> {
        self.0.as_ref().expect("pending reply").clone()
    }
    pub fn finish(mut self, result: Result<T, BackendError>) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(result);
        }
    }
}

impl<T> Drop for PendingReply<T> {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(Err(BackendError::message("library operation stopped before completing")));
        }
    }
}

pub(super) struct PendingOperations<C> {
    tasks: FuturesUnordered<LocalBoxFuture<'static, C>>,
    reads: Rc<tokio::sync::Semaphore>,
    imports: Rc<tokio::sync::Semaphore>,
    maintenance: Rc<tokio::sync::Semaphore>,
}

impl<C: 'static> Default for PendingOperations<C> {
    fn default() -> Self {
        Self { tasks: FuturesUnordered::new(), reads: Rc::new(tokio::sync::Semaphore::new(8)), imports: Rc::new(tokio::sync::Semaphore::new(1)), maintenance: Rc::new(tokio::sync::Semaphore::new(2)) }
    }
}

impl<C: 'static> PendingOperations<C> {
    const MAX_PENDING: usize = 128;

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    pub async fn next(&mut self) -> Option<C> {
        self.tasks.next().await
    }

    pub fn submit<R: 'static, T: 'static>(
        &mut self, kind: OperationKind, reply: PendingReply<R>, operation: impl Future<Output = Result<T, BackendError>> + 'static, complete: impl FnOnce(PendingReply<R>, Result<T, BackendError>) -> C + 'static,
    ) -> bool {
        if self.tasks.len() >= Self::MAX_PENDING {
            reply.finish(Err(BackendError::message("too many pending library operations; retry later")));
            return false;
        }
        let permits = match kind {
            OperationKind::Read => self.reads.clone(),
            OperationKind::Import => self.imports.clone(),
            OperationKind::Maintenance => self.maintenance.clone(),
        };
        let abandoned = reply.sender();
        self.tasks.push(Box::pin(async move {
            let run = async move {
                let _permit = permits.acquire().await.expect("operation permits are never closed");
                operation.await
            };
            let run = std::panic::AssertUnwindSafe(run).catch_unwind();
            let result = if matches!(kind, OperationKind::Read) {
                tokio::select! {
                    biased;
                    _ = abandoned.closed() => Err(BackendError::message("library request cancelled")),
                    result = run => result.unwrap_or_else(|_| Err(BackendError::message("library operation panicked"))),
                }
            } else {
                run.await.unwrap_or_else(|_| Err(BackendError::message("library operation panicked")))
            };
            complete(reply, result)
        }));
        true
    }
}

/// One background lane: repeated wakes coalesce without replacing active work.
pub(super) struct BackgroundJob<T> {
    pub task: Option<LocalBoxFuture<'static, T>>,
    pub pending: bool,
}
impl<T> Default for BackgroundJob<T> {
    fn default() -> Self {
        Self { task: None, pending: false }
    }
}
impl<T> BackgroundJob<T> {
    pub fn begin(&mut self) -> bool {
        if self.task.is_some() {
            self.pending = true;
            false
        } else {
            true
        }
    }
    pub fn finish(&mut self) -> bool {
        self.task = None;
        std::mem::take(&mut self.pending)
    }
    pub async fn next(&mut self) -> T {
        match self.task.as_mut() {
            Some(task) => task.await,
            None => std::future::pending().await,
        }
    }
}
impl BackgroundJob<()> {
    pub fn retry_after(&mut self, seconds: u64) {
        if self.task.is_none() {
            self.task = Some(Box::pin(crate::executor::sleep(std::time::Duration::from_secs(seconds))));
        }
    }
}

/// Channel closure alone retains queued messages while a sender survives.
/// Drain on every exit path, including panic and cancellation.
pub(super) struct Inbox<T>(pub async_channel::Receiver<T>);
impl<T> Drop for Inbox<T> {
    fn drop(&mut self) {
        self.0.close();
        while self.0.try_recv().is_ok() {}
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn reply() -> (PendingReply<()>, async_channel::Receiver<Result<(), BackendError>>) {
        let (sender, receiver) = async_channel::bounded(1);
        (PendingReply::new(sender), receiver)
    }
    fn finish(reply: PendingReply<()>, result: Result<(), BackendError>) {
        reply.finish(result);
    }

    #[tokio::test]
    async fn caller_cancellation_stops_reads_but_not_accepted_writes() {
        for kind in [OperationKind::Read, OperationKind::Import] {
            let mut operations = PendingOperations::<()>::default();
            let (reply, receiver) = reply();
            let (release, resume) = async_channel::bounded::<()>(1);
            let ran = Rc::new(Cell::new(false));
            let completed = ran.clone();
            operations.submit(
                kind,
                reply,
                async move {
                    resume.recv().await.unwrap();
                    completed.set(true);
                    Ok(())
                },
                finish,
            );
            assert!(operations.next().now_or_never().is_none());
            drop(receiver);
            if matches!(kind, OperationKind::Read) {
                operations.next().await;
                assert!(!ran.get());
            } else {
                assert!(operations.next().now_or_never().is_none());
                release.send(()).await.unwrap();
                operations.next().await;
                assert!(ran.get());
            }
        }
    }

    #[tokio::test]
    async fn ownership_resolves_unpolled_and_active_replies_and_contains_panics() {
        let mut operations = PendingOperations::<()>::default();
        let (pending, receiver) = reply();
        operations.submit(OperationKind::Read, pending, std::future::pending(), finish);
        assert!(operations.next().now_or_never().is_none());
        let (unpolled, queued) = reply();
        operations.submit(OperationKind::Read, unpolled, std::future::pending(), finish);
        drop(operations);
        assert!(receiver.recv().await.unwrap().is_err());
        assert!(queued.recv().await.unwrap().is_err());

        let mut operations = PendingOperations::<()>::default();
        let (pending, receiver) = reply();
        operations.submit(OperationKind::Read, pending, async { panic!("broken operation") }, finish);
        operations.next().await;
        assert!(receiver.recv().await.unwrap().unwrap_err().to_string().contains("panicked"));
        let (pending, receiver) = reply();
        operations.submit(OperationKind::Read, pending, async { Ok(()) }, finish);
        operations.next().await;
        assert!(receiver.recv().await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn concurrency_and_admission_are_bounded_without_blocking_other_classes() {
        let mut operations = PendingOperations::<()>::default();
        let started = Rc::new(Cell::new(0));
        let mut receivers = Vec::new();
        for _ in 0..PendingOperations::<()>::MAX_PENDING {
            let (pending, receiver) = reply();
            receivers.push(receiver);
            let count = started.clone();
            operations.submit(
                OperationKind::Read,
                pending,
                async move {
                    count.set(count.get() + 1);
                    std::future::pending::<Result<(), BackendError>>().await
                },
                finish,
            );
        }
        assert!(operations.next().now_or_never().is_none());
        assert_eq!(started.get(), 8);
        let (pending, receiver) = reply();
        operations.submit(OperationKind::Read, pending, async { Ok(()) }, finish);
        assert!(receiver.recv().await.unwrap().unwrap_err().to_string().contains("too many"));
        drop(operations);
        for receiver in receivers {
            assert!(receiver.recv().await.unwrap().is_err());
        }

        let mut operations = PendingOperations::<()>::default();
        let (pending, import) = reply();
        operations.submit(OperationKind::Import, pending, std::future::pending(), finish);
        let (pending, read) = reply();
        operations.submit(OperationKind::Read, pending, async { Ok(()) }, finish);
        operations.next().await;
        assert!(read.recv().await.unwrap().is_ok());
        assert!(import.try_recv().is_err());
    }

    #[test]
    fn inbox_unwind_releases_queued_replies_even_with_a_retained_sender() {
        let (commands, receiver) = async_channel::unbounded();
        let (pending, response) = reply();
        commands.try_send(pending).unwrap_or_else(|_| panic!("inbox open"));
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inbox = Inbox(receiver);
            panic!("actor failed");
        }));
        assert!(failed.is_err());
        assert!(commands.is_closed());
        assert!(response.try_recv().unwrap().is_err());
    }
}
