use super::*;
use account_client::{Session, SessionPersistence};
use axum::{body::Bytes, extract::State, http::HeaderMap, routing::post, Router};
use client_platform_runtime::executor::BoxedBackendFuture;
use cpu_host::{BackendError, BookTask, BoxedCpuReader, CpuHost, CpuRequest, CpuResponse};
use library_runtime::{account::LibraryAccountSession, events::LibraryEventState, scheduler::SyncScheduler};
use std::{
    io,
    sync::{atomic::AtomicUsize, Mutex, RwLock},
};
use sync_common::{DirId, PullStateResponse, PushMutationsResponse, SyncCursor, SyncExchangeRequest, SyncExchangeResponse, SyncGeneration, ROOT_DIR_ID};
use sync_transport::CredentialsSource;

struct NoCpu;
impl CpuHost for NoCpu {
    fn book(&self, _: BookTask, _: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
        panic!("state sync must not run CPU work");
    }
    fn request(&self, _: CpuRequest) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
        panic!("state sync must not run CPU work");
    }
}

#[derive(Clone, Debug)]
struct Persistence;
impl SessionPersistence for Persistence {
    fn load_account_session(&self) -> io::Result<Option<Session>> {
        Ok(None)
    }
    fn save_account_session(&self, _: &Session) -> io::Result<()> {
        Ok(())
    }
    fn clear_account_session(&self) -> io::Result<()> {
        Ok(())
    }
}

fn fixture(server_url: Url, credentials: SharedSyncCredentials, account: Option<LibraryAccount>) -> (tempfile::TempDir, LibrarySync, LibraryEventState) {
    let root = tempfile::tempdir().unwrap();
    let database = Arc::new(Database::open(root.path().join("library.sqlite3")).unwrap());
    let replica_id = database.initialize_library().unwrap();
    database.create_directory_with_id(&DirId::from_u128(1), &ROOT_DIR_ID, &"Offline shelf".into()).unwrap();
    database.set_sync_metadata("last_successful_sync_at", "previous").unwrap();
    let (events, state) = LibraryEventRouter::new(SyncScheduler::new().0, SyncScheduler::new().0, SyncScheduler::new().0, SyncScheduler::new().0);
    let sync = LibrarySync::new(LibrarySyncConfig {
        database,
        assets: AssetStore::open(root.path().to_str().unwrap()).unwrap(),
        replica_id,
        server_url,
        library_id: LibraryId::new_v4(),
        events: Some(events),
        credentials,
        account,
        notification_interest: Default::default(),
        cpu: Arc::new(NoCpu),
    });
    (root, sync, state)
}

#[tokio::test]
async fn signed_out_passes_leave_queued_changes_and_sync_status_untouched() {
    for with_account in [false, true] {
        let server: Url = "https://example.com".parse().unwrap();
        let session = LibraryAccountSession::load(Persistence, server.clone()).unwrap();
        let account = with_account.then(|| session.library_account());
        let credentials: SharedSyncCredentials = account.as_ref().map_or_else(|| Arc::new(RwLock::new(None)) as SharedSyncCredentials, LibraryAccount::credentials);
        let (root, sync, mut state) = fixture(server, credentials, account);
        let updates = state.subscribe_updates();
        let watermark = sync.database.sync_outbox_watermark().unwrap();
        assert!(watermark > 0);
        let probe = rusqlite::Connection::open(root.path().join("library.sqlite3")).unwrap();
        probe.execute_batch("CREATE TRIGGER reject_signed_out_preparation BEFORE UPDATE ON sync_outbox BEGIN SELECT RAISE(ABORT, 'signed-out sync must not prepare the outbox'); END;").unwrap();
        for outbox_only in [false, true] {
            assert!(sync.synchronize_state_mode(outbox_only).await.unwrap().mutation_issues.is_empty());
            state.drain(sync.events.as_ref().unwrap());
            assert!(updates.try_recv().is_err());
            assert_eq!(sync.database.sync_outbox_watermark().unwrap(), watermark);
            assert_eq!(sync.database.sync_pull_cursor().unwrap(), SyncCursor::default());
            assert_eq!(sync.database.sync_metadata("last_successful_sync_at").unwrap().as_deref(), Some("previous"));
        }
    }
}

#[tokio::test]
async fn identity_snapshots_follow_account_switches_and_reject_another_server() {
    let server: Url = "https://example.com".parse().unwrap();
    let account = LibraryAccountSession::load(Persistence, server.clone()).unwrap();
    let session = |user: &str, token: char| Session::try_new(server.clone(), user.into(), format!("{user}@example.com"), token.to_string().repeat(43)).unwrap();
    account.set(session("first", 'a')).unwrap();
    let capability = account.library_account();
    let (_root, mut sync, mut state) = fixture(server.clone(), capability.credentials(), Some(capability));
    let updates = state.subscribe_updates();
    let watermark = sync.database.sync_outbox_watermark().unwrap();
    let (first_user, first_credentials) = sync.sync_identity().unwrap().unwrap();
    account.set(session("second", 'b')).unwrap();
    let (second_user, second_credentials) = sync.sync_identity().unwrap().unwrap();
    assert_eq!((first_user.as_str(), first_credentials.access_token()), ("first", "a".repeat(43).as_str()));
    assert_eq!((second_user.as_str(), second_credentials.access_token()), ("second", "b".repeat(43).as_str()));
    assert_eq!(second_credentials.server_url(), &server);
    sync.server_url = "https://other.example".parse().unwrap();
    for outbox_only in [false, true] {
        let error = sync.synchronize_state_mode(outbox_only).await.unwrap_err();
        assert!(error.to_string().contains("account belongs to another server"));
    }
    state.drain(sync.events.as_ref().unwrap());
    assert!(updates.try_recv().is_err());
    assert_eq!(sync.database.sync_outbox_watermark().unwrap(), watermark);
    assert_eq!(sync.database.sync_metadata("last_successful_sync_at").unwrap().as_deref(), Some("previous"));
    account.take().unwrap();
    assert!(sync.sync_identity().unwrap().is_none());
}

/// Each pass can acquire credentials once. A second read would lose them,
/// exposing an accidental sign-in check followed by a separate snapshot.
#[derive(Debug)]
struct OncePerPass {
    credentials: SyncCredentials,
    reads: AtomicUsize,
}
impl CredentialsSource for OncePerPass {
    fn credentials(&self) -> Option<SyncCredentials> {
        (self.reads.fetch_add(1, Ordering::SeqCst) == 0).then(|| self.credentials.clone())
    }
}

async fn exchange(State(requests): State<Arc<Mutex<Vec<SyncExchangeRequest>>>>, headers: HeaderMap, body: Bytes) -> Vec<u8> {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer captured-token");
    let request = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
    requests.lock().unwrap().push(request);
    sync_common::transport::encode(&SyncExchangeResponse {
        push: PushMutationsResponse { rejected: Vec::new() },
        pull: PullStateResponse { mutations: Vec::new(), next_cursor: SyncCursor { generation: Some(SyncGeneration::from_u128(7)), ..Default::default() }, has_more: false },
    })
    .unwrap()
}

#[tokio::test]
async fn both_modes_use_one_credential_snapshot_and_only_full_sync_advances_the_checkpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route("/api/sync/exchange", post(exchange)).with_state(requests.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let credentials = Arc::new(OncePerPass { credentials: SyncCredentials::new(url.clone(), "captured-token"), reads: AtomicUsize::new(0) });
    let (_root, sync, mut state) = fixture(url, credentials.clone(), None);
    let updates = state.subscribe_updates();
    for outbox_only in [true, false] {
        credentials.reads.store(0, Ordering::SeqCst);
        assert!(sync.synchronize_state_mode(outbox_only).await.unwrap().mutation_issues.is_empty());
        assert_eq!(credentials.reads.load(Ordering::SeqCst), 1);
        assert!(sync.database.sync_publishable_mutations().unwrap().is_empty());
        let checkpoint = sync.database.sync_metadata("last_successful_sync_at").unwrap().unwrap();
        if outbox_only {
            assert_eq!(checkpoint, "previous");
            assert_eq!(sync.database.sync_pull_cursor().unwrap(), SyncCursor::default());
        } else {
            assert!(checkpoint.parse::<u64>().unwrap() > 0);
            assert_eq!(sync.database.sync_pull_cursor().unwrap().generation, Some(SyncGeneration::from_u128(7)));
        }
        state.drain(sync.events.as_ref().unwrap());
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::SyncStatusChanged);
        assert!(updates.try_recv().is_err());
    }
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].mutations.is_empty());
    assert!(requests[1].mutations.is_empty());
    drop(requests);
    server.abort();
    let _ = server.await;
}
