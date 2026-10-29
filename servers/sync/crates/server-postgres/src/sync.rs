use std::time::Instant;
use std::sync::Arc;
use tokio::sync::Notify;
use sync_common::ConflictRank;
use crate::{PostgresDatabase, assets, sync_store};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashSet;
use sync_common::api::libraries::{LibraryNameRequest, LibrarySummary};
use sync_common::{PullStateQuery, PullStateResponse, PushMutationsResponse, SyncExchangeRequest, SyncExchangeResponse};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("authenticated user does not match the request")]
    Forbidden,
    #[error("library belongs to a different user")]
    LibraryOwnershipConflict,
    #[error("invalid library name")]
    InvalidLibraryName,
    #[error("invalid OPDS feed")]
    InvalidOpdsFeed,
    #[error("OPDS feed was removed or belongs to a different user")]
    OpdsFeedConflict,
    #[error("synchronization request contains too many mutations")]
    TooManyMutations,
    #[error("synchronization request contains an ambiguous mutation identity")]
    AmbiguousMutationIdentity,
    #[error("stored synchronization data requires a newer client")]
    UpgradeRequired,
    #[error("client synchronization cursor is ahead of server state")]
    CursorAhead,
    #[error("client synchronization cursor belongs to a different server history")]
    HistoryMismatch,
    #[error("synchronization persistence failed: {0}")]
    Internal(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryNameOutcome {
    pub library: LibrarySummary,
    pub changed: bool,
}

#[derive(Clone)]
pub struct PostgresSyncRepository {
    pub(super) pool: PgPool,
    pub(super) cleanup: Arc<Notify>,
}

impl PostgresSyncRepository {
    pub fn new(database: PostgresDatabase) -> Self {
        Self { pool: database.pool().clone(), cleanup: database.asset_cleanup_signal() }
    }

    async fn authorize_owned_library(&self, transaction: &mut Transaction<'_, Postgres>, user_id: &str, library_id: &sync_common::LibraryId) -> Result<bool, SyncError> {
        let row = sqlx::query(include_str!("sql/sync/authorize_owned_library.sql")).bind(library_id.to_string()).bind(user_id).fetch_optional(&mut **transaction).await.map_err(internal)?;
        Ok(row.is_some())
    }
}

fn pull_response(page: sync_store::FetchChangesPage) -> PullStateResponse {
    PullStateResponse { mutations: page.changes, next_cursor: page.next_cursor, has_more: page.has_more }
}

pub(crate) fn internal(error: impl std::fmt::Display) -> SyncError {
    SyncError::Internal(error.to_string())
}

fn fetch_error(error: sync_store::FetchChangesError) -> SyncError {
    match error {
        sync_store::FetchChangesError::CursorAhead => SyncError::CursorAhead,
        sync_store::FetchChangesError::HistoryMismatch => SyncError::HistoryMismatch,
        other => internal(other),
    }
}

impl PostgresSyncRepository {
    pub async fn list_libraries(&self, user_id: &str) -> Result<Vec<LibrarySummary>, SyncError> {
        sync_store::list_libraries(&self.pool, user_id).await.map_err(internal)
    }

    pub async fn list_deleted_libraries(&self, user_id: &str) -> Result<Vec<sync_common::LibraryId>, SyncError> {
        let ids = sqlx::query_scalar::<_, String>("SELECT id FROM libraries WHERE user_id=$1 AND deleted_at IS NOT NULL ORDER BY deleted_at, id").bind(user_id).fetch_all(&self.pool).await.map_err(internal)?;
        ids.into_iter().map(|id| sync_common::LibraryId::parse_str(&id).map_err(internal)).collect()
    }

    pub async fn create_library(&self, user_id: &str, request: &LibraryNameRequest) -> Result<Option<LibraryNameOutcome>, SyncError> {
        let name = request.library_name.trim();
        if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
            return Err(SyncError::InvalidLibraryName);
        }
        sync_store::create_library(&self.pool, user_id, &LibraryNameRequest { library_name: name.to_owned(), ..request.clone() }).await.map_err(internal)
    }

    pub async fn rename_library(&self, user_id: &str, request: &LibraryNameRequest) -> Result<Option<LibraryNameOutcome>, SyncError> {
        let name = request.library_name.trim();
        if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
            return Err(SyncError::InvalidLibraryName);
        }
        sync_store::rename_library(&self.pool, user_id, &LibraryNameRequest { library_name: name.to_owned(), ..request.clone() }).await.map_err(internal)
    }

    /// Deletes library state and requests cleanup of unused physical assets.
    pub async fn delete_library(&self, user_id: &str, library_id: &sync_common::LibraryId) -> Result<bool, SyncError> {
        let phase_started = Instant::now();
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        tracing::debug!(target: "sync_performance", phase = "pool_acquire_and_begin", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        let library_id = library_id.to_string();
        let owned = sqlx::query("SELECT 1 FROM libraries WHERE id=$1 AND user_id=$2 AND deleted_at IS NULL FOR UPDATE").bind(&library_id).bind(user_id).fetch_optional(&mut *transaction).await.map_err(internal)?.is_some();
        if !owned {
            transaction.rollback().await.map_err(internal)?;
            return Ok(false);
        }

        // Retain the UUID row as a tombstone. Concurrent and stale upserts will
        // conflict with it and are forbidden from clearing deleted_at.
        sqlx::query("UPDATE libraries SET deleted_at=now(), name='Deleted library' WHERE id=$1 AND user_id=$2 AND deleted_at IS NULL").bind(&library_id).bind(user_id).execute(&mut *transaction).await.map_err(internal)?;

        sqlx::query(include_str!("sql/cloud_storage/delete_claims.sql")).bind(&library_id).execute(&mut *transaction).await.map_err(internal)?;
        // Release unfinished upload quota before deleting its reservations.
        let reserved: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(size_bytes), 0)::BIGINT
             FROM book_upload_reservation WHERE library_id=$1 AND user_id=$2",
        )
        .bind(&library_id)
        .bind(user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(internal)?;
        if reserved > 0 {
            sqlx::query("UPDATE user_storage_account SET reserved_bytes=GREATEST(reserved_bytes-$2, 0) WHERE user_id=$1").bind(user_id).bind(reserved).execute(&mut *transaction).await.map_err(internal)?;
        }
        let lifecycle_hashes = sqlx::query_scalar::<_, Vec<u8>>("SELECT content_hash FROM sync_state WHERE library_id=$1 AND kind='book_lifecycle' AND version_rank<>2 AND content_hash IS NOT NULL")
            .bind(&library_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(internal)?;
        let cloud_enabled: bool = sqlx::query_scalar(include_str!("sql/cloud_storage/enabled.sql")).bind(&library_id).fetch_one(&mut *transaction).await.map_err(internal)?;
        assets::apply_lifecycle_reference_deltas(&mut transaction, user_id, lifecycle_hashes.into_iter().filter(|_| cloud_enabled).map(|hash| sync_common::BookId::try_from(hash.as_slice()).map(|hash| (hash, -1))).collect::<Result<Vec<_>, _>>().map_err(internal)?).await.map_err(internal)?;
        sqlx::query(include_str!("sql/cloud_storage/release_unreferenced.sql")).bind(user_id).execute(&mut *transaction).await.map_err(internal)?;
        sqlx::query("DELETE FROM book_upload_reservation WHERE library_id=$1").bind(&library_id).execute(&mut *transaction).await.map_err(internal)?;
        sqlx::query("DELETE FROM sync_state WHERE library_id=$1").bind(&library_id).execute(&mut *transaction).await.map_err(internal)?;
        sqlx::query("DELETE FROM sync_library_revision WHERE library_id=$1").bind(&library_id).execute(&mut *transaction).await.map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        self.cleanup.notify_one();
        Ok(true)
    }

    pub(crate) async fn load_changes(&self, user_id: &str, query: &PullStateQuery) -> Result<PullStateResponse, SyncError> {
        match sync_store::fetch_changes(&self.pool, user_id, &query.library_id, &query.replica_id, query.cursor).await.map_err(fetch_error)? {
            sync_store::FetchChangesResult::Page(page) => Ok(pull_response(page)),
            sync_store::FetchChangesResult::Unauthorized => Err(SyncError::LibraryOwnershipConflict),
        }
    }

    pub async fn exchange(&self, user_id: &str, request: &SyncExchangeRequest) -> Result<SyncExchangeResponse, SyncError> {
        if request.mutations.len() > sync_common::MAX_PUSH_MUTATIONS {
            return Err(SyncError::TooManyMutations);
        }
        let mut identities = HashSet::with_capacity(request.mutations.len());
        if !request.mutations.iter().all(|mutation| identities.insert(mutation.publisher_seq)) {
            return Err(SyncError::AmbiguousMutationIdentity);
        }
        if request.mutations.is_empty() {
            let query = PullStateQuery { library_id: request.library_id.clone(), replica_id: request.replica_id.clone(), cursor: request.cursor };
            return Ok(SyncExchangeResponse { push: PushMutationsResponse { rejected: Vec::new() }, pull: self.load_changes(user_id, &query).await? });
        }

        let phase_started = Instant::now();
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        tracing::debug!(target: "sync_performance", phase = "pool_acquire_and_begin", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        let phase_started = Instant::now();
        if !self.authorize_owned_library(&mut transaction, user_id, &request.library_id).await? {
            transaction.rollback().await.map_err(internal)?;
            return Err(SyncError::LibraryOwnershipConflict);
        }
        tracing::debug!(target: "sync_performance", phase = "library_lock_and_authorize", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        let phase_started = Instant::now();
        sync_store::validate_owned_cursor(&mut transaction, &request.library_id, request.cursor).await.map_err(fetch_error)?;
        tracing::debug!(target: "sync_performance", phase = "validate_cursor", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        let phase_started = Instant::now();
        let inserted = sync_store::insert_changes(&mut transaction, user_id, &request.library_id, &request.replica_id, &request.mutations).await.map_err(internal)?;
        tracing::debug!(target: "sync_performance", phase = "insert_changes", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        let phase_started = Instant::now();
        let page = sync_store::fetch_owned_changes(&mut transaction, &request.library_id, request.cursor).await.map_err(fetch_error)?;
        tracing::debug!(target: "sync_performance", phase = "fetch_page", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        let phase_started = Instant::now();
        transaction.commit().await.map_err(internal)?;
        tracing::debug!(target: "sync_performance", phase = "commit", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        if request.mutations.iter().any(|mutation| mutation.state.value.address.kind == "book_lifecycle" && mutation.state.version.conflict_rank == ConflictRank::Purge) {
            self.cleanup.notify_one();
        }
        return Ok(SyncExchangeResponse { push: PushMutationsResponse { rejected: inserted.rejected }, pull: pull_response(page) });
    }
}
