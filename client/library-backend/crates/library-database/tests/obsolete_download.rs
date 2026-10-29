use library_database::{Database, NativeBookAcknowledgement, NativeBookCompletion, NativeBookSnapshot};
use sync_common::{BookId, ROOT_DIR_ID};

#[test]
fn late_download_requeues_removal_after_purge_work_has_finished() {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();
    let hash = BookId::new(&"a".repeat(64));
    // The book and its original purge jobs are already gone when the download finishes.
    let mut completion = NativeBookCompletion {
        snapshot: NativeBookSnapshot {
            content_hash: hash,
            directory: ROOT_DIR_ID,
            name: "book.epub".into(),
            relative_path: "/book.epub".into(),
            occupied_names: Vec::new(),
            format: book_model::BookFormat::Epub,
            audiobook_track_titles: Vec::new(),
        },
        publication: library_model::PublishedStagedBook { name: "book (1).epub".into(), relative_path: "/book (1).epub".into(), published: false, fingerprint: Some("verified".into()) },
        size_bytes: 123,
        provider_file: None,
    };
    assert_eq!(db.complete_downloaded_book(completion.clone(), true).unwrap(), NativeBookAcknowledgement::Obsolete);
    assert!(db.native_file_work().unwrap().is_empty());

    completion.publication.published = true;
    assert_eq!(db.complete_downloaded_book(completion.clone(), true).unwrap(), NativeBookAcknowledgement::Obsolete);
    let jobs = db.native_file_work().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].operation, "remove");
    assert_eq!(jobs[0].content_hash, hash);
    assert_eq!(jobs[0].original_path, completion.publication.relative_path);

    db.complete_downloaded_book(completion, true).unwrap();
    assert_eq!(db.native_file_work().unwrap().len(), 1, "retrying completion must not duplicate deletion work");
}
