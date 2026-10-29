//! Shared PostgreSQL capability plus synchronization and asset persistence.
//! Account persistence remains implemented by `server-account`.

mod applications;
mod assets;
mod cloud_storage;
mod cleanup;
mod database;
mod opds_feeds;
mod sync;
mod sync_store;

pub use cleanup::CleanupResult;
pub use applications::{AssetApplication, LibraryStorageUsage, StorageUsage};
pub async fn configure_user_storage_quota(database: &PostgresDatabase, user_id: &str, quota_bytes: i64) -> Result<i64, sqlx::Error> {
    assets::configure_user_storage_quota(database, user_id, quota_bytes).await
}
pub use database::{check_ready, connect, connect_configured, migrate, rotate_sync_generation, upgrade_existing, verify_schema, DatabasePoolConfig, PostgresDatabase};
pub use sync::{PostgresSyncRepository, SyncError};
