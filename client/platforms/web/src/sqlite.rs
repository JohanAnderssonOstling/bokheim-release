//! Browser SQLite connection mechanisms over the installed OPFS VFS.
use rusqlite::{Connection, OpenFlags};
use sqlite_wasm_vfs::sahpool::{install, OpfsSAHPoolCfgBuilder, OpfsSAHPoolUtil};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

thread_local! {
    static REFERENCE_STORAGE: RefCell<Option<Rc<OpfsSAHPoolUtil>>> = const { RefCell::new(None) };
}

/// OPFS must never wait synchronously for another same-worker borrower.
pub const SINGLE_OWNER: bool = true;

/// The CPU worker owns its own reference file handles. It never opens the
/// database worker's library/asset pool or performs asset reclamation.
pub async fn initialize_reference_storage() -> Result<(), String> {
    let config = OpfsSAHPoolCfgBuilder::new().vfs_name("bokheim-cpu-reference-v1").directory(".bokheim-cpu-reference-v1").initial_capacity(2).clear_on_init(false).build();
    let storage = install::<rusqlite::ffi::WasmOsCallback>(&config, true).await.map_err(|e| e.to_string())?;
    super::reference_storage::recover(&storage)?;
    storage.reserve_minimum_capacity(storage.count().saturating_add(2)).await.map_err(|e| e.to_string())?;
    REFERENCE_STORAGE.with(|slot| *slot.borrow_mut() = Some(Rc::new(storage)));
    Ok(())
}

/// The same bulk import and interruption recovery serve both worker pools.
pub fn import_reference_database(name: &str, image: &[u8]) -> Result<(), String> {
    match REFERENCE_STORAGE.with(|slot| slot.borrow().clone()) {
        Some(storage) => super::reference_storage::import(&storage, name, image),
        None => super::web_storage::import_reference_database(name, image),
    }
}

pub fn prune_reference_databases(legacy: &str, prefix: &str, selected: &str) -> Result<(), String> {
    match REFERENCE_STORAGE.with(|slot| slot.borrow().clone()) {
        Some(storage) => super::reference_storage::prune(&storage, legacy, prefix, selected),
        None => super::web_storage::prune_reference_databases(legacy, prefix, selected),
    }
}

pub fn open_additional(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open(path)
}

/// Check the VFS namespace rather than probing the host filesystem.
pub fn open_existing(path: &Path) -> Result<Option<Connection>, String> {
    open_existing_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

/// Allow SQLite to recover existing catalog storage without creating a new file.
pub fn open_existing_writable(path: &Path) -> Result<Option<Connection>, String> {
    open_existing_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
}

fn open_existing_with_flags(path: &Path, flags: OpenFlags) -> Result<Option<Connection>, String> {
    let locator = path.to_str().ok_or("browser database locator is not UTF-8")?;
    if !crate::web_storage::database_exists(locator)? {
        return Ok(None);
    }
    Connection::open_with_flags(path, flags).map(Some).map_err(|error| error.to_string())
}
