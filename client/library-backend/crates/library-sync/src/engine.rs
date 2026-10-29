//! Storage-independent state synchronization orchestration.

use library_replica::StatePublication;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use sync_common::{BookId, LibraryId, MutationIssue, OpaquePublication, PullStateResponse, PushBatcher, PushMutationsResponse, ReplicaId, ReplicaSeq, SyncCursor};

pub type StoreError = Box<dyn Error + Send + Sync>;

/// Local: stores borrow the library's single connection on its own thread.
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + 'a>>;

/// Everything needed before the first exchange, prepared by the storage owner.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum PreparedSync<T> {
    RecoveryPending,
    Ready { cursor: SyncCursor, outbox: T, mutations: Vec<StatePublication> },
}

/// Persistence required by state synchronization. Implementations own their
/// transaction semantics; the engine owns protocol and recovery policy.
pub trait SyncStore {
    type Outbox;
    /// Check recovery before freezing the outbox; include its first bounded page.
    fn prepare_sync(&self) -> StoreFuture<'_, PreparedSync<Self::Outbox>>;
    /// Prepare once. Pages are unique by state identity and globally ordered
    /// with book lifecycles first; newer edits stay outside this scan.
    fn prepare_outbox(&self) -> StoreFuture<'_, Self::Outbox>;
    fn begin_cursor_recovery(&self) -> StoreFuture<'_, ()>;
    fn complete_cursor_recovery(&self) -> StoreFuture<'_, usize>;
    fn pending_mutations<'a>(&'a self, scan: &'a mut Self::Outbox) -> StoreFuture<'a, Vec<StatePublication>>;
    fn record_mutation_issues(&self, issues: &[MutationIssue]) -> StoreFuture<'_, ()>;
    fn acknowledge_mutations(&self, mutation_ids: &[ReplicaSeq]) -> StoreFuture<'_, ()>;
    /// Persists validated push acknowledgments and a pull page. Implementations
    /// may share a commit, but must retain acknowledgments if applying pull fails.
    fn apply_exchange_response(&self, ids: &[ReplicaSeq], page: &PullStateResponse) -> StoreFuture<'_, ()>;
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum SyncError {
    AuthenticationRequired,
    CursorRejected,
    Failed(String),
}

impl SyncError {
    fn failed(error: impl Display) -> Self {
        Self::Failed(error.to_string())
    }
}

impl Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthenticationRequired => formatter.write_str("synchronization authentication is required"),
            Self::CursorRejected => formatter.write_str("the synchronization cursor was rejected after recovery"),
            Self::Failed(message) => message.fmt(formatter),
        }
    }
}

impl Error for SyncError {}

impl From<sync_transport::SyncRequestError> for SyncError {
    fn from(error: sync_transport::SyncRequestError) -> Self {
        match error {
            sync_transport::SyncRequestError::AuthenticationRequired => Self::AuthenticationRequired,
            sync_transport::SyncRequestError::CursorRejected => Self::CursorRejected,
            error => Self::failed(error),
        }
    }
}

#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub mutation_issues: Vec<MutationIssue>,
}

struct Acknowledgements {
    completed: Vec<ReplicaSeq>,
    issues: Vec<MutationIssue>,
}

struct SubmittedBatch {
    ids: Vec<ReplicaSeq>,
    creations: Vec<(ReplicaSeq, BookId)>,
}

impl SubmittedBatch {
    fn new(mutations: &[OpaquePublication]) -> Self {
        Self {
            ids: mutations.iter().map(|mutation| mutation.publisher_seq).collect(),
            creations: mutations.iter().filter(|mutation| mutation.state.value.address.kind == "book_lifecycle").filter_map(|mutation| mutation.state.value.address.entity_key.book().map(|book| (mutation.publisher_seq, book))).collect(),
        }
    }

    fn defer_rejected_fields<T>(&self, remaining: &mut Outgoing<T>, response: &PushMutationsResponse) {
        if self.creations.is_empty() || response.rejected.is_empty() {
            return;
        }
        let rejected: HashSet<_> = response.rejected.iter().map(|rejection| rejection.publisher_seq).collect();
        let books = self.creations.iter().filter(|(id, _)| rejected.contains(id)).map(|(_, book)| *book).collect::<Vec<_>>();
        remaining.deferred.extend(books.iter().copied());
        remaining.batches.defer_book_fields(&books);
    }
}

struct Outgoing<T> {
    scan: Option<T>,
    page: std::vec::IntoIter<StatePublication>,
    batches: PushBatcher,
    deferred: HashSet<BookId>,
}

impl<T> Outgoing<T> {
    fn new(scan: Option<T>) -> Self {
        Self { scan, page: Vec::new().into_iter(), batches: PushBatcher::default(), deferred: HashSet::new() }
    }
}

pub struct SyncEngine<'a, S: SyncStore> {
    store: S,
    http_client: &'a reqwest::Client,
    library_id: LibraryId,
    replica_id: ReplicaId,
    credentials: Option<sync_transport::SyncCredentials>,
}

pub struct SyncEngineConfig<'a, S: SyncStore> {
    pub store: S,
    pub http_client: &'a reqwest::Client,
    pub library_id: LibraryId,
    pub replica_id: ReplicaId,
    pub credentials: Option<sync_transport::SyncCredentials>,
}

impl<'a, S: SyncStore> SyncEngine<'a, S> {
    pub fn new(config: SyncEngineConfig<'a, S>) -> Self {
        Self { store: config.store, http_client: config.http_client, library_id: config.library_id, replica_id: config.replica_id, credentials: config.credentials }
    }

    /// Publish only committed local mutations. Exchange replies may contain a
    /// pull page, but an inactive library must neither apply nor advance it.
    pub async fn drain_outbox(&self) -> Result<SyncOutcome, SyncError> {
        let mut remaining = self.prepare_outbox().await?;
        let mut result = SyncOutcome::default();
        loop {
            let requested = self.next_push_batch(&mut remaining, &mut result).await?;
            if requested.is_empty() { break; }
            let submitted = SubmittedBatch::new(&requested);
            let credentials = self.current_credentials()?;
            let response = sync_transport::exchange_changes(self.http_client, &credentials, self.library_id, self.replica_id, requested, SyncCursor::default()).await?;
            let acknowledgements = self.prepare_acknowledgements(&submitted, &response.push).await?;
            self.acknowledge_completed(&acknowledgements.completed).await?;
            result.mutation_issues.extend(acknowledgements.issues);
            submitted.defer_rejected_fields(&mut remaining, &response.push);
        }
        Ok(result)
    }

    pub async fn synchronize(&self) -> Result<SyncOutcome, SyncError> {
        for attempt in 0..2 {
            let mut prepared = self.store.prepare_sync().await.map_err(SyncError::failed)?;
            if matches!(prepared, PreparedSync::RecoveryPending) {
                self.resume_cursor_recovery().await?;
                prepared = self.store.prepare_sync().await.map_err(SyncError::failed)?;
            }
            let PreparedSync::Ready { cursor, outbox, mutations } = prepared else {
                return Err(SyncError::failed("cursor recovery did not clear its pending marker"));
            };
            let mut remaining = Outgoing::new(if mutations.is_empty() { None } else { Some(outbox) });
            remaining.page = mutations.into_iter();
            match self.exchange_prepared(cursor, remaining).await {
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
        Ok(())
    }

    async fn resume_cursor_recovery(&self) -> Result<(), SyncError> {
        let mut result = SyncOutcome::default();
        let mut remaining = Outgoing::new(None);
        let credentials = self.current_credentials()?;
        self.exchange_until_idle(&credentials, SyncCursor::default(), &mut remaining, &mut result).await?;
        let inserted = self.store.complete_cursor_recovery().await.map_err(SyncError::failed)?;
        log::warn!("Recovered a rejected synchronization cursor and queued {inserted} canonical cells for republishing");
        Ok(())
    }

    async fn exchange_prepared(&self, starting_cursor: SyncCursor, mut remaining: Outgoing<S::Outbox>) -> Result<SyncOutcome, SyncError> {
        let mut trace = sync_transport::PerformanceTrace::new("sync_exchange", "prepare_request");
        log::debug!(target: "sync_performance", "trace_id={} library_id={} initial_cursor={:?} from_zero={}", trace.id(), self.library_id, starting_cursor, starting_cursor == SyncCursor::default());
        let mut result = SyncOutcome::default();
        let credentials = self.current_credentials()?;
        trace.phase("pages");
        self.exchange_until_idle(&credentials, starting_cursor, &mut remaining, &mut result).await?;
        trace.finish(true);
        Ok(result)
    }

    async fn prepare_outbox(&self) -> Result<Outgoing<S::Outbox>, SyncError> {
        Ok(Outgoing::new(Some(self.store.prepare_outbox().await.map_err(SyncError::failed)?)))
    }

    async fn next_push_batch(&self, remaining: &mut Outgoing<S::Outbox>, result: &mut SyncOutcome) -> Result<Vec<OpaquePublication>, SyncError> {
        while !remaining.batches.has_batch() {
            if let Some(publication) = remaining.page.next() {
                if publication.state.value.book().is_some_and(|book| remaining.deferred.contains(&book)) {
                    continue;
                }
                let opaque = sync_common::transport::representation(&publication).map_err(SyncError::failed)?;
                remaining.batches.push(opaque).map_err(SyncError::failed)?;
            } else {
                let Some(scan) = remaining.scan.as_mut() else { break };
                remaining.page = self.store.pending_mutations(scan).await.map_err(SyncError::failed)?.into_iter();
                if remaining.page.len() == 0 {
                    remaining.scan = None;
                    break;
                }
            }
        }
        let issues: Vec<_> = remaining.batches.take_untransmittable().into_iter().map(MutationIssue::Untransmittable).collect();
        self.record_issues(&issues).await?;
        result.mutation_issues.extend(issues);
        Ok(remaining.batches.next_batch())
    }

    async fn prepare_acknowledgements(&self, submitted: &SubmittedBatch, response: &PushMutationsResponse) -> Result<Acknowledgements, SyncError> {
        let validated = sync_transport::validate_push_response(&submitted.ids, response).map_err(SyncError::failed)?;
        self.record_issues(&validated.issues).await?;
        Ok(Acknowledgements { completed: validated.completed_sequences(), issues: validated.issues })
    }

    async fn acknowledge_completed(&self, completed: &[ReplicaSeq]) -> Result<(), SyncError> {
        if !completed.is_empty() {
            self.store.acknowledge_mutations(completed).await.map_err(SyncError::failed)?;
        }
        Ok(())
    }

    async fn exchange_until_idle(&self, credentials: &sync_transport::SyncCredentials, mut cursor: SyncCursor, remaining: &mut Outgoing<S::Outbox>, result: &mut SyncOutcome) -> Result<(), SyncError> {
        let mut page_number = 0_u64;
        let mut requested = self.next_push_batch(remaining, result).await?;
        loop {
            page_number += 1;
            let mut trace = sync_transport::PerformanceTrace::new("sync_page", "prepare_request");
            let submitted = SubmittedBatch::new(&requested);
            log::debug!(target: "sync_performance", "trace_id={} library_id={} page={page_number} push_count={} cursor={cursor:?}", trace.id(), self.library_id, requested.len());
            trace.phase("http_roundtrip");
            let response = sync_transport::exchange_changes(self.http_client, credentials, self.library_id, self.replica_id, requested, cursor).await?;
            trace.phase("validate_response");
            log::debug!(target: "sync_performance", "trace_id={} accepted={} rejected={} pull_count={} has_more={}", trace.id(), submitted.ids.len().saturating_sub(response.push.rejected.len()), response.push.rejected.len(), response.pull.mutations.len(), response.pull.has_more);
            let acknowledgements = self.prepare_acknowledgements(&submitted, &response.push).await?;
            result.mutation_issues.extend(acknowledgements.issues);
            let has_more = response.pull.has_more;
            if let Err(error) = sync_transport::validate_pull_batch(&response.pull, cursor) {
                // Preserve the old failure policy: a malformed pull response
                // must not discard independently valid push acknowledgments.
                self.acknowledge_completed(&acknowledgements.completed).await?;
                return Err(SyncError::failed(error));
            }
            cursor = response.pull.next_cursor;
            trace.phase("apply_local_transaction");
            self.store.apply_exchange_response(&acknowledgements.completed, &response.pull).await.map_err(SyncError::failed)?;
            submitted.defer_rejected_fields(remaining, &response.push);
            trace.finish(true);
            requested = self.next_push_batch(remaining, result).await?;
            if requested.is_empty() && !has_more {
                return Ok(());
            }
        }
    }

    fn current_credentials(&self) -> Result<sync_transport::SyncCredentials, SyncError> {
        self.credentials.clone().ok_or(SyncError::AuthenticationRequired)
    }

    async fn record_issues(&self, issues: &[MutationIssue]) -> Result<(), SyncError> {
        if !issues.is_empty() {
            self.store.record_mutation_issues(issues).await.map_err(SyncError::failed)?;
        }
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "engine_tests.rs"]
mod tests;
