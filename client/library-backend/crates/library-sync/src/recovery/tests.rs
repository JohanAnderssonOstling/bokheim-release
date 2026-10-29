use super::*;
use library_replica::LibraryStateEntry;
use std::collections::BTreeMap;
use sync_common::{DirId, StateAddress, Versioned, ROOT_DIR_ID};
use tempfile::TempDir;

fn fixture(directories: u128) -> (TempDir, Database) {
    let root = tempfile::tempdir().unwrap();
    let database = Database::open(root.path().join("recovery.sqlite3")).unwrap();
    database.initialize_library().unwrap();
    for index in 1..=directories {
        database.create_directory_with_id(&DirId::from_u128(index), &ROOT_DIR_ID, &format!("Shelf {index}")).unwrap();
    }
    (root, database)
}

fn publications(database: &Database) -> BTreeMap<StateAddress, Versioned<LibraryStateEntry>> {
    database.sync_publishable_mutations().unwrap().into_iter().map(|publication| (publication.state.value.address(), publication.state)).collect()
}

#[tokio::test]
async fn recovery_republishes_every_page_without_reauthoring_or_duplicating_state() {
    for directories in [0, 150] {
        let (_root, database) = fixture(directories);
        let expected = publications(&database);
        if directories > 0 {
            assert!(database.sync_inventory_page(None).unwrap().len() < expected.len(), "the fixture must span more than one recovery page");
        }
        database.clear_sync_outbox().unwrap();
        database.sync_begin_cursor_recovery().unwrap();
        assert_eq!(complete(&database).await.unwrap(), expected.len());
        assert!(!database.sync_cursor_recovery_pending().unwrap());
        assert_eq!(publications(&database), expected, "recovery must retain the original register versions");

        let queued = database.sync_publishable_mutations().unwrap();
        database.sync_begin_cursor_recovery().unwrap();
        assert_eq!(complete(&database).await.unwrap(), 0);
        assert!(!database.sync_cursor_recovery_pending().unwrap());
        assert_eq!(database.sync_publishable_mutations().unwrap(), queued, "retrying recovery must retain queued publication identities");
    }
}

#[tokio::test]
async fn failed_recovery_retains_committed_pages_and_resumes_after_reopen() {
    let (root, database) = fixture(150);
    let expected = publications(&database);
    let first_page = database.sync_inventory_page(None).unwrap().len();
    assert!(expected.len() > first_page + 1);
    database.clear_sync_outbox().unwrap();
    database.sync_begin_cursor_recovery().unwrap();
    let probe = rusqlite::Connection::open(root.path().join("recovery.sqlite3")).unwrap();
    // Let one row of the second page insert before failing. That page must
    // roll back in full while the already committed first page stays durable.
    probe
        .execute_batch(&format!(
            "
        CREATE TRIGGER fail_recovery_page BEFORE INSERT ON sync_outbox
        WHEN (SELECT COUNT(*) FROM sync_outbox) >= {}
        BEGIN SELECT RAISE(ABORT, 'injected recovery failure'); END;
    ",
            first_page + 1
        ))
        .unwrap();
    let error = complete(&database).await.unwrap_err();
    assert!(error.to_string().contains("injected recovery failure"), "{error}");
    assert!(database.sync_cursor_recovery_pending().unwrap());
    assert_eq!(probe.query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row.get::<_, i64>(0)).unwrap(), i64::try_from(first_page).unwrap());
    drop(probe);
    drop(database);

    let database = Database::open(root.path().join("recovery.sqlite3")).unwrap();
    database.initialize_library().unwrap();
    assert!(database.sync_cursor_recovery_pending().unwrap());
    let probe = rusqlite::Connection::open(root.path().join("recovery.sqlite3")).unwrap();
    probe.execute_batch("DROP TRIGGER fail_recovery_page;").unwrap();
    assert_eq!(complete(&database).await.unwrap(), expected.len() - first_page);
    assert!(!database.sync_cursor_recovery_pending().unwrap());
    assert_eq!(publications(&database), expected);
}
