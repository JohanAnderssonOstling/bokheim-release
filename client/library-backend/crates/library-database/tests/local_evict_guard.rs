use library_database::{Database, ImportCommit};
use sync_common::{ContentHash, ROOT_DIR_ID};

fn database() -> Database {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();
    db
}

fn import() -> ImportCommit {
    let hash = ContentHash::new(&"9".repeat(64));
    ImportCommit {
        parent_id: ROOT_DIR_ID,
        file_name: "book.epub".into(),
        format: book_model::BookFormat::Epub,
        content_hash: hash,
        checksum: hash,
        size_bytes: 10,
        inspection_version: 1,
        inspection: Some(book_metadata::InspectedBook {
            pdf: None,
            audiobook: None,
            metadata: book_model::BookRecord {
                title: "Original".into(),
                subtitle: None,
                contributors: vec![book_model::Contributor::new("Original Author", book_model::MarcRelatorCode(*b"aut")).unwrap()],
                description: String::new(),
                book: Default::default(),
            },
            toc: vec![book_model::BookTocEntry { title: "Chapter".into(), target: "ch1".into(), children: vec![] }],
        }),
        published: library_database::PublishedImport { name: "book.epub".into(), relative_path: "/book.epub".into(), published: false, fingerprint: None },
        request_thumbnail: false,
        restore_paths: vec![],
    }
}

/// A library holding one downloaded book, through the same calls production
/// uses to request and complete a download.
fn downloaded_book() -> (Database, ContentHash) {
    let db = database();
    let commit = import();
    let hash = commit.content_hash;
    db.commit_import(commit).unwrap();
    db.sync_request_book_downloads(&[hash], library_replica::TransferOrigin::UserInitiated).unwrap();
    db.complete_local_book_request(hash).unwrap();
    assert!(db.is_book_downloaded(&hash).unwrap(), "setup must leave the book downloaded");
    (db, hash)
}

#[test]
fn evicting_a_downloaded_book_without_a_server_copy_is_refused() {
    let (db, hash) = downloaded_book();
    let error = db.evict_local_book(&hash).unwrap_err();
    assert!(error.to_string().contains("only stored on this device"), "unexpected error: {error}");
    assert!(db.is_book_downloaded(&hash).unwrap(), "a refused eviction must keep the download");
}

#[test]
fn evicting_a_downloaded_book_with_a_server_copy_keeps_the_entry() {
    let (db, hash) = downloaded_book();
    db.record_remote_asset(library_replica::BlobKind::Book, &hash).unwrap();
    assert!(db.evict_local_book(&hash).unwrap(), "a server-backed download must evict");
    assert!(!db.is_book_downloaded(&hash).unwrap(), "eviction must clear the download");
    assert!(!db.book_paths(&hash).unwrap().is_empty(), "eviction must keep the book's placements");
    assert!(!db.evict_local_book(&hash).unwrap(), "evicting a non-download is a no-op");
}
