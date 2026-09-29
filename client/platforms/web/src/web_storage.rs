//! Browser SQLite VFS bootstrap. All higher-level behavior remains in the
//! regular backend; this module only installs durable browser file semantics.

use sqlite_wasm_vfs::sahpool::{install, OpfsSAHError, OpfsSAHPoolCfgBuilder, OpfsSAHPoolUtil};
use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
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
/// Adopt immutable bytes without copying them through the database worker.
/// The job owns uncommitted files, so an interrupted inspection preserves them.
pub async fn adopt_staged(physical: String, length: u64) -> Result<FileWriter, String> {
    let source = prepare_physical(physical.clone()).await?;
    let actual = source.length;
    if actual != length {
        return Err("Staged file size changed".into());
    }
    Ok(FileWriter { name: "unpublished".into(), physical, length, committed: false, owned: false, _source: source })
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
    static READERS: RefCell<std::collections::HashMap<String, ReaderState>> = RefCell::new(std::collections::HashMap::new());
    static ASSET_INDEX: RefCell<Option<rusqlite::Connection>> = const { RefCell::new(None) };
    static STORAGE: RefCell<Option<Rc<OpfsSAHPoolUtil>>> = const { RefCell::new(None) };
}

pub async fn initialize() -> Result<(), String> {
    let config = OpfsSAHPoolCfgBuilder::new().vfs_name(VFS_NAME).directory(VFS_DIRECTORY).clear_on_init(false).build();
    let storage = install::<rusqlite::ffi::WasmOsCallback>(&config, true).await.map_err(storage_error)?;
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
        client_platform_runtime::executor::sleep(std::time::Duration::ZERO).await;
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
        client_platform_runtime::executor::sleep(std::time::Duration::ZERO).await;
    }
    Ok(())
}

pub fn is_thumbnail(name: &str) -> bool {
    matches!(name.split('/').collect::<Vec<_>>().as_slice(), ["assets", _, "thumbnail", _])
}

pub fn database_exists(name: &str) -> Result<bool, String> {
    storage()?.exists(name).map_err(storage_error)
}

pub async fn reserve_database_capacity(additional_databases: usize) -> Result<(), String> {
    let storage = storage()?;
    let additional_databases = u32::try_from(additional_databases).unwrap_or(u32::MAX);
    let required_capacity = storage.count().saturating_add(additional_databases).saturating_add(TRANSIENT_FILE_CAPACITY);
    storage.reserve_minimum_capacity(required_capacity).await.map_err(|error| error.to_string())
}

fn storage_error(error: OpfsSAHError) -> String {
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
    ASSET_INDEX.with(|slot| {
        let index = slot.borrow();
        operation(index.as_ref().ok_or("browser asset index has not been initialized")?).map_err(|error| error.to_string())
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
    owned: bool,
    _source: FileReader,
}

pub async fn begin_write(name: &str) -> Result<FileWriter, String> {
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

async fn create_asset_writer(name: String, physical: String) -> Result<FileWriter, String> {
    // The task retains ownership across cancellation and closes/removes any late handle.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    wasm_bindgen_futures::spawn_local(async move {
        let source = FileReader::new(physical.clone(), 0);
        let writer = FileWriter { name, physical, length: 0, committed: false, owned: true, _source: source };
        let result = create_asset(&writer.physical).await.map_err(js_error).map(|()| writer);
        let _ = sender.send(result);
    });
    receiver.await.map_err(|_| "book creation stopped".to_owned())?
}

/// A reader pins a generation name, so replacing a logical file cannot change
/// the bytes observed by an already-open reader. Access stays in the owner worker.
pub struct FileReader {
    physical: String,
    position: u64,
    length: u64,
}

impl FileReader {
    /// Opens another cursor over the same pinned storage generation.
    pub fn fork(&self) -> Self {
        Self::new(self.physical.clone(), self.length)
    }

    pub fn checksum(&self) -> Result<Option<sync_common::ContentHash>, String> {
        use rusqlite::OptionalExtension;
        let value: Option<String> = with_index(|index| index.query_row(include_str!("sql/web_storage/checksum_select.sql"), [&self.physical], |row| row.get(0)).optional())?;
        Ok(value.and_then(|v| v.parse().ok()))
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
            let state = readers.get_mut(&self.physical).expect("reader generation was registered");
            state.count -= 1;
            if state.count == 0 {
                readers.remove(&self.physical)
            } else {
                None
            }
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
        let count = read_staged(&self.physical, self.position as f64, bytes).map_err(js_error).map_err(std::io::Error::other)?;
        self.position += count as u64;
        Ok(count)
    }
}

impl std::io::Seek for FileReader {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        let position = match from {
            std::io::SeekFrom::Start(position) => Some(position),
            std::io::SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            std::io::SeekFrom::End(offset) => self.length.checked_add_signed(offset),
        }
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid asset seek"))?;
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
    pub fn open_reader(&self) -> FileReader {
        FileReader::new(self.physical.clone(), self.length)
    }

    pub fn publish_as(mut self, name: String) -> Result<(), String> {
        self.name = name;
        self.commit()
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        write_staged(&self.physical, self.length as f64, bytes).map_err(js_error)?;
        self.length = self.length.checked_add(bytes.len() as u64).ok_or("asset length overflow")?;
        Ok(())
    }

    pub fn publish_verified_as(mut self, name: String, checksum: sync_common::ContentHash) -> Result<(), String> {
        self.name = name;
        self.commit_verified(checksum)
    }
    pub fn commit(self) -> Result<(), String> {
        self.commit_revision(None)
    }
    pub fn commit_verified(self, checksum: sync_common::ContentHash) -> Result<(), String> {
        self.commit_revision(Some(checksum))
    }
    fn commit_revision(mut self, checksum: Option<sync_common::ContentHash>) -> Result<(), String> {
        let previous = physical_name(&self.name)?;
        flush_staged(&self.physical).map_err(js_error)?;
        with_index(|index| {
            let tx = index.unchecked_transaction()?;
            super::asset_index::publish(&tx, &self.name, &self.physical, self.length)?;
            if let Some(checksum) = checksum {
                tx.execute(include_str!("sql/web_storage/commit_revision_insert.sql"), rusqlite::params![self.physical, checksum.as_str()])?;
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

impl Drop for FileWriter {
    fn drop(&mut self) {
        if !self.committed && self.owned {
            retire(self.physical.clone());
        }
    }
}

pub async fn write_file(name: &str, bytes: &[u8]) -> Result<(), String> {
    let mut writer = begin_write(name).await?;
    writer.write_all(bytes)?;
    writer.commit()
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

pub fn list_files_page(prefix: &str, after: &str) -> Result<Vec<String>, String> {
    with_index(|index| super::asset_index::page(index, prefix, after)).map(|page| page.into_iter().map(|(name, _)| name).collect())
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

/// Once all outcomes are checkpointed, discard failed/unreferenced copies.
/// Committed assets remain pinned by the durable index.
pub fn cleanup_staged_import(prefix: &str, files: u32) -> Result<(), String> {
    if !prefix.starts_with("__libraries/") || !prefix.ends_with('/') {
        return Err("invalid staged import prefix".into());
    }
    let live = with_index(|index| super::asset_index::live_physical_files(index, &prefix))?;
    for index in 0..files {
        let physical = format!("{prefix}{index}");
        if !live.contains(&physical) {
            retire(physical);
        }
    }
    Ok(())
}
