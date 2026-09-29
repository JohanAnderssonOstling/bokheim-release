//! Book access, transfer staging, and thumbnail storage.
//!
//! Native books live directly in library folders, with adjacent temporary
//! files for imports and downloads. Browser books have one hash-addressed
//! object in OPFS; their folder and trash locations exist only in SQLite.

use library_replica::RelativeBookPath;
pub type ImportProgressObserver = std::sync::Arc<dyn Fn(library_model::ImportFileStage) + Send + Sync>;
use book_access::BoxedBookReader;
use library_replica::BlobKind;
use std::fmt;
use sync_common::{ContentHash, DirId};

mod import;
pub use import::PreparedImport;

#[derive(Debug)]
pub struct AssetStoreError(std::sync::Arc<dyn std::error::Error + Send + Sync>);

impl AssetStoreError {
    pub fn operation(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self(std::sync::Arc::from(error.into()))
    }
}

impl fmt::Display for AssetStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for AssetStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

impl From<std::io::Error> for AssetStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::operation(error)
    }
}

impl From<library_database::DatabaseError> for AssetStoreError {
    fn from(error: library_database::DatabaseError) -> Self {
        Self::operation(error)
    }
}

#[derive(Clone, Debug)]
pub struct AssetStore(imp::Handle);

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub(super) struct BookPathLookup(std::sync::Arc<dyn Fn(&ContentHash) -> Result<Vec<String>, AssetStoreError> + Send + Sync>);

#[cfg(not(target_arch = "wasm32"))]
impl BookPathLookup {
    fn paths(&self, hash: &ContentHash) -> Result<Vec<String>, AssetStoreError> {
        (self.0)(hash)
    }
}

impl AssetStore {
    /// Returns an explicit sibling cover for a native M4B placement.
    pub async fn m4b_sidecar_cover(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>, AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let handle = self.0.clone();
            let hash = *hash;
            client_platform_runtime::executor::run_blocking(move || imp::m4b_sidecar_cover(&handle, &hash)).await.map_err(AssetStoreError::operation)?
        }
        #[cfg(target_arch = "wasm32")]
        { let _ = hash; Ok(None) }
    }

    /// Read a scanned MP3 directory as the same transfer archive used for sync.
    /// Native scans may have no cached book blob yet.
    pub async fn local_mp3_folder_archive(&self, hash: &ContentHash) -> Result<Option<BoxedBookReader>, AssetStoreError> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = hash;
            Ok(None)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let handle = self.0.clone();
            let hash = *hash;
            client_platform_runtime::executor::run_blocking(move || imp::local_mp3_folder_archive(&handle, &hash))
                .await
                .map_err(AssetStoreError::operation)?
        }
    }

    pub async fn local_m4b_reader(&self, hash: &ContentHash) -> Result<Option<BoxedBookReader>, AssetStoreError> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = hash;
            Ok(None)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let handle = self.0.clone();
            let hash = *hash;
            client_platform_runtime::executor::run_blocking(move || imp::local_m4b_reader(&handle, &hash))
                .await
                .map_err(AssetStoreError::operation)?
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn unavailable(locator: &str) -> Self {
        Self(imp::unavailable(locator))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn suspend_local_access(&self) {
        imp::suspend(&self.0);
    }
    pub fn local_access_enabled(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            imp::enabled(&self.0)
        }
        #[cfg(target_arch = "wasm32")]
        {
            true
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn local_book_bytes(&self) -> Result<u64, AssetStoreError> {
        imp::book_bytes_at_handle(&self.0).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_book_paths(self, lookup: impl Fn(&ContentHash) -> Result<Vec<String>, AssetStoreError> + Send + Sync + 'static) -> Self {
        Self(imp::with_book_paths(self.0, BookPathLookup(std::sync::Arc::new(lookup))))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(library_locator: &str) -> Result<Self, AssetStoreError> {
        imp::open(library_locator).map(Self)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn placement(&self) -> NativePlacement<'_> {
        native_placement::NativePlacement { store: self }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn open(library_locator: &str, hash_service: std::sync::Arc<dyn crate::hashing::HashService>) -> Result<Self, AssetStoreError> {
        imp::open(library_locator, hash_service).map(Self)
    }

    pub fn ensure_restore_available(&self) -> Result<(), AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        if !self.placement().root_available() {
            return Err(AssetStoreError::operation("library storage is unavailable"));
        }
        Ok(())
    }

    /// Each browser command releases SQLite and yields before the next page.
    pub async fn run_file_jobs(&self, database: &library_database::Database) -> Result<(), AssetStoreError> {
        let mut failure = None;
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(error) = self.placement().run_jobs(database).await {
            failure = Some(error);
        }
        let mut after = String::new();
        loop {
            let jobs = database.requested_purge_books(&after).map_err(|error| AssetStoreError::operation(error))?;
            let Some(last) = jobs.last() else { break };
            after = last.to_string();
            for hash in jobs {
                let result = self.purge_requested_book(database, hash);
                if let Err(error) = result {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Replays queued directory moves/renames immediately rather than
    /// waiting for the next scheduled pass.
    pub async fn run_directory_work(&self, database: &library_database::Database) -> Result<(), AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        return self.placement().run_directory_work(database).await;
        #[cfg(target_arch = "wasm32")]
        {
            let _ = database;
            Ok(())
        }
    }

    /// Same as [`Self::run_file_jobs`], scoped to one book's queued work.
    pub async fn run_file_jobs_for(&self, database: &library_database::Database, hash: &ContentHash) -> Result<(), AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        return self.placement().reconcile_file_work_for(database, hash).await;
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (database, hash);
            Ok(())
        }
    }

    pub async fn verified_book_reader(&self, hash: ContentHash) -> Result<Option<VerifiedBook>, AssetStoreError> {
        self.verified_book_version_reader(hash, None).await
    }

    /// Verifies one of the database-selected native placements.  Unlike the
    /// legacy hash reader, this never discovers paths by opening SQLite.
    pub async fn verified_book_version_reader_at_paths(&self, hash: ContentHash, expected: Option<ContentHash>, paths: Vec<String>) -> Result<Option<VerifiedBook>, AssetStoreError> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = paths;
            self.verified_book_version_reader(hash, expected).await
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let store = self.clone();
            client_platform_runtime::executor::run_blocking(move || {
                use std::io::{Read as _, Seek as _};
                let lease = imp::lease(&store.0, hash.as_str(), true)?.ok_or_else(|| AssetStoreError::operation("book is in use; retry verification"))?;
                for relative_path in paths {
                    let path = imp::placement_path(&store.0, &relative_path)?;
                    let (mut reader, folder_identity): (BoxedBookReader, Option<ContentHash>) = if path.is_dir() {
                        let tracks = audiobook_folder::discover(&path)?;
                        if tracks.is_empty() { continue; }
                        let identity = ContentHash::new(audiobook_folder::identity(&tracks)?.to_hex().as_str());
                        if identity != hash { continue; }
                        let nfo = audiobook_folder::discover_nfo(&path)?;
                        let cue = audiobook_folder::discover_cue(&path)?;
                        let cover = audiobook_folder::discover_cover(&path)?;
                        let mut archive = audiobook_folder::write_archive_with_sidecars(tempfile::tempfile()?, &tracks, nfo.as_ref(), cue.as_ref(), cover.as_ref())?;
                        archive.rewind()?;
                        (Box::new(archive), Some(identity))
                    } else {
                        let Some(reader) = imp::open_book_at_path(&store.0, &relative_path)? else { continue };
                        (reader, None)
                    };
                    let mut hasher = blake3::Hasher::new();
                    let mut buffer = [0_u8; 64 * 1024];
                    loop {
                        let read = reader.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]);
                    }
                    let checksum = ContentHash::new(hasher.finalize().to_hex().as_str());
                    if expected.is_some_and(|expected| expected != checksum) {
                        continue;
                    }
                    reader.seek(std::io::SeekFrom::Start(0))?;
                    let actual_identity = match folder_identity {
                        Some(identity) => {
                            let tracks = audiobook_folder::discover(&path)?;
                            if ContentHash::new(audiobook_folder::identity(&tracks)?.to_hex().as_str()) != identity { continue; }
                            identity
                        }
                        None => book_identity::read(&mut reader)?.unwrap_or(checksum),
                    };
                    if actual_identity != hash {
                        continue;
                    }
                    reader.seek(std::io::SeekFrom::Start(0))?;
                    return Ok(Some(VerifiedBook { reader, length: hasher.count(), checksum, _lease: BookLease(lease) }));
                }
                Ok(None)
            })
            .await
            .map_err(|error| AssetStoreError::operation(error))?
        }
    }

    pub async fn verified_book_version_reader(&self, hash: ContentHash, expected: Option<ContentHash>) -> Result<Option<VerifiedBook>, AssetStoreError> {
        #[cfg(target_arch = "wasm32")]
        {
            use std::io::{Read as _, Seek as _};
            let lease = imp::lease(&self.0, hash.as_str(), true)?.ok_or_else(|| AssetStoreError::operation("book is in use; retry verification"))?;
            let Some(mut reader) = imp::open_verified_reader(&self.0, &hash).await? else { return Ok(None) };
            let mut hasher = self.0.hash_service.start().await.map_err(AssetStoreError::operation)?;
            let mut length = 0_u64;
            let mut buffer = vec![0_u8; 256 * 1024];
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]).await.map_err(AssetStoreError::operation)?;
                length += read as u64;
            }
            let checksum: ContentHash = hasher.finish().await.map_err(AssetStoreError::operation)?.parse().map_err(|error| AssetStoreError::operation(format!("invalid checksum: {error}")))?;
            if expected.is_some_and(|expected| expected != checksum) {
                return Ok(None);
            }
            reader.seek(std::io::SeekFrom::Start(0))?;
            if book_identity::read(&mut reader)?.unwrap_or(checksum) != hash {
                drop(reader);
                self.remove(BlobKind::Book, &hash)?;
                return Ok(None);
            }
            reader.seek(std::io::SeekFrom::Start(0))?;
            Ok(Some(VerifiedBook { reader, length, checksum, _lease: BookLease(lease) }))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let store = self.clone();
            client_platform_runtime::executor::run_blocking(move || {
                use std::io::{Read as _, Seek as _};
                let lease = imp::lease(&store.0, hash.as_str(), true)?.ok_or_else(|| AssetStoreError::operation("book is in use; retry verification"))?;
                for path in imp::readable_book_paths(&store.0, &hash)? {
                    let mut reader: BoxedBookReader = match std::fs::File::open(&path) {
                        Ok(file) => Box::new(file),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    };
                    let mut hasher = blake3::Hasher::new();
                    let mut buffer = [0_u8; 64 * 1024];
                    loop {
                        let read = reader.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]);
                    }
                    let checksum = ContentHash::new(hasher.finalize().to_hex().as_str());
                    if expected.is_some_and(|expected| expected != checksum) {
                        continue;
                    }
                    reader.seek(std::io::SeekFrom::Start(0))?;
                    if book_identity::read(&mut reader)?.unwrap_or(checksum) != hash {
                        continue;
                    }
                    reader.seek(std::io::SeekFrom::Start(0))?;
                    return Ok(Some(VerifiedBook { reader, length: hasher.count(), checksum, _lease: BookLease(lease) }));
                }
                Ok(None)
            })
            .await
            .map_err(|error| AssetStoreError::operation(error))?
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn read(&self, kind: BlobKind, content_hash: &ContentHash) -> Result<Option<Vec<u8>>, AssetStoreError> {
        imp::read(&self.0, asset_name(kind, content_hash))
    }

    pub async fn read_bytes(&self, kind: BlobKind, hash: &ContentHash) -> Result<Option<Vec<u8>>, AssetStoreError> {
        self.read_named(asset_name(kind, hash)).await
    }

    async fn read_named(&self, name: String) -> Result<Option<Vec<u8>>, AssetStoreError> {
        #[cfg(target_arch = "wasm32")]
        {
            imp::read(&self.0, name).await
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let store = self.clone();
            client_platform_runtime::executor::run_blocking(move || imp::read(&store.0, name)).await.map_err(|error| AssetStoreError::operation(error))?
        }
    }

    #[cfg(target_os = "android")]
    pub fn open_playback_file(&self, content_hash: &ContentHash) -> Result<Option<std::fs::File>, AssetStoreError> {
        imp::open_playback_file(&self.0, asset_name(BlobKind::Book, content_hash))
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn playback_reader(&self, hash: &ContentHash) -> Result<Option<client_platform_web::web_storage::FileReader>, AssetStoreError> {
        imp::open_verified_reader(&self.0, hash).await
    }

    /// Opens one explicitly selected native book.  The caller owns
    /// selecting that placement from the library inventory; this layer only
    /// validates and opens the corresponding filesystem path.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn prepare_book_at_path(&self, relative_path: String) -> Result<Option<BoxedBookReader>, AssetStoreError> {
        let store = self.clone();
        client_platform_runtime::executor::run_blocking(move || imp::open_book_at_path(&store.0, &relative_path)).await.map_err(|error| AssetStoreError::operation(error))?
    }

    /// Opens an explicitly selected PDF and returns the filesystem fingerprint
    /// used by the database owner to select still-valid cached metadata.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn prepare_pdf_at_path(&self, relative_path: String) -> Result<Option<(BoxedBookReader, String)>, AssetStoreError> {
        let store = self.clone();
        client_platform_runtime::executor::run_blocking(move || imp::open_pdf_at_path(&store.0, &relative_path)).await.map_err(|error| AssetStoreError::operation(error))?
    }

    pub async fn prepare_reader(&self, kind: BlobKind, content_hash: &ContentHash) -> Result<Option<BoxedBookReader>, AssetStoreError> {
        #[cfg(target_arch = "wasm32")]
        {
            imp::open_reader(&self.0, asset_name(kind, content_hash)).await
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let store = self.clone();
            let hash = *content_hash;
            client_platform_runtime::executor::run_blocking(move || store.open_reader(kind, &hash)).await.map_err(|error| AssetStoreError::operation(error))?
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_reader(&self, kind: BlobKind, content_hash: &ContentHash) -> Result<Option<BoxedBookReader>, AssetStoreError> {
        imp::open_reader(&self.0, asset_name(kind, content_hash))
    }

    pub async fn write(&self, kind: BlobKind, content_hash: &ContentHash, bytes: &[u8]) -> Result<(), AssetStoreError> {
        imp::write(&self.0, asset_name(kind, content_hash), bytes).await?;
        if kind == BlobKind::Thumbnail {
            imp::remove(&self.0, thumbnail_variant_name(content_hash, thumbnail::BROWSE_THUMBNAIL_WIDTH))?;
        }
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn begin_stream_write(&self, kind: BlobKind, content_hash: &ContentHash) -> Result<AsyncAssetWriter, AssetStoreError> {
        #[cfg(target_arch = "wasm32")]
        let hasher = Some(self.0.hash_service.start().await.map_err(AssetStoreError::operation)?);
        imp::begin_stream_write(&self.0, asset_name(kind, content_hash)).await.map(|inner| AsyncAssetWriter {
            inner,
            expected_hash: *content_hash,
            transfer_checksum: *content_hash,
            #[cfg(not(target_arch = "wasm32"))]
            hasher: blake3::Hasher::new(),
            #[cfg(target_arch = "wasm32")]
            hasher,
            #[cfg(target_arch = "wasm32")]
            digest: None,
            store: self.clone(),
            written: 0,
            next_storage_check: 0,
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn stage_book(&self, relative: String, source: Box<dyn std::io::Read + Send>) -> Result<StagedBook, AssetStoreError> {
        self.stage_book_with_progress(relative, source, None).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn stage_book_with_progress(&self, relative: String, mut source: Box<dyn std::io::Read + Send>, progress: Option<ImportProgressObserver>) -> Result<StagedBook, AssetStoreError> {
        let store = self.clone();
        client_platform_runtime::executor::run_blocking(move || {
            let parent = relative.rsplit_once('/').map(|(parent, _)| parent).unwrap_or_default();
            let directory = imp::placement_path(&store.0, parent)?;
            imp::stage_book_in(&directory, source.as_mut(), progress.as_ref()).map(|inner| StagedBook { inner })
        })
        .await
        .map_err(|error| AssetStoreError::operation(error))?
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn copy_book(&self, relative: String, source: Box<dyn std::io::Read + Send>, occupied: Vec<String>) -> Result<(String, StagedBook), AssetStoreError> {
        self.copy_book_with_progress(relative, source, occupied, None).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn copy_book_with_progress(&self, relative: String, mut source: Box<dyn std::io::Read + Send>, occupied: Vec<String>, progress: Option<ImportProgressObserver>) -> Result<(String, StagedBook), AssetStoreError> {
        let store = self.clone();
        client_platform_runtime::executor::run_blocking(move || imp::copy_book_to_destination(&store.0, &relative, source.as_mut(), occupied, progress.as_ref()).map(|(name, inner)| (name, StagedBook { inner })))
            .await
            .map_err(|error| AssetStoreError::operation(error))?
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn begin_book_write(&self, relative: &str, hash: &ContentHash) -> Result<AsyncAssetWriter, AssetStoreError> {
        let relative = RelativeBookPath::parse(relative).map_err(|error| AssetStoreError::operation(error))?;
        let (parent, name) = relative.as_str().rsplit_once('/').unwrap();
        let destination = imp::placement_path(&self.0, parent)?.join(name);
        let inner = imp::begin_write_at(destination).await?;
        Ok(AsyncAssetWriter { inner, expected_hash: *hash, transfer_checksum: *hash, hasher: blake3::Hasher::new(), store: self.clone(), written: 0, next_storage_check: 0 })
    }

    #[cfg(target_arch = "wasm32")]
    pub fn finish_staged_write(&self, writer: client_platform_web::web_storage::FileWriter, content_hash: String) -> Result<StagedBook, AssetStoreError> {
        Ok(StagedBook { inner: imp::finish_staged_write(&self.0, writer, content_hash)? })
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn stage_reader(&self, mut source: Box<dyn std::io::Read + Send>) -> Result<StagedBook, AssetStoreError> {
        imp::stage_book(&self.0, source.as_mut()).await.map(|inner| StagedBook { inner })
    }

    /// Capacity is an adapter capability; unknown capacity relies on I/O quota errors.
    pub fn check_write_capacity(&self, additional_bytes: u64) -> Result<(), AssetStoreError> {
        if let Some(available) = imp::write_capacity(&self.0)? {
            if additional_bytes > available {
                return Err(AssetStoreError::operation("not enough free storage to write book".to_owned()));
            }
        }
        Ok(())
    }

    pub async fn read_thumbnail_variant(&self, content_hash: &ContentHash, width: u32) -> Result<Option<Vec<u8>>, AssetStoreError> {
        self.read_named(thumbnail_variant_name(content_hash, width)).await
    }

    pub async fn write_thumbnail_variant(&self, content_hash: &ContentHash, width: u32, bytes: &[u8]) -> Result<(), AssetStoreError> {
        imp::write(&self.0, thumbnail_variant_name(content_hash, width), bytes).await
    }

    pub fn thumbnail_variant_exists(&self, content_hash: &ContentHash, width: u32) -> Result<bool, AssetStoreError> {
        imp::exists(&self.0, thumbnail_variant_name(content_hash, width))
    }

    pub fn thumbnail_set_exists(&self, content_hash: &ContentHash) -> Result<bool, AssetStoreError> {
        Ok(self.exists(BlobKind::Thumbnail, content_hash)? && self.thumbnail_variant_exists(content_hash, thumbnail::BROWSE_THUMBNAIL_WIDTH)?)
    }

    pub fn remove(&self, kind: BlobKind, content_hash: &ContentHash) -> Result<bool, AssetStoreError> {
        let removed = imp::remove(&self.0, asset_name(kind, content_hash))?;
        if kind == BlobKind::Thumbnail {
            imp::remove(&self.0, thumbnail_variant_name(content_hash, thumbnail::BROWSE_THUMBNAIL_WIDTH))?;
        }
        Ok(removed)
    }

    pub fn exists(&self, kind: BlobKind, content_hash: &ContentHash) -> Result<bool, AssetStoreError> {
        imp::exists(&self.0, asset_name(kind, content_hash))
    }

    /// Whether a library-relative placement path is a readable file. Lets the
    /// import orchestrator resolve restore work from a database snapshot
    /// without holding the writer.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn placement_file_exists(&self, relative: &str) -> bool {
        imp::placement_path(&self.0, relative).is_ok_and(|path| path.is_file() || path.is_dir() && audiobook_folder::discover(&path).is_ok_and(|tracks| !tracks.is_empty()))
    }

    /// Best-effort filesystem ensure of a placement's parent directory, with
    /// no database involvement. Directory identity healing stays with the
    /// directory passes; this only keeps a download from failing on a
    /// missing folder that its snapshot still considers live.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn ensure_placement_parent(&self, relative: &str) -> Result<(), AssetStoreError> {
        let (parent, _) = relative.rsplit_once('/').unwrap_or(("", relative));
        let path = imp::placement_path(&self.0, parent)?;
        std::fs::create_dir_all(path)?;
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn evict_book_files(&self, relative_paths: &[String], hash: &ContentHash) -> Result<(), AssetStoreError> {
        imp::evict_book_files(&self.0, relative_paths, hash)
    }

    /// Hold across book and the referencing metadata commit. Cleanup
    /// skips leased hashes, including leases held by another native process.
    pub async fn lease_book(&self, hash: &ContentHash) -> Result<BookLease, AssetStoreError> {
        loop {
            if let Some(lease) = imp::lease(&self.0, hash.as_str(), false)? {
                return Ok(BookLease(lease));
            }
            client_platform_runtime::executor::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn lease_book_blocking(&self, hash: &ContentHash) -> Result<BookLease, AssetStoreError> {
        loop {
            if let Some(lease) = imp::lease(&self.0, hash.as_str(), false)? {
                return Ok(BookLease(lease));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    fn purge_requested_book(&self, database: &library_database::Database, hash: ContentHash) -> Result<(), AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        if !self.placement().root_available() {
            return Err(AssetStoreError::operation("library storage is unavailable"));
        }
        let Some(_exclusive) = imp::lease(&self.0, hash.as_str(), true)? else { return Ok(()) };
        let Some(snapshot) = database.purge_book_snapshot(&hash).map_err(|error| AssetStoreError::operation(error))? else { return Ok(()) };
        if snapshot.remove_bytes {
            self.remove(BlobKind::Book, &hash)?;
            self.remove(BlobKind::Thumbnail, &hash)?;
            #[cfg(not(target_arch = "wasm32"))]
            self.placement().purge_trash(&hash)?;
        }
        database.acknowledge_purge_book(snapshot).map_err(|error| AssetStoreError::operation(error))?;
        Ok(())
    }
}

impl AssetStore {
    /// Count already-resolved native book paths. Resolving placements is
    /// database policy and deliberately belongs to the caller.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn book_bytes_at_paths(&self, relative_paths: Vec<String>) -> Result<u64, AssetStoreError> {
        let store = self.clone();
        client_platform_runtime::executor::run_blocking(move || {
            let mut total = 0_u64;
            let mut counted = std::collections::HashSet::new();
            for relative in relative_paths {
                let path = imp::placement_path(&store.0, &relative)?;
                if counted.insert(path.clone()) {
                    match std::fs::metadata(path) {
                        Ok(metadata) if metadata.is_file() => total = total.saturating_add(metadata.len()),
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            Ok(total)
        })
        .await
        .map_err(|error| AssetStoreError::operation(error))?
    }
}

pub struct VerifiedBook {
    #[cfg(not(target_arch = "wasm32"))]
    reader: BoxedBookReader,
    #[cfg(target_arch = "wasm32")]
    reader: client_platform_web::web_storage::FileReader,
    pub length: u64,
    pub checksum: ContentHash,
    _lease: BookLease,
}

impl VerifiedBook {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn into_upload_stream(self) -> impl futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> + Send {
        imp::upload_stream(self)
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn upload_file(&self) -> Result<web_sys::Blob, AssetStoreError> {
        self.reader.upload_file().await.map_err(AssetStoreError::operation)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn into_upload_body(self) -> Result<reqwest::Body, AssetStoreError> {
        imp::upload_body(self)
    }
}

pub struct BookLease(#[allow(dead_code)] imp::Lease);

pub struct AsyncAssetWriter {
    inner: imp::AsyncWriter,
    expected_hash: ContentHash,
    transfer_checksum: ContentHash,
    #[cfg(not(target_arch = "wasm32"))]
    hasher: blake3::Hasher,
    #[cfg(target_arch = "wasm32")]
    hasher: Option<Box<dyn crate::hashing::HashSession>>,
    #[cfg(target_arch = "wasm32")]
    digest: Option<String>,
    store: AssetStore,
    written: u64,
    next_storage_check: u64,
}

impl AsyncAssetWriter {
    pub fn set_transfer_checksum(&mut self, checksum: ContentHash) {
        self.transfer_checksum = checksum;
    }
    pub async fn write_all(&mut self, buffer: &[u8]) -> Result<(), AssetStoreError> {
        #[cfg(target_arch = "wasm32")]
        if self.hasher.is_none() {
            return Err(AssetStoreError::operation("cannot append to a verified asset"));
        }
        if self.written >= self.next_storage_check {
            self.store.check_write_capacity(buffer.len() as u64)?;
            self.next_storage_check = self.written.saturating_add(8 * 1024 * 1024);
        }
        self.inner.write_all(buffer).await?;
        #[cfg(not(target_arch = "wasm32"))]
        self.hasher.update(buffer);
        #[cfg(target_arch = "wasm32")]
        self.hasher.as_mut().unwrap().update(buffer).await.map_err(AssetStoreError::operation)?;
        self.written = self.written.checked_add(buffer.len() as u64).ok_or_else(|| AssetStoreError::operation("asset length overflow"))?;
        Ok(())
    }

    pub async fn verify(&mut self) -> Result<(), AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        let actual = self.hasher.finalize().to_hex().to_string();
        #[cfg(target_arch = "wasm32")]
        let actual = {
            if self.digest.is_none() {
                self.digest = Some(self.hasher.take().ok_or_else(|| AssetStoreError::operation("hash verification was interrupted"))?.finish().await.map_err(AssetStoreError::operation)?);
            }
            self.digest.as_ref().unwrap()
        };
        if actual.as_str() != self.transfer_checksum.as_str() {
            return Err(AssetStoreError::operation(format!("asset hash mismatch expected={} got={actual}", self.transfer_checksum)));
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn finish_book(mut self) -> Result<StagedBook, AssetStoreError> {
        self.verify().await?;
        let temporary_path = self.inner.finish_book().await?;
        let mut source = std::fs::File::open(&temporary_path)?;
        let identity = if let Some(identity) = book_identity::read(&mut source)? {
            identity
        } else if self.expected_hash != self.transfer_checksum {
            audiobook_folder::archive_identity(std::fs::File::open(&temporary_path)?)
                .map(|hash| ContentHash::new(hash.to_hex().as_str()))
                .unwrap_or(self.transfer_checksum)
        } else {
            self.transfer_checksum
        };
        if identity != self.expected_hash {
            return Err(AssetStoreError::operation("downloaded book identity does not match the requested book"));
        }
        Ok(StagedBook { inner: imp::StagedBook { temporary_path, content_hash: self.expected_hash.as_str().to_owned(), checksum: self.transfer_checksum, size_bytes: self.written, direct: None } })
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn commit(mut self) -> Result<(), AssetStoreError> {
        self.verify().await?;
        if self.inner.embedded_identity()?.unwrap_or(self.transfer_checksum) != self.expected_hash {
            return Err(AssetStoreError::operation("downloaded book identity does not match the requested book"));
        }
        self.inner.commit_verified(self.transfer_checksum).await
    }
}

pub struct StagedBook {
    inner: imp::StagedBook,
}

#[derive(Clone, Debug)]
pub struct PublishedStagedBook {
    pub name: String,
    pub relative_path: String,
    pub published: bool,
    pub fingerprint: Option<String>,
}

impl StagedBook {
    /// Publishes bytes from an immutable database snapshot. The caller records
    /// the resulting placement through the database acknowledgement API.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn publish_snapshot(self, store: &AssetStore, relative_path: &str, occupied_names: &[String]) -> Result<PublishedStagedBook, AssetStoreError> {
        let hash = self.content_hash();
        let imp::StagedBook { temporary_path, direct, checksum, .. } = self.inner;
        // A direct import already wrote its bytes straight to the final
        // destination; re-running the move/dedup logic below would find that
        // path already occupied by itself and discard it as a duplicate.
        if let Some(direct) = direct {
            imp::sync_parent(&direct.path)?;
            let fingerprint = Some(client_platform_native::file_fingerprint::file_metadata_fingerprint(&std::fs::metadata(&direct.path)?)?.to_string());
            let name = relative_path.rsplit_once('/').map_or(relative_path, |(_, tail)| tail).to_owned();
            temporary_path.keep().map_err(|error| AssetStoreError::operation(format!("could not finalize the imported book '{}': {}", direct.path.display(), error.error)))?;
            return Ok(PublishedStagedBook { name, relative_path: relative_path.to_owned(), published: true, fingerprint });
        }
        if checksum != hash && audiobook_folder::archive_identity(std::fs::File::open(&temporary_path)?).is_ok_and(|identity| ContentHash::new(identity.to_hex().as_str()) == hash) {
            let (parent, requested) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
            let mut occupied = occupied_names.to_vec();
            let mut name = requested.to_owned();
            loop {
                let relative = if parent.is_empty() { name.clone() } else { format!("{parent}/{name}") };
                let destination = imp::placement_path(&store.0, &relative)?;
                if destination.exists() {
                    if destination.is_dir() {
                        let tracks = audiobook_folder::discover(&destination)?;
                        if !tracks.is_empty() && ContentHash::new(audiobook_folder::identity(&tracks)?.to_hex().as_str()) == hash {
                            temporary_path.close()?;
                            return Ok(PublishedStagedBook { name, relative_path: relative, published: false, fingerprint: None });
                        }
                    }
                    occupied.push(name);
                    name = library_replica::unique_folder_name(requested, occupied.iter().map(String::as_str));
                    continue;
                }
                let staged = tempfile::Builder::new().prefix(".bokheim-audio-").tempdir_in(destination.parent().unwrap())?;
                audiobook_folder::extract_archive(std::fs::File::open(&temporary_path)?, staged.path())?;
                let tracks = audiobook_folder::discover(staged.path())?;
                if ContentHash::new(audiobook_folder::identity(&tracks)?.to_hex().as_str()) != hash {
                    return Err(AssetStoreError::operation("extracted audiobook identity does not match the download"));
                }
                let fingerprint = crate::filesystem::audiobook_fingerprint(staged.path(), &tracks)?.to_string();
                std::fs::rename(staged.path(), &destination)?;
                temporary_path.close()?;
                imp::sync_parent(&destination)?;
                return Ok(PublishedStagedBook { name, relative_path: relative, published: true, fingerprint: Some(fingerprint) });
            }
        }
        let (parent, name) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
        let mut occupied = occupied_names.to_vec();
        let mut chosen = name.to_owned();
        let mut temporary = temporary_path;
        loop {
            let relative = if parent.is_empty() { chosen.clone() } else { format!("{parent}/{chosen}") };
            if !imp::placement_conflicts(&store.0, &hash, &relative)? {
                let destination = imp::placement_path(&store.0, &relative)?;
                let published = if destination.is_file() {
                    temporary.close()?;
                    false
                } else {
                    match imp::persist_book_noclobber(temporary, &destination) {
                        Ok(()) => true,
                        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                            temporary = error.path;
                            chosen = library_replica::unique_file_name(name, occupied.iter().map(String::as_str));
                            occupied.push(chosen.clone());
                            continue;
                        }
                        Err(error) => return Err(AssetStoreError::operation(format!("could not move the temporary book to '{}' without replacing existing files: {}", destination.display(), error.error))),
                    }
                };
                imp::sync_parent(&destination)?;
                let fingerprint = if published { Some(client_platform_native::file_fingerprint::file_metadata_fingerprint(&std::fs::metadata(&destination)?)?.to_string()) } else { None };
                return Ok(PublishedStagedBook { name: chosen, relative_path: relative, published, fingerprint });
            }
            chosen = library_replica::unique_file_name(name, occupied.iter().map(String::as_str));
            occupied.push(chosen.clone());
        }
    }

    pub fn content_hash(&self) -> ContentHash {
        ContentHash::new(&self.inner.content_hash)
    }
    pub fn checksum(&self) -> ContentHash {
        self.inner.checksum
    }
    pub fn size_bytes(&self) -> u64 {
        self.inner.size_bytes
    }
    pub fn open_reader(&self) -> Result<BoxedBookReader, AssetStoreError> {
        imp::open_staged_reader(&self.inner)
    }
    #[cfg(target_arch = "wasm32")]
    pub async fn commit(self) -> Result<(), AssetStoreError> {
        self.inner.commit().await
    }
}

fn asset_name(kind: BlobKind, content_hash: &ContentHash) -> String {
    format!("{}/{}", kind.storage(), content_hash.as_str())
}

fn thumbnail_variant_name(content_hash: &ContentHash, width: u32) -> String {
    format!("thumbnail/{}.{}w", content_hash.as_str(), width)
}

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
use native as imp;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mp3_folder_download_extracts_and_upload_repackages_same_revision() {
        let library = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("Disc 1")).unwrap();
        std::fs::write(source.path().join("Disc 1/01.mp3"), b"opening").unwrap();
        std::fs::write(source.path().join("Disc 1/02.mp3"), b"ending").unwrap();
        std::fs::write(source.path().join("book.nfo"), b"Title: Story\n").unwrap();
        std::fs::write(source.path().join("book.cue"), b"FILE \"Disc 1/01.mp3\" MP3\nTRACK 01 AUDIO\nTITLE \"Opening\"\nINDEX 01 00:00:00\n").unwrap();
        std::fs::write(source.path().join("cover.jpg"), b"cover fixture").unwrap();
        let tracks = audiobook_folder::discover(source.path()).unwrap();
        let hash = ContentHash::new(audiobook_folder::identity(&tracks).unwrap().to_hex().as_str());
        let nfo = audiobook_folder::discover_nfo(source.path()).unwrap();
        let cue = audiobook_folder::discover_cue(source.path()).unwrap();
        let cover = audiobook_folder::discover_cover(source.path()).unwrap();
        let archive = audiobook_folder::write_archive_with_sidecars(std::io::Cursor::new(Vec::new()), &tracks, nfo.as_ref(), cue.as_ref(), cover.as_ref()).unwrap().into_inner();
        let checksum = ContentHash::new(blake3::hash(&archive).to_hex().as_str());
        let store = AssetStore::open(library.path().to_str().unwrap()).unwrap().with_book_paths(|_| Ok(vec!["Story".into()]));
        let mut writer = store.begin_book_write("Story", &hash).await.unwrap();
        writer.set_transfer_checksum(checksum);
        writer.write_all(&archive).await.unwrap();
        let staged = writer.finish_book().await.unwrap();
        let published = staged.publish_snapshot(&store, "Story", &[]).unwrap();
        assert!(published.published);
        assert_eq!(published.name, "Story");
        assert_eq!(std::fs::read(library.path().join("Story/Disc 1/02.mp3")).unwrap(), b"ending");
        assert_eq!(std::fs::read(library.path().join("Story/book.nfo")).unwrap(), b"Title: Story\n");
        assert_eq!(std::fs::read(library.path().join("Story/book.cue")).unwrap(), b"FILE \"Disc 1/01.mp3\" MP3\nTRACK 01 AUDIO\nTITLE \"Opening\"\nINDEX 01 00:00:00\n");
        assert_eq!(std::fs::read(library.path().join("Story/cover.jpg")).unwrap(), b"cover fixture");
        assert!(store.exists(BlobKind::Book, &hash).unwrap());
        let verified = store.verified_book_version_reader_at_paths(hash, Some(checksum), vec!["Story".into()]).await.unwrap().unwrap();
        assert_eq!(verified.length, archive.len() as u64);
        assert_eq!(verified.checksum, checksum);
        use futures_util::StreamExt as _;
        let result = verified.into_upload_stream().collect::<Vec<_>>().await.into_iter().collect::<Result<Vec<_>, _>>().unwrap().concat();
        assert_eq!(result, archive);
    }

    #[test]
    fn mp3_folder_survives_trash_restore_and_local_eviction() {
        let library = tempfile::tempdir().unwrap();
        let store = AssetStore::open(library.path().to_str().unwrap()).unwrap();
        let folder = library.path().join("Story");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("01.mp3"), b"opening").unwrap();
        std::fs::write(folder.join("book.nfo"), b"Title: Story\n").unwrap();
        let hash = ContentHash::new(audiobook_folder::identity(&audiobook_folder::discover(&folder).unwrap()).unwrap().to_hex().as_str());
        imp::prepare_trash(&store.0, &hash, "Story").unwrap().publish().unwrap();
        assert!(!folder.exists());
        imp::prepare_restore(&store.0, &hash, "Story", None, &[], &[]).unwrap().unwrap().publish().unwrap();
        assert_eq!(std::fs::read(folder.join("01.mp3")).unwrap(), b"opening");
        assert_eq!(std::fs::read(folder.join("book.nfo")).unwrap(), b"Title: Story\n");
        imp::evict_book_files(&store.0, &["Story".into()], &hash).unwrap();
        assert!(!folder.join("01.mp3").exists());
        assert_eq!(std::fs::read(folder.join("book.nfo")).unwrap(), b"Title: Story\n");
    }

    #[tokio::test]
    async fn failed_thumbnail_book_preserves_the_browse_variant() {
        let library = tempfile::tempdir().unwrap();
        let store = AssetStore::open(library.path().to_str().unwrap()).unwrap();
        let hash = ContentHash::new(blake3::hash(b"thumbnail owner").to_hex().as_str());
        store.write_thumbnail_variant(&hash, thumbnail::BROWSE_THUMBNAIL_WIDTH, b"previous browse cover").await.unwrap();
        let destination = library.path().join(crate::APP_HIDDEN_DIR).join("assets/thumbnail").join(hash.as_str());
        std::fs::create_dir(&destination).unwrap();
        assert!(store.write(BlobKind::Thumbnail, &hash, b"replacement cover").await.is_err());
        assert_eq!(store.read_thumbnail_variant(&hash, thumbnail::BROWSE_THUMBNAIL_WIDTH).await.unwrap().unwrap(), b"previous browse cover");
        assert_eq!(std::fs::read_dir(destination.parent().unwrap()).unwrap().count(), 2, "failed book must remove its temporary file");
    }

    #[tokio::test]
    async fn abandoned_and_invalid_downloads_remove_adjacent_temporary_files() {
        let library = tempfile::tempdir().unwrap();
        let store = AssetStore::open(library.path().to_str().unwrap()).unwrap();
        let hash = ContentHash::new(blake3::hash(b"complete").to_hex().as_str());
        std::fs::write(library.path().join("book.epub"), b"existing user file").unwrap();
        for verify in [false, true] {
            let mut writer = store.begin_book_write("/book.epub", &hash).await.unwrap();
            writer.write_all(b"incomplete").await.unwrap();
            if verify {
                assert!(writer.finish_book().await.is_err());
            } else {
                drop(writer);
            }
            assert!(!std::fs::read_dir(library.path()).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().contains(".part-")));
            assert_eq!(std::fs::read(library.path().join("book.epub")).unwrap(), b"existing user file");
        }
        assert!(!library.path().join(".bokheim/assets/book").exists());
    }

    #[tokio::test]
    async fn empty_import_removes_adjacent_temporary_file() {
        let library = tempfile::tempdir().unwrap();
        let store = AssetStore::open(library.path().to_str().unwrap()).unwrap();
        assert!(store.stage_book("/book.epub".into(), Box::new(std::io::empty())).await.is_err());
        assert_eq!(std::fs::read_dir(library.path()).unwrap().count(), 1);
    }
}

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
use browser as imp;

/// Initialize platform resources required for book leases.
pub fn configure_book_leases(app_data: &std::path::Path) -> Result<(), AssetStoreError> {
    #[cfg(target_os = "android")]
    client_platform_android::book::configure_lease_root(app_data)?;
    let _ = app_data;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn import_in_progress(path: &std::path::Path) -> bool {
    imp::import_in_progress(path)
}

#[cfg(not(target_arch = "wasm32"))]
mod native_placement;
#[cfg(not(target_arch = "wasm32"))]
pub use native_placement::NativePlacement;
