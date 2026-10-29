pub(crate) use super::TransfersSql;
pub(crate) use crate::shared_sql::SharedSql;
pub(crate) use crate::*;
pub(crate) use library_model::BlobKind;
pub(crate) use library_model::DownloadState;
pub(crate) use sync_common::BookId;

impl Database {
    pub fn is_book_downloaded(&self, hash: &BookId) -> Result<bool, DatabaseError> {
        Ok(self.download_flags(hash)?.0)
    }

    /// Whether the book's bytes are marked downloaded, and whether a live
    /// placement has a queued download request.
    pub(crate) fn download_flags(&self, hash: &BookId) -> Result<(bool, bool), DatabaseError> {
        let mut flags = (false, false);
        self.connection.transfer_download_state(hash, |row| {
            flags = (row.get(0)?, row.get(1)?);
            Ok(())
        })?;
        Ok(flags)
    }

    /// Checks eviction eligibility without changing local availability. Native
    /// callers must run this before removing any files, then commit eviction.
    pub fn check_local_book_eviction(&self, hash: &BookId) -> Result<bool, DatabaseError> {
        if !self.is_book_downloaded(hash)? {
            return Ok(false);
        }
        // Eviction keeps the entry but drops its bytes. Without a server copy
        // those bytes are the only copy, so refusing keeps the entry recoverable.
        if !self.remote_asset_hashes(BlobKind::Book)?.contains(hash) {
            return Err(DatabaseError::message("this book is only stored on this device; upload it before removing the download"));
        }
        Ok(true)
    }

    pub fn evict_local_book(&self, hash: &BookId) -> Result<bool, DatabaseError> {
        self.write(|transaction| {
            if !self.check_local_book_eviction(hash)? {
                return Ok(false);
            }
            transaction.shared_transfer_complete_download(hash)?;
            transaction.asset_clear_downloaded_placements(&(&hash))?;
            transaction.asset_clear_book_paths(&(&hash))?;
            #[cfg(not(target_arch = "wasm32"))]
            transaction.shared_cancel_book_materialization(hash)?;
            Ok(true)
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn complete_local_book_request_on(connection: &rusqlite::Connection, hash: &BookId) -> Result<(), DatabaseError> {
        let mut placements = Vec::new();
        connection.shared_live_book_placements(hash, |row| {
            placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
            Ok(())
        })?;
        let mut folders = crate::placements::FolderPaths::new(connection);
        for (dir_id, file_name) in placements {
            if let Some(path) = folders.placement(&dir_id, &file_name)? {
                connection.transfer_queue_materialization(hash, path.as_str())?;
            }
        }
        connection.shared_transfer_complete_download(hash)?;
        Ok(())
    }

    pub fn sync_request_book_downloads(&self, hashes: &[BookId], origin: library_model::TransferOrigin) -> Result<(), DatabaseError> {
        let requested_at = crate::sync::unix_millis()?;
        self.write(|transaction| {
            for hash in hashes {
                let mut placed = false;
                transaction.transfer_download_placement(hash, |row| {
                    placed = row.get(0)?;
                    Ok(())
                })?;
                if !placed {
                    return Err(DatabaseError::message("book has no live placement"));
                }
                transaction.shared_request_book_download(hash, origin.storage(), requested_at)?;
            }
            Ok(())
        })
    }

    pub fn transfer_download_state(&self, hash: &BookId, bytes_present: bool) -> Result<DownloadState, DatabaseError> {
        Ok(match (self.download_flags(hash)?, bytes_present) {
            ((true, _), true) => DownloadState::Downloaded,
            ((_, true), _) => DownloadState::Queued,
            _ => DownloadState::NotDownloaded,
        })
    }
}
