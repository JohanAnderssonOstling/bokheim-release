//! Database-backed placement policy.
//!
//! `library-files` owns concrete storage operations; this module owns the
//! SQLite state transitions that decide which operations are required.
use crate::ContentHash;
use library_database::Database;
use library_files::{AssetStore, AssetStoreError};
use library_replica::BlobKind;

pub(crate) trait AssetPlacementWorkflow {
    fn evict_local_book(&self, database: &Database, hash: &ContentHash) -> Result<(), AssetStoreError>;
    fn placement_work(&self, database: &Database) -> Result<(bool, Option<String>), AssetStoreError>;
}

impl AssetPlacementWorkflow for AssetStore {
    fn evict_local_book(&self, database: &Database, hash: &ContentHash) -> Result<(), AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        if database.is_book_downloaded(hash)? {
            let paths = database.book_paths(hash)?;
            self.evict_book_files(&paths, hash)?;
        }
        if !database.evict_local_book(hash)? {
            return Ok(());
        }
        self.remove(BlobKind::Book, hash).map(|_| ())
    }
    fn placement_work(&self, database: &Database) -> Result<(bool, Option<String>), AssetStoreError> {
        Ok(database.local_file_work_status()?)
    }
}
