//! Shared PostgreSQL capability plus synchronization and asset persistence.
//! Account persistence remains implemented by `server-account`.

mod applications;
mod assets;
mod cloud_storage;
mod database;
mod sync;
mod sync_store;
#[cfg(test)]
mod test_support;

pub use applications::{AssetApplication, LibraryStorageUsage, StorageUsage};
pub async fn configure_user_storage_quota(database: &PostgresDatabase, user_id: &str, quota_bytes: i64) -> Result<i64, sqlx::Error> {
    assets::configure_user_storage_quota(database, user_id, quota_bytes).await
}
pub use database::{check_ready, connect, connect_configured, migrate, verify_schema, DatabasePoolConfig, PostgresDatabase};
pub use sync::{PostgresSyncRepository, SyncError};
mod book_admission;
#[cfg(test)]
mod book_admission_tests;
