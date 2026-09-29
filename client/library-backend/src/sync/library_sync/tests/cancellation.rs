use super::*;

#[tokio::test]
async fn sync_activity_clears_on_cancellation_without_hiding_overlapping_work() {
    let count = AtomicUsize::new(0);
    let other = SyncActivity::begin(&count, None);
    let mut work = Box::pin(async {
        let _activity = SyncActivity::begin(&count, None);
        std::future::pending::<()>().await;
    });
    assert!(futures_util::poll!(work.as_mut()).is_pending());
    assert_eq!(count.load(Ordering::Acquire), 2);
    drop(work);
    assert_eq!(count.load(Ordering::Acquire), 1);
    drop(other);
    assert_eq!(count.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn stalled_inventory_and_asset_planning_do_not_hold_the_metadata_gate() {
    use axum::response::IntoResponse;
    let started = Arc::new(tokio::sync::Notify::new());
    let signalled = started.clone();
    let app = axum::Router::new()
        .route(
            "/api/sync/inventory",
            axum::routing::post(move || {
                signalled.notify_one();
                async { std::future::pending::<axum::response::Response>().await }
            }),
        )
        .route(
            "/api/sync/exchange",
            axum::routing::post(|headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                let request = decode_sync_exchange(&headers, &body);
                binary_sync_response(&SyncExchangeResponse {
                    push: PushMutationsResponse { accepted: request.mutations.iter().map(|m| m.mutation_id).collect(), rejected: Vec::new() },
                    pull: PullStateResponse { book_creations: Vec::new(), mutations: Vec::new(), next_cursor: request.cursor, has_more: false },
                })
                .into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    let id = fixture_dir_id("held-repair");
    manager.database.seed_dir(&id, &crate::ROOT_DIR_ID, "Initial");
    let _asset = manager.asset_gate.lock().await;
    let repair = manager.repair_inventory();
    tokio::pin!(repair);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut repair => panic!("repair must remain held"),
            _ = started.notified() => {},
        }
    })
    .await
    .unwrap();
    for name in ["First action", "Second action"] {
        manager.database.rename_dir(&id, name);
        tokio::time::timeout(std::time::Duration::from_secs(5), manager.synchronize_state()).await.unwrap().unwrap();
        assert!(manager.database.sync_publishable_mutations().unwrap().is_empty());
    }
    assert!(futures_util::poll!(repair.as_mut()).is_pending());
    server.abort();
}

/// Reading-position validity is a property of the wire type, not of a
/// batch-level check. `validate_pull_batch` polices revision ordering.

#[tokio::test]
async fn a_second_client_applies_moves_while_thumbnail_http_is_blocked() {
    type Changes = Arc<std::sync::Mutex<Vec<ServerMutation>>>;
    async fn exchange(axum::extract::State(changes): axum::extract::State<Changes>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        let mut changes = changes.lock().unwrap();
        let accepted = request.mutations.iter().map(|mutation| mutation.mutation_id).collect();
        for mutation in request.mutations {
            if changes.iter().any(|change| change.mutation.mutation_id == mutation.mutation_id) {
                continue;
            }
            let revision = LibraryRevision::new(changes.len() as u64 + 1).unwrap();
            changes.push(ServerMutation { mutation, replica_id: request.replica_id, revision });
        }
        let mutations = changes.iter().filter(|change| Some(change.revision) > request.cursor.state_revision).cloned().collect();
        binary_sync_response(&SyncExchangeResponse {
            push: PushMutationsResponse { accepted, rejected: Vec::new() },
            pull: PullStateResponse { book_creations: Vec::new(), mutations, next_cursor: SyncCursor { state_revision: changes.last().map(|change| change.revision), reading_revision: request.cursor.reading_revision }, has_more: false },
        })
    }
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let received = started.clone();
    let held = release.clone();
    let service = axum::Router::new()
        .route("/api/sync/exchange", axum::routing::post(exchange))
        .route(
            "/api/libraries/:library_id/thumbnail-batch",
            axum::routing::post(move || {
                let received = received.clone();
                let held = held.clone();
                async move {
                    received.notify_one();
                    held.notified().await;
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                }
            }),
        )
        .with_state(Changes::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = ServerUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    for root in [first.path(), second.path()] {
        std::fs::create_dir_all(root.join(crate::APP_HIDDEN_DIR)).unwrap();
    }
    let mut writer = test_manager_with_replica(first.path(), "move-writer");
    let mut reader = test_manager_with_replica(second.path(), "move-reader");
    for manager in [&mut writer, &mut reader] {
        manager.server_url = url.clone();
        manager.credentials = test_shared_credentials(&url);
        manager.inventory_checked.store(true, Ordering::Release);
    }
    let shelf = fixture_dir_id("moving-shelf");
    let destination = fixture_dir_id("move-destination");
    {
        writer.database.create_directory_with_id(&shelf, &crate::ROOT_DIR_ID, &"Shelf".to_owned()).unwrap();
        writer.database.create_directory_with_id(&destination, &crate::ROOT_DIR_ID, &"Destination".to_owned()).unwrap();
    }
    writer.synchronize_state().await.unwrap();
    reader.synchronize_state().await.unwrap();
    reader.assets.run_file_jobs(&reader.database).await.unwrap();
    std::fs::create_dir_all(second.path().join("Shelf")).unwrap();
    std::fs::write(second.path().join("Shelf/book.epub"), b"preserve these library bytes").unwrap();
    let (events, updates) = crate::library::events::library_event_channel();
    reader.event_tx = Some(events);
    let worker = reader.transfer_worker();
    let generation = worker.queue.begin_reconcile();
    worker.clone().run_page(generation, vec![TransferJob::download_thumbnail(fixture_content_hash(881), TransferOrigin::Background)]);
    worker.clone().finish_plan(generation);
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified()).await.unwrap();
    writer.database.move_directory(&shelf, Some(&destination), None).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        writer.synchronize_state().await.unwrap();
        reader.synchronize_state().await.unwrap();
        assert!(std::iter::from_fn(|| updates.try_recv().ok()).any(|event| matches!(event, LibraryEvent::ContentsChanged)), "the receiving view must be invalidated");
        reader.assets.run_file_jobs(&reader.database).await.unwrap();
    })
    .await
    .expect("metadata exchange and file moves must not wait for thumbnail HTTP");
    assert_eq!(reader.database.dir_path_string(&shelf), "Destination/Shelf");
    assert_eq!(std::fs::read(second.path().join("Destination/Shelf/book.epub")).unwrap(), b"preserve these library bytes");
    assert!(!second.path().join("Shelf").exists());
    release.notify_one();
    server.abort();
}

#[tokio::test]
async fn state_checkpoint_does_not_wait_for_asset_reconciliation() {
    async fn exchange(headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        binary_sync_response(&SyncExchangeResponse {
            push: PushMutationsResponse { accepted: request.mutations.iter().map(|mutation| mutation.mutation_id).collect(), rejected: Vec::new() },
            pull: PullStateResponse { book_creations: Vec::new(), mutations: Vec::new(), next_cursor: request.cursor, has_more: false },
        })
    }

    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let route_started = started.clone();
    let route_release = release.clone();
    let app = axum::Router::new().route("/api/libraries/:library_id/thumbnails/presence", axum::routing::post(empty_thumbnail_presence)).route("/api/sync/exchange", axum::routing::post(exchange)).route(
        "/api/libraries/:library_id/blobs/negotiate",
        axum::routing::post(move || {
            let started = route_started.clone();
            let release = route_release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    manager.inventory_checked.store(true, Ordering::Release);

    let book_bytes = test_epub(b"asset failure must not erase state success");
    let content_hash = ContentHash::new(blake3::hash(&book_bytes).to_hex().as_ref());
    {
        let conn = &manager.database;
        conn.seed_book(&content_hash, Some("Checkpointed"), 1, "epub");
        conn.add_book_placement(&crate::ROOT_DIR_ID, &content_hash, "checkpointed.epub", "fingerprint", true);
    }
    std::fs::write(temp.path().join("checkpointed.epub"), &book_bytes).unwrap();
    manager.database.observe_local_placement_book(&crate::ROOT_DIR_ID, &content_hash, &content_hash, book_bytes.len() as u64).unwrap();

    manager.synchronize_state().await.expect("state exchange succeeds");
    assert!(manager.database.sync_metadata("last_successful_sync_at").unwrap().is_some(), "state success is checkpointed before assets start");
    let (events, updates) = crate::library::events::library_event_channel();
    manager.event_tx = Some(events);
    manager.reconcile_assets(Vec::new()).await.unwrap();
    assert!(manager.transfer_statuses().is_empty(), "unprepared uploads must remain blocked");
    manager.database.finish_upload_preparation(&manager.database.upload_preparation_snapshot().unwrap()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), manager.reconcile_assets(Vec::new())).await.expect("planning must return while the upload is blocked").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), started.notified()).await.unwrap();
    let reading = reading_change(1, 1, "epubcfi(/6/2)", 1);
    manager.queue_outbox_reading_change(&reading, 1);
    tokio::time::timeout(std::time::Duration::from_secs(3), manager.synchronize_state()).await.expect("a later metadata pass must not wait for the upload").unwrap();
    // Metadata referring to unpublished bytes may remain behind the upload barrier.
    // The independent reading mutation must still be acknowledged.
    let pending = manager.database.sync_publishable_mutations().unwrap();
    assert!(pending.iter().all(|mutation| mutation.mutation_id != reading.mutation_id));
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !manager.transfer_statuses().iter().any(|status| status.kind == crate::TransferJobKind::UploadBook && status.state == crate::TransferState::Retrying) {
            updates.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    let upload = manager.transfer_statuses().into_iter().find(|status| status.kind == crate::TransferJobKind::UploadBook).expect("upload remains queued");
    assert_eq!(upload.state, crate::TransferState::Retrying);
    assert_eq!(upload.attempts, 1);
    assert!(upload.last_error.is_some(), "admission failure remains visible and independently retryable");
    assert!(manager.database.sync_metadata("last_successful_sync_at").unwrap().is_some(), "asset failure cannot undo the state checkpoint");
    server.abort();
}
