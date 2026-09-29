//! Protobuf codec for HTTP and WebSocket transport only.
//!
//! Persisted BLOBs use the separate `wire` storage crate. Synchronization
//! values use the explicit `MutationValue` protobuf schema below.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt;

pub const MEDIA_TYPE: &str = "application/vnd.bokheim+protobuf; version=21";
pub const MEDIA_TYPE_BASE: &str = "application/vnd.bokheim+protobuf";
pub const MAX_DECODED_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DECODED_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

const VERSION: u32 = 21;

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

    let value = serde_value::to_value(value).map_err(|error| WireError::Encode(error.to_string()))?;
    let envelope = protobuf::Envelope { version: VERSION, value: Some(protobuf_value(value)?) };
    let mut body = Vec::with_capacity(envelope.encoded_len());
    envelope.encode(&mut body).map_err(|error| WireError::Encode(error.to_string()))?;
    Ok(body)
}

pub fn decode<T: DeserializeOwned>(body: &[u8], decoded_limit: usize) -> Result<T, WireError> {
    use prost::Message;

    if body.len() > decoded_limit {
        return Err(WireError::TooLarge { limit: decoded_limit });
    }
    let envelope = protobuf::Envelope::decode(body).map_err(|error| WireError::Decode(error.to_string()))?;
    if envelope.encode_to_vec() != body {
        return Err(WireError::Decode("non-canonical or trailing protobuf data".to_owned()));
    }
    if envelope.version != VERSION {
        return Err(WireError::UnsupportedVersion(envelope.version));
    }
    let value = domain_value(envelope.value.ok_or(WireError::InvalidHeader)?)?;
    T::deserialize(value).map_err(|error| WireError::Decode(error.to_string()))
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

fn domain_value(value: protobuf::Value) -> Result<serde_value::Value, WireError> {
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
        Kind::OptionValue(value) => Value::Option(value.value.map(|value| domain_value(*value).map(Box::new)).transpose()?),
        Kind::NewtypeValue(value) => Value::Newtype(Box::new(domain_value(*value)?)),
        Kind::Sequence(value) => Value::Seq(value.values.into_iter().map(domain_value).collect::<Result<_, _>>()?),
        Kind::Map(value) => Value::Map(value.entries.into_iter().map(|entry| Ok((domain_value(entry.key.ok_or(WireError::InvalidHeader)?)?, domain_value(entry.value.ok_or(WireError::InvalidHeader)?)?))).collect::<Result<_, WireError>>()?),
        Kind::BytesValue(value) => Value::Seq(value.into_iter().map(Value::U8).collect()),
    })
}

pub mod protobuf {
    // Generated from proto/bokheim_wire.proto by prost-build 0.14.4.
    include!("bokheim.wire.v16.rs");
}

/// Encodes one typed mutation value without the generic transport envelope.
pub fn encode_mutation_value(value: &protobuf::MutationValue) -> Result<Vec<u8>, WireError> {
    use prost::Message;
    let mut body = Vec::with_capacity(value.encoded_len());
    value.encode(&mut body).map_err(|error| WireError::Encode(error.to_string()))?;
    Ok(body)
}

/// Decodes one typed mutation value. Unknown protobuf fields remain safe for
/// forwarding because the sync service stores these bytes without decoding.
pub fn decode_mutation_value(body: &[u8], decoded_limit: usize) -> Result<protobuf::MutationValue, WireError> {
    use prost::Message;
    if body.len() > decoded_limit {
        return Err(WireError::TooLarge { limit: decoded_limit });
    }
    protobuf::MutationValue::decode(body).map_err(|error| WireError::Decode(error.to_string()))
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
    enum NumericVariant {
        First,
        Second(u64),
    }

    fn example(count: usize) -> Example {
        Example { name: "Bokheim".to_owned(), values: (0..count as u64).collect() }
    }

    #[test]
    fn protobuf_round_trip_uses_the_versioned_bokheim_envelope() {
        let encoded = encode(&example(8)).unwrap();
        assert_eq!(encoded.first(), Some(&0x08));
        assert_eq!(decode::<Example>(&encoded, MAX_DECODED_REQUEST_BYTES).unwrap(), example(8));

        let variant = encode(&NumericVariant::Second(42)).unwrap();
        assert_eq!(decode::<NumericVariant>(&variant, 64).unwrap(), NumericVariant::Second(42));
    }

    #[test]
    fn version_and_decoded_size_are_bounded() {
        assert_eq!(version(), 21);
        let mut unsupported = encode(&example(1)).unwrap();
        unsupported[1] = (VERSION + 1) as u8;
        assert!(matches!(decode::<Example>(&unsupported, MAX_DECODED_REQUEST_BYTES), Err(WireError::UnsupportedVersion(version)) if version == VERSION + 1));
        assert_eq!(decode::<Example>(&encode(&example(1_000)).unwrap(), 32), Err(WireError::TooLarge { limit: 32 }));
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
    fn media_type_requires_the_current_version() {
        assert!(is_current_media_type(MEDIA_TYPE));
        assert!(is_current_media_type("Application/Vnd.Bokheim+Protobuf; charset=binary; version=21"));
        assert!(!is_current_media_type("application/vnd.bokheim+protobuf; version=14"));
        assert!(!is_current_media_type(MEDIA_TYPE_BASE));
        assert!(!is_current_media_type("application/vnd.bokheim+protobuf; version=1"));
        assert!(!is_current_media_type("application/vnd.bokheim+protobuf; version=2"));
        assert!(!is_current_media_type("application/vnd.bokheim+protobuf; version=6"));
        assert!(!is_current_media_type("application/vnd.bokheim+protobuf; version=1; version=5"));
        assert!(!is_current_media_type("application/json; version=1"));
    }

    #[test]
    fn mutation_value_field_numbers_are_a_stable_compatibility_baseline() {
        use protobuf::mutation_value::Kind;
        let value = protobuf::MutationValue { kind: Some(Kind::DirectoryName("Shelf".to_owned())) };
        // field 1 (directory_name), wire type 2, followed by the UTF-8 value.
        assert_eq!(encode_mutation_value(&value).unwrap(), b"\x0a\x05Shelf");
        assert_eq!(decode_mutation_value(b"\x0a\x05Shelf", 64).unwrap(), value);
    }
}
