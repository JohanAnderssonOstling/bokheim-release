use server_postgres::{migrate, rotate_sync_generation, upgrade_existing, verify_schema, PostgresDatabase, PostgresSyncRepository, SyncError};
use sqlx::{postgres::PgPoolOptions, PgPool};
use sync_common::api::libraries::LibraryNameRequest;
use sync_common::{ConflictRank, LibraryId, LibraryRevision, OpaquePublication, OpaqueStateEntry, ReplicaId, ReplicaSeq, StateAddress, SyncCursor, SyncExchangeRequest, SyncGeneration, VersionKey, Versioned, MAX_PUSH_MUTATIONS};

/// Every test owns a schema; never alter the caller's existing tables.
async fn test_database() -> (PostgresDatabase, PgPool, String) {
    let url = std::env::var("SYNC_E2E_DATABASE_URL").expect("set SYNC_E2E_DATABASE_URL to a local test database");
    let admin = PgPool::connect(&url).await.unwrap();
    let schema = format!("sync_history_{}", SyncGeneration::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await.unwrap();
    let search_path = format!("SET search_path TO {schema}");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let statement = search_path.clone();
            Box::pin(async move { sqlx::query(&statement).execute(connection).await.map(|_| ()) })
        })
        .connect(&url)
        .await
        .unwrap();
    let database = PostgresDatabase::from_pool(pool);
    migrate(&database).await.unwrap();
    (database, admin, schema)
}

async fn cleanup(database: PostgresDatabase, admin: PgPool, schema: String) {
    database.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(&admin).await.unwrap();
    admin.close().await;
}

fn publication(replica: ReplicaId, sequence: u64, key: String) -> OpaquePublication {
    let sequence = ReplicaSeq::new(sequence).unwrap();
    OpaquePublication {
        publisher_seq: sequence,
        state: Versioned {
            version: VersionKey::new(1, ConflictRank::Update, replica, sequence),
            value: OpaqueStateEntry { address: StateAddress { kind: "directory_name".into(), entity_key: key.into(), entity_subkey: String::new() }, book: None, bytes: sync_common::transport::encode("Restored history").unwrap() },
        },
    }
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn restored_history_rejects_old_cursors_even_after_passing_their_revision() {
    let (database, admin, schema) = test_database().await;
    let repository = PostgresSyncRepository::new(database.clone());
    let user = "history-test-user";
    sqlx::query("INSERT INTO users(id) VALUES($1)").bind(user).execute(database.pool()).await.unwrap();
    let library = LibraryId::new_v4();
    let replica = ReplicaId::new_v4();
    repository.create_library(user, &LibraryNameRequest { library_id: library, library_name: "History".into(), storage: Default::default() }).await.unwrap();
    let empty = repository.exchange(user, &SyncExchangeRequest { library_id: library, replica_id: replica, mutations: Vec::new(), cursor: SyncCursor::default() }).await.unwrap();
    assert!(empty.pull.next_cursor.generation.is_some(), "even an empty library binds its checkpoint to the history");
    assert!(empty.pull.next_cursor.lifecycle_revision.is_none());
    assert!(empty.pull.next_cursor.field_revision.is_none());
    sync_common::validate_pull_batch(&empty.pull, SyncCursor::default()).unwrap();
    sqlx::query("SELECT setval('sync_server_sequence', 999)").execute(database.pool()).await.unwrap();
    let mut request = SyncExchangeRequest { library_id: library, replica_id: replica, mutations: vec![publication(replica, 1, "Before restore".into())], cursor: SyncCursor::default() };
    let old = repository.exchange(user, &request).await.unwrap().pull.next_cursor;
    assert_eq!(old.generation, empty.pull.next_cursor.generation);
    assert_eq!(old.field_revision.unwrap().get(), 1_000);

    // Simulate restoring an older snapshot and its delivery sequence. The
    // restore procedure explicitly rotates the history before traffic resumes.
    sqlx::raw_sql("DELETE FROM sync_state; DELETE FROM sync_library_revision; SELECT setval('sync_server_sequence', 800);").execute(database.pool()).await.unwrap();
    let generation = rotate_sync_generation(&database).await.unwrap();
    assert_ne!(Some(generation), old.generation);
    // More than the old 1,000-cell pull cap, including cells below the checkpoint.
    let new_count = 1_201;
    for start in (0..new_count).step_by(MAX_PUSH_MUTATIONS) {
        request.mutations = (start..(start + MAX_PUSH_MUTATIONS).min(new_count)).map(|index| publication(replica, index as u64 + 2, format!("new-{index:04}"))).collect();
        request.cursor = SyncCursor::default();
        repository.exchange(user, &request).await.unwrap();
    }
    let head: i64 = sqlx::query_scalar("SELECT current_revision FROM sync_library_revision").fetch_one(database.pool()).await.unwrap();
    assert!(head > 1_000);

    request.cursor = old;
    request.mutations.clear();
    assert!(matches!(repository.exchange(user, &request).await, Err(SyncError::HistoryMismatch)));
    request.mutations.push(publication(replica, 9_000, "Must not commit".into()));
    assert!(matches!(repository.exchange(user, &request).await, Err(SyncError::HistoryMismatch)));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sync_state").fetch_one(database.pool()).await.unwrap();
    assert_eq!(count, new_count as i64, "a rejected push must not write or acknowledge its publications");
    request.cursor.generation = None;
    assert!(matches!(repository.exchange(user, &request).await, Err(SyncError::HistoryMismatch)), "legacy nonzero cursors must reconcile");
    let ahead = LibraryRevision::new(head as u64 + 1).unwrap();
    request.cursor = SyncCursor { generation: Some(generation), lifecycle_revision: Some(ahead), field_revision: Some(ahead) };
    assert!(matches!(repository.exchange(user, &request).await, Err(SyncError::CursorAhead)), "generation binding must retain numeric cursor validation");

    request.cursor = SyncCursor::default();
    request.mutations.clear();
    let mut delivered = Vec::new();
    let mut pages = 0;
    loop {
        let response = repository.exchange(user, &request).await.unwrap();
        sync_common::validate_pull_batch(&response.pull, request.cursor).unwrap();
        assert_eq!(response.pull.next_cursor.generation, Some(generation));
        request.cursor = response.pull.next_cursor;
        delivered.extend(response.pull.mutations);
        pages += 1;
        if !response.pull.has_more {
            break;
        }
    }
    assert_eq!(delivered.len(), new_count);
    assert_eq!(pages, 1, "small states must not split at an arbitrary change count");
    assert!(delivered.iter().any(|row| row.revision.get() <= old.field_revision.unwrap().get()), "recovery must include cells hidden behind the old checkpoint");
    assert!(delivered.iter().all(|row| row.state.version.changed_at == 1 && row.state.version.replica_id == replica));
    cleanup(database, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn generation_survives_startup_and_upgrades_and_migrates_existing_state() {
    let (database, admin, schema) = test_database().await;
    let read = || async { sqlx::query_scalar::<_, SyncGeneration>("SELECT generation FROM sync_history").fetch_one(database.pool()).await.unwrap() };
    let initial = read().await;
    verify_schema(&database).await.unwrap();
    upgrade_existing(&database).await.unwrap();
    assert_eq!(read().await, initial);
    let replacement = rotate_sync_generation(&database).await.unwrap();
    verify_schema(&database).await.unwrap();
    upgrade_existing(&database).await.unwrap();
    assert_eq!(read().await, replacement);

    // Version 10 predates both the history singleton and storage providers.
    sqlx::raw_sql("DROP TABLE sync_history; ALTER TABLE libraries DROP COLUMN storage_configuration; ALTER TABLE bokheim_schema_version DROP CONSTRAINT bokheim_schema_version_version_check; UPDATE bokheim_schema_version SET version=10; ALTER TABLE bokheim_schema_version ADD CONSTRAINT bokheim_schema_version_version_check CHECK(version=10);").execute(database.pool()).await.unwrap();
    sqlx::query("INSERT INTO users(id) VALUES('migration-user')").execute(database.pool()).await.unwrap();
    upgrade_existing(&database).await.unwrap();
    let migrated = read().await;
    assert!(!migrated.is_nil());
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE id='migration-user'").fetch_one(database.pool()).await.unwrap(), 1);
    upgrade_existing(&database).await.unwrap();
    assert_eq!(read().await, migrated);
    cleanup(database, admin, schema).await;
}
