//! Admission accounting follows owned requests through dispatch and cancellation.
use std::{cell::Cell, rc::Rc};
#[derive(Clone)]
pub struct ByteBudget {
    used: Rc<Cell<usize>>,
    limit: usize,
}
pub struct Reservation {
    used: Rc<Cell<usize>>,
    bytes: usize,
}
impl ByteBudget {
    pub fn new(limit: usize) -> Self {
        Self { used: Rc::new(Cell::new(0)), limit }
    }
    pub fn reserve(&self, bytes: usize) -> Option<Reservation> {
        let next = self.used.get().checked_add(bytes)?;
        if next > self.limit {
            return None;
        }
        self.used.set(next);
        Some(Reservation { used: self.used.clone(), bytes })
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
    pub fn used(&self) -> usize {
        self.used.get()
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.used.set(self.used.get() - self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        coordinator::{CpuJobs, SyncJobs, TransferJobs},
        worker_lifecycle::Connection,
    };
    #[test]
    fn budget_rejects_oversize_and_overflow_without_changing_usage() {
        let budget = ByteBudget::new(10);
        let first = budget.reserve(8).unwrap();
        assert!(budget.reserve(3).is_none());
        assert!(budget.reserve(usize::MAX).is_none());
        assert_eq!(budget.used(), 8);
        drop(first);
        assert_eq!(budget.used(), 0);
        assert!(budget.reserve(10).is_some());
    }
    #[test]
    fn rejected_job_and_dropped_queue_return_all_capacity() {
        let budget = ByteBudget::new(10);
        let mut jobs = CpuJobs::new(1);
        assert!(jobs.request(1, budget.reserve(4).unwrap()).is_ok());
        let rejected = jobs.request(2, budget.reserve(6).unwrap());
        assert!(rejected.is_err());
        drop(rejected);
        assert_eq!(budget.used(), 4);
        drop(jobs);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn active_cpu_keeps_reservation_after_caller_cancellation() {
        let budget = ByteBudget::new(10);
        let mut jobs = CpuJobs::new(2);
        assert!(jobs.request(1, budget.reserve(7).unwrap()).is_ok());
        assert!(jobs.request(2, budget.reserve(3).unwrap()).is_ok());
        jobs.ready();
        jobs.next();
        assert!(jobs.cancel_pending(1).is_none());
        drop(jobs.cancel_pending(2));
        assert_eq!(budget.used(), 7);
        assert!(budget.reserve(4).is_none());
        drop(jobs.complete(1).unwrap());
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn repeated_startup_cancellation_failure_and_stale_reply_release_every_request() {
        let budget = ByteBudget::new(10);
        let mut jobs = CpuJobs::new(3);
        for cycle in 0..100 {
            let first = cycle * 3 + 1;
            let old = u64::from(first);
            let worker = Connection::new((), old, ());
            assert!(jobs.request(first, budget.reserve(4).unwrap()).is_ok());
            assert!(jobs.request(first + 1, budget.reserve(6).unwrap()).is_ok());
            drop(jobs.cancel_pending(first));
            assert_eq!(budget.used(), 6);
            drop(worker);
            drop(jobs.fail());
            assert_eq!(budget.used(), 0);
            let new = old + 1;
            let mut worker = Connection::new((), new, ());
            assert!(jobs.request(first + 2, budget.reserve(10).unwrap()).is_ok());
            worker.ready(old);
            assert!(!worker.accepts(old));
            assert!(worker.waiting(new));
            assert!(jobs.complete(first).is_err());
            worker.ready(new);
            jobs.ready();
            jobs.next();
            drop(jobs.complete(first + 2).unwrap());
            assert_eq!(budget.used(), 0);
        }
    }
    #[test]
    fn coalesced_sync_holds_every_input_until_its_pass_completes() {
        let budget = ByteBudget::new(12);
        let mut jobs = SyncJobs::new(3);
        assert!(jobs.request("a".into(), budget.reserve(3).unwrap()).is_ok());
        assert!(jobs.request("a".into(), budget.reserve(4).unwrap()).is_ok());
        assert!(jobs.request("a".into(), budget.reserve(5).unwrap()).is_ok());
        drop(jobs.complete("a"));
        assert_eq!(budget.used(), 9);
        drop(jobs.fail("host replaced".into()));
        assert_eq!(budget.used(), 0);
        assert!(jobs.complete("a").is_empty());
        let rejected = jobs.request("a".into(), budget.reserve(1).unwrap());
        assert!(rejected.is_err());
        drop(rejected);
        assert_eq!(budget.used(), 0);
    }
    #[test]
    fn transfer_failure_releases_owner_and_subscriber_once() {
        let resources = Rc::new(());
        let mut jobs = TransferJobs::new(3);
        for id in 1..=3 {
            jobs.request("book".into(), id, resources.clone()).unwrap();
        }
        jobs.cancel("book", 1);
        drop(jobs.cancel("book", 2));
        assert_eq!(Rc::strong_count(&resources), 3);
        drop(jobs.take_finished("book", 3, true));
        assert_eq!(Rc::strong_count(&resources), 1);
        assert!(jobs.take_finished("book", 1, false).is_none());
        drop(jobs.drain("host lost".into()));
        assert_eq!(Rc::strong_count(&resources), 1);
    }
}
