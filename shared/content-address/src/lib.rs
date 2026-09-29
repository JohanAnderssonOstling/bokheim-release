//! Content-addressed identities shared by storage models and transfer protocols.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; 64]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidContentHash;

impl fmt::Display for InvalidContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("content hash must contain exactly 64 lowercase hexadecimal characters")
    }
}

impl std::error::Error for InvalidContentHash {}

impl ContentHash {
    /// Constructs a content hash from trusted, canonical hash text.
    ///
    /// Untrusted input must use [`FromStr`] or Serde deserialization so an
    /// invalid value is returned as an error rather than causing a panic.
    pub fn new(value: &str) -> Self {
        value.parse().expect("ContentHash::new requires exactly 64 lowercase hexadecimal characters")
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("ContentHash is generated as ASCII hexadecimal text")
    }
}

impl FromStr for ContentHash {
    type Err = InvalidContentHash;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')) {
            return Err(InvalidContentHash);
        }
        let mut bytes = [0_u8; 64];
        bytes.copy_from_slice(value.as_bytes());
        Ok(Self(bytes))
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}

impl serde::Serialize for ContentHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <String as serde::Deserialize>::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_the_canonical_hash_text() {
        let value = "a".repeat(64);
        let hash = ContentHash::new(&value);
        assert_eq!(hash.as_str(), value);
        assert_eq!(hash.to_string(), value);
    }

    #[test]
    fn rejects_noncanonical_and_path_like_hashes() {
        for value in ["a".repeat(63), "a".repeat(65), "A".repeat(64), format!("/{}", "a".repeat(63)), format!("../{}", "a".repeat(61)), format!("é{}", "a".repeat(62))] {
            assert!(value.parse::<ContentHash>().is_err(), "accepted invalid content hash {value:?}");
        }
    }

    #[test]
    fn serde_rejects_invalid_hashes_without_panicking() {
        let input = serde::de::value::StrDeserializer::<serde::de::value::Error>::new("short");
        let result = <ContentHash as serde::Deserialize>::deserialize(input);
        assert!(result.is_err());
    }
}
