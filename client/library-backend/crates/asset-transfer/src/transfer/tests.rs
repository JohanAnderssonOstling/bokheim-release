use super::*;
use crate::{TransferError, TransferJob, TransferOperation, TransferOrigin, TransferQueue, TransferState};
use account_client::ServerUrl;
use library_database::{RelativeBookPath, TransferOutcome};
use library_model::{DownloadProgress, DownloadState};
use library_replica::{BlobKind, BookPlacement};
use library_runtime::transfer_history::queue_with_history;
use sync_common::{api::assets::MAX_THUMBNAIL_BATCH_ENTRIES, ContentHash, ROOT_DIR_ID};

const APP_HIDDEN_DIR: &str = ".bokheim";
use axum::body::Body;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::{get, post, put};
use axum::Router;
use reqwest::Client;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use test_support::fixture_content_hash;

const TEST_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn test_credentials(server_url: &ServerUrl) -> library_runtime::account::SharedSyncCredentials {
    Arc::new(std::sync::RwLock::new(Some(sync_transport::SyncCredentials::new(server_url.clone(), TEST_TOKEN))))
}

fn queued_upload(hash: &str, origin: TransferOrigin) -> TransferJob {
    TransferJob::upload_blob(ContentHash::new(&hash), origin)
}

/// The transfer stack speaks replica values; fixtures speak session values.
/// Conversion is field-identical and test-only.
fn db_placement(placement: &BookPlacement) -> library_database::BookPlacement {
    library_database::BookPlacement::new(placement.content_hash, library_database::RelativeBookPath::parse(placement.rel_path.as_str()).unwrap())
}

#[test]
fn ephemeral_queue_deduplicates_and_preserves_user_priority() {
    let queue = TransferQueue::default();
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    queue.reconcile(vec![queued_upload(hash, TransferOrigin::Background), queued_upload(hash, TransferOrigin::UserInitiated)]);
    queue.reconcile(vec![queued_upload(hash, TransferOrigin::Background)]);

    let statuses = queue.statuses();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].origin, TransferOrigin::UserInitiated);
    assert_eq!(statuses[0].state, TransferState::Queued);
}

#[test]
fn transfer_statuses_are_aggregated_with_stable_batch_progress() {
    let queue = TransferQueue::default();
    let first = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let second = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    queue.reconcile(vec![queued_upload(first, TransferOrigin::Background), queued_upload(second, TransferOrigin::Background)]);
    let initial = queue.statuses();
    assert_eq!(initial.len(), 1);
    assert_eq!((initial[0].completed_items, initial[0].total_items), (0, 2));

    let completed = queue.claim_due().unwrap();
    queue.complete(completed.id, None);
    let progressed = queue.statuses();
    assert_eq!(progressed.len(), 1);
    assert_eq!((progressed[0].completed_items, progressed[0].total_items), (1, 2));

    let completed = queue.claim_due().unwrap();
    queue.complete(completed.id, None);
    assert!(queue.statuses().is_empty());
    let history = queue.history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].state, TransferState::Completed);
    assert_eq!((history[0].completed_items, history[0].total_items), (2, 2));
}

#[test]
fn completed_batch_history_survives_queue_recreation() {
    let root = tempfile::tempdir().unwrap();
    let history_path = root.path().join("transfer-history-v1.msgpack");
    let db = Arc::new(library_database::Database::open(root.path().join("history.sqlite3").to_str().unwrap()).unwrap());
    let queue = queue_with_history(&db, history_path.clone());
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    queue.reconcile(vec![queued_upload(hash, TransferOrigin::Background)]);
    let completed = queue.claim_due().unwrap();
    queue.complete(completed.id, None);
    drop(queue);

    let restored = queue_with_history(&db, history_path);
    let history = restored.history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].state, TransferState::Completed);
    assert_eq!((history[0].completed_items, history[0].total_items), (1, 1));
}

#[test]
fn thumbnail_network_work_is_claimed_in_bounded_batches() {
    let queue = TransferQueue::default();
    let jobs: Vec<_> = (0..20).map(|index| TransferJob::download_thumbnail(fixture_content_hash(index), TransferOrigin::Background)).collect();
    queue.reconcile(jobs.clone());
    let first = queue.claim_due_batch().into_iter().map(|claimed| claimed.job).collect::<Vec<_>>();
    let second = queue.claim_due_batch().into_iter().map(|claimed| claimed.job).collect::<Vec<_>>();
    assert_eq!(first, jobs[..MAX_THUMBNAIL_BATCH_ENTRIES]);
    assert_eq!(second, jobs[MAX_THUMBNAIL_BATCH_ENTRIES..]);
}

#[test]
fn reconciliation_removes_obsolete_jobs_from_batch_progress() {
    let queue = TransferQueue::default();
    let first = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let second = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let obsolete = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    queue.reconcile(vec![queued_upload(first, TransferOrigin::Background), queued_upload(second, TransferOrigin::Background), queued_upload(obsolete, TransferOrigin::Background)]);

    let completed = queue.claim_due().unwrap();
    queue.complete(completed.id, None);
    queue.reconcile(vec![queued_upload(second, TransferOrigin::Background)]);

    let status = queue.statuses().pop().unwrap();
    assert_eq!((status.completed_items, status.total_items), (1, 2));
}

#[test]
fn thumbnails_are_claimed_before_user_requested_books() {
    let queue = TransferQueue::default();
    let thumbnail_hash = fixture_content_hash(1);
    let book_hash = fixture_content_hash(2);
    queue.reconcile(vec![
        TransferJob::download_blob(book_hash, db_placement(&BookPlacement::new(book_hash, RelativeBookPath::parse("/book.epub").unwrap())), TransferOrigin::UserInitiated),
        TransferJob::download_thumbnail(thumbnail_hash, TransferOrigin::Background),
    ]);

    let claimed = queue.claim_due().unwrap();
    assert!(matches!(claimed.job.operation, TransferOperation::DownloadThumbnail { .. }));
}

#[test]
fn ephemeral_queue_prioritizes_user_jobs_and_tracks_retries() {
    let queue = TransferQueue::default();
    let background = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let manual = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    queue.reconcile(vec![queued_upload(background, TransferOrigin::Background), queued_upload(manual, TransferOrigin::UserInitiated)]);

    let claimed = queue.claim_due().expect("user transfer has priority");
    assert_eq!(claimed.job.origin(), TransferOrigin::UserInitiated);
    assert!(queue.retry(claimed.id, "network unavailable".to_owned()).is_some());
    let retrying = queue.statuses().into_iter().find(|status| status.content_hash == ContentHash::new(manual)).unwrap();
    assert_eq!(retrying.state, TransferState::Retrying);
    assert_eq!(retrying.attempts, 1);
    assert_eq!(retrying.last_error.as_deref(), Some("network unavailable"));

    let background = queue.claim_due().expect("background transfer proceeds while the user job awaits retry");
    assert_eq!(background.job.origin(), TransferOrigin::Background);
}

#[test]
fn authentication_pause_does_not_consume_retry_budget() {
    let queue = TransferQueue::default();
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    queue.reconcile(vec![queued_upload(hash, TransferOrigin::Background)]);
    let rejected = queue.claim_due().unwrap();
    queue.release(std::slice::from_ref(&rejected));
    queue.pause_authentication();

    assert!(!queue.try_start_scheduler(), "authentication pauses the queue rather than changing a job");
    queue.resume_authentication();
    assert!(queue.try_start_scheduler());

    let retried = queue.claim_due().unwrap();
    assert_eq!(retried.id, rejected.id);
    assert_eq!(queue.statuses()[0].attempts, 0);
}

#[test]
fn only_one_scheduler_owns_the_queue() {
    let queue = TransferQueue::default();
    assert!(queue.try_start_scheduler());
    assert!(!queue.try_start_scheduler());
}

/// The upload path deliberately does not inspect book format; the library is
/// an inventory of the user's files, not an upload trust boundary. What it
/// must still refuse is bytes that do not match the content hash they claim
/// to be, and it must refuse them before any network I/O.
#[tokio::test]
async fn upload_rejects_bytes_that_do_not_match_their_content_hash_before_network_io() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let expected = ContentHash::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let (worker, _database) = worker_with_placement(root.path(), ServerUrl::parse("http://127.0.0.1:9").unwrap(), &expected);
    &_database.queue_book_upload(&expected, &expected, 1).unwrap();
    std::fs::write(root.path().join("Shelf/book.epub"), b"bytes that hash to something else").unwrap();

    let error = worker.upload_blob(&expected).await.unwrap_err();

    assert!(matches!(&error, TransferError::Rejected(_)), "unexpected error: {error}");
    assert_eq!(std::fs::read(root.path().join("Shelf/book.epub")).unwrap(), b"bytes that hash to something else");
}

#[tokio::test]
async fn upload_rejects_empty_books_without_network_io() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let hash = ContentHash::new(blake3::hash(&[]).to_hex().as_str());
    let (worker, _database) = worker_with_placement(root.path(), ServerUrl::parse("http://127.0.0.1:9").unwrap(), &hash);
    &_database.queue_book_upload(&hash, &hash, 1).unwrap();
    std::fs::write(root.path().join("Shelf/book.epub"), []).unwrap();

    let error = worker.upload_blob(&hash).await.unwrap_err();

    assert!(matches!(error, TransferError::Rejected(message) if message.to_string().contains("local book is empty")));
}

#[test]
fn only_an_active_upload_remains_retryable_after_admission() {
    use sync_common::api::assets::BlobManifestRejectionReason;

    assert!(matches!(TransferError::from_upload_admission(BlobManifestRejectionReason::UploadInProgress), TransferError::Retryable(_)));
    for reason in [BlobManifestRejectionReason::InvalidSize, BlobManifestRejectionReason::SizeMismatch, BlobManifestRejectionReason::IdentityMismatch, BlobManifestRejectionReason::QuotaExceeded] {
        assert!(matches!(TransferError::from_upload_admission(reason), TransferError::Rejected(_)));
    }
}

fn admit_uploads(app: Router) -> Router {
    app.route(
        &format!("/api/libraries/{}/blobs/negotiate", test_support::fixture_library_id("library")),
        axum::routing::post(|body: axum::body::Bytes| async move {
            let request: sync_common::api::assets::BlobManifestRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
            let response = sync_common::api::assets::BlobManifestResponse { upload: request.blobs.into_iter().map(|blob| blob.content_hash).collect(), ..Default::default() };
            ([("content-type", sync_common::transport::MEDIA_TYPE)], sync_common::transport::encode(&response).unwrap())
        }),
    )
}

#[tokio::test]
async fn batch_negotiation_completes_existing_revisions_without_local_file_reads() {
    use sync_common::api::assets::{BlobManifestRequest, BlobManifestResponse};
    let existing = fixture_content_hash(80);
    let checksum = fixture_content_hash(81);
    let missing = fixture_content_hash(82);
    let route = format!("/api/libraries/{}/blobs/negotiate", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        axum::routing::post(move |body: axum::body::Bytes| async move {
            let request: BlobManifestRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
            assert_eq!(request.blobs.len(), 2);
            assert_eq!(request.blobs[0].content_hash, existing);
            assert_eq!(request.blobs[0].checksum, checksum);
            let response = BlobManifestResponse { claimed: vec![existing], upload: vec![missing], ..Default::default() };
            ([("content-type", sync_common::transport::MEDIA_TYPE)], sync_common::transport::encode(&response).unwrap())
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = ServerUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), url, &existing);
    let pending = {
        pool.seed_book(&missing, None, 1, "epub");
        pool.observe_local_book(&existing, &checksum, 100).unwrap();
        pool.observe_local_book(&missing, &missing, 200).unwrap();
        let pending = pool.book_upload_intent(&missing).unwrap().unwrap();
        pending
    };
    // No book files exist. Negotiation must use captured checksums only.
    let intents = pool.book_upload_intents(&[existing, missing]).unwrap();
    let mut outcome = TransferOutcome::default();
    let prepared = worker.prepare_book_uploads(&intents, &mut outcome).await.unwrap();
    pool.commit_transfer_outcome(&outcome).unwrap();
    let expected = library_replica::BookUploadIntent { id: pending.id, content_hash: pending.content_hash, checksum: pending.checksum, size_bytes: pending.size_bytes };
    assert_eq!(prepared.jobs, vec![TransferJob::upload_book_intent(expected, true, TransferOrigin::Background)]);
    assert!(pool.book_upload_intent(&existing).unwrap().is_none());
    assert!(pool.book_upload_intent(&missing).unwrap().is_some());
    assert!(pool.remote_asset_hashes(BlobKind::Book).unwrap().contains(&existing));
    server.abort();
}

#[tokio::test]
async fn upload_reader_selects_the_exact_revision_among_multiple_placements() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("original.m4b");
    std::fs::write(&source, include_bytes!("../../../../../app/tests/fixtures/embedded-cover.m4b")).unwrap();
    let identity = book_identity::ensure(&source).unwrap();
    let original = std::fs::read(&source).unwrap();
    let mut edited = original.clone();
    edited.extend_from_slice(b"\0\0\0\x0cfreedata");
    let checksum = ContentHash::new(blake3::hash(&edited).to_hex().as_str());
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse("http://127.0.0.1:9").unwrap(), &identity);
    std::fs::write(root.path().join("Shelf/book.epub"), &original).unwrap();
    std::fs::write(root.path().join("edited.m4b"), &edited).unwrap();
    pool.add_book_placement(&ROOT_DIR_ID, &identity, "edited.m4b", "fp", true);
    let book = worker.assets.verified_book_version_reader(identity, Some(checksum)).await.unwrap().unwrap();
    assert_eq!(book.checksum, checksum);
    assert_eq!(book.length, edited.len() as u64);
    assert_eq!(std::fs::read(root.path().join("Shelf/book.epub")).unwrap(), original);
}

/// Fixture owner pair: the worker drives transfers through its own commit
/// connections while the test keeps the session handle for fixtures and
/// assertions. Both sides use methods on the owner; production paths only
/// touch the owner.
fn worker_with_placement(root: &Path, server_url: ServerUrl, content_hash: &ContentHash) -> (TransferWorker, library_database::Database) {
    // Placement rows name Shelf/ files; materialize the directory the way
    // placement recovery would so file reads resolve. The ownership marker
    // must match `d1` too, or later directory-work reconciliation (queued
    // whenever a placed book finishes downloading) sees an unmarked folder
    // and renames it aside instead of recognizing it as already correct.
    std::fs::create_dir_all(root.join("Shelf")).unwrap();
    std::fs::write(root.join("Shelf/.biblos_uuid"), test_support::fixture_dir_id("d1").to_string()).unwrap();
    let db_path = root.join(".bokheim/library.db");
    let database = library_database::Database::open(&db_path).unwrap();
    database.initialize_library().unwrap();
    {
        let dir_id = test_support::fixture_dir_id("d1");
        database.seed_dir(&dir_id, &ROOT_DIR_ID, "Shelf");
        database.seed_book(content_hash, None, 1, "epub");
        database.add_book_placement(&dir_id, content_hash, "book.epub", "", false);
        database.queue_directory_work(&ROOT_DIR_ID).unwrap();
    }
    let lookup_path = db_path.clone();
    let assets = library_files::asset_store::AssetStore::open(root.to_str().unwrap()).unwrap().with_book_paths(move |hash| {
        library_database::Database::book_paths_at(&lookup_path, hash).map_err(library_files::asset_store::AssetStoreError::operation)
    });
    let worker = TransferWorker {
        asset_storage_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        account: None,
        notification_interest: Default::default(),
        http_client: Client::new(),
        library_id: test_support::fixture_library_id("library"),
        assets,
        event_tx: None,
        credentials: test_credentials(&server_url),
        queue: Arc::new(TransferQueue::default()),
        availability_changed: Arc::default(),
        db_path,
        cpu: std::sync::Arc::new(client_platform_native::cpu_host::NativeHost::default()),
    };
    (worker, database)
}

#[tokio::test]
async fn complete_download_publishes_directly_and_the_reader_prefers_it_with_asset_storage_disabled() {
    let bytes = b"a complete epub payload stored by the common transfer worker".to_vec();
    let content_hash = ContentHash::new(&blake3::hash(&bytes).to_hex().as_str());
    let saw_request = Arc::new(AtomicBool::new(false));
    let route_bytes = Arc::new(bytes.clone());
    let route_saw_request = Arc::clone(&saw_request);
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        get(move |headers: HeaderMap| {
            let bytes = Arc::clone(&route_bytes);
            let saw_request = Arc::clone(&route_saw_request);
            async move {
                assert!(headers.get("range").is_none());
                saw_request.store(true, Ordering::SeqCst);
                Response::builder().status(StatusCode::OK).header("x-bokheim-content-checksum", blake3::hash(bytes.as_ref()).to_hex().as_str()).body(Body::from(bytes.as_ref().clone())).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (mut worker, _pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &content_hash);
    worker.asset_storage_enabled.store(false, Ordering::Release);
    _pool.add_book_placement(&ROOT_DIR_ID, &content_hash, "second.epub", "", false);
    let (event_tx, event_rx) = async_channel::unbounded();
    worker.event_tx = Some(event_tx);
    let placement = BookPlacement::new(content_hash, RelativeBookPath::parse("/Shelf/book.epub").unwrap());

    worker.download_blob(&content_hash, &db_placement(&placement)).await.unwrap();

    assert_eq!(std::fs::read(root.path().join("Shelf/book.epub")).unwrap(), bytes);
    assert!(!root.path().join(".bokheim/assets/book").exists());
    assert!(!root.path().join("second.epub").exists());
    assert_eq!(worker.assets.read(BlobKind::Book, &content_hash).unwrap().unwrap(), bytes);
    // This fixture inserts its remote folder directly; real remote folder
    // mutations queue their path reconciliation in the reducer.
    _pool.queue_directory_work(&ROOT_DIR_ID).unwrap();
    worker.assets.run_file_jobs(&_pool).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("Shelf/book.epub")).unwrap(), bytes);
    // "second.epub" has no local bytes and nothing materializes it; the
    // reader must still resolve to the downloaded placement instead of
    // failing on the empty one.
    assert!(!root.path().join("second.epub").exists());
    assert_eq!(_pool.local_book_info(&content_hash).unwrap().path.as_deref(), Some("/Shelf/book.epub"));
    assert!(saw_request.load(Ordering::SeqCst));
    assert_eq!(worker.assets.read(BlobKind::Book, &content_hash).unwrap().unwrap(), bytes);
    assert_eq!(_pool.thumbnail_work_state(&content_hash).unwrap().unwrap().0, "pending", "download completion must persist cover work");
    let states = std::iter::from_fn(|| event_rx.try_recv().ok())
        .filter_map(|event| match event {
            library_runtime::events::LibraryEvent::DownloadStatusChanged { content_hash: event_hash, state } if event_hash == content_hash => Some(state),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(states.first(), Some(DownloadState::Downloading(progress)) if *progress == DownloadProgress::ZERO));
    assert!(states.iter().any(|state| matches!(state, DownloadState::Downloading(progress) if progress.value() > 0.0)));
    assert!(matches!(states.last(), Some(DownloadState::Downloaded)));
    server.abort();
}

#[tokio::test]
async fn book_download_rejects_a_missing_transfer_checksum() {
    let bytes = b"valid raw bytes without a protocol checksum";
    let identity = ContentHash::new(blake3::hash(bytes).to_hex().as_str());
    let route = format!("/api/libraries/{}/blobs/{identity}", test_support::fixture_library_id("library"));
    let app = Router::new().route(&route, get(move || async move { bytes.as_slice() }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, _) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &identity);
    let placement = BookPlacement::new(identity, RelativeBookPath::parse("/Shelf/book.epub").unwrap());
    assert!(matches!(worker.download_blob(&identity, &db_placement(&placement)).await, Err(TransferError::Rejected(_))));
    assert!(!root.path().join("Shelf/book.epub").exists());
    server.abort();
}

#[tokio::test]
async fn book_revision_download_and_upload_verify_checksum_separately_from_identity() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("book.m4b");
    std::fs::write(&path, include_bytes!("../../../../../app/tests/fixtures/embedded-cover.m4b")).unwrap();
    let identity = book_identity::ensure(&path).unwrap();
    let bytes = std::fs::read(path).unwrap();
    let checksum = ContentHash::new(blake3::hash(&bytes).to_hex().as_str());
    assert_ne!(identity, checksum);
    let download_bytes = bytes.clone();
    let upload_bytes = bytes.clone();
    let route = format!("/api/libraries/{}/blobs/{identity}", test_support::fixture_library_id("library"));
    let uploaded = Arc::new(AtomicBool::new(false));
    let observed = uploaded.clone();
    let app = Router::new().route(
        &route,
        get(move || {
            let bytes = download_bytes.clone();
            async move { Response::builder().header("x-bokheim-content-checksum", checksum.as_str()).body(Body::from(bytes)).unwrap() }
        })
        .put(move |headers: HeaderMap, body: axum::body::Bytes| {
            let expected = upload_bytes.clone();
            let observed = observed.clone();
            async move {
                assert_eq!(headers.get("x-bokheim-content-checksum").unwrap(), checksum.as_str());
                assert_eq!(body.as_ref(), expected);
                observed.store(true, Ordering::SeqCst);
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &identity);
    let placement = BookPlacement::new(identity, RelativeBookPath::parse("/Shelf/book.epub").unwrap());
    worker.download_blob(&identity, &db_placement(&placement)).await.unwrap();
    assert_eq!(worker.assets.read(BlobKind::Book, &identity).unwrap().unwrap(), bytes);
    worker.upload_blob(&identity).await.unwrap();
    assert!(!uploaded.load(Ordering::SeqCst), "a completed download is not upload intent");
    &pool.queue_book_upload(&identity, &checksum, bytes.len() as u64);
    worker.upload_blob(&identity).await.unwrap();
    assert!(uploaded.load(Ordering::SeqCst));
    server.abort();
}

#[tokio::test]
async fn download_publishes_beside_an_unrelated_user_file() {
    let bytes = b"verified remote book".to_vec();
    let content_hash = ContentHash::new(blake3::hash(&bytes).to_hex().as_str());
    let route_bytes = Arc::new(bytes.clone());
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        get(move || {
            let bytes = Arc::clone(&route_bytes);
            async move { Response::builder().header("x-bokheim-content-checksum", blake3::hash(bytes.as_ref()).to_hex().as_str()).body(Body::from(bytes.as_ref().clone())).unwrap() }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &content_hash);
    std::fs::write(root.path().join("Shelf/book.epub"), b"unrelated user file").unwrap();
    let placement = BookPlacement::new(content_hash, RelativeBookPath::parse("/Shelf/book.epub").unwrap());
    pool.add_asset_request(BlobKind::Book, &content_hash, TransferOrigin::UserInitiated).unwrap();

    worker.download_blob(&content_hash, &db_placement(&placement)).await.unwrap();

    assert_eq!(std::fs::read(root.path().join("Shelf/book.epub")).unwrap(), b"unrelated user file");
    assert_eq!(std::fs::read(root.path().join("Shelf/book 2.epub")).unwrap(), bytes);
    assert!(!root.path().join(".bokheim/assets/book").exists());
    assert_eq!(worker.assets.read(BlobKind::Book, &content_hash).unwrap().unwrap(), bytes);
    assert!(pool.is_book_downloaded(&content_hash).unwrap());
    assert!(pool.asset_requests(BlobKind::Book).unwrap().is_empty());
    worker.assets.run_file_jobs(&pool).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("Shelf/book.epub")).unwrap(), b"unrelated user file");
    server.abort();
}

#[tokio::test]
async fn missing_remote_blob_resolves_durable_intent_instead_of_recreating_a_poison_job() {
    let content_hash = fixture_content_hash(41);
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(&route, get(|| async { StatusCode::NOT_FOUND }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &content_hash);
    let placement = BookPlacement::new(content_hash, RelativeBookPath::parse("/Shelf/book.epub").unwrap());
    {
        pool.add_asset_request(BlobKind::Book, &content_hash, TransferOrigin::UserInitiated).unwrap();
        pool.record_remote_asset(BlobKind::Book, &content_hash).unwrap();
    }

    worker.download_blob(&content_hash, &db_placement(&placement)).await.unwrap();

    assert!(pool.asset_requests(BlobKind::Book).unwrap().is_empty());
    assert!(!pool.remote_asset_hashes(BlobKind::Book).unwrap().contains(&content_hash));
    server.abort();
}

#[tokio::test]
async fn corrupt_remote_blob_invalidates_remote_fact_and_resolves_request() {
    let expected = fixture_content_hash(42);
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(&route, get(move || async move { Response::builder().header("x-bokheim-content-checksum", expected.as_str()).body(Body::from("bytes with the wrong content hash")).unwrap() }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &expected);
    let placement = BookPlacement::new(expected, RelativeBookPath::parse("/Shelf/book.epub").unwrap());
    {
        pool.add_asset_request(BlobKind::Book, &expected, TransferOrigin::UserInitiated).unwrap();
        pool.record_remote_asset(BlobKind::Book, &expected).unwrap();
    }

    let error = worker.download_blob(&expected, &db_placement(&placement)).await.unwrap_err();

    assert!(matches!(error, TransferError::Rejected(_)));
    assert!(pool.asset_requests(BlobKind::Book).unwrap().is_empty());
    assert!(!pool.remote_asset_hashes(BlobKind::Book).unwrap().contains(&expected));
    server.abort();
}

#[cfg(feature = "app-integration-tests")]
#[tokio::test]
async fn detached_transfers_refresh_once_and_keep_failures_in_the_queue() {
    #[derive(Clone, Debug)]
    struct Persistence;
    impl account_client::SessionPersistence for Persistence {
        fn load_account_session(&self) -> std::io::Result<Option<account_client::Session>> {
            Ok(None)
        }
        fn save_account_session(&self, _: &account_client::Session) -> std::io::Result<()> {
            Ok(())
        }
        fn clear_account_session(&self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // Successful refresh, revoked refresh, transient refresh failure, and an
    // access token rejected even after refresh all run without a sync caller.
    for (refresh_status, reject_again) in [(StatusCode::OK, false), (StatusCode::UNAUTHORIZED, false), (StatusCode::SERVICE_UNAVAILABLE, false), (StatusCode::OK, true)] {
        let requests = Arc::new(AtomicUsize::new(0));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let route_requests = requests.clone();
        let route_refreshes = refreshes.clone();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let route = format!("/api/libraries/{}/thumbnail-batch", test_support::fixture_library_id("library"));
        let app = Router::new()
            .route(
                &route,
                post(move |headers: HeaderMap| {
                    route_requests.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if reject_again || headers.get("authorization").unwrap().to_str().unwrap() == format!("Bearer {TEST_TOKEN}") {
                            StatusCode::UNAUTHORIZED
                        } else {
                            // A permanent asset rejection finishes this job and lets
                            // the test distinguish authentication success from retry.
                            StatusCode::BAD_REQUEST
                        }
                    }
                }),
            )
            .route(
                "/api/auth/refresh",
                post(move || {
                    route_refreshes.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let bytes = sync_common::transport::encode(&account_contract::AuthResponse {
                            token: "c".repeat(43),
                            expires_at: now + 3600,
                            refresh_token: "d".repeat(43),
                            refresh_expires_at: now + 7200,
                            user_id: "transfer-user".into(),
                            email: "reader@example.com".into(),
                        })
                        .unwrap();
                        Response::builder().status(refresh_status).header("content-type", sync_common::transport::MEDIA_TYPE).body(Body::from(bytes)).unwrap()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = ServerUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
        let hash = fixture_content_hash(75);
        let (mut worker, pool) = worker_with_placement(root.path(), url.clone(), &hash);
        let account = library_runtime::account::LibraryAccountSession::load(Persistence, url.clone()).unwrap();
        account.set(account_client::Session::try_new_with_tokens(url, "transfer-user".into(), "reader@example.com".into(), TEST_TOKEN.into(), Some(now + 3600), Some("r".repeat(43)), Some(now + 7200)).unwrap()).unwrap();
        worker.credentials = account.shared_sync_credentials();
        worker.account = Some(account.library_account());
        let (events, updates) = library_runtime::events::library_event_channel();
        worker.event_tx = Some(events);
        let generation = worker.queue.begin_reconcile();
        worker.clone().run_page(generation, vec![TransferJob::download_thumbnail(hash, TransferOrigin::Background)]);
        worker.clone().finish_plan(generation);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let settled = if refresh_status == StatusCode::SERVICE_UNAVAILABLE {
                    worker.queue.statuses().iter().any(|status| status.state == TransferState::Retrying)
                } else if refresh_status == StatusCode::UNAUTHORIZED || reject_again {
                    worker.queue.statuses().iter().any(|status| status.state == TransferState::Queued) && refreshes.load(Ordering::SeqCst) == 1 && (refresh_status != StatusCode::OK || requests.load(Ordering::SeqCst) == 2)
                } else {
                    worker.queue.statuses().is_empty()
                };
                if settled {
                    break;
                }
                updates.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(requests.load(Ordering::SeqCst), if refresh_status == StatusCode::OK { 2 } else { 1 });
        if refresh_status == StatusCode::UNAUTHORIZED {
            assert!(account.current().unwrap().is_none());
        }
        if refresh_status == StatusCode::UNAUTHORIZED || reject_again {
            assert!(!worker.queue.try_start_scheduler());
            assert_eq!(worker.queue.statuses()[0].attempts, 0);
        }
        // Retire any delayed retry before the temporary library is removed.
        worker.queue.reconcile(Vec::new());
        server.abort();
    }
}

#[tokio::test]
async fn unauthorized_transfer_pauses_the_scheduler_without_changing_the_job() {
    let route = format!("/api/libraries/{}/thumbnail-batch", test_support::fixture_library_id("library"));
    let app = Router::new().route(&route, post(|| async { StatusCode::UNAUTHORIZED }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let content_hash = ContentHash::new(&"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let (worker, _) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &content_hash);

    let job = TransferJob::download_thumbnail(content_hash, TransferOrigin::Background);
    let error = worker.clone().run(vec![job]).await.unwrap_err();

    assert!(matches!(error, TransferError::AuthenticationRequired));
    assert!(worker.credentials.credentials().is_some(), "the account layer must retain the refresh token so it can renew access before retrying");
    let status = worker.queue.statuses().pop().unwrap();
    assert_eq!(status.state, TransferState::Queued);
    assert_eq!(status.attempts, 0);
    assert!(!worker.queue.try_start_scheduler(), "the queue stays globally paused until authentication is explicitly resumed");
    let generation = worker.queue.begin_reconcile();
    worker.clone().run_page(generation, vec![TransferJob::download_thumbnail(content_hash, TransferOrigin::Background)]);
    worker.clone().finish_plan(generation);
    assert!(!worker.queue.try_start_scheduler(), "later planning pages cannot restart rejected authentication");
    server.abort();
}

#[tokio::test]
async fn thumbnail_batch_download_uses_one_request_for_multiple_hashes() {
    let first = fixture_content_hash(51);
    let second = fixture_content_hash(52);
    let local = fixture_content_hash(53);
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 3, image::Rgb([24, 96, 192]))).write_to(&mut jpeg, image::ImageOutputFormat::Jpeg(85)).unwrap();
    let jpeg = Arc::new(jpeg.into_inner());
    let request_count = Arc::new(AtomicUsize::new(0));
    let requested_hash_count = Arc::new(AtomicUsize::new(0));
    let route_request_count = Arc::clone(&request_count);
    let route_requested_hash_count = Arc::clone(&requested_hash_count);
    let route_jpeg = Arc::clone(&jpeg);
    let route = format!("/api/libraries/{}/thumbnail-batch", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        post(move |body: axum::body::Bytes| {
            let request_count = Arc::clone(&route_request_count);
            let requested_hash_count = Arc::clone(&route_requested_hash_count);
            let jpeg = Arc::clone(&route_jpeg);
            async move {
                let request: sync_common::api::assets::ThumbnailBatchDownloadRequest = sync_common::transport::decode(&body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).unwrap();
                request_count.fetch_add(1, Ordering::SeqCst);
                requested_hash_count.store(request.content_hashes.len(), Ordering::SeqCst);
                let thumbnails = request.content_hashes.into_iter().map(|content_hash| sync_common::api::assets::ThumbnailBatchDownloadEntry {
                    content_hash,
                    bytes: jpeg.as_ref().clone(),
                    browse_bytes: (content_hash == first).then(|| jpeg.as_ref().clone()),
                }).collect();
                let response = sync_common::api::assets::ThumbnailBatchDownloadResponse { thumbnails, missing: Vec::new() };
                Body::from(sync_common::transport::encode(&response).unwrap())
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (mut worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &first);
    pool.seed_book(&second, None, 1, "epub");

    pool.seed_book(&local, None, 1, "epub");
    worker.assets.write(BlobKind::Thumbnail, &local, &jpeg).await.unwrap();
    worker.assets.write(BlobKind::Thumbnail, &first, &[0xff, 0xd8, 0xff, 0x00]).await.unwrap();
    let (event_tx, event_rx) = async_channel::unbounded();
    worker.event_tx = Some(event_tx);
    worker
        .clone()
        .run(vec![TransferJob::download_thumbnail(first, TransferOrigin::Background), TransferJob::download_thumbnail(second, TransferOrigin::Background), TransferJob::download_thumbnail(local, TransferOrigin::Background)])
        .await
        .unwrap();

    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert_eq!(requested_hash_count.load(Ordering::SeqCst), 2, "only the corrupt and missing originals need downloading");
    assert!(worker.assets.thumbnail_set_exists(&local).unwrap());
    assert_eq!(worker.assets.read_bytes(BlobKind::Thumbnail, &local).await.unwrap().unwrap(), *jpeg);
    let stored = std::fs::read_dir(root.path().join(APP_HIDDEN_DIR).join("assets/thumbnail")).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();
    assert!(worker.assets.exists(BlobKind::Thumbnail, &first).unwrap(), "stored thumbnails: {stored:?}");
    assert!(worker.assets.exists(BlobKind::Thumbnail, &second).unwrap(), "stored thumbnails: {stored:?}");
    assert_eq!(worker.assets.read_thumbnail_variant(&first, thumbnail::BROWSE_THUMBNAIL_WIDTH).await.unwrap().unwrap(), *jpeg);
    assert!(worker.assets.read_thumbnail_variant(&second, thumbnail::BROWSE_THUMBNAIL_WIDTH).await.unwrap().is_some());
    let ready = std::iter::from_fn(|| event_rx.try_recv().ok())
        .filter_map(|event| match event {
            library_runtime::events::LibraryEvent::ThumbnailGenerated { content_hash } => Some(content_hash),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ready, std::collections::HashSet::from([first, second, local]));
    for hash in [first, second, local] {
        assert_eq!(pool.thumbnail_work_state(&hash).unwrap().unwrap().0, "ready");
    }
    assert!(!pool.has_pending_asset_work().unwrap(), "download completion must settle the pending-work check");
    server.abort();
}

#[tokio::test]
async fn completed_download_is_not_published_after_placement_becomes_stale() {
    let bytes = b"verified but stale bytes".to_vec();
    let content_hash = ContentHash::new(&blake3::hash(&bytes).to_hex().as_str());
    let route_bytes = Arc::new(bytes);
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        get(move || {
            let bytes = Arc::clone(&route_bytes);
            async move { Response::builder().status(StatusCode::OK).header("x-bokheim-content-checksum", blake3::hash(bytes.as_ref()).to_hex().as_str()).body(Body::from(bytes.as_ref().clone())).unwrap() }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &content_hash);
    pool.mark_all_placements_deleted();
    let placement = BookPlacement::new(fixture_content_hash(7), RelativeBookPath::parse("/Shelf/book.epub").unwrap());

    worker.download_blob(&content_hash, &db_placement(&placement)).await.unwrap();

    assert!(!root.path().join("Shelf/book.epub").exists());
    assert!(!worker.assets.exists(BlobKind::Book, &content_hash).unwrap());
    server.abort();
}

#[test]
fn failure_classification_is_closed_and_explicit() {
    assert!(matches!(TransferError::rejected("bad input"), TransferError::Rejected(_)));
    let io_error = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "offline");
    assert!(matches!(TransferError::from(io_error), TransferError::Retryable(_)));
}

#[tokio::test]
async fn download_applies_response_despite_account_switch() {
    for (status, corrupt) in [(StatusCode::OK, false), (StatusCode::NOT_FOUND, false), (StatusCode::OK, true), (StatusCode::FORBIDDEN, false)] {
        let bytes = b"account-bound book".to_vec();
        let hash = ContentHash::new(blake3::hash(&bytes).to_hex().as_str());
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let route_started = started.clone();
        let route_release = release.clone();
        let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
        let app = Router::new().route(
            &route,
            get(move || {
                let started = route_started.clone();
                let release = route_release.clone();
                let bytes = if corrupt { b"wrong hash".to_vec() } else { bytes.clone() };
                async move {
                    started.notify_one();
                    release.notified().await;
                    Response::builder().status(status).header("x-bokheim-content-checksum", blake3::hash(&bytes).to_hex().as_str()).body(Body::from(bytes)).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
        let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &hash);
        let placement = BookPlacement::new(hash, RelativeBookPath::parse("/Shelf/book.epub").unwrap());
        let change_account = async {
            started.notified().await;
            release.notify_one();
        };
        let transfer_placement = db_placement(&placement);
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(worker.download_blob(&hash, &transfer_placement), change_account) }).await.unwrap();
        match (status, corrupt) {
            (StatusCode::OK, false) => {
                result.unwrap();
                assert!(worker.assets.exists(BlobKind::Book, &hash).unwrap());
                assert!(pool.is_book_downloaded(&hash).unwrap());
            }
            (StatusCode::NOT_FOUND, _) => {
                result.unwrap();
                assert!(!worker.assets.exists(BlobKind::Book, &hash).unwrap());
                assert!(!pool.is_book_downloaded(&hash).unwrap());
            }
            (_, _) => assert!(matches!(result, Err(TransferError::Rejected(_))), "{result:?}"),
        }
        server.abort();
    }
}

#[tokio::test]
async fn upload_applies_response_despite_account_switch() {
    for status in [StatusCode::OK, StatusCode::FORBIDDEN] {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let route_started = started.clone();
        let route_release = release.clone();
        let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
        let app = Router::new().route(
            &route,
            put(move || {
                let started = route_started.clone();
                let release = route_release.clone();
                async move {
                    started.notify_one();
                    release.notified().await;
                    Response::builder().status(status).body(Body::empty()).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
        let bytes = b"account-bound upload";
        let hash = ContentHash::new(blake3::hash(bytes).to_hex().as_str());
        let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &hash);
        std::fs::write(root.path().join("Shelf/book.epub"), bytes).unwrap();
        &pool.queue_book_upload(&hash, &hash, bytes.len() as u64);
        let change_account = async {
            started.notified().await;
            release.notify_one();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(worker.upload_blob(&hash), change_account) }).await.unwrap();
        if status == StatusCode::OK {
            result.unwrap();
            assert!(pool.remote_asset_hashes(BlobKind::Book).unwrap().contains(&hash));
        } else {
            assert!(matches!(result, Err(TransferError::Rejected(_))), "{result:?}");
            assert!(pool.remote_asset_hashes(BlobKind::Book).unwrap().is_empty());
        }
        assert!(pool.rejected_asset_upload_hashes(BlobKind::Book).unwrap().is_empty());
        server.abort();
    }
}

#[tokio::test]
async fn a_scan_during_upload_keeps_the_new_version_queued() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let route_started = started.clone();
    let route_release = release.clone();
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        put(move || {
            let started = route_started.clone();
            let release = route_release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let bytes = b"upload version race";
    let hash = ContentHash::new(blake3::hash(bytes).to_hex().as_str());
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &hash);
    std::fs::write(root.path().join("Shelf/book.epub"), bytes).unwrap();
    let change_version = async {
        started.notified().await;
        pool.touch_all_placement_hashes();
        release.notify_one();
    };
    &pool.queue_book_upload(&hash, &hash, bytes.len() as u64);
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(worker.upload_blob(&hash), change_version) }).await.unwrap();
    assert!(matches!(result, Err(TransferError::Retryable(_))), "{result:?}");
    assert!(pool.remote_asset_hashes(BlobKind::Book).unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn permanent_http_asset_errors_are_not_retried() {
    let app = Router::new().route("/rejected", get(|| async { StatusCode::FORBIDDEN }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });

    let response = reqwest::Client::new().get(format!("http://{address}/rejected")).send().await.unwrap();
    let error = crate::validate_streaming_asset_response(response).unwrap_err();
    assert!(matches!(TransferError::from(error), TransferError::Rejected(_)));
    server.abort();
}

#[tokio::test]
async fn rejected_book_download_resolves_durable_request() {
    let content_hash = fixture_content_hash(61);
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(&route, get(|| async { StatusCode::FORBIDDEN }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &content_hash);
    pool.add_asset_request(BlobKind::Book, &content_hash, TransferOrigin::UserInitiated).unwrap();
    let placement = BookPlacement::new(content_hash, RelativeBookPath::parse("/Shelf/book.epub").unwrap());

    worker.clone().run(vec![TransferJob::download_blob(content_hash, db_placement(&placement), TransferOrigin::UserInitiated)]).await.unwrap();

    assert!(pool.asset_requests(BlobKind::Book).unwrap().is_empty());
    assert!(worker.queue.statuses().is_empty());
    assert!(matches!(worker.queue.history().first().map(|status| status.state), Some(TransferState::Failed)));
    server.abort();
}

#[tokio::test]
async fn rejected_book_upload_is_durably_suppressed() {
    let bytes = b"valid local book for rejected upload";
    let content_hash = ContentHash::new(blake3::hash(bytes).to_hex().as_str());
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(&route, put(|| async { StatusCode::FORBIDDEN }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &content_hash);
    std::fs::write(root.path().join("Shelf/book.epub"), bytes).unwrap();

    &pool.queue_book_upload(&content_hash, &content_hash, bytes.len() as u64);
    worker.clone().run(vec![TransferJob::upload_blob(content_hash, TransferOrigin::Background)]).await.unwrap();

    assert!(pool.rejected_asset_upload_hashes(BlobKind::Book).unwrap().contains(&content_hash));
    assert!(matches!(worker.queue.history().first().map(|status| status.state), Some(TransferState::Failed)));
    server.abort();
}

#[tokio::test]
async fn rejected_thumbnail_upload_does_not_suppress_its_peers() {
    let rejected = fixture_content_hash(71);
    let accepted = fixture_content_hash(72);
    let route = format!("/api/libraries/{}/thumbnail-batch", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        put(move |body: axum::body::Bytes| async move {
            let request: sync_common::api::assets::ThumbnailBatchUploadRequest = sync_common::transport::decode(&body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).unwrap();
            assert_eq!(request.thumbnails.len(), 1);
            if request.thumbnails[0].content_hash == rejected {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &accepted);
    {
        pool.seed_book(&rejected, None, 1, "epub");
        pool.complete_thumbnail_work(&rejected, true).unwrap();
        pool.complete_thumbnail_work(&accepted, true).unwrap();
    }
    worker.assets.write(BlobKind::Thumbnail, &rejected, &[0xff, 0xd8, 0xff, 1]).await.unwrap();
    worker.assets.write(BlobKind::Thumbnail, &accepted, &[0xff, 0xd8, 0xff, 2]).await.unwrap();

    worker.clone().run(vec![TransferJob::upload_thumbnail(rejected, TransferOrigin::Background), TransferJob::upload_thumbnail(accepted, TransferOrigin::Background)]).await.unwrap();

    assert!(pool.rejected_asset_upload_hashes(BlobKind::Thumbnail).unwrap().contains(&rejected));
    assert!(pool.remote_asset_hashes(BlobKind::Thumbnail).unwrap().contains(&accepted));
    assert!(!pool.rejected_asset_upload_hashes(BlobKind::Thumbnail).unwrap().contains(&accepted));
    server.abort();
}

#[tokio::test]
async fn invalid_local_thumbnail_does_not_create_upload_suppression() {
    let content_hash = fixture_content_hash(73);
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse("http://127.0.0.1:1").unwrap(), &content_hash);
    worker.assets.write(BlobKind::Thumbnail, &content_hash, b"not a jpeg").await.unwrap();

    worker.clone().run(vec![TransferJob::upload_thumbnail(content_hash, TransferOrigin::Background)]).await.unwrap();

    assert!(!worker.assets.exists(BlobKind::Thumbnail, &content_hash).unwrap());
    assert!(pool.rejected_asset_upload_hashes(BlobKind::Thumbnail).unwrap().is_empty());
}

#[tokio::test]
async fn download_rechecks_placement_after_waiting_for_book_lease() {
    let bytes = b"book awaiting a lease".to_vec();
    let hash = ContentHash::new(blake3::hash(&bytes).to_hex().as_str());
    let route_bytes = bytes.clone();
    let route = format!("/api/libraries/{}/blobs/:hash", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        get(move || {
            let bytes = route_bytes.clone();
            async move { Response::builder().status(StatusCode::OK).header("x-bokheim-content-checksum", blake3::hash(&bytes).to_hex().as_str()).body(Body::from(bytes)).unwrap() }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, admit_uploads(app)).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &hash);
    std::fs::write(root.path().join("Shelf/book.epub"), &bytes).unwrap();
    let pinned = worker.assets.verified_book_reader(hash).await.unwrap().unwrap();
    let placement = BookPlacement::new(hash, RelativeBookPath::parse("/Shelf/book.epub").unwrap());
    let transfer_placement = db_placement(&placement);
    let download = worker.download_blob(&hash, &transfer_placement);
    tokio::pin!(download);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(100), &mut download).await.is_err());
    pool.stale_all_placements();
    drop(pinned);
    download.await.unwrap();
    let downloaded: i64 = pool.downloaded_placement_total();
    assert_eq!(downloaded, 0);
    assert!(!pool.is_book_downloaded(&hash).unwrap());
    assert!(!pool.remote_asset_hashes(BlobKind::Book).unwrap().contains(&hash));
    server.abort();
}

#[tokio::test]
async fn upload_keeps_verified_reader_across_negotiation() {
    use sync_common::api::assets::{BlobManifestRequest, BlobManifestResponse};
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let bytes = b"verified before negotiation";
    let hash = ContentHash::new(blake3::hash(bytes).to_hex().as_str());
    let source = root.path().join("Shelf/book.epub");
    let original = source.clone();
    let negotiated = Arc::new(AtomicBool::new(false));
    let observed = negotiated.clone();
    let upload_observed = negotiated.clone();
    let route = format!("/api/libraries/{}/blobs", test_support::fixture_library_id("library"));
    let app = Router::new()
        .route(
            &format!("{route}/negotiate"),
            post(move |body: axum::body::Bytes| {
                let original = original.clone();
                let observed = observed.clone();
                async move {
                    let request: BlobManifestRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
                    assert_eq!(request.blobs[0].content_hash, hash);
                    assert_eq!(request.blobs[0].size_bytes, bytes.len() as u64);
                    // Replacing the pathname must not make upload reopen a different file.
                    let replacement = original.with_extension("replacement");
                    std::fs::write(&replacement, b"different bytes").unwrap();
                    std::fs::rename(replacement, original).unwrap();
                    observed.store(true, Ordering::SeqCst);
                    let response = BlobManifestResponse { upload: vec![hash], ..Default::default() };
                    ([("content-type", sync_common::transport::MEDIA_TYPE)], sync_common::transport::encode(&response).unwrap())
                }
            }),
        )
        .route(
            &format!("{route}/{hash}"),
            put(move |headers: HeaderMap, body: axum::body::Bytes| async move {
                assert!(upload_observed.load(Ordering::SeqCst));
                assert_eq!(headers.get("x-bokheim-content-checksum").unwrap(), hash.as_str());
                assert_eq!(body.as_ref(), bytes);
                StatusCode::OK
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &hash);
    std::fs::write(source, bytes).unwrap();
    &pool.queue_book_upload(&hash, &hash, bytes.len() as u64);
    worker.upload_blob(&hash).await.unwrap();
    assert!(negotiated.load(Ordering::SeqCst));
    server.abort();
}

#[tokio::test]
async fn missing_browse_thumbnail_is_repaired_without_credentials_or_network() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let hash = fixture_content_hash(54);
    let (mut worker, pool) = worker_with_placement(root.path(), ServerUrl::parse("http://127.0.0.1:9").unwrap(), &hash);
    let no_credentials: library_runtime::account::SharedSyncCredentials = Arc::new(std::sync::RwLock::new(None));
    worker.credentials = no_credentials;
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 3, image::Rgb([24, 96, 192]))).write_to(&mut jpeg, image::ImageOutputFormat::Jpeg(85)).unwrap();
    let jpeg = jpeg.into_inner();
    worker.assets.write(BlobKind::Thumbnail, &hash, &jpeg).await.unwrap();
    worker.clone().run(vec![TransferJob::download_thumbnail(hash, TransferOrigin::Background)]).await.unwrap();
    assert!(worker.assets.thumbnail_set_exists(&hash).unwrap());
    assert_eq!(worker.assets.read_bytes(BlobKind::Thumbnail, &hash).await.unwrap().unwrap(), jpeg);
    let browse = worker.assets.read_thumbnail_variant(&hash, thumbnail::BROWSE_THUMBNAIL_WIDTH).await.unwrap().unwrap();
    assert_eq!(image::load_from_memory(&browse).unwrap().width(), thumbnail::BROWSE_THUMBNAIL_WIDTH);
    assert!(!pool.remote_asset_hashes(BlobKind::Thumbnail).unwrap().contains(&hash), "local repair must not invent remote presence");
}

#[tokio::test]
async fn thumbnail_availability_only_queues_missing_locally_authored_covers() {
    use sync_common::api::assets::{ThumbnailBatchDownloadRequest, ThumbnailBatchDownloadResponse, ThumbnailBatchDownloadEntry};
    let hash = fixture_content_hash(91);
    let mode = Arc::new(AtomicUsize::new(0));
    let response_mode = mode.clone();
    let route = format!("/api/libraries/{}/thumbnail-batch", test_support::fixture_library_id("library"));
    let app = Router::new().route(
        &route,
        post(move |body: axum::body::Bytes| {
            let mode = response_mode.load(Ordering::SeqCst);
            async move {
                let request: ThumbnailBatchDownloadRequest = sync_common::transport::decode(&body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).unwrap();
                assert_eq!(request.content_hashes, vec![hash]);
                let response = match mode {
                    0 => ThumbnailBatchDownloadResponse { thumbnails: vec![ThumbnailBatchDownloadEntry { content_hash: hash, bytes: vec![0xff, 0xd8, 0xff], browse_bytes: None }], missing: vec![] },
                    1 => ThumbnailBatchDownloadResponse { thumbnails: vec![], missing: vec![hash] },
                    _ => ThumbnailBatchDownloadResponse { thumbnails: vec![], missing: vec![] },
                };
                sync_common::transport::encode(&response).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &hash);
    // A cover becomes uploadable after its parent book has reached the
    // server. Its local upload intent has already been consumed by then.
    pool.record_remote_asset(BlobKind::Book, &hash).unwrap();
    library_database::open_fixture_connection(pool.path()).unwrap().execute("DELETE FROM sync_outbox WHERE state_kind='book_lifecycle' AND state_key=?1", [hash.as_str()]).unwrap();
    pool.complete_thumbnail_work(&hash, true).unwrap();
    // Planning negotiates over a snapshot and the session commits before
    // asserting, exactly like the production planning pass.
    let prepared = async {
        let snapshot = pool.transfer_snapshot(&[hash]).unwrap();
        let mut outcome = TransferOutcome::default();
        let jobs = worker.prepare_thumbnail_uploads(&snapshot, &[hash], &mut outcome).await?;
        pool.commit_transfer_outcome(&outcome).unwrap();
        Ok::<_, TransferError>(jobs)
    };
    assert!(prepared.await.unwrap().is_empty(), "existing server covers must not create upload activity");
    assert!(pool.remote_asset_hashes(BlobKind::Thumbnail).unwrap().contains(&hash));
    pool.forget_remote_asset(BlobKind::Thumbnail, &hash).unwrap();
    mode.store(1, Ordering::SeqCst);
    let prepared = async {
        let snapshot = pool.transfer_snapshot(&[hash]).unwrap();
        let mut outcome = TransferOutcome::default();
        let jobs = worker.prepare_thumbnail_uploads(&snapshot, &[hash], &mut outcome).await?;
        pool.commit_transfer_outcome(&outcome).unwrap();
        Ok::<_, TransferError>(jobs)
    };
    assert_eq!(prepared.await.unwrap().len(), 1);
    pool.complete_downloaded_thumbnail(&hash).unwrap();
    let prepared = async {
        let snapshot = pool.transfer_snapshot(&[hash]).unwrap();
        let mut outcome = TransferOutcome::default();
        let jobs = worker.prepare_thumbnail_uploads(&snapshot, &[hash], &mut outcome).await?;
        pool.commit_transfer_outcome(&outcome).unwrap();
        Ok::<_, TransferError>(jobs)
    };
    assert!(prepared.await.unwrap().is_empty(), "downloaded bytes are never local upload intent");
    mode.store(2, Ordering::SeqCst);
    let prepared = async {
        let snapshot = pool.transfer_snapshot(&[hash]).unwrap();
        let mut outcome = TransferOutcome::default();
        let jobs = worker.prepare_thumbnail_uploads(&snapshot, &[hash], &mut outcome).await?;
        pool.commit_transfer_outcome(&outcome).unwrap();
        Ok::<_, TransferError>(jobs)
    };
    assert!(prepared.await.is_err(), "unknown availability is not permission to upload");
    server.abort();
}

#[tokio::test]
async fn book_batch_streams_multiple_files_and_preserves_per_book_outcomes() {
    use sync_common::book_batch::{UploadResponse, UploadResult};
    for mode in 0..3 {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
        let payloads = vec![vec![b'a'; 100_000], vec![b'b'; 100_000]];
        let identities = payloads.iter().map(|bytes| ContentHash::new(blake3::hash(bytes).to_hex().as_str())).collect::<Vec<_>>();
        let requests = Arc::new(AtomicUsize::new(0));
        let singles = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let expected = payloads.clone();
        let single_count = singles.clone();
        let library = test_support::fixture_library_id("library");
        let app = Router::new()
            .route(
                &format!("/api/libraries/{library}/book-batch"),
                put(move |headers: HeaderMap, body: axum::body::Bytes| {
                    let observed = observed.clone();
                    let expected = expected.clone();
                    async move {
                        observed.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(headers.get("content-type").unwrap(), sync_common::book_batch::MEDIA_TYPE);
                        let size = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
                        let manifest: sync_common::api::assets::BlobManifestRequest = sync_common::transport::decode(&body[4..4 + size], 16 * 1024).unwrap();
                        assert_eq!(manifest.blobs.len(), 2);
                        let mut offset = 4 + size;
                        for (entry, bytes) in manifest.blobs.iter().zip(expected) {
                            assert_eq!(&body[offset..offset + bytes.len()], bytes);
                            assert_eq!(entry.size_bytes, bytes.len() as u64);
                            offset += bytes.len();
                        }
                        assert_eq!(offset, body.len());
                        if mode == 2 {
                            return Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap();
                        }
                        let mut results: Vec<_> = manifest.blobs.iter().enumerate().map(|(i, e)| UploadResult { content_hash: e.content_hash, status: if i == 0 { 200 } else { 503 } }).collect();
                        if mode == 1 {
                            results[1].content_hash = results[0].content_hash;
                        }
                        Response::builder().header("content-type", sync_common::transport::MEDIA_TYPE).body(Body::from(sync_common::transport::encode(&UploadResponse { results }).unwrap())).unwrap()
                    }
                }),
            )
            .route(
                &format!("/api/libraries/{library}/blobs/:hash"),
                put(move |_: axum::body::Bytes| {
                    let count = single_count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        StatusCode::OK
                    }
                }),
            );
        // An old server rejects the route before consuming the body.
        let app = if mode == 2 {
            let count = requests.clone();
            app.layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
                let count = count.clone();
                async move {
                    if request.uri().path().ends_with("/book-batch") {
                        count.fetch_add(1, Ordering::SeqCst);
                        return Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap();
                    }
                    next.run(request).await
                }
            }))
        } else {
            app
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &identities[0]);
        for (name, bytes) in ["book.epub", "second.epub"].into_iter().zip(&payloads) {
            std::fs::write(root.path().join("Shelf").join(name), bytes).unwrap();
        }
        {
            pool.seed_book(&identities[1], None, 1, "epub");
            pool.add_book_placement(&test_support::fixture_dir_id("d1"), &identities[1], "second.epub", "", false);
        }
        let mut claimed = Vec::new();
        for (i, (&hash, bytes)) in identities.iter().zip(&payloads).enumerate() {
            let checksum = ContentHash::new(blake3::hash(bytes).to_hex().as_str());
            &pool.queue_book_upload(&hash, &checksum, bytes.len() as u64);
            let intent = pool.book_upload_intent(&hash).unwrap().unwrap();
            let intent = library_replica::BookUploadIntent { id: intent.id, content_hash: intent.content_hash, checksum: intent.checksum, size_bytes: intent.size_bytes };
            claimed.push(crate::ClaimedTransferJob { id: i as u64, job: TransferJob::upload_book_intent(intent, true, TransferOrigin::Background) });
        }
        let hashes: Vec<_> = claimed.iter().map(|item| *item.job.status_fields().1).collect();
        let snapshot = pool.transfer_snapshot(&hashes).unwrap();
        let mut outcome = TransferOutcome::default();
        let result = worker.upload_claimed_books_with_refresh(&snapshot, &claimed, &mut outcome).await;
        pool.commit_transfer_outcome(&outcome).unwrap();
        let remote = pool.remote_asset_hashes(BlobKind::Book).unwrap();
        if mode == 2 {
            assert!((1..=2).contains(&requests.load(Ordering::SeqCst)), "mode={mode} result={result:?}");
        } else {
            assert_eq!(requests.load(Ordering::SeqCst), 1, "mode={mode} result={result:?}");
        }
        match mode {
            0 => {
                let results = result.unwrap();
                assert!(results[0].is_ok());
                assert!(matches!(results[1], Err(TransferError::Retryable(_))));
                assert!(remote.contains(&identities[0]));
                assert!(!remote.contains(&identities[1]));
                assert!(pool.book_upload_intent(&identities[0]).unwrap().is_none());
                assert!(pool.book_upload_intent(&identities[1]).unwrap().is_some());
            }
            1 => {
                assert!(result.is_err());
                assert!(remote.is_empty());
            }
            _ => {
                assert!(result.unwrap().iter().all(Result::is_ok));
                assert_eq!(remote.len(), 2);
                assert_eq!(singles.load(Ordering::SeqCst), 2);
            }
        }
        server.abort();
    }
}

#[tokio::test]
async fn large_book_uploads_run_together_and_commit_without_waiting_for_drain() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let size = sync_common::book_batch::MAX_BOOK_BYTES as usize + 1;
    let payloads = [vec![b'a'; size], vec![b'b'; size]];
    let hashes = payloads.iter().map(|bytes| ContentHash::new(blake3::hash(bytes).to_hex().as_str())).collect::<Vec<_>>();
    let rendezvous = Arc::new(tokio::sync::Barrier::new(2));
    let library = test_support::fixture_library_id("library");
    let app = Router::new()
        .route(
            &format!("/api/libraries/{library}/blobs/:hash"),
            put(move |_: axum::body::Bytes| {
                let rendezvous = rendezvous.clone();
                async move {
                    rendezvous.wait().await;
                    StatusCode::OK
                }
            }),
        )
        .layer(axum::extract::DefaultBodyLimit::disable());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse(&format!("http://{address}")).unwrap(), &hashes[0]);
    for (name, bytes) in ["book.epub", "second.epub"].into_iter().zip(&payloads) {
        std::fs::write(root.path().join("Shelf").join(name), bytes).unwrap();
    }
    pool.seed_book(&hashes[1], None, 1, "epub");
    pool.add_book_placement(&test_support::fixture_dir_id("d1"), &hashes[1], "second.epub", "", false);
    let claimed = hashes
        .iter()
        .zip(&payloads)
        .enumerate()
        .map(|(index, (hash, bytes))| {
            pool.queue_book_upload(hash, hash, bytes.len() as u64).unwrap();
            let intent = pool.book_upload_intent(hash).unwrap().unwrap();
            let intent = library_replica::BookUploadIntent { id: intent.id, content_hash: intent.content_hash, checksum: intent.checksum, size_bytes: intent.size_bytes };
            crate::ClaimedTransferJob { id: index as u64, job: TransferJob::upload_book_intent(intent, true, TransferOrigin::Background) }
        })
        .collect::<Vec<_>>();
    let snapshot = pool.transfer_snapshot(&hashes).unwrap();
    let mut outcome = TransferOutcome::default();
    let results = tokio::time::timeout(std::time::Duration::from_secs(20), worker.upload_claimed_books_with_refresh(&snapshot, &claimed, &mut outcome)).await.unwrap().unwrap();
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert!(outcome.writes.is_empty(), "large books commit individually");
    assert_eq!(pool.remote_asset_hashes(BlobKind::Book).unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn disabled_asset_storage_preserves_upload_intents_and_skips_network() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(APP_HIDDEN_DIR)).unwrap();
    let hash = fixture_content_hash(901);
    let (worker, pool) = worker_with_placement(root.path(), ServerUrl::parse("http://127.0.0.1:9").unwrap(), &hash);
    &pool.queue_book_upload(&hash, &hash, 1).unwrap();
    let before = pool.book_upload_intent(&hash).unwrap();
    worker.asset_storage_enabled.store(false, Ordering::Release);
    let intents = pool.book_upload_intents(&[hash]).unwrap();
    assert!(worker.prepare_book_uploads(&intents, &mut TransferOutcome::default()).await.unwrap().jobs.is_empty());
    let snapshot = pool.transfer_snapshot(&[hash]).unwrap();
    assert!(worker.prepare_thumbnail_uploads(&snapshot, &[hash], &mut TransferOutcome::default()).await.unwrap().is_empty());
    worker.upload_blob(&hash).await.unwrap();
    worker.handle_job(TransferJob::upload_thumbnail(hash, TransferOrigin::Background)).await.unwrap();
    assert_eq!(pool.book_upload_intent(&hash).unwrap(), before);
    worker.asset_storage_enabled.store(true, Ordering::Release);
    let intents = pool.book_upload_intents(&[hash]).unwrap();
    assert!(worker.prepare_book_uploads(&intents, &mut TransferOutcome::default()).await.is_err(), "reenabling resumes negotiation against the unavailable test endpoint");
}
