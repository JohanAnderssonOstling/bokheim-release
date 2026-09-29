use super::*;
use crate::TransferOrigin;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Default)]
struct Host {
    outcomes: Mutex<VecDeque<TransferRunResult>>,
    batches: Mutex<Vec<usize>>,
    settled: Mutex<usize>,
}
impl TransferHost for Host {
    fn prepare_uploads<'a>(&'a self, _: &'a [library_database::BookUploadIntent], _: &'a mut library_database::TransferOutcome) -> LocalBoxFuture<'a, Result<PreparedUploads, TransferError>> {
        Box::pin(async { panic!("download tests must not negotiate uploads") })
    }
    fn execute<'a>(&'a self, _: &'a library_database::TransferSnapshot, claimed: &'a [ClaimedTransferJob], _: &'a mut library_database::TransferOutcome) -> LocalBoxFuture<'a, TransferRunResult> {
        Box::pin(async move {
            self.batches.lock().unwrap().push(claimed.len());
            self.outcomes.lock().unwrap().pop_front().unwrap_or(Ok(()))
        })
    }
    fn settle_rejection(&self, _: &TransferJob, _: &str, _: &mut library_database::TransferOutcome) {
        *self.settled.lock().unwrap() += 1;
    }
    fn changed(&self) {}
    fn flush(&self) {}
}
fn books() -> TransferQueue {
    let queue = TransferQueue::default();
    queue.reconcile(
        (1..=3)
            .map(|id| {
                let hash = test_support::fixture_content_hash(id);
                TransferJob::download_blob(hash, library_database::BookPlacement::new(hash, library_database::RelativeBookPath::parse(&format!("/{id}.epub")).unwrap()), TransferOrigin::UserInitiated)
            })
            .collect(),
    );
    queue
}

#[tokio::test]
async fn authentication_releases_unprocessed_jobs_for_resumption() {
    let queue = books();
    let host = Host::default();
    host.outcomes.lock().unwrap().push_back(Err(TransferError::AuthenticationRequired));
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    let runner = TransferRunner { queue: &queue, host: &host };
    assert!(queue.try_start_scheduler());
    runner.run(&snapshot, &mut out).await;
    assert_eq!(*host.batches.lock().unwrap(), vec![1]);
    assert!(!queue.try_start_scheduler(), "authentication pauses scheduler starts");
    queue.resume_authentication();
    assert!(queue.try_start_scheduler());
    runner.run(&snapshot, &mut out).await;
    assert_eq!(*host.batches.lock().unwrap(), vec![1, 1, 1, 1]);
    assert!(queue.statuses().is_empty());
}

#[tokio::test]
async fn a_retryable_file_does_not_block_its_neighbors() {
    let queue = books();
    let host = Host::default();
    host.outcomes.lock().unwrap().push_back(Err(TransferError::retryable("locked file")));
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    TransferRunner { queue: &queue, host: &host }.drain_ready(&snapshot, &mut out).await.unwrap();
    assert_eq!(*host.batches.lock().unwrap(), vec![1, 1, 1]);
    assert!(!queue.statuses().is_empty(), "failed file retains scheduled work");
    assert!(queue.claim_due_batch().is_empty(), "failed file observes its retry delay");
}

#[tokio::test]
async fn one_batch_drain_stops_before_the_next_claim() {
    let queue = books();
    let host = Host::default();
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    let runner = TransferRunner { queue: &queue, host: &host };
    runner.drain_one_batch(&snapshot, &mut out).await.committed(&queue).unwrap();
    assert_eq!(*host.batches.lock().unwrap(), vec![1]);
    let status = queue.statuses();
    assert_eq!(status[0].completed_items, 1);
    assert_eq!(status[0].total_items, 3);
    runner.drain_ready(&snapshot, &mut out).await.unwrap();
    assert_eq!(*host.batches.lock().unwrap(), vec![1, 1, 1]);
    assert!(queue.statuses().is_empty());
}

#[tokio::test]
async fn claimed_job_stays_running_until_database_replay_succeeds() {
    let queue = books();
    let host = Host::default();
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    let drain = TransferRunner { queue: &queue, host: &host }.drain_one_batch(&snapshot, &mut out).await;
    let status = queue.statuses();
    assert_eq!(status[0].completed_items, 0);
    assert_eq!(status[0].total_items, 3);
    drain.commit_failed(&queue, &TransferError::retryable("database commit failed"));
    let status = queue.statuses();
    assert_eq!(status[0].completed_items, 0);
    assert_eq!(status[0].total_items, 3);
    assert!(queue.download_error(test_support::fixture_content_hash(1)).unwrap().contains("database commit failed"));
}

#[tokio::test]
async fn rejected_work_completes_with_its_settlement_reason() {
    let queue = books();
    let host = Host::default();
    host.outcomes.lock().unwrap().push_back(Err(TransferError::rejected("missing asset")));
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    TransferRunner { queue: &queue, host: &host }.drain_ready(&snapshot, &mut out).await.unwrap();
    assert_eq!(*host.settled.lock().unwrap(), 1);
    assert_eq!(*host.batches.lock().unwrap(), vec![1, 1, 1]);
    assert!(queue.claim_due_batch().is_empty(), "settled rejection leaves no pending work");
}

#[tokio::test]
async fn thumbnail_downloads_share_one_bounded_execution() {
    let queue = TransferQueue::default();
    queue.reconcile((1..=3).map(|id| TransferJob::download_thumbnail(test_support::fixture_content_hash(id), TransferOrigin::Background)).collect());
    let host = Host::default();
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    TransferRunner { queue: &queue, host: &host }.drain_ready(&snapshot, &mut out).await.unwrap();
    assert_eq!(*host.batches.lock().unwrap(), vec![3]);
    assert!(queue.statuses().is_empty());
}

#[tokio::test]
async fn thumbnail_uploads_share_one_bounded_execution() {
    let queue = TransferQueue::default();
    queue.reconcile((1..=3).map(|id| TransferJob::upload_thumbnail(test_support::fixture_content_hash(id), TransferOrigin::Background)).collect());
    let host = Host::default();
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    TransferRunner { queue: &queue, host: &host }.drain_ready(&snapshot, &mut out).await.unwrap();
    assert_eq!(*host.batches.lock().unwrap(), vec![3]);
    assert!(queue.statuses().is_empty());
}

fn upload_claims(sizes: &[u64]) -> Vec<crate::ClaimedTransferJob> {
    sizes
        .iter()
        .enumerate()
        .map(|(i, &size_bytes)| {
            let hash = test_support::fixture_content_hash(i as u64);
            let intent = library_database::BookUploadIntent { id: i as i64, content_hash: hash, checksum: hash, size_bytes };
            crate::ClaimedTransferJob { id: i as u64, job: TransferJob::upload_book_intent(intent, true, TransferOrigin::Background) }
        })
        .collect()
}

#[test]
fn upload_groups_stop_at_total_bytes_count_or_large_book() {
    use sync_common::book_batch::*;
    assert_eq!(upload_group_len(&upload_claims(&[MAX_BOOK_BYTES; 8])), 4);
    assert_eq!(upload_group_len(&upload_claims(&[1; 20])), MAX_BOOKS);
    assert_eq!(upload_group_len(&upload_claims(&[1, MAX_BOOK_BYTES + 1, 1])), 1);
    assert_eq!(upload_group_len(&upload_claims(&[MAX_BOOK_BYTES + 1, 1, 1])), 1);
    assert_eq!(upload_group_len(&upload_claims(&[MAX_BOOK_BYTES + 1; 5])), MAX_CONCURRENT_LARGE_UPLOADS);
    assert_eq!(upload_group_len(&upload_claims(&[1, 1, 1])), 3);
}

struct UploadHost {
    jobs: Vec<TransferJob>,
    groups: Mutex<Vec<usize>>,
}
impl TransferHost for UploadHost {
    fn prepare_uploads<'a>(&'a self, _: &'a [library_database::BookUploadIntent], _: &'a mut library_database::TransferOutcome) -> LocalBoxFuture<'a, Result<PreparedUploads, TransferError>> {
        Box::pin(async { Ok(PreparedUploads { jobs: self.jobs.clone(), dropped: Vec::new() }) })
    }
    fn execute<'a>(&'a self, _: &'a library_database::TransferSnapshot, _: &'a [ClaimedTransferJob], _: &'a mut library_database::TransferOutcome) -> LocalBoxFuture<'a, TransferRunResult> {
        Box::pin(async { panic!("expected grouped upload") })
    }
    fn execute_uploads<'a>(&'a self, _: &'a library_database::TransferSnapshot, claimed: &'a [ClaimedTransferJob], _: &'a mut library_database::TransferOutcome) -> LocalBoxFuture<'a, Result<Vec<TransferRunResult>, TransferError>> {
        Box::pin(async move {
            self.groups.lock().unwrap().push(claimed.len());
            Ok(claimed.iter().enumerate().map(|(i, _)| if i == 1 { Err(TransferError::retryable("one file failed")) } else { Ok(()) }).collect())
        })
    }

    fn settle_rejection(&self, _: &TransferJob, _: &str, _: &mut library_database::TransferOutcome) {}
    fn changed(&self) {}
    fn flush(&self) {}
}

#[tokio::test]
async fn batch_failure_retries_only_the_failed_book() {
    let jobs = upload_claims(&[1, 2, 3]).into_iter().map(|item| item.job).collect::<Vec<_>>();
    let queue = TransferQueue::default();
    queue.reconcile(jobs.clone());
    let host = UploadHost { jobs, groups: Mutex::new(Vec::new()) };
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    TransferRunner { queue: &queue, host: &host }.drain_ready(&snapshot, &mut out).await.unwrap();
    assert_eq!(*host.groups.lock().unwrap(), vec![3]);
    let status = queue.statuses();
    assert_eq!(status[0].completed_items, 2);
    assert_eq!(status[0].total_items, 3);
    assert!(queue.claim_due_batch().is_empty(), "failed file retains retry backoff");
}

#[tokio::test]
async fn large_upload_group_retries_only_the_failed_book() {
    let jobs = upload_claims(&[sync_common::book_batch::MAX_BOOK_BYTES + 1; 4]).into_iter().map(|item| item.job).collect::<Vec<_>>();
    let queue = TransferQueue::default();
    queue.reconcile(jobs.clone());
    let host = UploadHost { jobs, groups: Mutex::new(Vec::new()) };
    let snapshot = library_database::TransferSnapshot::default();
    let mut out = library_database::TransferOutcome::default();
    TransferRunner { queue: &queue, host: &host }.drain_ready(&snapshot, &mut out).await.unwrap();
    assert_eq!(*host.groups.lock().unwrap(), vec![MAX_CONCURRENT_LARGE_UPLOADS]);
    let status = queue.statuses();
    assert_eq!(status[0].completed_items, 3);
    assert_eq!(status[0].total_items, 4);
}
