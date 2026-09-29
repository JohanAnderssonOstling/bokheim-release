use super::convergence_tests::{change, db, hash, metadata, present};
use library_replica::{BookLifecycleState, MutationBody, ReadingPositionState};
use sync_common::{PullStateResponse, ROOT_DIR_ID, SyncCursor};

fn page(mutations: Vec<sync_common::ServerMutation>) -> PullStateResponse {
    PullStateResponse { book_creations: vec![], mutations, next_cursor: SyncCursor::default(), has_more: false }
}
fn rows(db: &crate::Database) -> i64 {
    db.connection.query_row("SELECT count(*) FROM book", [], |r| r.get(0)).unwrap()
}

#[test]
fn ordinary_fields_and_lifecycle_updates_never_create_a_missing_book() {
    let values = vec![
        metadata("en", 100, 1),
        change(MutationBody::Description { content_hash: hash(), value: "Description".into() }, 100, 2),
        change(MutationBody::Placement { dir_id: ROOT_DIR_ID, content_hash: hash(), present: true, origin_folder_id: None }, 100, 3),
        change(MutationBody::ReadingPosition { content_hash: hash(), value: ReadingPositionState { location: book_model::ReadingPosition::parse("epubcfi(/6/2)").unwrap(), progress: 12.0 } }, 100, 4),
        change(MutationBody::BookFacts { content_hash: hash(), value: library_replica::BookFacts { added_at: 1, format: book_model::BookFormat::Epub } }, 100, 5),
        change(MutationBody::BookToc { content_hash: hash(), value: Default::default() }, 100, 6),
        change(
            MutationBody::Annotation {
                annotation_id: "early-annotation".into(),
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
                    toc_ordinal: Some(0),
                    progress: Some(10.0),
                },
            },
            100,
            8,
        ),
        change(
            MutationBody::PdfReaderMetadata {
                content_hash: hash(),
                value: pdf_view_common::PdfReaderMetadata {
                    version: 1,
                    checksum: hash().to_string(),
                    pages: vec![pdf_view_common::PdfStoredPage { width: 100.0, height: 200.0, rotation: 0 }],
                    skippable_pages: None,
                    annotations: vec![],
                    dependencies: None,
                },
            },
            100,
            9,
        ),
        present(100, 7),
    ];
    let replica = db();
    for value in values {
        replica.sync_commit_pull_response(&[], &page(vec![value])).unwrap();
        assert_eq!(rows(&replica), 0);
    }
    // Absent-book fields are discarded; a declaration alone cannot recover them.
    let mut creation = page(vec![]);
    creation.book_creations.push(present(100, 7));
    replica.sync_commit_pull_response(&[], &creation).unwrap();
    assert_eq!(rows(&replica), 1);
    let state: (Option<String>, String, f64) = replica.connection.query_row("SELECT title,format,read_progress FROM book", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(state, (None, "".into(), 0.0));
    assert!(replica.sync_publishable_mutations().unwrap().is_empty());
}

#[test]
fn creation_behind_cursor_precedes_reading_and_does_not_advance_state_cursor() {
    let replica = db();
    let cursor = SyncCursor { state_revision: sync_common::LibraryRevision::new(50).ok(), reading_revision: None };
    replica.set_sync_pull_cursor(&cursor).unwrap();
    let mut response = page(vec![change(MutationBody::ReadingPosition { content_hash: hash(), value: ReadingPositionState { location: book_model::ReadingPosition::parse("epubcfi(/6/2)").unwrap(), progress: 33.0 } }, 100, 51)]);
    response.book_creations = vec![present(1, 1)];
    response.next_cursor = SyncCursor { reading_revision: sync_common::LibraryRevision::new(51).ok(), ..cursor };
    sync_common::validate_pull_batch(&response, cursor).unwrap();
    replica.sync_commit_pull_response(&[], &response).unwrap();
    assert_eq!(rows(&replica), 1);
    assert_eq!(replica.sync_pull_cursor().unwrap(), response.next_cursor);
    replica.sync_commit_pull_response(&[], &response).unwrap();
    assert_eq!(rows(&replica), 1);
    assert!(replica.sync_publishable_mutations().unwrap().is_empty());
}

#[test]
fn retained_present_cannot_recreate_a_physically_removed_row() {
    let replica = db();
    let mut creation = page(vec![]);
    creation.book_creations.push(present(1, 1));
    replica.sync_commit_pull_response(&[], &creation).unwrap();
    replica.connection.execute_batch("UPDATE sync_metadata SET change_origin='remote'; DELETE FROM book; UPDATE sync_metadata SET change_origin='local';").unwrap();
    replica.sync_commit_pull_response(&[], &page(vec![metadata("fr", 100, 2)])).unwrap();
    assert_eq!(rows(&replica), 0);
}

#[test]
fn invalid_creation_rolls_back_acknowledgement_and_cursor() {
    let replica = db();
    replica.create_directory(&ROOT_DIR_ID, &"Pending".into()).unwrap();
    let pending = replica.sync_publishable_mutations().unwrap();
    let mut response = page(vec![]);
    response.book_creations.push(metadata("en", 100, 1));
    assert!(replica.sync_commit_pull_response(&pending.iter().map(|m| m.mutation_id).collect::<Vec<_>>(), &response).is_err());
    assert_eq!(replica.sync_publishable_mutations().unwrap(), pending);
    assert_eq!(rows(&replica), 0);
}

#[test]
fn trash_snapshot_can_explicitly_create_but_purge_cannot() {
    let replica = db();
    let mut response = page(vec![metadata("en", 100, 2)]);
    response.book_creations.push(change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Deleted { origin_folder_id: None } }, 200, 3));
    replica.sync_commit_pull_response(&[], &response).unwrap();
    let deleted: bool = replica.connection.query_row("SELECT deleted_at IS NOT NULL FROM book", [], |r| r.get(0)).unwrap();
    assert!(deleted);
    let mut invalid = page(vec![]);
    invalid.book_creations.push(change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Purged }, 300, 4));
    assert!(replica.sync_commit_pull_response(&[], &invalid).is_err());
}

#[test]
fn stale_creation_cannot_override_purge_in_same_or_previous_page() {
    for separate_page in [false, true] {
        let replica = db();
        let purge = change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Purged }, 300, 3);
        let mut response = page(vec![metadata("en", 200, 2)]);
        response.book_creations.push(present(100, 1));
        if separate_page {
            replica.sync_commit_pull_response(&[], &page(vec![purge])).unwrap();
        } else {
            response.mutations.push(purge);
        }
        replica.sync_commit_pull_response(&[], &response).unwrap();
        assert_eq!(rows(&replica), 0);
        let mut reimport = page(vec![]);
        reimport.book_creations.push(present(400, 4));
        replica.sync_commit_pull_response(&[], &reimport).unwrap();
        assert_eq!(rows(&replica), 1);
        assert!(replica.sync_publishable_mutations().unwrap().is_empty());
    }
}

#[test]
fn missed_purge_readd_republishes_acknowledged_annotation_with_original_version() {
    for deleted in [false, true] {
        let annotation = change(
            MutationBody::Annotation {
                annotation_id: "surviving-note".into(),
                value: book_model::AnnotationState {
                    content_hash: hash(),
                    anchor: book_model::AnnotationAnchor::epub_cfi("epubcfi(/6/2)"),
                    exact_text: "Text".into(),
                    style: book_model::AnnotationStyle::Highlight,
                    color: "#fff".into(),
                    note: "Acknowledged before purge".into(),
                    created_at: 1,
                    modified_at: 2,
                    deleted,
                    toc_ordinal: None,
                    progress: None,
                },
            },
            10,
            2,
        );
        let offline = db();
        let other = db();
        let initial = PullStateResponse { book_creations: vec![present(1, 1)], ..page(vec![annotation.clone()]) };
        for replica in [&offline, &other] {
            replica.sync_commit_pull_response(&[], &initial).unwrap();
            assert!(replica.sync_publishable_mutations().unwrap().is_empty());
        }
        other.sync_commit_pull_response(&[], &page(vec![change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Purged }, 20, 3)])).unwrap();
        let readd = PullStateResponse { book_creations: vec![present(30, 4)], ..page(vec![]) };
        for replica in [&offline, &other] {
            replica.sync_commit_pull_response(&[], &readd).unwrap();
        }
        let queued = offline.sync_publishable_mutations().unwrap();
        assert_eq!(queued.len(), 1, "acknowledged surviving annotation must be reconciled");
        let publication = &queued[0];
        let publisher = uuid::Uuid::from_u128(99);
        let wire = publication.to_wire().unwrap();
        assert_eq!(library_replica::VersionKey::from_wire(&wire, publisher), library_replica::VersionKey::from_wire(&annotation.mutation, annotation.replica_id));
        assert_eq!(offline.sync_publishable_mutations().unwrap(), queued, "retries preserve publication identity");
        let echoed = sync_common::ServerMutation { mutation: wire, replica_id: publisher, revision: sync_common::LibraryRevision::new(5).unwrap() };
        let reconciled = PullStateResponse { mutations: vec![echoed], ..readd.clone() };
        let fresh = db();
        for replica in [&offline, &other, &fresh] {
            replica.sync_commit_pull_response(&[publication.mutation_id], &reconciled).unwrap();
            let annotations = replica.annotations(hash()).unwrap();
            assert_eq!(annotations.len(), usize::from(!deleted));
            if let Some(annotation) = annotations.first() {
                assert_eq!(annotation.note, "Acknowledged before purge");
            }
            replica.sync_commit_pull_response(&[], &readd).unwrap();
            assert!(replica.sync_publishable_mutations().unwrap().is_empty(), "repeated declaration/echo must not start another publication");
        }
    }
}

#[test]
fn reconciliation_keeps_local_winner_but_does_not_echo_server_confirmed_fields() {
    for remote_time in [5, 10, 40] {
        let replica = db();
        let old = metadata("en", 10, 2);
        replica.sync_commit_pull_response(&[], &PullStateResponse { book_creations: vec![present(1, 1)], ..page(vec![old.clone()]) }).unwrap();
        let remote = if remote_time == 10 { old.clone() } else { metadata("fr", remote_time, 3) };
        let response = PullStateResponse { book_creations: vec![present(30, 4)], ..page(vec![remote]) };
        replica.sync_commit_pull_response(&[], &response).unwrap();
        let pending = replica.sync_publishable_mutations().unwrap();
        assert_eq!(pending.len(), usize::from(remote_time < 10));
        if let Some(publication) = pending.first() {
            assert_eq!(library_replica::VersionKey::from_wire(&publication.to_wire().unwrap(), uuid::Uuid::from_u128(99)), library_replica::VersionKey::from_wire(&old.mutation, old.replica_id));
        }
    }
}

#[test]
fn reconciliation_is_atomic_durable_and_does_not_replace_pending_work() {
    let path = std::env::temp_dir().join(format!("missed-cycle-{}.sqlite3", uuid::Uuid::new_v4()));
    let replica = crate::Database::open(&path).unwrap();
    replica.initialize_library().unwrap();
    replica.sync_commit_pull_response(&[], &PullStateResponse { book_creations: vec![present(1, 1)], ..page(vec![metadata("en", 10, 2)]) }).unwrap();
    let response = PullStateResponse { book_creations: vec![present(30, 4)], ..page(vec![]) };
    replica.connection.execute_batch("CREATE TEMP TRIGGER reject_recovery BEFORE INSERT ON sync_outbox BEGIN SELECT RAISE(ABORT,'injected recovery failure'); END;").unwrap();
    assert!(replica.sync_commit_pull_response(&[], &response).is_err());
    assert_eq!(replica.connection.query_row("SELECT changed_at FROM sync_state_version WHERE state_kind='book_lifecycle'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    replica.connection.execute_batch("DROP TRIGGER reject_recovery;").unwrap();
    replica.sync_commit_pull_response(&[], &response).unwrap();
    let pending = replica.sync_publishable_mutations().unwrap();
    assert_eq!(pending.len(), 1);
    drop(replica);
    let replica = crate::Database::open(&path).unwrap();
    replica.initialize_library().unwrap();
    assert_eq!(replica.sync_publishable_mutations().unwrap(), pending);
    replica.sync_commit_pull_response(&[], &PullStateResponse { book_creations: vec![present(40, 5)], ..page(vec![]) }).unwrap();
    assert_eq!(replica.sync_publishable_mutations().unwrap(), pending, "another declaration must preserve the pending publication identity");
    replica.sync_commit_pull_response(&[], &page(vec![change(MutationBody::BookLifecycle { content_hash: hash(), value: BookLifecycleState::Purged }, 50, 6)])).unwrap();
    assert!(replica.sync_publishable_mutations().unwrap().is_empty(), "a later purge cancels reconciliation");
    drop(replica);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn locally_authored_existence_reconciles_fields_before_its_equal_version_echo() {
    let replica = db();
    let old = metadata("en", 10, 2);
    replica.sync_commit_pull_response(&[], &PullStateResponse { book_creations: vec![present(1, 1)], ..page(vec![old.clone()]) }).unwrap();
    replica.trash_book(&hash()).unwrap();
    let pending = replica.sync_publishable_mutations().unwrap();
    let field = pending.iter().find(|m| matches!(m.body, MutationBody::Metadata { .. })).expect("local existence change queues acknowledged fields too");
    let publisher = uuid::Uuid::parse_str(&replica.connection.query_row("SELECT replica_id FROM sync_metadata", [], |r| r.get::<_, String>(0)).unwrap()).unwrap();
    assert_eq!(library_replica::VersionKey::from_wire(&field.to_wire().unwrap(), publisher), library_replica::VersionKey::from_wire(&old.mutation, old.replica_id));
    let echoed = pending.iter().enumerate().map(|(i, m)| sync_common::ServerMutation { mutation: m.to_wire().unwrap(), replica_id: publisher, revision: sync_common::LibraryRevision::new(i as u64 + 1).unwrap() }).collect::<Vec<_>>();
    let creation = echoed.iter().find(|m| m.mutation.kind == "book_lifecycle").unwrap().clone();
    replica.sync_commit_pull_response(&pending.iter().map(|m| m.mutation_id).collect::<Vec<_>>(), &PullStateResponse { book_creations: vec![creation], ..page(echoed) }).unwrap();
    assert!(replica.sync_publishable_mutations().unwrap().is_empty());
}

#[test]
fn acknowledgement_confirms_only_its_matching_field_generation() {
    for superseded in [false, true] {
        let replica = db();
        replica.sync_commit_pull_response(&[], &PullStateResponse { book_creations: vec![present(1, 1)], ..page(vec![metadata("en", 10, 2)]) }).unwrap();
        replica.sync_enqueue_missing_state_cells(&[sync_common::StateCell { kind: "metadata".into(), entity_key: hash().to_string(), entity_subkey: String::new() }]).unwrap();
        let sent = replica.sync_publishable_mutations().unwrap();
        if superseded {
            replica.sync_commit_pull_response(&[], &page(vec![metadata("fr", 20, 3)])).unwrap();
        }
        let pending_before = replica.sync_publishable_mutations().unwrap();
        replica.sync_commit_pull_response(&[sent[0].mutation_id], &PullStateResponse { book_creations: vec![present(30, 4)], ..page(vec![]) }).unwrap();
        let after = replica.sync_publishable_mutations().unwrap();
        if superseded {
            assert_eq!(after, pending_before, "an old acknowledgement cannot confirm a newer field");
        } else {
            assert!(after.is_empty(), "an acknowledged field need not wait for its pull page to be confirmed");
        }
    }
}
