use super::*;

#[tokio::test]
async fn account_switch_applies_an_in_flight_native_page_and_acknowledgments() {
    for queued in [false, true] {
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let observed = started.clone();
        let released = release.clone();
        let app = axum::Router::new().route(
            "/api/sync/exchange",
            axum::routing::post(move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let started = observed.clone();
                let release = released.clone();
                async move {
                    let request = decode_sync_exchange(&headers, &body);
                    started.notify_one();
                    release.notified().await;
                    binary_sync_response(&SyncExchangeResponse {
                        push: PushMutationsResponse { accepted: request.mutations.iter().map(|m| m.mutation_id).collect(), rejected: Vec::new() },
                        pull: pull_response(vec![pulled_change(1, added(99, 10))], LibraryRevision::new(1).ok(), false),
                    })
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
        manager.database.seed_dir(&fixture_dir_id("queued-account-edit"), &crate::ROOT_DIR_ID, "Queued edit");
        let pending = manager.database.sync_publishable_mutations().unwrap().len();
        assert!(pending > 0);
        if !queued {
            manager.database.clear_sync_outbox().unwrap();
        }
        let _pending = if queued { pending } else { 0 };
        let (events, updates) = crate::library::events::library_event_channel();
        manager.event_tx = Some(events);
        let exchange = manager.synchronize_state();
        tokio::pin!(exchange);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! { result = &mut exchange => panic!("request must remain held: {result:?}"), _ = started.notified() => {} }
        })
        .await
        .unwrap();
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), &mut exchange).await.unwrap().unwrap();
        let conn = &manager.database;
        assert!(conn.sync_publishable_mutations().unwrap().is_empty(), "the in-flight acknowledgments are consumed");
        assert_eq!(conn.sync_pull_cursor().unwrap().state_revision, LibraryRevision::new(1).ok());
        assert_eq!(conn.book_count(&fixture_content_hash(99)), 1);
        assert!(updates.try_recv().is_ok());
        server.abort();
    }
}

#[tokio::test]
async fn unauthorized_sync_response_preserves_credentials_for_account_level_refresh() {
    let app = axum::Router::new().route("/api/sync/exchange", axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join(crate::APP_HIDDEN_DIR)).unwrap();
    let mut manager = test_manager(temp.path());
    manager.server_url = ServerUrl::parse(&format!("http://{address}")).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);

    let error = manager.sync_once().await.unwrap_err();

    assert!(matches!(error, SyncError::AuthenticationRequired));
    assert!(manager.is_signed_in());
    server.abort();
}
