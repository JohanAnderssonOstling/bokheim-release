use crate::{Database, DatabaseError};

impl Database {
    /// Refresh outside the read transaction, then validate inside its snapshot.
    /// A racing writer can force another refresh, but the view is read only once.
    pub(super) fn with_cached_browse_snapshot<T>(
        &self, mut refresh: impl FnMut() -> Result<(), DatabaseError>, mut is_clean: impl FnMut(&rusqlite::Connection) -> Result<bool, DatabaseError>, read: impl FnOnce() -> Result<T, DatabaseError>,
    ) -> Result<T, DatabaseError> {
        loop {
            refresh()?;
            let snapshot = self.connection.unchecked_transaction()?;
            if !is_clean(&snapshot)? {
                snapshot.rollback()?;
                continue;
            }
            let result = read()?;
            snapshot.commit()?;
            return Ok(result);
        }
    }

    /// Hold one WAL read snapshot across a view's component queries.
    /// Membership-backed views use their cache refresh-and-retry boundary instead.
    pub(super) fn with_browse_snapshot<T>(&self, read: impl FnOnce() -> Result<T, DatabaseError>) -> Result<T, DatabaseError> {
        let snapshot = self.connection.unchecked_transaction()?;
        let result = read()?;
        snapshot.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn stale_snapshot_is_released_before_refresh_and_view_is_read_once() {
        let db = Database::open(":memory:").unwrap();
        let refreshes = Cell::new(0);
        let reads = Cell::new(0);
        let value = db
            .with_cached_browse_snapshot(
                || {
                    assert!(db.connection.is_autocommit());
                    refreshes.set(refreshes.get() + 1);
                    Ok(())
                },
                |connection| {
                    assert!(!connection.is_autocommit());
                    Ok(refreshes.get() == 2)
                },
                || {
                    assert!(!db.connection.is_autocommit());
                    reads.set(reads.get() + 1);
                    Ok(42)
                },
            )
            .unwrap();
        assert_eq!(value, 42);
        assert_eq!(refreshes.get(), 2);
        assert_eq!(reads.get(), 1);
        assert!(db.connection.is_autocommit());
    }

    #[test]
    fn cache_validation_and_view_errors_release_the_snapshot() {
        let db = Database::open(":memory:").unwrap();
        for fail_validation in [true, false] {
            let read_called = Cell::new(false);
            let result: Result<(), DatabaseError> = db.with_cached_browse_snapshot(
                || Ok(()),
                |_| if fail_validation { Err(DatabaseError::message("validation failed")) } else { Ok(true) },
                || {
                    read_called.set(true);
                    Err(DatabaseError::message("view failed"))
                },
            );
            assert!(result.is_err());
            assert_eq!(read_called.get(), !fail_validation);
            assert!(db.connection.is_autocommit());
        }
    }
}
