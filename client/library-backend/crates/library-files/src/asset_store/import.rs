//! Import storage lifecycle. Browser objects are committed asynchronously before
//! the metadata transaction; native files are published against its placements.
use super::{AssetStore, AssetStoreError, PublishedStagedBook, StagedBook};
use sync_common::{ContentHash, DirId};

impl AssetStore {
    pub async fn stage_import(&self, db: &library_database::Database, parent: DirId, name: String, source: Box<dyn std::io::Read + Send>) -> Result<StagedBook, AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let relative = self.prepare_import_path(db, parent, name).await?;
            self.stage_book(relative, source).await
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (db, parent, name);
            self.stage_reader(source).await
        }
    }
}

/// Verified storage ready to participate in the caller's metadata transaction.
/// The caller must hold its content lease through preparation and book.
pub struct PreparedImport {
    #[cfg(not(target_arch = "wasm32"))]
    staged: StagedBook,
}

impl StagedBook {
    pub async fn prepare_import(self) -> Result<PreparedImport, AssetStoreError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Ok(PreparedImport { staged: self })
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.commit().await?;
            Ok(PreparedImport {})
        }
    }
}

impl PreparedImport {
    /// Connection-free filesystem book from caller-resolved snapshots.
    /// The caller records the returned placement through the database commit.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn publish_snapshot(self, store: &AssetStore, relative_path: &str, occupied_names: &[String]) -> Result<PublishedStagedBook, AssetStoreError> {
        self.staged.publish_snapshot(store, relative_path, occupied_names)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl AssetStore {
    pub async fn prepare_import_path(&self, db: &library_database::Database, parent: DirId, name: String) -> Result<String, AssetStoreError> {
        self.placement().prepare_directory_with_database(db, parent).await?;
        db.native_book_path(parent, &name).map_err(|error| AssetStoreError::operation(error))?.ok_or_else(|| AssetStoreError::operation("prepared directory no longer exists".to_owned()))
    }
}
