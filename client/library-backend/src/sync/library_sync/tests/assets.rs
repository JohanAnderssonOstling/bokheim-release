use super::*;

/// Whether any device-local reconciliation is still queued: a directory move
/// awaiting physical materialization, a book awaiting follow-up file work,
/// or a file awaiting a trash/restore operation.
fn has_pending_file_work(database: &library_database::Database) -> bool {
    !database.native_directory_work_ids().unwrap().is_empty() || !database.native_book_work_directory_ids().unwrap().is_empty() || !database.native_file_work().unwrap().is_empty()
}

#[test]
fn only_new_path_changes_request_file_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let manager = test_manager(root.path());
    let database = &manager.database;
    let position = pulled_change(1, reading_change(1, 1, "epubcfi(/6/2)", 10).body);
    let cursor = database.sync_pull_cursor().unwrap();
    database.sync_commit_pull_response(&[], &sync_common::PullStateResponse { book_creations: Vec::new(), mutations: vec![position], next_cursor: cursor, has_more: false }).unwrap();
    for body in
        [MutationBody::DirectoryParent { dir_id: fixture_dir_id("moved"), parent_id: crate::ROOT_DIR_ID }, MutationBody::DirectoryLifecycle { dir_id: fixture_dir_id("moved"), value: library_replica::DirectoryLifecycleState::Present }]
    {
        let cursor = database.sync_pull_cursor().unwrap();
        database.sync_commit_pull_response(&[], &sync_common::PullStateResponse { book_creations: Vec::new(), mutations: vec![pulled_change(2, body)], next_cursor: cursor, has_more: false }).unwrap();
    }
    let moved = pulled_change(2, MutationBody::DirectoryName { dir_id: fixture_dir_id("moved"), name: "Shelf".into() });
    let cursor = database.sync_pull_cursor().unwrap();
    database.sync_commit_pull_response(&[], &sync_common::PullStateResponse { book_creations: Vec::new(), mutations: vec![moved.clone()], next_cursor: cursor, has_more: false }).unwrap();
    assert_eq!(database.native_directory_work_ids().unwrap(), vec![fixture_dir_id("moved")]);
    database.clear_local_directory_work();
    let cursor = database.sync_pull_cursor().unwrap();
    database.sync_commit_pull_response(&[], &sync_common::PullStateResponse { book_creations: Vec::new(), mutations: vec![moved], next_cursor: cursor, has_more: false }).unwrap();
    assert!(!has_pending_file_work(database), "an already applied change must not request another walk");
    let hash = fixture_content_hash(97);
    let placement = pulled_change(3, MutationBody::Placement { dir_id: crate::ROOT_DIR_ID, content_hash: hash, present: true, origin_folder_id: None });
    let cursor = database.sync_pull_cursor().unwrap();
    database.sync_commit_pull_response(&[], &sync_common::PullStateResponse { book_creations: vec![pulled_change(1, added(97, 1))], mutations: vec![placement], next_cursor: cursor, has_more: false }).unwrap();
    assert_eq!(database.local_book_work_hashes().unwrap(), vec![hash]);
    assert_eq!(database.native_directory_work_ids().unwrap().len(), 0, "a root placement must not reconcile the whole library");
}

#[test]
fn local_coverless_books_do_not_probe_remote_thumbnails() {
    assert!(!should_download_thumbnail(true, false, false, false));
    assert!(should_download_thumbnail(true, false, true, false));
    assert!(should_download_thumbnail(false, false, false, false));
    assert!(!should_download_thumbnail(false, true, true, false));
    assert!(!should_download_thumbnail(false, false, true, true));
}

#[tokio::test]
async fn changed_and_removed_sidecar_covers_reach_another_replica() {
    use axum::{extract::State, http::StatusCode, routing::{delete, post, put}, Router};
    use std::sync::{Arc, Mutex};
    use sync_common::api::assets::{ThumbnailBatchDownloadEntry, ThumbnailBatchDownloadRequest, ThumbnailBatchDownloadResponse, ThumbnailBatchUploadRequest, ThumbnailPresenceRequest, ThumbnailPresenceResponse, ThumbnailRevision};

    type Cover = Arc<Mutex<Option<Vec<u8>>>>;
    async fn upload(State(cover): State<Cover>, body: axum::body::Bytes) -> StatusCode {
        let request: ThumbnailBatchUploadRequest = sync_common::transport::decode(&body, sync_common::api::assets::MAX_THUMBNAIL_BATCH_BYTES + 1024 * 1024).unwrap();
        *cover.lock().unwrap() = Some(request.thumbnails.into_iter().next().unwrap().bytes);
        StatusCode::OK
    }
    async fn remove(State(cover): State<Cover>) -> StatusCode {
        *cover.lock().unwrap() = None;
        StatusCode::NO_CONTENT
    }
    async fn download(State(cover): State<Cover>, body: axum::body::Bytes) -> axum::response::Response {
        let request: ThumbnailBatchDownloadRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
        let cover = cover.lock().unwrap();
        let thumbnails = cover.as_ref().map(|bytes| request.content_hashes.iter().map(|hash| ThumbnailBatchDownloadEntry {
            content_hash: *hash, bytes: bytes.clone(), browse_bytes: Some(bytes.clone()),
        }).collect::<Vec<_>>()).unwrap_or_default();
        let missing = if cover.is_none() { request.content_hashes } else { Vec::new() };
        binary_sync_response(&ThumbnailBatchDownloadResponse { thumbnails, missing })
    }
    async fn presence(State(cover): State<Cover>, body: axum::body::Bytes) -> axum::response::Response {
        let request: ThumbnailPresenceRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
        let cover = cover.lock().unwrap();
        let revisions = cover.as_ref().map(|bytes| request.content_hashes.iter().map(|hash| ThumbnailRevision {
            content_hash: *hash, revision: ContentHash::new(blake3::hash(bytes).to_hex().as_str()),
        }).collect::<Vec<_>>()).unwrap_or_default();
        binary_sync_response(&ThumbnailPresenceResponse { present: revisions.iter().map(|entry| entry.content_hash).collect(), revisions })
    }

    let cover: Cover = Arc::new(Mutex::new(None));
    let service = Router::new()
        .route("/api/libraries/:library_id/thumbnail-batch", put(upload).post(download))
        .route("/api/libraries/:library_id/thumbnails/:content_hash", delete(remove))
        .route("/api/libraries/:library_id/thumbnails/presence", post(presence))
        .with_state(cover.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = ServerUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });

    let first = tempfile::tempdir().unwrap();
    let mut source = test_manager(first.path());
    source.server_url = url.clone();
    source.credentials = test_shared_credentials(&url);
    let hash = fixture_content_hash(701);
    source.database.seed_book(&hash, None, 1, "m4b");
    source.database.add_book_placement(&crate::ROOT_DIR_ID, &hash, "Story.m4b", "fp", true);
    let original = vec![0xff, 0xd8, 0xff, 1];
    source.assets.write(BlobKind::Thumbnail, &hash, &original).await.unwrap();
    source.database.complete_sidecar_thumbnail_work(&hash).unwrap();
    source.sync_pending_cover_changes().await.unwrap();
    assert_eq!(*cover.lock().unwrap(), Some(original.clone()));
    assert!(source.database.pending_thumbnail_sync_page("").unwrap().is_empty());

    let second = tempfile::tempdir().unwrap();
    let mut replica = test_manager(second.path());
    replica.server_url = url.clone();
    replica.credentials = test_shared_credentials(&url);
    replica.database.seed_book(&hash, None, 1, "m4b");
    replica.database.add_book_placement(&crate::ROOT_DIR_ID, &hash, "Story.m4b", "fp", true);
    replica.assets.write(BlobKind::Thumbnail, &hash, &original).await.unwrap();
    replica.database.complete_downloaded_thumbnail(&hash).unwrap();
    replica.refresh_remote_cover_revisions().await.unwrap();
    assert!(replica.assets.exists(BlobKind::Thumbnail, &hash).unwrap());

    let replacement = vec![0xff, 0xd8, 0xff, 2];
    source.assets.write(BlobKind::Thumbnail, &hash, &replacement).await.unwrap();
    source.database.complete_sidecar_thumbnail_work(&hash).unwrap();
    source.sync_pending_cover_changes().await.unwrap();
    replica.refresh_remote_cover_revisions().await.unwrap();
    assert!(!replica.assets.exists(BlobKind::Thumbnail, &hash).unwrap());
    assert!(replica.database.has_remote_thumbnail(&hash).unwrap());

    replica.sync_assets(Vec::new()).await.unwrap();
    assert_eq!(replica.assets.read_bytes(BlobKind::Thumbnail, &hash).await.unwrap(), Some(replacement.clone()));

    source.assets.remove(BlobKind::Thumbnail, &hash).unwrap();
    source.database.complete_replaced_sidecar_thumbnail_work(&hash, false).unwrap();
    source.sync_pending_cover_changes().await.unwrap();
    replica.refresh_remote_cover_revisions().await.unwrap();
    assert!(!replica.assets.exists(BlobKind::Thumbnail, &hash).unwrap());
    assert!(!replica.database.has_remote_thumbnail(&hash).unwrap());
    assert!(cover.lock().unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn asset_planning_leaves_moves_and_purges_to_local_jobs() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    let _server = serve_empty_cover_presence(&mut manager).await;
    let bytes = b"local book";
    let hash = ContentHash::new(blake3::hash(bytes).to_hex().as_str());
    std::fs::write(temp.path().join("original.epub"), bytes).unwrap();
    {
        let conn = &manager.database;
        conn.seed_book(&hash, None, 1, "epub");
        conn.add_book_placement(&crate::ROOT_DIR_ID, &hash, "renamed.epub", "fp", true);
        conn.seed_file_projection(&hash, &crate::ROOT_DIR_ID, "/original.epub");
        // The live placement (book_dir, above) already disagrees with this
        // remembered physical path; queue the same restore/trash pair a real
        // placement mutation would through `queue_file_work`.
        conn.seed_file_work("restore", &hash, "/renamed.epub");
        conn.seed_file_work("trash", &hash, "/original.epub");
        conn.record_remote_asset(BlobKind::Book, &hash).unwrap();
        conn.complete_thumbnail_work(&hash, false).unwrap();
    }
    manager.request_download(hash).unwrap();
    manager.reconcile_assets(Vec::new()).await.unwrap();
    assert!(temp.path().join("original.epub").exists());
    assert!(!temp.path().join("renamed.epub").exists());
    assert!(has_pending_file_work(&manager.database));
    assert!(!manager.database.is_book_download_requested(&hash).unwrap(), "remembered source satisfies downloads before the rename runs");
    assert!(manager.transfer_statuses().is_empty(), "a pending rename needs no network transfer");

    manager.assets.run_file_jobs(&manager.database).await.unwrap();
    assert!(temp.path().join("renamed.epub").exists());
    assert!(!temp.path().join("original.epub").exists());
    manager.assets.write(BlobKind::Thumbnail, &hash, b"retained cover").await.unwrap();
    {
        let conn = &manager.database;
        conn.trash_book(&hash).unwrap();
        conn.purge_book(&hash).unwrap();
    }
    manager.reconcile_assets(Vec::new()).await.unwrap();
    assert!(manager.transfer_statuses().is_empty(), "deleted placements cannot become transfer candidates");
    assert!(temp.path().join("renamed.epub").exists());
    assert!(manager.assets.exists(BlobKind::Thumbnail, &hash).unwrap());
    manager.assets.run_file_jobs(&manager.database).await.unwrap();
    assert!(!temp.path().join("renamed.epub").exists());
    assert!(!manager.assets.exists(BlobKind::Thumbnail, &hash).unwrap());
    assert!(!manager.database.has_pending_purge_work().unwrap());
}

#[tokio::test]
async fn download_request_pages_complete_existing_files_without_transfers() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    let _server = serve_empty_cover_presence(&mut manager).await;
    let mut hashes = Vec::new();
    for index in 0..65 {
        let bytes = format!("book {index}");
        let hash = ContentHash::new(blake3::hash(bytes.as_bytes()).to_hex().as_str());
        let name = format!("{index}.epub");
        std::fs::write(temp.path().join(&name), bytes).unwrap();
        let conn = &manager.database;
        conn.seed_book(&hash, None, 1, "epub");
        conn.add_book_placement(&crate::ROOT_DIR_ID, &hash, &name, "fp", false);
        conn.record_remote_asset(BlobKind::Book, &hash).unwrap();
        conn.complete_thumbnail_work(&hash, false).unwrap();
        hashes.push(hash);
    }
    manager.request_downloads(&hashes).unwrap();
    manager.reconcile_assets(Vec::new()).await.unwrap();
    let conn = &manager.database;
    assert!(conn.asset_requests(BlobKind::Book).unwrap().is_empty());
    for hash in hashes {
        assert!(conn.is_book_downloaded(&hash).unwrap());
    }
    assert!(manager.transfer_statuses().is_empty());
}

#[tokio::test]
async fn scheduled_download_completes_local_requests_without_book_transfers() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    let _server = serve_empty_cover_presence(&mut manager).await;
    let hash = test_content_hash();
    {
        let conn = &manager.database;
        conn.seed_book(&hash, None, 1, "epub");
        conn.add_book_placement(&crate::ROOT_DIR_ID, &hash, "book.epub", "fp", false);
        conn.record_remote_asset(BlobKind::Book, &hash).unwrap();
        conn.shared_request_thumbnail(&hash).unwrap();
    }
    std::fs::write(temp.path().join("book.epub"), b"already downloaded").unwrap();
    manager.database.shared_set_book_hash_downloaded(&hash, false).unwrap();
    manager.request_download(hash).unwrap();
    // Asset scheduling remains independent of metadata synchronization.
    let _gate = manager.sync_gate.lock().await;
    tokio::time::timeout(std::time::Duration::from_secs(3), manager.sync_assets(Vec::new())).await.unwrap().unwrap();
    assert!(!manager.database.is_book_download_requested(&hash).unwrap());
    assert!(manager.database.is_book_downloaded(&hash).unwrap());
}

#[test]
fn requested_download_uses_content_hash_identity() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let manager = test_manager(temp.path());
    let conn = &manager.database;
    let dir_id = fixture_dir_id("d1");
    conn.seed_dir(&dir_id, &crate::ROOT_DIR_ID, "Shelf");
    conn.seed_book(&test_content_hash(), None, 1, "epub");
    conn.add_book_placement(&dir_id, &test_content_hash(), "book.epub", "fp", false);

    let job = manager.prepare_requested_download(&test_content_hash()).unwrap().expect("live placement should produce a download job");
    assert_eq!(job, TransferJob::download_blob(test_content_hash(), db_placement(&crate::BookPlacement::new(test_content_hash(), crate::RelativeBookPath::parse("/Shelf/book.epub").unwrap())), TransferOrigin::UserInitiated,));
}

#[test]
fn download_request_requires_and_resolves_a_live_placement() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let manager = test_manager(temp.path());
    {
        let conn = &manager.database;
        conn.seed_book(&test_content_hash(), None, 1, "epub");
    }

    assert!(manager.request_download(test_content_hash()).is_err());

    {
        let conn = &manager.database;
        conn.add_book_placement(&crate::ROOT_DIR_ID, &test_content_hash(), "book.epub", "fp", false);
    }
    manager.request_download(test_content_hash()).unwrap();

    let conn = &manager.database;
    assert_eq!(conn.asset_requests(BlobKind::Book).unwrap(), vec![(test_content_hash(), TransferOrigin::UserInitiated)]);
}

#[test]
fn durable_download_intent_rederives_work_after_queue_recreation() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let manager = test_manager(temp.path());
    {
        let conn = &manager.database;
        conn.seed_book(&test_content_hash(), None, 1, "epub");
        conn.add_book_placement(&crate::ROOT_DIR_ID, &test_content_hash(), "book.epub", "fp", false);
    }
    manager.request_download(test_content_hash()).unwrap();
    assert_eq!(manager.transfer_queue.statuses()[0].total_items, 1, "the live scheduler receives the request immediately");
    drop(manager);

    let restored = test_manager(temp.path());
    let conn = &restored.database;
    assert_eq!(conn.asset_requests(BlobKind::Book).unwrap(), vec![(test_content_hash(), TransferOrigin::UserInitiated)]);
    assert!(restored.prepare_requested_download(&test_content_hash()).unwrap().is_some());
    assert!(restored.transfer_queue.statuses().is_empty());
}
