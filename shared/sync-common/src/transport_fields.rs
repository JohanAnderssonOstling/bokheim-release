//! Typed delivery bookkeeping. Domain payloads remain opaque to the relay;
//! the shared adapter retains legacy transport, JSON, and storage layouts.
use crate::{state_key, BookId, ConflictRank, OpaqueStateEntry, ReplicaId, ReplicaSeq, StateAddress, StateKey, VersionKey};
use prost::{Message, Oneof};
use protobuf_wire::compact::{self, Codec};
use serde::{Deserializer, Serialize, Serializer};

#[derive(Clone, PartialEq, Message)]
struct State {
    #[prost(string, tag = "1")]
    kind: String,
    #[prost(oneof = "Key", tags = "2, 3")]
    key: Option<Key>,
    #[prost(string, tag = "4")]
    subkey: String,
    #[prost(bytes = "vec", optional, tag = "5")]
    book: Option<Vec<u8>>,
    #[prost(bytes = "vec", tag = "6")]
    payload: Vec<u8>,
}

#[derive(Clone, PartialEq, Oneof)]
enum Key {
    #[prost(bytes, tag = "2")]
    Book(Vec<u8>),
    #[prost(string, tag = "3")]
    Text(String),
}

#[derive(Clone, PartialEq, Message)]
struct Version {
    #[prost(uint64, tag = "1")]
    changed_at: u64,
    #[prost(uint32, tag = "2")]
    rank: u32,
    #[prost(bytes = "vec", tag = "3")]
    replica: Vec<u8>,
    #[prost(uint64, tag = "4")]
    sequence: u64,
}

struct StateCodec;
impl Codec for StateCodec {
    type Value = OpaqueStateEntry;
    fn pack(value: &OpaqueStateEntry) -> Result<Vec<u8>, String> {
        let key = match &value.address.entity_key {
            StateKey::Book(book) => Key::Book(book.as_bytes().to_vec()),
            StateKey::Text(text) => Key::Text(text.clone()),
        };
        let state = State { kind: value.address.kind.clone(), key: Some(key), subkey: value.address.entity_subkey.clone(), book: value.book.map(|book| book.as_bytes().to_vec()), payload: value.bytes.clone() };
        if state.encoded_len() > protobuf_wire::MAX_DECODED_RESPONSE_BYTES {
            return Err("sync state exceeds its transport limit".into());
        }
        Ok(state.encode_to_vec())
    }
    fn unpack(bytes: &[u8]) -> Result<OpaqueStateEntry, String> {
        if bytes.len() > protobuf_wire::MAX_DECODED_RESPONSE_BYTES {
            return Err("sync state exceeds its transport limit".into());
        }
        let state = State::decode(bytes).map_err(|error| error.to_string())?;
        let key = match state.key.ok_or("missing state key")? {
            Key::Book(bytes) if state_key::book_kind(&state.kind) => StateKey::Book(BookId::try_from(bytes.as_slice()).map_err(|error| error.to_string())?),
            Key::Book(_) => return Err("text register requires a text key".into()),
            Key::Text(text) if state_key::book_kind(&state.kind) => StateKey::Book(text.parse::<BookId>().map_err(|error| error.to_string())?),
            Key::Text(text) => StateKey::Text(text),
        };
        let book = state.book.map(|book| BookId::try_from(book.as_slice()).map_err(|error| error.to_string())).transpose()?;
        Ok(OpaqueStateEntry { address: StateAddress { kind: state.kind, entity_key: key, entity_subkey: state.subkey }, book, bytes: state.payload })
    }
}

struct VersionCodec;
impl Codec for VersionCodec {
    type Value = VersionKey;
    fn pack(value: &VersionKey) -> Result<Vec<u8>, String> {
        Ok(Version { changed_at: value.changed_at, rank: u8::from(value.conflict_rank).into(), replica: value.replica_id.as_bytes().to_vec(), sequence: value.replica_seq.get() }.encode_to_vec())
    }
    fn unpack(bytes: &[u8]) -> Result<VersionKey, String> {
        if bytes.len() > 128 {
            return Err("sync version exceeds its transport limit".into());
        }
        let version = Version::decode(bytes).map_err(|error| error.to_string())?;
        if version.changed_at > i64::MAX as u64 {
            return Err("timestamp exceeds storage range".into());
        }
        let rank = u8::try_from(version.rank).map_err(|_| "invalid conflict rank")?;
        let rank = ConflictRank::try_from(rank).map_err(|error| error.to_string())?;
        let replica = ReplicaId::from_slice(&version.replica).map_err(|error| error.to_string())?;
        let sequence = ReplicaSeq::new(version.sequence).map_err(|error| error.to_string())?;
        Ok(VersionKey::new(version.changed_at, rank, replica, sequence))
    }
}

pub(super) mod opaque {
    use super::*;
    pub fn serialize<S: Serializer>(value: &OpaqueStateEntry, serializer: S) -> Result<S::Ok, S::Error> {
        compact::serialize::<StateCodec, S>(value, serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<OpaqueStateEntry, D::Error> {
        compact::deserialize::<StateCodec, D>(deserializer)
    }
}

pub(super) mod version {
    use super::*;
    pub fn serialize<S: Serializer>(value: &VersionKey, serializer: S) -> Result<S::Ok, S::Error> {
        compact::serialize::<VersionCodec, S>(value, serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<VersionKey, D::Error> {
        compact::deserialize::<VersionCodec, D>(deserializer)
    }
}

#[derive(Serialize)]
#[serde(transparent)]
pub(super) struct VersionRef<'a>(#[serde(with = "version")] pub &'a VersionKey);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{transport, wire, Versioned};

    #[derive(Serialize)]
    struct Previous<'a> {
        value: &'a OpaqueStateEntry,
        version: &'a VersionKey,
    }

    fn state(kind: &str, key: StateKey, book: Option<BookId>) -> OpaqueStateEntry {
        OpaqueStateEntry { address: StateAddress { kind: kind.into(), entity_key: key, entity_subkey: "entry".into() }, book, bytes: vec![0, 128, 255] }
    }

    #[test]
    fn delivery_bookkeeping_preserves_legacy_payloads_json_and_storage() {
        let hash = BookId::from_bytes([42; 32]);
        for value in [state("book_add", hash.into(), Some(hash)), state("annotation", "a".repeat(64).into(), Some(hash)), state("future_kind", "unknown".into(), None)] {
            for rank in [ConflictRank::Update, ConflictRank::Delete, ConflictRank::Purge] {
                let version = VersionKey::new(i64::MAX as u64, rank, ReplicaId::from_u128(42), ReplicaSeq::new(i64::MAX as u64).unwrap());
                let previous = Previous { value: &value, version: &version };
                let current = Versioned { value: value.clone(), version: version.clone() };
                let json = serde_json::to_vec(&previous).unwrap();
                assert_eq!(serde_json::to_vec(&current).unwrap(), json);
                assert_eq!(serde_json::from_slice::<Versioned<OpaqueStateEntry>>(&json).unwrap(), current);
                let stored = wire::encode(&previous).unwrap();
                assert_eq!(wire::encode(&current).unwrap(), stored);
                assert_eq!(wire::decode::<Versioned<OpaqueStateEntry>>(&stored, 4096).unwrap(), current);
                let mut legacy = transport::encode(&previous).unwrap();
                legacy[1] = 30;
                assert_eq!(transport::decode_state::<Versioned<OpaqueStateEntry>>(&legacy, 4096).unwrap(), current);
                let packed = transport::encode(&current).unwrap();
                assert_eq!(packed.len(), transport::encoded_len(&current).unwrap());
                assert_eq!(transport::decode::<Versioned<OpaqueStateEntry>>(&packed, 4096).unwrap(), current);
                assert_eq!(transport::decode_byte_fields::<Versioned<OpaqueStateEntry>>(&packed, 4096).unwrap(), current);
                assert_eq!(transport::representation::<_, Versioned<OpaqueStateEntry>>(&current).unwrap(), current);
                assert!(packed.len() < transport::encoded_len(&previous).unwrap());
            }
        }
    }

    #[test]
    fn compact_versions_reject_invalid_rank_uuid_time_and_sequence() {
        let valid = Version { changed_at: 0, rank: 0, replica: vec![0; 16], sequence: 1 };
        assert!(VersionCodec::unpack(&valid.encode_to_vec()).is_ok());
        for invalid in [
            Version { rank: 3, ..valid.clone() },
            Version { rank: 256, ..valid.clone() },
            Version { replica: vec![0; 15], ..valid.clone() },
            Version { changed_at: u64::MAX, ..valid.clone() },
            Version { sequence: 0, ..valid.clone() },
            Version { sequence: u64::MAX, ..valid.clone() },
        ] {
            assert!(VersionCodec::unpack(&invalid.encode_to_vec()).is_err());
        }
        assert!(VersionCodec::unpack(&[0xff]).is_err());
        assert!(VersionCodec::unpack(&[0; 129]).is_err());
    }

    #[test]
    fn compact_state_keys_preserve_book_and_text_identity_rules() {
        let hash = BookId::from_bytes([42; 32]);
        let state = State { kind: "book_add".into(), key: Some(Key::Text(hash.to_string())), subkey: String::new(), book: None, payload: vec![] };
        assert_eq!(StateCodec::unpack(&state.encode_to_vec()).unwrap().address.entity_key, StateKey::Book(hash));
        for invalid in [
            State { key: None, ..state.clone() },
            State { key: Some(Key::Book(vec![0; 31])), ..state.clone() },
            State { kind: "annotation".into(), key: Some(Key::Book(hash.as_bytes().to_vec())), ..state.clone() },
            State { book: Some(vec![0; 31]), ..state.clone() },
        ] {
            assert!(StateCodec::unpack(&invalid.encode_to_vec()).is_err());
        }
        assert!(StateCodec::unpack(&[0xff]).is_err());
    }
}
