//! Integration coverage across two independent library actors and asset stores.
use super::concurrency_tests::fixture;
use super::*;
use crate::LibraryClient;
use crate::client::TransferSelection;
use book_model::{Annotation, AnnotationAnchor, AnnotationStyle};
use sync_common::{BookId, ROOT_DIR_ID};

const EPUB: &[u8] = include_bytes!("../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
const OTHER_EPUB: &[u8] = include_bytes!("../../crates/book-inspection/tests/fixtures/malformed-language.epub");

async fn seed(session: &LibrarySession, parent: DirId, name: &str, bytes: &[u8]) -> BookId {
    let input = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(input.path(), bytes).unwrap();
    session.import.import_stored_book(parent, name.into(), input.path().into()).await.unwrap()
}
fn selection(folders: Vec<DirId>, books: Vec<BookId>, source: Option<DirId>) -> TransferSelection {
    TransferSelection { folders, books, source }
}
fn clients(source: &LibraryHandle, destination: &LibraryHandle) -> (LibraryClient, LibraryClient) {
    (LibraryClient::new(source.id, source.clone()), LibraryClient::new(destination.id, destination.clone()))
}

#[tokio::test]
async fn cross_library_copy_preserves_nested_empty_folders_bytes_and_reader_state() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (mut source_actor, source, source_inbox) = fixture(first.path(), "http://127.0.0.1:1".parse().unwrap());
    let (mut destination_actor, destination, destination_inbox) = fixture(second.path(), "http://127.0.0.1:1".parse().unwrap());
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    let shelf = src.placement.create_directory(&ROOT_DIR_ID, "Shelf").unwrap().id;
    let nested = src.placement.create_directory(&shelf, "Nested").unwrap().id;
    src.placement.create_directory(&nested, "Empty").unwrap();
    let hash = seed(&src, nested, "Novel.epub", EPUB).await;
    src.placement.copy_book_to_directory(&hash, Some(&nested), &shelf).await.unwrap();
    src.db.update_reading_position(&hash, "epubcfi(/6/2)", Some(42.0), None).unwrap();
    let note = Annotation {
        id: "transfer-note".into(),
        content_hash: hash,
        anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/2)"),
        exact_text: "Text".into(),
        style: AnnotationStyle::Underline,
        color: 0xffffff,
        note: "Remember".into(),
        created_at: 7,
        toc_ordinal: None,
        progress: Some(42.0),
        modified_at: 9,
        deleted: false,
    };
    src.db.upsert_annotation(&note).unwrap();
    let original = src.db.book_transfer_state(hash).unwrap();
    let (from, to) = clients(&source, &destination);
    let exercise = async {
        // Selecting both an ancestor and child copies the tree only once.
        let outcome = to.transfer_from(&from, selection(vec![shelf, nested], vec![], None), ROOT_DIR_ID, false).await;
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(outcome.created_folders.len(), 1);
        let folders = dst.db.folder_destinations().unwrap();
        assert_eq!(folders.len(), 4);
        assert!(folders.iter().any(|folder| folder.label == "Empty"));
        let transferred = dst.db.book_transfer_state(hash).unwrap();
        assert_eq!(transferred.addition, original.addition);
        assert_eq!(transferred.reading, original.reading);
        assert_eq!(transferred.annotations, original.annotations);
        let copied = dst.db.transfer_plan(selection(outcome.created_folders.clone(), vec![], None)).unwrap();
        assert_eq!(copied.books.len(), 2, "repeated placements must survive");
        for path in dst.db.book_paths(&hash).unwrap() {
            assert_eq!(std::fs::read(second.path().join(path.as_str().trim_start_matches('/'))).unwrap(), EPUB);
        }
        assert!(src.db.is_live_destination(&shelf).unwrap());
        source.shutdown();
        destination.shutdown();
    };
    tokio::join!(source_actor.serve(source_inbox), destination_actor.serve(destination_inbox), exercise);
}

#[tokio::test]
async fn cross_library_move_from_local_only_to_cloud_library_keeps_verified_local_bytes_and_is_reversible() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (mut source_actor, source, source_inbox) = fixture(first.path(), "http://127.0.0.1:1".parse().unwrap());
    let (mut destination_actor, destination, destination_inbox) = fixture(second.path(), "http://127.0.0.1:1".parse().unwrap());
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    src.db.set_asset_storage_policy(false, false).unwrap();
    src.sync.clone().set_asset_storage_enabled(false);
    let hash = seed(&src, ROOT_DIR_ID, "Local.epub", EPUB).await;
    let facts = src.db.book_transfer_state(hash).unwrap().addition;
    src.db.record_catalog_answer(&hash, facts.record).unwrap();
    let (from, to) = clients(&source, &destination);
    let exercise = async {
        let outcome = to.transfer_from(&from, selection(vec![], vec![hash], Some(ROOT_DIR_ID)), ROOT_DIR_ID, true).await;
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(outcome.created_placements, vec![(hash, ROOT_DIR_ID)]);
        assert_eq!(outcome.removed_placements, vec![(hash, ROOT_DIR_ID)]);
        assert!(dst.sync.clone().asset_storage_enabled());
        assert!(!src.sync.clone().asset_storage_enabled());
        assert_eq!(src.db.library_trash().unwrap().books.len(), 1);
        assert_eq!(std::fs::read(second.path().join(dst.db.book_paths(&hash).unwrap()[0].as_str().trim_start_matches('/'))).unwrap(), EPUB);
        // Destination receives a local upload revision independently of source policy.
        assert!(dst.db.upload_preparation_snapshot().unwrap().hashes().contains(&hash));
        dst.run_file_jobs().await.unwrap();
        dst.admit_uploads().unwrap();
        assert!(dst.db.pending_book_uploads(0, 10).unwrap().iter().any(|intent| intent.content_hash == hash));
        from.restore_book_placement(hash, ROOT_DIR_ID).await.unwrap();
        to.remove_book_from_directory(hash, ROOT_DIR_ID).await.unwrap();
        assert!(src.db.library_trash().unwrap().books.is_empty());
        assert!(!src.db.book_paths(&hash).unwrap().is_empty());
        source.shutdown();
        destination.shutdown();
    };
    tokio::join!(source_actor.serve(source_inbox), destination_actor.serve(destination_inbox), exercise);
}

#[tokio::test]
async fn cross_library_failed_copy_preserves_the_entire_source_selection_and_reports_partial_destination_changes() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (mut source_actor, source, source_inbox) = fixture(first.path(), "http://127.0.0.1:1".parse().unwrap());
    let (mut destination_actor, destination, destination_inbox) = fixture(second.path(), "http://127.0.0.1:1".parse().unwrap());
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    let good = seed(&src, ROOT_DIR_ID, "Good.epub", EPUB).await;
    let broken = seed(&src, ROOT_DIR_ID, "Broken.epub", OTHER_EPUB).await;
    let path = src.db.book_paths(&broken).unwrap()[0].clone();
    std::fs::write(first.path().join(path.as_str().trim_start_matches('/')), b"corrupt").unwrap();
    let (from, to) = clients(&source, &destination);
    let exercise = async {
        let outcome = to.transfer_from(&from, selection(vec![], vec![good, broken], Some(ROOT_DIR_ID)), ROOT_DIR_ID, true).await;
        assert!(outcome.error.is_some());
        assert_eq!(outcome.created_placements, vec![(good, ROOT_DIR_ID)]);
        assert!(outcome.removed_placements.is_empty());
        assert!(src.db.library_trash().unwrap().books.is_empty());
        assert!(src.db.transfer_plan(selection(vec![], vec![good, broken], Some(ROOT_DIR_ID))).is_ok());
        assert!(dst.db.book_paths(&broken).unwrap().is_empty());
        source.shutdown();
        destination.shutdown();
    };
    tokio::join!(source_actor.serve(source_inbox), destination_actor.serve(destination_inbox), exercise);
}

#[tokio::test]
async fn cross_library_move_rejects_a_source_tree_changed_during_copy() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let shelf = actor.session.placement.create_directory(&ROOT_DIR_ID, "Shelf").unwrap().id;
    seed(&actor.session, shelf, "Novel.epub", EPUB).await;
    let plan = actor.session.db.transfer_plan(selection(vec![shelf], vec![], None)).unwrap();
    actor.session.placement.create_directory(&shelf, "Added during copy").unwrap();
    assert!(actor.session.db.remove_transferred_selection(&plan).is_err());
    assert!(actor.session.db.is_live_destination(&shelf).unwrap());
    assert!(actor.session.db.library_trash().unwrap().books.is_empty());
}

#[tokio::test]
async fn cross_library_copy_of_an_mp3_folder_transfers_the_full_archive_and_track_metadata() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let input = tempfile::tempdir().unwrap();
    let audio = input.path().join("Audio");
    std::fs::create_dir(&audio).unwrap();
    let track = include_bytes!("../../crates/book-inspection/tests/fixtures/silence.mp3");
    std::fs::write(audio.join("01.mp3"), track).unwrap();
    std::fs::write(audio.join("02.mp3"), track).unwrap();
    let (mut source_actor, source, source_inbox) = fixture(first.path(), "http://127.0.0.1:1".parse().unwrap());
    let (mut destination_actor, destination, destination_inbox) = fixture(second.path(), "http://127.0.0.1:1".parse().unwrap());
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    assert!(src.import_paths(ROOT_DIR_ID, vec![audio], false).await.unwrap().is_empty());
    src.scan_folder().await.unwrap();
    let hash = src.db.upload_preparation_snapshot().unwrap().hashes().into_iter().next().unwrap();
    let original = src.db.book_transfer_state(hash).unwrap();
    let (from, to) = clients(&source, &destination);
    let exercise = async {
        let outcome = to.transfer_from(&from, selection(vec![], vec![hash], Some(ROOT_DIR_ID)), ROOT_DIR_ID, false).await;
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(dst.db.book_transfer_state(hash).unwrap().addition, original.addition);
        let path = dst.db.book_paths(&hash).unwrap()[0].clone();
        let audio = second.path().join(path.as_str().trim_start_matches('/'));
        assert_eq!(std::fs::read(audio.join("01.mp3")).unwrap(), track);
        assert_eq!(std::fs::read(audio.join("02.mp3")).unwrap(), track);
        assert_eq!(dst.db.audiobook_tracks(&hash).unwrap().len(), 2);
        source.shutdown();
        destination.shutdown();
    };
    tokio::join!(source_actor.serve(source_inbox), destination_actor.serve(destination_inbox), exercise);
}

#[tokio::test]
async fn cross_library_move_to_existing_book_preserves_destination_state_and_other_source_placements() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (mut source_actor, source, source_inbox) = fixture(first.path(), "http://127.0.0.1:1".parse().unwrap());
    let (mut destination_actor, destination, destination_inbox) = fixture(second.path(), "http://127.0.0.1:1".parse().unwrap());
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    let hash = seed(&src, ROOT_DIR_ID, "Novel.epub", EPUB).await;
    seed(&dst, ROOT_DIR_ID, "Existing.epub", EPUB).await;
    let retained = src.placement.create_directory(&ROOT_DIR_ID, "Retained").unwrap().id;
    src.placement.copy_book_to_directory(&hash, Some(&ROOT_DIR_ID), &retained).await.unwrap();
    src.db.update_reading_position(&hash, "epubcfi(/6/2)", Some(20.0), None).unwrap();
    dst.db.update_reading_position(&hash, "epubcfi(/6/4)", Some(60.0), None).unwrap();
    let original = dst.db.book_transfer_state(hash).unwrap();
    let (from, to) = clients(&source, &destination);
    let exercise = async {
        let outcome = to.transfer_from(&from, selection(vec![], vec![hash], None), ROOT_DIR_ID, true).await;
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert!(outcome.created_placements.is_empty(), "existing destination placement is excluded from undo");
        assert_eq!(outcome.removed_placements, vec![(hash, ROOT_DIR_ID)]);
        let after = dst.db.book_transfer_state(hash).unwrap();
        assert_eq!(after.addition, original.addition);
        assert_eq!(after.reading, original.reading);
        assert!(second.path().join("Existing.epub").is_file());
        assert!(!second.path().join("Novel.epub").exists());
        assert!(src.db.transfer_plan(selection(vec![], vec![hash], Some(retained))).is_ok());
        assert!(src.db.transfer_plan(selection(vec![], vec![hash], Some(ROOT_DIR_ID))).is_err());
        assert!(src.db.library_trash().unwrap().books.is_empty());
        source.shutdown();
        destination.shutdown();
    };
    tokio::join!(source_actor.serve(source_inbox), destination_actor.serve(destination_inbox), exercise);
}

#[tokio::test]
async fn cross_library_import_rejects_wrong_bytes_before_publishing() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (source_actor, _, _) = fixture(first.path(), "http://127.0.0.1:1".parse().unwrap());
    let (destination_actor, _, _) = fixture(second.path(), "http://127.0.0.1:1".parse().unwrap());
    let hash = seed(&source_actor.session, ROOT_DIR_ID, "Novel.epub", EPUB).await;
    let state = source_actor.session.db.book_transfer_state(hash).unwrap();
    let result = destination_actor.session.import.import_transferred_book(ROOT_DIR_ID, "Novel.epub".into(), Box::new(std::io::Cursor::new(b"bad bytes".to_vec())), state).await;
    assert!(result.unwrap_err().to_string().contains("verification"));
    assert!(destination_actor.session.db.book_paths(&hash).unwrap().is_empty());
    assert!(!second.path().join("Novel.epub").exists());
    assert!(!source_actor.session.db.book_paths(&hash).unwrap().is_empty());
}

#[tokio::test]
async fn cross_library_folder_move_uses_fresh_ids_resolves_name_collisions_and_restores_source_on_undo() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let (mut source_actor, source, source_inbox) = fixture(first.path(), "http://127.0.0.1:1".parse().unwrap());
    let (mut destination_actor, destination, destination_inbox) = fixture(second.path(), "http://127.0.0.1:1".parse().unwrap());
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    let shelf = src.placement.create_directory(&ROOT_DIR_ID, "Shelf").unwrap().id;
    let hash = seed(&src, shelf, "Novel.epub", EPUB).await;
    let existing = dst.placement.create_directory(&ROOT_DIR_ID, "Shelf").unwrap().id;
    let (from, to) = clients(&source, &destination);
    let exercise = async {
        let outcome = to.transfer_from(&from, selection(vec![shelf], vec![], None), ROOT_DIR_ID, true).await;
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(outcome.removed_folders, vec![(shelf, Some(ROOT_DIR_ID))]);
        let copy = outcome.created_folders[0];
        assert_ne!(copy, shelf);
        assert_ne!(copy, existing);
        assert!(!src.db.is_live_destination(&shelf).unwrap());
        assert_eq!(src.db.library_trash().unwrap().books.len(), 1);
        let folders = dst.db.folder_destinations().unwrap();
        assert_ne!(folders.iter().find(|folder| folder.id == copy).unwrap().label, "Shelf");
        assert!(dst.db.is_live_destination(&existing).unwrap());
        src.run_file_jobs().await.unwrap();
        from.restore_directory(shelf, Some(ROOT_DIR_ID)).await.unwrap();
        to.move_directory_to_trash(copy).await.unwrap();
        src.run_file_jobs().await.unwrap();
        assert!(src.db.is_live_destination(&shelf).unwrap());
        assert!(dst.db.is_live_destination(&existing).unwrap());
        assert!(!dst.db.is_live_destination(&copy).unwrap());
        let path = src.db.book_paths(&hash).unwrap()[0].clone();
        assert_eq!(std::fs::read(first.path().join(path.as_str().trim_start_matches('/'))).unwrap(), EPUB);
        source.shutdown();
        destination.shutdown();
    };
    tokio::join!(source_actor.serve(source_inbox), destination_actor.serve(destination_inbox), exercise);
}
