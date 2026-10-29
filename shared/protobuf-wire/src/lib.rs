//! Protobuf codec for HTTP and WebSocket transport only.
//!
//! Persisted BLOBs use the separate `wire` storage crate. Synchronization
//! values serialize their domain types through this same codec.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;

pub mod compact;
mod value_serializer;
mod canonical;
mod deserializer;
mod serializer;
mod size_serializer;
mod serialization;
#[cfg(test)]
mod legacy_serializer;
#[cfg(test)]
mod compatibility_tests;

pub const MEDIA_TYPE: &str = "application/vnd.bokheim+protobuf; version=31";
pub const MEDIA_TYPE_BASE: &str = "application/vnd.bokheim+protobuf";
pub const MAX_DECODED_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DECODED_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

const VERSION: u32 = 31;

/// The protobuf protocol version written into every envelope.
pub const fn version() -> u32 {
    VERSION
}

pub fn is_current_media_type(value: &str) -> bool {
    let mut fields = value.split(';');
    if !fields.next().is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case(MEDIA_TYPE_BASE)) {
        return false;
    }
    let mut version = None;
    for parameter in fields {
        let Some((name, value)) = parameter.trim().split_once('=') else { continue };
        if name.trim().eq_ignore_ascii_case("version") {
            if version.is_some() {
                return false;
            }
            version = Some(value.trim());
        }
    }
    version.and_then(|value| value.parse::<u32>().ok()) == Some(VERSION)
}

#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    Encode(String),
    Decode(String),
    InvalidHeader,
    UnsupportedVersion(u32),
    TooLarge { limit: usize },
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(reason) => write!(formatter, "cannot encode Bokheim body: {reason}"),
            Self::Decode(reason) => write!(formatter, "cannot decode Bokheim body: {reason}"),
            Self::InvalidHeader => formatter.write_str("Bokheim body has an invalid wire header"),
            Self::UnsupportedVersion(version) => write!(formatter, "Bokheim wire version {version} is unsupported"),
            Self::TooLarge { limit } => write!(formatter, "decoded Bokheim body exceeds the {limit}-byte limit"),
        }
    }
}

impl std::error::Error for WireError {}

pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, WireError> {
    use prost::Message;

    let value = serializer::to_value(value).map_err(|error| WireError::Encode(error.to_string()))?;
    let envelope = protobuf::Envelope { version: VERSION, value: Some(value) };
    let mut body = Vec::with_capacity(envelope.encoded_len());
    envelope.encode(&mut body).map_err(|error| WireError::Encode(error.to_string()))?;
    Ok(body)
}

/// Exact encoded length without encoding a temporary message or copying byte
/// payloads. Uses the same adapters and map duplicate rules as `encode`.
pub fn encoded_len<T: Serialize + ?Sized>(value: &T) -> Result<usize, WireError> {
    size_serializer::encoded_len(value).map_err(|error| WireError::Encode(error.to_string()))
}

/// Apply serialization adapters without encoding and reparsing an outer byte
/// envelope. Used when a relay needs the opaque form of a typed payload.
pub fn representation<S: Serialize + ?Sized, D: DeserializeOwned>(value: &S) -> Result<D, WireError> {
    let value = value_serializer::to_value(value).map_err(|error| WireError::Encode(error.to_string()))?;
    D::deserialize(value).map_err(|error| WireError::Decode(error.to_string()))
}

pub fn decode<T: DeserializeOwned>(body: &[u8], decoded_limit: usize) -> Result<T, WireError> {
    decode_versions(body, decoded_limit, &[VERSION], false)
}

/// For contracts whose byte fields use `serde_bytes`. Keeps protobuf bytes
/// compact through deserialization instead of allocating one Value per byte.
/// The wire representation and strict version checks are unchanged.
pub fn decode_byte_fields<T: DeserializeOwned>(body: &[u8], decoded_limit: usize) -> Result<T, WireError> {
    decode_versions(body, decoded_limit, &[VERSION], true)
}

/// Reads opaque domain values retained by the sync relay. Historical content
/// identities retain their encodings in protocol 31. Versions 28 through 30 remain
/// readable. HTTP and
/// WebSocket envelopes must still use the strict current-version `decode`.
pub fn decode_state<T: DeserializeOwned>(body: &[u8], decoded_limit: usize) -> Result<T, WireError> {
    decode_versions(body, decoded_limit, &[28, 29, 30, VERSION], false)
}

fn decode_versions<T: DeserializeOwned>(body: &[u8], decoded_limit: usize, versions: &[u32], byte_fields: bool) -> Result<T, WireError> {
    use prost::Message;

    if body.len() > decoded_limit {
        return Err(WireError::TooLarge { limit: decoded_limit });
    }
    let envelope = protobuf::Envelope::decode(body).map_err(|error| WireError::Decode(error.to_string()))?;
    if !canonical::matches(&envelope, body) {
        return Err(WireError::Decode("non-canonical or trailing protobuf data".to_owned()));
    }
    if !versions.contains(&envelope.version) {
        return Err(WireError::UnsupportedVersion(envelope.version));
    }
    let value = envelope.value.ok_or(WireError::InvalidHeader)?;
    deserializer::validate(&value)?;
    T::deserialize(deserializer::Deserializer { value, byte_fields }).map_err(|error| WireError::Decode(error.to_string()))
}

fn protobuf_value(value: serde_value::Value) -> Result<protobuf::Value, WireError> {
    use protobuf::value::Kind;
    use serde_value::Value;

    let kind = match value {
        Value::Bool(value) => Kind::BoolValue(value),
        Value::U8(value) => Kind::U32Value(value.into()),
        Value::U16(value) => Kind::U32Value(value.into()),
        Value::U32(value) => Kind::U32Value(value),
        Value::U64(value) => Kind::U64Value(value),
        Value::I8(value) => Kind::I32Value(value.into()),
        Value::I16(value) => Kind::I32Value(value.into()),
        Value::I32(value) => Kind::I32Value(value),
        Value::I64(value) => Kind::I64Value(value),
        Value::F32(value) => Kind::F32Bits(value.to_bits()),
        Value::F64(value) => Kind::F64Bits(value.to_bits()),
        Value::Char(value) => Kind::CharValue(value.into()),
        Value::String(value) => Kind::StringValue(value),
        Value::Unit => Kind::Unit(protobuf::Unit {}),
        Value::Option(value) => Kind::OptionValue(Box::new(protobuf::OptionalValue { value: value.map(|value| protobuf_value(*value)).transpose()?.map(Box::new) })),
        Value::Newtype(value) => Kind::NewtypeValue(Box::new(protobuf_value(*value)?)),
        Value::Seq(values) if values.iter().all(|value| matches!(value, Value::U8(_))) => Kind::BytesValue(
            values
                .into_iter()
                .map(|value| match value {
                    Value::U8(value) => Ok(value),
                    _ => Err(WireError::Encode("byte sequence changed while encoding".to_owned())),
                })
                .collect::<Result<_, _>>()?,
        ),
        Value::Seq(values) => Kind::Sequence(protobuf::Sequence { values: values.into_iter().map(protobuf_value).collect::<Result<_, _>>()? }),
        Value::Map(values) => Kind::Map(protobuf::Map { entries: values.into_iter().map(|(key, value)| Ok(protobuf::MapEntry { key: Some(protobuf_value(key)?), value: Some(protobuf_value(value)?) })).collect::<Result<_, WireError>>()? }),
        Value::Bytes(value) => Kind::BytesValue(value),
    };
    Ok(protobuf::Value { kind: Some(kind) })
}

fn domain_value(value: protobuf::Value, byte_fields: bool) -> Result<serde_value::Value, WireError> {
    use protobuf::value::Kind;
    use serde_value::Value;

    Ok(match value.kind.ok_or(WireError::InvalidHeader)? {
        Kind::BoolValue(value) => Value::Bool(value),
        Kind::U32Value(value) => Value::U32(value),
        Kind::U64Value(value) => Value::U64(value),
        Kind::I32Value(value) => Value::I32(value),
        Kind::I64Value(value) => Value::I64(value),
        Kind::F32Bits(value) => Value::F32(f32::from_bits(value)),
        Kind::F64Bits(value) => Value::F64(f64::from_bits(value)),
        Kind::CharValue(value) => Value::Char(char::from_u32(value).ok_or_else(|| WireError::Decode(format!("invalid protobuf character value {value}")))?),
        Kind::StringValue(value) => Value::String(value),
        Kind::Unit(_) => Value::Unit,
        Kind::OptionValue(value) => Value::Option(value.value.map(|value| domain_value(*value, byte_fields).map(Box::new)).transpose()?),
        Kind::NewtypeValue(value) => Value::Newtype(Box::new(domain_value(*value, byte_fields)?)),
        Kind::Sequence(value) => Value::Seq(value.values.into_iter().map(|value| domain_value(value, byte_fields)).collect::<Result<_, _>>()?),
        Kind::Map(value) => Value::Map(value.entries.into_iter().map(|entry| Ok((domain_value(entry.key.ok_or(WireError::InvalidHeader)?, byte_fields)?, domain_value(entry.value.ok_or(WireError::InvalidHeader)?, byte_fields)?))).collect::<Result<_, WireError>>()?),
        // The established encoder also represents empty sequences as bytes.
        // Keep those deserializable as lists (e.g. no rejections or mutations).
        Kind::BytesValue(value) if byte_fields && !value.is_empty() => Value::Bytes(value),
        Kind::BytesValue(value) => Value::Seq(value.into_iter().map(Value::U8).collect()),
    })
}

pub mod protobuf {
    // Generated from proto/bokheim_wire.proto by prost-build 0.14.4.
    include!("bokheim.wire.v16.rs");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Example {
        name: String,
        values: Vec<u64>,
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Blob {
        bytes: Vec<u8>,
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct CompactBlob {
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    enum NumericVariant {
        First,
        Second(u64),
    }

    fn example(count: usize) -> Example {
        Example { name: "Bokheim".to_owned(), values: (0..count as u64).collect() }
    }

    #[test]
    fn version_and_decoded_size_are_bounded() {
        assert_eq!(version(), 31);
        let mut unsupported = encode(&example(1)).unwrap();
        unsupported[1] = (VERSION + 1) as u8;
        assert!(matches!(decode::<Example>(&unsupported, MAX_DECODED_REQUEST_BYTES), Err(WireError::UnsupportedVersion(version)) if version == VERSION + 1));
        assert_eq!(decode::<Example>(&encode(&example(1_000)).unwrap(), 32), Err(WireError::TooLarge { limit: 32 }));
    }

    #[test]
    fn retained_state_supports_previous_version_without_downgrading_http() {
        let mut historical = encode(&example(1)).unwrap();
        historical[1] = 28;
        assert!(matches!(decode::<Example>(&historical, MAX_DECODED_REQUEST_BYTES), Err(WireError::UnsupportedVersion(28))));
        assert_eq!(decode_state::<Example>(&historical, MAX_DECODED_REQUEST_BYTES).unwrap(), example(1));
        historical[1] = (VERSION + 1) as u8;
        assert!(matches!(decode_state::<Example>(&historical, MAX_DECODED_REQUEST_BYTES), Err(WireError::UnsupportedVersion(version)) if version == VERSION + 1));
    }

    #[test]
    fn decoder_rejects_malformed_data() {
        assert!(matches!(decode::<NumericVariant>(&[0xff], 64), Err(WireError::Decode(_))));
        let mut trailing = encode(&NumericVariant::First).unwrap();
        trailing.push(0);
        assert!(matches!(decode::<NumericVariant>(&trailing, 64), Err(WireError::Decode(_))));
    }

    #[test]
    fn byte_vectors_use_protobuf_bytes_without_per_byte_messages() {
        let blob = Blob { bytes: vec![0xab; 1024 * 1024] };
        let encoded = encode(&blob).unwrap();
        assert!(encoded.len() < blob.bytes.len() + 128);
        assert_eq!(decode::<Blob>(&encoded, 2 * 1024 * 1024).unwrap(), blob);
    }

    #[test]
    fn compact_bytes_preserve_wire_format_and_both_decoders() {
        for bytes in [Vec::new(), (0..=255).collect(), vec![0xab; 1024 * 1024]] {
            let legacy = Blob { bytes: bytes.clone() };
            let compact = CompactBlob { bytes };
            let encoded = encode(&compact).unwrap();
            assert_eq!(encoded, encode(&legacy).unwrap());
            assert_eq!(decode::<Blob>(&encoded, MAX_DECODED_RESPONSE_BYTES).unwrap(), legacy);
            assert_eq!(decode::<CompactBlob>(&encoded, MAX_DECODED_RESPONSE_BYTES).unwrap(), compact);
            assert_eq!(decode_byte_fields::<CompactBlob>(&encoded, MAX_DECODED_RESPONSE_BYTES).unwrap(), compact);
            let nested = vec![compact];
            assert_eq!(decode_byte_fields::<Vec<CompactBlob>>(&encode(&nested).unwrap(), MAX_DECODED_RESPONSE_BYTES).unwrap(), nested);
        }
    }

    #[test]
    fn compact_byte_fields_stay_buffers_in_intermediate_trees() {
        use serde_value::Value;

        let blob = CompactBlob { bytes: vec![0xab; 1024] };
        let Value::Map(fields) = value_serializer::to_value(&blob).unwrap() else { panic!("expected fields") };
        assert_eq!(fields.get(&Value::String("bytes".into())), Some(&Value::Bytes(blob.bytes.clone())));
        let encoded = protobuf_value(Value::Bytes(blob.bytes.clone())).unwrap();
        assert_eq!(domain_value(encoded, true).unwrap(), Value::Bytes(blob.bytes));
    }

    #[test]
    fn compact_decoder_preserves_version_size_and_canonical_checks() {
        let encoded = encode(&CompactBlob { bytes: vec![1, 2, 3] }).unwrap();
        assert_eq!(decode_byte_fields::<CompactBlob>(&encoded, encoded.len() - 1), Err(WireError::TooLarge { limit: encoded.len() - 1 }));
        let mut historical = encoded.clone();
        historical[1] = 28;
        assert_eq!(decode_byte_fields::<CompactBlob>(&historical, MAX_DECODED_RESPONSE_BYTES), Err(WireError::UnsupportedVersion(28)));
        let mut duplicate_version = encoded;
        duplicate_version.extend_from_slice(&[8, VERSION as u8]);
        assert!(matches!(decode_byte_fields::<CompactBlob>(&duplicate_version, MAX_DECODED_RESPONSE_BYTES), Err(WireError::Decode(_))));
    }

    #[test]
    fn compact_decoder_reads_empty_non_byte_sequences() {
        let empty = example(0);
        assert_eq!(decode_byte_fields::<Example>(&encode(&empty).unwrap(), 4096).unwrap(), empty);
        let blobs: Vec<CompactBlob> = Vec::new();
        assert_eq!(decode_byte_fields::<Vec<CompactBlob>>(&encode(&blobs).unwrap(), 4096).unwrap(), blobs);
    }
}
