use super::*;
type BookCache = super::BookCache<TestStorage>;

fn source(id: u64, length: usize) -> RemoteFile {
    let hash = test_support::fixture_content_hash(id);
    RemoteFile { hash, checksum: hash, length: length as u64 }
}

async fn settled(cache: &BookCache) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while cache.pending.load(Ordering::Acquire) != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn persistent_cache_reopens_and_downloads_only_missing_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.db");
    let source = source(881, 5 * BLOCK_BYTES + 17);
    let cache = BookCache::open(&path);
    cache.store(source.clone(), 0, vec![1; BLOCK_BYTES]).await.unwrap();
    cache.store(source.clone(), 2 * BLOCK_BYTES as u64, vec![3; BLOCK_BYTES]).await.unwrap();
    drop(cache);
    let cache = BookCache::open(&path);
    let (saved, prefix) = cache.describe(source.hash).await.unwrap();
    assert_eq!(saved.length, source.length);
    assert_eq!(prefix, vec![1; BLOCK_BYTES]);
    let executor = BackendExecutor::new().unwrap();
    let mut requests = Vec::new();
    let bytes = cache
        .read(&executor, &source, 0, 4 * BLOCK_BYTES, |offset, length| {
            requests.push((offset, length));
            std::future::ready(Ok(vec![2; length]))
        })
        .await
        .unwrap();
    assert_eq!(requests, vec![(BLOCK_BYTES as u64, BLOCK_BYTES), (3 * BLOCK_BYTES as u64, BLOCK_BYTES)]);
    assert_eq!(&bytes[2 * BLOCK_BYTES..3 * BLOCK_BYTES], vec![3; BLOCK_BYTES]);
    settled(&cache).await;
    drop(cache);
    let cache = BookCache::open(&path);
    let again = cache.read(&executor, &source, 31, 3 * BLOCK_BYTES, |_, _| async { Err("offline".into()) }).await.unwrap();
    assert_eq!(again, bytes[31..31 + 3 * BLOCK_BYTES]);
    assert!(cache.read(&executor, &source, 4 * BLOCK_BYTES as u64, 1, |_, _| async { Err("offline".into()) }).await.is_err());
}

#[tokio::test]
async fn persistent_cache_evicts_lru_across_books_without_touching_downloads() {
    let dir = tempfile::tempdir().unwrap();
    let download = dir.path().join("offline.epub");
    std::fs::write(&download, b"pinned full download").unwrap();
    let mut cache = BookCache::open(&dir.path().join("cache.db"));
    cache.budget = 2 * BLOCK_BYTES;
    let a = source(882, BLOCK_BYTES);
    let b = source(883, BLOCK_BYTES);
    let c = source(884, BLOCK_BYTES);
    cache.store(a.clone(), 0, vec![1; BLOCK_BYTES]).await.unwrap();
    cache.store(b.clone(), 0, vec![2; BLOCK_BYTES]).await.unwrap();
    assert!(cache.describe(a.hash).await.is_some());
    cache.store(c.clone(), 0, vec![3; BLOCK_BYTES]).await.unwrap();
    assert!(cache.describe(a.hash).await.is_some());
    assert!(cache.describe(b.hash).await.is_none());
    assert!(cache.describe(c.hash).await.is_some());
    let conn = cache.database.as_ref().unwrap().get().unwrap();
    assert_eq!(conn.query_row(include_str!("sql/persistent_cache_evicts_lru_across_books_without_touching_downloads_select.sql"), [], |r| r.get::<_, i64>(0)).unwrap(), cache.budget as i64);
    assert_eq!(conn.query_row(include_str!("sql/persistent_cache_evicts_lru_across_books_without_touching_downloads_select_2.sql"), [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    assert_eq!(conn.query_row(include_str!("sql/persistent_cache_evicts_lru_across_books_without_touching_downloads_pragma.sql"), [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(std::fs::read(download).unwrap(), b"pinned full download");
}

#[tokio::test]
async fn persistent_cache_handles_short_tail_failure_and_unavailable_storage() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BookCache::open(&dir.path().join("cache.db"));
    let source = source(885, BLOCK_BYTES + 17);
    let executor = BackendExecutor::new().unwrap();
    assert!(cache.read(&executor, &source, 0, 1, |_, _| async { Ok(vec![0]) }).await.is_err());
    assert!(cache.describe(source.hash).await.is_none());
    assert!(cache.store(source.clone(), 0, vec![0; 17]).await.is_err());
    let tail = cache
        .read(&executor, &source, BLOCK_BYTES as u64 + 2, 15, |offset, size| async move {
            assert_eq!((offset, size), (BLOCK_BYTES as u64, 17));
            Ok(vec![7; size])
        })
        .await
        .unwrap();
    assert_eq!(tail, vec![7; 15]);
    settled(&cache).await;
    assert_eq!(cache.read(&executor, &source, BLOCK_BYTES as u64, 17, |_, _| async { Err("offline".into()) }).await.unwrap(), vec![7; 17]);
    // A cache database that cannot open must not stop remote reading.
    let broken = BookCache::open(dir.path());
    assert!(broken.database.is_none());
    assert_eq!(broken.read(&executor, &source, 0, 3, |_, size| async move { Ok(vec![9; size]) }).await.unwrap(), vec![9; 3]);
    for (offset, size) in [(u64::MAX, 1), (source.length, 1), (0, MAX_RANGE_BYTES + 1), (0, 0)] {
        assert!(cache.read(&executor, &source, offset, size, |_, _| async { panic!("invalid range reached network") }).await.is_err());
    }
}

#[tokio::test]
async fn persistent_cache_rejects_mixed_sources_and_recovers_truncated_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BookCache::open(&dir.path().join("cache.db"));
    let source = source(886, BLOCK_BYTES);
    cache.store(source.clone(), 0, vec![1; BLOCK_BYTES]).await.unwrap();
    let mut other = source.clone();
    other.checksum = test_support::fixture_content_hash(887);
    assert!(cache.store(other, 0, vec![2; BLOCK_BYTES]).await.is_err());
    cache.database.as_ref().unwrap().get().unwrap().execute(include_str!("sql/persistent_cache_rejects_mixed_sources_and_recovers_truncated_blocks_update.sql"), []).unwrap();
    assert!(cache.describe(source.hash).await.is_none());
    let executor = BackendExecutor::new().unwrap();
    assert_eq!(cache.read(&executor, &source, 0, 2, |_, size| async move { Ok(vec![3; size]) }).await.unwrap(), vec![3; 2]);
    settled(&cache).await;
    assert!(cache.describe(source.hash).await.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_cache_reopens_real_epub_without_network_or_whole_book_read() {
    use std::io::{Cursor, Write};
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in [
        ("META-INF/container.xml", br#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#.as_slice()),
        ("content.opf", br#"<package><metadata/><manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="one"/></spine></package>"#),
        ("one.xhtml", b"<html><body>Streamed chapter</body></html>"),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.start_file("unused.bin", options).unwrap();
    zip.write_all(&vec![7; 4 * 1024 * 1024]).unwrap();
    let bytes = Arc::new(zip.finish().unwrap().into_inner());
    let source = source(888, bytes.len());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.db");
    let executor = BackendExecutor::new().unwrap();
    let fetched = Arc::new(AtomicUsize::new(BLOCK_BYTES));
    for reopen in [false, true] {
        let cache = BookCache::open(&path);
        let prefix = if reopen {
            cache.describe(source.hash).await.expect("opening metadata survived close").1
        } else {
            let prefix = bytes[..BLOCK_BYTES].to_vec();
            cache.remember(&executor, source.clone(), 0, prefix.clone());
            prefix
        };
        let read_cache = cache.clone();
        let source = source.clone();
        let bytes = bytes.clone();
        let executor = executor.clone();
        let fetched = fetched.clone();
        let reader = crate::remote_file::native_reader(source.length, prefix, move |offset, length| {
            let cache = read_cache.clone();
            let executor = executor.clone();
            let source = source.clone();
            let bytes = bytes.clone();
            let fetched = fetched.clone();
            Box::pin(async move {
                cache
                    .read(&executor, &source, offset, length, |offset, length| {
                        assert!(!reopen, "reopening requested network bytes");
                        fetched.fetch_add(length, Ordering::Relaxed);
                        std::future::ready(Ok(bytes[offset as usize..offset as usize + length].to_vec()))
                    })
                    .await
            })
        })
        .unwrap();
        tokio::task::spawn_blocking(move || {
            let epub = epub_provider::EpubProvider::try_from_reader(reader).unwrap();
            assert!(epub.read_string("one.xhtml").unwrap().contains("Streamed chapter"));
        })
        .await
        .unwrap();
        settled(&cache).await;
    }
    assert!(fetched.load(Ordering::Relaxed) < bytes.len() / 4, "first open must remain streamed");
}

#[tokio::test]
async fn cached_bytes_survive_read_only_storage() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    initialize(&conn).unwrap();
    let source = source(950, BLOCK_BYTES);
    conn.execute(include_str!("sql/cached_bytes_survive_read_only_storage_insert.sql"), params![source.hash.as_str(), source.checksum.as_str(), source.length as i64]).unwrap();
    conn.execute(include_str!("sql/cached_bytes_survive_read_only_storage_insert_2.sql"), params![source.hash.as_str(), vec![7_u8; BLOCK_BYTES]]).unwrap();
    conn.execute_batch(include_str!("sql/cached_bytes_survive_read_only_storage_pragma.sql")).unwrap();
    assert_eq!(read_blocks(&mut conn, &source, 0, BLOCK_BYTES).unwrap(), vec![Some(vec![7; BLOCK_BYTES])]);
}

#[tokio::test]
async fn activating_a_pdf_revision_discards_old_blocks_and_rejects_late_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.db");
    let cache = BookCache::open(&path);
    let old = source(951, 2 * BLOCK_BYTES);
    cache.store(old.clone(), 0, vec![1; BLOCK_BYTES]).await.unwrap();
    cache.store(old.clone(), BLOCK_BYTES as u64, vec![2; BLOCK_BYTES]).await.unwrap();
    let mut new = old.clone();
    new.checksum = test_support::fixture_content_hash(952);
    cache.activate(new.clone()).await;
    cache.store(new.clone(), 0, vec![3; BLOCK_BYTES]).await.unwrap();
    assert!(cache.store(old.clone(), BLOCK_BYTES as u64, vec![4; BLOCK_BYTES]).await.is_err());
    drop(cache);
    let cache = BookCache::open(&path);
    let (saved, prefix) = cache.describe(new.hash).await.unwrap();
    assert_eq!(saved.checksum, new.checksum);
    assert_eq!(prefix, vec![3; BLOCK_BYTES]);
    let executor = BackendExecutor::new().unwrap();
    assert!(cache.read(&executor, &new, BLOCK_BYTES as u64, 8, |_, _| async { Err("offline".into()) }).await.is_err());
    assert!(cache.read(&executor, &old, 0, 8, |_, _| async { Err("offline".into()) }).await.is_err());
}

#[tokio::test]
async fn persistent_bundle_batches_holes_and_reopens_offline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.db");
    let source = source(990, 64 * BLOCK_BYTES + 17);
    let cache = BookCache::open(&path);
    let executor = BackendExecutor::new().unwrap();
    cache.store(source.clone(), 2 * BLOCK_BYTES as u64, vec![3; BLOCK_BYTES]).await.unwrap();
    let mut requests = Vec::new();
    let ranges = vec![(0, 64 * BLOCK_BYTES)];
    let bytes = cache
        .read_bundle(&executor, &source, ranges.clone(), |ranges| {
            requests.push(ranges.clone());
            std::future::ready(Ok(vec![7; ranges.iter().map(|(_, length)| length).sum()]))
        })
        .await
        .unwrap();
    assert_eq!(requests, vec![vec![(0, 2 * BLOCK_BYTES), (3 * BLOCK_BYTES as u64, 61 * BLOCK_BYTES)]]);
    assert_eq!(&bytes[2 * BLOCK_BYTES..3 * BLOCK_BYTES], vec![3; BLOCK_BYTES]);
    settled(&cache).await;
    drop(cache);
    let cache = BookCache::open(&path);
    assert_eq!(cache.read_bundle(&executor, &source, ranges, |_| async { Err("offline".into()) }).await.unwrap(), bytes);
    let mut changed = source.clone();
    changed.checksum = test_support::fixture_content_hash(991);
    assert!(cache.read_bundle(&executor, &changed, vec![(0, BLOCK_BYTES)], |_| async { Err("revision changed".into()) }).await.is_err());
}

#[derive(Clone, Debug)]
struct TestStorage(Arc<std::sync::Mutex<rusqlite::Connection>>);
impl TestStorage {
    fn get(&self) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>, DatabaseError> {
        self.0.lock().map_err(DatabaseError::operation)
    }
}
impl CacheStorage for TestStorage {
    fn open(path: &Path, initialize: fn(&rusqlite::Connection) -> rusqlite::Result<()>) -> Result<Self, DatabaseError> {
        let conn = rusqlite::Connection::open(path)?;
        initialize(&conn)?;
        Ok(Self(Arc::new(std::sync::Mutex::new(conn))))
    }
    fn command<T: super::super::storage::CacheSend + 'static>(
        &self, _priority: Priority, operation: impl FnOnce(&mut rusqlite::Connection) -> Result<T, DatabaseError> + super::super::storage::CacheSend + 'static,
    ) -> crate::FetchFuture<'static, Result<T, DatabaseError>> {
        let storage = self.clone();
        Box::pin(async move { operation(&mut *storage.get()?) })
    }
}
use client_platform_runtime::executor::BackendExecutor;
