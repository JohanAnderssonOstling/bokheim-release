//! Transport-independent validation for synchronization pages.

use crate::{LibraryRevision, MutationId, MutationRejection, MutationRejectionReason, PullStateResponse, PushMutationsResponse, SyncCursor, WireMutation, mutation_kind};
use std::collections::{HashSet, VecDeque};

#[derive(Debug)]
struct EncodedMutation {
    mutation: WireMutation,
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
    untransmittable: Vec<MutationId>,
}

impl PushBatcher {
    pub fn new(mutations: Vec<WireMutation>) -> Result<Self, crate::transport::WireError> {
        let mut batcher = Self { ready: VecDeque::with_capacity(mutations.len()), untransmittable: Vec::new() };
        for mutation in mutations {
            let bytes = crate::transport::encode(&mutation)?.len();
            if bytes > crate::MAX_PUSH_BATCH_BYTES {
                batcher.untransmittable.push(mutation.mutation_id);
            } else {
                batcher.ready.push_back(EncodedMutation { mutation, bytes });
            }
        }
        Ok(batcher)
    }

    pub fn untransmittable(&self) -> &[MutationId] {
        &self.untransmittable
    }

    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    pub fn next_batch(&mut self) -> Vec<WireMutation> {
        let mut batch = Vec::with_capacity(crate::MAX_PUSH_MUTATIONS.min(self.ready.len()));
        let mut bytes = 0_usize;
        while batch.len() < crate::MAX_PUSH_MUTATIONS {
            let Some(next) = self.ready.front() else { break };
            if !batch.is_empty() && bytes.saturating_add(next.bytes) > crate::MAX_PUSH_BATCH_BYTES {
                break;
            }
            bytes = bytes.saturating_add(next.bytes);
            batch.push(self.ready.pop_front().expect("the inspected mutation is still queued").mutation);
        }
        batch
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PushResponseError {
    InvalidAcknowledgments(String),
    PermanentRejection { mutation_id: MutationId, reason: MutationRejectionReason },
}

impl std::fmt::Display for PushResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAcknowledgments(reason) => write!(formatter, "invalid mutation acknowledgement response: {reason}"),
            Self::PermanentRejection { mutation_id, reason } => write!(formatter, "server permanently rejected mutation {mutation_id}: {reason:?}"),
        }
    }
}

impl std::error::Error for PushResponseError {}

#[derive(Debug, PartialEq, Eq)]
pub struct PullBatchError(String);

impl std::fmt::Display for PullBatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid pull batch: {}", self.0)
    }
}

impl std::error::Error for PullBatchError {}

fn revision(value: Option<LibraryRevision>) -> u64 {
    value.map(LibraryRevision::get).unwrap_or(0)
}

pub fn validate_pull_batch(response: &PullStateResponse, cursor: SyncCursor) -> Result<(), PullBatchError> {
    let mut creations = std::collections::HashSet::new();
    for creation in &response.book_creations {
        let mutation = &creation.mutation;
        if mutation.kind != mutation_kind::BOOK_LIFECYCLE
            || !mutation.entity_subkey.is_empty()
            || !mutation.blob_reference.as_ref().is_some_and(|reference| reference.present && reference.content_hash.as_ref().is_some_and(|hash| hash.as_str() == mutation.entity_key))
            || !creations.insert(&mutation.entity_key)
        {
            return Err(PullBatchError("invalid or duplicate book creation prerequisite".into()));
        }
    }
    let starting_state = revision(cursor.state_revision);
    let starting_reading = revision(cursor.reading_revision);
    let mut observed_state = starting_state;
    let mut observed_reading = starting_reading;
    let mut previous_revision = 0;
    for mutation in &response.mutations {
        let revision = mutation.revision.get();
        let channel_cursor = if mutation.mutation.kind == mutation_kind::READING_POSITION { &mut observed_reading } else { &mut observed_state };
        if revision <= *channel_cursor || revision <= previous_revision {
            return Err(PullBatchError(format!("library revision {revision} does not advance its channel or response order")));
        }
        *channel_cursor = revision;
        previous_revision = revision;
    }
    let next_state = revision(response.next_cursor.state_revision);
    let next_reading = revision(response.next_cursor.reading_revision);
    if next_state < observed_state || next_reading < observed_reading {
        return Err(PullBatchError("next cursor precedes an observed channel revision".into()));
    }
    if response.has_more && response.mutations.is_empty() {
        return Err(PullBatchError("a non-final page cannot be empty".into()));
    }
    if response.has_more && (next_state != observed_state || next_reading != observed_reading) {
        return Err(PullBatchError("a non-final page cursor must equal the observed per-channel revisions".into()));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct ValidatedPushResponse {
    pub accepted: Vec<MutationId>,
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
    Untransmittable(MutationId),
}

impl ValidatedPushResponse {
    /// Mutations that must leave the durable outbox, whether accepted or
    /// permanently rejected. Retrying either class cannot change the result.
    pub fn completed_ids(&self) -> Vec<MutationId> {
        self.accepted
            .iter()
            .copied()
            .chain(self.issues.iter().filter_map(|issue| match issue {
                MutationIssue::Rejected(rejection) => Some(rejection.mutation_id),
                MutationIssue::Retryable(_) | MutationIssue::Untransmittable(_) => None,
            }))
            .collect()
    }
}

pub fn validate_push_response(requested: &[MutationId], response: &PushMutationsResponse) -> Result<ValidatedPushResponse, PushResponseError> {
    let requested_ids: HashSet<_> = requested.iter().copied().collect();
    if requested_ids.len() != requested.len() {
        return Err(PushResponseError::InvalidAcknowledgments("submitted mutation identities are not unique".into()));
    }
    let mut accounted = HashSet::new();
    for mutation_id in &response.accepted {
        if !requested_ids.contains(mutation_id) {
            return Err(PushResponseError::InvalidAcknowledgments(format!("server accepted unsubmitted mutation {mutation_id}")));
        }
        if !accounted.insert(*mutation_id) {
            return Err(PushResponseError::InvalidAcknowledgments(format!("server accounted for mutation {mutation_id} more than once")));
        }
    }
    let mut issues = Vec::new();
    for rejection in &response.rejected {
        if !requested_ids.contains(&rejection.mutation_id) {
            return Err(PushResponseError::InvalidAcknowledgments(format!("server rejected unsubmitted mutation {}", rejection.mutation_id)));
        }
        if !accounted.insert(rejection.mutation_id) {
            return Err(PushResponseError::InvalidAcknowledgments(format!("server accounted for mutation {} more than once", rejection.mutation_id)));
        }
        if rejection.reason.is_transient() {
            issues.push(MutationIssue::Retryable(rejection.clone()));
        } else {
            issues.push(MutationIssue::Rejected(rejection.clone()));
        }
    }
    if accounted != requested_ids {
        return Err(PushResponseError::InvalidAcknowledgments("server omitted one or more submitted mutations".into()));
    }
    Ok(ValidatedPushResponse { accepted: response.accepted.clone(), issues })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_mutation(kind: &str, revision: u64) -> crate::ServerMutation {
        crate::ServerMutation {
            mutation: crate::WireMutation {
                origin: None,
                mutation_id: MutationId::new(),
                kind: kind.to_owned(),
                entity_key: "key".to_owned(),
                entity_subkey: String::new(),
                value: Vec::new(),
                conflict_rank: 0,
                blob_reference: None,
                changed_at: 1,
                replica_seq: crate::ReplicaSeq::new(1).unwrap(),
            },
            replica_id: crate::ReplicaId::nil(),
            revision: LibraryRevision::new(revision).unwrap(),
        }
    }

    #[test]
    fn creation_prerequisites_roundtrip_without_advancing_the_cursor() {
        let mut creation = server_mutation(mutation_kind::BOOK_LIFECYCLE, 1);
        let hash = crate::ContentHash::new(&"a".repeat(64));
        creation.mutation.entity_key = hash.to_string();
        creation.mutation.blob_reference = Some(crate::DeclaredBlobReference { present: true, content_hash: Some(hash) });
        let cursor = SyncCursor { state_revision: Some(LibraryRevision::new(50).unwrap()), reading_revision: None };
        let page = PullStateResponse { book_creations: vec![creation.clone()], mutations: vec![], next_cursor: cursor, has_more: false };
        let encoded = crate::transport::encode(&page).unwrap();
        let decoded: PullStateResponse = crate::transport::decode(&encoded, crate::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
        assert_eq!(decoded.book_creations, page.book_creations);
        assert_eq!(decoded.next_cursor, cursor);
        validate_pull_batch(&decoded, cursor).unwrap();
        let mut invalid = decoded;
        invalid.book_creations.push(creation);
        assert!(validate_pull_batch(&invalid, cursor).is_err());
        invalid.book_creations.pop();
        invalid.book_creations[0].mutation.blob_reference.as_mut().unwrap().present = false;
        assert!(validate_pull_batch(&invalid, cursor).is_err());
    }

    fn wire_mutation(sequence: u64, value_bytes: usize) -> WireMutation {
        WireMutation {
            origin: None,
            mutation_id: MutationId::new(),
            kind: mutation_kind::ANNOTATION.to_owned(),
            entity_key: sequence.to_string(),
            entity_subkey: String::new(),
            value: vec![0; value_bytes],
            conflict_rank: 0,
            blob_reference: None,
            changed_at: sequence,
            replica_seq: crate::ReplicaSeq::new(sequence).unwrap(),
        }
    }

    #[test]
    fn push_batcher_preserves_order_and_sets_aside_untransmittable_mutations() {
        let first = wire_mutation(1, 1);
        let oversized = wire_mutation(2, crate::MAX_PUSH_BATCH_BYTES + 1);
        let last = wire_mutation(3, 1);
        let oversized_id = oversized.mutation_id;
        let mut batcher = PushBatcher::new(vec![first.clone(), oversized, last.clone()]).unwrap();

        assert_eq!(batcher.untransmittable(), &[oversized_id]);
        assert_eq!(batcher.next_batch().iter().map(|mutation| mutation.mutation_id).collect::<Vec<_>>(), vec![first.mutation_id, last.mutation_id]);
        assert!(batcher.is_empty());
    }

    #[test]
    fn push_batcher_limits_both_count_and_encoded_bytes() {
        let mut count_limited = PushBatcher::new((1..=crate::MAX_PUSH_MUTATIONS as u64 + 1).map(|sequence| wire_mutation(sequence, 1)).collect()).unwrap();
        assert_eq!(count_limited.next_batch().len(), crate::MAX_PUSH_MUTATIONS);
        assert_eq!(count_limited.next_batch().len(), 1);

        let mut byte_limited = PushBatcher::new((1..=100).map(|sequence| wire_mutation(sequence, 64 * 1024)).collect()).unwrap();
        let batch = byte_limited.next_batch();
        let encoded_bytes = batch.iter().map(|mutation| crate::transport::encode(mutation).unwrap().len()).sum::<usize>();
        assert!(!batch.is_empty());
        assert!(batch.len() < 100);
        assert!(encoded_bytes <= crate::MAX_PUSH_BATCH_BYTES);
        assert!(!byte_limited.is_empty());
    }

    #[test]
    fn identity_acknowledgements_do_not_depend_on_sequence_prefixes() {
        let first = MutationId::new();
        let second = MutationId::new();
        let response = PushMutationsResponse { accepted: vec![second, first], rejected: Vec::new() };
        let validated = validate_push_response(&[first, second], &response).unwrap();
        assert_eq!(validated.accepted, vec![second, first]);
    }

    #[test]
    fn every_requested_identity_is_partitioned_by_rejection_lifetime() {
        let accepted = MutationId::new();
        let transient = MutationId::new();
        let first_permanent = MutationId::new();
        let second_permanent = MutationId::new();
        let response = PushMutationsResponse {
            accepted: vec![accepted],
            rejected: vec![
                crate::MutationRejection { mutation_id: transient, reason: MutationRejectionReason::MissingDependency },
                crate::MutationRejection { mutation_id: first_permanent, reason: MutationRejectionReason::InvalidPayload },
                crate::MutationRejection { mutation_id: second_permanent, reason: MutationRejectionReason::InvalidPayload },
            ],
        };

        let validated = validate_push_response(&[accepted, transient, first_permanent, second_permanent], &response).unwrap();
        assert_eq!(validated.accepted, vec![accepted]);
        assert_eq!(
            validated.issues,
            vec![
                MutationIssue::Retryable(MutationRejection { mutation_id: transient, reason: MutationRejectionReason::MissingDependency }),
                MutationIssue::Rejected(MutationRejection { mutation_id: first_permanent, reason: MutationRejectionReason::InvalidPayload }),
                MutationIssue::Rejected(MutationRejection { mutation_id: second_permanent, reason: MutationRejectionReason::InvalidPayload }),
            ]
        );
        assert_eq!(validated.completed_ids(), vec![accepted, first_permanent, second_permanent]);
    }

    #[test]
    fn final_cursor_may_advance_past_the_last_returned_state() {
        let response = PullStateResponse { book_creations: Vec::new(), mutations: Vec::new(), next_cursor: SyncCursor { state_revision: LibraryRevision::new(10).ok(), reading_revision: LibraryRevision::new(7).ok() }, has_more: false };
        validate_pull_batch(&response, SyncCursor { state_revision: LibraryRevision::new(5).ok(), reading_revision: None }).unwrap();
        let _ = "replica".to_owned();
    }

    #[test]
    fn reading_progress_does_not_consume_the_state_channel() {
        let response = PullStateResponse {
            book_creations: Vec::new(),
            mutations: vec![server_mutation(mutation_kind::READING_POSITION, 22)],
            next_cursor: SyncCursor { state_revision: LibraryRevision::new(10).ok(), reading_revision: LibraryRevision::new(22).ok() },
            has_more: true,
        };
        validate_pull_batch(&response, SyncCursor { state_revision: LibraryRevision::new(10).ok(), reading_revision: LibraryRevision::new(20).ok() }).unwrap();
        assert_eq!(response.next_cursor.state_revision.unwrap().get(), 10);
    }
}
