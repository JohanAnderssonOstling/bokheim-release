use super::*;

#[tokio::test]
async fn independent_repair_republishes_state_missing_from_the_server_inventory() {
    #[derive(Clone)]
    struct InventoryState(Arc<std::sync::Mutex<Vec<String>>>);

    async fn inventory(body: axum::body::Bytes) -> axum::response::Response {
        let request: sync_common::StateInventoryRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
        binary_sync_response(&sync_common::StateInventoryResponse { missing: request.cells })
    }

    async fn exchange(axum::extract::State(state): axum::extract::State<InventoryState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        state.0.lock().unwrap().extend(request.mutations.iter().map(|mutation| mutation.kind.clone()));
        binary_sync_response(&SyncExchangeResponse {
            push: PushMutationsResponse { accepted: request.mutations.iter().map(|mutation| mutation.mutation_id).collect(), rejected: Vec::new() },
            pull: PullStateResponse { book_creations: Vec::new(), mutations: Vec::new(), next_cursor: request.cursor, has_more: false },
        })
    }

    let pushed_kinds = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = axum::Router::new().route("/api/sync/inventory", axum::routing::post(inventory)).route("/api/sync/exchange", axum::routing::post(exchange)).with_state(InventoryState(Arc::clone(&pushed_kinds)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    {
        let conn = &manager.database;
        let dir_id = fixture_dir_id("inventory/shelf");
        let hash = fixture_content_hash(7);
        conn.seed_shelf_dir(&dir_id);
        conn.seed_book(&hash, Some("Inventory Book"), 1, "epub");
        conn.add_book_placement(&dir_id, &hash, "inventory.epub", "fingerprint", true);
        assert!(!conn.sync_inventory_page(None).unwrap().is_empty());
        conn.clear_sync_outbox().unwrap();
    }

    manager.state_engine().unwrap().synchronize().await.unwrap();
    assert!(pushed_kinds.lock().unwrap().is_empty(), "ordinary sync must not run inventory repair");
    manager.repair_inventory().await.unwrap();
    manager.state_engine().unwrap().synchronize().await.unwrap();

    let pushed = pushed_kinds.lock().unwrap();
    assert!(pushed.iter().any(|kind| kind == "directory_lifecycle"));
    assert!(pushed.iter().any(|kind| kind == "book_lifecycle"));
    assert!(pushed.iter().any(|kind| kind == "placement"));
    assert!(manager.database.sync_publishable_mutations().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn inventory_repair_resumes_its_checkpoint_after_a_failed_page() {
    use axum::response::IntoResponse;
    let requests = Arc::new(std::sync::Mutex::new(Vec::<Vec<sync_common::StateCell>>::new()));
    let observed = requests.clone();
    let app = axum::Router::new().route(
        "/api/sync/inventory",
        axum::routing::post(move |body: axum::body::Bytes| {
            let observed = observed.clone();
            async move {
                let request: sync_common::StateInventoryRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
                let count = {
                    let mut calls = observed.lock().unwrap();
                    calls.push(request.cells);
                    calls.len()
                };
                if count == 2 {
                    return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                binary_sync_response(&sync_common::StateInventoryResponse { missing: Vec::new() })
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
    {
        let conn = &manager.database;
        for index in 0..120 {
            let id = fixture_dir_id(&format!("repair-{index}"));
            conn.seed_dir(&id, &crate::ROOT_DIR_ID, &format!("Repair {index}"));
        }
    }
    assert!(manager.repair_inventory().await.is_err());
    assert!(manager.database.sync_inventory_checkpoint().unwrap().is_some());
    assert!(crate::test_database::raw(&manager.database).query_row("SELECT operation_sync_unfinished FROM sync_metadata WHERE singleton=1", [], |row| row.get::<_, bool>(0)).unwrap());
    // A fresh handle has no in-memory progress, but reads the durable checkpoint.
    let url = manager.server_url.clone();
    drop(manager);
    let mut manager = test_manager(temp.path());
    manager.server_url = url;
    manager.credentials = test_shared_credentials(&manager.server_url);
    manager.repair_inventory().await.unwrap();
    let calls = requests.lock().unwrap();
    assert_eq!(calls[0].len(), 256);
    assert_eq!(calls[1], calls[2], "retry resumes the failed page, not the completed first page");
    assert!(manager.database.sync_inventory_checkpoint().unwrap().is_none());
    assert!(!crate::test_database::raw(&manager.database).query_row("SELECT operation_sync_unfinished FROM sync_metadata WHERE singleton=1", [], |row| row.get::<_, bool>(0)).unwrap());
    assert!(crate::test_database::raw(&manager.database).query_row("SELECT last_successful_sync_at IS NULL FROM sync_metadata WHERE singleton=1", [], |row| row.get::<_, bool>(0)).unwrap(), "repair is not a full-sync checkpoint");
    server.abort();
}

/// When the server's state regresses beneath a device's cursor the device
/// cannot make progress in either direction. Recovery is the existing
/// republish path: drop the unusable cursor and re-reconcile, rather than
/// failing every future synchronization.

#[tokio::test]
async fn a_rejected_cursor_is_dropped_and_the_exchange_recovers() {
    #[derive(Clone)]
    struct ExchangeState {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    async fn exchange_page(axum::extract::State(state): axum::extract::State<ExchangeState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        use axum::response::IntoResponse;

        let request = decode_sync_exchange(&headers, &body);
        state.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match request.cursor.state_revision.map(LibraryRevision::get) {
            // The server has regressed beneath this cursor and says so.
            Some(99) => axum::http::StatusCode::GONE.into_response(),
            None => binary_sync_response(&SyncExchangeResponse {
                push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() },
                pull: pull_response(vec![pulled_change(1, added(1, 10))], LibraryRevision::new(1).ok(), false),
            }),
            Some(1) => binary_sync_response(&SyncExchangeResponse {
                push: PushMutationsResponse { accepted: request.mutations.iter().map(|mutation| mutation.mutation_id).collect(), rejected: Vec::new() },
                pull: pull_response(Vec::new(), LibraryRevision::new(1).ok(), false),
            }),
            other => panic!("unexpected cursor {other:?}"),
        }
    }

    let state = ExchangeState { calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)) };
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(exchange_page)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    {
        let conn = &manager.database;
        conn.set_sync_pull_cursor(&cursor(LibraryRevision::new(99).ok())).unwrap();
    }

    manager.sync_once().await.expect("a rejected cursor must be recoverable without operator intervention");

    assert_eq!(state.calls.load(std::sync::atomic::Ordering::SeqCst), 3, "recovery performs one clean pull before the one-shot publishing retry");
    let conn = &manager.database;
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(1).ok()), "the device adopts the server's current head");
    assert_eq!(conn.sync_metadata(CURSOR_RECOVERY_PENDING_KEY).unwrap(), None, "successful recovery clears its durable marker");
    server.abort();
}

#[tokio::test]
async fn beginning_cursor_recovery_atomically_persists_a_clean_cursor_and_resume_marker() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let manager = test_manager(temp.path());
    {
        let conn = &manager.database;
        conn.set_sync_pull_cursor(&cursor(LibraryRevision::new(99).ok())).unwrap();
    }

    manager.state_engine().unwrap().begin_cursor_recovery().await.unwrap();

    let conn = &manager.database;
    assert_eq!(conn.sync_pull_cursor().unwrap(), SyncCursor::default());
    assert_eq!(conn.sync_metadata(CURSOR_RECOVERY_PENDING_KEY).unwrap().as_deref(), Some("1"));
}

/// A backup can retain a cell while losing its newest value. Missing-key
/// inventory cannot detect that case, so cursor recovery must republish the
/// local canonical state even when the restored server says every key is
/// still present.

#[tokio::test]
async fn cursor_recovery_republishes_cells_that_still_exist_on_the_restored_server() {
    #[derive(Clone)]
    struct RecoveryState {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        pushed_kinds: Arc<std::sync::Mutex<Vec<String>>>,
    }

    async fn inventory(body: axum::body::Bytes) -> axum::response::Response {
        let request: sync_common::StateInventoryRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
        assert!(!request.cells.is_empty(), "recovery has local canonical cells to consider");
        binary_sync_response(&sync_common::StateInventoryResponse { missing: Vec::new() })
    }

    async fn exchange_page(axum::extract::State(state): axum::extract::State<RecoveryState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        use axum::response::IntoResponse;

        let request = decode_sync_exchange(&headers, &body);
        state.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if request.cursor.state_revision.map(LibraryRevision::get) == Some(99) {
            return axum::http::StatusCode::GONE.into_response();
        }
        state.pushed_kinds.lock().unwrap().extend(request.mutations.iter().map(|mutation| mutation.kind.clone()));
        binary_sync_response(&SyncExchangeResponse {
            push: PushMutationsResponse { accepted: request.mutations.iter().map(|mutation| mutation.mutation_id).collect(), rejected: Vec::new() },
            pull: pull_response(Vec::new(), LibraryRevision::new(1).ok(), false),
        })
    }

    let state = RecoveryState { calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)), pushed_kinds: Arc::new(std::sync::Mutex::new(Vec::new())) };
    let app = axum::Router::new().route("/api/sync/inventory", axum::routing::post(inventory)).route("/api/sync/exchange", axum::routing::post(exchange_page)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    {
        let conn = &manager.database;
        let hash = fixture_content_hash(77);
        conn.seed_book(&hash, Some("Newer local value"), 10, "epub");
        assert!(!conn.sync_inventory_page(None).unwrap().is_empty());
        conn.clear_sync_outbox().unwrap();
        conn.set_sync_pull_cursor(&cursor(LibraryRevision::new(99).ok())).unwrap();
    }
    // Model a client that already performed its ordinary opening inventory
    // pass before the server restore became visible.
    manager.inventory_checked.store(true, Ordering::Release);

    manager.sync_once().await.expect("cursor recovery must republish local canonical state");

    assert_eq!(state.calls.load(std::sync::atomic::Ordering::SeqCst), 3, "one rejected exchange is followed by a clean pull and one publishing retry");
    assert!(state.pushed_kinds.lock().unwrap().iter().any(|kind| kind == sync_common::mutation_kind::BOOK_LIFECYCLE), "the newer local lifecycle value must be republished even though its key still exists remotely");
    assert_eq!(manager.database.sync_metadata(CURSOR_RECOVERY_PENDING_KEY).unwrap(), None, "successful recovery clears its durable marker");
    server.abort();
}
