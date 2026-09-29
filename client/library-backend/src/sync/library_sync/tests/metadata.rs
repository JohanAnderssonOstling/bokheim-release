use super::*;

#[tokio::test]
async fn committed_pages_notify_before_the_next_http_page_finishes() {
    use axum::response::IntoResponse;
    let app = axum::Router::new().route(
        "/api/sync/exchange",
        axum::routing::post(|headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
            let request = decode_sync_exchange(&headers, &body);
            if request.cursor.state_revision.is_some() {
                return std::future::pending().await;
            }
            binary_sync_response(&SyncExchangeResponse { push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() }, pull: pull_response(vec![pulled_change(1, added(1, 10))], LibraryRevision::new(1).ok(), true) })
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
    let (events, updates) = crate::library::events::library_event_channel();
    manager.event_tx = Some(events);
    let exchange = manager.synchronize_state();
    tokio::pin!(exchange);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        // The exchange also announces its own start (`TransferStatusChanged`)
        // before any page is fetched; only `ContentsChanged` confirms the
        // first page actually committed ahead of the blocked second page.
        loop {
            tokio::select! {
                result = &mut exchange => panic!("second page must remain blocked: {result:?}"),
                event = updates.recv() => match event.unwrap() {
                    LibraryEvent::ContentsChanged => break,
                    _ => continue,
                },
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(manager.database.sync_pull_cursor().unwrap().state_revision, LibraryRevision::new(1).ok());
    assert!(manager.database.sync_metadata("last_successful_sync_at").unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn inactive_outbox_drains_acknowledged_changes_without_pulling() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = calls.clone();
    let app = axum::Router::new().route(
        "/api/sync/exchange",
        axum::routing::post(move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
            let calls = state.clone();
            async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let request = decode_sync_exchange(&headers, &body);
                assert!(!request.mutations.is_empty(), "inactive libraries never issue pull-only requests");
                binary_sync_response(&SyncExchangeResponse {
                    push: PushMutationsResponse { accepted: request.mutations.iter().map(|m| m.mutation_id).collect(), rejected: Vec::new() },
                    pull: pull_response(vec![pulled_change(1, added(u64::MAX, 10))], LibraryRevision::new(1).ok(), true),
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    let count = sync_common::MAX_PUSH_MUTATIONS as u64 + 1;
    for sequence in 1..=count {
        manager.queue_outbox_reading_change(&reading_change(sequence, sequence, "epubcfi(/6/2)", sequence), sequence);
    }
    manager.state_engine().unwrap().drain_outbox().await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let conn = &manager.database;
    assert!(conn.sync_publishable_mutations().unwrap().is_empty());
    assert_eq!(conn.sync_pull_cursor().unwrap(), SyncCursor::default());
    assert_eq!(conn.count_books(&[fixture_content_hash(u64::MAX)]), 0);
    server.abort();
    let refused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    manager.server_url = ServerUrl::parse(&format!("http://{}", refused.local_addr().unwrap())).unwrap();
    drop(refused);
    manager.credentials = test_shared_credentials(&manager.server_url);
    manager.queue_outbox_reading_change(&reading_change(count + 1, count + 1, "epubcfi(/6/2)", count + 1), count + 1);
    assert!(manager.state_engine().unwrap().drain_outbox().await.is_err());
    assert_eq!(manager.database.sync_publishable_mutations().unwrap().len(), 1, "offline changes must remain durable");
}

#[tokio::test]
async fn production_client_drains_all_pull_pages_before_finishing() {
    #[derive(Clone)]
    struct PageState(Arc<std::sync::atomic::AtomicUsize>);

    async fn pull_page(axum::extract::State(state): axum::extract::State<PageState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        state.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let request = decode_sync_exchange(&headers, &body);
        let pull = match request.cursor.state_revision.map(LibraryRevision::get) {
            None => pull_response(vec![pulled_change(1, added(1, 10))], LibraryRevision::new(1).ok(), true),
            Some(1) => pull_response(vec![pulled_change(2, deleted(1))], LibraryRevision::new(2).ok(), false),
            _ => PullStateResponse { book_creations: Vec::new(), mutations: Vec::new(), next_cursor: request.cursor, has_more: false },
        };
        binary_sync_response(&SyncExchangeResponse { push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() }, pull })
    }

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(pull_page)).with_state(PageState(calls.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);

    manager.state_engine().unwrap().pull_events_from_server(SyncCursor::default()).await.unwrap();

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let conn = &manager.database;
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(2).ok()));
    server.abort();
}

#[tokio::test]
async fn restart_after_failed_pull_page_reapplies_the_whole_page_and_cursor() {
    #[derive(Clone)]
    struct PageState(Arc<std::sync::atomic::AtomicUsize>);

    async fn pull_page(axum::extract::State(state): axum::extract::State<PageState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        assert_eq!(request.cursor, SyncCursor::default(), "a failed page must not advance either cursor channel");
        let mut second = pulled_change(2, added(2, 20));
        if state.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            second.mutation.value = vec![0xff];
        }
        binary_sync_response(&SyncExchangeResponse { push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() }, pull: pull_response(vec![pulled_change(1, added(1, 10)), second], LibraryRevision::new(2).ok(), false) })
    }

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(pull_page)).with_state(PageState(calls.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    {
        let mut manager = test_manager(root.path());
        manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
        manager.credentials = test_shared_credentials(&manager.server_url);

        let (events, updates) = crate::library::events::library_event_channel();
        manager.event_tx = Some(events);
        assert!(manager.state_engine().unwrap().pull_events_from_server(SyncCursor::default()).await.is_err(), "the corrupt second mutation must abort the page");
        assert!(updates.try_recv().is_err(), "rolled-back pages must never be announced to the UI");
        let conn = &manager.database;
        let books: i64 = conn.count_books(&[fixture_content_hash(1), fixture_content_hash(2)]);
        assert_eq!(books, 0, "the valid first mutation must roll back with the failed page");
        assert_eq!(conn.sync_pull_cursor().unwrap(), SyncCursor::default(), "the failed page must not persist its cursor");
    }

    let mut restarted = test_manager(root.path());
    restarted.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    restarted.credentials = test_shared_credentials(&restarted.server_url);
    restarted.state_engine().unwrap().pull_events_from_server(SyncCursor::default()).await.expect("restart must reapply the complete valid page");

    let conn = &restarted.database;
    let books: i64 = conn.count_books(&[fixture_content_hash(1), fixture_content_hash(2)]);
    assert_eq!(books, 2);
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(2).ok()));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn production_client_persists_exact_final_cursor_beyond_last_mutation() {
    async fn pull_page(headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        binary_sync_response(&SyncExchangeResponse {
            push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() },
            pull: pull_response(vec![pulled_change(1, added(1, 10))], request.cursor.state_revision.or(LibraryRevision::new(10).ok()), false),
        })
    }
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(pull_page));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);

    manager.state_engine().unwrap().pull_events_from_server(SyncCursor::default()).await.unwrap();

    let conn = &manager.database;
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(10).ok()));
    server.abort();
}

#[tokio::test]
async fn production_client_persists_advanced_cursor_from_empty_final_page() {
    async fn pull_page(headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        assert_eq!(request.cursor.state_revision.map(LibraryRevision::get), Some(10));
        binary_sync_response(&SyncExchangeResponse { push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() }, pull: pull_response(Vec::new(), LibraryRevision::new(12).ok(), false) })
    }
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(pull_page));
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
        conn.set_sync_pull_cursor(&cursor(LibraryRevision::new(10).ok())).unwrap();
        assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(10).ok()));
    }

    manager.state_engine().unwrap().pull_events_from_server(cursor(LibraryRevision::new(10).ok())).await.unwrap();

    let conn = &manager.database;
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(12).ok()));
    server.abort();
}

#[tokio::test]
async fn production_client_uses_exchange_then_drains_remaining_pull_pages() {
    #[derive(Clone)]
    struct ExchangeState {
        exchanges: Arc<std::sync::atomic::AtomicUsize>,
        pulls: Arc<std::sync::atomic::AtomicUsize>,
    }

    // Every page travels over the exchange endpoint: the first carries the
    // push and the first pull page, and each later page is the same request
    // with an advanced cursor and nothing to push.
    async fn exchange_page(axum::extract::State(state): axum::extract::State<ExchangeState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        match request.cursor.state_revision.map(LibraryRevision::get) {
            None => {
                state.exchanges.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                binary_sync_response(&SyncExchangeResponse { push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() }, pull: pull_response(vec![pulled_change(1, added(1, 10))], LibraryRevision::new(1).ok(), true) })
            }
            Some(1) => {
                state.pulls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert!(request.mutations.is_empty(), "a drain page has nothing left to push");
                binary_sync_response(&SyncExchangeResponse { push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() }, pull: pull_response(vec![pulled_change(2, deleted(1))], LibraryRevision::new(2).ok(), false) })
            }
            other => panic!("unexpected cursor {other:?}"),
        }
    }

    let state = ExchangeState { exchanges: Arc::new(std::sync::atomic::AtomicUsize::new(0)), pulls: Arc::new(std::sync::atomic::AtomicUsize::new(0)) };
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(exchange_page)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);

    manager.state_engine().unwrap().exchange_events_with_server(SyncCursor::default()).await.unwrap();

    assert_eq!(state.exchanges.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(state.pulls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let conn = &manager.database;
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(2).ok()));
    server.abort();
}

#[tokio::test]
async fn push_and_pull_pagination_advance_in_the_same_exchange_loop() {
    #[derive(Clone)]
    struct ExchangeState(Arc<std::sync::atomic::AtomicUsize>);

    async fn exchange_page(axum::extract::State(state): axum::extract::State<ExchangeState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        let call = state.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let pull = match call {
            0 => {
                assert_eq!(request.cursor, SyncCursor::default());
                assert_eq!(request.mutations.len(), sync_common::MAX_PUSH_MUTATIONS);
                pull_response(vec![pulled_change(1, added(1, 10))], LibraryRevision::new(1).ok(), true)
            }
            1 => {
                assert_eq!(request.cursor.state_revision, LibraryRevision::new(1).ok());
                assert_eq!(request.mutations.len(), 1, "the next push page shares the next pull request");
                pull_response(vec![pulled_change(2, deleted(1))], LibraryRevision::new(2).ok(), false)
            }
            2 => {
                assert_eq!(request.mutations.len(), 1, "the later lifecycle declaration schedules one reconciliation");
                assert_eq!(request.mutations[0].kind, "reading_position");
                assert_eq!(request.mutations[0].entity_key, fixture_content_hash(1).to_string());
                PullStateResponse { book_creations: vec![], mutations: vec![], next_cursor: request.cursor, has_more: false }
            }
            _ => panic!("reconciliation must finish without a publication loop"),
        };
        binary_sync_response(&SyncExchangeResponse { push: PushMutationsResponse { accepted: request.mutations.iter().map(|mutation| mutation.mutation_id).collect(), rejected: Vec::new() }, pull })
    }

    let state = ExchangeState(Arc::new(std::sync::atomic::AtomicUsize::new(0)));
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
        // Fresh reading positions are eligible in this cycle, even across
        // multiple push pages; there is no separate reading quiet period.
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap().as_millis() as u64;
        for sequence in 1..=sync_common::MAX_PUSH_MUTATIONS as u64 + 1 {
            let mutation = reading_change(sequence, sequence, "epubcfi(/6/2)", now);
            manager.queue_outbox_reading_change(&mutation, sequence);
        }
    }

    let original = manager.database.sync_publishable_mutations().unwrap().into_iter().find(|m| m.to_wire().unwrap().entity_key == fixture_content_hash(1).to_string()).unwrap();
    let result = manager.state_engine().unwrap().exchange_events_with_server(SyncCursor::default()).await.unwrap();

    assert!(result.mutation_issues.is_empty());
    assert_eq!(state.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    let conn = &manager.database;
    let reconciliation = conn.sync_publishable_mutations().unwrap();
    assert_eq!(reconciliation.len(), 1);
    assert_eq!(reconciliation[0].body, original.body);
    assert_eq!(reconciliation[0].changed_at, original.changed_at);
    assert_eq!(reconciliation[0].origin.as_ref().unwrap().mutation_id, original.mutation_id);
    assert_eq!(reconciliation[0].origin.as_ref().unwrap().replica_seq, original.replica_seq);
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(2).ok()));
    manager.state_engine().unwrap().exchange_events_with_server(conn.sync_pull_cursor().unwrap()).await.unwrap();
    assert_eq!(state.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(conn.sync_publishable_mutations().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn transient_rejection_does_not_starve_later_push_pages() {
    #[derive(Clone)]
    struct ExchangeState {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        rejected: Arc<std::sync::Mutex<Option<sync_common::MutationId>>>,
    }
    async fn exchange_page(axum::extract::State(state): axum::extract::State<ExchangeState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        let call = state.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let push = if call == 0 {
            assert_eq!(request.mutations.len(), sync_common::MAX_PUSH_MUTATIONS);
            let rejected = request.mutations[0].mutation_id;
            *state.rejected.lock().unwrap() = Some(rejected);
            PushMutationsResponse { accepted: request.mutations[1..].iter().map(|mutation| mutation.mutation_id).collect(), rejected: vec![MutationRejection { mutation_id: rejected, reason: MutationRejectionReason::MissingDependency }] }
        } else {
            assert_eq!(request.mutations.len(), 1);
            PushMutationsResponse { accepted: vec![request.mutations[0].mutation_id], rejected: Vec::new() }
        };
        binary_sync_response(&SyncExchangeResponse { push, pull: PullStateResponse { book_creations: Vec::new(), mutations: Vec::new(), next_cursor: request.cursor, has_more: false } })
    }

    let state = ExchangeState { calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)), rejected: Arc::new(std::sync::Mutex::new(None)) };
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
        for sequence in 1..=sync_common::MAX_PUSH_MUTATIONS as u64 + 1 {
            let mutation = reading_change(sequence, sequence, "epubcfi(/6/2)", sequence);
            manager.queue_outbox_reading_change(&mutation, sequence);
        }
    }

    let exchange = manager.state_engine().unwrap().exchange_events_with_server(SyncCursor::default()).await.expect("a deferred page still completes the exchange");

    assert_eq!(state.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let rejected = state.rejected.lock().unwrap().unwrap();
    assert_eq!(exchange.mutation_issues, vec![sync_common::MutationIssue::Retryable(MutationRejection { mutation_id: rejected, reason: MutationRejectionReason::MissingDependency })]);
    let conn = &manager.database;
    let pending = conn.sync_publishable_mutations().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].mutation_id, rejected);
    server.abort();
}

#[tokio::test]
async fn a_deferred_mutation_does_not_fail_the_surrounding_synchronization() {
    #[derive(Clone)]
    struct ExchangeState {
        deferred: Arc<std::sync::Mutex<Option<sync_common::MutationId>>>,
    }
    async fn exchange_page(axum::extract::State(state): axum::extract::State<ExchangeState>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        assert_eq!(request.mutations.len(), 2, "both queued positions travel in one page");
        let deferred = request.mutations[0].mutation_id;
        *state.deferred.lock().unwrap() = Some(deferred);
        binary_sync_response(&SyncExchangeResponse {
            push: PushMutationsResponse { accepted: vec![request.mutations[1].mutation_id], rejected: vec![MutationRejection { mutation_id: deferred, reason: MutationRejectionReason::TimestampTooFarFuture }] },
            pull: pull_response(vec![pulled_change(1, added(1, 10))], LibraryRevision::new(1).ok(), false),
        })
    }

    let state = ExchangeState { deferred: Arc::new(std::sync::Mutex::new(None)) };
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(exchange_page)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    for sequence in 1..=2 {
        manager.queue_outbox_reading_change(&reading_change(sequence, sequence, "epubcfi(/6/2)", sequence), sequence);
    }

    let (events, updates) = crate::library::events::library_event_channel();

    manager.event_tx = Some(events);
    let outcome = manager.synchronize_state().await;

    // Side effects first, so this test reports what the exchange actually
    // did before it reports how it answered.
    let conn = &manager.database;
    let pending = conn.sync_publishable_mutations().unwrap();
    assert_eq!(pending.len(), 1, "the deferred mutation stays queued for a later attempt");
    assert_eq!(pending[0].mutation_id, state.deferred.lock().unwrap().unwrap());
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(1).ok()), "the pulled page is still applied");

    assert!(std::iter::from_fn(|| updates.try_recv().ok()).any(|event| matches!(event, LibraryEvent::ContentsChanged)), "remote state must invalidate ordinary library views");
    let outcome = outcome.expect("one deferred mutation must not fail the synchronization");
    assert_eq!(outcome.mutation_issues, vec![sync_common::MutationIssue::Retryable(MutationRejection { mutation_id: state.deferred.lock().unwrap().unwrap(), reason: MutationRejectionReason::TimestampTooFarFuture })]);
    assert!(conn.sync_metadata("last_successful_sync_at").unwrap().is_some(), "the exchange completed, so the sync is checkpointed");
    server.abort();
}

#[tokio::test]
async fn permanent_rejection_removes_poison_mutation_from_durable_outbox() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let manager = test_manager(temp.path());
    let requested = reading_change(1, 1, "epubcfi(/6/24)", 1);
    {
        let conn = &manager.database;
        conn.seed_poison_outbox_mutation(requested.replica_seq.get() as i64, &requested.mutation_id.to_string(), &sync_common::wire::encode(&requested.body).unwrap(), i64::try_from(requested.changed_at).unwrap()).unwrap();
    }
    let response = PushMutationsResponse { accepted: Vec::new(), rejected: vec![MutationRejection { mutation_id: requested.mutation_id, reason: MutationRejectionReason::InvalidPayload }] };

    assert!(manager.state_engine().unwrap().apply_push_response(&[requested.mutation_id], &response).await.is_err());

    let conn = &manager.database;
    assert!(conn.sync_publishable_mutations().unwrap().is_empty());
}

#[tokio::test]
async fn permanent_rejection_is_an_outcome_after_applying_the_exchange_pull_half() {
    async fn exchange(headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        let rejected = request.mutations.first().expect("the fixture publishes one mutation").mutation_id;
        binary_sync_response(&SyncExchangeResponse {
            push: PushMutationsResponse { accepted: Vec::new(), rejected: vec![MutationRejection { mutation_id: rejected, reason: MutationRejectionReason::InvalidPayload }] },
            pull: pull_response(
                vec![
                    pulled_change(1, added(9, 10)),
                    pulled_change(2, MutationBody::Metadata { content_hash: fixture_content_hash(9), value: library_replica::SyncBookMetadata { title: "Pulled despite rejection".into(), ..Default::default() } }),
                ],
                LibraryRevision::new(2).ok(),
                false,
            ),
        })
    }

    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(exchange));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    let requested = reading_change(1, 1, "epubcfi(/6/24)", 1);
    manager.queue_outbox_reading_change(&requested, 1);

    let outcome = manager.state_engine().unwrap().exchange_events_with_server(SyncCursor::default()).await.unwrap();
    assert_eq!(outcome.mutation_issues, vec![sync_common::MutationIssue::Rejected(MutationRejection { mutation_id: requested.mutation_id, reason: MutationRejectionReason::InvalidPayload })]);

    let conn = &manager.database;
    assert!(conn.sync_publishable_mutations().unwrap().is_empty());
    assert_eq!(conn.sync_pull_cursor().unwrap(), cursor(LibraryRevision::new(2).ok()));
    let pulled_title: String = conn.book_title(&fixture_content_hash(9));
    assert_eq!(pulled_title, "Pulled despite rejection");
    server.abort();
}

#[tokio::test]
async fn rejected_creation_defers_its_later_fields_without_starving_other_books() {
    #[derive(Clone, Default)]
    struct State {
        blocked: Arc<std::sync::Mutex<Option<String>>>,
        retry: Arc<std::sync::atomic::AtomicBool>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    async fn exchange(axum::extract::State(state): axum::extract::State<State>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
        let request = decode_sync_exchange(&headers, &body);
        let call = state.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut push = PushMutationsResponse { accepted: vec![], rejected: vec![] };
        if call == 0 {
            assert_eq!(request.mutations.len(), sync_common::MAX_PUSH_MUTATIONS);
            assert!(request.mutations.iter().all(|m| m.kind == "book_lifecycle"));
            *state.blocked.lock().unwrap() = Some(request.mutations[0].entity_key.clone());
            push.rejected.push(MutationRejection { mutation_id: request.mutations[0].mutation_id, reason: MutationRejectionReason::MissingDependency });
            push.accepted.extend(request.mutations[1..].iter().map(|m| m.mutation_id));
        } else {
            if !state.retry.load(std::sync::atomic::Ordering::SeqCst) {
                let blocked = state.blocked.lock().unwrap();
                assert!(request.mutations.iter().all(|m| m.book_field_owner() != blocked.as_deref()));
            } else {
                assert_eq!(request.mutations[0].kind, "book_lifecycle");
                assert!(request.mutations.iter().any(|m| m.kind == "book_facts"));
            }
            push.accepted.extend(request.mutations.iter().map(|m| m.mutation_id));
        }
        binary_sync_response(&SyncExchangeResponse { push, pull: PullStateResponse { book_creations: vec![], mutations: vec![], next_cursor: request.cursor, has_more: false } })
    }
    let state = State::default();
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(exchange)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    for index in 1..=sync_common::MAX_PUSH_MUTATIONS as u64 {
        manager.database.seed_book(&fixture_content_hash(index), Some("Book"), 1, "epub");
    }
    let outcome = manager.state_engine().unwrap().exchange_events_with_server(SyncCursor::default()).await.unwrap();
    assert_eq!(outcome.mutation_issues.len(), 1);
    let pending = manager.database.sync_publishable_mutations().unwrap();
    assert!(pending.iter().any(|m| matches!(m.body, MutationBody::BookFacts { .. })));
    let blocked = state.blocked.lock().unwrap().clone().unwrap();
    assert!(pending.iter().all(|m| { let wire = m.to_wire().unwrap(); wire.entity_key == blocked || wire.book_field_owner() == Some(&blocked) }));
    state.retry.store(true, std::sync::atomic::Ordering::SeqCst);
    manager.state_engine().unwrap().exchange_events_with_server(SyncCursor::default()).await.unwrap();
    assert!(manager.database.sync_publishable_mutations().unwrap().is_empty());
    server.abort();
}
