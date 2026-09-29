//! Transaction boundaries and temporary sync state, private to the database owner.
use crate::import::ImportCommitSql;
use crate::sync::SyncApplySql;
use crate::{Database, DatabaseError};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) enum WriteOutcome<T> {
    Commit(T),
    Rollback(T),
}

impl Database {
    /// Reserve only main: BEGIN IMMEDIATE also involves the attached read-only
    /// taxonomy. Errors and panics drop the transaction without committing.
    pub(crate) fn with_write_transaction<T>(&self, operation: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<WriteOutcome<T>, DatabaseError>) -> Result<T, DatabaseError> {
        let mut timing = crate::timing::PhaseTimer::new("write_transaction");
        let tx = self.connection.unchecked_transaction()?;
        tx.reserve_write_lock()?;
        timing.mark("begin_and_reserve");
        let outcome = operation(&tx);
        timing.mark("body");
        match outcome? {
            WriteOutcome::Commit(value) => {
                crate::sync::apply::project_dirty(&tx)?;
                self.refresh_subject_cache_in_transaction(&tx)?;
                tx.commit()?;
                timing.mark("commit");
                Ok(value)
            }
            WriteOutcome::Rollback(value) => {
                tx.rollback()?;
                timing.mark("rollback");
                Ok(value)
            }
        }
    }
}

/// Lazy so a page containing only stale mutations doesn't write change_origin.
pub(crate) struct RemoteOrigin {
    previous: Option<String>,
    active: bool,
}

impl RemoteOrigin {
    pub(crate) fn mark(&mut self, tx: &rusqlite::Transaction<'_>) -> Result<(), DatabaseError> {
        if !self.active {
            tx.mark_origin_remote()?;
            self.active = true;
        }
        Ok(())
    }
}

/// The caller owns the transaction and must propagate errors. A panic is handled
/// by the outer transaction rollback; normal errors also restore origin here.
pub(crate) fn with_remote_origin<T>(tx: &rusqlite::Transaction<'_>, operation: impl FnOnce(&mut RemoteOrigin) -> Result<T, DatabaseError>) -> Result<T, DatabaseError> {
    let mut previous = None;
    tx.change_origin_select(|row| {
        previous = row.get(0)?;
        Ok(())
    })?;
    let mut origin = RemoteOrigin { previous, active: false };
    let result = operation(&mut origin);
    if origin.active {
        tx.restore_origin(origin.previous.as_deref().unwrap_or("local"))?;
    }
    result
}

// SQL rollback cannot restore this connection-local scalar flag.
struct MetadataBatchGuard<'a> {
    flag: &'a AtomicBool,
    previous: bool,
}
impl Drop for MetadataBatchGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(self.previous, Ordering::Relaxed);
    }
}

/// Coalesce local contributor changes; only the outermost batch flushes.
/// Remote writes keep their origin and never produce local outbox payloads.
/// Propagate errors to the owning transaction, which rolls back pending rows.
pub(crate) fn with_metadata_batch<T>(tx: &rusqlite::Transaction<'_>, flag: &AtomicBool, operation: impl FnOnce() -> Result<T, DatabaseError>) -> Result<T, DatabaseError> {
    let mut origin: Option<String> = None;
    tx.change_origin_select(|row| {
        origin = row.get(0)?;
        Ok(())
    })?;
    if origin.as_deref() == Some("remote") {
        return operation();
    }
    let guard = MetadataBatchGuard { flag, previous: flag.swap(true, Ordering::Relaxed) };
    let mut timing = crate::timing::PhaseTimer::new("metadata_batch");
    let result = operation()?;
    timing.mark("writes");
    if !guard.previous {
        tx.batch_flag_flush()?;
        timing.mark("flush_payloads");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn database() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.connection.execute_batch("CREATE TABLE transaction_probe(value INTEGER);").unwrap();
        db
    }

    fn origin(db: &Database) -> String {
        db.connection.query_row("SELECT change_origin FROM sync_metadata WHERE singleton=1", [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn write_boundary_commits_or_rolls_back_explicit_outcomes_and_errors() {
        let db = database();
        db.with_write_transaction(|tx| {
            tx.execute("INSERT INTO transaction_probe VALUES (1)", [])?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        let outcome = db
            .with_write_transaction(|tx| {
                tx.execute("INSERT INTO transaction_probe VALUES (2)", [])?;
                Ok(WriteOutcome::Rollback("stale"))
            })
            .unwrap();
        assert_eq!(outcome, "stale");
        let failed: Result<(), _> = db.with_write_transaction(|tx| {
            tx.execute("INSERT INTO transaction_probe VALUES (3)", [])?;
            Err(DatabaseError::message("injected error"))
        });
        assert!(failed.is_err());
        assert_eq!(db.connection.query_row("SELECT SUM(value) FROM transaction_probe", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert!(db.connection.is_autocommit());
    }

    #[test]
    fn panic_rolls_back_remote_writes_and_restores_connection_batch_flag() {
        let db = database();
        let before = origin(&db);
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _: Result<(), _> = db.with_write_transaction(|tx| {
                with_metadata_batch(tx, &db.metadata_batch_flag, || {
                    with_remote_origin(tx, |remote| {
                        remote.mark(tx)?;
                        tx.execute("INSERT INTO transaction_probe VALUES (1)", [])?;
                        panic!("injected panic");
                    })
                })
            });
        }));
        assert!(panic.is_err());
        assert_eq!(origin(&db), before);
        assert!(!db.metadata_batch_flag.load(Ordering::Relaxed));
        assert_eq!(db.connection.query_row("SELECT COUNT(*) FROM transaction_probe", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert!(db.connection.is_autocommit());
    }

    #[test]
    fn nested_remote_scopes_restore_their_callers_origin_on_error() {
        let db = database();
        let before = origin(&db);
        db.with_write_transaction(|tx| {
            with_remote_origin(tx, |outer| {
                outer.mark(tx)?;
                let failed: Result<(), _> = with_remote_origin(tx, |inner| {
                    inner.mark(tx)?;
                    Err(DatabaseError::message("injected error"))
                });
                assert!(failed.is_err());
                assert_eq!(origin(&db), "remote");
                Ok(())
            })?;
            assert_eq!(origin(&db), before);
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
    }

    fn seed_book(db: &Database) {
        db.connection.execute("INSERT INTO book(content_hash,title,added_at,format) VALUES (?1,'Before',1,'epub')", ["a".repeat(64)]).unwrap();
        db.with_write_transaction(|tx| {
            let credit = book_model::Contributor::new("Before", book_model::MarcRelatorCode(*b"aut")).unwrap();
            let credits = crate::contributors::resolve_credits(tx, &[credit])?;
            crate::contributors::write_credits(tx, &"a".repeat(64), &credits)?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        db.connection.execute("DELETE FROM sync_outbox", []).unwrap();
    }

    #[test]
    fn nested_metadata_batches_flush_once_and_remote_batches_do_not_echo() {
        let db = database();
        seed_book(&db);
        db.with_write_transaction(|tx| {
            with_metadata_batch(tx, &db.metadata_batch_flag, || {
                assert!(tx.query_row("SELECT metadata_batch_flag(NULL)", [], |row| row.get::<_, bool>(0))?);
                tx.execute("UPDATE book_contributor SET name='First'", [])?;
                with_metadata_batch(tx, &db.metadata_batch_flag, || {
                    tx.execute("UPDATE book_contributor SET name='Second'", [])?;
                    Ok(())
                })?;
                assert!(db.metadata_batch_flag.load(Ordering::Relaxed));
                assert_eq!(tx.query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get::<_, i64>(0))?, 0);
                Ok(())
            })?;
            assert!(!db.metadata_batch_flag.load(Ordering::Relaxed));
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM pending_metadata_payload", [], |r| r.get::<_, i64>(0))?, 0);
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get::<_, i64>(0))?, 1);
            tx.execute("DELETE FROM sync_outbox", [])?;
            with_remote_origin(tx, |remote| {
                remote.mark(tx)?;
                with_metadata_batch(tx, &db.metadata_batch_flag, || {
                    assert!(!db.metadata_batch_flag.load(Ordering::Relaxed));
                    tx.execute("UPDATE book_contributor SET name='Remote'", [])?;
                    Ok(())
                })
            })?;
            assert_eq!(tx.query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get::<_, i64>(0))?, 0);
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
    }

    #[test]
    fn failed_metadata_flush_restores_flag_and_rolls_back_pending_rows() {
        let db = database();
        seed_book(&db);
        db.connection.execute_batch("CREATE TEMP TRIGGER reject_flush BEFORE INSERT ON sync_outbox BEGIN SELECT RAISE(ABORT,'injected flush failure'); END;").unwrap();
        let result = db.with_write_transaction(|tx| {
            with_metadata_batch(tx, &db.metadata_batch_flag, || {
                tx.execute("UPDATE book_contributor SET name='Must roll back'", [])?;
                Ok(())
            })?;
            Ok(WriteOutcome::Commit(()))
        });
        assert!(result.is_err());
        assert!(!db.metadata_batch_flag.load(Ordering::Relaxed));
        assert_eq!(db.connection.query_row("SELECT name FROM book_contributor", [], |r| r.get::<_, String>(0)).unwrap(), "Before");
        assert_eq!(db.connection.query_row("SELECT COUNT(*) FROM pending_metadata_payload", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn reservation_blocks_other_main_writers_but_allows_wal_readers() {
        let path = std::env::temp_dir().join(format!("library-writer-{}.sqlite3", uuid::Uuid::new_v4()));
        let db = Database::open(&path).unwrap();
        db.initialize_library().unwrap();
        db.connection.execute_batch("CREATE TABLE transaction_probe(value INTEGER);").unwrap();
        let other = rusqlite::Connection::open(&path).unwrap();
        other.busy_timeout(std::time::Duration::ZERO).unwrap();
        db.with_write_transaction(|_| {
            assert_eq!(other.query_row("SELECT COUNT(*) FROM transaction_probe", [], |r| r.get::<_, i64>(0))?, 0);
            let error = other.execute("INSERT INTO transaction_probe VALUES (1)", []).unwrap_err();
            assert!(matches!(error, rusqlite::Error::SqliteFailure(ref code, _) if code.code == rusqlite::ErrorCode::DatabaseBusy));
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        other.execute("INSERT INTO transaction_probe VALUES (2)", []).unwrap();
        drop(other);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
