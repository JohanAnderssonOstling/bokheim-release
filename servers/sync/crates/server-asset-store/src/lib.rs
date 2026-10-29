use std::io::ErrorKind;
use std::time::Instant;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use sync_common::BookId;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub type AssetReader = Box<dyn AsyncRead + Unpin + Send>;
pub const MAX_BOOK_UPLOAD_DURATION: Duration = Duration::from_secs(2 * 60 * 60);
pub const MAX_THUMBNAIL_UPLOAD_DURATION: Duration = Duration::from_secs(2 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Book,
    Thumbnail,
    ThumbnailBrowse,
}

impl AssetKind {
    pub const fn max_bytes(self) -> Option<u64> {
        match self {
            Self::Book => None,
            Self::Thumbnail | Self::ThumbnailBrowse => Some(sync_common::MAX_THUMBNAIL_BYTES),
        }
    }
}

pub trait StoredAssetReader: AsyncRead + tokio::io::AsyncSeek + Unpin + Send {}
impl<T: AsyncRead + tokio::io::AsyncSeek + Unpin + Send> StoredAssetReader for T {}

pub struct StoredAsset {
    pub reader: Box<dyn StoredAssetReader>,
    pub total_len: u64,
    pub offset: u64,
    pub content_hash: BookId,
}

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("asset access is forbidden")]
    Forbidden,
    #[error("invalid asset input: {0}")]
    InvalidInput(String),
    #[error("asset bytes do not match the requested content hash")]
    HashMismatch,
    #[error("asset uploads require an exact Content-Length")]
    LengthRequired,
    #[error("asset body length {actual} does not match the declared length {expected}")]
    LengthMismatch { expected: u64, actual: u64 },
    #[error("an upload for this user and book is already in progress")]
    UploadInProgress,
    #[error("book upload staging capacity is temporarily exhausted")]
    Busy,
    #[error("book storage quota exceeded: limit={limit}, used={used}, reserved={reserved}, requested={requested}")]
    QuotaExceeded { limit: u64, used: u64, reserved: u64, requested: u64 },
    #[error("asset exceeds the {limit}-byte limit")]
    TooLarge { limit: u64 },
    #[error("asset byte offset {offset} is beyond its {len}-byte length")]
    RangeNotSatisfiable { offset: u64, len: u64 },
    #[error("asset storage failed: {0}")]
    Storage(String),
}

const DEFAULT_MAX_STAGED_BOOKS: usize = 16;
const DEFAULT_MAX_STAGED_BOOKS_PER_ACCOUNT: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadLimits {
    pub max_staged_books: usize,
    pub max_staged_books_per_account: usize,
}

impl Default for UploadLimits {
    fn default() -> Self {
        Self { max_staged_books: DEFAULT_MAX_STAGED_BOOKS, max_staged_books_per_account: DEFAULT_MAX_STAGED_BOOKS_PER_ACCOUNT }
    }
}

impl UploadLimits {
    pub fn from_env() -> Result<Self, String> {
        let names = ["BOKHEIM_MAX_STAGED_BOOK_UPLOADS", "BOKHEIM_MAX_STAGED_BOOK_UPLOADS_PER_ACCOUNT"];
        let mut values = HashMap::new();
        for name in names {
            match std::env::var(name) {
                Ok(value) => {
                    values.insert(name, value);
                }
                Err(std::env::VarError::NotPresent) => {}
                Err(error) => return Err(format!("cannot read {name}: {error}")),
            }
        }
        Self::from_values(|name| values.get(name).cloned())
    }

    fn from_values(mut value: impl FnMut(&str) -> Option<String>) -> Result<Self, String> {
        let defaults = Self::default();
        let integer = |raw: Option<String>, name: &str| raw.map(|raw| raw.parse::<u64>().map_err(|_| format!("{name} must be an integer"))).transpose();
        let max_staged_books = integer(value("BOKHEIM_MAX_STAGED_BOOK_UPLOADS"), "BOKHEIM_MAX_STAGED_BOOK_UPLOADS")?.unwrap_or(defaults.max_staged_books as u64);
        let max_staged_books_per_account = integer(value("BOKHEIM_MAX_STAGED_BOOK_UPLOADS_PER_ACCOUNT"), "BOKHEIM_MAX_STAGED_BOOK_UPLOADS_PER_ACCOUNT")?.unwrap_or(defaults.max_staged_books_per_account as u64);
        if !(1..=256).contains(&max_staged_books) || !(1..=max_staged_books).contains(&max_staged_books_per_account) {
            return Err("staged book upload counts must be between 1 and 256 and the per-account limit cannot exceed the global limit".to_string());
        }
        Ok(Self { max_staged_books: max_staged_books as usize, max_staged_books_per_account: max_staged_books_per_account as usize })
    }
}

struct AccountStagingLimit {
    books: Arc<Semaphore>,
}

struct StagingPermit {
    _account_books: OwnedSemaphorePermit,
    _global_books: OwnedSemaphorePermit,
    _account: Arc<AccountStagingLimit>,
}

struct StagingGate {
    books: Arc<Semaphore>,
    per_account_books: usize,
    accounts: Mutex<HashMap<String, Weak<AccountStagingLimit>>>,
}

impl StagingGate {
    fn new(limits: UploadLimits) -> Self {
        Self { books: Arc::new(Semaphore::new(limits.max_staged_books)), per_account_books: limits.max_staged_books_per_account, accounts: Mutex::new(HashMap::new()) }
    }

    fn acquire(&self, user_id: &str, _size_bytes: u64) -> Result<StagingPermit, AssetError> {
        let account = {
            let mut accounts = self.accounts.lock().map_err(|_| AssetError::Storage("upload staging account lock is poisoned".to_string()))?;
            accounts.retain(|_, account| account.strong_count() > 0);
            match accounts.get(user_id).and_then(Weak::upgrade) {
                Some(account) => account,
                None => {
                    let account = Arc::new(AccountStagingLimit { books: Arc::new(Semaphore::new(self.per_account_books)) });
                    accounts.insert(user_id.to_string(), Arc::downgrade(&account));
                    account
                }
            }
        };
        let account_books = account.books.clone().try_acquire_owned().map_err(|_| AssetError::Busy)?;
        let global_books = self.books.clone().try_acquire_owned().map_err(|_| AssetError::Busy)?;
        Ok(StagingPermit { _account_books: account_books, _global_books: global_books, _account: account })
    }
}

pub struct FsAssetStore {
    books_root: PathBuf,
    thumbnails_root: PathBuf,
    staging: StagingGate,
}

/// Verified upload bytes that have not yet been published. Dropping this value
/// removes its temporary file, including when database authorization fails.
pub struct StagedAsset {
    temp: TemporaryFile,
    final_path: PathBuf,
    kind: AssetKind,
    total: u64,
    _permit: Option<StagingPermit>,
}

impl StagedAsset {
    pub fn size_bytes(&self) -> u64 { self.total }

    pub async fn publish(self) -> Result<u64, AssetError> {
        let Self { temp, final_path, kind, total, _permit } = self;
        let parent = final_path.parent().expect("staged asset has a parent");
        if kind == AssetKind::Book {
            match tokio::fs::hard_link(temp.path(), &final_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                    let existing = tokio::fs::metadata(&final_path).await.map_err(storage_error)?.len();
                    if existing != total {
                        return Err(AssetError::Storage(format!("existing content-addressed asset has length {existing}, uploaded copy has length {total}")));
                    }
                }
                Err(error) => return Err(storage_error(error)),
            }
        } else {
            tokio::fs::rename(temp.path(), &final_path).await.map_err(storage_error)?;
        }
        temp.remove().map_err(storage_error)?;
        std::fs::File::open(parent).and_then(|directory| directory.sync_all()).map_err(storage_error)?;
        Ok(total)
    }
}

fn storage_error(error: std::io::Error) -> AssetError { AssetError::Storage(error.to_string()) }

fn remove_if_present(path: &Path) -> Result<(), AssetError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_error(error)),
    }
}

struct TemporaryFile {
    path: Option<PathBuf>,
}

impl TemporaryFile {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("temporary file path is present until cleanup")
    }

    fn remove(mut self) -> std::io::Result<()> {
        let path = self.path.take().expect("temporary file path is present until cleanup");
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => {
                self.path = Some(path);
                Err(error)
            }
        }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl FsAssetStore {
    pub fn new(books_root: PathBuf, thumbnails_root: PathBuf) -> Self {
        Self::with_upload_limits(books_root, thumbnails_root, UploadLimits::default())
    }

    pub fn with_upload_limits(books_root: PathBuf, thumbnails_root: PathBuf, limits: UploadLimits) -> Self {
        Self { books_root, thumbnails_root, staging: StagingGate::new(limits) }
    }

    pub fn user_scope(user_id: &str) -> String {
        blake3::hash(user_id.as_bytes()).to_hex().to_string()
    }

    /// Enumerates published books only; active temporary uploads are excluded.
    pub async fn book_hashes(&self) -> Result<Vec<BookId>, AssetError> {
        let mut hashes = Vec::new();
        let mut shards = tokio::fs::read_dir(&self.books_root).await.map_err(storage_error)?;
        while let Some(shard) = shards.next_entry().await.map_err(storage_error)? {
            if !shard.file_type().await.map_err(storage_error)?.is_dir() { continue; }
            let mut files = tokio::fs::read_dir(shard.path()).await.map_err(storage_error)?;
            while let Some(file) = files.next_entry().await.map_err(storage_error)? {
                let Some(hash) = file.file_name().to_str().and_then(|name| name.parse::<BookId>().ok()) else { continue; };
                if file.path() == self.path("", AssetKind::Book, &hash)? && file.file_type().await.map_err(storage_error)?.is_file() {
                    hashes.push(hash);
                }
            }
        }
        Ok(hashes)
    }

    /// Enumerates account-scoped covers, including both legacy variants.
    pub async fn thumbnail_objects(&self) -> Result<Vec<(String, BookId)>, AssetError> {
        let mut objects = Vec::new();
        let mut users = match tokio::fs::read_dir(self.thumbnails_root.join("users")).await {
            Ok(users) => users,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(objects),
            Err(error) => return Err(storage_error(error)),
        };
        while let Some(user) = users.next_entry().await.map_err(storage_error)? {
            let scope = user.file_name().to_string_lossy().into_owned();
            if scope.len() != 64 || !scope.bytes().all(|byte| byte.is_ascii_hexdigit()) || !user.file_type().await.map_err(storage_error)?.is_dir() { continue; }
            let mut seen = HashSet::new();
            let mut files = tokio::fs::read_dir(user.path()).await.map_err(storage_error)?;
            while let Some(file) = files.next_entry().await.map_err(storage_error)? {
                let name = file.file_name();
                let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".pair").or_else(|| name.strip_suffix(".jpg"))) else { continue; };
                let stem = stem.strip_suffix("-300").unwrap_or(stem);
                if let Ok(hash) = stem.parse::<BookId>() {
                    if seen.insert(hash) { objects.push((scope.clone(), hash)); }
                }
            }
        }
        Ok(objects)
    }

    /// Synchronous unlink avoids an offloaded unlink continuing after the
    /// caller's database lock has been released by task cancellation.
    pub fn remove_book(&self, hash: &BookId) -> Result<(), AssetError> {
        remove_if_present(&self.path("", AssetKind::Book, hash)?)
    }

    pub fn remove_scoped_thumbnail(&self, scope: &str, hash: &BookId) -> Result<(), AssetError> {
        if scope.len() != 64 || !scope.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AssetError::InvalidInput("invalid thumbnail account scope".into()));
        }
        let root = self.thumbnails_root.join("users").join(scope);
        for name in [format!("{hash}.pair"), format!("{hash}.jpg"), format!("{hash}-300.jpg")] {
            remove_if_present(&root.join(name))?;
        }
        Ok(())
    }

    fn path(&self, user_id: &str, kind: AssetKind, hash: &BookId) -> Result<PathBuf, AssetError> {
        Ok(match kind {
            AssetKind::Book => self.books_root.join(&hash.to_hex().as_str()[..2]).join(hash.to_hex().as_str()),
            AssetKind::Thumbnail => self.thumbnails_root.join("users").join(Self::user_scope(user_id)).join(format!("{hash}.jpg")),
            AssetKind::ThumbnailBrowse => self.thumbnails_root.join("users").join(Self::user_scope(user_id)).join(format!("{hash}-300.jpg")),
        })
    }

    async fn probe_root(label: &str, root: &Path, create: bool) -> Result<(), AssetError> {
        if create {
            tokio::fs::create_dir_all(root).await.map_err(|error| AssetError::Storage(format!("cannot create {label} storage directory {}: {error}", root.display())))?;
        }
        let metadata = tokio::fs::metadata(root).await.map_err(|error| AssetError::Storage(format!("cannot inspect {label} storage directory {}: {error}", root.display())))?;
        if !metadata.is_dir() {
            return Err(AssetError::Storage(format!("{label} storage path is not a directory: {}", root.display())));
        }

        let probe = root.join(format!(".bokheim-write-check-{}", uuid::Uuid::new_v4()));
        let mut file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&probe).await.map_err(|error| AssetError::Storage(format!("{label} storage directory is not writable at {}: {error}", root.display())))?;
        if let Err(error) = file.write_all(b"bokheim-storage-check").await {
            drop(file);
            let _ = tokio::fs::remove_file(&probe).await;
            return Err(AssetError::Storage(format!("cannot write to {label} storage directory {}: {error}", root.display())));
        }
        if let Err(error) = file.sync_all().await {
            drop(file);
            let _ = tokio::fs::remove_file(&probe).await;
            return Err(AssetError::Storage(format!("cannot sync {label} storage directory probe {}: {error}", root.display())));
        }
        drop(file);
        tokio::fs::remove_file(&probe).await.map_err(|error| AssetError::Storage(format!("cannot remove probe from {label} storage directory {}: {error}", root.display())))?;
        Ok(())
    }
}

impl FsAssetStore {
    pub async fn initialize(&self) -> Result<(), AssetError> {
        Self::probe_root("book", &self.books_root, true).await?;
        Self::probe_root("thumbnail", &self.thumbnails_root, true).await?;
        Ok(())
    }

    pub async fn check_ready(&self) -> Result<(), AssetError> {
        Self::probe_root("book", &self.books_root, false).await?;
        Self::probe_root("thumbnail", &self.thumbnails_root, false).await?;
        Ok(())
    }

    pub async fn size(&self, user_id: &str, kind: AssetKind, hash: &BookId) -> Result<Option<u64>, AssetError> {
        if kind != AssetKind::Book {
            if let Some((bytes, browse)) = self.thumbnail_pair(user_id, hash).await? {
                return Ok(Some(if kind == AssetKind::Thumbnail { bytes.len() } else { browse.len() } as u64));
            }
        }
        match tokio::fs::metadata(self.path(user_id, kind, hash)?).await {
            Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(AssetError::Storage(error.to_string())),
        }
    }

    fn thumbnail_pair_path(&self, user_id: &str, hash: &BookId) -> PathBuf {
        self.thumbnails_root.join("users").join(Self::user_scope(user_id)).join(format!("{hash}.pair"))
    }

    /// Publish both opaque variants with a single atomic rename.
    pub async fn put_thumbnail_pair(&self, user_id: &str, hash: &BookId, bytes: &[u8], browse_bytes: &[u8]) -> Result<(), AssetError> {
        for variant in [bytes, browse_bytes] {
            if variant.is_empty() {
                return Err(AssetError::InvalidInput("thumbnail variants must not be empty".into()));
            }
            if variant.len() as u64 > sync_common::MAX_THUMBNAIL_BYTES {
                return Err(AssetError::TooLarge { limit: sync_common::MAX_THUMBNAIL_BYTES });
            }
        }
        let path = self.thumbnail_pair_path(user_id, hash);
        let parent = path.parent().expect("thumbnail path has a parent");
        tokio::fs::create_dir_all(parent).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        let temp = TemporaryFile::new(parent.join(format!(".thumbnail-{}.part", uuid::Uuid::new_v4())));
        // Create synchronously so cancellation cannot strand a file after its guard runs.
        let mut file = File::from_std(std::fs::OpenOptions::new().write(true).create_new(true).open(temp.path()).map_err(|error| AssetError::Storage(error.to_string()))?);
        file.write_all(b"BTH1").await.map_err(|error| AssetError::Storage(error.to_string()))?;
        file.write_all(&(bytes.len() as u64).to_le_bytes()).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        file.write_all(bytes).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        file.write_all(browse_bytes).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        file.flush().await.map_err(|error| AssetError::Storage(error.to_string()))?;
        file.sync_all().await.map_err(|error| AssetError::Storage(error.to_string()))?;
        drop(file);
        tokio::fs::rename(temp.path(), &path).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        std::fs::File::open(parent).and_then(|directory| directory.sync_all()).map_err(|error| AssetError::Storage(error.to_string()))?;
        Ok(())
    }

    /// Opening one file pins both variants even when another upload replaces it.
    pub async fn thumbnail_pair(&self, user_id: &str, hash: &BookId) -> Result<Option<(Vec<u8>, Vec<u8>)>, AssetError> {
        let file = match File::open(self.thumbnail_pair_path(user_id, hash)).await {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AssetError::Storage(error.to_string())),
        };
        let mut data = Vec::new();
        file.take(2 * sync_common::MAX_THUMBNAIL_BYTES + 13).read_to_end(&mut data).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        if data.len() < 12 || &data[..4] != b"BTH1" || data.len() as u64 > 2 * sync_common::MAX_THUMBNAIL_BYTES + 12 {
            return Err(AssetError::Storage("invalid stored thumbnail pair".into()));
        }
        let count = u64::from_le_bytes(data[4..12].try_into().unwrap());
        if count == 0 || count > sync_common::MAX_THUMBNAIL_BYTES || count >= (data.len() - 12) as u64 || (data.len() - 12) as u64 - count > sync_common::MAX_THUMBNAIL_BYTES {
            return Err(AssetError::Storage("invalid stored thumbnail variant lengths".into()));
        }
        let browse = data.split_off(12 + count as usize);
        Ok(Some((data.split_off(12), browse)))
    }

    pub async fn remove_thumbnail(&self, user_id: &str, hash: &BookId) -> Result<(), AssetError> {
        match tokio::fs::remove_file(self.thumbnail_pair_path(user_id, hash)).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(AssetError::Storage(error.to_string())),
        }
        for kind in [AssetKind::Thumbnail, AssetKind::ThumbnailBrowse] {
            let path = self.path(user_id, kind, hash)?;
            match tokio::fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(AssetError::Storage(error.to_string())),
            }
        }
        Ok(())
    }

    /// A cheap validator for the atomically published cover pair. Replacing a cover uses
    /// atomic rename, so a new inode changes this token even when byte length
    /// and filesystem timestamp resolution are unchanged.
    pub async fn thumbnail_revision(&self, user_id: &str, hash: &BookId) -> Result<Option<BookId>, AssetError> {
        let metadata = match tokio::fs::metadata(self.thumbnail_pair_path(user_id, hash)).await {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AssetError::Storage(error.to_string())),
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(&metadata.len().to_le_bytes());
        let modified = metadata.modified().map_err(|error| AssetError::Storage(error.to_string()))?
            .duration_since(std::time::UNIX_EPOCH).map_err(|error| AssetError::Storage(error.to_string()))?;
        hasher.update(&modified.as_nanos().to_le_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            hasher.update(&metadata.dev().to_le_bytes());
            hasher.update(&metadata.ino().to_le_bytes());
        }
        Ok(Some(BookId::from(hasher.finalize())))
    }

    pub async fn open_from(&self, user_id: &str, kind: AssetKind, hash: &BookId, offset: u64) -> Result<Option<StoredAsset>, AssetError> {
        if kind != AssetKind::Book {
            if let Some((bytes, browse)) = self.thumbnail_pair(user_id, hash).await? {
                let bytes = if kind == AssetKind::Thumbnail { bytes } else { browse };
                let len = bytes.len() as u64;
                if offset > len || (offset > 0 && offset == len) {
                    return Err(AssetError::RangeNotSatisfiable { offset, len });
                }
                let mut reader = std::io::Cursor::new(bytes);
                reader.set_position(offset);
                return Ok(Some(StoredAsset { reader: Box::new(reader), total_len: len, offset, content_hash: *hash }));
            }
        }
        let mut file = match File::open(self.path(user_id, kind, hash)?).await {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AssetError::Storage(error.to_string())),
        };
        let len = file.metadata().await.map_err(|error| AssetError::Storage(error.to_string()))?.len();
        if offset > len || (offset > 0 && offset == len) {
            return Err(AssetError::RangeNotSatisfiable { offset, len });
        }
        file.seek(SeekFrom::Start(offset)).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        Ok(Some(StoredAsset { reader: Box::new(file), total_len: len, offset, content_hash: *hash }))
    }

    pub async fn book_tracks(&self, content_hash: &BookId) -> Result<Vec<audiobook_archive::ArchivedTrack>, AssetError> {
        let path = self.path("", AssetKind::Book, content_hash)?;
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(path).map_err(|error| AssetError::Storage(error.to_string()))?;
            audiobook_archive::archived_tracks(file).map_err(|error| AssetError::InvalidInput(error.to_string()))
        }).await.map_err(|error| AssetError::Storage(error.to_string()))?
    }

    pub async fn put(&self, user_id: &str, kind: AssetKind, hash: &BookId, expected_len: Option<u64>, reader: AssetReader) -> Result<u64, AssetError> {
        self.stage(user_id, kind, hash, expected_len, reader).await?.publish().await
    }

    pub async fn stage(&self, user_id: &str, kind: AssetKind, hash: &BookId, expected_len: Option<u64>, mut reader: AssetReader) -> Result<StagedAsset, AssetError> {
        let permit = if kind == AssetKind::Book { Some(self.staging.acquire(user_id, expected_len.ok_or(AssetError::LengthRequired)?)?) } else { None };
        let final_path = self.path(user_id, kind, hash)?;
        let parent = final_path.parent().ok_or_else(|| AssetError::Storage("asset path has no parent".to_string()))?;
        tokio::fs::create_dir_all(parent).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        let temp = TemporaryFile::new(PathBuf::from(format!("{}.tmp-{}", final_path.display(), uuid::Uuid::new_v4())));
        // Open synchronously so cancellation cannot race an offloaded create
        // operation and strand a file after the cleanup guard has run.
        let mut file = File::from_std(std::fs::File::create(temp.path()).map_err(|error| AssetError::Storage(error.to_string()))?);
        let stream_started = Instant::now();
        let mut read_time = std::time::Duration::ZERO;
        let mut write_time = std::time::Duration::ZERO;
        let measure = tracing::enabled!(target: "sync_performance", tracing::Level::DEBUG);
        let mut total = 0_u64;
        let mut book_hasher = (kind == AssetKind::Book).then(blake3::Hasher::new);
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read_started = measure.then(Instant::now);
            let count = match reader.read(&mut buffer).await {
                Ok(count) => count,
                Err(error) => return Err(AssetError::InvalidInput(error.to_string())),
            };
            if let Some(started) = read_started { read_time += started.elapsed(); }
            if count == 0 {
                break;
            }
            total = total.saturating_add(count as u64);
            if let Some(hasher) = book_hasher.as_mut() {
                hasher.update(&buffer[..count]);
            }
            if expected_len.is_some_and(|expected| total > expected) {
                return Err(AssetError::LengthMismatch { expected: expected_len.unwrap_or(0), actual: total });
            }
            if kind.max_bytes().is_some_and(|limit| total > limit) {
                return Err(AssetError::TooLarge { limit: kind.max_bytes().unwrap_or(u64::MAX) });
            }
            let write_started = measure.then(Instant::now);
            file.write_all(&buffer[..count]).await.map_err(|error| AssetError::Storage(error.to_string()))?;
            if let Some(started) = write_started { write_time += started.elapsed(); }
        }
        if expected_len.is_some_and(|expected| total != expected) {
            return Err(AssetError::LengthMismatch { expected: expected_len.unwrap_or(0), actual: total });
        }
        if let Some(hasher) = book_hasher {
            if hasher.finalize().as_bytes() != hash.as_bytes() {
                return Err(AssetError::HashMismatch);
            }
        }
        tracing::debug!(target: "sync_performance", phase = "receive_hash_write", bytes = total, elapsed_ms = stream_started.elapsed().as_secs_f64() * 1000.0, read_ms = read_time.as_secs_f64() * 1000.0, write_ms = write_time.as_secs_f64() * 1000.0);
        let durable_started = Instant::now();
        file.flush().await.map_err(|error| AssetError::Storage(error.to_string()))?;
        drop(file);
        let durable_file = tokio::fs::OpenOptions::new().write(true).open(temp.path()).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        durable_file.sync_all().await.map_err(|error| AssetError::Storage(error.to_string()))?;
        drop(durable_file);
        tracing::debug!(target: "sync_performance", phase = "file_flush_fsync", elapsed_ms = durable_started.elapsed().as_secs_f64() * 1000.0);
        Ok(StagedAsset { temp, final_path, kind, total, _permit: permit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageOutputFormat};
    use std::io::{Cursor, Write};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
    use zip::write::SimpleFileOptions;

    fn reader(bytes: Vec<u8>) -> Box<dyn AsyncRead + Unpin + Send> {
        Box::new(std::io::Cursor::new(bytes))
    }


    fn jpeg(pixel: [u8; 3]) -> Vec<u8> {
        let mut image = DynamicImage::new_rgb8(1, 1).to_rgb8();
        image.put_pixel(0, 0, image::Rgb(pixel));
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image).write_to(&mut bytes, ImageOutputFormat::Jpeg(75)).unwrap();
        bytes.into_inner()
    }

    fn epub_with_chapter(chapter: &str, undeclared_image: bool) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let compressed = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        archive.start_file("mimetype", stored).unwrap();
        archive.write_all(b"application/epub+zip").unwrap();
        archive.start_file("META-INF/container.xml", compressed).unwrap();
        archive
            .write_all(br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#)
            .unwrap();
        archive.start_file("OEBPS/content.opf", compressed).unwrap();
        archive.write_all(br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#).unwrap();
        archive.start_file("OEBPS/chapter.xhtml", compressed).unwrap();
        archive.write_all(format!(r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>{chapter}</body></html>"#).as_bytes()).unwrap();
        if undeclared_image {
            archive.start_file("OEBPS/hidden.jpg", compressed).unwrap();
            archive.write_all(&jpeg([1, 2, 3])).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    fn epub(undeclared_image: bool) -> Vec<u8> {
        epub_with_chapter("book", undeclared_image)
    }

    #[tokio::test]
    async fn initialization_probes_storage_and_fails_for_a_non_directory_root() {
        let temp = tempfile::tempdir().unwrap();
        let books = temp.path().join("books");
        let thumbnails = temp.path().join("thumbnails");
        let store = FsAssetStore::new(books.clone(), thumbnails.clone());
        store.initialize().await.unwrap();
        assert_eq!(std::fs::read_dir(&books).unwrap().count(), 0, "write probe must be removed");
        assert_eq!(std::fs::read_dir(&thumbnails).unwrap().count(), 0, "write probe must be removed");

        let invalid = temp.path().join("not-a-directory");
        std::fs::write(&invalid, b"file").unwrap();
        let error = FsAssetStore::new(invalid.clone(), thumbnails).initialize().await.unwrap_err();
        assert!(error.to_string().contains("book storage"), "unexpected error: {error}");
        assert!(error.to_string().contains(&invalid.display().to_string()), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn readiness_does_not_recreate_a_missing_storage_root() {
        let temp = tempfile::tempdir().unwrap();
        let books = temp.path().join("books");
        let thumbnails = temp.path().join("thumbs");
        let store = FsAssetStore::new(books.clone(), thumbnails);
        store.initialize().await.unwrap();
        tokio::fs::remove_dir(&books).await.unwrap();

        let error = store.check_ready().await.unwrap_err();
        assert!(error.to_string().contains("cannot inspect book storage"));
        assert!(!books.exists(), "readiness must not hide a lost mount by recreating its directory");
    }

    #[tokio::test]
    async fn verifies_books_but_keys_thumbnails_by_source_content() {
        let temp = tempfile::tempdir().unwrap();
        let store = FsAssetStore::new(temp.path().join("books"), temp.path().join("thumbs"));
        store.initialize().await.unwrap();
        let bytes = epub(false);
        let len = bytes.len() as u64;
        let hash = BookId::from(blake3::hash(&bytes));
        store.put("user", AssetKind::Book, &hash, Some(len), reader(bytes)).await.unwrap();
        assert!(store.size("other-user", AssetKind::Book, &hash).await.unwrap().is_some(), "book blobs remain globally deduplicated");
        let wrong = epub_with_chapter("different book", false);
        assert!(matches!(store.put("user", AssetKind::Book, &hash, Some(wrong.len() as u64), reader(wrong)).await, Err(AssetError::HashMismatch)));
        let mut stored = store.open_from("other-user", AssetKind::Book, &hash, 0).await.unwrap().unwrap();
        let mut stored_bytes = Vec::new();
        stored.reader.read_to_end(&mut stored_bytes).await.unwrap();
        assert_eq!(stored_bytes, epub(false));
        store.put("user", AssetKind::Thumbnail, &hash, None, reader(jpeg([1, 2, 3]))).await.unwrap();
        assert!(store.size("user", AssetKind::Thumbnail, &hash).await.unwrap().is_some());
        assert!(store.size("other-user", AssetKind::Thumbnail, &hash).await.unwrap().is_none(), "covers must not cross account boundaries");
        let browse = jpeg([4, 5, 6]);
        store.put("user", AssetKind::ThumbnailBrowse, &hash, Some(browse.len() as u64), reader(browse.clone())).await.unwrap();
        let mut stored_browse = store.open_from("user", AssetKind::ThumbnailBrowse, &hash, 0).await.unwrap().unwrap();
        let mut stored_browse_bytes = Vec::new();
        stored_browse.reader.read_to_end(&mut stored_browse_bytes).await.unwrap();
        assert_eq!(stored_browse_bytes, browse);
        assert!(store.size("other-user", AssetKind::ThumbnailBrowse, &hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ranges_report_total_length_and_stream_only_the_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let store = FsAssetStore::new(temp.path().join("books"), temp.path().join("thumbs"));
        store.initialize().await.unwrap();
        let bytes = epub(false);
        let expected_suffix = bytes[4..].to_vec();
        let expected_len = bytes.len() as u64;
        let hash = BookId::from(blake3::hash(&bytes));
        store.put("user", AssetKind::Book, &hash, Some(expected_len), reader(bytes)).await.unwrap();

        let mut stored = store.open_from("user", AssetKind::Book, &hash, 4).await.unwrap().unwrap();
        let mut suffix = Vec::new();
        stored.reader.read_to_end(&mut suffix).await.unwrap();
        assert_eq!((stored.offset, stored.total_len, suffix), (4, expected_len, expected_suffix));
        assert!(matches!(store.open_from("user", AssetKind::Book, &hash, expected_len).await, Err(AssetError::RangeNotSatisfiable { .. })));
    }

    #[tokio::test]
    async fn declared_book_length_is_enforced_before_book() {
        let temp = tempfile::tempdir().unwrap();
        let store = FsAssetStore::new(temp.path().join("books"), temp.path().join("thumbs"));
        store.initialize().await.unwrap();
        let bytes = epub(false);
        let hash = BookId::from(blake3::hash(&bytes));
        assert!(matches!(store.put("user", AssetKind::Book, &hash, Some(bytes.len() as u64 - 1), reader(bytes)).await, Err(AssetError::LengthMismatch { .. })));
        assert!(store.size("user", AssetKind::Book, &hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn cancelling_an_upload_removes_its_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(FsAssetStore::new(temp.path().join("books"), temp.path().join("thumbs")));
        store.initialize().await.unwrap();
        let hash = BookId::new(&"a".repeat(64));
        let final_path = store.path("user", AssetKind::Book, &hash).unwrap();
        let parent = final_path.parent().unwrap().to_path_buf();
        let (mut writer, reader) = tokio::io::duplex(8);
        writer.write_all(b"a").await.unwrap();

        let uploading = {
            let store = store.clone();
            tokio::spawn(async move { store.put("user", AssetKind::Book, &hash, Some(2), Box::new(reader)).await })
        };
        let mut temporary_file_created = false;
        for _ in 0..1_000 {
            if parent.exists() && std::fs::read_dir(&parent).unwrap().next().is_some() {
                temporary_file_created = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(temporary_file_created, "the upload should create its temporary file before cancellation");
        uploading.abort();
        assert!(uploading.await.unwrap_err().is_cancelled());
        assert!(!final_path.exists());
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0, "cancellation must not strand a temporary file");
    }

    #[tokio::test]
    async fn thumbnail_pairs_are_atomic_bounded_and_revisioned_together() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(FsAssetStore::new(temp.path().join("books"), temp.path().join("thumbs")));
        store.initialize().await.unwrap();
        let hash = BookId::new(&"d".repeat(64));
        store.put_thumbnail_pair("alice", &hash, b"master", b"browse-one").await.unwrap();
        let old_revision = store.thumbnail_revision("alice", &hash).await.unwrap();
        store.put_thumbnail_pair("alice", &hash, b"master", b"browse-two").await.unwrap();
        assert_ne!(store.thumbnail_revision("alice", &hash).await.unwrap(), old_revision, "a browse-only replacement changes the pair revision");
        for (master, browse) in [(Vec::new(), b"browse".to_vec()), (b"master".to_vec(), Vec::new()), (vec![0; sync_common::MAX_THUMBNAIL_BYTES as usize + 1], b"browse".to_vec()), (b"master".to_vec(), vec![0; sync_common::MAX_THUMBNAIL_BYTES as usize + 1])] {
            assert!(store.put_thumbnail_pair("alice", &hash, &master, &browse).await.is_err());
            assert_eq!(store.thumbnail_pair("alice", &hash).await.unwrap(), Some((b"master".to_vec(), b"browse-two".to_vec())));
        }
        assert!(store.thumbnail_pair("bob", &hash).await.unwrap().is_none());
        assert!(store.thumbnail_revision("bob", &hash).await.unwrap().is_none());
        let mut pinned = store.open_from("alice", AssetKind::ThumbnailBrowse, &hash, 0).await.unwrap().unwrap();
        store.put_thumbnail_pair("alice", &hash, &[0; 100], &[0; 40]).await.unwrap();
        let mut bytes = Vec::new();
        pinned.reader.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"browse-two");
        let writer_store = store.clone();
        let writer = tokio::spawn(async move {
            for generation in 1..40u8 {
                writer_store.put_thumbnail_pair("alice", &hash, &[generation; 100], &[generation; 40]).await.unwrap();
            }
        });
        for _ in 0..100 {
            let (master, browse) = store.thumbnail_pair("alice", &hash).await.unwrap().unwrap();
            assert_eq!(master.len(), 100);
            assert_eq!(browse.len(), 40);
            assert_eq!(master[0], browse[0], "concurrent replacements must never mix variants");
            tokio::task::yield_now().await;
        }
        writer.await.unwrap();
        store.remove_thumbnail("alice", &hash).await.unwrap();
        assert!(store.thumbnail_pair("alice", &hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn incomplete_legacy_thumbnails_are_not_reported_as_a_pair() {
        let temp = tempfile::tempdir().unwrap();
        let store = FsAssetStore::new(temp.path().join("books"), temp.path().join("thumbs"));
        store.initialize().await.unwrap();
        let hash = BookId::new(&"e".repeat(64));
        store.put("alice", AssetKind::Thumbnail, &hash, None, reader(b"old master".to_vec())).await.unwrap();
        assert!(store.thumbnail_pair("alice", &hash).await.unwrap().is_none());
        assert!(store.thumbnail_revision("alice", &hash).await.unwrap().is_none());
        tokio::fs::write(store.thumbnail_pair_path("alice", &hash), b"BTH1bad").await.unwrap();
        assert!(store.thumbnail_pair("alice", &hash).await.is_err());
    }

    #[tokio::test]
    async fn thumbnails_are_opaque_bounded_and_isolated_by_user() {
        let temp = tempfile::tempdir().unwrap();
        let thumbnails = temp.path().join("thumbs");
        let store = FsAssetStore::new(temp.path().join("books"), thumbnails.clone());
        store.initialize().await.unwrap();
        let hash = BookId::new(&"a".repeat(64));
        let thumbnail_limit = AssetKind::Thumbnail.max_bytes().unwrap();
        assert!(matches!(store.put("alice", AssetKind::Thumbnail, &hash, None, reader(vec![0; thumbnail_limit as usize + 1])).await, Err(AssetError::TooLarge { .. })));

        let first = jpeg([1, 2, 3]);
        let second = jpeg([9, 8, 7]);
        store.put_thumbnail_pair("alice", &hash, &first, &first).await.unwrap();
        let first_revision = store.thumbnail_revision("alice", &hash).await.unwrap().unwrap();
        store.put_thumbnail_pair("alice", &hash, &second, &first).await.unwrap();
        assert_ne!(store.thumbnail_revision("alice", &hash).await.unwrap(), Some(first_revision));
        store.put_thumbnail_pair("bob", &hash, &second, &second).await.unwrap();

        for (user, expected) in [("alice", &second), ("bob", &second)] {
            let mut stored = store.open_from(user, AssetKind::Thumbnail, &hash, 0).await.unwrap().unwrap();
            let mut actual = Vec::new();
            stored.reader.read_to_end(&mut actual).await.unwrap();
            assert_eq!(&actual, expected, "each account has its own latest cover bytes");
        }
        store.put("alice", AssetKind::ThumbnailBrowse, &hash, None, reader(first.clone())).await.unwrap();
        store.remove_thumbnail("alice", &hash).await.unwrap();
        store.remove_thumbnail("alice", &hash).await.unwrap();
        assert!(store.size("alice", AssetKind::Thumbnail, &hash).await.unwrap().is_none());
        assert!(store.thumbnail_revision("alice", &hash).await.unwrap().is_none());
        assert!(store.size("alice", AssetKind::ThumbnailBrowse, &hash).await.unwrap().is_none());
        assert!(store.size("bob", AssetKind::Thumbnail, &hash).await.unwrap().is_some());

        // The server intentionally does not inspect cover contents. Clients
        // validate and decode within their own limits before display.
        let opaque_hash = BookId::new(&"b".repeat(64));
        let opaque = b"arbitrary private client bytes".to_vec();
        store.put("alice", AssetKind::Thumbnail, &opaque_hash, None, reader(opaque.clone())).await.unwrap();
        let mut stored = store.open_from("alice", AssetKind::Thumbnail, &opaque_hash, 0).await.unwrap().unwrap();
        let mut actual = Vec::new();
        stored.reader.read_to_end(&mut actual).await.unwrap();
        assert_eq!(actual, opaque);
    }
}
