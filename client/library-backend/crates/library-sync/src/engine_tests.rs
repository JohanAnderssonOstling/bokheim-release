use super::*;
use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use library_replica::LibraryStateEntry;
use std::{
    cell::RefCell,
    sync::{Arc, Mutex},
};
use sync_common::{LibraryRevision, SyncExchangeRequest, SyncExchangeResponse, SyncGeneration, Versioned};
use sync_transport::{SyncCredentials, Url};

struct MemoryState {
    events: Vec<&'static str>,
    issues: Vec<MutationIssue>,
    cursor: SyncCursor,
    recovering: bool,
    canonical: Vec<Versioned<LibraryStateEntry>>,
    outbox: Vec<StatePublication>,
}
struct MemoryStore(RefCell<MemoryState>);

struct DatabaseStore(library_database::Database);

#[tokio::test]
#[ignore = "requires the isolated server from client/app/tests/run_sync_e2e.sh"]
async fn production_http_postgres_directory_roundtrip_survives_restart() {
    use library_database::Database;
    use reqwest::Client;
    use std::env;
    use std::time::Duration;
    use sync_common::{DirId, ROOT_DIR_ID};

    let server: Url = env::var("SYNC_E2E_SERVER_URL").expect("run client/app/tests/run_sync_e2e.sh").parse().unwrap();
    let password = env::var("SYNC_E2E_ADMIN_PASSWORD").unwrap();
    let session = account_client::login(&server, "admin@bokheim.local", &password).await.unwrap();
    let library = LibraryId::new_v4();
    account_client::create_library(&session, library, format!("Release regression {library}")).await.unwrap();
    let credentials = SyncCredentials::new(server, session.token());
    let client = Client::new();
    let disk = tempfile::tempdir().unwrap();
    let first_path = disk.path().join("first.sqlite3");
    let first = DatabaseStore(Database::open(&first_path).unwrap());
    let first_replica = first.0.initialize_library().unwrap();
    let second = DatabaseStore(Database::open(disk.path().join("second.sqlite3")).unwrap());
    let second_replica = second.0.initialize_library().unwrap();
    let folder = DirId::new_v4();
    first.0.create_directory_with_id(&folder, &ROOT_DIR_ID, &"Initial".into()).unwrap();

    async fn sync(store: &DatabaseStore, replica_id: ReplicaId, client: &Client, library_id: LibraryId, credentials: &SyncCredentials) {
        let engine = SyncEngine::new(SyncEngineConfig { store, http_client: client, library_id, replica_id, credentials: Some(credentials.clone()) });
        let result = tokio::time::timeout(Duration::from_secs(30), engine.synchronize()).await.expect("real-server synchronization timed out").unwrap();
        assert!(result.mutation_issues.is_empty(), "{:?}", result.mutation_issues);
    }

    sync(&first, first_replica, &client, library, &credentials).await;
    sync(&second, second_replica, &client, library, &credentials).await;
    assert_eq!(second.0.child_directory(&ROOT_DIR_ID, "Initial").unwrap(), Some(folder));
    second.0.move_directory(&folder, None, Some("Renamed")).unwrap();
    sync(&second, second_replica, &client, library, &credentials).await;
    sync(&first, first_replica, &client, library, &credentials).await;
    assert_eq!(first.0.child_directory(&ROOT_DIR_ID, "Renamed").unwrap(), Some(folder));
    assert_eq!(first.0.child_directory(&ROOT_DIR_ID, "Initial").unwrap(), None);

    drop(first);
    let reopened = DatabaseStore(Database::open(&first_path).unwrap());
    assert_eq!(reopened.0.initialize_library().unwrap(), first_replica);
    sync(&reopened, first_replica, &client, library, &credentials).await;
    let cursor = reopened.0.sync_pull_cursor().unwrap();
    sync(&reopened, first_replica, &client, library, &credentials).await;
    assert_eq!(reopened.0.sync_pull_cursor().unwrap(), cursor);
    assert_eq!(reopened.0.child_directory(&ROOT_DIR_ID, "Renamed").unwrap(), Some(folder));
    assert!(reopened.0.sync_publishable_mutations().unwrap().is_empty());
    assert!(second.0.sync_publishable_mutations().unwrap().is_empty());
    account_client::delete_library(&session, &library).await.unwrap();
    account_client::logout(&session).await.unwrap();
}

fn database_result<T: 'static>(result: Result<T, library_database::DatabaseError>) -> StoreFuture<'static, T> {
    Box::pin(async move { result.map_err(|error| Box::new(error) as StoreError) })
}

impl SyncStore for &DatabaseStore {
    type Outbox = library_database::OutboxScan;
    fn prepare_outbox(&self) -> StoreFuture<'_, Self::Outbox> { database_result(self.0.sync_prepare_outbox()) }
    fn cursor_recovery_pending(&self) -> StoreFuture<'_, bool> { database_result(self.0.sync_cursor_recovery_pending()) }
    fn begin_cursor_recovery(&self) -> StoreFuture<'_, ()> { database_result(self.0.sync_begin_cursor_recovery()) }
    fn complete_cursor_recovery(&self) -> StoreFuture<'_, usize> {
        Box::pin(async {
            let (mut after, mut inserted) = (None, 0);
            loop {
                let (next, count) = self.0.sync_recover_state_page(after.as_ref())?;
                inserted += count;
                if next.is_none() { return Ok(inserted); }
                after = next;
            }
        })
    }
    fn pull_cursor(&self) -> StoreFuture<'_, SyncCursor> { database_result(self.0.sync_pull_cursor()) }
    fn pending_mutations<'a>(&'a self, scan: &'a mut Self::Outbox) -> StoreFuture<'a, Vec<StatePublication>> { database_result(self.0.sync_next_outbox_page(scan)) }
    fn record_mutation_issues(&self, issues: &[MutationIssue]) -> StoreFuture<'_, ()> { database_result(self.0.sync_record_mutation_issues(issues)) }
    fn acknowledge_mutations(&self, ids: &[ReplicaSeq]) -> StoreFuture<'_, ()> { database_result(self.0.sync_acknowledge_mutations(ids)) }
    fn apply_exchange_response(&self, ids: &[ReplicaSeq], page: &PullStateResponse) -> StoreFuture<'_, ()> { database_result(self.0.sync_commit_pull_response(ids, page).map(|_| ())) }
}

#[tokio::test]
#[ignore = "release benchmark; set SYNC_PIPELINE_BENCH_DB and SYNC_PIPELINE_BENCH_COUNT"]
async fn outbox_pipeline_performance() {
    use std::{hint::black_box, time::Instant};
    let db = library_database::Database::open(std::env::var("SYNC_PIPELINE_BENCH_DB").unwrap()).unwrap();
    db.initialize_library().unwrap();
    let expected: usize = std::env::var("SYNC_PIPELINE_BENCH_COUNT").unwrap().parse().unwrap();
    let store = DatabaseStore(db);
    let client = reqwest::Client::new();
    let engine = SyncEngine::new(SyncEngineConfig { store: &store, http_client: &client, library_id: LibraryId::from_u128(1), replica_id: ReplicaId::from_u128(2), credentials: None });
    let mut times = Vec::new();
    for _ in 0..8 {
        let started = Instant::now();
        let mut remaining = engine.prepare_outbox().await.unwrap();
        let mut result = SyncOutcome::default();
        let (mut count, mut checksum, mut batches) = (0, 0, 0);
        loop {
            let batch = engine.next_push_batch(&mut remaining, &mut result).await.unwrap();
            if batch.is_empty() { break; }
            count += batch.len();
            checksum += batch.iter().map(|p| p.publisher_seq.get()).sum::<u64>();
            batches += 1;
            black_box(batch);
        }
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        assert!(result.mutation_issues.is_empty());
        assert_eq!(count, expected);
        assert_eq!(checksum, expected as u64 * (expected as u64 + 1) / 2);
        black_box(batches);
    }
    times.remove(0);
    times.sort_by(f64::total_cmp);
    println!("PIPELINE count={expected} median_ms={:.3} samples={times:?}", times[3]);
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(line) = status.lines().find(|line| line.starts_with("VmHWM:")) { println!("PIPELINE {line}"); }
    }
}

impl SyncStore for &MemoryStore {
    type Outbox = std::vec::IntoIter<StatePublication>;
    fn prepare_outbox(&self) -> StoreFuture<'_, Self::Outbox> {
        Box::pin(async {
            let mut values = library_replica::coalesce_mutations_for_push(self.0.borrow().outbox.clone());
            values.sort_by_key(|p| !matches!(p.state.value, LibraryStateEntry::BookLifecycle { .. }));
            Ok(values.into_iter())
        })
    }
    fn record_mutation_issues(&self, issues: &[MutationIssue]) -> StoreFuture<'_, ()> {
        let mut state = self.0.borrow_mut();
        state.events.push("issues");
        state.issues.extend_from_slice(issues);
        Box::pin(async { Ok(()) })
    }
    fn cursor_recovery_pending(&self) -> StoreFuture<'_, bool> {
        Box::pin(async { Ok(self.0.borrow().recovering) })
    }
    fn begin_cursor_recovery(&self) -> StoreFuture<'_, ()> {
        Box::pin(async {
            let mut state = self.0.borrow_mut();
            state.recovering = true;
            state.cursor = SyncCursor::default();
            Ok(())
        })
    }
    fn complete_cursor_recovery(&self) -> StoreFuture<'_, usize> {
        Box::pin(async {
            let mut state = self.0.borrow_mut();
            let outbox = state.canonical.iter().enumerate().map(|(index, value)| StatePublication { publisher_seq: ReplicaSeq::new(index as u64 + 1_000).unwrap(), state: value.clone() }).collect::<Vec<_>>();
            let count = outbox.len();
            state.outbox = outbox;
            state.recovering = false;
            Ok(count)
        })
    }
    fn pull_cursor(&self) -> StoreFuture<'_, SyncCursor> {
        Box::pin(async { Ok(self.0.borrow().cursor) })
    }
    fn pending_mutations<'a>(&'a self, scan: &'a mut Self::Outbox) -> StoreFuture<'a, Vec<StatePublication>> {
        Box::pin(async { Ok(scan.by_ref().take(2).collect()) })
    }
    fn acknowledge_mutations(&self, ids: &[ReplicaSeq]) -> StoreFuture<'_, ()> {
        let ids = ids.to_vec();
        Box::pin(async move {
            let mut state = self.0.borrow_mut();
            state.events.push("acknowledge");
            state.outbox.retain(|row| !ids.contains(&row.publisher_seq));
            Ok(())
        })
    }
    fn apply_exchange_response(&self, ids: &[ReplicaSeq], page: &PullStateResponse) -> StoreFuture<'_, ()> {
        let ids = ids.to_vec();
        let page = page.clone();
        Box::pin(async move {
            self.acknowledge_mutations(&ids).await?;
            let mut state = self.0.borrow_mut();
            for delivery in &page.mutations {
                let value = LibraryStateEntry::decode(&delivery.state)?.unwrap();
                state.canonical.push(Versioned { value, version: delivery.state.version.clone() });
            }
            state.cursor = page.next_cursor;
            Ok(())
        })
    }
}

struct Server {
    generation: SyncGeneration,
    recovered: sync_common::StateDelivery,
    requests: Mutex<Vec<SyncExchangeRequest>>,
}

async fn exchange(State(server): State<Arc<Server>>, bytes: Bytes) -> Response {
    let request: SyncExchangeRequest = sync_common::transport::decode(&bytes, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
    let valid = request.cursor.belongs_to(server.generation);
    let needs_cell = request.cursor.field_revision.map_or(true, |revision| revision < server.recovered.revision);
    server.requests.lock().unwrap().push(request);
    if !valid {
        return StatusCode::GONE.into_response();
    }
    let head = LibraryRevision::new(1_100).unwrap();
    let response = SyncExchangeResponse {
        push: PushMutationsResponse { rejected: Vec::new() },
        pull: PullStateResponse {
            mutations: if needs_cell { vec![server.recovered.clone()] } else { Vec::new() },
            next_cursor: SyncCursor { generation: Some(server.generation), lifecycle_revision: Some(head), field_revision: Some(head) },
            has_more: false,
        },
    };
    sync_common::transport::encode(&response).unwrap().into_response()
}

#[tokio::test]
async fn history_mismatch_recovers_and_republishes_original_versions() {
    let old_history = SyncGeneration::from_u128(1);
    let new_history = SyncGeneration::from_u128(2);
    let retained = LibraryStateEntry::DirectoryName { dir_id: sync_common::DirId::from_u128(7), name: "Retained offline".into() }.publication(ReplicaId::from_u128(8), 50, ReplicaSeq::new(40).unwrap());
    let recovered = LibraryStateEntry::DirectoryName { dir_id: sync_common::DirId::from_u128(9), name: "Previously skipped".into() }.publication(ReplicaId::from_u128(10), 60, ReplicaSeq::new(41).unwrap());
    let old_head = LibraryRevision::new(1_000).unwrap();
    let store = MemoryStore(RefCell::new(MemoryState {
        events: Vec::new(),
        issues: Vec::new(),
        cursor: SyncCursor { generation: Some(old_history), lifecycle_revision: Some(old_head), field_revision: Some(old_head) },
        recovering: false,
        canonical: vec![retained.state.clone()],
        outbox: Vec::new(),
    }));
    let server = Arc::new(Server {
        generation: new_history,
        recovered: sync_common::StateDelivery { state: sync_common::transport::representation(&recovered.state).unwrap(), revision: LibraryRevision::new(900).unwrap() },
        requests: Mutex::new(Vec::new()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    let app = Router::new().route("/api/sync/exchange", post(exchange)).with_state(server.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let engine = SyncEngine::new(SyncEngineConfig { store: &store, http_client: &client, library_id: LibraryId::from_u128(11), replica_id: ReplicaId::from_u128(12), credentials: Some(SyncCredentials::new(url, "test-token")) });
    engine.synchronize().await.unwrap();
    task.abort();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].cursor.generation, Some(old_history));
    assert_eq!(requests[1].cursor, SyncCursor::default());
    assert!(requests[1].mutations.is_empty(), "recover server state before republishing");
    assert_eq!(requests[2].cursor.generation, Some(new_history));
    assert_eq!(requests[2].mutations.len(), 2);
    for expected in [&retained, &recovered] {
        let publication = requests[2].mutations.iter().find(|row| row.state.value.address == expected.state.value.address()).unwrap();
        assert_eq!(publication.state.version, expected.state.version, "recovery must not author a newer edit");
        assert_ne!(publication.publisher_seq, expected.publisher_seq);
    }
    let state = store.0.borrow();
    assert_eq!(state.cursor.generation, Some(new_history));
    assert!(state.outbox.is_empty());
    assert!(!state.recovering);
}

#[tokio::test]
async fn both_sync_modes_preserve_rejections_and_acknowledgements_when_pull_is_invalid() {
    use sync_common::{DirId, MutationRejection, MutationRejectionReason};
    let app = Router::new().route(
        "/api/sync/exchange",
        post(|body: Bytes| async move {
            let request: SyncExchangeRequest = sync_common::transport::decode_byte_fields(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
            assert_eq!(request.mutations.len(), 3);
            let response = SyncExchangeResponse {
                push: PushMutationsResponse {
                    rejected: vec![
                        MutationRejection { publisher_seq: ReplicaSeq::new(1).unwrap(), reason: MutationRejectionReason::InvalidPayload },
                        MutationRejection { publisher_seq: ReplicaSeq::new(2).unwrap(), reason: MutationRejectionReason::TimestampTooFarFuture },
                    ],
                },
                // Missing history makes this invalid for an active pull. An
                // upload-only library intentionally does not apply its pull.
                pull: PullStateResponse { mutations: Vec::new(), next_cursor: SyncCursor::default(), has_more: false },
            };
            sync_common::transport::encode(&response).unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    for active in [false, true] {
        let outbox = (1..=3).map(|seq| LibraryStateEntry::DirectoryName { dir_id: DirId::from_u128(seq.into()), name: format!("Shelf {seq}") }.publication(ReplicaId::from_u128(8), 1, ReplicaSeq::new(seq).unwrap())).collect();
        let store = MemoryStore(RefCell::new(MemoryState { events: Vec::new(), issues: Vec::new(), cursor: SyncCursor::default(), recovering: false, canonical: Vec::new(), outbox }));
        let engine = SyncEngine::new(SyncEngineConfig { store: &store, http_client: &client, library_id: LibraryId::from_u128(11), replica_id: ReplicaId::from_u128(12), credentials: Some(SyncCredentials::new(url.clone(), "test-token")) });
        if active {
            assert!(engine.exchange_events_with_server(SyncCursor::default()).await.is_err());
        } else {
            assert_eq!(engine.drain_outbox().await.unwrap().mutation_issues.len(), 2);
        }
        let state = store.0.borrow();
        assert_eq!(state.events, ["issues", "acknowledge"]);
        assert_eq!(state.issues.len(), 2);
        assert_eq!(state.outbox.len(), 1);
        assert_eq!(state.outbox[0].publisher_seq, ReplicaSeq::new(2).unwrap());
        assert_eq!(state.cursor, SyncCursor::default());
    }
    task.abort();
}

#[tokio::test]
async fn lifecycle_rejections_defer_fields_on_later_storage_pages_in_both_modes() {
    use library_replica::BookLifecycleState;
    use sync_common::{ContentHash, DirId, MutationRejection, MutationRejectionReason, MAX_PUSH_MUTATIONS};
    let book = ContentHash::from_bytes([7; 32]);
    let rejected_id = ReplicaSeq::new(MAX_PUSH_MUTATIONS as u64 + 5).unwrap();
    let field_id = ReplicaSeq::new(MAX_PUSH_MUTATIONS as u64 + 6).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<SyncExchangeRequest>::new()));
    let app = Router::new().route("/api/sync/exchange", post({
        let requests = requests.clone();
        move |bytes: Bytes| {
            let requests = requests.clone();
            async move {
                let request: SyncExchangeRequest = sync_common::transport::decode(&bytes, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
                let rejected = if request.mutations.iter().any(|p| p.publisher_seq == rejected_id) {
                    vec![MutationRejection { publisher_seq: rejected_id, reason: MutationRejectionReason::TimestampTooFarFuture }]
                } else { Vec::new() };
                requests.lock().unwrap().push(request);
                sync_common::transport::encode(&SyncExchangeResponse {
                    push: PushMutationsResponse { rejected },
                    pull: PullStateResponse { mutations: Vec::new(), next_cursor: SyncCursor { generation: Some(SyncGeneration::from_u128(1)), ..Default::default() }, has_more: false },
                }).unwrap()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    for active in [false, true] {
        requests.lock().unwrap().clear();
        let replica = ReplicaId::from_u128(8);
        let mut outbox: Vec<_> = (1..=MAX_PUSH_MUTATIONS as u64 + 4).map(|seq| LibraryStateEntry::DirectoryName { dir_id: DirId::from_u128(seq.into()), name: format!("Shelf {seq}") }.publication(replica, 1, ReplicaSeq::new(seq).unwrap())).collect();
        outbox.push(LibraryStateEntry::BookLifecycle { content_hash: book, value: BookLifecycleState::Present }.publication(replica, 1, rejected_id));
        outbox.push(LibraryStateEntry::Placement { dir_id: sync_common::ROOT_DIR_ID, content_hash: book, present: true, origin_folder_id: None }.publication(replica, 1, field_id));
        let store = MemoryStore(RefCell::new(MemoryState { events: Vec::new(), issues: Vec::new(), cursor: SyncCursor::default(), recovering: false, canonical: Vec::new(), outbox }));
        let engine = SyncEngine::new(SyncEngineConfig { store: &store, http_client: &client, library_id: LibraryId::from_u128(11), replica_id: ReplicaId::from_u128(12), credentials: Some(SyncCredentials::new(url.clone(), "test-token")) });
        let outcome = if active { engine.exchange_events_with_server(SyncCursor::default()).await } else { engine.drain_outbox().await }.unwrap();
        assert_eq!(outcome.mutation_issues.len(), 1);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].mutations[0].publisher_seq, rejected_id);
        assert!(requests.iter().all(|r| r.mutations.len() <= MAX_PUSH_MUTATIONS));
        assert!(requests.iter().flat_map(|r| &r.mutations).all(|p| p.publisher_seq != field_id));
        let state = store.0.borrow();
        assert_eq!(state.outbox.len(), 2);
        assert!(state.outbox.iter().any(|p| p.publisher_seq == field_id));
        assert!(state.outbox.iter().any(|p| p.publisher_seq == rejected_id));
    }
    task.abort();
}

#[tokio::test]
async fn oversized_values_do_not_block_later_storage_pages() {
    use sync_common::{DirId, MAX_PUSH_BATCH_BYTES};
    let outbox = (1..=5).map(|seq| LibraryStateEntry::DirectoryName {
        dir_id: DirId::from_u128(seq.into()),
        name: if seq == 1 { "x".repeat(MAX_PUSH_BATCH_BYTES + 1024) } else { format!("Shelf {seq}") },
    }.publication(ReplicaId::from_u128(8), 1, ReplicaSeq::new(seq).unwrap())).collect();
    let store = MemoryStore(RefCell::new(MemoryState { events: Vec::new(), issues: Vec::new(), cursor: SyncCursor::default(), recovering: false, canonical: Vec::new(), outbox }));
    let client = reqwest::Client::new();
    let engine = SyncEngine::new(SyncEngineConfig { store: &store, http_client: &client, library_id: LibraryId::from_u128(1), replica_id: ReplicaId::from_u128(2), credentials: None });
    let mut outgoing = engine.prepare_outbox().await.unwrap();
    let mut result = SyncOutcome::default();
    let batch = engine.next_push_batch(&mut outgoing, &mut result).await.unwrap();
    assert_eq!(batch.iter().map(|p| p.publisher_seq.get()).collect::<Vec<_>>(), [2, 3, 4, 5]);
    assert!(engine.next_push_batch(&mut outgoing, &mut result).await.unwrap().is_empty());
    assert_eq!(result.mutation_issues, [MutationIssue::Untransmittable(ReplicaSeq::new(1).unwrap())]);
    assert_eq!(store.0.borrow().issues, result.mutation_issues);
}
