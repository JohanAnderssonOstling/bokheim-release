//! Bounded queues own their payloads; browser adapters supply ports and replies.
use std::collections::{HashMap, VecDeque};

struct LibraryJobs<T> {
    active: Vec<T>,
    pending: Vec<T>,
}
pub struct SyncJobs<T> {
    libraries: HashMap<String, LibraryJobs<T>>,
    count: usize,
    limit: usize,
    failure: Option<String>,
}
impl<T> SyncJobs<T> {
    pub fn new(limit: usize) -> Self {
        Self { libraries: HashMap::new(), count: 0, limit, failure: None }
    }
    pub fn request(&mut self, key: String, request: T) -> Result<bool, (String, T)> {
        if let Some(error) = &self.failure {
            return Err((error.clone(), request));
        }
        if self.count >= self.limit {
            return Err(("Sync queue is full".into(), request));
        }
        self.count += 1;
        let jobs = self.libraries.entry(key).or_insert_with(|| LibraryJobs { active: Vec::new(), pending: Vec::new() });
        if jobs.active.is_empty() {
            jobs.active.push(request);
            Ok(true)
        } else {
            jobs.pending.push(request);
            Ok(false)
        }
    }
    pub fn active_mut(&mut self, key: &str) -> Option<&mut T> {
        self.libraries.get_mut(key)?.active.last_mut()
    }
    pub fn complete(&mut self, key: &str) -> Vec<T> {
        let Some(mut jobs) = self.libraries.remove(key) else {
            return Vec::new();
        };
        self.count -= jobs.active.len();
        let completed = jobs.active;
        if !jobs.pending.is_empty() {
            jobs.active = jobs.pending;
            jobs.pending = Vec::new();
            self.libraries.insert(key.to_owned(), jobs);
        }
        completed
    }
    pub fn fail(&mut self, error: String) -> Vec<T> {
        self.failure = Some(error);
        self.count = 0;
        self.libraries.drain().flat_map(|(_, jobs)| jobs.active.into_iter().chain(jobs.pending)).collect()
    }
    pub fn count(&self) -> usize {
        self.count
    }
}

pub struct Transfer<T> {
    pub owner: (u32, T),
    pub owner_subscribed: bool,
    pub subscribers: HashMap<u32, T>,
    pub latest_progress: Option<f64>,
}
pub struct TransferJobs<T> {
    jobs: HashMap<String, Transfer<T>>,
    count: usize,
    limit: usize,
    failure: Option<String>,
}
impl<T> TransferJobs<T> {
    pub fn new(limit: usize) -> Self {
        Self { jobs: HashMap::new(), count: 0, limit, failure: None }
    }
    pub fn request(&mut self, key: String, id: u32, payload: T) -> Result<bool, (String, T)> {
        if let Some(error) = &self.failure {
            return Err((error.clone(), payload));
        }
        if self.count >= self.limit || (!self.jobs.contains_key(&key) && self.jobs.len() >= self.limit) {
            return Err(("Transfer queue is full; retry the job".into(), payload));
        }
        self.count += 1;
        if let Some(job) = self.jobs.get_mut(&key) {
            job.subscribers.insert(id, payload);
            Ok(false)
        } else {
            self.jobs.insert(key, Transfer { owner: (id, payload), owner_subscribed: true, subscribers: HashMap::new(), latest_progress: None });
            Ok(true)
        }
    }
    /// Return cancelled subscriber payloads for disposal outside the queue borrow.
    /// The executor stays alive even when its own subscription is cancelled.
    pub fn cancel(&mut self, key: &str, id: u32) -> Option<T> {
        let job = self.jobs.get_mut(key)?;
        if id == job.owner.0 {
            if job.owner_subscribed {
                job.owner_subscribed = false;
                self.count -= 1;
            }
            None
        } else {
            let subscriber = job.subscribers.remove(&id)?;
            self.count -= 1;
            Some(subscriber)
        }
    }
    pub fn take_finished(&mut self, key: &str, id: u32, connection_failed: bool) -> Option<Transfer<T>> {
        let job = self.jobs.get(key)?;
        if id != job.owner.0 && !(connection_failed && job.subscribers.contains_key(&id)) {
            return None;
        }
        let job = self.jobs.remove(key).unwrap();
        self.count -= job.subscribers.len() + usize::from(job.owner_subscribed);
        Some(job)
    }
    pub fn latest_progress(&self, key: &str) -> Option<f64> {
        self.jobs.get(key).and_then(|job| job.latest_progress)
    }
    pub fn report_progress(&mut self, key: &str, id: u32, progress: f64, mut send: impl FnMut(&T)) -> bool {
        let Some(job) = self.jobs.get_mut(key) else { return false };
        if job.owner.0 != id || !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
            return false;
        }
        job.latest_progress = Some(progress);
        if job.owner_subscribed {
            send(&job.owner.1);
        }
        for subscriber in job.subscribers.values() {
            send(subscriber);
        }
        true
    }
    pub fn drain(&mut self, error: String) -> Vec<Transfer<T>> {
        self.failure = Some(error);
        self.count = 0;
        self.jobs.drain().map(|(_, job)| job).collect()
    }
    pub fn count(&self) -> usize {
        self.count
    }
    pub fn job_count(&self) -> usize {
        self.jobs.len()
    }
}

pub struct CpuJobs<T> {
    pending: VecDeque<(u32, T)>,
    active: Option<(u32, T)>,
    limit: usize,
    ready: bool,
}
impl<T> CpuJobs<T> {
    pub fn new(limit: usize) -> Self {
        Self { pending: VecDeque::new(), active: None, limit, ready: false }
    }
    pub fn request(&mut self, id: u32, job: T) -> Result<(), (String, T)> {
        if self.pending.len() + usize::from(self.active.is_some()) >= self.limit {
            return Err(("CPU queue is full; retry the job".into(), job));
        }
        self.pending.push_back((id, job));
        Ok(())
    }
    /// Queued work can be removed. Active work retains its slot until completion.
    pub fn cancel_pending(&mut self, id: u32) -> Option<T> {
        let index = self.pending.iter().position(|(pending, _)| *pending == id)?;
        self.pending.remove(index).map(|(_, job)| job)
    }
    pub fn ready(&mut self) {
        self.ready = true;
    }
    pub fn next(&mut self) -> Option<(u32, &mut T)> {
        if !self.ready || self.active.is_some() {
            return None;
        }
        self.active = self.pending.pop_front();
        self.active.as_mut().map(|(id, job)| (*id, job))
    }
    pub fn complete(&mut self, id: u32) -> Result<T, String> {
        if self.active.as_ref().map(|(active, _)| *active) != Some(id) {
            return Err("Unexpected CPU worker response".into());
        }
        Ok(self.active.take().unwrap().1)
    }
    pub fn fail(&mut self) -> Vec<T> {
        self.ready = false;
        self.active.take().into_iter().chain(self.pending.drain(..)).map(|(_, job)| job).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sync_coalesces_followups_with_the_latest_payload_and_returns_every_reply() {
        let mut jobs = SyncJobs::new(4);
        assert_eq!(jobs.request("a".into(), 1), Ok(true));
        assert_eq!(jobs.request("a".into(), 2), Ok(false));
        assert_eq!(jobs.request("a".into(), 3), Ok(false));
        assert_eq!(jobs.request("b".into(), 4), Ok(true));
        assert_eq!(jobs.request("c".into(), 5).unwrap_err().1, 5);
        assert_eq!(jobs.complete("a"), vec![1]);
        assert_eq!(jobs.active_mut("a").copied(), Some(3));
        assert_eq!(jobs.complete("a"), vec![2, 3]);
        assert_eq!(jobs.active_mut("a").copied(), None);
        assert_eq!(jobs.count(), 1);
        assert_eq!(jobs.fail("stopped".into()), vec![4]);
        assert_eq!(jobs.count(), 0);
        assert_eq!(jobs.request("a".into(), 6), Err(("stopped".into(), 6)));
        assert!(jobs.complete("b").is_empty());
    }
    #[test]
    fn transfer_retains_cancelled_executor_and_ignores_stale_completions() {
        let mut jobs = TransferJobs::new(2);
        assert_eq!(jobs.request("a".into(), 1, "owner"), Ok(true));
        assert_eq!(jobs.request("a".into(), 2, "subscriber"), Ok(false));
        assert!(jobs.request("b".into(), 3, "overflow").is_err());
        assert_eq!(jobs.cancel("a", 1), None);
        assert_eq!(jobs.count(), 1);
        assert_eq!(jobs.cancel("a", 1), None);
        let job = jobs.take_finished("a", 1, false).unwrap();
        assert_eq!(job.owner.1, "owner");
        assert!(!job.owner_subscribed);
        assert_eq!(job.subscribers[&2], "subscriber");
        assert_eq!(jobs.count(), 0);
        assert_eq!(jobs.request("a".into(), 4, "replacement"), Ok(true));
        assert!(jobs.take_finished("a", 1, true).is_none());
        assert_eq!(jobs.job_count(), 1);
        assert_eq!(jobs.drain("shutdown".into()).len(), 1);
        assert_eq!(jobs.job_count(), 0);
        assert_eq!(jobs.request("a".into(), 5, "late"), Err(("shutdown".into(), "late")));
    }
    #[test]
    fn cancelled_transfer_subscriber_is_released_without_stopping_the_owner() {
        let mut jobs = TransferJobs::new(2);
        jobs.request("a".into(), 1, "owner").unwrap();
        jobs.request("a".into(), 2, "subscriber").unwrap();
        assert_eq!(jobs.cancel("a", 2), Some("subscriber"));
        assert_eq!(jobs.count(), 1);
        assert!(jobs.take_finished("a", 2, true).is_none());
        assert!(jobs.take_finished("a", 1, false).is_some());
    }
    #[test]
    fn transfer_progress_is_shared_with_followers_and_late_joiners() {
        let mut jobs = TransferJobs::new(3);
        jobs.request("book".into(), 1, "owner").unwrap();
        jobs.request("book".into(), 2, "follower").unwrap();
        let mut delivered = Vec::new();
        assert!(!jobs.report_progress("book", 2, 0.5, |port| delivered.push(*port)));
        assert!(delivered.is_empty());
        assert!(jobs.report_progress("book", 1, 0.5, |port| delivered.push(*port)));
        delivered.sort_unstable();
        assert_eq!(delivered, ["follower", "owner"]);
        assert_eq!(jobs.latest_progress("book"), Some(0.5));
        jobs.request("book".into(), 3, "late").unwrap();
        assert_eq!(jobs.latest_progress("book"), Some(0.5));
        jobs.take_finished("book", 1, false).unwrap();
        assert_eq!(jobs.latest_progress("book"), None);
    }
    #[test]
    fn cancelled_executors_still_count_toward_the_job_limit() {
        let mut jobs = TransferJobs::new(1);
        jobs.request("a".into(), 1, "owner").unwrap();
        jobs.cancel("a", 1);
        assert_eq!(jobs.count(), 0);
        assert!(jobs.request("b".into(), 2, "other").is_err());
        assert_eq!(jobs.request("a".into(), 3, "join"), Ok(false));
        assert!(jobs.take_finished("a", 3, true).is_some());
    }
    #[test]
    fn cpu_failure_returns_owned_jobs_and_allows_restart() {
        let mut jobs = CpuJobs::new(2);
        assert_eq!(jobs.request(1, "first"), Ok(()));
        assert_eq!(jobs.request(2, "second"), Ok(()));
        assert!(jobs.next().is_none());
        jobs.ready();
        assert_eq!(jobs.next().map(|(id, value)| (id, *value)), Some((1, "first")));
        assert!(jobs.next().is_none());
        assert!(jobs.request(3, "overflow").is_err());
        assert!(jobs.complete(99).is_err());
        assert_eq!(jobs.fail(), vec!["first", "second"]);
        assert_eq!(jobs.request(4, "restart"), Ok(()));
        jobs.ready();
        assert_eq!(jobs.next().map(|(id, value)| (id, *value)), Some((4, "restart")));
        assert_eq!(jobs.complete(4), Ok("restart"));
    }
}

#[cfg(test)]
mod cpu_cancellation_tests {
    use super::*;
    #[test]
    fn cancelling_queued_job_releases_capacity_and_preserves_order() {
        let mut jobs = CpuJobs::new(2);
        jobs.request(1, "cancelled").unwrap();
        jobs.request(2, "next").unwrap();
        assert_eq!(jobs.cancel_pending(1), Some("cancelled"));
        jobs.request(3, "last").unwrap();
        jobs.ready();
        assert_eq!(jobs.next().map(|(id, _)| id), Some(2));
        assert_eq!(jobs.complete(2), Ok("next"));
        assert_eq!(jobs.next().map(|(id, _)| id), Some(3));
    }
    #[test]
    fn abandoned_active_job_finishes_before_its_successor() {
        let mut jobs = CpuJobs::new(2);
        let (reply, response) = tokio::sync::oneshot::channel::<()>();
        jobs.request(1, reply).unwrap();
        let (next_reply, _next_response) = tokio::sync::oneshot::channel();
        jobs.request(2, next_reply).unwrap();
        jobs.ready();
        assert_eq!(jobs.next().map(|(id, _)| id), Some(1));
        drop(response);
        assert!(jobs.cancel_pending(1).is_none());
        assert!(jobs.next().is_none());
        assert!(jobs.complete(1).unwrap().send(()).is_err());
        assert!(jobs.cancel_pending(1).is_none());
        assert_eq!(jobs.next().map(|(id, _)| id), Some(2));
    }
}
