use super::*;
use crate::library::handle::concurrency_tests::fixture;
use crate::DownloadState;
use library_model::BlobKind;
use sync_common::ROOT_DIR_ID;

const EPUB: &[u8] = include_bytes!("../../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
const OTHER_EPUB: &[u8] = include_bytes!("../../../crates/book-inspection/tests/fixtures/malformed-language.epub");

fn query(directory: DirId) -> LibraryBrowseQuery {
    LibraryBrowseQuery { location: BrowseLocation::Folder(directory), search: String::new(), file_types: Vec::new(), chip_sort: Default::default(), book_sort: Default::default(), languages: Vec::new(), hide_finished: false }
}

async fn nested_books(session: &LibrarySession) -> (DirId, BookId, BookId) {
    let shelf = session.placement.create_directory(&ROOT_DIR_ID, "Shelf").unwrap().id;
    let nested = session.placement.create_directory(&shelf, "Nested").unwrap().id;
    let keep = Box::pin(session.import.import_downloaded_book(nested, "Keep.epub", EPUB.to_vec())).await.unwrap();
    let skip = Box::pin(session.import.import_downloaded_book(nested, "Skip.epub", OTHER_EPUB.to_vec())).await.unwrap();
    session.placement.copy_book_to_directory(&keep, Some(&nested), &shelf).await.unwrap();
    (shelf, keep, skip)
}

#[tokio::test]
async fn download_browse_preserves_filters_across_nested_placements_and_keeps_existing_requests() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let session = &actor.session;
    let (shelf, keep, skip) = nested_books(session).await;
    for hash in [keep, skip] {
        session.db.record_remote_asset(BlobKind::Book, &hash).unwrap();
        session.remove_local_book_copy(hash).unwrap();
    }
    let mut query = query(shelf);
    query.search = session.db.browse_book_detail(&keep).unwrap().book.title;
    session.queue_browse_downloads(query.clone()).unwrap();
    assert!(matches!(session.persistent_download_state(keep).unwrap(), DownloadState::Queued));
    assert!(matches!(session.persistent_download_state(skip).unwrap(), DownloadState::NotDownloaded));
    let queued = session.db.book_download_requests(-1, None, 10).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].0, keep);
    session.queue_browse_downloads(query).unwrap();
    assert_eq!(session.db.book_download_requests(-1, None, 10).unwrap(), queued);
}

#[tokio::test]
async fn folder_eviction_counts_unique_books_and_keeps_device_only_bytes_and_reader_state() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let session = &actor.session;
    let (shelf, remote, local) = nested_books(session).await;
    session.db.record_remote_asset(BlobKind::Book, &remote).unwrap();
    session.db.update_reading_position(&remote, "epubcfi(/6/2)", Some(42.0), None).unwrap();
    let reading = session.db.reading_position(&remote).unwrap();
    assert_eq!(session.evict_folder_downloads(&shelf).unwrap(), 1);
    assert!(!session.assets.book_exists(&remote, &session.db.book_paths(&remote).unwrap()).unwrap());
    assert!(session.assets.book_exists(&local, &session.db.book_paths(&local).unwrap()).unwrap());
    assert_eq!(session.db.reading_position(&remote).unwrap(), reading);
    assert!(session.db.book_format(&remote).unwrap().is_some());
    assert_eq!(session.evict_folder_downloads(&shelf).unwrap(), 0);
}
