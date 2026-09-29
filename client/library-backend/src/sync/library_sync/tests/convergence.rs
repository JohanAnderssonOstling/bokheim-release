//! Real HTTP/PostgreSQL convergence tests. Run with client/app/tests/run_sync_e2e.sh.
//! Only scheduling and process restarts are simulated; all merges, publication,
//! acknowledgements, cursors and projections use production implementations.
use super::*;
use book_model::ReaderAnnotation;
use rusqlite::types::Value;

struct Cluster {
    disk: tempfile::TempDir,
    session: account_client::Session,
    library: sync_common::LibraryId,
}

impl Cluster {
    async fn new() -> Self {
        let url = ServerUrl::parse(&std::env::var("SYNC_E2E_SERVER_URL").expect("run client/app/tests/run_sync_e2e.sh")).unwrap();
        let password = std::env::var("SYNC_E2E_ADMIN_PASSWORD").expect("runner supplies admin password");
        let session = account_client::login(&url, "admin@bokheim.local", &password).await.unwrap();
        let library = uuid::Uuid::new_v4();
        account_client::create_library(&session, library, format!("Convergence {library}")).await.unwrap();
        // This suite tests state; bytes and upload admission have separate server tests.
        account_client::set_cloud_storage(&session, library, sync_common::api::libraries::CloudStorageChange { change_id: uuid::Uuid::new_v4(), enabled: false }).await.unwrap();
        Self { disk: tempfile::tempdir().unwrap(), session, library }
    }

    fn device(&self, name: &str) -> TestManager {
        let root = self.disk.path().join(name);
        std::fs::create_dir_all(root.join(crate::APP_HIDDEN_DIR)).unwrap();
        let mut device = test_manager_with_replica(&root, name);
        // The database's canonical author and the HTTP publication author must agree.
        library_database::configure_fixture_connection(&device.database, |conn| {
            conn.execute("UPDATE sync_metadata SET replica_id=?1", [device.replica_id.to_string()])?;
            Ok(())
        })
        .unwrap();
        device.library_id = self.library;
        device.server_url = self.session.server_url().clone();
        device.credentials = shared_credentials(session_credentials(&self.session));
        device
    }

    async fn close(self) {
        account_client::delete_library(&self.session, &self.library).await.unwrap();
        account_client::logout(&self.session).await.unwrap();
    }
}

fn rows(db: &Database, sql: &str) -> Vec<Vec<Value>> {
    let mut result = Vec::new();
    library_database::configure_fixture_connection(db, |conn| {
        let mut statement = conn.prepare(sql)?;
        let columns = statement.column_count();
        result = statement.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())?.collect::<rusqlite::Result<_>>()?;
        Ok(())
    })
    .unwrap();
    result
}

// Physical filenames are deliberately local: projection retains an existing name
// and uses the hash for a remotely created placement (project_book in apply/pull.rs).
fn snapshot(device: &TestManager) -> Vec<Vec<Vec<Value>>> {
    [
        // Compare original winner identities, never publication IDs or row IDs.
        "SELECT state_kind,state_key,state_subkey,changed_at,conflict_rank,replica_id,replica_seq,mutation_id,body,book_key FROM sync_state_version ORDER BY state_kind,state_key,state_subkey",
        "SELECT id,parent_id,name,intent_parent_id,intent_name,intent_lifecycle,deleted_at,purged_at FROM dir ORDER BY id",
        "SELECT content_hash,title,subtitle,book_metadata,format,read_pos,read_progress,added_at,deleted_at,trash_origin_dir_id FROM book ORDER BY content_hash",
        "SELECT b.content_hash,p.dir_id,p.deleted_at,p.trash_origin_dir_id FROM book_dir p JOIN book b ON b.row_id=p.book_row_id ORDER BY b.content_hash,p.dir_id",
        "SELECT a.id,b.content_hash,a.toc_ordinal,a.progress,a.detail,a.modified_at,a.deleted_at FROM annotation a JOIN book b ON b.row_id=a.book_row_id ORDER BY a.id",
    ]
    .iter()
    .map(|sql| rows(&device.database, sql))
    .collect()
}

async fn sync(device: &TestManager) {
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), device.synchronize_state()).await.expect("sync timeout").unwrap();
    assert!(outcome.mutation_issues.is_empty(), "{:?}", outcome.mutation_issues);
}

fn assert_rooted_tree(device: &TestManager) {
    let mut parents = std::collections::BTreeMap::<String, String>::new();
    library_database::configure_fixture_connection(&device.database, |conn| {
        parents = conn.prepare("SELECT id,parent_id FROM dir WHERE deleted_at IS NULL AND purged_at IS NULL")?.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(())
    })
    .unwrap();
    let root = crate::ROOT_DIR_ID.to_string();
    for start in parents.keys() {
        let mut current = start;
        let mut visited = std::collections::HashSet::new();
        while current != &root {
            assert!(visited.insert(current), "visible folder cycle from {start}: {visited:?}");
            current = parents.get(current).expect("visible folder has a missing or suppressed parent");
        }
    }
}

async fn settle(devices: &[&TestManager]) {
    // Empty queues alone do not imply convergence: a later device may have
    // published a field after an earlier device finished pulling.
    for round in 0..12 {
        for device in devices {
            sync(device).await;
        }
        let expected = snapshot(devices[0]);
        // Republishing an unchanged winner advances delivery without changing
        // canonical state. Earlier devices still need to consume those pages.
        let expected_cursor = devices[0].database.sync_pull_cursor().unwrap();
        if devices.iter().all(|d| d.database.sync_publishable_mutations().unwrap().is_empty() && snapshot(d) == expected && d.database.sync_pull_cursor().unwrap() == expected_cursor) {
            let cursors = devices.iter().map(|d| d.database.sync_pull_cursor().unwrap()).collect::<Vec<_>>();
            for (device, cursor) in devices.iter().zip(cursors) {
                sync(device).await;
                assert_rooted_tree(device);
                assert_eq!(snapshot(device), expected, "extra sync changed state");
                assert!(device.database.sync_publishable_mutations().unwrap().is_empty(), "publication loop");
                assert_eq!(device.database.sync_pull_cursor().unwrap(), cursor, "extra sync advanced server revision");
            }
            return;
        }
        if round == 11 {
            for (index, device) in devices.iter().enumerate() {
                let actual = snapshot(device);
                for (table, (left, right)) in ["canonical", "directories", "books", "placements", "annotations"].iter().zip(expected.iter().zip(&actual)) {
                    if left != right {
                        let first = (0..left.len().max(right.len())).find(|&i| left.get(i) != right.get(i)).unwrap();
                        panic!("device {index} diverged in {table}: counts {} / {}, first differing row {first}: {:?} / {:?}", left.len(), right.len(), left.get(first), right.get(first));
                    }
                }
                assert!(device.database.sync_publishable_mutations().unwrap().is_empty(), "device {index} keeps publishing: {:?}", device.database.sync_publishable_mutations().unwrap());
                assert_eq!(device.database.sync_pull_cursor().unwrap(), expected_cursor, "device {index} has not reached the same delivery revision");
            }
            unreachable!("non-convergence must differ in state, pending work or cursors");
        }
    }
}

fn add(device: &TestManager) {
    device.database.seed_book(&test_content_hash(), Some("Convergence"), 1, "epub");
    device.database.restore_book_placement(&test_content_hash(), &crate::ROOT_DIR_ID).unwrap();
}

fn annotate(device: &TestManager, note: &str, time: i64) {
    device
        .database
        .upsert_annotation(&ReaderAnnotation {
            id: "shared-note".into(),
            content_hash: test_content_hash(),
            anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/2)"),
            exact_text: "Passage".into(),
            style: AnnotationStyle::Underline,
            color: "#ffffff".into(),
            note: note.into(),
            created_at: 10,
            modified_at: time,
            toc_ordinal: Some(0),
            progress: Some(0.25),
        })
        .unwrap();
}

#[tokio::test]
#[ignore = "requires release HTTP server and PostgreSQL; run client/app/tests/run_sync_e2e.sh"]
async fn production_http_postgres_missed_purge_readd_converges_after_restart() {
    let cluster = Cluster::new().await;
    let a = cluster.device("a");
    let b = cluster.device("b");
    add(&a);
    annotate(&a, "acknowledged before purge", 20);
    a.database.update_reading_position(&test_content_hash(), "epubcfi(/6/2)", Some(0.25), None).unwrap();
    settle(&[&a, &b]).await;
    assert_eq!(a.database.annotations(test_content_hash()).unwrap().len(), 1);
    drop(a); // Offline device misses two entire lifetimes, including their removals.
    for _ in 0..2 {
        b.database.trash_book(&test_content_hash()).unwrap();
        b.database.purge_book(&test_content_hash()).unwrap();
        sync(&b).await;
        assert_eq!(b.database.book_count(&test_content_hash()), 0);
        add(&b);
        sync(&b).await;
    }
    let a = cluster.device("a");
    assert!(a.database.sync_publishable_mutations().unwrap().is_empty(), "survivor had already been acknowledged");
    let fresh = cluster.device("fresh");
    settle(&[&fresh, &a, &b]).await;
    assert_eq!(fresh.database.book_count(&test_content_hash()), 1);
    // Annotation survival is deliberately unspecified; equality and quiescence are required.
    cluster.close().await;
}

#[tokio::test]
#[ignore = "requires release HTTP server and PostgreSQL; run client/app/tests/run_sync_e2e.sh"]
async fn production_http_postgres_concurrent_cycle_restore_and_annotation_converge() {
    let cluster = Cluster::new().await;
    let a = cluster.device("a");
    let b = cluster.device("b");
    let x = fixture_dir_id("cycle-x");
    let y = fixture_dir_id("cycle-y");
    a.database.create_directory_with_id(&x, &crate::ROOT_DIR_ID, &"X".into()).unwrap();
    a.database.create_directory_with_id(&y, &crate::ROOT_DIR_ID, &"Y".into()).unwrap();
    add(&a);
    settle(&[&a, &b]).await;
    // Each move is legal in its local tree; together they form a cycle.
    a.database.move_directory(&x, Some(&y), None).unwrap();
    b.database.move_directory(&y, Some(&x), None).unwrap();
    annotate(&a, "A", 30);
    annotate(&b, "B", 30);
    a.database.trash_book(&test_content_hash()).unwrap();
    b.database.transfer_book_placement(&test_content_hash(), &crate::ROOT_DIR_ID, &x, true).unwrap();
    settle(&[&b, &a]).await;
    let plan = a.database.restore_book_plan(&test_content_hash()).unwrap();
    assert!(!plan.is_empty());
    let choices = plan.iter().map(|p| (p.original, crate::ROOT_DIR_ID)).collect::<Vec<_>>();
    a.database.restore_book_commit(&test_content_hash(), &choices).unwrap();
    b.database.delete_annotation("shared-note", 40).unwrap();
    drop(b);
    let b = cluster.device("b");
    let fresh = cluster.device("fresh");
    settle(&[&b, &fresh, &a]).await;
    assert!(fresh.database.annotations(test_content_hash()).unwrap().is_empty());
    cluster.close().await;
}

#[tokio::test]
#[ignore = "requires release HTTP server and PostgreSQL; run client/app/tests/run_sync_e2e.sh"]
async fn production_http_postgres_lost_response_retries_after_purge() {
    let cluster = Cluster::new().await;
    let a = cluster.device("a");
    let b = cluster.device("b");
    add(&a);
    settle(&[&a, &b]).await;
    annotate(&a, "in flight", 30);
    let pending = a.database.sync_publishable_mutations().unwrap();
    let mut batch = prepare_push(pending.clone()).unwrap();
    // Server commits the request, but the client never receives its ACK/page.
    let response = sync_transport::exchange_changes(&a.http_client, &session_credentials(&cluster.session), cluster.library, a.replica_id, batch.next_batch(), a.database.sync_pull_cursor().unwrap()).await.unwrap();
    assert!(response.push.rejected.is_empty());
    assert_eq!(response.push.accepted.len(), pending.len());
    b.database.trash_book(&test_content_hash()).unwrap();
    b.database.purge_book(&test_content_hash()).unwrap();
    sync(&b).await;
    drop(a);
    let a = cluster.device("a");
    assert_eq!(a.database.sync_publishable_mutations().unwrap(), pending, "restart must preserve publication identity");
    let fresh = cluster.device("fresh");
    settle(&[&a, &b, &fresh]).await;
    assert_eq!(a.database.book_count(&test_content_hash()), 0, "retry cannot create a book");
    assert!(a.database.annotations(test_content_hash()).unwrap().is_empty());
    add(&b);
    settle(&[&b, &a, &fresh]).await;
    assert_eq!(fresh.database.book_count(&test_content_hash()), 1);
    cluster.close().await;
}

#[tokio::test]
#[ignore = "requires release HTTP server and PostgreSQL; run client/app/tests/run_sync_e2e.sh"]
async fn production_http_postgres_delayed_page_and_paginated_snapshot_converge() {
    let cluster = Cluster::new().await;
    let a = cluster.device("a");
    let b = cluster.device("b");
    add(&a);
    annotate(&a, "before delayed page", 20);
    sync(&a).await;
    let cursor = b.database.sync_pull_cursor().unwrap();
    let delayed = sync_transport::exchange_changes(&b.http_client, &session_credentials(&cluster.session), cluster.library, b.replica_id, vec![], cursor).await.unwrap();
    validate_pull_batch(&delayed.pull, cursor).unwrap();
    a.database.trash_book(&test_content_hash()).unwrap();
    a.database.purge_book(&test_content_hash()).unwrap();
    sync(&a).await;
    add(&a);
    // More than one complete pull page and push batch. Three cells per folder.
    for i in 0..=sync_common::MAX_PULL_CHANGES / 3 {
        a.database.create_directory_with_id(&fixture_dir_id(&format!("page-{i}")), &crate::ROOT_DIR_ID, &format!("Folder {i:04}").into()).unwrap();
    }
    assert!(a.database.sync_publishable_mutations().unwrap().len() > sync_common::MAX_PUSH_MUTATIONS);
    sync(&a).await;
    let fresh = cluster.device("fresh");
    let first = sync_transport::exchange_changes(&fresh.http_client, &session_credentials(&cluster.session), cluster.library, fresh.replica_id, vec![], SyncCursor::default()).await.unwrap();
    assert!(first.pull.has_more, "fixture must cross a real server page boundary");
    // The old response is delivered in order on B's stream, after the server
    // has moved on. Restart before pulling the newer lifetime and remaining pages.
    b.database.sync_commit_pull_response(&[], &delayed.pull).unwrap();
    drop(b);
    let b = cluster.device("b");
    settle(&[&fresh, &b, &a]).await;
    assert_eq!(fresh.database.book_count(&test_content_hash()), 1);
    assert_eq!(rows(&fresh.database, "SELECT id FROM dir WHERE deleted_at IS NULL").len(), sync_common::MAX_PULL_CHANGES / 3 + 2);
    cluster.close().await;
}

// Override only the local authoring clock, retaining the real triggers and
// acceptance path. Explicit times make restore win without sleeps or scheduler luck.
fn author_at(device: &TestManager, millis: u64) {
    library_database::configure_fixture_connection(&device.database, |conn| conn.create_scalar_function("julianday", 1, rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |_| Ok(2440587.5 + millis as f64 / 86_400_000.0))).unwrap();
}

async fn concurrent_purge_restore(purge_first: bool) {
    let cluster = Cluster::new().await;
    let a = cluster.device("a");
    let b = cluster.device("b");
    author_at(&a, 10_000);
    add(&a);
    annotate(&a, "original", 20);
    a.database.trash_book(&test_content_hash()).unwrap();
    settle(&[&a, &b]).await;
    a.database.purge_book(&test_content_hash()).unwrap();
    // Purge uses the Rust clock; place restore explicitly above that real
    // version while staying inside the service's permitted future skew.
    let purge_time = a.database.sync_publishable_mutations().unwrap().into_iter().find(|m| matches!(m.body, MutationBody::BookLifecycle { .. })).unwrap().changed_at;
    author_at(&b, purge_time + 10_000);
    let plan = b.database.restore_book_plan(&test_content_hash()).unwrap();
    assert!(!plan.is_empty());
    b.database.restore_book_commit(&test_content_hash(), &plan.iter().map(|p| (p.original, crate::ROOT_DIR_ID)).collect::<Vec<_>>()).unwrap();
    annotate(&b, "edited during purge", 50);
    b.database.update_reading_position(&test_content_hash(), "epubcfi(/6/4)", Some(0.5), None).unwrap();
    let lifecycle_time = |d: &TestManager| d.database.sync_publishable_mutations().unwrap().into_iter().find(|m| matches!(m.body, MutationBody::BookLifecycle { .. })).unwrap().changed_at;
    assert!(lifecycle_time(&b) > lifecycle_time(&a), "restore must beat purge in both delivery orders");
    drop(a);
    drop(b);
    let a = cluster.device("a");
    let b = cluster.device("b");
    assert!(!a.database.sync_publishable_mutations().unwrap().is_empty());
    assert!(!b.database.sync_publishable_mutations().unwrap().is_empty());
    let fresh = cluster.device("fresh");
    if purge_first {
        settle(&[&a, &fresh, &b]).await;
    } else {
        settle(&[&b, &fresh, &a]).await;
    }
    assert_eq!(fresh.database.book_count(&test_content_hash()), 1);
    cluster.close().await;
}

#[tokio::test]
#[ignore = "requires release HTTP server and PostgreSQL; run client/app/tests/run_sync_e2e.sh"]
async fn production_http_postgres_concurrent_purge_before_restore_converges() {
    concurrent_purge_restore(true).await;
}

#[tokio::test]
#[ignore = "requires release HTTP server and PostgreSQL; run client/app/tests/run_sync_e2e.sh"]
async fn production_http_postgres_concurrent_restore_before_losing_purge_converges() {
    concurrent_purge_restore(false).await;
}
