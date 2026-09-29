use super::*;
use library_database::Database as TestDatabase;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use sync_common::ROOT_DIR_ID;
use tokio::sync::Notify;

const APP_HIDDEN_LIBRARY_DB_PATH: &str = ".bokheim/library.db";

fn fixture() -> (tempfile::TempDir, TestDatabase, AssetStore, ContentHash) {
    let root = tempfile::tempdir().unwrap();
    let assets = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let db = TestDatabase::open(root.path().join(APP_HIDDEN_LIBRARY_DB_PATH)).unwrap();
    let bytes = b"book processed by the injected test worker";
    let hash = ContentHash::new(blake3::hash(bytes).to_hex().as_str());
    db.initialize_library().unwrap();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".to_owned()).unwrap();
    std::fs::create_dir_all(root.path().join("Shelf")).unwrap();
    std::fs::write(root.path().join("Shelf/book.epub"), bytes).unwrap();
    db.commit_import(library_database::ImportCommit {
        parent_id: shelf.id,
        file_name: "book.epub".to_owned(),
        format: book_model::BookFormat::Epub,
        content_hash: hash,
        checksum: hash,
        size_bytes: bytes.len() as u64,
        inspection_version: 1,
        inspection: Some(book_metadata::InspectedBook {
            pdf: None,
            metadata: book_model::BookRecord { title: "Fixture".to_owned(), subtitle: None, contributors: Vec::new(), description: String::new(), book: book_model::BookMetadata::default() },
            audiobook: None,
            toc: Vec::new(),
        }),
        published: library_database::PublishedImport { name: "book.epub".to_owned(), relative_path: "Shelf/book.epub".to_owned(), published: true, fingerprint: None },
        request_thumbnail: true,
        restore_paths: Vec::new(),
    })
    .unwrap();
    let lookup_path = root.path().join(APP_HIDDEN_LIBRARY_DB_PATH);
    let assets = assets.with_book_paths(move |hash| TestDatabase::book_paths_at(&lookup_path, hash).map_err(library_files::AssetStoreError::operation));
    (root, db, assets, hash)
}

#[derive(Default)]
struct Observer {
    active: Arc<AtomicUsize>,
    total: u64,
    completed: usize,
    ready: Vec<ContentHash>,
}
struct Activity(Arc<AtomicUsize>);
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ThumbnailObserver for Observer {
    type Activity = Activity;
    fn prepare(&mut self, pending: u64) {
        self.total = pending;
    }
    fn begin(&mut self) -> Activity {
        self.active.fetch_add(1, Ordering::SeqCst);
        Activity(self.active.clone())
    }
    fn completed(&mut self) {
        assert_eq!(self.active.load(Ordering::SeqCst), 1);
        self.completed += 1;
    }
    fn ready(&mut self, hash: ContentHash) {
        self.ready.push(hash);
    }
}

struct StalledProcessor(Notify);
impl ThumbnailProcessor for StalledProcessor {
    fn decode(&self, _: Vec<u8>) -> BoxedBackendFuture<'_, Result<ThumbnailVersions, BackendError>> {
        unreachable!()
    }
    fn generate(&self, _: String, _: BoxedBookReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailVersions>, BackendError>> {
        Box::pin(async {
            self.0.notify_one();
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn cancellation_drops_batch_activity_without_consuming_the_job() {
    cancellation_releases_activity(false).await;
}

#[tokio::test]
async fn cancellation_drops_explicit_batch_activity_without_consuming_the_job() {
    cancellation_releases_activity(true).await;
}

async fn cancellation_releases_activity(explicit: bool) {
    let (_root, db, assets, hash) = fixture();
    let processor = StalledProcessor(Notify::new());
    let mut observer = Observer::default();
    let active = observer.active.clone();
    let jobs = ThumbnailJobs::new(&db, &assets, &processor);
    let mut run = Box::pin(async {
        if explicit {
            jobs.run_members(vec![hash], &mut observer).await
        } else {
            jobs.run_batch(&mut observer).await
        }
    });
    tokio::select! {
        result = &mut run => panic!("worker unexpectedly finished: {result:?}"),
        _ = processor.0.notified() => {},
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("worker never started"),
    }
    assert_eq!(active.load(Ordering::SeqCst), 1);
    // CPU work must release placement leases before it can stall.
    let lease = tokio::time::timeout(std::time::Duration::from_secs(1), assets.lease_book(&hash)).await.unwrap().unwrap();
    drop(lease);
    drop(run);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(observer.completed, 0);
    assert!(observer.ready.is_empty());
    assert_eq!(db.thumbnail_work_state(&hash).unwrap(), Some(("pending".into(), 0)));
}

struct CoverProcessor;
impl ThumbnailProcessor for CoverProcessor {
    fn decode(&self, _: Vec<u8>) -> BoxedBackendFuture<'_, Result<ThumbnailVersions, BackendError>> {
        Box::pin(async { Ok(ThumbnailVersions { browse: b"browse".to_vec(), high_density: b"master".to_vec() }) })
    }
    fn generate(&self, _: String, _: BoxedBookReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailVersions>, BackendError>> {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn removing_an_mp3_sidecar_cover_clears_its_cached_thumbnail() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("Story");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("01.mp3"), include_bytes!("../../../book-metadata/tests/fixtures/silence.mp3")).unwrap();
    std::fs::write(folder.join("cover.jpg"), b"cover").unwrap();
    let tracks = audiobook_folder::discover(&folder).unwrap();
    let hash = ContentHash::new(audiobook_folder::identity(&tracks).unwrap().to_hex().as_str());
    let archive = audiobook_folder::write_archive_with_sidecars(std::io::Cursor::new(Vec::new()), &tracks, None, None, audiobook_folder::discover_cover(&folder).unwrap().as_ref()).unwrap().into_inner();
    let checksum = ContentHash::new(blake3::hash(&archive).to_hex().as_str());
    let db_path = root.path().join(APP_HIDDEN_LIBRARY_DB_PATH);
    let db = TestDatabase::open(&db_path).unwrap();
    db.initialize_library().unwrap();
    let inspected = book_metadata::inspect_book_bytes("Story.mp3folder", book_model::BookFormat::Mp3Folder, archive.clone()).unwrap();
    db.commit_import(library_database::ImportCommit {
        parent_id: ROOT_DIR_ID, file_name: "Story".into(), format: book_model::BookFormat::Mp3Folder,
        content_hash: hash, checksum, size_bytes: archive.len() as u64, inspection_version: 1,
        inspection: Some(inspected),
        published: library_database::PublishedImport { name: "Story".into(), relative_path: "Story".into(), published: true, fingerprint: None },
        request_thumbnail: true, restore_paths: Vec::new(),
    }).unwrap();
    let assets = AssetStore::open(root.path().to_str().unwrap()).unwrap().with_book_paths(move |hash| TestDatabase::book_paths_at(&db_path, hash).map_err(library_files::AssetStoreError::operation));
    let jobs = ThumbnailJobs::new(&db, &assets, &CoverProcessor);
    let mut observer = Observer::default();
    assert_eq!(db.thumbnail_book_format(hash).unwrap(), Some(book_model::BookFormat::Mp3Folder));
    assert!(jobs.recover(hash, &mut observer).await.unwrap());
    assert!(db.has_sidecar_thumbnail(&hash).unwrap());
    assert!(assets.exists(BlobKind::Thumbnail, &hash).unwrap());

    std::fs::remove_file(folder.join("cover.jpg")).unwrap();
    let archive = audiobook_folder::write_archive(std::io::Cursor::new(Vec::new()), &tracks).unwrap().into_inner();
    db.commit_import(library_database::ImportCommit {
        parent_id: ROOT_DIR_ID, file_name: "Story".into(), format: book_model::BookFormat::Mp3Folder,
        content_hash: hash, checksum: ContentHash::new(blake3::hash(&archive).to_hex().as_str()), size_bytes: archive.len() as u64,
        inspection_version: 1, inspection: Some(book_metadata::inspect_book_bytes("Story.mp3folder", book_model::BookFormat::Mp3Folder, archive.clone()).unwrap()),
        published: library_database::PublishedImport { name: "Story".into(), relative_path: "Story".into(), published: true, fingerprint: None },
        request_thumbnail: true, restore_paths: Vec::new(),
    }).unwrap();
    assert!(jobs.recover(hash, &mut observer).await.unwrap());
    assert!(!db.has_sidecar_thumbnail(&hash).unwrap());
    assert!(!assets.exists(BlobKind::Thumbnail, &hash).unwrap());
}

#[tokio::test]
async fn removing_an_m4b_sidecar_cover_uses_the_local_book_for_recovery() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Story.m4b"), b"audio fixture").unwrap();
    std::fs::write(root.path().join("Story.jpg"), b"cover").unwrap();
    let hash = ContentHash::new(blake3::hash(b"audio fixture").to_hex().as_str());
    let db_path = root.path().join(APP_HIDDEN_LIBRARY_DB_PATH);
    let db = TestDatabase::open(&db_path).unwrap();
    db.initialize_library().unwrap();
    let imported = || library_database::ImportCommit {
        parent_id: ROOT_DIR_ID, file_name: "Story.m4b".into(), format: book_model::BookFormat::M4b,
        content_hash: hash, checksum: hash, size_bytes: b"audio fixture".len() as u64, inspection_version: 1,
        inspection: Some(book_metadata::InspectedBook {
            pdf: None, metadata: book_model::BookRecord { title: "Story".into(), subtitle: None, contributors: Vec::new(), description: String::new(), book: book_model::BookMetadata::default() },
            audiobook: None, toc: Vec::new(),
        }),
        published: library_database::PublishedImport { name: "Story.m4b".into(), relative_path: "Story.m4b".into(), published: true, fingerprint: None },
        request_thumbnail: true, restore_paths: Vec::new(),
    };
    db.commit_import(imported()).unwrap();
    let assets = AssetStore::open(root.path().to_str().unwrap()).unwrap().with_book_paths(move |hash| TestDatabase::book_paths_at(&db_path, hash).map_err(library_files::AssetStoreError::operation));
    let jobs = ThumbnailJobs::new(&db, &assets, &CoverProcessor);
    let mut observer = Observer::default();
    assert!(jobs.recover(hash, &mut observer).await.unwrap());
    assert!(db.has_sidecar_thumbnail(&hash).unwrap());

    std::fs::remove_file(root.path().join("Story.jpg")).unwrap();
    db.commit_import(imported()).unwrap();
    assert!(jobs.recover(hash, &mut observer).await.unwrap());
    assert!(!db.has_sidecar_thumbnail(&hash).unwrap());
    assert!(!assets.exists(BlobKind::Thumbnail, &hash).unwrap());
}

struct FailingProcessor(AtomicUsize);
impl ThumbnailProcessor for FailingProcessor {
    fn decode(&self, _: Vec<u8>) -> BoxedBackendFuture<'_, Result<ThumbnailVersions, BackendError>> {
        unreachable!()
    }
    fn generate(&self, _: String, _: BoxedBookReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailVersions>, BackendError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err("worker failed".into()) })
    }
}

#[tokio::test]
async fn processor_failure_defers_retry_without_reporting_completion() {
    let (_root, db, assets, hash) = fixture();
    let processor = FailingProcessor(AtomicUsize::new(0));
    let jobs = ThumbnailJobs::new(&db, &assets, &processor);
    let mut observer = Observer::default();
    assert!(jobs.run_batch(&mut observer).await.is_err());
    assert_eq!(observer.total, 1);
    assert_eq!(observer.active.load(Ordering::SeqCst), 0);
    assert_eq!(observer.completed, 0);
    assert!(observer.ready.is_empty());
    let state = db.thumbnail_work_state(&hash).unwrap().unwrap();
    assert_eq!(state.0, "pending");
    assert!(state.1 > 0);
    jobs.run_batch(&mut observer).await.unwrap();
    assert_eq!(processor.0.load(Ordering::SeqCst), 1, "a new pass must respect the persisted retry deadline");
    assert!(jobs.run_members(vec![hash], &mut observer).await.is_err());
    assert_eq!(processor.0.load(Ordering::SeqCst), 2, "explicit members retain their independent selection rules");
    assert_eq!(observer.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn empty_explicit_batch_does_not_reset_progress() {
    let (_root, db, assets, _) = fixture();
    let processor = FailingProcessor(AtomicUsize::new(0));
    let jobs = ThumbnailJobs::new(&db, &assets, &processor);
    let mut observer = Observer { total: 42, ..Default::default() };
    jobs.run_members(Vec::new(), &mut observer).await.unwrap();
    assert_eq!(observer.total, 42);
    assert_eq!(processor.0.load(Ordering::SeqCst), 0);
    assert_eq!(observer.active.load(Ordering::SeqCst), 0);
}
