use super::TransfersSql;
use crate::shared_sql::SharedSql;
use crate::*;
use library_model::DownloadState;
use library_replica::BlobKind;
use sync_common::ContentHash;

impl Database {
    pub fn is_book_downloaded(&self, hash: &ContentHash) -> Result<bool, DatabaseError> {
        let mut downloaded = false;
        self.connection.transfer_is_book_downloaded(hash.as_str(), |row| {
            downloaded = row.get(0)?;
            Ok(())
        })?;
        Ok(downloaded)
    }

    pub fn evict_local_book(&self, hash: &ContentHash) -> Result<bool, DatabaseError> {
        if !self.is_book_downloaded(hash)? {
            return Ok(false);
        }
        // Eviction keeps the entry but drops its bytes. Without a server copy
        // those bytes are the only copy, so refusing is what keeps the entry
        // recoverable.
        if !self.remote_asset_hashes(BlobKind::Book)?.contains(hash) {
            return Err(DatabaseError::message("this book is only stored on this device"));
        }
        let transaction = self.connection.unchecked_transaction()?;
        transaction.shared_transfer_complete_download(hash.as_str())?;
        transaction.shared_set_book_hash_downloaded(hash.as_str(), 0)?;
        #[cfg(not(target_arch = "wasm32"))]
        transaction.shared_cancel_book_restore(hash.as_str())?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn local_file_work_status(&self) -> Result<(bool, Option<String>), DatabaseError> {
        #[cfg(target_arch = "wasm32")]
        return Ok((false, None));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut pending = false;
            self.connection.transfer_has_pending_file_work(|row| {
                pending = row.get(0)?;
                Ok(())
            })?;
            Ok((pending, None))
        }
    }

    pub fn has_pending_asset_work(&self) -> Result<bool, DatabaseError> {
        let mut pending = false;
        self.connection.transfer_has_pending_asset_work(|row| {
            pending = row.get(0)?;
            Ok(())
        })?;
        Ok(pending)
    }

    pub fn requested_book_download(&self, hash: ContentHash) -> Result<Option<BookPlacement>, DatabaseError> {
        match self.book_path(&hash)? {
            Some(path) => Ok(Some(BookPlacement::new(hash, path))),
            None => {
                self.connection.shared_transfer_complete_download(hash.as_str())?;
                Ok(None)
            }
        }
    }

    pub fn pending_download_requests(&self) -> Result<Vec<ContentHash>, DatabaseError> {
        let mut hashes = Vec::new();
        self.connection.transfer_pending_download_requests(|row| {
            hashes.push(ContentHash::new(&row.get::<_, String>(0)?));
            Ok(())
        })?;
        Ok(hashes)
    }

    pub fn complete_local_book_request(&self, hash: ContentHash) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        Self::complete_local_book_request_on(&transaction, &hash)?;
        transaction.commit().map_err(Into::into)
    }

    pub(super) fn complete_local_book_request_on(connection: &rusqlite::Connection, hash: &ContentHash) -> Result<(), DatabaseError> {
        connection.shared_set_book_hash_downloaded(hash.as_str(), 1)?;
        let mut placements = Vec::new();
        connection.transfer_live_placements(hash.as_str(), |row| {
            placements.push(row.get::<_, String>(2)?);
            Ok(())
        })?;
        for path in placements {
            connection.transfer_queue_restore(hash.as_str(), &path)?;
        }
        connection.shared_transfer_complete_download(hash.as_str())?;
        Ok(())
    }

    pub fn complete_retained_download(&self, hash: &ContentHash, placement: &BookPlacement) -> Result<bool, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let matches = Self::complete_retained_download_on(&transaction, hash, placement)?;
        transaction.commit()?;
        Ok(matches)
    }

    pub(super) fn complete_retained_download_on(connection: &rusqlite::Connection, hash: &ContentHash, placement: &BookPlacement) -> Result<bool, DatabaseError> {
        let mut matches = false;
        connection.transfer_placement_matches(hash.as_str(), placement.rel_path.as_str(), |row| {
            matches = row.get(0)?;
            Ok(())
        })?;
        if !matches {
            return Ok(false);
        }
        connection.shared_set_book_hash_downloaded(hash.as_str(), 1)?;
        connection.shared_transfer_complete_download(hash.as_str())?;
        Ok(true)
    }

    pub fn cancel_book_download(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        Self::cancel_book_download_on(&transaction, hash)?;
        transaction.commit().map_err(Into::into)
    }

    pub(super) fn cancel_book_download_on(connection: &rusqlite::Connection, hash: &ContentHash) -> Result<(), DatabaseError> {
        connection.forget_remote_asset(BlobKind::Book.storage(), hash.as_str())?;
        connection.shared_transfer_complete_download(hash.as_str())?;
        Ok(())
    }

    pub fn book_placement_is_current(&self, placement: &BookPlacement, hash: &ContentHash) -> Result<bool, DatabaseError> {
        if placement.content_hash != *hash {
            return Ok(false);
        }
        let mut matches = false;
        self.connection.transfer_placement_matches(hash.as_str(), placement.rel_path.as_str(), |row| {
            matches = row.get(0)?;
            Ok(())
        })?;
        Ok(matches)
    }

    pub fn native_download_directory(&self, placement: &BookPlacement, hash: &ContentHash) -> Result<Option<sync_common::DirId>, DatabaseError> {
        if placement.content_hash != *hash {
            return Ok(None);
        }
        let mut raw_directory = None;
        self.connection.transfer_placement_directory(hash.as_str(), placement.rel_path.as_str(), |row| {
            raw_directory = Some(row.get::<_, String>(0)?);
            Ok(())
        })?;
        raw_directory.map(|id| sync_common::DirId::parse_str(&id).map_err(DatabaseError::operation)).transpose()
    }

    pub fn sync_request_book_downloads(&self, hashes: &[ContentHash], origin: library_replica::TransferOrigin) -> Result<(), DatabaseError> {
        let created_at = super::unix_secs()?;
        let transaction = self.connection.unchecked_transaction()?;
        for hash in hashes {
            let mut placed = false;
            transaction.transfer_download_placement(hash.as_str(), |row| {
                placed = row.get(0)?;
                Ok(())
            })?;
            if !placed {
                return Err(DatabaseError::message(format!("book {hash} has no live placement")));
            }
            transaction.transfer_add_download_request(hash.as_str(), origin.storage(), created_at)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn transfer_download_state(&self, hash: &ContentHash, bytes_present: bool) -> Result<DownloadState, DatabaseError> {
        let mut state = (false, false);
        self.connection.transfer_download_state(hash.as_str(), |row| {
            state = (row.get(0)?, row.get(1)?);
            Ok(())
        })?;
        Ok(match (state.0, state.1, bytes_present) {
            (true, _, true) => DownloadState::Downloaded,
            (_, true, _) => DownloadState::Queued,
            _ => DownloadState::NotDownloaded,
        })
    }

    pub fn transfer_first_file_names(&self, hashes: &[ContentHash]) -> Result<Vec<(ContentHash, String)>, DatabaseError> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let payload = serde_json::to_string(&hashes.iter().map(ContentHash::to_string).collect::<Vec<_>>()).map_err(DatabaseError::operation)?;
        let mut names = Vec::new();
        self.connection.transfer_first_file_names(&payload, |row| {
            let hash = ContentHash::new(&row.get::<_, String>(0)?);
            if let Some(name) = row.get::<_, Option<String>>(1)? {
                names.push((hash, name));
            }
            Ok(())
        })?;
        Ok(names)
    }

    pub fn is_book_download_requested(&self, hash: &ContentHash) -> Result<bool, DatabaseError> {
        let mut requested = false;
        self.connection
            .download_requested(hash.as_str(), |row| {
                requested = row.get(0)?;
                Ok(())
            })
            .map_err(DatabaseError::operation)?;
        Ok(requested)
    }
}
