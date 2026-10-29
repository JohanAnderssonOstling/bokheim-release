//! Bulk installation of closed reference files in either worker's OPFS pool.
use super::web_storage::storage_error;
use sqlite_wasm_vfs::sahpool::OpfsSAHPoolUtil;

const PENDING: &str = "__reference_import__/";

pub(super) fn recover(storage: &OpfsSAHPoolUtil) -> Result<(), String> {
    for marker in storage.list() {
        if let Some(name) = marker.strip_prefix(PENDING) {
            storage.delete_db(name).map_err(storage_error)?;
            storage.delete_db(&marker).map_err(storage_error)?;
        }
    }
    Ok(())
}

pub(super) fn import(storage: &OpfsSAHPoolUtil, name: &str, image: &[u8]) -> Result<(), String> {
    // A persistent pool entry marks an unfinished copy. Neither reference
    // installation nor recovery needs the library asset-index connection.
    let marker = format!("{PENDING}{name}");
    storage.import_db_unchecked(&marker, &[]).map_err(storage_error)?;
    storage.delete_db(name).map_err(storage_error)?;
    storage.import_db(name, image).map_err(storage_error)?;
    storage.delete_db(&marker).map_err(storage_error)?;
    Ok(())
}

pub(super) fn prune(storage: &OpfsSAHPoolUtil, legacy: &str, prefix: &str, selected: &str) -> Result<(), String> {
    for filename in storage.list() {
        if filename != selected && (filename == legacy || filename.starts_with(prefix)) {
            storage.delete_db(&filename).map_err(storage_error)?;
        }
    }
    Ok(())
}
