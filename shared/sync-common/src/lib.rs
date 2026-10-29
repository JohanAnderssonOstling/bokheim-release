//! Stable synchronization protocol shared by clients and the sync service.
//!
//! Library payload bytes are intentionally opaque here. Their client-side
//! schema lives in `library-replica`.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroU64;

pub mod api;
pub mod storage;
pub use protobuf_wire as transport;
pub use wire;
mod protocol;
mod transport_fields;
pub use protocol::{validate_pull_batch, validate_push_response, MutationIssue, PullBatchError, PushBatcher, PushResponseError, ValidatedPushResponse};

pub use content_address::BookId;

/// Deterministic content-address fixture shared by protocol client and server tests.

/// Delivery fixture for values authored by the supplied replica. Forwarded
/// values already have a non-nil original author and preserve it.

/// A non-negative Unix timestamp measured in milliseconds.
pub type UnixMillis = u64;

/// The replica that produced a synchronized mutation.
pub type ReplicaId = uuid::Uuid;
/// A synchronized library.
pub type LibraryId = uuid::Uuid;
/// The server database history to which a pull checkpoint belongs.
pub type SyncGeneration = uuid::Uuid;
/// A synchronized directory.
pub type DirId = uuid::Uuid;
/// A file name is user-visible text, not an identity.
pub type FileName = String;

/// The canonical root directory. No generated directory can collide with nil.
pub const ROOT_DIR_ID: DirId = uuid::Uuid::nil();

/// Maximum encoded size of a synchronized JPEG cover.
pub const MAX_THUMBNAIL_BYTES: u64 = 4 * 1024 * 1024;
/// Decoded-byte budget for a pull page, without a separate change-count cap.
/// Reserve 512 KiB below the hard response limit for the outer envelope and
/// push rejections. HTTP compression determines the transferred byte count.
pub const MAX_PULL_BATCH_BYTES: usize = transport::MAX_DECODED_RESPONSE_BYTES - 512 * 1024;
/// Fixed protobuf wrapper allowance; variable-length fields are counted too.
pub const STATE_DELIVERY_OVERHEAD_BYTES: usize = 1_024;
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
/// A replica's sequence only increases, so `(replica_id, replica_seq)` names
/// one authored change and needs no further tie-breaker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct VersionKey {
    #[serde(deserialize_with = "deserialize_storable_millis")]
    pub changed_at: UnixMillis,
    pub conflict_rank: ConflictRank,
    pub replica_id: ReplicaId,
    pub replica_seq: ReplicaSeq,
}

impl VersionKey {
    pub fn new(changed_at: UnixMillis, conflict_rank: ConflictRank, replica_id: ReplicaId, replica_seq: ReplicaSeq) -> Self {
        Self { changed_at, conflict_rank, replica_id, replica_seq }
    }
}

/// Timestamps are stored in signed 64-bit database columns.
fn deserialize_storable_millis<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<UnixMillis, D::Error> {
    let value = UnixMillis::deserialize(deserializer)?;
    if value > i64::MAX as u64 {
        return Err(serde::de::Error::custom("timestamp exceeds the signed 64-bit database limit"));
    }
    Ok(value)
}

/// Breaks an exact timestamp tie: destructive updates win, and a purge wins
/// over a delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum ConflictRank {
    Update = 0,
    Delete = 1,
    Purge = 2,
}

impl From<ConflictRank> for u8 {
    fn from(rank: ConflictRank) -> Self {
        rank as u8
    }
}

impl TryFrom<u8> for ConflictRank {
    type Error = ConflictRankError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Update),
            1 => Ok(Self::Delete),
            2 => Ok(Self::Purge),
            _ => Err(ConflictRankError),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConflictRankError;

impl fmt::Display for ConflictRankError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("conflict rank must be update, delete, or purge")
    }
}

impl std::error::Error for ConflictRankError {}

/// Compares versions known to have been produced by the same replica.
///
/// Replica identity is constant in this case, so omitting it produces exactly
/// the same ordering as [`VersionKey`] without requiring callers to manufacture
/// an identity they do not otherwise need.
pub fn compare_same_replica_versions(left: (UnixMillis, ConflictRank, ReplicaSeq), right: (UnixMillis, ConflictRank, ReplicaSeq)) -> Ordering {
    left.cmp(&right)
}

/// A pull checkpoint. Book lifecycles are delivered before every other value,
/// and a field is delivered only at or below the lifecycle cursor, so a field
/// never reaches a replica before its book. `field_revision` never exceeds
/// `lifecycle_revision`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncCursor {
    /// Absent only when starting from zero, including cursor recovery.
    #[serde(default)]
    pub generation: Option<SyncGeneration>,
    #[serde(default, deserialize_with = "deserialize_library_cursor")]
    pub lifecycle_revision: Option<LibraryRevision>,
    #[serde(default, deserialize_with = "deserialize_library_cursor")]
    pub field_revision: Option<LibraryRevision>,
}

impl SyncCursor {
    /// An unbound cursor can only bootstrap from zero. Existing checkpoints
    /// must name the current history, even when its head has passed them.
    pub fn belongs_to(self, generation: SyncGeneration) -> bool {
        match self.generation {
            Some(stored) => stored == generation,
            None => self == Self::default(),
        }
    }
}

/// A canonical value and its original last-writer-wins version.
#[derive(Debug, Clone, PartialEq)]
pub struct Versioned<T> {
    pub value: T,
    pub version: VersionKey,
}

/// An immutable submission snapshot. The publisher's sequence identifies it
/// within the publisher's requests; state.version always belongs to the
/// original author.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(bound = "T: StatePayload")]
pub struct Publication<T> {
    pub publisher_seq: ReplicaSeq,
    pub state: Versioned<T>,
}

/// The only representation needed by the service: an open state address, the
/// book the value belongs to, and the opaque serialized domain value. A purge
/// removes every value of its book; a book lifecycle holds the book's bytes
/// unless its version is a purge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpaqueStateEntry {
    pub address: StateAddress,
    pub book: Option<BookId>,
    // Keep opaque payloads as byte buffers in the codec's intermediate trees.
    // Protobuf bytes and the persisted bincode layout stay identical.
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

impl OpaqueStateEntry {
    pub fn delivery_byte_budget(&self) -> usize {
        self.bytes.len().saturating_add(self.address.kind.len()).saturating_add(self.address.entity_key.as_bytes().len()).saturating_add(self.address.entity_subkey.len()).saturating_add(STATE_DELIVERY_OVERHEAD_BYTES)
    }
}

/// Client-owned serialization and envelope validation. This trait does not
/// introduce a second domain model or require the service to link that model.
pub trait StatePayload: Serialize + serde::de::DeserializeOwned + Sized {
    fn recognizes_kind(_: &str) -> bool {
        true
    }
    fn validate_version(&self, _: &VersionKey) -> Result<(), transport::WireError> {
        Ok(())
    }
    fn serialize_entry<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
    fn deserialize_entry<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error>;
}

impl StatePayload for OpaqueStateEntry {
    fn serialize_entry<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        transport_fields::opaque::serialize(self, serializer)
    }
    fn deserialize_entry<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        transport_fields::opaque::deserialize(deserializer)
    }
}

/// Serialization of state payloads belongs at the wire boundary. Persisted
/// values retain their canonical representation and historical binary layout.
pub mod state_encoding {
    use super::*;

    pub fn serialize<T: StatePayload, S: serde::Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            value.serialize_entry(serializer)
        } else {
            value.serialize(serializer)
        }
    }
    pub fn deserialize<'de, T: StatePayload, D: serde::Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
        if deserializer.is_human_readable() {
            T::deserialize_entry(deserializer)
        } else {
            T::deserialize(deserializer)
        }
    }

    /// Unknown kinds are retained by the relay and skipped by older domain clients.
    pub fn decode_known<T: StatePayload>(state: &Versioned<OpaqueStateEntry>) -> Result<Option<Versioned<T>>, transport::WireError> {
        if !T::recognizes_kind(&state.value.address.kind) {
            return Ok(None);
        }
        transport::representation(state).map(Some)
    }
}

impl<T: StatePayload> Serialize for Versioned<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        self.value.validate_version(&self.version).map_err(serde::ser::Error::custom)?;
        // The adapter serializes just the value field, without rebuilding a
        // Versioned<OpaqueStateEntry> or copying version metadata.
        struct Entry<'a, T>(&'a T);
        impl<T: StatePayload> Serialize for Entry<'_, T> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                state_encoding::serialize(self.0, serializer)
            }
        }
        let mut fields = serializer.serialize_struct("Versioned", 2)?;
        fields.serialize_field("value", &Entry(&self.value))?;
        fields.serialize_field("version", &transport_fields::VersionRef(&self.version))?;
        fields.end()
    }
}
impl<'de, T: StatePayload> Deserialize<'de> for Versioned<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(bound = "T: StatePayload")]
        struct Fields<T> {
            #[serde(deserialize_with = "state_encoding::deserialize")]
            value: T,
            #[serde(with = "transport_fields::version")]
            version: VersionKey,
        }
        let fields = Fields::<T>::deserialize(deserializer)?;
        fields.value.validate_version(&fields.version).map_err(serde::de::Error::custom)?;
        Ok(Self { value: fields.value, version: fields.version })
    }
}

pub type OpaquePublication = Publication<OpaqueStateEntry>;
pub type StateDelivery = Delivery<OpaqueStateEntry>;

/// Server delivery metadata is independent of the state's winning version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(bound = "T: StatePayload")]
pub struct Delivery<T> {
    pub state: Versioned<T>,
    pub revision: LibraryRevision,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PushMutationsRequest {
    pub library_id: LibraryId,
    pub replica_id: ReplicaId,
    pub mutations: Vec<OpaquePublication>,
}

/// Every submitted publication not listed as rejected was accepted.
#[derive(Debug, Serialize, Deserialize)]
pub struct PushMutationsResponse {
    #[serde(default)]
    pub rejected: Vec<MutationRejection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationRejection {
    pub publisher_seq: ReplicaSeq,
    pub reason: MutationRejectionReason,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationRejectionReason {
    TimestampTooFarFuture,
    InvalidPayload,
}

impl MutationRejectionReason {
    pub fn is_transient(self) -> bool {
        matches!(self, Self::TimestampTooFarFuture)
    }
}

/// Lifecycle mutations precede field mutations within a page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullStateResponse {
    pub mutations: Vec<StateDelivery>,
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
    pub mutations: Vec<OpaquePublication>,
    #[serde(default)]
    pub cursor: SyncCursor,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncExchangeResponse {
    pub push: PushMutationsResponse,
    pub pull: PullStateResponse,
}

/// The stable identity of one independently merged synchronization register.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateAddress {
    pub kind: String,
    pub entity_key: StateKey,
    #[serde(default)]
    pub entity_subkey: String,
}

fn deserialize_library_cursor<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<LibraryRevision>, D::Error> {
    match Option::<u64>::deserialize(deserializer)? {
        None | Some(0) => Ok(None),
        Some(value) => LibraryRevision::new(value).map(Some).map_err(serde::de::Error::custom),
    }
}

mod state_key;
pub use state_key::StateKey;
impl<'de> Deserialize<'de> for StateAddress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Address { kind: String, entity_key: StateKey, #[serde(default)] entity_subkey: String }
        let Address { kind, mut entity_key, entity_subkey } = Address::deserialize(deserializer)?;
        if state_key::book_kind(&kind) {
            if let StateKey::Text(text) = entity_key { entity_key = StateKey::Book(text.parse().map_err(serde::de::Error::custom)?); }
        } else if matches!(entity_key,StateKey::Book(_)) { return Err(serde::de::Error::custom("text register requires a text key")); }
        Ok(Self { kind, entity_key, entity_subkey })
    }
}
