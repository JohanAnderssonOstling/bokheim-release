//! Device-local SQLite opening policy.
//!
//! Callers supply storage locators and database policy without knowing whether
//! SQLite is backed by native files or a browser-installed virtual filesystem.

use rusqlite::Connection;
use std::fmt;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum JournalMode {
    #[default]
    PlatformDefault,
    Wal,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenOptions {
    busy_timeout: Option<Duration>,
    journal_mode: JournalMode,
}

impl OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn busy_timeout(mut self, timeout: Duration) -> Self {
        self.busy_timeout = Some(timeout);
        self
    }

    pub fn journal_mode(mut self, journal_mode: JournalMode) -> Self {
        self.journal_mode = journal_mode;
        self
    }
}

#[derive(Debug)]
pub struct OpenError(String);

impl fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for OpenError {}

/// Opens a device-local database through the active platform's storage
/// implementation. Native parents are created as needed; browser locators are
/// passed directly to the SQLite VFS installed by the host.
pub fn open(path: impl AsRef<Path>, options: OpenOptions) -> Result<Connection, OpenError> {
    let path = path.as_ref();
    platform::prepare(path).map_err(OpenError)?;
    let connection = Connection::open(path).map_err(|error| OpenError(error.to_string()))?;
    if let Some(timeout) = options.busy_timeout {
        connection.busy_timeout(timeout).map_err(|error| OpenError(error.to_string()))?;
    }
    platform::configure_journal(&connection, options.journal_mode).map_err(OpenError)?;
    Ok(connection)
}

#[cfg(not(target_arch = "wasm32"))]
mod platform {
    use super::JournalMode;
    use rusqlite::Connection;
    use std::path::Path;

    pub(super) fn prepare(path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| format!("failed to create database directory {}: {error}", parent.display()))?;
        }
        Ok(())
    }

    pub(super) fn configure_journal(connection: &Connection, mode: JournalMode) -> Result<(), String> {
        if mode == JournalMode::Wal {
            connection.pragma_update(None, "journal_mode", "WAL").map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
mod platform {
    use super::JournalMode;
    use rusqlite::Connection;
    use std::path::Path;

    pub(super) fn prepare(_path: &Path) -> Result<(), String> {
        Ok(())
    }

    pub(super) fn configure_journal(_connection: &Connection, _mode: JournalMode) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn opening_creates_native_parent_directories() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/storage/database.sqlite3");

        let connection = open(&path, OpenOptions::new()).unwrap();

        assert!(path.is_file());
        connection.execute("CREATE TABLE example(value TEXT NOT NULL)", []).unwrap();
    }

    #[test]
    fn native_wal_policy_is_applied() {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(directory.path().join("database.sqlite3"), OpenOptions::new().journal_mode(JournalMode::Wal)).unwrap();

        let mode = connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)).unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }
}
