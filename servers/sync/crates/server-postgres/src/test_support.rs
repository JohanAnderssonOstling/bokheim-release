//! Shared by every crate test module that needs a live Postgres.
//!
//! Baseline schema application is intentionally not repeatable -- each test
//! gets its own schema via `search_path` so many
//! tests can run against the same `SYNC_E2E_DATABASE_URL` without colliding
//! on tables an earlier test already created.

use crate::PostgresDatabase;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// A connection URL scoped (via `search_path`) to a freshly created schema,
/// for tests that need more than one pool/connection sharing that schema
/// (e.g. to exercise an advisory lock held by one connection against
/// another). Callers are responsible for applying the schema themselves.
pub(crate) async fn isolated_schema_url(prefix: &str) -> Option<String> {
    let database_url = std::env::var("SYNC_E2E_DATABASE_URL").ok()?;
    let root = PgPoolOptions::new().max_connections(1).connect(&database_url).await.unwrap();
    let schema = format!("{prefix}_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&root).await.unwrap();
    drop(root);
    let separator = if database_url.contains('?') { '&' } else { '?' };
    Some(format!("{database_url}{separator}options=-csearch_path%3D{schema}"))
}

pub(crate) async fn isolated_pool(prefix: &str, max_connections: u32) -> Option<PgPool> {
    let scoped_url = isolated_schema_url(prefix).await?;
    let pool = PgPoolOptions::new().max_connections(max_connections).connect(&scoped_url).await.unwrap();
    crate::migrate(&PostgresDatabase::from_pool(pool.clone())).await.unwrap();
    Some(pool)
}
