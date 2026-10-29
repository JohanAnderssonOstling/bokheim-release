//! Correlation and disconnect draining shared by worker transports.

use std::future::Future;
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
    pub fn next_id(&self) -> Result<u64, String> {
        let id = self.next.get().checked_add(1).ok_or_else(|| "browser request IDs exhausted".to_owned())?;
        self.next.set(id);
        Ok(id)
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

/// Cancellation wins ties and drops the operation before returning to its owner.
pub async fn until_cancelled<T>(operation: impl Future<Output = T>, cancelled: impl Future) -> Option<T> {
    futures_util::pin_mut!(operation, cancelled);
    match futures_util::future::select(cancelled, operation).await {
        futures_util::future::Either::Left(_) => None,
        futures_util::future::Either::Right((result, _)) => Some(result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_ids_leave_existing_requests_available() {
        let pending = Pending::default();
        pending.insert(7, "existing request");
        pending.next.set(u64::MAX);
        assert!(pending.next_id().is_err());
        assert_eq!(pending.take(7), Some("existing request"));
        assert_eq!(pending.next.get(), u64::MAX);
    }
}
