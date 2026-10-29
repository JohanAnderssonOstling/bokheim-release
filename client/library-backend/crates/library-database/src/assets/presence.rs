use super::TransfersSql;
use crate::*;
use library_model::{BlobKind, TransferOrigin};
use std::collections::HashSet;
use sync_common::BookId;

impl Database {
    /// Records that the server already holds these bytes.
    pub fn record_remote_asset(&self, kind: BlobKind, hash: &BookId) -> Result<(), DatabaseError> {
        self.connection.record_remote_asset(kind.storage(), hash)?;
        Ok(())
    }

    /// Forgets server-side presence, e.g. after a local edit.
    pub fn forget_remote_asset(&self, kind: BlobKind, hash: &BookId) -> Result<(), DatabaseError> {
        self.connection.forget_remote_asset(kind.storage(), hash)?;
        Ok(())
    }

    /// Hashes the server is known to hold for one asset kind.
    pub fn remote_asset_hashes(&self, kind: BlobKind) -> Result<HashSet<BookId>, DatabaseError> {
        if self.storage_policy()?.external().is_some() {
            let mut query = self.connection.prepare("SELECT book_id FROM provider_book WHERE available AND (?1='book' OR thumbnails IS NOT NULL)")?;
            return query.query_map([kind.storage()], |row| row.get(0))?.collect::<Result<HashSet<_>, _>>().map_err(DatabaseError::from);
        }
        let mut hashes = HashSet::new();
        self.connection.remote_asset_hashes(kind.storage(), |row| {
            hashes.insert(row.get::<_, BookId>(0)?);
            Ok(())
        })?;
        Ok(hashes)
    }

    /// Requested book downloads, oldest first.
    pub fn book_download_requests(&self, after_at: i64, after_hash: Option<&BookId>, limit: i64) -> Result<Vec<(BookId, i64)>, DatabaseError> {
        let mut rows = Vec::new();
        self.connection.book_download_requests(after_at, after_hash, limit, |row| {
            rows.push((row.get::<_, BookId>(0)?, row.get(1)?));
            Ok(())
        })?;
        Ok(rows)
    }

    pub fn book_download_origin(&self, hash: &BookId) -> Result<Option<TransferOrigin>, DatabaseError> {
        let mut origin = None;
        self.connection.book_download_origin(hash, |row| {
            origin = TransferOrigin::parse_storage(&row.get::<_, String>(0)?);
            Ok(())
        })?;
        Ok(origin)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn download_cursor_pages_raw_hashes_at_equal_timestamps() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let first = BookId::from_bytes([0; 32]);
        let second = BookId::from_bytes([0xff; 32]);
        for hash in [second, first] {
            db.connection.fixtures_insert_book_7(&hash).unwrap();
            db.connection.fixtures_insert_book_transfer(&hash).unwrap();
        }
        assert_eq!(db.book_download_requests(-1, None, 1).unwrap(), vec![(first, 10)]);
        assert_eq!(db.book_download_requests(10, Some(&first), 1).unwrap(), vec![(second, 10)]);
        assert!(db.book_download_requests(10, Some(&second), 1).unwrap().is_empty());
    }
}

#[cfg(test)]
use crate::sql_fixtures::FixturesSql;
