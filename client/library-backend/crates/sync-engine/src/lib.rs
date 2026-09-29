//! Storage-independent state synchronization orchestration.

use library_replica::{coalesce_mutations_for_push, prepare_push, StateMutation};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use sync_common::{LibraryId, MutationId, MutationIssue, PullStateResponse, PushMutationsResponse, ReplicaId, StateCell, SyncCursor};

pub type StoreError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(not(target_arch = "wasm32"))]
pub type StoreFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, StoreError>> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
pub type StoreFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, StoreError>> + 'a>>;

/// Persistence required by state synchronization. Implementations own their
/// transaction semantics; the engine owns protocol and recovery policy.
pub trait SyncStore {
    fn cursor_recovery_pending(&self) -> StoreFuture<'_, bool>;
    fn begin_cursor_recovery(&self) -> StoreFuture<'_, ()>;
    fn complete_cursor_recovery(&self) -> StoreFuture<'_, usize>;
    fn pull_cursor(&self) -> StoreFuture<'_, SyncCursor>;
    fn inventory_page(&self, after: Option<StateCell>) -> StoreFuture<'_, Vec<StateCell>>;
    fn inventory_checkpoint(&self) -> StoreFuture<'_, Option<StateCell>>;
    fn save_inventory_checkpoint(&self, after: Option<StateCell>) -> StoreFuture<'_, ()>;
    fn enqueue_missing_state_cells(&self, cells: &[StateCell]) -> StoreFuture<'_, usize>;
    fn pending_mutations(&self) -> StoreFuture<'_, Vec<StateMutation>>;
    fn acknowledge_mutations(&self, mutation_ids: &[MutationId]) -> StoreFuture<'_, ()>;
    /// Atomically applies the mutations and advances the cursor.
    fn apply_pull_page(&self, page: &PullStateResponse) -> StoreFuture<'_, ()>;
    /// Persists validated push acknowledgments and a pull page. Implementations
    /// may share a commit, but must retain acknowledgments if applying pull fails.
    fn apply_exchange_response(&self, ids: &[MutationId], page: &PullStateResponse) -> StoreFuture<'_, ()>;
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum SyncError {
    AuthenticationRequired,
    CursorRejected,
    Failed(String),
}

impl SyncError {
    fn failed(error: impl std::fmt::Display) -> Self {
        Self::Failed(error.to_string())
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthenticationRequired => formatter.write_str("synchronization authentication is required"),
            Self::CursorRejected => formatter.write_str("the synchronization cursor was rejected after recovery"),
            Self::Failed(message) => message.fmt(formatter),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<sync_transport::SyncRequestError> for SyncError {
    fn from(error: sync_transport::SyncRequestError) -> Self {
        match error {
            sync_transport::SyncRequestError::AuthenticationRequired => Self::AuthenticationRequired,
            sync_transport::SyncRequestError::CursorRejected => Self::CursorRejected,
            sync_transport::SyncRequestError::Other(error) => Self::failed(error),
        }
    }
}

#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub mutation_issues: Vec<MutationIssue>,
}

pub struct SyncEngine<'a, S: SyncStore> {
    store: S,
    http_client: &'a reqwest::Client,
    library_id: LibraryId,
    replica_id: ReplicaId,
    credentials: Option<sync_transport::SyncCredentials>,
    inventory_checked: AtomicBool,
}

pub struct SyncEngineConfig<'a, S: SyncStore> {
    pub store: S,
    pub http_client: &'a reqwest::Client,
    pub library_id: LibraryId,
    pub replica_id: ReplicaId,
    pub credentials: Option<sync_transport::SyncCredentials>,
    pub inventory_checked: &'a AtomicBool,
}

impl<'a, S: SyncStore> SyncEngine<'a, S> {
    pub fn new(config: SyncEngineConfig<'a, S>) -> Self {
        Self {
            store: config.store,
            http_client: config.http_client,
            library_id: config.library_id,
            replica_id: config.replica_id,
            credentials: config.credentials,
            inventory_checked: AtomicBool::new(config.inventory_checked.load(Ordering::Acquire)),
        }
    }

    pub fn inventory_checked(&self) -> bool {
        self.inventory_checked.load(Ordering::Acquire)
    }
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Publish only committed local mutations. Exchange replies may contain a
    /// pull page, but an inactive library must neither apply nor advance it.
    pub async fn drain_outbox(&self) -> Result<SyncOutcome, SyncError> {
        let changes = coalesce_mutations_for_push(self.store.pending_mutations().await.map_err(SyncError::failed)?);
        let mut remaining = prepare_push(changes).map_err(SyncError::failed)?;
        let mut result = SyncOutcome { mutation_issues: remaining.untransmittable().iter().copied().map(MutationIssue::Untransmittable).collect() };
        while !remaining.is_empty() {
            let requested = remaining.next_batch();
            let ids = requested.iter().map(|mutation| mutation.mutation_id).collect::<Vec<_>>();
            let credentials = self.current_credentials()?;
            let response = sync_transport::exchange_changes(self.http_client, &credentials, self.library_id, self.replica_id, requested, SyncCursor::default()).await?;
            result.mutation_issues.extend(self.apply_push_response_semantics(&ids, &response.push).await?);
        }
        Ok(result)
    }

    pub async fn synchronize(&self) -> Result<SyncOutcome, SyncError> {
        if self.store.cursor_recovery_pending().await.map_err(SyncError::failed)? {
            self.resume_cursor_recovery().await?;
        }
        for attempt in 0..2 {
            let cursor = self.store.pull_cursor().await.map_err(SyncError::failed)?;
            match self.exchange_events_with_server(cursor).await {
                Ok(result) => return Ok(result),
                Err(SyncError::CursorRejected) if attempt == 0 => {
                    self.begin_cursor_recovery().await?;
                    self.resume_cursor_recovery().await?;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("cursor recovery retry must return")
    }

    pub async fn begin_cursor_recovery(&self) -> Result<(), SyncError> {
        self.store.begin_cursor_recovery().await.map_err(SyncError::failed)?;
        self.inventory_checked.store(false, Ordering::Release);
        Ok(())
    }

    async fn resume_cursor_recovery(&self) -> Result<(), SyncError> {
        let mut result = SyncOutcome::default();
        let mut remaining = prepare_push(Vec::new()).map_err(SyncError::failed)?;
        let credentials = self.current_credentials()?;
        self.exchange_until_idle(&credentials, SyncCursor::default(), &mut remaining, &mut result).await?;
        let inserted = self.store.complete_cursor_recovery().await.map_err(SyncError::failed)?;
        self.inventory_checked.store(true, Ordering::Release);
        log::warn!("Recovered a rejected synchronization cursor and queued {inserted} canonical cells for republishing");
        Ok(())
    }

    /// Independent, resumable repair. Ordinary exchange never awaits this audit.
    pub async fn repair_inventory(&self) -> Result<(), SyncError> {
        if self.inventory_checked.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut after = self.store.inventory_checkpoint().await.map_err(SyncError::failed)?;
        loop {
            let mut trace = sync_transport::PerformanceTrace::new("inventory_page", "load_local");
            let cells = self.store.inventory_page(after.clone()).await.map_err(SyncError::failed)?;
            log::debug!(target: "sync_performance", "trace_id={} library_id={} cells={}", trace.id(), self.library_id, cells.len());
            trace.phase("compare_remote");
            if cells.is_empty() {
                self.store.save_inventory_checkpoint(None).await.map_err(SyncError::failed)?;
                self.inventory_checked.store(true, Ordering::Release);
                trace.finish(true);
                return Ok(());
            }
            let credentials = self.current_credentials()?;
            let Some(response) = sync_transport::compare_state_inventory(self.http_client, &credentials, self.library_id, cells.clone()).await? else {
                self.store.save_inventory_checkpoint(None).await.map_err(SyncError::failed)?;
                self.inventory_checked.store(true, Ordering::Release);
                trace.finish(true);
                return Ok(());
            };
            trace.phase("queue_missing_and_checkpoint");
            log::debug!(target: "sync_performance", "trace_id={} missing={}", trace.id(), response.missing.len());
            let requested = cells.iter().collect::<HashSet<_>>();
            if response.missing.len() > cells.len() || response.missing.iter().any(|cell| !requested.contains(cell)) {
                return Err(SyncError::failed("server returned an invalid state inventory response"));
            }
            if !response.missing.is_empty() {
                self.store.enqueue_missing_state_cells(&response.missing).await.map_err(SyncError::failed)?;
            }
            after = cells.last().cloned();
            // Checkpoint only after missing cells are durably queued. A crash
            // between these operations can repeat a page but cannot lose work.
            self.store.save_inventory_checkpoint(after.clone()).await.map_err(SyncError::failed)?;
            trace.finish(true);
        }
    }

    pub async fn exchange_events_with_server(&self, starting_cursor: SyncCursor) -> Result<SyncOutcome, SyncError> {
        let mut trace = sync_transport::PerformanceTrace::new("sync_exchange", "load_and_batch_outbox");
        log::debug!(target: "sync_performance", "trace_id={} library_id={} initial_cursor={:?} from_zero={}", trace.id(), self.library_id, starting_cursor, starting_cursor == SyncCursor::default());
        let changes = coalesce_mutations_for_push(self.store.pending_mutations().await.map_err(SyncError::failed)?);
        let mut remaining = prepare_push(changes).map_err(SyncError::failed)?;
        let untransmittable = remaining.untransmittable().to_vec();
        let mut result = SyncOutcome { mutation_issues: untransmittable.iter().copied().map(MutationIssue::Untransmittable).collect() };
        let credentials = self.current_credentials()?;
        trace.phase("pages");
        self.exchange_until_idle(&credentials, starting_cursor, &mut remaining, &mut result).await?;
        trace.finish(true);
        Ok(result)
    }

    pub async fn pull_events_from_server(&self, cursor: SyncCursor) -> Result<(), SyncError> {
        let mut result = SyncOutcome::default();
        let mut remaining = prepare_push(Vec::new()).map_err(SyncError::failed)?;
        let credentials = self.current_credentials()?;
        self.exchange_until_idle(&credentials, cursor, &mut remaining, &mut result).await
    }

    pub async fn apply_push_response(&self, requested_ids: &[MutationId], response: &PushMutationsResponse) -> Result<Vec<sync_common::MutationRejection>, SyncError> {
        let issues = self.apply_push_response_semantics(requested_ids, response).await?;
        if let Some(MutationIssue::Rejected(rejection)) = issues.iter().find(|issue| matches!(issue, MutationIssue::Rejected(_))) {
            return Err(SyncError::failed(sync_common::PushResponseError::PermanentRejection { mutation_id: rejection.mutation_id, reason: rejection.reason }));
        }
        Ok(issues
            .into_iter()
            .filter_map(|issue| match issue {
                MutationIssue::Retryable(rejection) => Some(rejection),
                _ => None,
            })
            .collect())
    }

    async fn apply_push_response_semantics(&self, requested_ids: &[MutationId], response: &PushMutationsResponse) -> Result<Vec<MutationIssue>, SyncError> {
        let validated = sync_transport::validate_push_response(requested_ids, response).map_err(SyncError::failed)?;
        let completed = validated.completed_ids();
        if !completed.is_empty() {
            self.store.acknowledge_mutations(&completed).await.map_err(SyncError::failed)?;
        }
        Ok(validated.issues)
    }

    async fn exchange_until_idle(&self, credentials: &sync_transport::SyncCredentials, mut cursor: SyncCursor, remaining: &mut sync_common::PushBatcher, result: &mut SyncOutcome) -> Result<(), SyncError> {
        let mut page_number = 0_u64;
        loop {
            page_number += 1;
            let mut trace = sync_transport::PerformanceTrace::new("sync_page", "prepare_request");
            let requested = remaining.next_batch();
            let requested_ids = requested.iter().map(|mutation| mutation.mutation_id).collect::<Vec<_>>();
            log::debug!(target: "sync_performance", "trace_id={} library_id={} page={page_number} push_count={} cursor={cursor:?}", trace.id(), self.library_id, requested.len());
            trace.phase("http_roundtrip");
            let response = sync_transport::exchange_changes(self.http_client, credentials, self.library_id, self.replica_id, requested, cursor).await?;
            trace.phase("validate_response");
            log::debug!(target: "sync_performance", "trace_id={} accepted={} rejected={} pull_count={} has_more={}", trace.id(), response.push.accepted.len(), response.push.rejected.len(), response.pull.mutations.len(), response.pull.has_more);
            let validated = sync_transport::validate_push_response(&requested_ids, &response.push).map_err(SyncError::failed)?;
            let completed = validated.completed_ids();
            result.mutation_issues.extend(validated.issues);
            let has_more = response.pull.has_more;
            if let Err(error) = sync_transport::validate_pull_batch(&response.pull, cursor) {
                // Preserve the old failure policy: a malformed pull response
                // must not discard independently valid push acknowledgments.
                if !completed.is_empty() {
                    self.store.acknowledge_mutations(&completed).await.map_err(SyncError::failed)?;
                }
                return Err(SyncError::failed(error));
            }
            cursor = response.pull.next_cursor;
            trace.phase("apply_local_transaction");
            self.store.apply_exchange_response(&completed, &response.pull).await.map_err(SyncError::failed)?;
            trace.finish(true);
            if remaining.is_empty() && !has_more {
                return Ok(());
            }
        }
    }

    fn current_credentials(&self) -> Result<sync_transport::SyncCredentials, SyncError> {
        self.credentials.clone().ok_or(SyncError::AuthenticationRequired)
    }
}

#[cfg(test)]
mod wire_tests {

    #[test]
    fn shared_sync_errors_preserve_the_worker_wire_format() {
        // This is the worker's previous wire schema. Keep the fixture independent
        // of the shared error type so accidental protocol changes are detected.
        #[derive(serde::Serialize)]
        enum PreviousSyncFailure {
            AuthenticationRequired,
            CursorRejected,
            Failed(String),
        }
        for (previous, shared) in [
            (PreviousSyncFailure::AuthenticationRequired, super::SyncError::AuthenticationRequired),
            (PreviousSyncFailure::CursorRejected, super::SyncError::CursorRejected),
            (PreviousSyncFailure::Failed("transport unavailable".into()), super::SyncError::Failed("transport unavailable".into())),
        ] {
            let previous_bytes = rmp_serde::to_vec_named(&previous).unwrap();
            assert_eq!(rmp_serde::to_vec_named(&shared).unwrap(), previous_bytes);
            let decoded: super::SyncError = rmp_serde::from_slice(&previous_bytes).unwrap();
            assert_eq!(decoded.to_string(), shared.to_string());
        }
    }
}
