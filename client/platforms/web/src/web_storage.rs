//! Browser SQLite VFS bootstrap. All higher-level behavior remains in the
//! regular backend; this module only installs durable browser file semantics.

use sqlite_wasm_vfs::sahpool::{OpfsSAHError, OpfsSAHPoolCfgBuilder, OpfsSAHPoolUtil, install};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Error, SeekFrom};
use std::rc::Rc;
use std::time::Duration;

use operation_failure::Failure;
use rusqlite::OptionalExtension;
use sync_common::BookId;
use wasm_bindgen::prelude::*;

#[derive(Debug)]
pub struct StorageError {
    message: String,
    failure: Option<Failure>,
}
impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failure.as_ref().map(|failure| failure as _)
    }
}
impl From<String> for StorageError {
    fn from(message: String) -> Self {
        Self { message, failure: None }
    }
}
impl From<&str> for StorageError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl From<StorageError> for String {
    fn from(error: StorageError) -> Self {
        error.to_string()
    }
}
impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        let failure = match &error {
            rusqlite::Error::SqliteFailure(code, _) => match code.code {
                rusqlite::ErrorCode::DiskFull => Some(Failure::StorageFull),
                rusqlite::ErrorCode::ReadOnly => Some(Failure::ReadOnlyStorage),
                rusqlite::ErrorCode::PermissionDenied => Some(Failure::PermissionDenied),
                _ => None,
            },
            _ => None,
        };
        Self { message: error.to_string(), failure }
    }
}

fn storage_js_error(error: JsValue) -> StorageError {
    let name = js_sys::Reflect::get(&error, &"name".into()).ok().and_then(|name| name.as_string());
    let failure = match name.as_deref() {
        Some("QuotaExceededError") => Some(Failure::StorageFull),
        Some("NotAllowedError" | "SecurityError") => Some(Failure::PermissionDenied),
        Some("NoModificationAllowedError") => Some(Failure::StorageBusy),
        _ => None,
    };
    StorageError { message: js_error(error), failure }
}
#[wasm_bindgen(module = "/src/staged_assets.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = removeAsset)]
    async fn remove_asset(name: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(catch, js_name = removeLibrary)]
    async fn remove_library(library: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(catch, js_name = listAssets)]
    async fn list_assets() -> Result<js_sys::Array, JsValue>;
    #[wasm_bindgen(catch, js_name = createAsset)]
    async fn create_asset(name: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(catch, js_name = writeStaged)]
    fn write_staged(name: &str, offset: f64, bytes: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(catch, js_name = flushStaged)]
    fn flush_staged(name: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(catch, js_name = stagedBlob)]
    async fn staged_blob(name: &str) -> Result<web_sys::Blob, JsValue>;
    #[wasm_bindgen(catch, js_name = openStaged)]
    async fn open_staged(name: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = closeStaged)]
    fn close_staged(name: &str);
    #[wasm_bindgen(catch, js_name = readStaged)]
    fn read_staged(name: &str, offset: f64, bytes: &mut [u8]) -> Result<usize, JsValue>;
    #[wasm_bindgen(js_name = retireStaged)]
    fn retire_staged(name: &str);
}
fn js_error(error: JsValue) -> String {
    javascript_error_detail(&error).unwrap_or_else(|| format!("{error:?}"))
}
const VFS_NAME: &str = "bokheim-opfs-sahpool-v1";
const VFS_DIRECTORY: &str = ".bokheim-opfs-sahpool-v1";
const TRANSIENT_FILE_CAPACITY: u32 = 8;

#[derive(Default)]
struct ReaderState {
    count: usize,
    delete_on_close: bool,
}

thread_local! {
    static READERS: RefCell<HashMap<String, ReaderState>> = RefCell::new(HashMap::new());
    static ASSET_INDEX: RefCell<Option<rusqlite::Connection>> = const { RefCell::new(None) };
    static STORAGE: RefCell<Option<Rc<OpfsSAHPoolUtil>>> = const { RefCell::new(None) };
}

/// Why browser storage could not be opened.
#[derive(Debug)]
pub enum StorageInitError {
    /// Another tab holds the storage's file handles.
    Busy(String),
    Failed(String),
}

impl From<String> for StorageInitError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl std::fmt::Display for StorageInitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(error) | Self::Failed(error) => formatter.write_str(error),
        }
    }
}

pub async fn initialize() -> Result<(), StorageInitError> {
    let config = OpfsSAHPoolCfgBuilder::new().vfs_name(VFS_NAME).directory(VFS_DIRECTORY).clear_on_init(false).build();
    let storage = install::<rusqlite::ffi::WasmOsCallback>(&config, true).await.map_err(|error| {
        // A sync access handle cannot be created while another tab holds it.
        let busy = matches!(error, OpfsSAHError::CreateSyncAccessHandle(_));
        let error = storage_error(error);
        if busy { StorageInitError::Busy(error) } else { StorageInitError::Failed(error) }
    })?;
    super::reference_storage::recover(&storage)?;
    // SQLite can synchronously create short-lived journal files. Keep eight
    // unused handles available without permanently growing the pool on every
    // launch, and clean up surplus handles left by older builds.
    let required_capacity = storage.count().saturating_add(TRANSIENT_FILE_CAPACITY + 1);
    storage.reserve_minimum_capacity(required_capacity).await.map_err(|error| error.to_string())?;
    STORAGE.with(|slot| *slot.borrow_mut() = Some(Rc::new(storage)));
    let index = rusqlite::Connection::open("__asset_index_v1.sqlite3").map_err(|error| error.to_string())?;
    super::asset_index::initialize(&index).map_err(|error| error.to_string())?;
    ASSET_INDEX.with(|slot| *slot.borrow_mut() = Some(index));
    // Initialization must not wait for a library-wide object sweep.
    client_platform_runtime::executor::spawn_detached(async {
        if let Err(error) = client_platform_runtime::executor::sleep(Duration::ZERO).await {
            log::warn!("Object reclamation could not be scheduled: {error}");
            return;
        }
        if let Err(error) = reclaim_unreferenced_objects().await {
            log::warn!("Object reclamation will retry on next startup: {error}");
        }
    });
    let storage = self::storage()?;
    let surplus = storage.get_capacity().saturating_sub(storage.count().saturating_add(TRANSIENT_FILE_CAPACITY));
    if surplus != 0 {
        storage.reduce_capacity(surplus).await.map_err(storage_error)?;
    }
    Ok(())
}

async fn reclaim_unreferenced_objects() -> Result<(), String> {
    for value in list_assets().await.map_err(js_error)?.iter() {
        let name = value.as_string().ok_or("Invalid book object name")?;
        // Recheck current ownership after enumeration: imports and readers may
        // have started since the sweep began. Generations are never reused.
        let pinned = READERS.with(|readers| readers.borrow().contains_key(&name));
        let published = with_index(|index| index.query_row(include_str!("sql/web_storage/asset_lifecycle_contract_select.sql"), [&name], |row| row.get::<_, bool>(0)))?;
        if !pinned && !published {
            remove_asset(&name).await.map_err(js_error)?;
        }
        client_platform_runtime::executor::sleep(Duration::ZERO).await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub fn database_exists(name: &str) -> Result<bool, String> {
    storage()?.exists(name).map_err(storage_error)
}

/// Bulk-install a closed, disposable reference cache, never a user database.
/// A pending pool entry lets the next startup discard an interrupted import.
pub fn import_reference_database(name: &str, image: &[u8]) -> Result<(), String> {
    let storage = storage()?;
    super::reference_storage::import(&storage, name, image)
}

pub fn reference_database(name: &str) -> Result<Option<String>, String> {
    with_index(|index| index.query_row("SELECT filename FROM reference_selection WHERE name=?1", [name], |row| row.get(0)).optional())
}

/// Publish a completed immutable file for the next startup. Open readers keep
/// their original file; publication never replaces an active reference.
pub fn publish_reference_database(name: &str, filename: &str) -> Result<(), String> {
    with_index(|index| index.execute("INSERT INTO reference_selection(name,filename) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET filename=excluded.filename WHERE filename<>excluded.filename", [name, filename]).map(|_| ()))
}

/// Call only during startup, before any library opens an attached reference.
pub fn prune_reference_databases(legacy: &str, prefix: &str, selected: &str) -> Result<(), String> {
    let storage = storage()?;
    super::reference_storage::prune(&storage, legacy, prefix, selected)
}

pub async fn reserve_database_capacity(additional_databases: usize) -> Result<(), String> {
    let storage = storage()?;
    let additional_databases = u32::try_from(additional_databases).unwrap_or(u32::MAX);
    let required_capacity = storage.count().saturating_add(additional_databases).saturating_add(TRANSIENT_FILE_CAPACITY);
    storage.reserve_minimum_capacity(required_capacity).await.map_err(|error| error.to_string())
}

pub(super) fn storage_error(error: OpfsSAHError) -> String {
    let detail = match &error {
        OpfsSAHError::GetDirHandle(value)
        | OpfsSAHError::GetFileHandle(value)
        | OpfsSAHError::CreateSyncAccessHandle(value)
        | OpfsSAHError::IterHandle(value)
        | OpfsSAHError::GetPath(value)
        | OpfsSAHError::RemoveEntity(value)
        | OpfsSAHError::GetSize(value)
        | OpfsSAHError::Read(value)
        | OpfsSAHError::Write(value)
        | OpfsSAHError::Flush(value)
        | OpfsSAHError::Truncate(value)
        | OpfsSAHError::Reflect(value) => javascript_error_detail(value),
        _ => None,
    };
    detail.map_or_else(|| error.to_string(), |detail| format!("{error}: {detail}"))
}

fn javascript_error_detail(value: &wasm_bindgen::JsValue) -> Option<String> {
    let property = |name| js_sys::Reflect::get(value, &wasm_bindgen::JsValue::from_str(name)).ok()?.as_string();
    let name = property("name").filter(|name| !name.is_empty());
    let message = property("message").filter(|message| !message.is_empty());
    match (name, message) {
        (Some(name), Some(message)) => Some(format!("{name}: {message}")),
        (Some(name), None) => Some(name),
        (None, Some(message)) => Some(message),
        (None, None) => value.as_string(),
    }
}

fn storage() -> Result<Rc<OpfsSAHPoolUtil>, String> {
    STORAGE.with(|slot| slot.borrow().clone().ok_or_else(|| "browser storage has not been initialized".to_owned()))
}

fn with_index<T>(operation: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>) -> Result<T, String> {
    with_index_typed(operation).map_err(Into::into)
}

fn with_index_typed<T>(operation: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>) -> Result<T, StorageError> {
    ASSET_INDEX.with(|slot| {
        let index = slot.borrow();
        operation(index.as_ref().ok_or("browser asset index has not been initialized")?).map_err(Into::into)
    })
}

fn physical_name(name: &str) -> Result<Option<String>, String> {
    use rusqlite::OptionalExtension;
    with_index(|index| index.query_row(include_str!("sql/web_storage/physical_name_select.sql"), [name], |row| row.get(0)).optional())
}

pub async fn read_file(name: &str) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let Some(mut reader) = open_reader(name).await? else { return Ok(None) };
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(Some(bytes))
}

/// A private generation. Only the SQLite index commit makes it visible.
/// Replaced generations survive until their last reader closes; bootstrap
/// discards abandoned generations left by interrupted writes.
pub struct FileWriter {
    name: String,
    physical: String,
    length: u64,
    committed: bool,
    _source: FileReader,
}

pub async fn begin_write(name: &str) -> Result<FileWriter, StorageError> {
    let mut parts = name.split('/');
    let (Some("assets"), Some(library), Some(kind), Some(_asset), None) = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err("Invalid library asset name".into());
    };
    let library = uuid::Uuid::parse_str(library).map_err(|_| "Invalid library asset owner")?;
    if !matches!(kind, "book" | "thumbnail") {
        return Err("Invalid library asset kind".into());
    }
    create_asset_writer(name.to_owned(), format!("__libraries/{library}/{kind}/{}", uuid::Uuid::new_v4())).await
}

async fn create_asset_writer(name: String, physical: String) -> Result<FileWriter, StorageError> {
    // The task retains ownership across cancellation and closes/removes any late handle.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    wasm_bindgen_futures::spawn_local(async move {
        let source = FileReader::new(physical.clone(), 0);
        let writer = FileWriter { name, physical, length: 0, committed: false, _source: source };
        let result = create_asset(&writer.physical).await.map_err(storage_js_error).map(|()| writer);
        let _ = sender.send(result);
    });
    receiver.await.map_err(|_| StorageError::from("book creation stopped"))?
}

/// A reader pins a generation name, so replacing a logical file cannot change
/// the bytes observed by an already-open reader. Access stays in the owner worker.
pub struct FileReader {
    physical: String,
    position: u64,
    length: u64,
}

impl FileReader {
    pub fn content_hash(&self) -> Result<Option<BookId>, String> {
        use rusqlite::OptionalExtension;
        let value: Option<BookId> = with_index(|index| index.query_row(include_str!("sql/web_storage/content_hash_select.sql"), [&self.physical], |row| row.get(0)).optional())?;
        Ok(value)
    }

    pub async fn upload_file(&self) -> Result<web_sys::Blob, String> {
        staged_blob(&self.physical).await.map_err(js_error)
    }

    fn new(physical: String, length: u64) -> Self {
        READERS.with(|readers| readers.borrow_mut().entry(physical.clone()).or_default().count += 1);
        Self { physical, position: 0, length }
    }
}

fn retire(physical: String) {
    let deferred = READERS.with(|readers| {
        if let Some(state) = readers.borrow_mut().get_mut(&physical) {
            state.delete_on_close = true;
            true
        } else {
            false
        }
    });
    if !deferred {
        // Book already succeeded; bootstrap retries failed cleanup.
        retire_staged(&physical);
    }
}

impl Drop for FileReader {
    fn drop(&mut self) {
        let finished = READERS.with(|readers| {
            let mut readers = readers.borrow_mut();
            let Some(state) = readers.get_mut(&self.physical) else {
                log::warn!("book reader cleanup found no registered generation: {}", self.physical);
                return None;
            };
            let Some(count) = state.count.checked_sub(1) else {
                log::warn!("book reader cleanup found an empty generation: {}", self.physical);
                return readers.remove(&self.physical);
            };
            state.count = count;
            if state.count == 0 { readers.remove(&self.physical) } else { None }
        });
        if let Some(state) = finished {
            if state.delete_on_close {
                retire_staged(&self.physical);
            } else {
                close_staged(&self.physical);
            }
        }
    }
}

impl std::io::Read for FileReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = read_staged(&self.physical, self.position as f64, bytes).map_err(js_error).map_err(Error::other)?;
        self.position += count as u64;
        Ok(count)
    }
}

impl std::io::Seek for FileReader {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let position = match from {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.length.checked_add_signed(offset),
        }
        .ok_or_else(|| Error::new(std::io::ErrorKind::InvalidInput, "invalid asset seek"))?;
        self.position = position;
        Ok(position)
    }
}

// Pin the generation before awaiting OPFS. The task owns the pin until opening
// completes, even if its caller is cancelled; a late handle cannot leak or race
// physical retirement.
async fn prepare_physical(physical: String) -> Result<FileReader, String> {
    let mut reader = FileReader::new(physical, 0);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    wasm_bindgen_futures::spawn_local(async move {
        let result = async {
            reader.length = open_staged(&reader.physical).await.map_err(js_error)?.as_f64().ok_or("Invalid staged file size")? as u64;
            Ok(reader)
        }
        .await;
        let _ = sender.send(result);
    });
    receiver.await.map_err(|_| "file preparation stopped".to_owned())?
}

pub async fn open_reader(name: &str) -> Result<Option<FileReader>, String> {
    let Some(physical) = physical_name(name)? else { return Ok(None) };
    prepare_physical(physical).await.map(Some)
}

impl FileWriter {
    /// The durable import manifest and its flushed source appear together.
    /// A repeated import never overwrites a previously retained generation.
    pub fn retain_provider_upload(mut self, name: String, checksum: BookId, metadata: Vec<u8>) -> Result<(), StorageError> {
        use rusqlite::OptionalExtension;
        if metadata.len() > 8 * 1024 * 1024 {
            return Err("pending import metadata is too large".into());
        }
        flush_staged(&self.physical).map_err(storage_js_error)?;
        let created = with_index_typed(|index| {
            let tx = index.unchecked_transaction()?;
            let previous: Option<(Vec<u8>, BookId, i64)> = tx
                .query_row("SELECT p.metadata,r.content_hash,f.byte_length FROM retained_provider_upload p JOIN asset_file f USING(name) JOIN asset_revision r ON r.physical_name=f.physical_name WHERE p.name=?1", [&name], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .optional()?;
            if let Some((previous, hash, size)) = previous {
                if previous != metadata || hash != checksum || u64::try_from(size).ok() != Some(self.length) {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                return Ok(false);
            }
            super::asset_index::publish(&tx, &name, &self.physical, self.length)?;
            tx.execute(include_str!("sql/web_storage/commit_revision_insert.sql"), rusqlite::params![self.physical, checksum])?;
            tx.execute("INSERT INTO retained_provider_upload(name,metadata) VALUES(?1,?2)", rusqlite::params![name, metadata])?;
            tx.commit()?;
            Ok(true)
        })?;
        self.committed = created;
        Ok(())
    }
    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn open_reader(&self) -> FileReader {
        FileReader::new(self.physical.clone(), self.length)
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), StorageError> {
        write_staged(&self.physical, self.length as f64, bytes).map_err(storage_js_error)?;
        self.length = self.length.checked_add(bytes.len() as u64).ok_or("asset length overflow")?;
        Ok(())
    }

    pub fn publish_verified_as(mut self, name: String, content_hash: BookId) -> Result<(), StorageError> {
        self.name = name;
        self.commit_verified(content_hash)
    }
    /// Retain the published physical generation until the owning database has
    /// accepted or rejected it. Rejection cannot remove a later replacement.
    pub fn publish_pinned_as(self, name: String, content_hash: BookId) -> Result<PublishedFile, StorageError> {
        let reader = self.open_reader();
        self.publish_verified_as(name.clone(), content_hash)?;
        Ok(PublishedFile { name, reader })
    }
    pub fn commit(self) -> Result<(), StorageError> {
        self.commit_revision(None)
    }
    pub fn commit_verified(self, content_hash: BookId) -> Result<(), StorageError> {
        self.commit_revision(Some(content_hash))
    }
    fn commit_revision(mut self, content_hash: Option<BookId>) -> Result<(), StorageError> {
        let previous = physical_name(&self.name)?;
        flush_staged(&self.physical).map_err(storage_js_error)?;
        with_index_typed(|index| {
            let tx = index.unchecked_transaction()?;
            super::asset_index::publish(&tx, &self.name, &self.physical, self.length)?;
            if let Some(content_hash) = content_hash {
                tx.execute(include_str!("sql/web_storage/commit_revision_insert.sql"), rusqlite::params![self.physical, content_hash])?;
            }
            tx.execute(include_str!("sql/web_storage/commit_revision_delete.sql"), [])?;
            tx.commit()
        })?;
        self.committed = true;
        if let Some(previous) = previous.filter(|previous| previous != &self.physical) {
            retire(previous);
        }
        Ok(())
    }
}

pub fn retained_provider_uploads(prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StorageError> {
    with_index_typed(|index| {
        let mut query = index.prepare("SELECT name,metadata FROM retained_provider_upload WHERE substr(name,1,length(?1))=?1 ORDER BY name")?;
        let rows = query.query_map([prefix], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    })
}

/// Compare the previous manifest before replacing reader state for a retry.
pub fn replace_provider_upload_metadata(name: &str, previous: &[u8], metadata: &[u8]) -> Result<(), StorageError> {
    if metadata.len() > 8 * 1024 * 1024 {
        return Err("pending import metadata is too large".into());
    }
    with_index_typed(|index| {
        let changed = index.execute("UPDATE retained_provider_upload SET metadata=?1 WHERE name=?2 AND metadata=?3", rusqlite::params![metadata, name, previous])?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    })
}

/// Reuse the retained immutable generation as the confirmed provider's cache.
/// This needs no second book-sized OPFS allocation. If the library database
/// commit is interrupted, the upload queue can acknowledge this cache on retry.
pub fn promote_provider_upload(name: &str, destination: &str, checksum: BookId, book: BookId, length: u64) -> Result<bool, StorageError> {
    use rusqlite::OptionalExtension;
    if name.rsplit_once('/').map(|(prefix, _)| prefix) != destination.rsplit_once('/').map(|(prefix, _)| prefix) {
        return Err("provider upload belongs to another library".into());
    }
    let previous = physical_name(destination)?;
    let (promoted, physical) = with_index_typed(|index| {
        let tx = index.unchecked_transaction()?;
        let lookup = |name: &str| {
            tx.query_row("SELECT f.physical_name,r.content_hash,f.byte_length FROM asset_file f JOIN asset_revision r ON r.physical_name=f.physical_name WHERE name=?1", [name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, BookId>(1)?, row.get::<_, i64>(2)?))
            })
            .optional()
        };
        let Some((physical, hash, size)) = lookup(name)? else {
            let present = lookup(destination)?.is_some_and(|(_, hash, size)| hash == book && u64::try_from(size).ok() == Some(length));
            return Ok((present, None));
        };
        if hash != checksum || u64::try_from(size).ok() != Some(length) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        super::asset_index::publish(&tx, destination, &physical, length)?;
        tx.execute("DELETE FROM asset_file WHERE name=?1", [name])?;
        tx.execute("DELETE FROM retained_provider_upload WHERE name=?1", [name])?;
        tx.execute(include_str!("sql/web_storage/commit_revision_insert.sql"), rusqlite::params![physical, book])?;
        tx.commit()?;
        Ok((true, Some(physical)))
    })?;
    if let Some(physical) = physical {
        if let Some(previous) = previous.filter(|previous| previous != &physical) {
            retire(previous);
        }
    }
    Ok(promoted)
}

pub fn remove_provider_upload(name: &str) -> Result<(), StorageError> {
    let previous = physical_name(name)?;
    with_index_typed(|index| {
        let tx = index.unchecked_transaction()?;
        tx.execute("DELETE FROM asset_file WHERE name=?1", [name])?;
        tx.execute("DELETE FROM retained_provider_upload WHERE name=?1", [name])?;
        tx.commit()
    })?;
    if let Some(previous) = previous {
        retire(previous);
    }
    Ok(())
}

pub struct PublishedFile {
    name: String,
    reader: FileReader,
}
impl PublishedFile {
    pub fn discard(self) -> Result<(), String> {
        let removed = with_index(|index| index.execute(include_str!("sql/web_storage/discard_publication.sql"), rusqlite::params![self.name, self.reader.physical]))?;
        if removed != 0 {
            retire(self.reader.physical.clone());
        }
        Ok(())
    }
}

impl Drop for FileWriter {
    fn drop(&mut self) {
        if !self.committed {
            retire(self.physical.clone());
        }
    }
}

pub async fn write_file(name: &str, bytes: &[u8]) -> Result<(), String> {
    let mut writer = begin_write(name).await?;
    writer.write_all(bytes)?;
    writer.commit().map_err(Into::into)
}

pub fn remove_file(name: &str) -> Result<bool, String> {
    let previous = physical_name(name)?;
    let existed = previous.is_some();
    // Publish the deletion in the durable index.
    with_index(|index| index.execute(include_str!("sql/web_storage/remove_file_delete.sql"), [name]))?;
    if let Some(previous) = previous {
        retire(previous);
    }
    Ok(existed)
}

pub fn file_exists(name: &str) -> Result<bool, String> {
    // Book flushes bytes before committing the index entry.
    Ok(physical_name(name)?.is_some())
}

pub async fn total_file_bytes(prefix: &str) -> Result<u64, String> {
    let prefix = prefix.to_owned();
    client_platform_runtime::storage_queue::run(client_platform_runtime::storage_queue::Priority::Background, move || with_index(|index| super::asset_index::total_bytes(index, &prefix))).await?
}

/// Retire one library's physical OPFS container and its global index entries.
/// The caller has already detached the library from the registry and stopped
/// its actor. A failed removal leaves the durable purge record for retry.
pub async fn purge_library(library: sync_common::LibraryId) -> Result<(), String> {
    let physical_prefix = format!("__libraries/{library}/");
    if READERS.with(|readers| readers.borrow().keys().any(|name| name.starts_with(&physical_prefix))) {
        return Err(format!("library {library} still has open asset readers"));
    }
    remove_library(&library.to_string()).await.map_err(js_error)?;
    with_index(|index| {
        let tx = index.unchecked_transaction()?;
        super::asset_index::purge_library(&tx, &library.to_string())?;
        tx.commit()
    })
}
