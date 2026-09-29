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
