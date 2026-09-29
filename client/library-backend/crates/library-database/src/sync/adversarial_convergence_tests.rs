//! Adversarial release regressions for recovery and safe folder projection.
use super::convergence_tests::{change, db, hash, present, pull};
use library_replica::{BookLifecycleState, DirectoryLifecycleState, MutationBody};
use sync_common::{LibraryRevision, ROOT_DIR_ID, ServerMutation};

fn folder(id: u128, name: &str) -> Vec<ServerMutation> {
    let dir_id = uuid::Uuid::from_u128(id);
    [MutationBody::DirectoryName { dir_id, name: name.into() }, MutationBody::DirectoryParent { dir_id, parent_id: ROOT_DIR_ID }, MutationBody::DirectoryLifecycle { dir_id, value: DirectoryLifecycleState::Present }]
        .into_iter()
        .enumerate()
        .map(|(index, body)| change(body, 100, id as u64 * 10 + index as u64 + 1))
        .collect()
}

#[test]
fn stale_recovery_must_not_override_a_known_purge() {
    let stale = db();
    let current = db();
    let addition = present(100, 1);
    let purge = change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Purged }, 100, 2);
    pull(&stale, &[addition.clone()]);
    pull(&current, &[addition, purge]);
    stale.sync_enqueue_missing_state_cells(&stale.sync_inventory_page(None).unwrap()).unwrap();
    let publications = stale.sync_publishable_mutations().unwrap();
    let replay = publications.iter().map(|m| ServerMutation { mutation: m.to_wire().unwrap(), replica_id: uuid::Uuid::from_u128(51), revision: LibraryRevision::new(3).unwrap() }).collect::<Vec<_>>();
    pull(&current, &replay);
    let purged: bool = current.connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM book WHERE content_hash=?1)", [hash().as_str()], |row| row.get(0)).unwrap();
    assert!(purged, "recovery alone revived a purged book: {publications:?}");
}

#[test]
fn retrying_import_after_name_repair_must_accept_unchanged_intent() {
    let replica = db();
    let local = uuid::Uuid::from_u128(1);
    replica.create_directory_with_id(&local, &ROOT_DIR_ID, &"Shelf".into()).unwrap();
    pull(&replica, &folder(2, "Shelf"));
    let names: (String, String) = replica.connection.query_row("SELECT name,intent_name FROM dir WHERE id=?1", [local.to_string()], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(names, ("Shelf 2".into(), "Shelf".into()));
    replica.create_directory_with_id(&local, &ROOT_DIR_ID, &"Shelf".into()).expect("retry must compare import intent, not a repaired display name");
}

#[test]
fn collision_repair_must_leave_folder_names_materializable() {
    let replica = db();
    let original = "a".repeat(255);
    pull(&replica, &folder(1, &original));
    pull(&replica, &folder(2, &original));
    let repaired: String = replica.connection.query_row("SELECT name FROM dir WHERE id=?1", [uuid::Uuid::from_u128(1).to_string()], |row| row.get(0)).unwrap();
    // A valid input component must not become too long for ordinary filesystems
    // solely because another device independently created the same folder name.
    assert!(repaired.len() <= 240, "collision repair expanded a valid 255-byte folder name to {} bytes", repaired.len());
}

#[test]
fn pending_remote_winner_must_not_change_its_tombstone_time_when_preparing_a_request() {
    let replica = db();
    // A pending recovery already exists when the winning purge arrives.
    pull(&replica, &[present(90, 1)]);
    replica.sync_enqueue_missing_state_cells(&replica.sync_inventory_page(None).unwrap()).unwrap();
    pull(&replica, &[change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Purged }, 100, 2)]);
    let before = super::apply::registers::canonical_body(&replica.connection, "book_lifecycle", hash().as_str(), "").unwrap();
    replica.sync_publishable_mutations().unwrap();
    let after = super::apply::registers::canonical_body(&replica.connection, "book_lifecycle", hash().as_str(), "").unwrap();
    assert_eq!(replica.connection.query_row("SELECT count(*) FROM book", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(after, before, "preparing a network request changed the projected purge time without a user edit");
}

#[test]
fn a_remote_folder_must_not_expose_a_parent_traversal_component() {
    let replica = db();
    let response = sync_common::PullStateResponse { book_creations: Vec::new(), mutations: folder(1, ".."), next_cursor: sync_common::SyncCursor::default(), has_more: false };
    let result = replica.sync_commit_pull_response(&[], &response);
    if result.is_ok() {
        let names = replica.connection.prepare("SELECT name FROM dir WHERE deleted_at IS NULL AND id!=?1").unwrap().query_map([ROOT_DIR_ID.to_string()], |row| row.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert!(names.iter().all(|name| library_replica::filenames::validate_component(name).is_ok()), "accepted remote state projected unsafe folder components: {names:?}");
    }
    // Either rejecting the invalid mutation or projecting a deterministic safe
    // component is acceptable. Exposing '..' as a live folder name is not.
}


#[test]
fn separate_import_jobs_with_the_same_folder_name_reach_collision_projection() {
    for order in [[1, 2], [2, 1]] {
        let replica = db();
        for id in order {
            replica.create_directory_with_id(&uuid::Uuid::from_u128(id), &ROOT_DIR_ID, &"Shelf".into()).expect("creation must reach deterministic sibling repair");
        }
        for (id, expected) in [(1, "Shelf 2"), (2, "Shelf")] {
            let id = uuid::Uuid::from_u128(id);
            assert_eq!(replica.directory_relative_path_string(&id).unwrap(), Some(expected.into()));
            replica.create_directory_with_id(&id, &ROOT_DIR_ID, &"Shelf".into()).expect("retry must retain the original intent");
        }
        let mutations = replica.sync_publishable_mutations().unwrap();
        assert_eq!(mutations.len(), 6);
        for mutation in mutations {
            match mutation.body {
                MutationBody::DirectoryName { name, .. } => assert_eq!(name, "Shelf"),
                MutationBody::DirectoryLifecycle { value, .. } => assert_eq!(value, DirectoryLifecycleState::Present),
                MutationBody::DirectoryParent { parent_id, .. } => assert_eq!(parent_id, ROOT_DIR_ID),
                other => panic!("unexpected publication: {other:?}"),
            }
        }
    }
}

#[test]
fn colliding_directory_creation_rolls_back_when_projection_fails() {
    let replica = db();
    let first = uuid::Uuid::from_u128(1);
    let second = uuid::Uuid::from_u128(2);
    replica.create_directory_with_id(&first, &ROOT_DIR_ID, &"Shelf".into()).unwrap();
    let before = replica.sync_publishable_mutations().unwrap();
    replica.connection.execute_batch("CREATE TEMP TRIGGER reject_collision_projection BEFORE UPDATE OF name ON dir BEGIN SELECT RAISE(ABORT,'injected collision failure'); END;").unwrap();
    assert!(replica.create_directory_with_id(&second, &ROOT_DIR_ID, &"Shelf".into()).is_err());
    assert_eq!(replica.directory_relative_path_string(&first).unwrap(), Some("Shelf".into()));
    let count: i64 = replica.connection.query_row("SELECT count(*) FROM dir WHERE id=?1", [second.to_string()], |row| row.get(0)).unwrap();
    assert_eq!(count, 0, "staged directory must roll back with projection");
    assert_eq!(replica.sync_publishable_mutations().unwrap(), before);
    replica.connection.execute_batch("DROP TRIGGER reject_collision_projection;").unwrap();
    replica.create_directory_with_id(&second, &ROOT_DIR_ID, &"Shelf".into()).unwrap();
    assert_eq!(replica.directory_relative_path_string(&first).unwrap(), Some("Shelf 2".into()));
}

#[test]
fn import_retry_can_complete_a_directory_received_in_a_partial_pull_page() {
    let replica = db();
    let id = uuid::Uuid::from_u128(1);
    pull(&replica, &[change(MutationBody::DirectoryName { dir_id: id, name: "Shelf".into() }, 100, 1)]);
    let result = replica.create_directory_with_id(&id, &ROOT_DIR_ID, &"Shelf".into());
    assert!(result.is_ok(), "an identical name received before parent/lifecycle must not prevent resuming the import: {result:?}");
    assert_eq!(replica.directory_relative_path_string(&id).unwrap(), Some("Shelf".into()));
}


#[test]
fn old_recovery_ack_does_not_remove_a_new_local_folder_edit() {
    let replica = db();
    pull(&replica, &folder(1, "Shelf"));
    replica.sync_enqueue_missing_state_cells(&replica.sync_inventory_page(None).unwrap()).unwrap();
    let recovered = replica.sync_publishable_mutations().unwrap();
    replica.move_directory(&uuid::Uuid::from_u128(1), None, Some("Edited")).unwrap();
    replica.sync_acknowledge_mutations(&recovered.iter().map(|m| m.mutation_id).collect::<Vec<_>>()).unwrap();
    let pending = replica.sync_publishable_mutations().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(matches!(&pending[0].body, MutationBody::DirectoryName { name, .. } if name == "Edited"));
    assert!(pending[0].origin.is_none(), "a user edit must get its own version rather than forward the recovered version");
}

#[test]
fn restore_must_not_resurrect_a_partially_received_purged_directory() {
    let replica = db();
    let id = uuid::Uuid::from_u128(101);
    pull(&replica, &[change(MutationBody::DirectoryLifecycle { dir_id: id, value: DirectoryLifecycleState::Purged }, 100, 1)]);
    let result = replica.restore_directory(&id, None);
    pull(&replica, &[change(MutationBody::DirectoryName { dir_id: id, name: "Purged folder".into() }, 100, 2), change(MutationBody::DirectoryParent { dir_id: id, parent_id: ROOT_DIR_ID }, 100, 3)]);
    assert_eq!(replica.directory_relative_path_string(&id).unwrap(), None, "restore returned {result:?} and revived a known purge when missing creation state arrived");
    assert!(replica.sync_publishable_mutations().unwrap().is_empty());
}

#[test]
fn restoring_a_partial_directory_must_not_report_success_without_a_path() {
    let replica = db();
    let id = uuid::Uuid::from_u128(102);
    pull(&replica, &[change(MutationBody::DirectoryLifecycle { dir_id: id, value: DirectoryLifecycleState::Deleted }, 100, 1)]);
    let result = replica.restore_directory(&id, None);
    if result.is_ok() {
        assert!(replica.directory_relative_path_string(&id).unwrap().is_some(), "restore returned {result:?} but the folder is still unreachable");
    }
}

#[test]
fn empty_trash_must_not_purge_a_folder_whose_lifecycle_has_not_arrived() {
    let replica = db();
    let id = uuid::Uuid::from_u128(103);
    pull(&replica, &[change(MutationBody::DirectoryName { dir_id: id, name: "Live folder".into() }, 100, 1)]);
    let shown_in_trash = replica.library_trash().unwrap().folders.iter().any(|folder| folder.id == id);
    replica.empty_trash().unwrap();
    pull(&replica, &[change(MutationBody::DirectoryParent { dir_id: id, parent_id: ROOT_DIR_ID }, 100, 2), change(MutationBody::DirectoryLifecycle { dir_id: id, value: DirectoryLifecycleState::Present }, 100, 3)]);
    assert_eq!(replica.directory_relative_path_string(&id).unwrap(), Some("Live folder".into()), "emptying Trash must not author a purge for missing lifecycle state; placeholder shown in Trash: {shown_in_trash}");
}

#[test]
fn import_completion_preserves_received_versions_for_every_partial_shape() {
    for mask in 0..8 {
        let replica = db();
        let id = uuid::Uuid::from_u128(104);
        let creation = folder(104, "Shelf");
        let received = creation.into_iter().enumerate().filter_map(|(bit, value)| (mask & (1 << bit) != 0).then_some(value)).collect::<Vec<_>>();
        if !received.is_empty() {
            pull(&replica, &received);
        }
        let versions = || {
            replica
                .connection
                .prepare("SELECT state_kind,changed_at,replica_id,replica_seq,mutation_id,body FROM sync_state_version ORDER BY state_kind")
                .unwrap()
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, String>(4)?, row.get::<_, Vec<u8>>(5)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let before = versions();
        replica.create_directory_with_id(&id, &ROOT_DIR_ID, &"Shelf".into()).unwrap();
        assert_eq!(replica.directory_relative_path_string(&id).unwrap(), Some("Shelf".into()));
        let after = versions();
        for winner in before {
            assert!(after.contains(&winner), "import overwrote a received winner for mask {mask}");
        }
        let pending = replica.sync_publishable_mutations().unwrap();
        assert_eq!(pending.len(), 3 - received.len());
        replica.create_directory_with_id(&id, &ROOT_DIR_ID, &"Shelf".into()).unwrap();
        assert_eq!(versions(), after);
        assert_eq!(replica.sync_publishable_mutations().unwrap(), pending);
    }
}

#[test]
fn import_rejects_partial_conflicts_and_tombstones_without_publication() {
    let id = uuid::Uuid::from_u128(105);
    for body in [
        MutationBody::DirectoryName { dir_id: id, name: "Other".into() },
        MutationBody::DirectoryParent { dir_id: id, parent_id: uuid::Uuid::from_u128(999) },
        MutationBody::DirectoryLifecycle { dir_id: id, value: DirectoryLifecycleState::Deleted },
        MutationBody::DirectoryLifecycle { dir_id: id, value: DirectoryLifecycleState::Purged },
    ] {
        let replica = db();
        pull(&replica, &[change(body, 100, 1)]);
        assert!(replica.create_directory_with_id(&id, &ROOT_DIR_ID, &"Shelf".into()).is_err());
        assert!(replica.sync_publishable_mutations().unwrap().is_empty());
    }
}

#[test]
fn partial_restore_can_be_retried_after_creation_state_arrives() {
    let replica = db();
    let id = uuid::Uuid::from_u128(106);
    pull(&replica, &[change(MutationBody::DirectoryLifecycle { dir_id: id, value: DirectoryLifecycleState::Deleted }, 100, 1)]);
    assert!(replica.restore_directory(&id, None).is_err());
    assert!(replica.sync_publishable_mutations().unwrap().is_empty());
    pull(&replica, &[change(MutationBody::DirectoryName { dir_id: id, name: "Shelf".into() }, 100, 2), change(MutationBody::DirectoryParent { dir_id: id, parent_id: ROOT_DIR_ID }, 100, 3)]);
    assert_eq!(replica.restore_directory(&id, None).unwrap(), "Shelf");
    assert_eq!(replica.directory_relative_path_string(&id).unwrap(), Some("Shelf".into()));
}

#[test]
fn individual_purge_rejects_a_placeholder_without_a_deletion() {
    let replica = db();
    let id = uuid::Uuid::from_u128(107);
    pull(&replica, &[change(MutationBody::DirectoryName { dir_id: id, name: "Shelf".into() }, 100, 1)]);
    assert!(!replica.library_trash().unwrap().folders.iter().any(|folder| folder.id == id));
    assert!(replica.purge_directory(&id).is_err());
    assert!(replica.sync_publishable_mutations().unwrap().is_empty());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn restoring_a_folder_with_nested_books_queues_resolvable_file_work() {
    let replica = db();
    let parent = uuid::Uuid::from_u128(201);
    let child = uuid::Uuid::from_u128(202);
    replica.create_directory_with_id(&parent, &ROOT_DIR_ID, &"Parent".into()).unwrap();
    replica.create_directory_with_id(&child, &parent, &"Child".into()).unwrap();
    pull(&replica, &[present(100, 1), change(MutationBody::Placement { dir_id: child, content_hash: hash(), present: true, origin_folder_id: None }, 100, 2)]);
    replica.trash_directory(&parent).unwrap();
    assert_eq!(replica.directory_relative_path_string(&child).unwrap(), None);
    let result = replica.restore_directory(&parent, None);
    assert!(result.is_ok(), "restoring a parent must rebuild child paths before queuing restored files: {result:?}");
    assert_eq!(replica.directory_relative_path_string(&child).unwrap(), Some("Parent/Child".into()));
    let jobs = replica.native_file_work().unwrap().into_iter().filter(|work| work.operation == "restore").collect::<Vec<_>>();
    assert!(!jobs.is_empty());
    for job in jobs {
        let snapshot = replica.native_file_work_snapshot(job).unwrap().unwrap();
        assert_eq!(snapshot.placement.as_ref().map(|(dir, _)| dir.as_str()), Some(child.to_string().as_str()), "nested restore job has no target: {snapshot:?}");
    }
}

#[test]
fn book_restore_rejects_duplicate_choices_that_omit_an_original_placement() {
    let replica = db();
    let folder = uuid::Uuid::from_u128(203);
    replica.create_directory_with_id(&folder, &ROOT_DIR_ID, &"Shelf".into()).unwrap();
    pull(
        &replica,
        &[
            present(100, 1),
            change(MutationBody::Placement { dir_id: ROOT_DIR_ID, content_hash: hash(), present: true, origin_folder_id: None }, 100, 2),
            change(MutationBody::Placement { dir_id: folder, content_hash: hash(), present: true, origin_folder_id: None }, 100, 3),
        ],
    );
    replica.trash_book(&hash()).unwrap();
    let pending = replica.sync_publishable_mutations().unwrap();
    let result = replica.restore_book_commit(&hash(), &[(ROOT_DIR_ID, ROOT_DIR_ID), (ROOT_DIR_ID, ROOT_DIR_ID)]);
    assert!(result.is_err(), "each original placement needs exactly one choice; duplicate root choices silently omit Shelf: {result:?}");
    assert_eq!(replica.sync_publishable_mutations().unwrap(), pending);
    replica.restore_book_commit(&hash(), &[(ROOT_DIR_ID, ROOT_DIR_ID), (folder, folder)]).unwrap();
    let count: i64 = replica.connection.query_row("SELECT count(*) FROM book_dir WHERE deleted_at IS NULL", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 2, "valid distinct choices must restore both placements");
}

#[test]
fn review_restore_plan_excludes_placements_removed_before_the_book_was_trashed() {
    let replica = db();
    let old = uuid::Uuid::from_u128(301);
    let current = uuid::Uuid::from_u128(302);
    replica.create_directory_with_id(&old, &ROOT_DIR_ID, &"Old".into()).unwrap();
    replica.create_directory_with_id(&current, &ROOT_DIR_ID, &"Current".into()).unwrap();
    pull(
        &replica,
        &[
            present(100, 1),
            change(MutationBody::Placement { dir_id: old, content_hash: hash(), present: false, origin_folder_id: None }, 100, 2),
            change(MutationBody::Placement { dir_id: current, content_hash: hash(), present: true, origin_folder_id: None }, 200, 3),
        ],
    );
    replica.trash_book(&hash()).unwrap();
    let plan = replica.restore_book_plan(&hash()).unwrap();
    assert_eq!(plan.iter().map(|p| p.original).collect::<Vec<_>>(), vec![current], "an older independent placement removal must not be undone by restoring the later book deletion");
    replica.restore_book_commit(&hash(), &[(current, current)]).unwrap();
    let old_removed: bool = replica.connection.query_row("SELECT deleted_at IS NOT NULL FROM book_dir WHERE dir_id=?1", [old.to_string()], |r| r.get(0)).unwrap();
    assert!(old_removed, "committing the plan must preserve the old independent removal");
    let current_live: bool = replica.connection.query_row("SELECT deleted_at IS NULL FROM book_dir WHERE dir_id=?1", [current.to_string()], |r| r.get(0)).unwrap();
    assert!(current_live);
}

#[test]
fn review_restore_plan_prefers_remaining_live_placements_over_old_removals() {
    let replica = db();
    let old = uuid::Uuid::from_u128(306);
    let current = uuid::Uuid::from_u128(307);
    replica.create_directory_with_id(&old, &ROOT_DIR_ID, &"Old".into()).unwrap();
    replica.create_directory_with_id(&current, &ROOT_DIR_ID, &"Current".into()).unwrap();
    // A lifecycle page can arrive before the placement-removal page. A later
    // independent removal must not outweigh a still-present placement either.
    pull(&replica, &[
        change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Deleted { origin_folder_id: None } }, 400, 1),
        change(MutationBody::Placement { dir_id: old, content_hash: hash(), present: false, origin_folder_id: None }, 300, 2),
        change(MutationBody::Placement { dir_id: current, content_hash: hash(), present: true, origin_folder_id: None }, 200, 3),
    ]);
    assert_eq!(replica.restore_book_plan(&hash()).unwrap().iter().map(|p| p.original).collect::<Vec<_>>(), vec![current]);
}

#[test]
fn review_restore_plan_keeps_live_ancestors_of_suppressed_directories() {
    let replica = db();
    let grandparent = uuid::Uuid::from_u128(303);
    let parent = uuid::Uuid::from_u128(304);
    let child = uuid::Uuid::from_u128(305);
    replica.create_directory_with_id(&grandparent, &ROOT_DIR_ID, &"Live".into()).unwrap();
    replica.create_directory_with_id(&parent, &grandparent, &"Deleted".into()).unwrap();
    replica.create_directory_with_id(&child, &parent, &"Child".into()).unwrap();
    pull(&replica, &[present(100, 1), change(MutationBody::Placement { dir_id: child, content_hash: hash(), present: true, origin_folder_id: None }, 100, 2)]);
    replica.trash_directory(&parent).unwrap();
    let plan = replica.restore_book_plan(&hash()).unwrap();
    assert!(plan[0].ancestry.iter().any(|a| a.id == grandparent && a.live), "restore planning lost the original live ancestor: {plan:?}");
    assert_eq!(plan[0].ancestry.iter().map(|a| a.id).collect::<Vec<_>>(), vec![child, parent, grandparent]);
    assert_eq!(plan[0].ancestry.last().unwrap().path, "Live");
    replica.restore_book_commit(&hash(), &[(child, grandparent)]).unwrap();
    let target_live: bool = replica.connection.query_row("SELECT deleted_at IS NULL FROM book_dir WHERE dir_id=?1", [grandparent.to_string()], |r| r.get(0)).unwrap();
    assert!(target_live, "the live ancestor offered by the plan must be a valid restore destination");
    assert!(replica.directory_relative_path_string(&parent).unwrap().is_none(), "restoring the book must not restore the deleted folder");
}

#[test]
fn review_restore_placement_rejects_a_purged_book() {
    let replica = db();
    pull(&replica, &[present(100, 1)]);
    pull(&replica, &[change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Purged }, 200, 2)]);
    let result = replica.restore_book_placement(&hash(), &ROOT_DIR_ID);
    assert!(result.is_err(), "restoring a purged book placement must not report success without a visible placement: {result:?}");
    assert!(replica.sync_publishable_mutations().unwrap().is_empty());
}

#[test]
fn review_invalid_annotation_cannot_poison_later_reimport_while_purged() {
    let replica = db();
    pull(&replica, &[change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Purged }, 100, 1)]);
    let bad = change(
        MutationBody::Annotation {
            annotation_id: "invalid-position".into(),
            value: book_model::AnnotationState {
                content_hash: hash(),
                anchor: book_model::AnnotationAnchor::epub_cfi("epubcfi(/6/2)"),
                exact_text: "Text".into(),
                style: book_model::AnnotationStyle::Underline,
                color: "#fff".into(),
                note: String::new(),
                created_at: 10,
                modified_at: 20,
                deleted: false,
                toc_ordinal: Some(-1),
                progress: Some(10.0),
            },
        },
        200,
        2,
    );
    let page = sync_common::PullStateResponse { book_creations: Vec::new(), mutations: vec![bad], next_cursor: Default::default(), has_more: false };
    let accepted = replica.sync_commit_pull_response(&[], &page);
    if accepted.is_ok() {
        let reimport = sync_common::PullStateResponse { book_creations: vec![present(300, 3)], mutations: vec![present(300, 3)], next_cursor: Default::default(), has_more: false };
        let result = replica.sync_commit_pull_response(&[], &reimport);
        assert!(result.is_ok(), "an annotation accepted while purged poisoned reimport: {result:?}");
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn review_native_collision_ack_reprojects_siblings_before_commit() {
    let replica = db();
    let loser = uuid::Uuid::from_u128(310);
    let winner = uuid::Uuid::from_u128(311);
    replica.create_directory_with_id(&loser, &ROOT_DIR_ID, &"Shelf".into()).unwrap();
    replica.create_directory_with_id(&winner, &ROOT_DIR_ID, &"Shelf".into()).unwrap();
    let snapshot = replica.native_directory_snapshot(&winner).unwrap().unwrap();
    replica.acknowledge_native_directory(crate::NativeDirectoryCompletion { snapshot, renamed_to: Some("Shelf 3".into()), materialized: true }).unwrap();
    assert_eq!(replica.directory_relative_path_string(&loser).unwrap(), Some("Shelf".into()));
    let dirty: i64 = replica.connection.query_row("SELECT count(*) FROM sync_projection_dirty", [], |row| row.get(0)).unwrap();
    assert_eq!(dirty, 0);
}

#[test]
fn review_restore_plan_does_not_revive_an_old_removal_from_a_faster_clock() {
    let replica = db();
    let old = uuid::Uuid::from_u128(401);
    let current = uuid::Uuid::from_u128(402);
    replica.create_directory_with_id(&old, &ROOT_DIR_ID, &"Old".into()).unwrap();
    replica.create_directory_with_id(&current, &ROOT_DIR_ID, &"Current".into()).unwrap();
    // Two minutes of device clock skew is within the server's accepted range.
    // This removal has already arrived before the user trashes the book.
    let ahead = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 120_000;
    pull(&replica, &[
        present(100, 1),
        change(MutationBody::Placement { dir_id: old, content_hash: hash(), present: false, origin_folder_id: None }, ahead, 2),
        change(MutationBody::Placement { dir_id: current, content_hash: hash(), present: true, origin_folder_id: None }, 200, 3),
    ]);
    replica.trash_book(&hash()).unwrap();
    let plan = replica.restore_book_plan(&hash()).unwrap();
    assert_eq!(plan.iter().map(|p| p.original).collect::<Vec<_>>(), vec![current], "a faster clock on an earlier removal must not change which placement this trash operation removed");
}

#[test]
fn review_restore_plan_keeps_every_placement_removed_by_one_trash_operation() {
    let replica = db();
    let first = uuid::Uuid::from_u128(403);
    let second = uuid::Uuid::from_u128(404);
    replica.create_directory_with_id(&first, &ROOT_DIR_ID, &"First".into()).unwrap();
    replica.create_directory_with_id(&second, &ROOT_DIR_ID, &"Second".into()).unwrap();
    let ahead = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 120_000;
    pull(&replica, &[
        present(100, 1),
        change(MutationBody::Placement { dir_id: first, content_hash: hash(), present: true, origin_folder_id: None }, 200, 2),
        change(MutationBody::Placement { dir_id: second, content_hash: hash(), present: true, origin_folder_id: None }, ahead, 3),
    ]);
    replica.trash_book(&hash()).unwrap();
    let plan = replica.restore_book_plan(&hash()).unwrap();
    assert_eq!(plan.iter().map(|p| p.original).collect::<Vec<_>>(), vec![first, second], "one trash operation can assign different per-placement version timestamps and must still restore both placements");
}

#[test]
fn book_trash_preserves_placement_versions_and_restore_moves_only_selected_originals() {
    let replica = db();
    let first = uuid::Uuid::from_u128(405);
    let second = uuid::Uuid::from_u128(406);
    replica.create_directory_with_id(&first, &ROOT_DIR_ID, &"First".into()).unwrap();
    replica.create_directory_with_id(&second, &ROOT_DIR_ID, &"Second".into()).unwrap();
    pull(&replica, &[
        present(100, 1),
        change(MutationBody::Placement { dir_id: first, content_hash: hash(), present: true, origin_folder_id: None }, 200, 2),
        change(MutationBody::Placement { dir_id: second, content_hash: hash(), present: true, origin_folder_id: None }, 300, 3),
    ]);
    let version = |dir: sync_common::DirId| super::apply::registers::canonical_body(&replica.connection, "placement", hash().as_str(), &dir.to_string()).unwrap();
    let before = [version(first), version(second)];
    replica.trash_book(&hash()).unwrap();
    assert_eq!([version(first), version(second)], before, "trash must not author placement changes");
    assert!(replica.sync_publishable_mutations().unwrap().iter().all(|m| !matches!(m.body, MutationBody::Placement { present: false, .. })));
    assert!(replica.local_book_paths().unwrap().is_empty(), "retained placements of a trashed book are not live files");
    replica.restore_book_commit(&hash(), &[(first, ROOT_DIR_ID), (second, second)]).unwrap();
    let live: Vec<String> = replica.connection.prepare("SELECT dir_id FROM book_dir WHERE deleted_at IS NULL ORDER BY dir_id").unwrap()
        .query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(live, vec![ROOT_DIR_ID.to_string(), second.to_string()], "redirecting restore must not leave the old folder association live");
    assert_eq!(version(second), before[1], "a retained placement needs no new version on restore");
}

#[cfg(feature = "scanner")]
#[test]
fn scanner_excludes_retained_placements_of_trashed_books() {
    let replica = db();
    replica.seed_book(&hash(), Some("book"), 1, "epub");
    replica.add_book_placement(&ROOT_DIR_ID, &hash(), "book.epub", "123", true);
    assert_eq!(replica.scan_snapshot().unwrap().placements.len(), 1);
    assert_eq!(replica.transfer_snapshot(&[hash()]).unwrap().asset_for(&hash()).unwrap().upload.local_versions.len(), 1);
    replica.trash_book(&hash()).unwrap();
    assert!(replica.book_upload_snapshot(&hash()).unwrap().local_versions.is_empty());
    assert!(replica.transfer_snapshot(&[hash()]).unwrap().asset_for(&hash()).unwrap().upload.local_versions.is_empty());
    let snapshot = replica.scan_snapshot().unwrap();
    assert!(snapshot.books.is_empty());
    assert!(snapshot.placements.is_empty());
    assert_eq!(snapshot.tombstoned_files, vec![(ROOT_DIR_ID, "book.epub".into())]);
    assert_eq!(replica.restore_book_plan(&hash()).unwrap()[0].original, ROOT_DIR_ID);
}

fn take_publications(replica: &crate::Database) -> Vec<ServerMutation> {
    let id: String = replica.connection.query_row("SELECT replica_id FROM sync_metadata", [], |r| r.get(0)).unwrap();
    let pending = replica.sync_publishable_mutations().unwrap();
    let events = pending
        .iter()
        .enumerate()
        .map(|(index, m)| ServerMutation {
            mutation: m.to_wire().unwrap(),
            replica_id: uuid::Uuid::parse_str(&id).unwrap(),
            revision: LibraryRevision::new(index as u64 + 1).unwrap(),
        })
        .collect();
    replica.sync_acknowledge_mutations(&pending.iter().map(|m| m.mutation_id).collect::<Vec<_>>()).unwrap();
    events
}

fn membership_snapshot(replica: &crate::Database) -> Vec<Vec<rusqlite::types::Value>> {
    let mut rows = Vec::new();
    for sql in [
        "SELECT state_kind,state_key,state_subkey,changed_at,conflict_rank,replica_id,replica_seq,mutation_id,body FROM sync_state_version ORDER BY state_kind,state_key,state_subkey",
        "SELECT content_hash,deleted_at IS NOT NULL,trash_origin_dir_id FROM book ORDER BY content_hash",
        "SELECT b.content_hash,bd.dir_id,bd.deleted_at IS NOT NULL FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id ORDER BY b.content_hash,bd.dir_id",
        "SELECT id,parent_id,deleted_at IS NOT NULL FROM dir ORDER BY id",
    ] {
        let mut statement = replica.connection.prepare(sql).unwrap();
        let count = statement.column_count();
        rows.extend(statement.query_map([], |row| (0..count).map(|column| row.get(column)).collect()).unwrap().collect::<Result<Vec<Vec<_>>, _>>().unwrap());
    }
    rows
}

fn membership_seed(two_placements: bool) -> Vec<ServerMutation> {
    let mut seed = folder(501, "Source");
    seed.extend(folder(502, "Destination"));
    seed.push(present(100, 6000));
    for id in if two_placements { vec![501, 502] } else { vec![501] } {
        seed.push(change(
            MutationBody::Placement {
                dir_id: uuid::Uuid::from_u128(id),
                content_hash: hash(),
                present: true,
                origin_folder_id: None,
            },
            100,
            6000 + id as u64,
        ));
    }
    seed
}

#[test]
fn concurrent_trash_and_transfer_preserve_membership_in_both_delivery_orders() {
    let source = uuid::Uuid::from_u128(501);
    let destination = uuid::Uuid::from_u128(502);
    for folder_trash in [false, true] {
        for remove_source in [false, true] {
            let seed = membership_seed(false);
            let a = db();
            let b = db();
            for replica in [&a, &b] {
                pull(replica, &seed);
                take_publications(replica);
            }
            if folder_trash {
                a.trash_directory(&source).unwrap();
            } else {
                a.trash_book(&hash()).unwrap();
            }
            b.transfer_book_placement(&hash(), &source, &destination, remove_source).unwrap();
            let trash = take_publications(&a);
            let transfer = take_publications(&b);
            pull(&a, &transfer);
            pull(&b, &trash);
            assert_eq!(membership_snapshot(&a), membership_snapshot(&b));
            assert!(a.local_book_paths().unwrap().is_empty(), "a concurrent transfer must not clear Trash");
            let expected = membership_snapshot(&a);
            for reversed in [false, true] {
                let observer = db();
                pull(&observer, &seed);
                let groups = if reversed { [&transfer, &trash] } else { [&trash, &transfer] };
                for group in groups {
                    for event in group {
                        pull(&observer, std::slice::from_ref(event));
                    }
                }
                assert_eq!(membership_snapshot(&observer), expected);
                for group in groups {
                    pull(&observer, group);
                }
                assert_eq!(membership_snapshot(&observer), expected, "duplicate delivery must be inert");
            }
            let plan = a.restore_book_plan(&hash()).unwrap();
            assert_eq!(plan.len(), if remove_source { 1 } else { 2 });
            let choices = plan.iter().map(|p| (p.original, if folder_trash { destination } else { p.original })).collect::<Vec<_>>();
            a.restore_book_commit(&hash(), &choices).unwrap();
            let restore = take_publications(&a);
            pull(&b, &restore);
            assert_eq!(membership_snapshot(&a), membership_snapshot(&b));
            assert!(!a.local_book_paths().unwrap().is_empty());
        }
    }
}

#[test]
fn concurrent_removal_survives_book_restore_and_rejects_a_stale_restore_plan() {
    let source = uuid::Uuid::from_u128(501);
    let destination = uuid::Uuid::from_u128(502);
    for removal_before_commit in [false, true] {
        let seed = membership_seed(true);
        let a = db();
        let b = db();
        for replica in [&a, &b] {
            pull(replica, &seed);
            take_publications(replica);
        }
        a.trash_book(&hash()).unwrap();
        let trash = take_publications(&a);
        let plan = a.restore_book_plan(&hash()).unwrap();
        let choices = plan.iter().map(|p| (p.original, p.original)).collect::<Vec<_>>();
        assert!(!b.remove_book_placement(&hash(), &source).unwrap());
        let removal = take_publications(&b);
        if removal_before_commit {
            pull(&a, &removal);
            let before = membership_snapshot(&a);
            assert!(a.restore_book_commit(&hash(), &choices).is_err());
            assert_eq!(membership_snapshot(&a), before, "stale plans must fail atomically");
            assert!(a.sync_publishable_mutations().unwrap().is_empty());
            a.restore_book_commit(&hash(), &[(destination, destination)]).unwrap();
        } else {
            a.restore_book_commit(&hash(), &choices).unwrap();
        }
        let restore = take_publications(&a);
        assert!(restore.iter().all(|m| m.mutation.kind != "placement" || m.mutation.origin.is_some()), "ordinary restore must not author placements");
        pull(&a, &removal);
        pull(&b, &trash);
        pull(&b, &restore);
        assert_eq!(membership_snapshot(&a), membership_snapshot(&b));
        let removed: bool = a.connection.query_row("SELECT deleted_at IS NOT NULL FROM book_dir WHERE dir_id=?1", [source.to_string()], |r| r.get(0)).unwrap();
        assert!(removed);
        for events in [vec![&removal, &trash, &restore], vec![&restore, &trash, &removal]] {
            let observer = db();
            pull(&observer, &seed);
            for group in events {
                pull(&observer, group);
            }
            assert_eq!(membership_snapshot(&observer), membership_snapshot(&a));
        }
    }
}

#[test]
fn ordinary_restore_preserves_retained_local_file_metadata() {
    let replica = db();
    pull(&replica, &membership_seed(false));
    replica.connection.execute("UPDATE book_dir SET local_hash='retained-checksum',last_scan=123", []).unwrap();
    replica.trash_book(&hash()).unwrap();
    let source = uuid::Uuid::from_u128(501);
    replica.restore_book_commit(&hash(), &[(source, source)]).unwrap();
    let retained: (String, i64) = replica.connection.query_row("SELECT local_hash,last_scan FROM book_dir", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(retained, ("retained-checksum".into(), 123));
}
