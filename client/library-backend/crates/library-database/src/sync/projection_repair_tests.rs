use super::convergence_tests::{change, db, hash, present, pull};
use library_replica::{BookLifecycleState, MutationBody};
use sync_common::{ContentHash, PullStateResponse, ROOT_DIR_ID, SyncCursor};

#[test]
fn remote_placement_restore_handles_reused_local_filename() {
    for reused in ["Shared.epub", "SHARED.EPUB"] {
        let replica = db();
        let other = ContentHash::new(&"b".repeat(64));
        let placement = |hash, present, time, seq| change(MutationBody::Placement { dir_id: ROOT_DIR_ID, content_hash: hash, present, origin_folder_id: None }, time, seq);
        pull(&replica, &[present(100, 1), change(MutationBody::BookLifecycle { content_hash: other.clone(), value: BookLifecycleState::Present }, 100, 2), placement(hash(), true, 100, 3), placement(other.clone(), true, 100, 4)]);
        replica.connection.execute("UPDATE book_dir SET file_name='Shared.epub' WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", [hash().as_str()]).unwrap();
        pull(&replica, &[placement(hash(), false, 200, 5)]);
        replica.connection.execute("UPDATE book_dir SET file_name=?2 WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", rusqlite::params![other.as_str(), reused]).unwrap();
        let page = PullStateResponse { book_creations: Vec::new(), mutations: vec![placement(hash(), true, 300, 6)], next_cursor: SyncCursor::default(), has_more: false };
        let result = replica.sync_commit_pull_response(&[], &page);
        assert!(result.is_ok(), "local filenames must not block a remote placement winner: {result:?}");
        let name: String = replica.connection.query_row("SELECT file_name FROM book_dir WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1) AND deleted_at IS NULL", [hash().as_str()], |row| row.get(0)).unwrap();
        assert_eq!(name, hash().as_str());
        replica.sync_commit_pull_response(&[], &page).unwrap();
        assert!(replica.sync_publishable_mutations().unwrap().is_empty(), "filename repairs must not become synchronized edits");
    }
}

#[test]
fn purging_trashed_directory_purges_its_descendant_intents() {
    for empty_all in [false, true] {
        let replica = db();
        let parent = uuid::Uuid::from_u128(1);
        let child = uuid::Uuid::from_u128(2);
        replica.create_directory_with_id(&parent, &ROOT_DIR_ID, &"Parent".into()).unwrap();
        replica.create_directory_with_id(&child, &parent, &"Child".into()).unwrap();
        replica.trash_directory(&parent).unwrap();
        if empty_all {
            replica.empty_trash().unwrap();
        } else {
            replica.purge_directory(&parent).unwrap();
        }
        let state: (i64, bool) = replica.connection.query_row("SELECT intent_lifecycle,purged_at IS NOT NULL FROM dir WHERE id=?1", [child.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        let later: i64 = replica.connection.query_row("SELECT MAX(changed_at)+1000 FROM sync_state_version", [], |row| row.get(0)).unwrap();
        pull(&replica, &[change(MutationBody::DirectoryParent { dir_id: child, parent_id: ROOT_DIR_ID }, later as u64, 100)]);
        let path = replica.directory_relative_path_string(&child).unwrap();
        assert_eq!(state, (2, true), "purging a folder should reach its hidden descendants; a subsequent parent-only update exposes {path:?}");
        assert_eq!(path, None, "a parent-only update cannot undo the child's purge");
    }
}

#[test]
fn subtree_purge_terminates_for_retained_parent_cycles() {
    for empty_all in [false, true] {
        let replica = db();
        let first = uuid::Uuid::from_u128(1);
        let second = uuid::Uuid::from_u128(2);
        let unrelated = uuid::Uuid::from_u128(3);
        replica.create_directory_with_id(&unrelated, &ROOT_DIR_ID, &"Unrelated".into()).unwrap();
        replica.create_directory_with_id(&second, &ROOT_DIR_ID, &"Second".into()).unwrap();
        replica.create_directory_with_id(&first, &second, &"First".into()).unwrap();
        let later: i64 = replica.connection.query_row("SELECT MAX(changed_at)+1000 FROM sync_state_version", [], |row| row.get(0)).unwrap();
        pull(&replica, &[change(MutationBody::DirectoryParent { dir_id: second, parent_id: first }, later as u64, 100)]);
        replica.trash_directory(&first).unwrap();
        if empty_all {
            replica.empty_trash().unwrap();
        } else {
            replica.purge_directory(&first).unwrap();
        }
        assert_eq!(replica.directory_relative_path_string(&second).unwrap(), None);
        assert_eq!(replica.directory_relative_path_string(&unrelated).unwrap(), Some("Unrelated".into()));
        let child_purged: bool = replica.connection.query_row("SELECT purged_at IS NOT NULL FROM dir WHERE id=?1", [second.to_string()], |row| row.get(0)).unwrap();
        assert!(child_purged);
        let purged: bool = replica.connection.query_row("SELECT purged_at IS NOT NULL FROM dir WHERE id=?1", [first.to_string()], |row| row.get(0)).unwrap();
        assert!(purged);
    }
}

#[test]
fn subtree_purge_rolls_back_all_lifecycles_when_a_child_write_fails() {
    let replica = db();
    let parent = uuid::Uuid::from_u128(1);
    let child = uuid::Uuid::from_u128(2);
    replica.create_directory_with_id(&parent, &ROOT_DIR_ID, &"Parent".into()).unwrap();
    replica.create_directory_with_id(&child, &parent, &"Child".into()).unwrap();
    replica.trash_directory(&parent).unwrap();
    let before = replica.sync_publishable_mutations().unwrap();
    replica
        .connection
        .execute_batch(
            "CREATE TEMP TRIGGER reject_child_purge BEFORE UPDATE OF intent_lifecycle ON dir WHEN NEW.id='00000000-0000-0000-0000-000000000002' AND NEW.intent_lifecycle=2 BEGIN SELECT RAISE(ABORT,'injected child purge failure'); END;",
        )
        .unwrap();
    assert!(replica.purge_directory(&parent).is_err());
    let purged: i64 = replica.connection.query_row("SELECT count(*) FROM dir WHERE purged_at IS NOT NULL", [], |row| row.get(0)).unwrap();
    assert_eq!(purged, 0);
    assert_eq!(replica.sync_publishable_mutations().unwrap(), before);
    replica.connection.execute_batch("DROP TRIGGER reject_child_purge;").unwrap();
    replica.purge_directory(&parent).unwrap();
    let purged: i64 = replica.connection.query_row("SELECT count(*) FROM dir WHERE purged_at IS NOT NULL", [], |row| row.get(0)).unwrap();
    assert_eq!(purged, 2);
}
