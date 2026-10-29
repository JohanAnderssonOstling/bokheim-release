//! Shared PostgreSQL connection and initial schema setup.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;
use std::sync::Arc;
use tokio::sync::Notify;
use sync_common::SyncGeneration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatabasePoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Default for DatabasePoolConfig {
    fn default() -> Self {
        Self { max_connections: 32, min_connections: 2, acquire_timeout: Duration::from_secs(5), idle_timeout: Duration::from_secs(10 * 60) }
    }
}

impl DatabasePoolConfig {
    pub fn from_env() -> Result<Self, String> {
        Self::from_values(|name| std::env::var(name).ok())
    }

    fn from_values(mut value: impl FnMut(&str) -> Option<String>) -> Result<Self, String> {
        let defaults = Self::default();
        let max_connections = parse_bounded_u32(value("BOKHEIM_DATABASE_MAX_CONNECTIONS"), defaults.max_connections, 2, 256, "BOKHEIM_DATABASE_MAX_CONNECTIONS")?;
        let min_connections = parse_bounded_u32(value("BOKHEIM_DATABASE_MIN_CONNECTIONS"), defaults.min_connections, 0, max_connections, "BOKHEIM_DATABASE_MIN_CONNECTIONS")?;
        let acquire_seconds = parse_bounded_u64(value("BOKHEIM_DATABASE_ACQUIRE_TIMEOUT_SECONDS"), defaults.acquire_timeout.as_secs(), 1, 60, "BOKHEIM_DATABASE_ACQUIRE_TIMEOUT_SECONDS")?;
        let idle_seconds = parse_bounded_u64(value("BOKHEIM_DATABASE_IDLE_TIMEOUT_SECONDS"), defaults.idle_timeout.as_secs(), 30, 24 * 60 * 60, "BOKHEIM_DATABASE_IDLE_TIMEOUT_SECONDS")?;
        Ok(Self { max_connections, min_connections, acquire_timeout: Duration::from_secs(acquire_seconds), idle_timeout: Duration::from_secs(idle_seconds) })
    }
}

fn parse_bounded_u32(raw: Option<String>, default: u32, minimum: u32, maximum: u32, name: &str) -> Result<u32, String> {
    let value = raw.map(|raw| raw.parse::<u32>().map_err(|_| format!("{name} must be an integer"))).transpose()?.unwrap_or(default);
    (minimum..=maximum).contains(&value).then_some(value).ok_or_else(|| format!("{name} must be between {minimum} and {maximum}"))
}

fn parse_bounded_u64(raw: Option<String>, default: u64, minimum: u64, maximum: u64, name: &str) -> Result<u64, String> {
    let value = raw.map(|raw| raw.parse::<u64>().map_err(|_| format!("{name} must be an integer"))).transpose()?.unwrap_or(default);
    (minimum..=maximum).contains(&value).then_some(value).ok_or_else(|| format!("{name} must be between {minimum} and {maximum}"))
}

#[derive(Clone)]
pub struct PostgresDatabase {
    pool: PgPool,
    cleanup: Arc<Notify>,
}

impl PostgresDatabase {
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool, cleanup: Arc::new(Notify::new()) }
    }

    /// Shared by the account, sync and asset services within this server.
    pub fn asset_cleanup_signal(&self) -> Arc<Notify> { self.cleanup.clone() }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

pub async fn connect(database_url: &str) -> Result<PostgresDatabase, sqlx::Error> {
    PgPool::connect(database_url).await.map(PostgresDatabase::from_pool)
}

pub async fn connect_configured(database_url: &str, config: DatabasePoolConfig) -> Result<PostgresDatabase, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(config.acquire_timeout)
        .idle_timeout(Some(config.idle_timeout))
        .connect(database_url)
        .await
        .map(PostgresDatabase::from_pool)
}

const SCHEMA_VERSION: i32 = 12;

/// Applies the initial schema to an empty database. Files are
/// applied in filename order -- `01_core.sql` before the domain files that
/// reference it.
pub async fn migrate(database: &PostgresDatabase) -> Result<(), sqlx::Error> {
    const SCHEMA_FILES: &[&str] = &[include_str!("../schema/01_core.sql"), include_str!("../schema/02_accounts.sql"), include_str!("../schema/03_sync.sql"), include_str!("../schema/04_assets.sql")];
    for file in SCHEMA_FILES {
        sqlx::raw_sql(file).execute(database.pool()).await?;
    }
    Ok(())
}

/// Adopt the deployed pre-release baseline and upgrade it to the current schema
/// in one transaction, preserving data.
/// Startup remains read-only; only the explicit deployment command upgrades it.
pub async fn upgrade_existing(database: &PostgresDatabase) -> Result<(), sqlx::Error> {
    let mut transaction = database.pool().begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('bokheim-schema-upgrade'))").execute(&mut *transaction).await?;
    let versioned: bool = sqlx::query_scalar("SELECT to_regclass('bokheim_schema_version') IS NOT NULL").fetch_one(&mut *transaction).await?;
    if !versioned {
        sqlx::raw_sql(include_str!("../schema/migrations/adopt_release_baseline.sql")).execute(&mut *transaction).await?;
    }
    let version: i32 = sqlx::query_scalar("SELECT version FROM bokheim_schema_version").fetch_one(&mut *transaction).await?;
    if !(1..=SCHEMA_VERSION).contains(&version) {
        return Err(sqlx::Error::Protocol(format!("unsupported sync schema version {version}; expected {SCHEMA_VERSION}")));
    }
    if version == 1 {
        sqlx::raw_sql(include_str!("../schema/migrations/book_owner.sql")).execute(&mut *transaction).await?;
    }
    if version <= 2 {
        sqlx::raw_sql(include_str!("../schema/migrations/shrink_identity.sql")).execute(&mut *transaction).await?;
    }
    if version <= 3 {
        sqlx::raw_sql(include_str!("../schema/migrations/merge_sync_state.sql")).execute(&mut *transaction).await?;
    }
    if version <= 4 {
        sqlx::raw_sql(include_str!("../schema/migrations/unify_content_hash.sql")).execute(&mut *transaction).await?;
    }
    if version <= 5 {
        sqlx::raw_sql(include_str!("../schema/migrations/raw_content_hash.sql")).execute(&mut *transaction).await?;
    }
    if version <= 6 {
        sqlx::raw_sql(include_str!("../schema/migrations/raw_state_keys.sql")).execute(&mut *transaction).await?;
    }
    if version <= 7 {
        sqlx::raw_sql(include_str!("../schema/migrations/immutable_book_records.sql")).execute(&mut *transaction).await?;
    }
    if version <= 8 {
        sqlx::raw_sql(include_str!("../schema/migrations/completed_book_additions.sql")).execute(&mut *transaction).await?;
    }
    if version <= 9 {
        sqlx::raw_sql(include_str!("../schema/migrations/opds_feeds.sql")).execute(&mut *transaction).await?;
    }
    if version <= 10 {
        sqlx::raw_sql(include_str!("../schema/migrations/sync_history.sql")).execute(&mut *transaction).await?;
    }
    if version <= 11 {
        sqlx::raw_sql(include_str!("../schema/migrations/storage_provider.sql")).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    Ok(())
}

/// Reject databases created with earlier development schemas before activating a release.
pub async fn verify_schema(database: &PostgresDatabase) -> Result<(), sqlx::Error> {
    let version: i32 = sqlx::query_scalar("SELECT version FROM bokheim_schema_version").fetch_one(database.pool()).await?;
    if version != SCHEMA_VERSION {
        return Err(sqlx::Error::Protocol(format!("unsupported sync schema version {version}; expected {SCHEMA_VERSION}")));
    }
    Ok(())
}

/// Invalidates pre-restore checkpoints without changing state or authored
/// versions. Run after restoring/replacing history, before resuming traffic.
pub async fn rotate_sync_generation(database: &PostgresDatabase) -> Result<SyncGeneration, sqlx::Error> {
    verify_schema(database).await?;
    let generation = SyncGeneration::new_v4();
    sqlx::query_scalar("UPDATE sync_history SET generation=$1 WHERE singleton=true RETURNING generation").bind(generation).fetch_one(database.pool()).await
}

pub async fn check_ready(database: &PostgresDatabase) -> Result<(), sqlx::Error> {
    sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(database.pool()).await.map(|_| ())
}
