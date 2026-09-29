//! Stable synchronization protocol shared by clients and the sync service.
//!
//! Library payload bytes are intentionally opaque here. Their client-side
//! schema and conversion live in `library-replica`.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroU64;

pub mod api;
pub mod book_batch;
pub use protobuf_wire as transport;
pub use wire;
pub mod mutation_ids;
pub use mutation_ids::{MutationId, MutationIdError};
mod protocol;
pub use protocol::{MutationIssue, PullBatchError, PushBatcher, PushResponseError, ValidatedPushResponse, validate_pull_batch, validate_push_response};

pub use content_address::ContentHash;

/// Deterministic content-address fixture shared by protocol client and server tests.
#[cfg(any(test, feature = "test-support"))]
pub fn fixture_content_hash(value: u64) -> ContentHash {
    ContentHash::new(&format!("{value:064x}"))
}

/// A non-negative Unix timestamp measured in milliseconds.
pub type UnixMillis = u64;

/// Mutation kinds whose envelope-level meaning is visible to the sync service.
/// Payload schemas remain client-owned in `library-replica`.
pub mod mutation_kind {
    pub const ANNOTATION: &str = "annotation";
    pub const BOOK_LIFECYCLE: &str = "book_lifecycle";
    pub const BOOK_FACTS: &str = "book_facts";
    pub const BOOK_TOC: &str = "book_toc";
    pub const DIRECTORY_LIFECYCLE: &str = "directory_lifecycle";
    pub const DIRECTORY_NAME: &str = "directory_name";
    pub const DIRECTORY_PARENT: &str = "directory_parent";
    pub const METADATA: &str = "metadata";
    pub const PDF_READER_METADATA: &str = "pdf_reader_metadata";
    pub const DESCRIPTION: &str = "description";
    pub const PLACEMENT: &str = "placement";
    pub const READING_POSITION: &str = "reading_position";
}

/// The replica that produced a synchronized mutation.
pub type ReplicaId = uuid::Uuid;
/// A synchronized library.
pub type LibraryId = uuid::Uuid;
/// A synchronized directory.
pub type DirId = uuid::Uuid;
/// A file name is user-visible text, not an identity.
pub type FileName = String;

/// The canonical root directory. No generated directory can collide with nil.
pub const ROOT_DIR_ID: DirId = uuid::Uuid::nil();

/// Maximum encoded size of a synchronized JPEG cover.
pub const MAX_THUMBNAIL_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_PULL_CHANGES: usize = 1_000;
pub const MAX_PUSH_MUTATIONS: usize = 1_000;
/// Target ceiling for the encoded mutations in one push request.
pub const MAX_PUSH_BATCH_BYTES: usize = 4 * 1024 * 1024;
/// Backwards-compatible name for the batch ceiling.
pub const MAX_PUSH_MUTATION_BYTES: usize = MAX_PUSH_BATCH_BYTES;
/// Admission bound shared by the generic service and domain clients.
pub const MAX_LWW_FUTURE_SKEW_MS: u64 = 5 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceError;

impl fmt::Display for SequenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sequence numbers must be between one and the signed 64-bit database limit")
    }
}

impl std::error::Error for SequenceError {}

macro_rules! sequence_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(NonZeroU64);

        impl $name {
            pub fn new(value: u64) -> Result<Self, SequenceError> {
                if value > i64::MAX as u64 {
                    return Err(SequenceError);
                }
                NonZeroU64::new(value).map(Self).ok_or(SequenceError)
            }

            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(formatter)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

sequence_type!(ReplicaSeq);
sequence_type!(LibraryRevision);

/// Canonical total order for last-writer-wins state cells.
///
/// Payload interpretation remains client-owned, but every Rust component that
/// selects a winner must compare this shared key. PostgreSQL mirrors this exact
/// field order in `enforce_lww_order()` so the atomic upsert has the same law.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VersionKey {
    pub changed_at: UnixMillis,
    pub conflict_rank: u8,
    pub replica_id: ReplicaId,
    pub replica_seq: u64,
    pub mutation_id: MutationId,
}

impl VersionKey {
    pub fn new(changed_at: UnixMillis, conflict_rank: u8, replica_id: ReplicaId, replica_seq: u64, mutation_id: MutationId) -> Self {
        Self { changed_at, conflict_rank, replica_id, replica_seq, mutation_id }
    }

    pub fn from_wire(mutation: &WireMutation, replica_id: ReplicaId) -> Self {
        if let Some(origin) = &mutation.origin {
            return Self::new(mutation.changed_at, mutation.conflict_rank, origin.replica_id, origin.replica_seq.get(), origin.mutation_id);
        }
        Self::new(mutation.changed_at, mutation.conflict_rank, replica_id, mutation.replica_seq.get(), mutation.mutation_id)
    }
}

/// Compares versions known to have been produced by the same replica.
///
/// Replica identity is constant in this case, so omitting it produces exactly
/// the same ordering as [`VersionKey`] without requiring callers to manufacture
/// an identity they do not otherwise need.
pub fn compare_same_replica_versions(left: (UnixMillis, u8, ReplicaSeq, MutationId), right: (UnixMillis, u8, ReplicaSeq, MutationId)) -> Ordering {
    left.cmp(&right)
}

/// A checkpoint over independently paged synchronization channels.
/// A component advances only after that channel has been inspected.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncCursor {
    #[serde(default, deserialize_with = "deserialize_library_cursor")]
    pub state_revision: Option<LibraryRevision>,
    #[serde(default, deserialize_with = "deserialize_library_cursor")]
    pub reading_revision: Option<LibraryRevision>,
}

/// Mutation envelope transported and persisted by the sync service. `value`
/// is an encoded protobuf `MutationValue`; the service forwards it opaquely so
/// it need not depend on client domain models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WireMutation {
    pub mutation_id: MutationId,
    pub kind: String,
    pub entity_key: String,
    #[serde(default)]
    pub entity_subkey: String,
    pub value: Vec<u8>,
    pub conflict_rank: u8,
    #[serde(default)]
    pub blob_reference: Option<DeclaredBlobReference>,
    pub changed_at: UnixMillis,
    pub replica_seq: ReplicaSeq,
    /// Original winner identity when forwarding retained state. Transport IDs
    /// above remain local to the publisher and are used only for acknowledgements.
    pub origin: Option<MutationOrigin>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationOrigin {
    pub replica_id: ReplicaId,
    pub replica_seq: ReplicaSeq,
    pub mutation_id: MutationId,
}

/// Server-visible book prerequisite. Lifecycle declarations also own the
/// reference count; declarations on other state kinds do not affect accounting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeclaredBlobReference {
    pub present: bool,
    pub content_hash: Option<ContentHash>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerMutation {
    pub mutation: WireMutation,
    pub replica_id: ReplicaId,
    pub revision: LibraryRevision,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushMutationsRequest {
    pub library_id: LibraryId,
    pub replica_id: ReplicaId,
    pub mutations: Vec<WireMutation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushMutationsResponse {
    #[serde(default)]
    pub accepted: Vec<MutationId>,
    #[serde(default)]
    pub rejected: Vec<MutationRejection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationRejection {
    pub mutation_id: MutationId,
    pub reason: MutationRejectionReason,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationRejectionReason {
    TimestampTooFarFuture,
    InvalidPayload,
    MissingDependency,
}

impl MutationRejectionReason {
    pub fn is_transient(self) -> bool {
        matches!(self, Self::TimestampTooFarFuture | Self::MissingDependency)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullStateResponse {
    /// Explicit creation prerequisites, applied before mutations. These may be
    /// behind the pull cursor and never advance it. Only live/Trash lifecycle
    /// records are allowed; ordinary field values cannot create a book.
    #[serde(default)]
    pub book_creations: Vec<ServerMutation>,
    pub mutations: Vec<ServerMutation>,
    pub next_cursor: SyncCursor,
    pub has_more: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullStateQuery {
    pub library_id: LibraryId,
    pub replica_id: ReplicaId,
    #[serde(default)]
    pub cursor: SyncCursor,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncExchangeRequest {
    pub library_id: LibraryId,
    pub replica_id: ReplicaId,
    #[serde(default)]
    pub mutations: Vec<WireMutation>,
    #[serde(default)]
    pub cursor: SyncCursor,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncExchangeResponse {
    pub push: PushMutationsResponse,
    pub pull: PullStateResponse,
}

/// The stable identity of one independently merged synchronization register.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StateCell {
    pub kind: String,
    pub entity_key: String,
    #[serde(default)]
    pub entity_subkey: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StateInventoryRequest {
    pub library_id: LibraryId,
    pub cells: Vec<StateCell>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StateInventoryResponse {
    pub missing: Vec<StateCell>,
}

/// Inventory is deliberately paged independently of mutation exchange so an
/// open library never needs to build or transmit one unbounded request.
pub const MAX_STATE_INVENTORY_CELLS: usize = 1_000;

fn deserialize_library_cursor<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<LibraryRevision>, D::Error> {
    match Option::<u64>::deserialize(deserializer)? {
        None | Some(0) => Ok(None),
        Some(value) => LibraryRevision::new(value).map(Some).map_err(serde::de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutation_id(value: u128) -> MutationId {
        MutationId::parse(&uuid::Uuid::from_u128(value).to_string()).unwrap()
    }

    #[test]
    fn version_key_orders_every_tie_breaker_in_protocol_order() {
        let replica = uuid::Uuid::from_u128(10);
        let base = VersionKey::new(10, 1, replica, 10, mutation_id(10));

        assert!(VersionKey::new(11, 0, uuid::Uuid::nil(), 1, mutation_id(1)) > base);
        assert!(VersionKey::new(10, 2, uuid::Uuid::nil(), 1, mutation_id(1)) > base);
        assert!(VersionKey::new(10, 1, uuid::Uuid::from_u128(11), 1, mutation_id(1)) > base);
        assert!(VersionKey::new(10, 1, replica, 11, mutation_id(1)) > base);
        assert!(VersionKey::new(10, 1, replica, 10, mutation_id(11)) > base);
    }

    #[test]
    fn same_replica_comparison_is_the_version_key_order_without_replica_identity() {
        let replica = uuid::Uuid::from_u128(10);
        let left = (10, 1, ReplicaSeq::new(3).unwrap(), mutation_id(4));
        let right = (10, 1, ReplicaSeq::new(3).unwrap(), mutation_id(5));

        assert_eq!(compare_same_replica_versions(left, right), VersionKey::new(left.0, left.1, replica, left.2.get(), left.3).cmp(&VersionKey::new(right.0, right.1, replica, right.2.get(), right.3)));
    }

    #[test]
    fn protocol_identifiers_use_their_canonical_strings_in_json_and_bincode_storage() {
        let content_hash = ContentHash::new(&"a".repeat(64));
        let mutation_id = MutationId::parse("12345678-1234-5678-9234-567812345678").unwrap();

        assert_eq!(serde_json::to_value(&content_hash).unwrap(), serde_json::json!(content_hash.as_str()));
        assert_eq!(serde_json::to_value(mutation_id).unwrap(), serde_json::json!(mutation_id.to_string()));

        assert_eq!(wire::encode(&content_hash).unwrap(), wire::encode(&content_hash.as_str()).unwrap());
        assert_eq!(wire::encode(&mutation_id).unwrap(), wire::encode(&mutation_id.to_string()).unwrap());
    }

    #[test]
    fn transport_and_storage_codecs_are_not_interchangeable() {
        let value = ContentHash::new(&"a".repeat(64));
        let stored = wire::encode(&value).unwrap();
        let transported = transport::encode(&value).unwrap();

        assert!(stored.starts_with(b"BKHM"));
        assert!(!transported.starts_with(b"BKHM"));
        assert!(wire::decode::<ContentHash>(&transported, 1024).is_err());
        assert!(transport::decode::<ContentHash>(&stored, 1024).is_err());
    }

    #[test]
    fn recovery_origin_survives_transport_without_promoting_the_version() {
        let original = MutationOrigin { replica_id: uuid::Uuid::from_u128(1), replica_seq: ReplicaSeq::new(2).unwrap(), mutation_id: MutationId::new() };
        let mutation = WireMutation {
            mutation_id: MutationId::new(),
            kind: "directory_name".into(),
            entity_key: "shelf".into(),
            entity_subkey: String::new(),
            value: vec![],
            conflict_rank: 0,
            blob_reference: None,
            changed_at: 10,
            replica_seq: ReplicaSeq::new(500).unwrap(),
            origin: Some(original.clone()),
        };
        let decoded: WireMutation = transport::decode(&transport::encode(&mutation).unwrap(), transport::MAX_DECODED_REQUEST_BYTES).unwrap();
        assert_eq!(decoded, mutation);
        assert_eq!(VersionKey::from_wire(&decoded, uuid::Uuid::from_u128(999)), VersionKey::new(10, 0, original.replica_id, 2, original.mutation_id));
    }

    #[test]
    fn wire_mutation_preserves_an_absent_blob_reference_in_binary_encoding() {
        let mutation = WireMutation {
            origin: None,
            mutation_id: MutationId::new(),
            kind: "directory_name".to_owned(),
            entity_key: "shelf".to_owned(),
            entity_subkey: String::new(),
            value: Vec::new(),
            conflict_rank: 0,
            blob_reference: None,
            changed_at: 1,
            replica_seq: ReplicaSeq::new(1).unwrap(),
        };
        let encoded = wire::encode(&mutation).unwrap();
        assert_eq!(wire::decode::<WireMutation>(&encoded, wire::MAX_DECODED_REQUEST_BYTES).unwrap(), mutation);
    }
}
