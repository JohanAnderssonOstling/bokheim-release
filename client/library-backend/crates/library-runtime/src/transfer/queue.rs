use super::history::HISTORY_LIMIT;
use super::{MessagePackTransferHistory, TransferJob};
use library_replica::TransferJobKind;
use library_replica::{TransferOrigin, TransferState, TransferStatus};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sync_common::api::assets::MAX_THUMBNAIL_BATCH_ENTRIES;
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueJobState {
    Pending,
    Running,
}

#[derive(Clone, Debug)]
struct QueueJob {
    job: TransferJob,
    generation: u64,
    state: QueueJobState,
    attempts: u32,
    next_attempt_at: u64,
    last_error: Option<String>,
    updated_at: u64,
}

#[derive(Clone, Debug)]
pub struct ClaimedTransferJob {
    pub id: u64,
    pub job: TransferJob,
}

#[derive(Debug)]
struct TransferBatch {
    jobs: BTreeMap<u64, QueueJob>,
    completed: u32,
    attempts: u32,
    created_at: u64,
    updated_at: u64,
    origin: TransferOrigin,
    permanent_error: Option<String>,
    failed_hash: Option<sync_common::ContentHash>,
    last_job: Option<TransferJob>,
}

impl TransferBatch {
    fn new(now: u64, origin: TransferOrigin) -> Self {
        Self { jobs: BTreeMap::new(), completed: 0, attempts: 0, created_at: now, updated_at: now, origin, permanent_error: None, failed_hash: None, last_job: None }
    }

    fn status(&self, kind: TransferJobKind) -> Option<TransferStatus> {
        let jobs = self.jobs.values();
        let first = jobs.clone().find(|job| job.last_error.is_some()).or_else(|| jobs.clone().next());
        let representative = first.map(|job| &job.job).or(self.last_job.as_ref())?;
        let (_, content_hash) = representative.status_fields();
        let state = if jobs.clone().any(|job| job.state == QueueJobState::Running) {
            TransferState::Running
        } else if jobs.clone().any(|job| job.last_error.is_some()) {
            TransferState::Retrying
        } else {
            TransferState::Queued
        };
        Some(TransferStatus {
            id: format!("batch-{}", kind.storage()),
            kind,
            content_hash: self.failed_hash.unwrap_or(*content_hash),
            state,
            origin: self.origin,
            attempts: self.attempts.max(jobs.clone().map(|job| job.attempts).max().unwrap_or(0)),
            completed_items: self.completed,
            total_items: self.completed.saturating_add(jobs.len() as u32),
            failed_content_hash: self.failed_hash.or_else(|| first.and_then(|first| first.last_error.as_ref()).map(|_| *content_hash)),
            file_name: None,
            last_error: self.permanent_error.clone().or_else(|| jobs.clone().find_map(|job| job.last_error.clone())),
            next_retry_at: jobs.clone().filter_map(|job| (job.next_attempt_at > 0).then_some(job.next_attempt_at)).min().map(|value| value.to_string()),
            created_at: self.created_at.to_string(),
            updated_at: self.updated_at.max(jobs.clone().map(|job| job.updated_at).max().unwrap_or(self.updated_at)).to_string(),
        })
    }
}

#[derive(Debug, Default)]
struct QueueState {
    batches: HashMap<TransferJobKind, TransferBatch>,
    history: VecDeque<TransferStatus>,
    next_id: u64,
    generation: u64,
    planning: bool,
    producers: HashMap<TransferJobKind, usize>,
    final_plans: HashMap<TransferJobKind, u64>,
}

#[derive(Default)]
pub struct TransferQueue {
    state: Mutex<QueueState>,
    history: Option<Box<dyn super::HistoryPersistence>>,
    history_error: Option<String>,
    scheduler_running: AtomicBool,
    authentication_paused: AtomicBool,
    wake_scheduler: Notify,
}

/// Keeps a batch open while local work can still produce transfers.
pub struct TransferProducer {
    queue: Arc<TransferQueue>,
    kind: TransferJobKind,
}

impl TransferProducer {
    /// A new planning pass must observe the producer's final durable writes.
    /// Cancelling a producer instead drops it without requiring this handoff.
    pub fn finish(self) {
        let mut state = self.queue.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let next_generation = state.generation + 1;
        state.final_plans.insert(self.kind, next_generation);
        // The lock is released before Drop removes the live producer count.
    }
}

impl Drop for TransferProducer {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = state.producers.get_mut(&self.kind) {
            *count -= 1;
            if *count == 0 {
                state.producers.remove(&self.kind);
            }
        }
        self.queue.finalize_empty_batches(&mut state);
    }
}

impl std::fmt::Debug for TransferQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TransferQueue").field("persistent_history", &self.history.is_some()).finish_non_exhaustive()
    }
}

impl TransferQueue {
    pub fn begin_producer(self: &Arc<Self>, kind: TransferJobKind) -> TransferProducer {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *state.producers.entry(kind).or_default() += 1;
        TransferProducer { queue: self.clone(), kind }
    }

    pub fn with_history(history: MessagePackTransferHistory) -> Self {
        let entries = history.load().unwrap_or_else(|error| {
            log::warn!("Could not load transfer history: {error}");
            VecDeque::new()
        });
        let (history, history_error) = match super::history::writer::HistoryWriter::new(history) {
            Ok(writer) => (Some(Box::new(writer) as Box<dyn super::HistoryPersistence>), None),
            Err(error) => {
                log::warn!("Could not start transfer history writer: {error}");
                (None, Some(error.to_string()))
            }
        };
        Self { state: Mutex::new(QueueState { history: entries, ..QueueState::default() }), history, history_error, ..Self::default() }
    }

    /// Restore completed history using a host-provided persistence mechanism.
    pub fn with_persistence(mut entries: VecDeque<TransferStatus>, persistence: impl super::HistoryPersistence + 'static) -> Self {
        entries.truncate(HISTORY_LIMIT);
        Self { state: Mutex::new(QueueState { history: entries, ..QueueState::default() }), history: Some(Box::new(persistence)), ..Self::default() }
    }

    pub fn reconcile(&self, desired: Vec<TransferJob>) {
        let generation = self.begin_reconcile();
        self.reconcile_page(generation, desired);
        self.finish_reconcile(generation);
    }

    /// A cancelled planner may have submitted useful pages. Only a completed
    /// pass removes jobs absent from its pages.
    pub fn begin_reconcile(&self) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation += 1;
        state.planning = true;
        state.generation
    }

    pub fn finish_reconcile(&self, generation: u64) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != generation {
            return;
        }
        let now = unix_seconds();
        state.batches.retain(|_, batch| {
            let previous_len = batch.jobs.len();
            batch.jobs.retain(|_, queued| queued.state == QueueJobState::Running || queued.generation == generation);
            if batch.jobs.len() != previous_len {
                batch.updated_at = now;
            }
            !batch.jobs.is_empty() || batch.completed > 0
        });
        state.planning = false;
        state.final_plans.retain(|_, required_generation| *required_generation > generation);
        self.finalize_empty_batches(&mut state);
        drop(state);
        self.wake_scheduler.notify_one();
    }

    /// Close a cancelled producer without discarding already submitted work.
    pub fn cancel_reconcile(&self, generation: u64) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation != generation || !state.planning {
            return;
        }
        state.planning = false;
        self.finalize_empty_batches(&mut state);
    }

    fn finalize_empty_batches(&self, state: &mut QueueState) {
        if state.planning {
            return;
        }
        let finished = state.batches.iter().filter(|(kind, batch)| batch.jobs.is_empty() && !state.producers.contains_key(kind) && !state.final_plans.contains_key(kind)).map(|(kind, _)| *kind).collect::<Vec<_>>();
        if finished.is_empty() {
            return;
        }
        for kind in finished {
            let batch = state.batches.remove(&kind).unwrap();
            if let Some(job) = batch.last_job.clone() {
                state.history.push_front(completed_status(kind, batch, &job));
            }
        }
        state.history.truncate(HISTORY_LIMIT);
        if let Some(history) = &self.history {
            history.submit(state.history.clone());
        }
    }

    pub fn reconcile_page(&self, generation: u64, desired: Vec<TransferJob>) {
        self.enqueue_page(Some(generation), desired);
    }

    /// Admit explicit requests into the active scheduler without replacing its plan.
    pub fn enqueue(&self, desired: Vec<TransferJob>) {
        self.enqueue_page(None, desired);
    }

    fn enqueue_page(&self, generation: Option<u64>, mut desired: Vec<TransferJob>) {
        desired.sort_by_key(TransferJob::priority);
        let now = unix_seconds();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.is_some_and(|generation| state.generation != generation) {
            return;
        }

        let generation = state.generation;
        let QueueState { batches, next_id, .. } = &mut *state;
        for candidate in desired {
            let kind = candidate.status_fields().0;
            let origin = candidate.origin();
            let batch = batches.entry(kind).or_insert_with(|| TransferBatch::new(now, origin));
            if origin == TransferOrigin::UserInitiated {
                batch.origin = TransferOrigin::UserInitiated;
            }
            batch.updated_at = now;
            if let Some(existing) = batch.jobs.values_mut().find(|queued| queued.job.operation == candidate.operation) {
                if origin == TransferOrigin::UserInitiated {
                    existing.job.origin = TransferOrigin::UserInitiated;
                }
                existing.updated_at = now;
                existing.generation = generation;
            } else {
                *next_id = next_id.saturating_add(1);
                batch.jobs.insert(*next_id, QueueJob { job: candidate, generation, state: QueueJobState::Pending, attempts: 0, next_attempt_at: 0, last_error: None, updated_at: now });
            }
        }
        drop(state);
        self.wake_scheduler.notify_one();
    }

    /// Hashes of currently due jobs, without claiming anything. The session
    /// snapshots transfer inputs for these before driving the queue.
    pub fn due_hashes(&self) -> Vec<sync_common::ContentHash> {
        let now = unix_seconds();
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut hashes = Vec::new();
        for queued in state.batches.values().flat_map(|batch| batch.jobs.values()).filter(|queued| queued.state == QueueJobState::Pending && queued.next_attempt_at <= now) {
            let hash = *queued.job.status_fields().1;
            if !hashes.contains(&hash) {
                hashes.push(hash);
            }
        }
        hashes
    }

    pub fn claim_due(&self) -> Option<ClaimedTransferJob> {
        let now = unix_seconds();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::claim_due_from(&mut state, now)
    }

    pub fn claim_due_batch(&self) -> Vec<ClaimedTransferJob> {
        let now = unix_seconds();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(first) = Self::claim_due_from(&mut state, now) else { return Vec::new() };
        let Some(kind) = first.job.batch_kind() else { return vec![first] };
        let Some(batch) = state.batches.get_mut(&kind) else { return vec![first] };
        let limit = if kind == TransferJobKind::UploadBook { 32 } else { MAX_THUMBNAIL_BATCH_ENTRIES };
        let mut claimed = vec![first];
        for (id, queued) in batch.jobs.iter_mut().filter(|(_, queued)| queued.state == QueueJobState::Pending && queued.next_attempt_at <= now).take(limit.saturating_sub(1)) {
            log::debug!(target: "sync_performance", "operation=transfer_claim job_id={} kind={:?} pending_seconds={} due_lag_seconds={} previous_failures={}", id, queued.job.batch_kind(), now.saturating_sub(queued.updated_at), now.saturating_sub(queued.next_attempt_at), queued.attempts);
            queued.state = QueueJobState::Running;
            queued.updated_at = now;
            claimed.push(ClaimedTransferJob { id: *id, job: queued.job.clone() });
        }
        claimed
    }

    fn claim_due_from(state: &mut QueueState, now: u64) -> Option<ClaimedTransferJob> {
        let (id, queued) =
            state.batches.values_mut().flat_map(|batch| batch.jobs.iter_mut()).filter(|(_, queued)| queued.state == QueueJobState::Pending && queued.next_attempt_at <= now).min_by_key(|(id, queued)| (queued.job.priority(), **id))?;
        log::debug!(target: "sync_performance", "operation=transfer_claim job_id={} kind={:?} pending_seconds={} due_lag_seconds={} previous_failures={}", id, queued.job.batch_kind(), now.saturating_sub(queued.updated_at), now.saturating_sub(queued.next_attempt_at), queued.attempts);
        queued.state = QueueJobState::Running;
        queued.updated_at = now;
        Some(ClaimedTransferJob { id: *id, job: queued.job.clone() })
    }

    pub fn release(&self, claimed: &[ClaimedTransferJob]) {
        let now = unix_seconds();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for item in claimed {
            if let Some(queued) = find_job_mut(&mut state, item.id) {
                queued.state = QueueJobState::Pending;
                queued.updated_at = now;
            }
        }
    }

    pub fn retry(&self, id: u64, message: String) -> Option<Duration> {
        let now = unix_seconds();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let queued = find_job_mut(&mut state, id)?;
        let backoff = (2_u64.pow(queued.attempts.min(8)) * 2).min(600);
        let delay = backoff + now % 3;
        queued.attempts = queued.attempts.saturating_add(1);
        queued.state = QueueJobState::Pending;
        queued.last_error = Some(message);
        queued.next_attempt_at = now.saturating_add(delay);
        queued.updated_at = now;
        Some(Duration::from_secs(delay))
    }

    pub fn complete(&self, id: u64, permanent_error: Option<String>) {
        let now = unix_seconds();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let planning = state.planning;
        let Some(kind) = state.batches.iter().find_map(|(kind, batch)| batch.jobs.contains_key(&id).then_some(*kind)) else { return };
        let producing = state.producers.contains_key(&kind) || state.final_plans.contains_key(&kind);
        let batch = state.batches.get_mut(&kind).expect("batch disappeared while completing a transfer");
        let Some(removed) = batch.jobs.remove(&id) else { return };
        batch.completed = batch.completed.saturating_add(1);
        batch.last_job = Some(removed.job.clone());
        batch.attempts = batch.attempts.max(removed.attempts);
        batch.updated_at = now;
        if let Some(error) = permanent_error {
            batch.failed_hash = Some(*removed.job.status_fields().1);
            batch.permanent_error = Some(error);
        }
        if planning || producing || !batch.jobs.is_empty() {
            return;
        }
        let batch = state.batches.remove(&kind).expect("completed batch disappeared while holding the queue lock");
        let status = completed_status(kind, batch, &removed.job);
        state.history.push_front(status);
        state.history.truncate(HISTORY_LIMIT);
        // Enqueue in completion order; serialization and disk writes run outside this lock.
        if let Some(history) = &self.history {
            history.submit(state.history.clone());
        }
    }

    pub fn next_due_in(&self) -> Option<Duration> {
        let now = unix_seconds();
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .batches
            .values()
            .flat_map(|batch| batch.jobs.values())
            .filter(|queued| queued.state == QueueJobState::Pending)
            .map(|queued| Duration::from_secs(queued.next_attempt_at.saturating_sub(now)))
            .min()
    }

    pub fn resume_authentication(&self) {
        self.authentication_paused.store(false, Ordering::Release);
        self.wake_scheduler.notify_one();
    }

    pub fn pause_authentication(&self) {
        self.authentication_paused.store(true, Ordering::Release);
        self.scheduler_running.store(false, Ordering::Release);
    }

    pub fn try_start_scheduler(&self) -> bool {
        !self.authentication_paused.load(Ordering::Acquire) && self.scheduler_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    pub fn keep_scheduler_or_stop(&self) -> bool {
        self.scheduler_running.store(false, Ordering::Release);
        if self.authentication_paused.load(Ordering::Acquire) || self.next_due_in().is_none() {
            return false;
        }
        self.scheduler_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    pub async fn wait_for_wake(&self, delay: Duration) {
        tokio::select! {
            _ = crate::executor::sleep(delay) => {}
            _ = self.wake_scheduler.notified() => {}
        }
    }

    pub fn download_error(&self, hash: sync_common::ContentHash) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        state.batches.get(&TransferJobKind::DownloadBook)?.jobs.values().find(|job| *job.job.status_fields().1 == hash)?.last_error.clone()
    }

    pub fn statuses(&self) -> Vec<TransferStatus> {
        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut statuses = state.batches.iter().filter_map(|(kind, batch)| batch.status(*kind)).collect::<Vec<_>>();
        statuses.sort_by_key(|status| status.kind.storage());
        statuses
    }

    /// Flush the current history snapshot. This may block on disk I/O, but never
    /// holds the queue lock while waiting. Dropping the queue also drains its writer.
    /// Call before opening another queue for the same history path.
    pub fn flush_history(&self) -> std::io::Result<()> {
        if let Some(error) = &self.history_error {
            return Err(std::io::Error::other(error.clone()));
        }
        if let Some(writer) = &self.history {
            // Wait for any completion already handing off its snapshot, then
            // release the queue before waiting for disk. Successful flushes do
            // not rewrite unchanged history during repeated shutdown hooks.
            drop(self.state.lock().unwrap_or_else(|error| error.into_inner()));
            if writer.flush().is_err() {
                {
                    let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                    writer.submit(state.history.clone());
                }
                writer.flush()?;
            }
        }
        Ok(())
    }

    pub fn history(&self) -> Vec<TransferStatus> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).history.iter().cloned().collect()
    }
}

fn find_job_mut(state: &mut QueueState, id: u64) -> Option<&mut QueueJob> {
    state.batches.values_mut().find_map(|batch| batch.jobs.get_mut(&id))
}

fn completed_status(kind: TransferJobKind, batch: TransferBatch, representative: &TransferJob) -> TransferStatus {
    let (_, content_hash) = representative.status_fields();
    let failed = batch.permanent_error.is_some();
    TransferStatus {
        id: format!("history-{}-{}", kind.storage(), uuid::Uuid::new_v4()),
        kind,
        content_hash: batch.failed_hash.unwrap_or(*content_hash),
        state: if failed { TransferState::Failed } else { TransferState::Completed },
        origin: batch.origin,
        attempts: batch.attempts,
        completed_items: batch.completed,
        total_items: batch.completed,
        failed_content_hash: batch.failed_hash,
        file_name: None,
        last_error: batch.permanent_error,
        next_retry_at: None,
        created_at: batch.created_at.to_string(),
        updated_at: batch.updated_at.to_string(),
    }
}

fn unix_seconds() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use sync_common::ContentHash;

    fn complete_one(queue: &TransferQueue, value: u64) {
        queue.reconcile(vec![TransferJob::upload_blob(ContentHash::new(&format!("{value:064x}")), TransferOrigin::Background)]);
        queue.complete(queue.claim_due().unwrap().id, None);
    }

    #[test]
    fn stalled_history_io_does_not_block_queue_operations_and_shutdown_drains_latest() {
        use std::sync::mpsc;
        use std::time::Duration;
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let saved = Arc::new(Mutex::new(Vec::new()));
        let writes = saved.clone();
        let mut first = true;
        let writer = super::super::history::writer::HistoryWriter::spawn(move |entries| {
            if first {
                first = false;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            writes.lock().unwrap().push(entries.iter().map(|entry| entry.content_hash).collect::<Vec<_>>());
            Ok(())
        })
        .unwrap();
        let queue = Arc::new(TransferQueue { history: Some(Box::new(writer)), ..Default::default() });
        complete_one(&queue, 1);
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let other = queue.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let work = std::thread::spawn(move || {
            for value in 2..=20 {
                complete_one(&other, value);
            }
            assert!(other.statuses().is_empty());
            done_tx.send(other.history().len()).unwrap();
        });
        let responsive = done_rx.recv_timeout(Duration::from_secs(2));
        release_tx.send(()).unwrap();
        work.join().unwrap();
        assert_eq!(responsive.unwrap(), 20, "queue operations must finish while disk I/O is blocked");
        let expected = queue.history().iter().map(|entry| entry.content_hash).collect::<Vec<_>>();
        drop(queue); // Must join the writer after publishing the latest pending snapshot.
        let saved = saved.lock().unwrap();
        assert_eq!(saved.len(), 2, "one active write plus one coalesced snapshot");
        assert_eq!(saved.last().unwrap(), &expected);
    }

    #[test]
    #[ignore = "release filesystem latency benchmark; run explicitly with --ignored --nocapture"]
    fn benchmark_history_completion_latency() {
        use std::time::Instant;
        let root = tempfile::tempdir().unwrap();
        let synchronous = MessagePackTransferHistory::new(root.path().join("synchronous.msgpack"));
        let old = TransferQueue::default();
        let mut before = Vec::new();
        let start = Instant::now();
        for value in 0..200 {
            let one = Instant::now();
            complete_one(&old, value);
            synchronous.store(&old.state.lock().unwrap().history).unwrap();
            before.push(one.elapsed());
        }
        let old_total = start.elapsed();
        let new = TransferQueue::with_history(MessagePackTransferHistory::new(root.path().join("asynchronous.msgpack")));
        let mut after = Vec::new();
        let start = Instant::now();
        for value in 0..200 {
            let one = Instant::now();
            complete_one(&new, value);
            after.push(one.elapsed());
        }
        let enqueue = start.elapsed();
        new.flush_history().unwrap();
        let new_total = start.elapsed();
        before.sort();
        after.sort();
        println!("200 completions: synchronous total={old_total:?}, caller p95={:?}; asynchronous enqueue={enqueue:?}, including flush={new_total:?}, caller p95={:?}", before[189], after[189]);
        assert_eq!(new.history().len(), HISTORY_LIMIT);
    }

    #[test]
    fn failed_batch_keeps_the_failed_book_when_another_book_finishes_last() {
        let queue = TransferQueue::default();
        let first = ContentHash::new(&format!("{:064x}", 1));
        let second = ContentHash::new(&format!("{:064x}", 2));
        queue.reconcile(vec![TransferJob::upload_blob(first, TransferOrigin::Background), TransferJob::upload_blob(second, TransferOrigin::Background)]);
        let failed = queue.claim_due().unwrap();
        let failed_hash = *failed.job.status_fields().1;
        queue.complete(failed.id, Some("raw diagnostic".into()));
        assert_eq!(queue.statuses()[0].content_hash, failed_hash);
        let succeeded = queue.claim_due().unwrap();
        queue.complete(succeeded.id, None);
        let history = queue.history();
        assert_eq!(history[0].state, TransferState::Failed);
        assert_eq!(history[0].content_hash, failed_hash);
        assert_eq!(history[0].failed_content_hash, Some(failed_hash));
    }

    #[test]
    fn reopened_history_keeps_prior_completions_and_remains_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.msgpack");
        let mut queue = TransferQueue::with_history(MessagePackTransferHistory::new(&path));
        for value in 0..55 {
            complete_one(&queue, value);
            assert_eq!(queue.history().len(), (value as usize + 1).min(HISTORY_LIMIT));
            if value == 2 {
                queue.flush_history().unwrap();
                queue = TransferQueue::with_history(MessagePackTransferHistory::new(&path));
            }
        }
        let saved_ids = queue.history().into_iter().map(|entry| entry.id).collect::<Vec<_>>();
        assert_eq!(saved_ids.len(), HISTORY_LIMIT);
        drop(queue);
        let reopened = TransferQueue::with_history(MessagePackTransferHistory::new(&path));
        assert_eq!(reopened.history().into_iter().map(|entry| entry.id).collect::<Vec<_>>(), saved_ids);
    }

    #[test]
    fn failed_save_keeps_recent_history_visible_and_next_save_includes_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.msgpack");
        let queue = TransferQueue::with_history(MessagePackTransferHistory::new(&path));
        complete_one(&queue, 1);
        queue.flush_history().unwrap();
        std::fs::create_dir(path.with_extension("msgpack.new")).unwrap();
        complete_one(&queue, 2);
        assert!(queue.flush_history().is_err());
        assert_eq!(queue.history().len(), 2);
        assert_eq!(MessagePackTransferHistory::new(&path).load().unwrap().len(), 1);
        std::fs::remove_dir(path.with_extension("msgpack.new")).unwrap();
        queue.flush_history().unwrap();
        assert_eq!(MessagePackTransferHistory::new(&path).load().unwrap().len(), 2);
        complete_one(&queue, 3);
        queue.flush_history().unwrap();
        let restored = TransferQueue::with_history(MessagePackTransferHistory::new(&path));
        assert_eq!(restored.history().into_iter().map(|entry| entry.id).collect::<Vec<_>>(), queue.history().into_iter().map(|entry| entry.id).collect::<Vec<_>>());
    }
}

#[cfg(test)]
mod page_tests {
    use super::*;
    use sync_common::ContentHash;

    fn job(value: u64) -> TransferJob {
        TransferJob::upload_blob(ContentHash::new(&format!("{value:064x}")), TransferOrigin::Background)
    }

    #[test]
    fn thumbnail_upload_batch_spans_generation_and_drains_after_producer_finishes() {
        let queue = Arc::new(TransferQueue::default());
        let producer = queue.begin_producer(TransferJobKind::UploadThumbnail);
        let thumbnail = |value| TransferJob::upload_thumbnail(ContentHash::new(&format!("{value:064x}")), TransferOrigin::Background);
        queue.reconcile(vec![thumbnail(1)]);
        queue.complete(queue.claim_due().unwrap().id, None);
        // Separate planning passes, including an empty pass, must preserve the batch.
        queue.reconcile(vec![]);
        assert_eq!(queue.statuses()[0].completed_items, 1);
        assert!(queue.history().is_empty());
        assert!(queue.next_due_in().is_none(), "waiting for generation must not spin the scheduler");
        queue.reconcile(vec![thumbnail(2), job(3)]);
        queue.complete(queue.claim_due().unwrap().id, None);
        queue.complete(queue.claim_due().unwrap().id, None);
        assert_eq!(queue.history().len(), 1, "book uploads do not wait for thumbnail generation");
        assert_eq!(queue.statuses()[0].completed_items, 2);
        queue.reconcile(vec![thumbnail(4)]);
        drop(producer);
        assert_eq!(queue.statuses()[0].total_items, 3, "generation ending must preserve outstanding uploads");
        queue.complete(queue.claim_due().unwrap().id, None);
        assert!(queue.statuses().is_empty());
        assert_eq!(queue.history()[0].completed_items, 3);
    }

    #[test]
    fn generation_completion_waits_for_a_fresh_successful_plan() {
        let queue = Arc::new(TransferQueue::default());
        let producer = queue.begin_producer(TransferJobKind::UploadThumbnail);
        let generation = queue.begin_reconcile();
        queue.reconcile_page(generation, vec![TransferJob::upload_thumbnail(ContentHash::new(&format!("{:064x}", 1)), TransferOrigin::Background)]);
        queue.complete(queue.claim_due().unwrap().id, None);
        producer.finish();
        queue.finish_reconcile(generation);
        assert_eq!(queue.statuses().len(), 1, "a pass started during generation can miss its final writes");
        let generation = queue.begin_reconcile();
        queue.cancel_reconcile(generation);
        assert_eq!(queue.statuses().len(), 1, "failed planning must retry the final handoff");
        queue.reconcile(vec![]);
        assert!(queue.statuses().is_empty());
        assert_eq!(queue.history()[0].completed_items, 1);
    }

    #[test]
    fn ending_thumbnail_generation_closes_an_empty_batch_after_planning() {
        let queue = Arc::new(TransferQueue::default());
        let producer = queue.begin_producer(TransferJobKind::UploadThumbnail);
        let generation = queue.begin_reconcile();
        queue.reconcile_page(generation, vec![TransferJob::upload_thumbnail(ContentHash::new(&format!("{:064x}", 1)), TransferOrigin::Background)]);
        queue.complete(queue.claim_due().unwrap().id, None);
        drop(producer);
        assert_eq!(queue.statuses().len(), 1, "an open planner can still add uploads");
        queue.cancel_reconcile(generation);
        assert!(queue.statuses().is_empty());
        assert_eq!(queue.history()[0].completed_items, 1);
    }

    #[test]
    fn batch_visibility_spans_producer_pages_and_cancellation_closes_it() {
        let queue = TransferQueue::default();
        let generation = queue.begin_reconcile();
        queue.reconcile_page(generation, vec![job(1)]);
        queue.complete(queue.claim_due().unwrap().id, None);
        assert_eq!(queue.statuses()[0].completed_items, 1);
        assert!(queue.history().is_empty());
        queue.reconcile_page(generation, vec![job(2)]);
        assert_eq!(queue.statuses()[0].total_items, 2);
        queue.complete(queue.claim_due().unwrap().id, None);
        queue.finish_reconcile(generation);
        assert!(queue.statuses().is_empty());
        assert_eq!(queue.history()[0].completed_items, 2);
        let generation = queue.begin_reconcile();
        queue.reconcile_page(generation, vec![job(3)]);
        queue.complete(queue.claim_due().unwrap().id, None);
        queue.cancel_reconcile(generation);
        assert!(queue.statuses().is_empty());
        assert_eq!(queue.history().len(), 2);
    }

    #[test]
    fn first_page_can_run_before_later_pages_and_cleanup() {
        let queue = TransferQueue::default();
        let generation = queue.begin_reconcile();
        queue.reconcile_page(generation, vec![job(1), job(2)]);
        let first = queue.claim_due().unwrap();
        assert_eq!(first.job, job(1));
        queue.reconcile_page(generation, vec![job(3)]);
        queue.finish_reconcile(generation);
        queue.complete(first.id, None);
        assert_eq!(queue.claim_due().unwrap().job, job(2));
        assert_eq!(queue.claim_due().unwrap().job, job(3));
    }

    #[test]
    fn interrupted_or_superseded_planning_cannot_prune_unseen_work() {
        let queue = TransferQueue::default();
        queue.reconcile(vec![job(1)]);
        let abandoned = queue.begin_reconcile();
        queue.reconcile_page(abandoned, vec![job(2)]);
        assert_eq!(queue.statuses()[0].total_items, 2);
        let current = queue.begin_reconcile();
        queue.reconcile_page(current, vec![job(3)]);
        queue.finish_reconcile(abandoned);
        queue.reconcile_page(abandoned, vec![job(4)]);
        assert_eq!(queue.statuses()[0].total_items, 3);
        queue.finish_reconcile(current);
        assert_eq!(queue.statuses()[0].total_items, 1);
        assert_eq!(queue.claim_due().unwrap().job, job(3));
    }

    #[test]
    fn rediscovered_jobs_keep_retry_deadlines_and_user_priority() {
        let queue = TransferQueue::default();
        queue.reconcile(vec![job(1)]);
        let claimed = queue.claim_due().unwrap();
        queue.retry(claimed.id, "offline".into());
        let before = queue.statuses()[0].next_retry_at.clone();
        let generation = queue.begin_reconcile();
        let mut manual = job(1);
        manual.origin = TransferOrigin::UserInitiated;
        queue.reconcile_page(generation, vec![manual]);
        queue.reconcile_page(generation, vec![job(1)]);
        queue.finish_reconcile(generation);
        let status = &queue.statuses()[0];
        assert_eq!(status.total_items, 1);
        assert_eq!(status.origin, TransferOrigin::UserInitiated);
        assert_eq!(status.attempts, 1);
        assert_eq!(status.next_retry_at, before);
        assert!(queue.claim_due().is_none());
    }
    #[test]
    fn explicit_requests_join_the_active_plan_without_pruning_or_duplication() {
        let queue = TransferQueue::default();
        let generation = queue.begin_reconcile();
        queue.reconcile_page(generation, vec![job(1)]);
        queue.enqueue(vec![job(2), job(2)]);
        queue.finish_reconcile(generation);
        assert_eq!(queue.statuses()[0].total_items, 2);
        assert_eq!(queue.claim_due().unwrap().job, job(1));
        assert_eq!(queue.claim_due().unwrap().job, job(2));
    }
}
