#![cfg(not(target_arch = "wasm32"))]

use library_database::Database;
use library_files::AssetStore;
use sync_common::ROOT_DIR_ID;

fn raw(db: &Database) -> library_database::rusqlite::Connection {
    library_database::open_fixture_connection(db.path()).unwrap()
}

#[tokio::test]
async fn replay_retires_completed_generations_but_keeps_newer_and_blocked_work() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let dir = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let hash = sync_common::ContentHash::new(&"a".repeat(64));
    db.queue_book_work(&hash).unwrap();
    let old_dirs: Vec<_> = db.native_directory_replay_work().unwrap().into_iter().map(|(id, _)| id).collect();
    let old_books = db.native_book_replay_work_ids().unwrap();
    db.queue_directory_work(&dir).unwrap();
    db.queue_book_work(&hash).unwrap();
    db.complete_native_directory_replay(&old_dirs).unwrap();
    db.complete_native_book_replay(&old_books).unwrap();
    assert_eq!(db.native_directory_work_ids().unwrap(), vec![dir]);
    assert_eq!(db.local_book_work_hashes().unwrap(), vec![hash]);
    store.run_directory_work(&db).await.unwrap();
    db.complete_native_book_replay(&old_books).unwrap();
    assert_eq!(db.local_book_work_hashes().unwrap(), vec![hash], "old completion must not retire a newer generation");
    store.run_file_jobs(&db).await.unwrap();
    assert!(root.path().join("Shelf").is_dir());
    assert!(db.native_directory_work_ids().unwrap().is_empty());
    assert!(db.local_book_work_hashes().unwrap().is_empty());

    // Skipping a leased file is not successful book reconciliation.
    raw(&db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'book','epub')", [hash.as_str()]).unwrap();
    // Finish the raw fixture's canonical writes before taking worker generations.
    db.update_reading_position(&hash, "epubcfi(/6/2)", None, None).unwrap();
    db.seed_file_work("trash", &hash, "/missing.epub");
    db.queue_book_work(&hash).unwrap();
    let lease = store.lease_book(&hash).await.unwrap();
    store.run_file_jobs(&db).await.unwrap();
    assert_eq!(db.local_book_work_hashes().unwrap(), vec![hash]);
    drop(lease);
    store.run_file_jobs(&db).await.unwrap();
    assert!(db.native_file_work().unwrap().is_empty());
    assert!(db.local_book_work_hashes().unwrap().is_empty());
}

#[tokio::test]
async fn failed_directory_replay_keeps_its_generation() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let dir = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    db.move_directory(&dir, None, Some("Renamed")).unwrap();
    // A foreign identity must fail replay without acknowledging queued work.
    std::fs::write(root.path().join("Shelf/.biblos_uuid"), "foreign identity").unwrap();
    let work = db.native_directory_replay_work().unwrap();
    assert!(store.run_directory_work(&db).await.is_err());
    assert_eq!(db.native_directory_replay_work().unwrap(), work);
}

#[tokio::test]
async fn copy_then_remove_source_preserves_destination_bytes() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let source = db.create_directory(&ROOT_DIR_ID, &"A".into()).unwrap().id;
    let destination = db.create_directory(&ROOT_DIR_ID, &"Z".into()).unwrap().id;
    for id in [source, destination] {
        store.placement().prepare_directory_with_database(&db, id).await.unwrap();
    }
    let bytes = b"book bytes for placement copy regression";
    let hash = book_identity::identify(&mut std::io::Cursor::new(bytes)).unwrap();
    std::fs::write(root.path().join("A/book.epub"), bytes).unwrap();
    raw(&db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'book.epub','epub')", [hash.as_str()]).unwrap();
    db.restore_book_placement(&hash, &source).unwrap();
    db.seed_file_projection(&hash, &source, "/A/book.epub");
    store.placement().reconcile_file_work(&db).await.unwrap();
    db.transfer_book_placement(&hash, &source, &destination, false).unwrap();
    store.placement().reconcile_file_work(&db).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("A/book.epub")).unwrap(), bytes);
    assert_eq!(std::fs::read(root.path().join("Z/book.epub")).unwrap(), bytes);
    db.remove_book_placement(&hash, &source).unwrap();
    store.placement().reconcile_file_work(&db).await.unwrap();
    assert!(!root.path().join("A/book.epub").exists());
    assert_eq!(std::fs::read(root.path().join("Z/book.epub")).unwrap(), bytes);
    assert!(db.native_file_work().unwrap().is_empty());
}

#[tokio::test]
async fn cycle_recovery_converges_from_opposite_physical_moves() {
    use library_replica::{MutationBody, StateMutation};
    use sync_common::{LibraryRevision, MutationId, PullStateResponse, ReplicaSeq, ServerMutation, SyncCursor};
    let a = "00000000-0000-0000-0000-000000000001".parse().unwrap();
    let b = "00000000-0000-0000-0000-000000000002".parse().unwrap();
    for reverse in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let db = Database::open(root.path().join("library.db")).unwrap();
        db.initialize_library().unwrap();
        let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
        db.create_directory_with_id(&a, &ROOT_DIR_ID, &"A".into()).unwrap();
        db.create_directory_with_id(&b, &ROOT_DIR_ID, &"B".into()).unwrap();
        store.run_directory_work(&db).await.unwrap();
        std::fs::write(root.path().join("A/book.epub"), b"A bytes").unwrap();
        std::fs::write(root.path().join("B/book.epub"), b"B bytes").unwrap();
        for (dir, digit) in [(a, 'a'), (b, 'b')] {
            let hash = sync_common::ContentHash::new(&digit.to_string().repeat(64));
            raw(&db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'book','epub')", [hash.as_str()]).unwrap();
            db.add_book_placement(&dir, &hash, "book.epub", "", true);
        }
        // Fail promptly if an intermediate cycle makes a recursive trigger loop.
        library_database::set_fixture_progress_limit(&db, 10_000).unwrap();
        if reverse {
            db.move_directory(&b, Some(&a), None).unwrap();
        } else {
            db.move_directory(&a, Some(&b), None).unwrap();
        }
        store.run_directory_work(&db).await.unwrap();
        let replica = "00000000-0000-0000-0000-000000000003".parse().unwrap();
        // Both clients receive identical winning parents, in opposite orders.
        let mut mutations: Vec<_> = [(a, b), (b, a)]
            .into_iter()
            .enumerate()
            .map(|(index, (dir_id, parent_id))| {
                let mutation = StateMutation {
                    origin: None,
                    mutation_id: MutationId::parse(&format!("00000000-0000-0000-0000-{:012}", index + 10)).unwrap(),
                    body: MutationBody::DirectoryParent { dir_id, parent_id },
                    changed_at: 4_000_000_000_000,
                    replica_seq: ReplicaSeq::new(index as u64 + 1).unwrap(),
                };
                ServerMutation { mutation: mutation.to_wire().unwrap(), replica_id: replica, revision: LibraryRevision::new(index as u64 + 1).unwrap() }
            })
            .collect();
        if reverse {
            mutations.reverse();
        }
        let ids = db.sync_publishable_mutations().unwrap().iter().map(|m| m.mutation_id).collect::<Vec<_>>();
        let page = PullStateResponse { book_creations: Vec::new(), mutations, next_cursor: SyncCursor { state_revision: LibraryRevision::new(2).ok(), reading_revision: None }, has_more: false };
        db.sync_commit_pull_response(&ids, &page).unwrap();
        assert_eq!(db.directory_relative_path_string(&b).unwrap().as_deref(), Some("B"));
        assert_eq!(db.directory_relative_path_string(&a).unwrap().as_deref(), Some("B/A"));
        assert!(db.sync_publishable_mutations().unwrap().is_empty(), "repair must not emit a new move");
        store.run_directory_work(&db).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("B/A/book.epub")).unwrap(), b"A bytes");
        assert_eq!(std::fs::read(root.path().join("B/book.epub")).unwrap(), b"B bytes");
        assert!(!root.path().join("A").exists());
        assert!(db.native_directory_work_ids().unwrap().is_empty());
        db.sync_commit_pull_response(&[], &page).unwrap();
        store.run_directory_work(&db).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("B/A/book.epub")).unwrap(), b"A bytes");
    }
}

#[tokio::test]
async fn managed_name_swap_preserves_resolved_names_and_files() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let a = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let b = db.create_directory(&ROOT_DIR_ID, &"Shelf 2".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    std::fs::write(root.path().join("Shelf/a.epub"), b"A").unwrap();
    std::fs::write(root.path().join("Shelf 2/b.epub"), b"B").unwrap();
    // Equivalent to a resolver exchanging the unnumbered winner and loser.
    raw(&db).execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
    raw(&db).execute("UPDATE dir SET deleted_at=1 WHERE id IN (?1,?2)", [a.to_string(), b.to_string()]).unwrap();
    raw(&db).execute("UPDATE dir SET name='Shelf 2' WHERE id=?1", [a.to_string()]).unwrap();
    raw(&db).execute("UPDATE dir SET name='Shelf' WHERE id=?1", [b.to_string()]).unwrap();
    raw(&db).execute("UPDATE dir SET deleted_at=NULL WHERE id IN (?1,?2)", [a.to_string(), b.to_string()]).unwrap();
    raw(&db).execute("UPDATE sync_metadata SET change_origin='local'", []).unwrap();
    db.queue_directory_work(&a).unwrap();
    db.queue_directory_work(&b).unwrap();
    let before = db.sync_publishable_mutations().unwrap().iter().map(|m| m.mutation_id).collect::<Vec<_>>();
    store.placement().prepare_directory_with_database(&db, a).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("Shelf 2/a.epub")).unwrap(), b"A");
    assert_eq!(std::fs::read(root.path().join("Shelf/b.epub")).unwrap(), b"B");
    assert_eq!(db.directory_relative_path_string(&a).unwrap().as_deref(), Some("Shelf 2"));
    assert_eq!(db.directory_relative_path_string(&b).unwrap().as_deref(), Some("Shelf"));
    assert_eq!(before, db.sync_publishable_mutations().unwrap().iter().map(|m| m.mutation_id).collect::<Vec<_>>());
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".biblos-move-")));
}


#[tokio::test]
async fn directory_replay_recovers_unacknowledged_staging_with_descendants() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("library.db");
    let db = Database::open(&path).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let parent = db.create_directory(&ROOT_DIR_ID, &"Old".into()).unwrap().id;
    let child = db.create_directory(&parent, &"Child".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    std::fs::write(root.path().join("Old/Child/book.epub"), b"retained").unwrap();
    db.move_directory(&parent, None, Some("New")).unwrap();
    // Crash window: filesystem rename completed, projection acknowledgement did not.
    std::fs::rename(root.path().join("Old"), root.path().join(format!(".biblos-move-{parent}"))).unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    db.initialize_library().unwrap();
    store.run_directory_work(&db).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("New/Child/book.epub")).unwrap(), b"retained");
    assert_eq!(db.native_directory_snapshot(&child).unwrap().unwrap().projected_path.as_deref(), Some("/New/Child"));
    assert!(db.native_directory_work_ids().unwrap().is_empty());
}

#[tokio::test]
async fn interrupted_staging_is_recovered_after_rename_undo() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("library.db");
    let db = Database::open(&path).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let id = db.create_directory(&ROOT_DIR_ID, &"Old".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    std::fs::write(root.path().join("Old/book.epub"), b"book").unwrap();
    db.move_directory(&id, None, Some("New")).unwrap();
    let staged = root.path().join(format!(".biblos-move-{id}"));
    std::fs::rename(root.path().join("Old"), &staged).unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    db.initialize_library().unwrap();
    db.move_directory(&id, None, Some("Old")).unwrap();
    store.run_directory_work(&db).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("Old/book.epub")).unwrap(), b"book");
    assert!(!staged.exists());
    assert!(db.native_directory_work_ids().unwrap().is_empty());
}


#[tokio::test]
async fn direct_preparation_recovers_interrupted_staging_before_creating_destination() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("library.db");
    let db = Database::open(&path).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let id = db.create_directory(&ROOT_DIR_ID, &"Old".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    std::fs::write(root.path().join("Old/book.epub"), b"book").unwrap();
    db.move_directory(&id, None, Some("New")).unwrap();
    let staged = root.path().join(format!(".biblos-move-{id}"));
    std::fs::rename(root.path().join("Old"), &staged).unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    db.initialize_library().unwrap();
    let before = db.sync_publishable_mutations().unwrap().iter().map(|m| m.mutation_id).collect::<Vec<_>>();
    store.placement().prepare_directory_with_database(&db, id).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("New/book.epub")).unwrap(), b"book");
    assert!(!staged.exists());
    assert_eq!(db.native_directory_snapshot(&id).unwrap().unwrap().projected_path.as_deref(), Some("/New"));
    assert!(db.native_directory_work_ids().unwrap().is_empty());
    store.run_directory_work(&db).await.unwrap();
    assert_eq!(before, db.sync_publishable_mutations().unwrap().iter().map(|m| m.mutation_id).collect::<Vec<_>>());
}

#[tokio::test]
async fn trash_after_interrupted_stage_preserves_bytes_in_trash() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let id = db.create_directory(&ROOT_DIR_ID, &"Old".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    let bytes = b"book";
    let hash = book_identity::identify(&mut std::io::Cursor::new(bytes)).unwrap();
    std::fs::write(root.path().join("Old/book.epub"), bytes).unwrap();
    raw(&db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'book','epub')", [hash.as_str()]).unwrap();
    db.add_book_placement(&id, &hash, "book.epub", "", true);
    db.seed_file_projection(&hash, &id, "/Old/book.epub");
    db.move_directory(&id, None, Some("New")).unwrap();
    let staged = root.path().join(format!(".biblos-move-{id}"));
    std::fs::rename(root.path().join("Old"), &staged).unwrap();
    db.trash_directory(&id).unwrap();
    store.run_file_jobs(&db).await.unwrap();
    assert!(!staged.join("book.epub").exists());
    assert!(db.native_directory_work_ids().unwrap().is_empty());
    assert!(db.native_file_work().unwrap().is_empty());
    // Restoring the book proves its bytes reached recoverable storage.
    db.restore_book_commit(&hash, &[(id, ROOT_DIR_ID)]).unwrap();
    store.run_file_jobs(&db).await.unwrap();
    assert_eq!(std::fs::read(root.path().join("book.epub")).unwrap(), bytes);
}

#[tokio::test]
async fn unrelated_broken_move_does_not_block_direct_preparation() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let broken = db.create_directory(&ROOT_DIR_ID, &"Broken".into()).unwrap().id;
    let good = db.create_directory(&ROOT_DIR_ID, &"Good".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    db.move_directory(&broken, None, Some("Renamed")).unwrap();
    std::fs::write(root.path().join("Broken/.biblos_uuid"), "foreign").unwrap();
    store.placement().prepare_directory_with_database(&db, good).await.unwrap();
    assert_eq!(db.native_directory_work_ids().unwrap(), vec![broken]);
    assert!(root.path().join("Good/.biblos_uuid").is_file());
}

#[tokio::test]
async fn unsafe_and_colliding_remote_names_materialize_without_changing_intent() {
    use library_replica::{DirectoryLifecycleState, MutationBody};
    use sync_common::{LibraryRevision, MutationId, PullStateResponse, ReplicaSeq, ServerMutation, SyncCursor};
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let names = ["..".to_owned(), "CON.txt".to_owned(), "a".repeat(255), "a".repeat(255), "界".repeat(100), "A/B".to_owned()];
    let mut mutations = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let dir_id = uuid::Uuid::from_u128(index as u128 + 1);
        for body in [MutationBody::DirectoryName { dir_id, name: name.clone() }, MutationBody::DirectoryParent { dir_id, parent_id: ROOT_DIR_ID }, MutationBody::DirectoryLifecycle { dir_id, value: DirectoryLifecycleState::Present }] {
            let seq = mutations.len() as u64 + 1;
            mutations.push(ServerMutation { mutation: body.to_wire(MutationId::new(), 100, ReplicaSeq::new(seq).unwrap()).unwrap(), replica_id: uuid::Uuid::from_u128(100), revision: LibraryRevision::new(seq).unwrap() });
        }
    }
    db.sync_commit_pull_response(&[], &PullStateResponse { book_creations: Vec::new(), mutations, next_cursor: SyncCursor::default(), has_more: false }).unwrap();
    store.run_directory_work(&db).await.unwrap();
    for (index, intent) in names.iter().enumerate() {
        let id = uuid::Uuid::from_u128(index as u128 + 1);
        let (name, retained): (String, String) = raw(&db).query_row("SELECT name,intent_name FROM dir WHERE id=?1", [id.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(&retained, intent);
        assert_eq!(library_replica::filenames::validate_component(&name).unwrap(), name);
        assert!(root.path().join(&name).is_dir());
        assert_eq!(std::fs::read_to_string(root.path().join(&name).join(".biblos_uuid")).unwrap(), id.to_string());
    }
    assert!(db.native_directory_work_ids().unwrap().is_empty());
    assert!(db.sync_publishable_mutations().unwrap().is_empty(), "physical materialization must not publish repaired names");
}

#[tokio::test]
async fn restoring_parent_recovers_nested_book_bytes_from_trash() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let parent = db.create_directory(&ROOT_DIR_ID, &"Parent".into()).unwrap().id;
    let child = db.create_directory(&parent, &"Child".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    let bytes = b"nested book restored without a download";
    let hash = book_identity::identify(&mut std::io::Cursor::new(bytes)).unwrap();
    let path = root.path().join("Parent/Child/book.epub");
    std::fs::write(&path, bytes).unwrap();
    raw(&db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'book','epub')", [hash.as_str()]).unwrap();
    db.add_book_placement(&child, &hash, "book.epub", "", true);
    db.seed_file_projection(&hash, &child, "/Parent/Child/book.epub");
    db.trash_directory(&parent).unwrap();
    store.run_file_jobs(&db).await.unwrap();
    assert!(!path.exists());
    db.restore_directory(&parent, None).unwrap();
    store.run_file_jobs(&db).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(!root.path().join("book.epub").exists());
    assert!(db.native_file_work().unwrap().is_empty());
}

#[tokio::test]
async fn review_copy_collision_keeps_a_maximum_length_filename_materializable() {
    let root = tempfile::tempdir().unwrap();
    let db = Database::open(root.path().join("library.db")).unwrap();
    db.initialize_library().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let source = db.create_directory(&ROOT_DIR_ID, &"Source".into()).unwrap().id;
    let target = db.create_directory(&ROOT_DIR_ID, &"Target".into()).unwrap().id;
    store.run_directory_work(&db).await.unwrap();
    let name = format!("{}.epub", "a".repeat(250));
    let source_bytes = b"source bytes";
    let source_hash = book_identity::identify(&mut std::io::Cursor::new(source_bytes)).unwrap();
    let target_bytes = b"different existing book";
    let target_hash = book_identity::identify(&mut std::io::Cursor::new(target_bytes)).unwrap();
    for (dir, directory, hash, bytes) in [(source, "Source", source_hash, source_bytes.as_slice()), (target, "Target", target_hash, target_bytes.as_slice())] {
        std::fs::write(root.path().join(directory).join(&name), bytes).unwrap();
        raw(&db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'book','epub')", [hash.as_str()]).unwrap();
        db.add_book_placement(&dir, &hash, &name, "", true);
        db.seed_file_projection(&hash, &dir, &format!("/{directory}/{name}"));
    }
    let chosen = db.transfer_book_placement(&source_hash, &source, &target, false).unwrap();
    let result = store.run_file_jobs(&db).await;
    assert!(result.is_ok(), "a valid source filename became {} bytes after collision repair: {result:?}", chosen.len());
    assert_eq!(std::fs::read(root.path().join("Target").join(chosen)).unwrap(), source_bytes);
}

#[tokio::test]
async fn book_trash_preserves_membership_but_moves_bytes_until_restore() {
    for remove_last_copy in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let db = Database::open(root.path().join("library.db")).unwrap();
        db.initialize_library().unwrap();
        let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
        let bytes = b"preserved placement";
        let hash = book_identity::identify(&mut std::io::Cursor::new(bytes)).unwrap();
        std::fs::write(root.path().join("book.epub"), bytes).unwrap();
        db.seed_book(&hash, Some("book"), 1, "epub");
        db.add_book_placement(&ROOT_DIR_ID, &hash, "book.epub", "", true);
        db.seed_file_projection(&hash, &ROOT_DIR_ID, "/book.epub");
        if remove_last_copy { assert!(db.remove_book_placement(&hash, &ROOT_DIR_ID).unwrap()); }
        else { db.trash_book(&hash).unwrap(); }
        assert!(raw(&db).query_row("SELECT deleted_at IS NULL FROM book_dir", [], |r| r.get::<_, bool>(0)).unwrap());
        assert_eq!(raw(&db).query_row("SELECT is_downloaded FROM book_dir", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        store.run_file_jobs(&db).await.unwrap();
        assert!(!root.path().join("book.epub").exists(), "retaining membership must not keep trashed bytes at the live path");
        assert!(db.native_file_work().unwrap().is_empty());
        assert_eq!(db.restore_book_plan(&hash).unwrap().len(), 1);
        db.restore_book_commit(&hash, &[(ROOT_DIR_ID, ROOT_DIR_ID)]).unwrap();
        store.run_file_jobs(&db).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("book.epub")).unwrap(), bytes);
        assert!(db.native_file_work().unwrap().is_empty());
    }
}
