//! Transfer queue execution policy, independent of application authentication and events.
use crate::{ClaimedTransferJob, TransferError, TransferJob, TransferQueue};
use futures_util::future::LocalBoxFuture;
use library_database::{BookUploadIntent, TransferOutcome, TransferSnapshot};
use library_replica::TransferJobKind;
use sync_common::ContentHash;

type TransferRunResult = Result<(), TransferError>;

/// Large native books use independent streams, bounded separately from the
/// small-book batch endpoint so one slow book does not serialize the queue.
pub const MAX_CONCURRENT_LARGE_UPLOADS: usize = 4;

/// Host operations preserve application credential refresh, browser coordination,
/// and notifications without coupling the runner to backend. The host works
/// only on snapshots and records durable effects into the outcome the
/// orchestrator commits; no storage crosses this boundary in either direction.
/// Futures stay local to the driving task, so the host is not `Sync`.
pub trait TransferHost {
    fn prepare_uploads<'a>(&'a self, intents: &'a [BookUploadIntent], out: &'a mut TransferOutcome) -> LocalBoxFuture<'a, Result<PreparedUploads, TransferError>>;
    fn execute<'a>(&'a self, snapshot: &'a TransferSnapshot, claimed: &'a [ClaimedTransferJob], out: &'a mut TransferOutcome) -> LocalBoxFuture<'a, TransferRunResult>;
    fn execute_uploads<'a>(&'a self, snapshot: &'a TransferSnapshot, claimed: &'a [ClaimedTransferJob], out: &'a mut TransferOutcome) -> LocalBoxFuture<'a, Result<Vec<TransferRunResult>, TransferError>> {
        self.execute_uploads_sequentially(snapshot, claimed, out)
    }
    fn execute_uploads_sequentially<'a>(&'a self, snapshot: &'a TransferSnapshot, claimed: &'a [ClaimedTransferJob], out: &'a mut TransferOutcome) -> LocalBoxFuture<'a, Result<Vec<TransferRunResult>, TransferError>> {
        Box::pin(async move {
            let mut results = Vec::new();
            for item in claimed {
                let result = self.execute(snapshot, std::slice::from_ref(item), out).await;
                if matches!(result, Err(TransferError::AuthenticationRequired)) {
                    return Err(TransferError::AuthenticationRequired);
                }
                results.push(result);
            }
            Ok(results)
        })
    }
    fn settle_rejection(&self, job: &TransferJob, reason: &str, out: &mut TransferOutcome);
    fn changed(&self);
    fn flush(&self);
}

/// One negotiated upload batch: jobs to run plus per-intent dispositions for
/// claimed hashes the negotiation dropped. Dropped intents never reach the
/// queue as jobs; the runner completes or retries them from this data.
pub struct PreparedUploads {
    pub jobs: Vec<TransferJob>,
    pub dropped: Vec<DroppedUpload>,
}

/// Why a claimed hash produced no job.
pub enum DroppedUpload {
    /// The server already has the bytes or the rejection was recorded; the
    /// settle record carries the durable effect.
    Completed { hash: ContentHash },
    /// The server rejected the intent with a reason to complete with.
    Rejected { hash: ContentHash, reason: String },
    /// The intent changed or vanished; retry it on a later pass.
    Changed { hash: ContentHash },
}

pub struct TransferRunner<'a, H> {
    pub queue: &'a TransferQueue,
    pub host: &'a H,
}

enum QueueSettlement {
    Complete(u64, Option<String>),
    Retry(u64, String),
    Release(u64),
}

/// Queue results for one database replay. Claimed jobs stay running until the
/// owner confirms that every recorded database write has been replayed.
pub struct TransferDrain {
    result: TransferRunResult,
    claimed: Vec<ClaimedTransferJob>,
    settlements: Vec<QueueSettlement>,
}

impl TransferDrain {
    pub fn committed(self, queue: &TransferQueue) -> TransferRunResult {
        for settlement in self.settlements {
            match settlement {
                QueueSettlement::Complete(id, error) => queue.complete(id, error),
                QueueSettlement::Retry(id, error) => { queue.retry(id, error); }
                QueueSettlement::Release(id) => {
                    if let Some(item) = self.claimed.iter().find(|item| item.id == id) {
                        queue.release(std::slice::from_ref(item));
                    }
                }
            }
        }
        self.result
    }

    pub fn commit_failed(self, queue: &TransferQueue, error: &TransferError) {
        for item in &self.claimed {
            queue.retry(item.id, error.to_string());
        }
    }
}

impl<H: TransferHost> TransferRunner<'_, H> {
    /// Drives due batches with snapshot inputs, accumulating durable writes
    /// into `out`. The orchestrator commits `out` even when this returns an
    /// error; write records are idempotent replays of the guarded commits.
    #[cfg(test)]
    pub async fn drain_ready(&self, snapshot: &TransferSnapshot, out: &mut TransferOutcome) -> TransferRunResult {
        self.drain_with_limit(snapshot, out, None).await.committed(self.queue)
    }

    /// One claimed batch per database snapshot. The production driver commits
    /// the resulting writes before claiming more work, so a slow audiobook or
    /// continuously refilled queue cannot hide completed transfers.
    pub async fn drain_one_batch(&self, snapshot: &TransferSnapshot, out: &mut TransferOutcome) -> TransferDrain {
        self.drain_with_limit(snapshot, out, Some(1)).await
    }

    async fn drain_with_limit(&self, snapshot: &TransferSnapshot, out: &mut TransferOutcome, limit: Option<usize>) -> TransferDrain {
        let mut processed = 0;
        let mut drain = TransferDrain { result: Ok(()), claimed: Vec::new(), settlements: Vec::new() };
        loop {
            if limit.is_some_and(|limit| processed >= limit) {
                break;
            }
            let mut claimed = self.queue.claim_due_batch();
            let claimed_at = web_time::Instant::now();
            if claimed.is_empty() {
                break;
            }
            drain.claimed.extend(claimed.iter().cloned());
            processed += 1;
            self.host.changed();
            if claimed[0].job.batch_kind() == Some(TransferJobKind::UploadBook) {
                let mut intents = Vec::new();
                for item in &claimed {
                    let hash = *item.job.status_fields().1;
                    if let Some(intent) = snapshot.asset_for(&hash).and_then(|asset| asset.upload.intent.as_ref()) {
                        if !intents.iter().any(|intent: &BookUploadIntent| intent.content_hash == hash) {
                            intents.push(intent.clone());
                        }
                    }
                }
                let mut trace = sync_transport::PerformanceTrace::new("prepare_upload_batch", "negotiate_and_plan");
                log::debug!(target: "sync_performance", "trace_id={} books={}", trace.id(), intents.len());
                let result = self.host.prepare_uploads(&intents, out).await;
                trace.finish(result.is_ok());
                match result {
                    Ok(prepared) => {
                        claimed.retain_mut(|item| {
                            let hash = *item.job.status_fields().1;
                            if let Some(job) = prepared.jobs.iter().find(|job| *job.status_fields().1 == hash) {
                                item.job = job.clone();
                                true
                            } else {
                                fn dropped_hash(dropped: &DroppedUpload) -> &ContentHash {
                                    match dropped {
                                        DroppedUpload::Completed { hash } | DroppedUpload::Rejected { hash, .. } | DroppedUpload::Changed { hash } => hash,
                                    }
                                }
                                match prepared.dropped.iter().find(|dropped| dropped_hash(dropped) == &hash) {
                                    Some(DroppedUpload::Completed { .. }) => drain.settlements.push(QueueSettlement::Complete(item.id, None)),
                                    Some(DroppedUpload::Rejected { reason, .. }) => drain.settlements.push(QueueSettlement::Complete(item.id, Some(reason.clone()))),
                                    _ => {
                                        drain.settlements.push(QueueSettlement::Retry(item.id, "upload intent changed during transfer".to_owned()));
                                    }
                                }
                                false
                            }
                        });
                        self.host.changed();
                    }
                    Err(TransferError::AuthenticationRequired) => {
                        drain.settlements.extend(claimed.iter().map(|item| QueueSettlement::Release(item.id)));
                        self.host.changed();
                        drain.result = Err(TransferError::AuthenticationRequired);
                        return drain;
                    }
                    Err(error) => {
                        for item in &claimed {
                            drain.settlements.push(QueueSettlement::Retry(item.id, error.to_string()));
                        }
                        self.host.changed();
                        continue;
                    }
                }
            }
            // Thumbnail requests are bounded by both count and payload size,
            // and their endpoint completes the whole request atomically.
            // They therefore share one outcome and one retry decision.
            let is_thumbnail_batch = matches!(claimed.first().and_then(|item| item.job.batch_kind()), Some(TransferJobKind::DownloadThumbnail | TransferJobKind::UploadThumbnail));
            if is_thumbnail_batch {
                let outcome = { self.host.execute(snapshot, &claimed, out).await };
                match outcome {
                    Ok(()) => {
                        for item in &claimed {
                            drain.settlements.push(QueueSettlement::Complete(item.id, None));
                        }
                    }
                    Err(TransferError::Retryable(message)) => {
                        log::warn!("Retryable thumbnail batch error: {message}");
                        for item in &claimed {
                            drain.settlements.push(QueueSettlement::Retry(item.id, message.to_string()));
                        }
                    }
                    Err(TransferError::Rejected(message)) => {
                        log::warn!("Rejected thumbnail batch: {message}");
                        for item in &claimed {
                            self.host.settle_rejection(&item.job, &message.to_string(), out);
                            drain.settlements.push(QueueSettlement::Complete(item.id, Some(message.to_string())));
                        }
                    }
                    Err(TransferError::AuthenticationRequired) => {
                        drain.settlements.extend(claimed.iter().map(|item| QueueSettlement::Release(item.id)));
                        self.host.changed();
                        self.host.flush();
                        drain.result = Err(TransferError::AuthenticationRequired);
                        return drain;
                    }
                }
            } else {
                let mut batch_results = std::collections::VecDeque::new();
                for (index, item) in claimed.iter().enumerate() {
                    let mut trace = sync_transport::PerformanceTrace::new("transfer_job", "execute");
                    log::debug!(target: "sync_performance", "trace_id={} job_id={} kind={:?} since_batch_claim_ms={:.3}", trace.id(), item.id, item.job.batch_kind(), claimed_at.elapsed().as_secs_f64() * 1000.0);
                    if batch_results.is_empty() {
                        // OPFS uploads still use the individual browser path;
                        // settle each result immediately there.
                        let count = if cfg!(target_arch = "wasm32") { 1 } else { upload_group_len(&claimed[index..]) };
                        if count > 1 {
                            let group = &claimed[index..index + count];
                            batch_results = match self.host.execute_uploads(snapshot, group, out).await {
                                Ok(results) if results.len() == count => results.into(),
                                Ok(_) => (0..count).map(|_| Err(TransferError::retryable("incomplete upload batch results"))).collect(),
                                Err(TransferError::AuthenticationRequired) => (0..count).map(|_| Err(TransferError::AuthenticationRequired)).collect(),
                                Err(error) => (0..count).map(|_| Err(TransferError::retryable(error.clone()))).collect(),
                            };
                        } else {
                            batch_results.push_back(self.host.execute(snapshot, std::slice::from_ref(item), out).await);
                        }
                    }
                    let outcome = batch_results.pop_front().expect("one result per claimed job");
                    trace.finish(outcome.is_ok());
                    match outcome {
                        Ok(()) => drain.settlements.push(QueueSettlement::Complete(item.id, None)),
                        Err(TransferError::Retryable(message)) => {
                            log::warn!("Retryable transfer error for job {}: {message}", item.id);
                            drain.settlements.push(QueueSettlement::Retry(item.id, message.to_string()));
                        }
                        Err(TransferError::Rejected(message)) => {
                            log::warn!("Rejected transfer for job {}: {message}", item.id);
                            self.host.settle_rejection(&item.job, &message.to_string(), out);
                            drain.settlements.push(QueueSettlement::Complete(item.id, Some(message.to_string())));
                        }
                        Err(TransferError::AuthenticationRequired) => {
                            // Release the current and all not-yet-processed
                            // jobs. Authentication is global; file failures are
                            // deliberately not.
                            drain.settlements.extend(claimed[index..].iter().map(|item| QueueSettlement::Release(item.id)));
                            self.host.changed();
                            self.host.flush();
                            drain.result = Err(TransferError::AuthenticationRequired);
                            return drain;
                        }
                    }
                    self.host.changed();
                }
            }
            self.host.changed();
        }
        self.host.flush();
        drain
    }

    /// Run until idle or authentication pauses the queue. The host owns spawning
    /// and must first acquire the queue with `try_start_scheduler`.
    #[cfg(test)]
    pub async fn run(&self, snapshot: &TransferSnapshot, out: &mut TransferOutcome) {
        loop {
            if let Err(TransferError::AuthenticationRequired) = self.drain_ready(snapshot, out).await {
                self.queue.pause_authentication();
                return;
            }
            let Some(delay) = self.queue.next_due_in() else {
                if self.queue.keep_scheduler_or_stop() {
                    continue;
                }
                return;
            };
            self.queue.wait_for_wake(delay).await;
        }
    }
}

#[cfg(test)]
mod tests;

/// Bound payloads as well as count; oversized/non-negotiated jobs stay single.
fn upload_group_len(claimed: &[ClaimedTransferJob]) -> usize {
    use sync_common::book_batch::{MAX_BATCH_BYTES, MAX_BOOKS, MAX_BOOK_BYTES};
    // Large books cannot use the small-book batch endpoint. Execute several
    // independent uploads together instead of serializing every HTTP stream.
    if claimed.first().is_some_and(|item| matches!(&item.job.operation, crate::TransferOperation::UploadBlob { intent: Some(intent), negotiated: true, .. } if intent.size_bytes > MAX_BOOK_BYTES)) {
        return claimed
            .iter()
            .take(MAX_CONCURRENT_LARGE_UPLOADS)
            .take_while(|item| matches!(&item.job.operation, crate::TransferOperation::UploadBlob { intent: Some(intent), negotiated: true, .. } if intent.size_bytes > MAX_BOOK_BYTES))
            .count();
    }
    let mut bytes = 0u64;
    let mut count = 0;
    for item in claimed.iter().take(MAX_BOOKS) {
        let crate::TransferOperation::UploadBlob { intent: Some(intent), negotiated: true, .. } = &item.job.operation else { break };
        if intent.size_bytes == 0 || intent.size_bytes > MAX_BOOK_BYTES || bytes + intent.size_bytes > MAX_BATCH_BYTES {
            break;
        }
        bytes += intent.size_bytes;
        count += 1;
    }
    count.max(1)
}
