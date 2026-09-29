//! Native SQLite connection mechanisms; callers own schema and transaction policy.
use crate::database_diagnostics;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub const SINGLE_OWNER: bool = false;

pub fn initialize(connection: &Connection, initialize: fn(&Connection) -> rusqlite::Result<()>) -> rusqlite::Result<()> {
    database_diagnostics::install(connection);
    let result = initialize(connection);
    if result.is_err() {
        database_diagnostics::command_failed(connection);
    } else {
        // Schema initialization may replace SQLite's busy handler.
        database_diagnostics::install(connection);
    }
    result
}

/// Pool expansion must not recreate a database whose library was moved.
pub fn open_additional(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
}

/// Probe existing storage without creating it or initializing a schema.
pub fn open_existing(path: &Path) -> Result<Option<Connection>, String> {
    if !path.exists() {
        return Ok(None);
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map(Some).map_err(|error| error.to_string())
}
