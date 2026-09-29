#![cfg(not(target_arch = "wasm32"))]

use library_database::Database;
use library_files::AssetStore;
use sync_common::{ContentHash, ROOT_DIR_ID};

fn raw(db: &Database) -> library_database::rusqlite::Connection {
    library_database::open_fixture_connection(db.path()).unwrap()
}

fn add(db: &Database, hash: &ContentHash) {
    raw(db).execute("INSERT INTO book(content_hash,title,format,added_at) VALUES(?1,'book.epub','epub',1)", [hash.as_str()]).unwrap();
    db.restore_book_placement(hash, &ROOT_DIR_ID).unwrap();
    db.seed_file_projection(hash, &ROOT_DIR_ID, "/book.epub");
}

#[tokio::test]
async fn local_and_remote_purge_delete_rows_and_retry_leased_files_after_restart() {
    for remote in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.db");
        let db = Database::open(&path).unwrap();
        db.initialize_library().unwrap();
        let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
        let bytes = b"physical purge regression";
        let hash = book_identity::identify(&mut std::io::Cursor::new(bytes)).unwrap();
        std::fs::write(root.path().join("book.epub"), bytes).unwrap();
        add(&db, &hash);
        if remote {
            let mutation = library_replica::MutationBody::BookLifecycle { content_hash: hash, value: library_replica::BookLifecycleState::Purged }
                .to_wire(sync_common::MutationId::new(), 8_000_000_000_000, sync_common::ReplicaSeq::new(1).unwrap())
                .unwrap();
            db.sync_commit_pull_response(
                &[],
                &sync_common::PullStateResponse {
                    book_creations: vec![],
                    mutations: vec![sync_common::ServerMutation { mutation, replica_id: uuid::Uuid::new_v4(), revision: sync_common::LibraryRevision::new(1).unwrap() }],
                    next_cursor: Default::default(),
                    has_more: false,
                },
            )
            .unwrap();
        } else {
            db.trash_book(&hash).unwrap();
            db.purge_book(&hash).unwrap();
        }
        assert_eq!(raw(&db).query_row("SELECT count(*) FROM book", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let lease = store.lease_book(&hash).await.unwrap();
        store.run_file_jobs(&db).await.unwrap();
        assert!(root.path().join("book.epub").exists());
        assert!(db.has_pending_purge_work().unwrap());
        drop(lease);
        drop(db);
        let db = Database::open(&path).unwrap();
        db.initialize_library().unwrap();
        store.run_file_jobs(&db).await.unwrap();
        assert!(!root.path().join("book.epub").exists());
        assert!(!root.path().join(".bokheim/trash").join(hash.as_str()).exists(), "purge must delete the trash bytes too");
        assert!(db.native_file_work().unwrap().is_empty());
        assert!(!db.has_pending_purge_work().unwrap());
        store.run_file_jobs(&db).await.unwrap();
    }
}

#[tokio::test]
async fn readd_before_old_purge_cleanup_preserves_book_bytes() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let bytes = b"explicit readd protects these bytes";
    let hash = book_identity::identify(&mut std::io::Cursor::new(bytes)).unwrap();
    std::fs::write(root.path().join("book.epub"), bytes).unwrap();
    add(&db, &hash);
    db.trash_book(&hash).unwrap();
    db.purge_book(&hash).unwrap();
    add(&db, &hash);
    store.run_file_jobs(&db).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("book.epub")).unwrap(), bytes);
    assert!(!db.has_pending_purge_work().unwrap());
    assert_eq!(raw(&db).query_row("SELECT count(*) FROM book", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}
