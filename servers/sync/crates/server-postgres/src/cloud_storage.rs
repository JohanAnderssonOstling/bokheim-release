//! Account-wide asset retention, independent of library metadata lifecycle.
use crate::{assets, PostgresSyncRepository, SyncError};
use sync_common::{LibraryId, ContentHash, api::libraries::LibraryCloudStorage};

fn internal(error: impl std::fmt::Display) -> SyncError { SyncError::Internal(error.to_string()) }

impl PostgresSyncRepository {
    pub async fn cloud_storage(&self, user: &str) -> Result<Vec<LibraryCloudStorage>, SyncError> {
        let rows = sqlx::query_as::<_, (String, bool)>(include_str!("sql/cloud_storage/list.sql")).bind(user).fetch_all(&self.pool).await.map_err(internal)?;
        rows.into_iter().map(|(id, enabled)| Ok(LibraryCloudStorage { library_id: LibraryId::parse_str(&id).map_err(internal)?, enabled })).collect()
    }

    pub async fn set_cloud_storage(&self, user: &str, id: &LibraryId, change_id: uuid::Uuid, enabled: bool) -> Result<LibraryCloudStorage, SyncError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let current: Option<bool> = sqlx::query_scalar(include_str!("sql/cloud_storage/lock.sql")).bind(id.to_string()).bind(user).fetch_optional(&mut *tx).await.map_err(internal)?;
        let Some(current) = current else { return Err(SyncError::LibraryOwnershipConflict) };
        let inserted: Option<uuid::Uuid> = sqlx::query_scalar(include_str!("sql/cloud_storage/record_change.sql")).bind(id.to_string()).bind(change_id).fetch_optional(&mut *tx).await.map_err(internal)?;
        if inserted.is_none() {
            tx.commit().await.map_err(internal)?;
            return Ok(LibraryCloudStorage { library_id: *id, enabled: current });
        }
        if current != enabled {
            sqlx::query(include_str!("sql/account/ensure_exists.sql")).bind(user).execute(&mut *tx).await.map_err(internal)?;
            sqlx::query(include_str!("sql/cloud_storage/lock_account.sql")).bind(user).execute(&mut *tx).await.map_err(internal)?;
            if !enabled { sqlx::query(include_str!("sql/cloud_storage/delete_claims.sql")).bind(id.to_string()).execute(&mut *tx).await.map_err(internal)?; }
            let hashes: Vec<String> = sqlx::query_scalar(include_str!("sql/cloud_storage/references.sql")).bind(id.to_string()).fetch_all(&mut *tx).await.map_err(internal)?;
            assets::apply_lifecycle_reference_deltas(&mut tx, user, hashes.into_iter().map(|hash| (ContentHash::new(&hash), if enabled { 1 } else { -1 }))).await.map_err(internal)?;
            sqlx::query(include_str!("sql/cloud_storage/update.sql")).bind(id.to_string()).bind(user).bind(enabled).execute(&mut *tx).await.map_err(internal)?;
            if !enabled {
                sqlx::query(include_str!("sql/cloud_storage/release_unreferenced.sql")).bind(user).execute(&mut *tx).await.map_err(internal)?;
                sqlx::query(include_str!("sql/cloud_storage/release_reservations.sql")).bind(id.to_string()).bind(user).execute(&mut *tx).await.map_err(internal)?;
            }
        }
        tx.commit().await.map_err(internal)?;
        Ok(LibraryCloudStorage { library_id: *id, enabled })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use server_asset_store::AssetError;
    use crate::assets::BookUploadAdmission;
    use sync_common::api::assets::BlobManifestEntry;

    #[tokio::test]
    async fn cloud_storage_releases_quota_preserves_shared_books_and_blocks_late_uploads() {
        let pool = crate::test_support::isolated_pool("cloud_storage", 6).await.expect("SYNC_E2E_DATABASE_URL is required");
        let database = crate::PostgresDatabase::from_pool(pool.clone());
        crate::verify_schema(&database).await.unwrap();
        let repo = PostgresSyncRepository::new(database.clone());
        let user = uuid::Uuid::new_v4().to_string();
        sqlx::query(include_str!("sql/cloud_storage/test_seed_user.sql")).bind(&user).bind(format!("{user}@example.com")).execute(&pool).await.unwrap();
        crate::assets::configure_user_storage_quota(&crate::PostgresDatabase::from_pool(pool.clone()), &user, 1000).await.unwrap();
        let a = LibraryId::new_v4(); let b = LibraryId::new_v4();
        for id in [a,b] { repo.create_library(&user, &sync_common::api::libraries::LibraryNameRequest { library_id: id, library_name: "Books".into() }).await.unwrap(); }
        let hash = ContentHash::new(&"a".repeat(64));
        let revision = ContentHash::new(&"d".repeat(64));
        let admission = assets::reserve_book_upload(&pool, &user, &a, &hash, 60).await.unwrap();
        assets::finish_book_revision_upload(&pool, &user, &a, &hash, &hash, &admission, 60).await.unwrap();
        // Claim it in a second library before either library publishes metadata.
        let mut manifest = [BlobManifestEntry { content_hash: hash, checksum: hash, size_bytes: 60 }];
        assets::negotiate_books(&pool, &user, &b, &manifest, &[hash, revision].into_iter().collect()).await.unwrap();
        let revised = assets::reserve_book_upload(&pool, &user, &a, &revision, 70).await.unwrap();
        assets::finish_book_revision_upload(&pool, &user, &a, &hash, &revision, &revised, 70).await.unwrap();
        assert_eq!(assets::storage_usage(&pool, &user).await.unwrap().used_bytes, 70, "shared identity revisions must not retain an obsolete charge");
        manifest[0].checksum = revision;
        manifest[0].size_bytes = 70;
        let pending = ContentHash::new(&"b".repeat(64));
        let late = assets::reserve_book_upload(&pool, &user, &a, &pending, 20).await.unwrap();
        assert_eq!(assets::storage_usage(&pool, &user).await.unwrap().reserved_bytes, 20);
        let off = uuid::Uuid::new_v4();
        repo.set_cloud_storage(&user, &a, off, false).await.unwrap();
        let usage = assets::storage_usage(&pool, &user).await.unwrap();
        assert_eq!((usage.used_bytes, usage.reserved_bytes), (70,0));
        assert!(matches!(assets::finish_book_revision_upload(&pool, &user, &a, &pending, &pending, &late, 20).await, Err(AssetError::Forbidden)));
        assert!(matches!(assets::reserve_book_upload(&pool, &user, &a, &hash, 60).await, Err(AssetError::Forbidden)));
        assert!(matches!(assets::negotiate_books(&pool, &user, &a, &manifest, &[hash, revision].into_iter().collect()).await, Err(AssetError::Forbidden)));
        assert!(repo.set_cloud_storage("other-user", &a, uuid::Uuid::new_v4(), true).await.is_err());
        repo.set_cloud_storage(&user, &b, uuid::Uuid::new_v4(), false).await.unwrap();
        assert_eq!(assets::storage_usage(&pool, &user).await.unwrap().used_bytes, 0);
        crate::verify_schema(&database).await.unwrap();
        assert!(repo.cloud_storage(&user).await.unwrap().iter().all(|library| !library.enabled));
        repo.set_cloud_storage(&user, &a, uuid::Uuid::new_v4(), true).await.unwrap();
        // Retrying an acknowledged older change cannot undo a later choice.
        assert!(repo.set_cloud_storage(&user, &a, off, false).await.unwrap().enabled);
        assets::negotiate_books(&pool, &user, &a, &manifest, &[hash, revision].into_iter().collect()).await.unwrap();
        assert_eq!(assets::storage_usage(&pool, &user).await.unwrap().used_bytes, 70);
        assert_eq!(repo.list_libraries(&user).await.unwrap().len(), 2);
        assert!(matches!(late, BookUploadAdmission::Reserved(_)));
    }
}
