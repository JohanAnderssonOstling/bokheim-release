//! Correlation and disconnect draining shared by worker transports.
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

pub struct Pending<T> {
    next: Cell<u64>,
    entries: RefCell<HashMap<u64, T>>,
}
impl<T> Default for Pending<T> {
    fn default() -> Self {
        Self { next: Cell::new(0), entries: RefCell::new(HashMap::new()) }
    }
}
impl<T> Pending<T> {
    pub fn next_id(&self) -> u64 {
        let id = self.next.get().checked_add(1).expect("browser request IDs exhausted");
        self.next.set(id);
        id
    }
    pub fn insert(&self, id: u64, value: T) {
        self.entries.borrow_mut().insert(id, value);
    }
    pub fn take(&self, id: u64) -> Option<T> {
        self.entries.borrow_mut().remove(&id)
    }
    /// Release the map borrow before callbacks: settlement may remove itself or
    /// submit another request. New requests belong to the next connection.
    pub fn fail_all(&self, mut fail: impl FnMut(T)) {
        let entries = std::mem::take(&mut *self.entries.borrow_mut());
        for entry in entries.into_values() {
            fail(entry);
        }
    }
    #[cfg(any(test, feature = "web-runtime-tests"))]
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
    #[cfg(any(test, feature = "web-runtime-tests"))]
    pub fn last_id(&self) -> u64 {
        self.next.get()
    }
}

/// Removing a request on future drop prevents cancelled calls retaining replies.
pub struct Registration<'a, T> {
    pending: &'a Pending<T>,
    id: u64,
}
impl<T> Pending<T> {
    pub fn track(&self, id: u64, value: T) -> Registration<'_, T> {
        self.insert(id, value);
        Registration { pending: self, id }
    }
}
impl<T> Drop for Registration<'_, T> {
    fn drop(&mut self) {
        self.pending.take(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_releases_request_and_ignores_late_completion() {
        let pending = Pending::default();
        let id = pending.next_id();
        let owned = std::rc::Rc::new(());
        let registration = pending.track(id, owned.clone());
        drop(registration);
        assert_eq!(std::rc::Rc::strong_count(&owned), 1);
        assert!(pending.take(id).is_none());
    }
    #[test]
    fn completion_is_consumed_once() {
        let pending = Pending::default();
        let id = pending.next_id();
        let registration = pending.track(id, 7);
        assert_eq!(pending.take(id), Some(7));
        assert_eq!(pending.take(id), None);
        drop(registration);
        assert!(pending.is_empty());
    }
    #[test]
    fn disconnect_drains_old_requests_before_reentrant_submission() {
        let pending = Pending::default();
        pending.insert(pending.next_id(), 1);
        pending.insert(pending.next_id(), 2);
        let mut failed = Vec::new();
        pending.fail_all(|value| {
            failed.push(value);
            pending.insert(pending.next_id(), 3);
        });
        failed.sort();
        assert_eq!(failed, vec![1, 2]);
        let mut remaining = Vec::new();
        pending.fail_all(|value| remaining.push(value));
        assert_eq!(remaining, vec![3, 3]);
        assert!(pending.is_empty());
    }
}

/// Capture before constructing an async block so even an unpolled future cleans up.
pub struct OnDrop<F: FnOnce()>(Option<F>);
impl<F: FnOnce()> OnDrop<F> {
    pub fn new(cleanup: F) -> Self {
        Self(Some(cleanup))
    }
}
impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup();
        }
    }
}

/// Cancellation/completion of an older command must not remove its successor.
pub fn take_matching<T>(slot: &RefCell<Option<(u32, T)>>, id: u32) -> Option<T> {
    let mut pending = slot.borrow_mut();
    if pending.as_ref().map(|(active, _)| *active) == Some(id) {
        pending.take().map(|(_, value)| value)
    } else {
        None
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::{
        future::Future,
        rc::Rc,
        task::{Context, Poll},
    };

    #[test]
    fn dropping_unpolled_or_running_future_releases_its_slot() {
        for poll_first in [false, true] {
            let slot = Rc::new(RefCell::new(Some((1, Rc::new(())))));
            let resource = slot.borrow().as_ref().unwrap().1.clone();
            let pending = slot.clone();
            let cleanup = OnDrop::new(move || {
                drop(take_matching(&pending, 1));
            });
            let mut future = Box::pin(async move {
                let _cleanup = cleanup;
                std::future::pending::<()>().await;
            });
            if poll_first {
                let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
                assert_eq!(future.as_mut().poll(&mut context), Poll::Pending);
            }
            drop(future);
            assert!(slot.borrow().is_none());
            assert_eq!(Rc::strong_count(&resource), 1);
        }
    }

    #[test]
    fn late_reply_and_old_cleanup_preserve_the_next_command() {
        let slot = RefCell::new(Some((1, "first")));
        let cleanup = OnDrop::new(|| {
            take_matching(&slot, 1);
        });
        assert_eq!(take_matching(&slot, 1), Some("first"));
        *slot.borrow_mut() = Some((2, "second"));
        assert_eq!(take_matching(&slot, 1), None);
        drop(cleanup);
        assert_eq!(take_matching(&slot, 2), Some("second"));
    }

    #[test]
    fn completed_future_cleanup_does_not_touch_a_new_request() {
        let slot = Rc::new(RefCell::new(Some((1, "first"))));
        let pending = slot.clone();
        let cleanup = OnDrop::new(move || {
            take_matching(&pending, 1);
        });
        let mut future = Box::pin(async move {
            let _cleanup = cleanup;
        });
        assert_eq!(take_matching(&slot, 1), Some("first"));
        *slot.borrow_mut() = Some((2, "second"));
        let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
        assert_eq!(future.as_mut().poll(&mut context), Poll::Ready(()));
        drop(future);
        assert_eq!(take_matching(&slot, 2), Some("second"));
    }
}

/// Cancellation wins ties and drops the operation before returning to its owner.
pub async fn until_cancelled<T>(operation: impl std::future::Future<Output = T>, cancelled: impl std::future::Future) -> Option<T> {
    futures_util::pin_mut!(operation, cancelled);
    match futures_util::future::select(cancelled, operation).await {
        futures_util::future::Either::Left(_) => None,
        futures_util::future::Either::Right((result, _)) => Some(result),
    }
}

#[cfg(test)]
mod remote_cancellation_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    #[test]
    fn cancellation_and_disconnect_drop_unstarted_operation() {
        for disconnect in [false, true] {
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            if disconnect {
                drop(cancel);
            } else {
                cancel.send(()).unwrap();
            }
            let dropped = Rc::new(Cell::new(false));
            let observed = dropped.clone();
            let cleanup = OnDrop::new(move || observed.set(true));
            let operation = async move {
                let _cleanup = cleanup;
                panic!("cancelled work must not start");
            };
            assert_eq!(futures_executor::block_on(until_cancelled(operation, cancelled)), None::<()>);
            assert!(dropped.get());
        }
    }
    #[test]
    fn completed_operation_returns_reply() {
        assert_eq!(futures_executor::block_on(until_cancelled(async { 42 }, std::future::pending::<()>())), Some(42));
    }
}

#[cfg(test)]
mod deadline_cleanup_tests {
    use super::*;
    use std::{
        future::Future,
        rc::Rc,
        task::{Context, Poll},
    };
    #[test]
    fn readiness_or_failure_releases_a_running_startup_timer() {
        let resource = Rc::new(());
        let captured = resource.clone();
        let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
        let timer = async move {
            let _resource = captured;
            std::future::pending::<()>().await;
        };
        let mut deadline = Box::pin(until_cancelled(timer, cancelled));
        let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
        assert_eq!(deadline.as_mut().poll(&mut context), Poll::Pending);
        drop(cancel);
        assert_eq!(deadline.as_mut().poll(&mut context), Poll::Ready(None));
        assert_eq!(Rc::strong_count(&resource), 1);
    }
}
