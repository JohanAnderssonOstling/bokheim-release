//! Account-wide asset retention, independent of library metadata lifecycle.

use crate::{assets, PostgresSyncRepository, SyncError};
use sync_common::{LibraryId, BookId, api::libraries::LibraryCloudStorage};

fn internal(error: impl std::fmt::Display) -> SyncError { SyncError::Internal(error.to_string()) }

impl PostgresSyncRepository {
    pub async fn cloud_storage(&self, user: &str) -> Result<Vec<LibraryCloudStorage>, SyncError> {
        let rows = sqlx::query_as::<_, (String, bool)>(include_str!("sql/cloud_storage/list.sql")).bind(user).fetch_all(&self.pool).await.map_err(internal)?;
        rows.into_iter().map(|(id, enabled)| Ok(LibraryCloudStorage { library_id: LibraryId::parse_str(&id).map_err(internal)?, enabled })).collect()
    }

    pub async fn set_cloud_storage(&self, user: &str, id: &LibraryId, change_id: uuid::Uuid, enabled: bool) -> Result<LibraryCloudStorage, SyncError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let current: Option<(bool, bool)> = sqlx::query_as(include_str!("sql/cloud_storage/lock.sql")).bind(id.to_string()).bind(user).fetch_optional(&mut *tx).await.map_err(internal)?;
        let Some((current, external)) = current else { return Err(SyncError::LibraryOwnershipConflict) };
        if external && enabled { return Err(SyncError::Forbidden); }
        let inserted: Option<uuid::Uuid> = sqlx::query_scalar(include_str!("sql/cloud_storage/record_change.sql")).bind(id.to_string()).bind(change_id).fetch_optional(&mut *tx).await.map_err(internal)?;
        if inserted.is_none() {
            tx.commit().await.map_err(internal)?;
            return Ok(LibraryCloudStorage { library_id: *id, enabled: current });
        }
        if current != enabled {
            sqlx::query(include_str!("sql/account/ensure_exists.sql")).bind(user).execute(&mut *tx).await.map_err(internal)?;
            sqlx::query(include_str!("sql/cloud_storage/lock_account.sql")).bind(user).execute(&mut *tx).await.map_err(internal)?;
            if !enabled { sqlx::query(include_str!("sql/cloud_storage/delete_claims.sql")).bind(id.to_string()).execute(&mut *tx).await.map_err(internal)?; }
            let hashes: Vec<Vec<u8>> = sqlx::query_scalar(include_str!("sql/cloud_storage/references.sql")).bind(id.to_string()).fetch_all(&mut *tx).await.map_err(internal)?;
            assets::apply_lifecycle_reference_deltas(&mut tx, user, hashes.into_iter().map(|hash| BookId::try_from(hash.as_slice()).map(|hash| (hash, if enabled { 1 } else { -1 }))).collect::<Result<Vec<_>, _>>().map_err(internal)?).await.map_err(internal)?;
            sqlx::query(include_str!("sql/cloud_storage/update.sql")).bind(id.to_string()).bind(user).bind(enabled).execute(&mut *tx).await.map_err(internal)?;
            if !enabled {
                sqlx::query(include_str!("sql/cloud_storage/release_unreferenced.sql")).bind(user).execute(&mut *tx).await.map_err(internal)?;
                sqlx::query(include_str!("sql/cloud_storage/release_reservations.sql")).bind(id.to_string()).bind(user).execute(&mut *tx).await.map_err(internal)?;
            }
        }
        tx.commit().await.map_err(internal)?;
        if !enabled { self.cleanup.notify_one(); }
        Ok(LibraryCloudStorage { library_id: *id, enabled })
    }
}
