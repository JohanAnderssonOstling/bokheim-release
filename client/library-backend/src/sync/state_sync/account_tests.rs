use super::*;
use crate::sync::LibrarySync;
use crate::sync::library_sync::LibrarySyncConfig;
use std::sync::{Arc, RwLock};
use sync_engine::SyncStore;

fn page(count: usize, revision: u64) -> PullStateResponse {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 10000 + revision;
    PullStateResponse {
        book_creations: (1..=count).map(|id| addition(sync_common::ContentHash::new(&format!("{id:064x}")), 0)).collect(),
        mutations: (1..=count)
            .map(|id| {
                let change = library_replica::StateMutation {
                    origin: None,
                    mutation_id: MutationId::new(),
                    body: library_replica::MutationBody::Metadata {
                        content_hash: sync_common::ContentHash::new(&format!("{id:064x}")),
                        value: library_replica::SyncBookMetadata { title: format!("Remote {revision}"), ..Default::default() },
                    },
                    changed_at: now,
                    replica_seq: sync_common::ReplicaSeq::new(revision + id as u64).unwrap(),
                };
                sync_common::ServerMutation { mutation: change.to_wire().unwrap(), replica_id: test_support::fixture_replica_id("combined"), revision: sync_common::LibraryRevision::new(revision).unwrap() }
            })
            .collect(),
        next_cursor: SyncCursor { state_revision: Some(sync_common::LibraryRevision::new(revision).unwrap()), reading_revision: None },
        has_more: false,
    }
}

fn addition(hash: sync_common::ContentHash, changed_at: u64) -> sync_common::ServerMutation {
    let change = library_replica::StateMutation {
        origin: None,
        mutation_id: MutationId::new(),
        body: library_replica::MutationBody::BookLifecycle { content_hash: hash, value: library_replica::BookLifecycleState::Present },
        changed_at,
        replica_seq: sync_common::ReplicaSeq::new(changed_at.max(1)).unwrap(),
    };
    sync_common::ServerMutation { mutation: change.to_wire().unwrap(), replica_id: test_support::fixture_replica_id("combined"), revision: sync_common::LibraryRevision::new(1).unwrap() }
}

/// Raw second connection for fixtures and assertions only. Production paths
/// below only touch the owner.
fn raw(db_path: &std::path::Path) -> library_database::rusqlite::Connection {
    // A second connection to the same file: distinct from the owner's, but
    // triggers on tables like `book` still call `bokheim_mutation`, so this
    // connection needs the custom function registered too.
    library_database::open_fixture_connection(db_path).unwrap()
}

/// Test handle pairing the session-owned database with its sync worker.
/// Method calls dereference to `LibrarySync`; storage travels explicitly.
struct TestSync {
    sync: LibrarySync,
    database: library_database::Database,
}

impl std::ops::Deref for TestSync {
    type Target = LibrarySync;
    fn deref(&self) -> &LibrarySync {
        &self.sync
    }
}

fn test_sync(root: &std::path::Path) -> TestSync {
    let db_path = root.join("library.db");
    let database = library_database::Database::open(&db_path).unwrap();
    database.initialize_library().unwrap();
    let assets = crate::asset_store::open(root.to_str().unwrap()).unwrap();
    let sync = LibrarySync::new(LibrarySyncConfig {
        database_path: db_path.clone(),
        assets,
        replica_id: test_support::fixture_replica_id("account-tests"),
        server_url: account_client::ServerUrl::parse("http://127.0.0.1:9").unwrap(),
        library_id: test_support::fixture_library_id("library"),
        event_tx: None,
        transfer_queue: Default::default(),
        credentials: Arc::new(RwLock::new(None::<sync_transport::SyncCredentials>)),
        account: None,
        notification_interest: Default::default(),
        cpu: crate::default_cpu_host(),
    });
    TestSync { sync, database }
}

fn outbox_ids(db_path: &std::path::Path) -> Vec<String> {
    raw(db_path).prepare("SELECT mutation_id FROM sync_outbox ORDER BY replica_seq").unwrap().query_map([], |row| row.get(0)).unwrap().collect::<Result<Vec<String>, _>>().unwrap()
}

#[tokio::test]
async fn combined_response_failure_retains_ack_but_rolls_back_pull_and_cursor() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("library.db");
    let sync = test_sync(root.path());
    let hash = format!("{:064x}", 1);
    {
        let conn = raw(&db_path);
        conn.execute("INSERT INTO book(content_hash, title, format) VALUES (?1, 'Local', 'epub')", [hash.as_str()]).unwrap();
        // Whatever the page would record first is gone, so the whole commit
        // fails and rolls back the acknowledgement with it.
        conn.execute_batch("DROP TABLE sync_state_version").unwrap();
    }
    let ids: Vec<MutationId> = outbox_ids(&db_path).into_iter().map(|id| MutationId::parse(&id).unwrap()).collect();
    // The insert queues metadata, lifecycle, and independent book facts.
    assert_eq!(ids.len(), 3);
    let store = AccountStore { sync: &sync.sync };
    assert!(store.apply_exchange_response(&ids, &page(1, 1)).await.is_err());
    assert!(outbox_ids(&db_path).is_empty(), "the acknowledgement is retried under a fresh transaction");
    let conn = raw(&db_path);
    assert_eq!(conn.query_row("SELECT title FROM book WHERE content_hash = ?1", [&hash], |row| row.get::<_, String>(0)).unwrap(), "Local");
    assert_eq!(sync.database.sync_pull_cursor().unwrap(), SyncCursor::default());
    assert_ne!(conn.query_row("SELECT change_origin FROM sync_metadata WHERE singleton = 1", [], |row| row.get::<_, Option<String>>(0)).unwrap(), Some("remote".to_owned()));
}

#[tokio::test]
async fn combined_response_applies_regardless_of_account() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("library.db");
    let sync = test_sync(root.path());
    {
        let conn = raw(&db_path);
        conn.execute("INSERT INTO sync_outbox(mutation_id, state_kind, state_key, state_subkey, body, changed_at) VALUES ('00000000-0000-0000-0000-000000000002', 'metadata', ?1, '', X'00', 10)", [format!("{:064x}", 1).as_str()]).unwrap();
        conn.execute("INSERT INTO sync_outbox(mutation_id, state_kind, state_key, state_subkey, body, changed_at) VALUES ('00000000-0000-0000-0000-000000000003', 'metadata', ?1, '', X'00', 20)", [format!("{:064x}", 2).as_str()]).unwrap();
    }
    let store = AccountStore { sync: &sync.sync };
    let ids: Vec<MutationId> = outbox_ids(&db_path).into_iter().map(|id| MutationId::parse(&id).unwrap()).collect();
    let old = ids[0];
    let newer = ids[1];
    store.apply_exchange_response(&[old], &page(0, 1)).await.unwrap();
    assert_eq!(outbox_ids(&db_path), vec![newer.to_string()]);
    // No account is bound: applying a response created for another account
    // still advances the shared cursor and acknowledges the mutations.
    store.apply_exchange_response(&[newer], &page(1, 2)).await.unwrap();
    assert!(outbox_ids(&db_path).is_empty());
    assert_eq!(sync.database.sync_pull_cursor().unwrap(), page(1, 2).next_cursor);
}

#[tokio::test]
async fn native_recovery_resumes_committed_pages() {
    let root = tempfile::tempdir().unwrap();
    let sync = test_sync(root.path());
    // Remote additions write versions without outbox work, so recovery has
    // exactly the committed pages to re-enqueue.
    let incoming: Vec<sync_common::ServerMutation> = (1..=300).map(|id| addition(sync_common::ContentHash::new(&format!("{id:064x}")), 1000 + id as u64)).collect();
    let next = SyncCursor { state_revision: Some(sync_common::LibraryRevision::new(9).unwrap()), reading_revision: None };
    assert!(sync.database.sync_commit_pull_response(&[], &PullStateResponse { book_creations: incoming.clone(), mutations: incoming, next_cursor: next, has_more: false }).unwrap());
    assert!(sync.database.sync_publishable_mutations().unwrap().is_empty());
    sync.database.sync_begin_cursor_recovery().unwrap();
    let store = AccountStore { sync: &sync.sync };
    assert_eq!(store.complete_cursor_recovery().await.unwrap(), 300);
    assert!(!sync.database.sync_cursor_recovery_pending().unwrap());
    assert_eq!(sync.database.sync_publishable_mutations().unwrap().len(), 300);
}

#[tokio::test]
async fn inventory_checkpoint_and_cursor_survive_account_switch() {
    let root = tempfile::tempdir().unwrap();
    let sync = test_sync(root.path());
    let hash = sync_common::ContentHash::new(&format!("{:064x}", 7));
    let next = SyncCursor { state_revision: Some(sync_common::LibraryRevision::new(9).unwrap()), reading_revision: None };
    assert!(sync.database.sync_commit_pull_response(&[], &PullStateResponse { book_creations: vec![addition(hash, 100)], mutations: vec![addition(hash, 100)], next_cursor: next, has_more: false }).unwrap());
    let store = AccountStore { sync: &sync.sync };
    let cell = store.inventory_page(None).await.unwrap().into_iter().next().unwrap();
    store.save_inventory_checkpoint(Some(cell.clone())).await.unwrap();
    // Switching accounts is a non-event: the checkpoint, cursor, and repair
    // machinery keep working against the same durable state.
    assert_eq!(sync.database.sync_inventory_checkpoint().unwrap(), Some(cell.clone()));
    store.save_inventory_checkpoint(Some(cell.clone())).await.unwrap();
    store.begin_cursor_recovery().await.unwrap();
    assert!(sync.database.sync_cursor_recovery_pending().unwrap());
}
