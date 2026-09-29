//! SQLite-backed device registry of library storage roots.
//!
//! Registry and key/value behavior is identical on every target. Native code
//! supplies real directory roots; the browser supplies SQLite VFS filenames.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, MutexGuard};
use sync_common::LibraryId;

const DATABASE_FILENAME: &str = "app-state-v1.sqlite3";

const SCHEMA: &str = r#"
    CREATE TABLE IF NOT EXISTS library_registry (
        library_id TEXT PRIMARY KEY NOT NULL,
        library_name TEXT NOT NULL CHECK(length(trim(library_name)) BETWEEN 1 AND 255),
        storage_path TEXT NOT NULL UNIQUE
    ) STRICT;
    CREATE TABLE IF NOT EXISTS detached_library (
        library_id TEXT PRIMARY KEY NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS pending_local_purge (
        library_id TEXT PRIMARY KEY NOT NULL,
        storage_path TEXT NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS app_value (
        key TEXT PRIMARY KEY NOT NULL,
        value BLOB NOT NULL
    ) STRICT;
"#;

pub type RegistryResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LibraryEntry {
    library_id: LibraryId,
    library_name: String,
    storage_locator: String,
}

impl LibraryEntry {
    fn new(library_id: LibraryId, name: &str, storage_locator: impl Into<String>) -> RegistryResult<Self> {
        Ok(Self { library_id, library_name: validate_name(name)?, storage_locator: storage_locator.into() })
    }

    pub fn library_id(&self) -> &LibraryId {
        &self.library_id
    }
    pub fn library_name(&self) -> &str {
        &self.library_name
    }
    pub fn storage_locator(&self) -> &str {
        &self.storage_locator
    }
}

#[derive(Clone)]
pub struct LibraryRegistry {
    app_data_locator: String,
    default_library_root: String,
    connection: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for LibraryRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("LibraryRegistry").field("default_library_root", &self.default_library_root).finish_non_exhaustive()
    }
}

impl LibraryRegistry {
    pub fn open(app_data_locator: impl Into<String>) -> RegistryResult<Self> {
        let app_data_locator = app_data_locator.into();
        let library_root = join_locator(&app_data_locator, "libraries");
        Self::open_with_library_root(app_data_locator, library_root)
    }

    pub fn open_with_library_root(app_data_locator: impl Into<String>, default_library_root: impl Into<String>) -> RegistryResult<Self> {
        let app_data_locator = app_data_locator.into();
        let default_library_root = default_library_root.into();
        let database_locator = Self::database_locator(&app_data_locator);
        let connection = device_sqlite::open(database_locator, device_sqlite::OpenOptions::new().busy_timeout(std::time::Duration::from_secs(5)))?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self { app_data_locator, default_library_root, connection: Arc::new(Mutex::new(connection)) })
    }

    /// Actual registry filename in the selected platform storage namespace.
    pub fn database_locator(app_data_locator: &str) -> String {
        join_locator(app_data_locator, DATABASE_FILENAME)
    }

    /// Native databases belong to the device, independently of the content folder.
    pub fn library_database_locator(&self, id: &LibraryId, content_locator: &str) -> String {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = content_locator;
            join_locator(&self.app_data_locator, &format!("libraries/{id}/library.db"))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = id;
            content_locator.to_owned()
        }
    }

    fn connection(&self) -> RegistryResult<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| "library registry lock is poisoned".into())
    }

    pub fn entries(&self) -> RegistryResult<Vec<LibraryEntry>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT library_id,library_name,storage_path FROM library_registry ORDER BY library_name COLLATE NOCASE,library_id")?;
        let entries = statement
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
            .map(|row| {
                let (id, name, path) = row?;
                LibraryEntry::new(LibraryId::parse_str(&id)?, &name, path)
            })
            .collect();
        entries
    }

    pub fn entry(&self, id: &LibraryId) -> RegistryResult<Option<LibraryEntry>> {
        let row = self.connection()?.query_row("SELECT library_name,storage_path FROM library_registry WHERE library_id=?1", [id.to_string()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).optional()?;
        row.map(|(name, path)| LibraryEntry::new(*id, &name, path)).transpose()
    }

    // Keep the original device key so an earlier creation choice is preserved.
    // The choice now affects asset uploads only, never metadata sync.
    pub fn asset_storage_enabled(&self, id: &LibraryId) -> RegistryResult<bool> {
        Ok(self.read_value(&format!("library_sync_disabled:{id}"))?.as_deref() != Some(b"1"))
    }

    pub fn set_asset_storage_enabled(&self, id: &LibraryId, enabled: bool) -> RegistryResult<()> {
        if self.entry(id)?.is_none() {
            return Err("library not found".into());
        }
        self.write_value(&format!("library_sync_disabled:{id}"), if enabled { b"0" } else { b"1" })
    }

    pub fn cache_cloud_storage_with_reset(&self, id: &LibraryId, enabled: bool) -> RegistryResult<()> {
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        tx.execute(include_str!("sql/set_sync_disabled.sql"), params![format!("library_sync_disabled:{id}"), if enabled { b"0".as_slice() } else { b"1".as_slice() }])?;
        tx.execute(include_str!("sql/set_sync_disabled.sql"), params![format!("cloud-storage-reset:{id}"), uuid::Uuid::new_v4().as_bytes().as_slice()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn clear_value_if_current(&self, key: &str, value: &[u8]) -> RegistryResult<()> {
        self.connection()?.execute(include_str!("sql/clear_value_if_current.sql"), params![key, value])?;
        Ok(())
    }

    /// Save the local cache and an idempotent account write together.
    pub fn queue_cloud_storage(&self, id: &LibraryId, enabled: bool) -> RegistryResult<()> {
        if self.entry(id)?.is_none() {
            return Err("library not found".into());
        }
        let pending = format!("{}:{}", uuid::Uuid::new_v4(), u8::from(enabled));
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        tx.execute(include_str!("sql/set_sync_disabled.sql"), params![format!("library_sync_disabled:{id}"), if enabled { b"0".as_slice() } else { b"1".as_slice() }])?;
        tx.execute(include_str!("sql/set_sync_disabled.sql"), params![format!("pending-cloud-storage:{id}"), pending.as_bytes()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn create(&self, name: &str, locator: Option<String>) -> RegistryResult<LibraryEntry> {
        self.create_with_asset_storage(name, locator, true)
    }

    /// Persist the policy before the new library becomes visible to background work.
    pub fn create_with_asset_storage(&self, name: &str, locator: Option<String>, asset_storage_enabled: bool) -> RegistryResult<LibraryEntry> {
        let generated_id = LibraryId::new_v4();
        let requested_name = if name.trim().is_empty() { "Library" } else { name };
        let requested_locator = match locator {
            Some(locator) => locator,
            None => platform::allocate_library_location(&self.effective_library_root()?, &generated_id, requested_name)?,
        };
        #[cfg(not(target_arch = "wasm32"))]
        let pending_locator = std::path::Path::new(&requested_locator).canonicalize().unwrap_or_else(|_| std::path::PathBuf::from(&requested_locator)).to_string_lossy().into_owned();
        #[cfg(target_arch = "wasm32")]
        let pending_locator = requested_locator.clone();
        let pending: bool = self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM pending_local_purge WHERE storage_path=?1)", [&pending_locator], |row| row.get(0))?;
        if pending {
            return Err("library location is pending deletion".into());
        }
        let entry = platform::prepare_created_library(generated_id, requested_name, requested_locator)?;
        if self.is_detached(entry.library_id())? {
            return Err("library is pending deletion".into());
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction()?;
        tx.execute(include_str!("sql/set_sync_disabled.sql"), params![format!("library_sync_disabled:{}", entry.library_id), if asset_storage_enabled { b"0".as_slice() } else { b"1".as_slice() }])?;
        tx.execute(include_str!("sql/create_library.sql"), params![entry.library_id.to_string(), entry.library_name, entry.storage_locator])?;
        if !asset_storage_enabled {
            let pending = format!("{}:0", uuid::Uuid::new_v4());
            tx.execute(include_str!("sql/set_sync_disabled.sql"), params![format!("pending-cloud-storage:{}", entry.library_id), pending.as_bytes()])?;
        }
        tx.commit()?;
        Ok(entry)
    }

    pub fn attach_remote(&self, id: &LibraryId, name: &str) -> RegistryResult<LibraryEntry> {
        // An existing missing root may have moved. Account reconciliation must
        // not recreate it and prevent relocation from recognizing the move.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(existing) = self.entry(id)? {
            let entry = LibraryEntry::new(*id, name, existing.storage_locator())?;
            if read_library_manifest(std::path::Path::new(existing.storage_locator())).ok().flatten().is_some_and(|(found, _)| found == *id) {
                platform::write_manifest(&entry)?;
            }
            if existing.library_name() != entry.library_name() {
                self.connection()?.execute(include_str!("sql/update_library_name.sql"), params![id.to_string(), entry.library_name])?;
            }
            return Ok(entry);
        }
        let existing_locator = self.entry(id)?.map(|entry| entry.storage_locator);
        let requested_locator = match existing_locator {
            Some(locator) => locator,
            None => platform::allocate_library_location(&self.effective_library_root()?, id, name)?,
        };
        let entry = platform::prepare_attached_library(*id, name, requested_locator)?;
        self.connection()?.execute(
            "INSERT INTO library_registry(library_id,library_name,storage_path) VALUES(?1,?2,?3) ON CONFLICT(library_id) DO UPDATE SET library_name=excluded.library_name",
            params![id.to_string(), entry.library_name, entry.storage_locator],
        )?;
        Ok(entry)
    }

    /// Compare-and-swap a missing root after validating the on-disk identity.
    /// Never creates a library or replaces an independently selected location.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn relocate(&self, id: &LibraryId, previous: &str, destination: &std::path::Path) -> RegistryResult<bool> {
        // An occupied pathname is not proof that this library is still there.
        if std::path::Path::new(previous).is_dir() && read_library_manifest(std::path::Path::new(previous))?.is_some_and(|(found, _)| found == *id) {
            return Ok(false);
        }
        let destination = destination.canonicalize()?;
        if !destination.is_dir() || read_library_manifest(&destination)?.is_none_or(|(found, _)| found != *id) {
            return Err("directory does not belong to this library".into());
        }
        Ok(self.connection()?.execute("UPDATE library_registry SET storage_path=?3 WHERE library_id=?1 AND storage_path=?2", params![id.to_string(), previous, destination.to_string_lossy()])? == 1)
    }

    fn effective_library_root(&self) -> RegistryResult<String> {
        self.read_value("default_library_save_location")?.map(|bytes| String::from_utf8(bytes).map_err(Into::into)).unwrap_or_else(|| Ok(self.default_library_root.clone()))
    }

    pub fn default_library_save_location(&self) -> RegistryResult<Option<String>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.effective_library_root().map(Some)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(None)
        }
    }

    /// Changes the destination for future libraries. Registered roots never move.
    pub fn set_default_library_save_location(&self, path: &str) -> RegistryResult<String> {
        let path = platform::validate_save_location(path)?;
        self.write_value("default_library_save_location", path.as_bytes())?;
        Ok(path)
    }

    pub fn is_detached(&self, library_id: &LibraryId) -> RegistryResult<bool> {
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM detached_library WHERE library_id=?1) OR EXISTS(SELECT 1 FROM pending_local_purge WHERE library_id=?1)", [library_id.to_string()], |row| row.get(0))?)
    }

    pub fn pending_server_deletions(&self) -> RegistryResult<Vec<LibraryId>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(include_str!("sql/pending_server_deletions.sql"))?;
        let ids = statement.query_map([], |row| row.get::<_, String>(0))?.map(|row| Ok(LibraryId::parse_str(&row?)?)).collect();
        ids
    }

    pub fn complete_server_deletion(&self, library_id: &LibraryId) -> RegistryResult<()> {
        self.connection()?.execute("DELETE FROM detached_library WHERE library_id=?1", [library_id.to_string()])?;
        Ok(())
    }

    /// Hide the library and persist both server and local deletion intents
    /// before physical cleanup can be interrupted.
    pub fn begin_local_removal(&self, library_id: &LibraryId) -> RegistryResult<()> {
        let entry = self.entry(library_id)?.ok_or("library is not registered")?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO pending_local_purge(library_id,storage_path) VALUES(?1,?2)", params![library_id.to_string(), entry.storage_locator()])?;
        transaction.execute("INSERT OR IGNORE INTO detached_library(library_id) VALUES(?1)", [library_id.to_string()])?;
        transaction.execute("DELETE FROM library_registry WHERE library_id=?1", [library_id.to_string()])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_local_purges(&self) -> RegistryResult<Vec<LibraryId>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT library_id FROM pending_local_purge ORDER BY library_id")?;
        let ids = statement.query_map([], |row| row.get::<_, String>(0))?.map(|row| Ok(LibraryId::parse_str(&row?)?)).collect();
        ids
    }

    /// Physical cleanup is idempotent. Keep the purge record if any step fails.
    pub fn complete_local_purge(&self, library_id: &LibraryId) -> RegistryResult<()> {
        let locator: Option<String> = self.connection()?.query_row("SELECT storage_path FROM pending_local_purge WHERE library_id=?1", [library_id.to_string()], |row| row.get(0)).optional()?;
        let Some(locator) = locator else { return Ok(()) };
        #[cfg(target_arch = "wasm32")]
        platform::remove_database(&locator)?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            platform::remove_library_state(library_id, &locator)?;
            let database = self.library_database_locator(library_id, &locator);
            for suffix in ["", "-wal", "-shm"] {
                match std::fs::remove_file(format!("{database}{suffix}")) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        self.connection()?.execute("DELETE FROM pending_local_purge WHERE library_id=?1", [library_id.to_string()])?;
        Ok(())
    }

    /// Discovers libraries exposed by the current platform and registers any
    /// new entries. Platforms without directory discovery return an explicit
    /// capability error from the adapter; registration behavior stays shared.
    pub fn discover(&self, parent: &std::path::Path) -> RegistryResult<()> {
        let discovered = platform::discover_libraries(parent)?;
        for entry in discovered {
            self.connection()?
                .execute("INSERT INTO library_registry(library_id,library_name,storage_path) SELECT ?1,?2,?3 WHERE NOT EXISTS (SELECT 1 FROM detached_library WHERE library_id=?1) AND NOT EXISTS (SELECT 1 FROM pending_local_purge WHERE library_id=?1) ON CONFLICT(library_id) DO NOTHING", params![entry.library_id.to_string(), entry.library_name, entry.storage_locator])?;
        }
        Ok(())
    }

    pub fn remove(&self, library_id: &LibraryId) -> RegistryResult<()> {
        self.begin_local_removal(library_id)?;
        self.complete_local_purge(library_id)
    }

    pub fn read_value(&self, key: &str) -> RegistryResult<Option<Vec<u8>>> {
        Ok(self.connection()?.query_row("SELECT value FROM app_value WHERE key=?1", [key], |row| row.get(0)).optional()?)
    }

    pub fn write_value(&self, key: &str, value: &[u8]) -> RegistryResult<()> {
        self.connection()?.execute("INSERT INTO app_value(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
        Ok(())
    }

    pub fn remove_value(&self, key: &str) -> RegistryResult<()> {
        self.connection()?.execute("DELETE FROM app_value WHERE key=?1", [key])?;
        Ok(())
    }
}

fn validate_name(value: &str) -> RegistryResult<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err("invalid library name".into());
    }
    Ok(value.to_owned())
}

fn join_locator(root: &str, child: &str) -> String {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        child.to_owned()
    } else {
        format!("{root}/{child}")
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_library_manifest(root: &std::path::Path) -> RegistryResult<Option<(LibraryId, String)>> {
    platform::read_manifest(root)
}

#[cfg(not(target_arch = "wasm32"))]
pub use platform::LIBRARY_MANIFEST_FILENAME;

#[cfg(not(target_arch = "wasm32"))]
mod platform {
    use super::*;
    use serde::Deserialize;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    pub(super) fn validate_save_location(path: &str) -> RegistryResult<String> {
        let path = Path::new(path);
        if !path.is_absolute() {
            return Err("choose an absolute library save location".into());
        }
        fs::create_dir_all(path)?;
        Ok(canonical_library_path(path)?.to_string_lossy().into_owned())
    }

    pub(super) fn allocate_library_location(root: &str, id: &LibraryId, name: &str) -> RegistryResult<String> {
        let root = Path::new(root);
        fs::create_dir_all(root)?;
        let safe: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
        let safe = safe.trim().trim_matches('.');
        let safe = if safe.is_empty() { "Library" } else { safe };
        for index in 0..10000 {
            let path = root.join(if index == 0 { safe.to_owned() } else { format!("{safe} ({index})") });
            match fs::create_dir(&path) {
                Ok(()) => {
                    write_manifest(&LibraryEntry::new(*id, name, path.to_string_lossy())?)?;
                    return Ok(canonical_library_path(&path)?.to_string_lossy().into_owned());
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if read_manifest(&path).ok().flatten().is_some_and(|(existing, _)| existing == *id) {
                        return Ok(canonical_library_path(&path)?.to_string_lossy().into_owned());
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err("could not choose an unused library directory".into())
    }

    pub const LIBRARY_MANIFEST_FILENAME: &str = ".bokheim-library.json";
    const MANIFEST_FORMAT: &str = "bokheim-library";
    const MANIFEST_VERSION: u32 = 1;
    const METADATA_DIRECTORY: &str = ".bokheim";
    /// Folder-local database accompanying a native library root. This mirrors the
    /// APP_HIDDEN_LIBRARY_DB_PATH constant in library-backend, the working
    /// database the native backend opens (distinct from the centrally stored
    /// registry copy that `remove()` already deletes). Removing it on delete
    /// keeps a recreated library at the same path from inheriting stale
    /// enrichment and subject state. Books and other folder contents are kept.
    const FOLDER_DATABASE_FILENAME: &str = "library.db";

    #[derive(Serialize, Deserialize)]
    struct Manifest<S = String> {
        format: S,
        version: u32,
        library_id: LibraryId,
        library_name: S,
    }

    fn canonical_library_path(path: &Path) -> RegistryResult<PathBuf> {
        let canonical = path.canonicalize()?;
        if !canonical.is_dir() {
            return Err(format!("library root is not a directory: {}", path.display()).into());
        }
        Ok(canonical)
    }

    pub(super) fn prepare_created_library(generated_id: LibraryId, name: &str, requested_locator: String) -> RegistryResult<LibraryEntry> {
        let requested = PathBuf::from(requested_locator);
        fs::create_dir_all(&requested)?;
        let requested = canonical_library_path(&requested)?;
        let (id, name) = read_manifest(&requested)?.unwrap_or((generated_id, validate_name(name)?));
        let entry = LibraryEntry::new(id, &name, requested.to_string_lossy())?;
        write_manifest(&entry)?;
        Ok(entry)
    }

    pub(super) fn prepare_attached_library(id: LibraryId, name: &str, requested_locator: String) -> RegistryResult<LibraryEntry> {
        let path = PathBuf::from(requested_locator);
        fs::create_dir_all(&path)?;
        let path = canonical_library_path(&path)?;
        if read_manifest(&path)?.is_some_and(|(existing, _)| existing != id) {
            return Err("directory belongs to a different library".into());
        }
        let entry = LibraryEntry::new(id, name, path.to_string_lossy())?;
        write_manifest(&entry)?;
        Ok(entry)
    }

    pub(super) fn discover_libraries(parent: &Path) -> RegistryResult<Vec<LibraryEntry>> {
        fs::create_dir_all(parent)?;
        let mut paths = fs::read_dir(parent)?.filter_map(Result::ok).filter_map(|entry| entry.file_type().ok().filter(|kind| kind.is_dir()).map(|_| entry.path())).collect::<Vec<_>>();
        paths.sort();
        let mut found = Vec::new();
        for path in paths {
            let path = canonical_library_path(&path)?;
            if let Some((id, name)) = read_manifest(&path)? {
                found.push(LibraryEntry::new(id, &name, path.to_string_lossy())?);
            }
        }
        Ok(found)
    }

    pub(super) fn remove_library_state(id: &LibraryId, locator: &str) -> RegistryResult<()> {
        let root = Path::new(locator);
        // Folder-local state belongs to the on-disk identity, not the pathname.
        if !read_manifest(root)?.is_some_and(|(found, _)| found == *id) {
            return Ok(());
        }
        let metadata = root.join(METADATA_DIRECTORY);
        let database = metadata.join(FOLDER_DATABASE_FILENAME);
        let mut paths = vec![metadata.join("session.json"), database.clone()];
        for suffix in ["-wal", "-shm"] {
            paths.push(database.with_file_name(format!("{FOLDER_DATABASE_FILENAME}{suffix}")));
        }
        // Keep an identity marker until every other state file is gone. A
        // failed purge can then retry without touching a replacement library.
        paths.push(metadata.join("library.json"));
        paths.push(root.join(LIBRARY_MANIFEST_FILENAME));
        for path in paths {
            match fs::symlink_metadata(&path) {
                Ok(value) if value.file_type().is_symlink() => return Err(format!("refusing to remove symlinked library state file: {}", path.display()).into()),
                Ok(value) if value.is_file() => fs::remove_file(path)?,
                Ok(_) => return Err(format!("library state path is not a file: {}", path.display()).into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(super) fn read_manifest(root: &Path) -> RegistryResult<Option<(LibraryId, String)>> {
        let path = root.join(LIBRARY_MANIFEST_FILENAME);
        let contents = match fs::read_to_string(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match fs::read_to_string(root.join(METADATA_DIRECTORY).join("library.json")) {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            },
            Err(error) => return Err(error.into()),
        };
        let manifest: Manifest = serde_json::from_str(&contents)?;
        if manifest.format != MANIFEST_FORMAT || manifest.version != MANIFEST_VERSION {
            return Err(format!("unsupported library manifest in {}", path.display()).into());
        }
        Ok(Some((manifest.library_id, validate_name(&manifest.library_name)?)))
    }

    pub(super) fn write_manifest(entry: &LibraryEntry) -> RegistryResult<()> {
        let root = Path::new(entry.storage_locator());
        let metadata = root.join(METADATA_DIRECTORY);
        match fs::create_dir(&metadata) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && metadata.is_dir() => {}
            Err(error) => return Err(error.into()),
        }
        write_manifest_file(entry, &root.join(METADATA_DIRECTORY).join("library.json"))?;
        let path = root.join(LIBRARY_MANIFEST_FILENAME);
        write_manifest_file(entry, &path)
    }

    fn write_manifest_file(entry: &LibraryEntry, path: &Path) -> RegistryResult<()> {
        // Check each copy independently so a missing or stale backup is repaired.
        if fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok())
            .is_some_and(|manifest| manifest.format == MANIFEST_FORMAT && manifest.version == MANIFEST_VERSION && manifest.library_id == *entry.library_id() && manifest.library_name == entry.library_name())
        {
            return Ok(());
        }
        let temporary = path.with_file_name(format!(".library-manifest-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
            serde_json::to_writer_pretty(&mut file, &Manifest { format: MANIFEST_FORMAT, version: MANIFEST_VERSION, library_id: *entry.library_id(), library_name: entry.library_name() })?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

#[cfg(target_arch = "wasm32")]
mod platform {
    pub(super) fn validate_save_location(_path: &str) -> RegistryResult<String> {
        Err("browser storage has no filesystem save location".into())
    }
    pub(super) fn allocate_library_location(root: &str, id: &LibraryId, _name: &str) -> RegistryResult<String> {
        Ok(join_locator(root, &id.to_string()))
    }

    use super::*;
    use std::ffi::CString;

    pub(super) fn prepare_created_library(id: LibraryId, name: &str, requested_locator: String) -> RegistryResult<LibraryEntry> {
        LibraryEntry::new(id, name, requested_locator)
    }

    pub(super) fn prepare_attached_library(id: LibraryId, name: &str, requested_locator: String) -> RegistryResult<LibraryEntry> {
        LibraryEntry::new(id, name, requested_locator)
    }

    pub(super) fn discover_libraries(_parent: &std::path::Path) -> RegistryResult<Vec<LibraryEntry>> {
        Err("library directory discovery is not available on this platform".into())
    }

    pub(super) fn remove_database(locator: &str) -> RegistryResult<()> {
        let filename = CString::new(locator)?;
        // The registered SQLite VFS owns OPFS naming and deletion. Calling its
        // xDelete is the browser equivalent of removing the native database
        // file, and keeps that difference entirely at the storage boundary.
        let result = unsafe {
            let vfs = rusqlite::ffi::sqlite3_vfs_find(std::ptr::null());
            if vfs.is_null() {
                return Err("SQLite has no default VFS".into());
            }
            let delete = (*vfs).xDelete.ok_or("SQLite VFS does not support database deletion")?;
            delete(vfs, filename.as_ptr(), 1)
        };
        if result == rusqlite::ffi::SQLITE_OK || result == rusqlite::ffi::SQLITE_IOERR_DELETE_NOENT {
            Ok(())
        } else {
            Err(format!("SQLite VFS could not delete {locator}: error {result}").into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_removal_detaches_before_physical_cleanup_and_preserves_retry_record() {
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("state").to_string_lossy()).unwrap();
        let entry = registry.create("Books", None).unwrap();
        let id = *entry.library_id();

        registry.begin_local_removal(&id).unwrap();
        assert!(registry.entry(&id).unwrap().is_none());
        assert!(registry.is_detached(&id).unwrap());
        assert_eq!(registry.pending_local_purges().unwrap(), [id]);

        drop(registry);
        let reopened = LibraryRegistry::open(temp.path().join("state").to_string_lossy()).unwrap();
        assert_eq!(reopened.pending_local_purges().unwrap(), [id]);
    }

    #[test]
    fn pending_native_purge_survives_cleanup_failure_and_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("libraries");
        let registry = LibraryRegistry::open(temp.path().join("state").to_string_lossy()).unwrap();
        let entry = registry.create("Books", Some(parent.join("books").to_string_lossy().into_owned())).unwrap();
        let id = *entry.library_id();
        let blocked_file = std::path::Path::new(entry.storage_locator()).join(".bokheim/library.db");
        std::fs::create_dir(&blocked_file).unwrap();

        registry.begin_local_removal(&id).unwrap();
        registry.complete_server_deletion(&id).unwrap();
        assert!(registry.is_detached(&id).unwrap(), "pending local cleanup still blocks reattachment");
        registry.discover(&parent).unwrap();
        assert!(registry.entry(&id).unwrap().is_none());
        assert!(registry.complete_local_purge(&id).is_err());
        assert_eq!(registry.pending_local_purges().unwrap(), [id]);
        assert_eq!(read_library_manifest(std::path::Path::new(entry.storage_locator())).unwrap().unwrap().0, id);
        assert!(registry.create("Replacement", Some(entry.storage_locator().to_owned())).is_err());

        std::fs::remove_dir(&blocked_file).unwrap();
        registry.complete_local_purge(&id).unwrap();
        assert!(registry.pending_local_purges().unwrap().is_empty());
        assert!(registry.entry(&id).unwrap().is_none());
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_attachment_preserves_files_and_rows_but_repairs_backup() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("state").to_string_lossy()).unwrap();
        let entry = registry.create("Books", None).unwrap();
        let root = std::path::Path::new(entry.storage_locator());
        let primary = root.join(LIBRARY_MANIFEST_FILENAME);
        let backup = root.join(".bokheim/library.json");
        let primary_inode = std::fs::metadata(&primary).unwrap().ino();
        let backup_inode = std::fs::metadata(&backup).unwrap().ino();
        let changes = registry.connection().unwrap().total_changes();
        registry.attach_remote(entry.library_id(), "Books").unwrap();
        assert_eq!(registry.connection().unwrap().total_changes(), changes);
        assert_eq!(std::fs::metadata(&primary).unwrap().ino(), primary_inode);
        assert_eq!(std::fs::metadata(&backup).unwrap().ino(), backup_inode);
        std::fs::write(&backup, b"invalid manifest").unwrap();
        registry.attach_remote(entry.library_id(), "Books").unwrap();
        assert_eq!(std::fs::read(&primary).unwrap(), std::fs::read(&backup).unwrap());
        assert_eq!(std::fs::metadata(&primary).unwrap().ino(), primary_inode);
        std::fs::remove_file(&backup).unwrap();
        registry.attach_remote(entry.library_id(), "Books").unwrap();
        assert_eq!(std::fs::read(&primary).unwrap(), std::fs::read(&backup).unwrap());
    }

    #[test]
    fn relocation_validates_identity_and_does_not_recreate_or_duplicate_a_library() {
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("app").to_string_lossy()).unwrap();
        let entry = registry.create("Books", Some(temp.path().join("old").to_string_lossy().into_owned())).unwrap();
        let moved = temp.path().join("moved");
        std::fs::rename(entry.storage_locator(), &moved).unwrap();
        registry.attach_remote(entry.library_id(), "Books renamed").unwrap();
        assert!(!std::path::Path::new(entry.storage_locator()).exists());
        let other = registry.create("Other", None).unwrap();
        assert!(registry.relocate(entry.library_id(), entry.storage_locator(), std::path::Path::new(other.storage_locator())).is_err());
        // The hidden backup remains sufficient if the root marker was deleted.
        std::fs::remove_file(moved.join(LIBRARY_MANIFEST_FILENAME)).unwrap();
        assert!(registry.relocate(entry.library_id(), entry.storage_locator(), &moved).unwrap());
        assert!(!registry.relocate(entry.library_id(), entry.storage_locator(), &moved).unwrap());
        assert_eq!(registry.entries().unwrap().len(), 2);
        let current = registry.entry(entry.library_id()).unwrap().unwrap();
        assert_eq!(current.library_name(), "Books renamed");
        assert_eq!(current.storage_locator(), moved.to_str().unwrap());
        registry.remove(entry.library_id()).unwrap();
        assert!(read_library_manifest(&moved).unwrap().is_none());
    }

    #[test]
    fn refresh_does_not_claim_a_markerless_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("app").to_string_lossy()).unwrap();
        let entry = registry.create("Books", None).unwrap();
        std::fs::rename(entry.storage_locator(), temp.path().join("moved")).unwrap();
        std::fs::create_dir(entry.storage_locator()).unwrap();
        registry.attach_remote(entry.library_id(), "Renamed").unwrap();
        assert_eq!(registry.entry(entry.library_id()).unwrap().unwrap().library_name(), "Renamed");
        assert_eq!(std::fs::read_dir(entry.storage_locator()).unwrap().count(), 0);
    }

    #[test]
    fn refresh_and_removal_preserve_another_library_at_the_old_path() {
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("app").to_string_lossy()).unwrap();
        let entry = registry.create("Books", None).unwrap();
        let other = registry.create("Other", None).unwrap();
        std::fs::rename(entry.storage_locator(), temp.path().join("moved")).unwrap();
        std::fs::rename(other.storage_locator(), entry.storage_locator()).unwrap();
        let root = std::path::Path::new(entry.storage_locator());
        std::fs::write(root.join(".bokheim/session.json"), b"other session").unwrap();
        let files = [".bokheim-library.json", ".bokheim/library.json", ".bokheim/session.json"];
        let before = files.map(|file| std::fs::read(root.join(file)).unwrap());
        let database = registry.library_database_locator(entry.library_id(), entry.storage_locator());
        std::fs::create_dir_all(std::path::Path::new(&database).parent().unwrap()).unwrap();
        std::fs::write(&database, b"own database").unwrap();
        let folder_database = root.join(".bokheim/library.db");
        std::fs::write(&folder_database, b"folder database").unwrap();
        registry.attach_remote(entry.library_id(), "Renamed").unwrap();
        assert_eq!(files.map(|file| std::fs::read(root.join(file)).unwrap()), before);
        registry.remove(entry.library_id()).unwrap();
        assert_eq!(files.map(|file| std::fs::read(root.join(file)).unwrap()), before);
        assert!(folder_database.exists());
        assert!(!std::path::Path::new(&database).exists());
        assert!(registry.entry(entry.library_id()).unwrap().is_none());
        assert!(registry.entry(other.library_id()).unwrap().is_some());
    }

    #[test]
    fn relocation_accepts_a_match_when_another_library_occupies_the_old_path() {
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("app").to_string_lossy()).unwrap();
        let entry = registry.create("Books", None).unwrap();
        let other = registry.create("Other", None).unwrap();
        let moved = temp.path().join("moved");
        std::fs::rename(entry.storage_locator(), &moved).unwrap();
        std::fs::rename(other.storage_locator(), entry.storage_locator()).unwrap();
        assert!(registry.relocate(entry.library_id(), entry.storage_locator(), &moved).unwrap());
        assert_eq!(read_library_manifest(std::path::Path::new(entry.storage_locator())).unwrap().unwrap().0, *other.library_id());
        assert_eq!(registry.entry(entry.library_id()).unwrap().unwrap().storage_locator(), moved.to_str().unwrap());
    }

    #[test]
    fn relocation_never_overrides_a_restored_original_path() {
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("app").to_string_lossy()).unwrap();
        let entry = registry.create("Books", None).unwrap();
        assert!(!registry.relocate(entry.library_id(), entry.storage_locator(), std::path::Path::new(entry.storage_locator())).unwrap());
    }

    #[test]
    fn device_database_paths_are_isolated_and_removed_with_registration() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let registry = LibraryRegistry::open(app.to_string_lossy()).unwrap();
        let entry = registry.create("Books", Some(temp.path().join("books").to_string_lossy().into_owned())).unwrap();
        let other = registry.create("Other", Some(temp.path().join("other").to_string_lossy().into_owned())).unwrap();
        let database = registry.library_database_locator(entry.library_id(), entry.storage_locator());
        let other_database = registry.library_database_locator(other.library_id(), other.storage_locator());
        assert_eq!(std::path::Path::new(&database), app.join("libraries").join(entry.library_id().to_string()).join("library.db"));
        assert_eq!(database, registry.library_database_locator(entry.library_id(), "moved-content"));
        for path in [&database, &other_database] {
            std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
            std::fs::write(path, b"database").unwrap();
        }
        std::fs::write(format!("{database}-wal"), b"wal").unwrap();
        registry.remove(entry.library_id()).unwrap();
        assert!(!std::path::Path::new(&database).exists());
        assert!(!std::path::Path::new(&format!("{database}-wal")).exists());
        assert!(std::path::Path::new(&other_database).exists());
        assert!(std::path::Path::new(entry.storage_locator()).is_dir());
    }

    #[test]
    fn removal_deletes_folder_local_database_but_preserves_books() {
        let temp = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temp.path().join("app").to_string_lossy()).unwrap();
        let entry = registry.create("Books", Some(temp.path().join("books").to_string_lossy().into_owned())).unwrap();
        let root = std::path::Path::new(entry.storage_locator());
        let database = root.join(".bokheim/library.db");
        std::fs::write(&database, b"database").unwrap();
        let wal = database.with_file_name("library.db-wal");
        std::fs::write(&wal, b"wal").unwrap();
        std::fs::write(root.join("book.epub"), b"book").unwrap();
        registry.remove(entry.library_id()).unwrap();
        assert!(!database.exists());
        assert!(!wal.exists());
        assert_eq!(std::fs::read(root.join("book.epub")).unwrap(), b"book");
        assert!(registry.entry(entry.library_id()).unwrap().is_none());
        assert!(registry.is_detached(entry.library_id()).unwrap());
    }

    #[test]
    fn asset_storage_policy_survives_registry_reopen() {
        let temporary = tempfile::tempdir().unwrap();
        let app = temporary.path().join("app").to_string_lossy().into_owned();
        let registry = LibraryRegistry::open(&app).unwrap();
        let local = registry.create_with_asset_storage("Local", None, false).unwrap();
        let synced = registry.create("Synced", None).unwrap();
        assert!(!registry.asset_storage_enabled(local.library_id()).unwrap());
        assert!(registry.asset_storage_enabled(synced.library_id()).unwrap());
        drop(registry);
        let registry = LibraryRegistry::open(&app).unwrap();
        assert!(!registry.asset_storage_enabled(local.library_id()).unwrap());
        assert!(registry.asset_storage_enabled(synced.library_id()).unwrap());
        registry.set_asset_storage_enabled(local.library_id(), true).unwrap();
        assert!(registry.asset_storage_enabled(local.library_id()).unwrap());
        registry.set_asset_storage_enabled(local.library_id(), false).unwrap();
        registry.remove(local.library_id()).unwrap();
        assert_eq!(registry.pending_server_deletions().unwrap(), vec![*local.library_id()]);
    }

    #[test]
    fn registry_round_trips_through_sqlite() {
        let temporary = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temporary.path().join("app").to_string_lossy()).unwrap();
        let root = temporary.path().join("library");
        let entry = registry.create(" Books ", Some(root.to_string_lossy().into_owned())).unwrap();
        assert_eq!(entry.library_name(), "Books");
        assert_eq!(registry.entries().unwrap().len(), 1);
        drop(registry);
        assert_eq!(LibraryRegistry::open(temporary.path().join("app").to_string_lossy()).unwrap().entries().unwrap().len(), 1);
    }

    #[test]
    fn discovery_registers_manifests_and_preserves_existing_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("libraries");
        let source = LibraryRegistry::open(temporary.path().join("source").to_string_lossy()).unwrap();
        let original = source.create("Books", Some(parent.join("books").to_string_lossy().into_owned())).unwrap();
        let registry = LibraryRegistry::open(temporary.path().join("destination").to_string_lossy()).unwrap();
        registry.discover(&parent).unwrap();
        let discovered = registry.entries().unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].library_id(), original.library_id());
        assert_eq!(discovered[0].storage_locator(), original.storage_locator());
        registry.attach_remote(original.library_id(), "Registered name").unwrap();
        source.attach_remote(original.library_id(), "Manifest name").unwrap();
        registry.discover(&parent).unwrap();
        let rediscovered = registry.entries().unwrap();
        assert_eq!(rediscovered[0].library_name(), "Registered name");
        assert_eq!(registry.entries().unwrap().len(), 1);
    }

    #[test]
    fn values_and_detached_libraries_share_the_store() {
        let temporary = tempfile::tempdir().unwrap();
        let registry = LibraryRegistry::open(temporary.path().join("app").to_string_lossy()).unwrap();
        registry.write_value("preference", b"value").unwrap();
        assert_eq!(registry.read_value("preference").unwrap().as_deref(), Some(b"value".as_slice()));
        let entry = registry.create("Books", None).unwrap();
        registry.remove(entry.library_id()).unwrap();
        assert!(registry.is_detached(entry.library_id()).unwrap());
        assert!(registry.entries().unwrap().is_empty());
    }
    #[test]
    fn default_location_changes_only_future_libraries_and_preserves_selected_roots() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let next = root.path().join("next");
        let state = root.path().join("state");
        let registry = LibraryRegistry::open_with_library_root(state.to_str().unwrap(), first.to_str().unwrap()).unwrap();
        let created = registry.create("My Library", None).unwrap();
        assert_eq!(std::path::Path::new(created.storage_locator()), first.join("My Library"));
        registry.set_default_library_save_location(next.to_str().unwrap()).unwrap();
        assert_eq!(registry.entries().unwrap()[0].storage_locator(), created.storage_locator());
        let remote = registry.attach_remote(&LibraryId::new_v4(), "My Library").unwrap();
        assert_eq!(std::path::Path::new(remote.storage_locator()), next.join("My Library"));
        let selected = root.path().join("existing library");
        std::fs::create_dir_all(selected.join("Original folder")).unwrap();
        std::fs::write(selected.join("Original folder/book.epub"), b"original file").unwrap();
        let existing = registry.create("Existing", Some(selected.to_string_lossy().into_owned())).unwrap();
        assert_eq!(std::path::Path::new(existing.storage_locator()), selected);
        assert_eq!(std::fs::read(selected.join("Original folder/book.epub")).unwrap(), b"original file");
        drop(registry);
        let reopened = LibraryRegistry::open_with_library_root(state.to_str().unwrap(), first.to_str().unwrap()).unwrap();
        assert_eq!(reopened.default_library_save_location().unwrap(), Some(next.to_string_lossy().into_owned()));
        let again = reopened.attach_remote(remote.library_id(), "Renamed remote").unwrap();
        assert_eq!(again.storage_locator(), remote.storage_locator());
        let collision = reopened.create("My Library", None).unwrap();
        assert_ne!(collision.storage_locator(), remote.storage_locator());
    }
}
