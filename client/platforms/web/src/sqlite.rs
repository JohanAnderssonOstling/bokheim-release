//! Browser SQLite connection mechanisms over the installed OPFS VFS.
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// OPFS must never wait synchronously for another same-worker borrower.
pub const SINGLE_OWNER: bool = true;

pub fn initialize(connection: &Connection, initialize: fn(&Connection) -> rusqlite::Result<()>) -> rusqlite::Result<()> {
    initialize(connection)
}

pub fn open_additional(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open(path)
}

/// Check the VFS namespace rather than probing the host filesystem.
pub fn open_existing(path: &Path) -> Result<Option<Connection>, String> {
    let locator = path.to_str().ok_or("browser database locator is not UTF-8")?;
    if !crate::web_storage::database_exists(locator)? {
        return Ok(None);
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map(Some).map_err(|error| error.to_string())
}
