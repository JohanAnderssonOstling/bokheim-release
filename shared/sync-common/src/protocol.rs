//! Transport-independent validation for synchronization pages.

use crate::transport::encoded_len;
use crate::BookId;
use crate::{LibraryRevision, MutationRejection, MutationRejectionReason, OpaquePublication, PullStateResponse, PushMutationsResponse, SyncCursor};
use crate::{ReplicaSeq, MAX_PUSH_BATCH_BYTES, MAX_PUSH_MUTATIONS};
use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
struct EncodedMutation {
    publication: OpaquePublication,
    bytes: usize,
}

/// Wire mutations prepared for bounded, order-preserving push requests.
///
/// A mutation larger than a complete batch is retained separately so it does
/// not block later mutations. The caller remains responsible for deciding how
/// to surface or quarantine those untransmittable mutations.
#[derive(Debug, Default)]
pub struct PushBatcher {
    ready: VecDeque<EncodedMutation>,
    untransmittable: Vec<ReplicaSeq>,
    ready_bytes: usize,
}

impl PushBatcher {
    pub fn new(mutations: Vec<OpaquePublication>) -> Result<Self, crate::transport::WireError> {
        Self::try_from_publications(mutations.into_iter().map(Ok))
    }

    /// Convert and size publications as they arrive, without an intermediate
    /// vector of opaque payloads alongside the replica's typed snapshot.
    pub fn try_from_publications(mutations: impl IntoIterator<Item = Result<OpaquePublication, crate::transport::WireError>>) -> Result<Self, crate::transport::WireError> {
        let mutations = mutations.into_iter();
        let mut batcher = Self { ready: VecDeque::with_capacity(mutations.size_hint().0), untransmittable: Vec::new(), ready_bytes: 0 };
        for mutation in mutations {
            batcher.push(mutation?)?;
        }
        Ok(batcher)
    }

    /// Append one publication; streaming callers stop once a batch is ready.
    pub fn push(&mut self, publication: OpaquePublication) -> Result<(), crate::transport::WireError> {
        let bytes = encoded_len(&publication)?;
        if bytes > MAX_PUSH_BATCH_BYTES {
            self.untransmittable.push(publication.publisher_seq);
        } else {
            self.ready_bytes += bytes;
            self.ready.push_back(EncodedMutation { publication, bytes });
        }
        Ok(())
    }

    pub fn has_batch(&self) -> bool {
        self.ready.len() >= MAX_PUSH_MUTATIONS || self.ready_bytes >= MAX_PUSH_BATCH_BYTES
    }

    pub fn take_untransmittable(&mut self) -> Vec<ReplicaSeq> {
        std::mem::take(&mut self.untransmittable)
    }

    pub fn untransmittable(&self) -> &[ReplicaSeq] {
        &self.untransmittable
    }

    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    /// Leave fields of failed creations in the durable outbox for a later
    /// exchange. Sending them in a later page would look like standalone
    /// absent-book edits, which the server correctly acknowledges as no-ops.
    pub fn defer_book_fields(&mut self, books: &[BookId]) {
        if books.is_empty() {
            return;
        }
        if books.len() < 16 {
            self.ready.retain(|row| !row.publication.state.value.book.is_some_and(|book| books.contains(&book)));
        } else {
            let books: HashSet<_> = books.iter().copied().collect();
            self.ready.retain(|row| !row.publication.state.value.book.is_some_and(|book| books.contains(&book)));
        }
        self.ready_bytes = self.ready.iter().map(|row| row.bytes).sum();
    }

    pub fn next_batch(&mut self) -> Vec<OpaquePublication> {
        let mut batch = Vec::with_capacity(MAX_PUSH_MUTATIONS.min(self.ready.len()));
        let mut bytes = 0_usize;
        while batch.len() < MAX_PUSH_MUTATIONS {
            let Some(next) = self.ready.front() else { break };
            if !batch.is_empty() && bytes.saturating_add(next.bytes) > MAX_PUSH_BATCH_BYTES {
                break;
            }
            let Some(mutation) = self.ready.pop_front() else { break };
            bytes = bytes.saturating_add(mutation.bytes);
            batch.push(mutation.publication);
        }
        self.ready_bytes -= bytes;
        batch
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PushResponseError {
    InvalidAcknowledgments(String),
    PermanentRejection { publisher_seq: ReplicaSeq, reason: MutationRejectionReason },
}

impl Display for PushResponseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAcknowledgments(reason) => write!(formatter, "invalid mutation acknowledgement response: {reason}"),
            Self::PermanentRejection { publisher_seq, reason } => write!(formatter, "server permanently rejected mutation {publisher_seq}: {reason:?}"),
        }
    }
}

impl Error for PushResponseError {}

#[derive(Debug, PartialEq, Eq)]
pub struct PullBatchError(String);

impl Display for PullBatchError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid pull batch: {}", self.0)
    }
}

impl Error for PullBatchError {}

fn revision(value: Option<LibraryRevision>) -> u64 {
    value.map(LibraryRevision::get).unwrap_or(0)
}

pub fn validate_pull_batch(response: &PullStateResponse, cursor: SyncCursor) -> Result<(), PullBatchError> {
    let bytes = response.mutations.iter().fold(0usize, |sum, row| sum.saturating_add(row.state.value.delivery_byte_budget()));
    if bytes > crate::MAX_PULL_BATCH_BYTES {
        return Err(PullBatchError("a pull page exceeds its byte budget".into()));
    }
    let generation = response.next_cursor.generation.ok_or_else(|| PullBatchError("a pull page must name its server history".into()))?;
    if !cursor.belongs_to(generation) {
        return Err(PullBatchError("a pull page belongs to a different server history".into()));
    }
    let next_lifecycle = revision(response.next_cursor.lifecycle_revision);
    let next_field = revision(response.next_cursor.field_revision);
    let mut observed_lifecycle = revision(cursor.lifecycle_revision);
    let mut observed_field = revision(cursor.field_revision);
    let mut fields_started = false;
    for mutation in &response.mutations {
        let revision = mutation.revision.get();
        let lifecycle = mutation.state.value.address.kind == "book_lifecycle";
        if lifecycle && fields_started {
            return Err(PullBatchError("a book lifecycle follows a field in the page".into()));
        }
        fields_started |= !lifecycle;
        let channel = if lifecycle { &mut observed_lifecycle } else { &mut observed_field };
        if revision <= *channel {
            return Err(PullBatchError(format!("library revision {revision} does not advance its channel")));
        }
        if !lifecycle && revision > next_lifecycle {
            return Err(PullBatchError(format!("field revision {revision} precedes the lifecycles it may depend on")));
        }
        *channel = revision;
    }
    if next_lifecycle < observed_lifecycle || next_field < observed_field || next_field > next_lifecycle {
        return Err(PullBatchError("next cursor precedes an observed revision or passes the lifecycle cursor".into()));
    }
    if response.has_more && response.mutations.is_empty() {
        return Err(PullBatchError("a non-final page cannot be empty".into()));
    }
    let ends_in_lifecycles = !fields_started;
    if response.has_more && (next_field != observed_field || (ends_in_lifecycles && next_lifecycle != observed_lifecycle)) {
        return Err(PullBatchError("a non-final page cursor must end at its last delivered revision".into()));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedPushResponse {
    pub accepted: Vec<ReplicaSeq>,
    pub issues: Vec<MutationIssue>,
}

/// A local mutation that still needs attention after an otherwise successful
/// synchronization exchange. Keeping these cases in one collection prevents
/// callers from having to keep parallel vectors aligned.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MutationIssue {
    /// The server explicitly asked the client to submit this mutation later.
    Retryable(MutationRejection),
    /// The server conclusively rejected this mutation; it has left the outbox.
    Rejected(MutationRejection),
    /// The mutation cannot fit in a legal request and remains in the outbox.
    Untransmittable(ReplicaSeq),
}

impl ValidatedPushResponse {
    /// Mutations that must leave the durable outbox, whether accepted or
    /// permanently rejected. Retrying either class cannot change the result.
    pub fn completed_sequences(&self) -> Vec<ReplicaSeq> {
        self.accepted
            .iter()
            .copied()
            .chain(self.issues.iter().filter_map(|issue| match issue {
                MutationIssue::Rejected(rejection) => Some(rejection.publisher_seq),
                MutationIssue::Retryable(_) | MutationIssue::Untransmittable(_) => None,
            }))
            .collect()
    }
}

pub fn validate_push_response(requested: &[ReplicaSeq], response: &PushMutationsResponse) -> Result<ValidatedPushResponse, PushResponseError> {
    let requested_ids: HashSet<_> = requested.iter().copied().collect();
    if requested_ids.len() != requested.len() {
        return Err(PushResponseError::InvalidAcknowledgments("submitted mutation identities are not unique".into()));
    }
    let mut accounted = HashSet::new();
    let mut issues = Vec::new();
    for rejection in &response.rejected {
        if !requested_ids.contains(&rejection.publisher_seq) {
            return Err(PushResponseError::InvalidAcknowledgments(format!("server rejected unsubmitted mutation {}", rejection.publisher_seq)));
        }
        if !accounted.insert(rejection.publisher_seq) {
            return Err(PushResponseError::InvalidAcknowledgments(format!("server accounted for mutation {} more than once", rejection.publisher_seq)));
        }
        if rejection.reason.is_transient() {
            issues.push(MutationIssue::Retryable(rejection.clone()));
        } else {
            issues.push(MutationIssue::Rejected(rejection.clone()));
        }
    }
    let accepted = requested.iter().copied().filter(|seq| !accounted.contains(seq)).collect();
    Ok(ValidatedPushResponse { accepted, issues })
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::SyncGeneration;

    fn cursor(generation: Option<SyncGeneration>, revision: u64) -> SyncCursor {
        let revision = LibraryRevision::new(revision).unwrap();
        SyncCursor { generation, lifecycle_revision: Some(revision), field_revision: Some(revision) }
    }

    #[test]
    fn history_is_required_for_nonzero_checkpoints_and_stable_between_pages() {
        let history = SyncGeneration::from_u128(1);
        let old = cursor(Some(history), 1_000);
        let next = cursor(Some(history), 1_100);
        let mut page = PullStateResponse { mutations: Vec::new(), next_cursor: next, has_more: false };
        assert!(validate_pull_batch(&page, old).is_ok());
        assert!(validate_pull_batch(&page, SyncCursor::default()).is_ok());
        assert!(validate_pull_batch(&page, cursor(None, 1_000)).is_err());
        page.next_cursor.generation = Some(SyncGeneration::from_u128(2));
        assert!(validate_pull_batch(&page, old).is_err(), "a higher head cannot make another history valid");
        page.next_cursor.generation = None;
        assert!(validate_pull_batch(&page, SyncCursor::default()).is_err());
    }

    #[test]
    fn persisted_legacy_checkpoints_require_recovery() {
        let old: SyncCursor = serde_json::from_str(r#"{"lifecycle_revision":1000,"field_revision":1000}"#).unwrap();
        let history = SyncGeneration::from_u128(1);
        assert!(!old.belongs_to(history));
        assert!(SyncCursor::default().belongs_to(history));
        let bound = cursor(Some(history), 1_000);
        let decoded: SyncCursor = crate::transport::decode(&crate::transport::encode(&bound).unwrap(), 1024).unwrap();
        assert_eq!(decoded, bound);
    }

    fn page(count: usize) -> PullStateResponse {
        use crate::{ConflictRank, OpaqueStateEntry, ReplicaId, StateAddress, VersionKey, Versioned};
        PullStateResponse {
            mutations: (1..=count as u64)
                .map(|revision| crate::Delivery {
                    state: Versioned {
                        value: OpaqueStateEntry { address: StateAddress { kind: "directory_name".into(), entity_key: "a".into(), entity_subkey: String::new() }, book: None, bytes: vec![] },
                        version: VersionKey::new(1, ConflictRank::Update, ReplicaId::from_u128(1), ReplicaSeq::new(revision).unwrap()),
                    },
                    revision: LibraryRevision::new(revision).unwrap(),
                })
                .collect(),
            next_cursor: cursor(Some(SyncGeneration::from_u128(1)), count as u64),
            has_more: false,
        }
    }

    #[test]
    fn pull_pages_use_bytes_without_a_thousand_change_limit() {
        assert!(validate_pull_batch(&page(2_000), SyncCursor::default()).is_ok());
        let mut page = page(2);
        let overhead: usize = page.mutations.iter().map(|row| row.state.value.delivery_byte_budget()).sum();
        page.mutations[0].state.value.bytes.resize(crate::MAX_PULL_BATCH_BYTES - overhead, 0);
        assert!(validate_pull_batch(&page, SyncCursor::default()).is_ok());
        page.mutations[0].state.value.bytes.push(0);
        assert!(validate_pull_batch(&page, SyncCursor::default()).is_err());
        page.mutations.remove(1);
        page.mutations[0].state.value.bytes.resize(crate::MAX_PULL_BATCH_BYTES, 0);
        assert!(validate_pull_batch(&page, SyncCursor::default()).is_err(), "a single oversized state must retain the byte bound");
    }
}
