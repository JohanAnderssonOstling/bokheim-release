//! Versioned bincode codec for persisted values only.
//!
//! HTTP and WebSocket traffic uses the separate `protobuf-wire` crate.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt;

pub const MAX_DECODED_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DECODED_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

const MAGIC: &[u8; 4] = b"BKHM";
const VERSION: u8 = 13;
const HEADER_BYTES: usize = MAGIC.len() + 1;

/// The magic+version header bytes prefixing every persisted value.
pub const fn header_bytes() -> [u8; HEADER_BYTES] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION]
}

#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    Encode(String),
    Decode(String),
    InvalidHeader,
    UnsupportedVersion(u8),
    TooLarge { limit: usize },
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(reason) => write!(formatter, "cannot encode Bokheim storage value: {reason}"),
            Self::Decode(reason) => write!(formatter, "cannot decode Bokheim storage value: {reason}"),
            Self::InvalidHeader => formatter.write_str("Bokheim storage value has an invalid header"),
            Self::UnsupportedVersion(version) => write!(formatter, "Bokheim storage version {version} is unsupported"),
            Self::TooLarge { limit } => write!(formatter, "decoded Bokheim storage value exceeds the {limit}-byte limit"),
        }
    }
}

impl std::error::Error for WireError {}

pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, WireError> {
    let payload = bincode::serde::encode_to_vec(value, bincode::config::standard()).map_err(|error| WireError::Encode(error.to_string()))?;
    let mut body = Vec::with_capacity(HEADER_BYTES + payload.len());
    body.extend_from_slice(MAGIC);
    body.push(VERSION);
    body.extend_from_slice(&payload);
    Ok(body)
}

pub fn decode<T: DeserializeOwned>(body: &[u8], decoded_limit: usize) -> Result<T, WireError> {
    if body.len() > decoded_limit {
        return Err(WireError::TooLarge { limit: decoded_limit });
    }
    if body.len() < HEADER_BYTES || &body[..MAGIC.len()] != MAGIC {
        return Err(WireError::InvalidHeader);
    }
    if body[MAGIC.len()] != VERSION {
        return Err(WireError::UnsupportedVersion(body[MAGIC.len()]));
    }
    let (value, consumed) = bincode::serde::decode_from_slice(&body[HEADER_BYTES..], bincode::config::standard()).map_err(|error| WireError::Decode(error.to_string()))?;
    if consumed != body.len() - HEADER_BYTES {
        return Err(WireError::Decode("trailing bytes after Bokheim storage value".to_owned()));
    }
    Ok(value)
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
    enum NumericVariant {
        First,
        Second(u64),
    }

    fn example(count: usize) -> Example {
        Example { name: "Bokheim".to_owned(), values: (0..count as u64).collect() }
    }

    #[test]
    fn bincode_round_trip_uses_the_versioned_storage_header() {
        let encoded = encode(&example(8)).unwrap();
        assert_eq!(encoded, b"BKHM\x0d\x07Bokheim\x08\x00\x01\x02\x03\x04\x05\x06\x07");
        assert_eq!(decode::<Example>(&encoded, MAX_DECODED_REQUEST_BYTES).unwrap(), example(8));
        assert_eq!(encode(&NumericVariant::Second(42)).unwrap(), b"BKHM\x0d\x01\x2a");
    }

    #[test]
    fn version_size_and_trailing_data_are_rejected() {
        let mut unsupported = encode(&example(1)).unwrap();
        unsupported[MAGIC.len()] = VERSION + 1;
        assert!(matches!(decode::<Example>(&unsupported, MAX_DECODED_REQUEST_BYTES), Err(WireError::UnsupportedVersion(version)) if version == VERSION + 1));
        assert_eq!(decode::<Example>(&encode(&example(1_000)).unwrap(), 32), Err(WireError::TooLarge { limit: 32 }));
        let mut trailing = encode(&NumericVariant::First).unwrap();
        trailing.push(0);
        assert!(matches!(decode::<NumericVariant>(&trailing, 64), Err(WireError::Decode(_))));
    }
}
