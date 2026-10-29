//! Unreferenced asset cleanup. Publication and collection share a database
//! advisory lock, so a successful upload cannot lose its file to a concurrent
//! sweep. Network uploads are staged before taking the publication lock.

use crate::AssetApplication;
use server_asset_store::{AssetError, FsAssetStore};
use sqlx::{Postgres, Transaction};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use sync_common::BookId;
use tokio::time::{interval, MissedTickBehavior};

const ASSET_PUBLICATION_LOCK: i64 = 0x426f6b6865696d;

fn storage(error: impl std::fmt::Display) -> AssetError {
    AssetError::Storage(error.to_string())
}

pub(super) async fn lock_publication(tx: &mut Transaction<'_, Postgres>) -> Result<(), AssetError> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)").bind(ASSET_PUBLICATION_LOCK).execute(&mut **tx).await.map_err(storage)?;
    Ok(())
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct CleanupResult {
    pub books: usize,
    pub thumbnails: usize,
}

impl AssetApplication {
    /// Starts a sweep immediately, after a committed deletion, and every minute
    /// to retry failures and recover files orphaned by crashes or failed uploads.
    pub fn spawn_cleanup_worker(self: &Arc<Self>) {
        let assets = Arc::downgrade(self);
        let signal = self.cleanup.clone();
        tokio::spawn(async move {
            let mut retry = interval(Duration::from_secs(60));
            retry.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = signal.notified() => {},
                    _ = retry.tick() => {},
                }
                let Some(assets) = assets.upgrade() else {
                    break;
                };
                match assets.collect_unused_assets().await {
                    Ok(result) if result.books + result.thumbnails > 0 => tracing::info!(books = result.books, thumbnails = result.thumbnails, "removed unused assets"),
                    Ok(_) => {}
                    Err(error) => tracing::error!(%error, "asset cleanup failed; will retry"),
                }
            }
        });
    }

    pub async fn collect_unused_assets(&self) -> Result<CleanupResult, AssetError> {
        // Enumerate outside the lock, then recheck ownership under the lock for
        // each object. Never hold up upload publication for a directory scan.
        let mut books: HashSet<BookId> = self.store.book_hashes().await?.into_iter().collect();
        let recorded: Vec<Vec<u8>> = sqlx::query_scalar("SELECT content_hash FROM blob_object").fetch_all(&self.pool).await.map_err(storage)?;
        for hash in recorded {
            books.insert(BookId::try_from(hash.as_slice()).map_err(storage)?);
        }
        // Most objects remain owned. Filter them in one query rather than
        // taking a lock and querying ownership once for every stored book.
        let owned: HashSet<Vec<u8>> = sqlx::query_scalar("SELECT content_hash FROM user_blob_charge UNION SELECT content_hash FROM library_blob_claim").fetch_all(&self.pool).await.map_err(storage)?.into_iter().collect();
        books.retain(|hash| !owned.contains(hash.as_bytes()));
        let mut result = CleanupResult::default();
        for hash in books {
            let mut tx = self.pool.begin().await.map_err(storage)?;
            sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(ASSET_PUBLICATION_LOCK).execute(&mut *tx).await.map_err(storage)?;
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM user_blob_charge WHERE content_hash=$1)
                     OR EXISTS(SELECT 1 FROM library_blob_claim WHERE content_hash=$1)",
            )
            .bind(hash.as_bytes())
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
            if !owned {
                self.store.remove_book(&hash)?;
                sqlx::query("DELETE FROM blob_object WHERE content_hash=$1").bind(hash.as_bytes()).execute(&mut *tx).await.map_err(storage)?;
                result.books += 1;
            }
            tx.commit().await.map_err(storage)?;
        }

        let users: Vec<String> = sqlx::query_scalar("SELECT id FROM users").fetch_all(&self.pool).await.map_err(storage)?;
        let scopes: HashMap<_, _> = users.into_iter().map(|user| (FsAssetStore::user_scope(&user), user)).collect();
        let live_covers: Vec<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT DISTINCT library.user_id, state.content_hash FROM sync_state state JOIN libraries library ON library.id=state.library_id
             WHERE library.deleted_at IS NULL AND state.kind='book_lifecycle' AND state.version_rank<>2 AND state.content_hash IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let live_covers: HashSet<_> = live_covers.into_iter().map(|(user, hash)| BookId::try_from(hash.as_slice()).map(|hash| (FsAssetStore::user_scope(&user), hash)).map_err(storage)).collect::<Result<_, _>>()?;
        for (scope, hash) in self.store.thumbnail_objects().await? {
            if live_covers.contains(&(scope.clone(), hash)) {
                continue;
            }
            let mut tx = self.pool.begin().await.map_err(storage)?;
            sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(ASSET_PUBLICATION_LOCK).execute(&mut *tx).await.map_err(storage)?;
            // Covers belong to an account, unlike the shared book bytes. Keep
            // them while any live library in that account still holds the book.
            let owned: bool = if let Some(user) = scopes.get(&scope) {
                sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM sync_state state JOIN libraries library ON library.id=state.library_id
                     WHERE library.user_id=$1 AND library.deleted_at IS NULL AND state.kind='book_lifecycle'
                       AND state.version_rank<>2 AND state.content_hash=$2)",
                )
                .bind(user)
                .bind(hash.as_bytes())
                .fetch_one(&mut *tx)
                .await
                .map_err(storage)?
            } else {
                // Recheck users created since enumeration before treating an
                // unknown scope as an account that has been deleted.
                let current: Vec<String> = sqlx::query_scalar("SELECT id FROM users").fetch_all(&mut *tx).await.map_err(storage)?;
                current.iter().any(|user| FsAssetStore::user_scope(user) == scope)
            };
            if !owned {
                self.store.remove_scoped_thumbnail(&scope, &hash)?;
                result.thumbnails += 1;
            }
            tx.commit().await.map_err(storage)?;
        }
        Ok(result)
    }
}
