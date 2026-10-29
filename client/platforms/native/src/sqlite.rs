//! Native SQLite connection mechanisms; callers own schema and transaction policy.
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub const SINGLE_OWNER: bool = false;

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

/// Open existing writable storage so SQLite can recover an interrupted transaction.
/// Unlike `Connection::open`, this never creates a missing database.
pub fn open_existing_writable(path: &Path) -> Result<Option<Connection>, String> {
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(None);
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map(Some).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Error, ErrorCode};
    use std::{
        env,
        process::{self, Command},
        thread,
    };

    #[test]
    fn writable_open_does_not_create_missing_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing.sqlite3");
        assert!(open_existing_writable(&path).unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn writable_open_recovers_interrupted_transaction() {
        const CHILD_DATABASE: &str = "BOKHEIM_SQLITE_RECOVERY_TEST_DATABASE";
        if let Some(path) = env::var_os(CHILD_DATABASE) {
            let db = Connection::open(path).unwrap();
            db.execute_batch(
                "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=1; PRAGMA cache_spill=ON;
                BEGIN IMMEDIATE; UPDATE recovery_probe SET value=2;",
            )
            .unwrap();
            // Exit without dropping the connection: leave the rollback journal
            // and dirty database pages exactly as an interrupted process would.
            process::exit(0);
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite3");
        let journal = directory.path().join("catalog.sqlite3-journal");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=DELETE;
            CREATE TABLE recovery_probe(id INTEGER PRIMARY KEY,value INTEGER,payload BLOB);
            WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<32)
            INSERT INTO recovery_probe SELECT id,1,zeroblob(8192) FROM n;",
        )
        .unwrap();
        drop(db);

        let test_name = thread::current().name().unwrap().to_owned();
        let status = Command::new(env::current_exe().unwrap()).args(["--exact", &test_name, "--nocapture"]).env(CHILD_DATABASE, &path).status().unwrap();
        assert!(status.success());
        assert!(journal.exists(), "the interrupted writer must leave a journal");

        let probe = open_existing(&path).unwrap().unwrap();
        let error = probe.query_row("SELECT sum(value) FROM recovery_probe", [], |row| row.get::<_, i64>(0)).unwrap_err();
        assert!(matches!(error, Error::SqliteFailure(ref failure, _) if failure.code == ErrorCode::ReadOnly), "{error}");
        drop(probe);

        let recovered = open_existing_writable(&path).unwrap().unwrap();
        assert_eq!(recovered.query_row("SELECT sum(value) FROM recovery_probe", [], |row| row.get::<_, i64>(0)).unwrap(), 32);
        assert_eq!(recovered.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
        assert!(!journal.exists());
    }
}
