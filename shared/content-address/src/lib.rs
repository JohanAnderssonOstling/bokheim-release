//! Content-addressed identities shared by storage models and transfer protocols.

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::str::FromStr;

mod book_id;
pub use book_id::{BookId, InvalidBookId};

/// The raw 32-byte BLAKE3 digest. Hexadecimal is only a text representation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidContentHash;

impl fmt::Display for InvalidContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("content hash requires 32 bytes or 64 lowercase hexadecimal characters")
    }
}
impl std::error::Error for InvalidContentHash {}

impl ContentHash {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn to_hex(&self) -> String {
        blake3::Hash::from(self.0).to_hex().to_string()
    }

    /// Constructs a hash from trusted text at a text boundary.
    pub fn new(value: &str) -> Self {
        value.parse().expect("ContentHash::new requires 64 lowercase hexadecimal characters")
    }
}
impl From<blake3::Hash> for ContentHash {
    fn from(value: blake3::Hash) -> Self {
        Self(*value.as_bytes())
    }
}
impl From<[u8; 32]> for ContentHash {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}
impl TryFrom<&[u8]> for ContentHash {
    type Error = InvalidContentHash;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let bytes: [u8; 32] = value.try_into().map_err(|_| InvalidContentHash)?;
        Ok(Self(bytes))
    }
}
impl FromStr for ContentHash {
    type Err = InvalidContentHash;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        fn digit(byte: u8) -> Result<u8, InvalidContentHash> {
            match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(InvalidContentHash),
            }
        }
        if value.len() != 64 {
            return Err(InvalidContentHash);
        }
        let mut bytes = [0; 32];
        for (output, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            *output = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        Ok(Self(bytes))
    }
}
impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        blake3::Hash::from(self.0).to_hex().fmt(formatter)
    }
}
impl fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}
impl serde::Serialize for ContentHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Encoding<'a>(&'a ContentHash);
        impl serde::Serialize for Encoding<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                if serializer.is_human_readable() { serializer.collect_str(self.0) } else { serializer.serialize_bytes(self.0.as_bytes()) }
            }
        }
        serializer.serialize_newtype_struct("ContentHash", &Encoding(self))
    }
}
impl<'de> serde::Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HashVisitor;
        impl<'de> serde::de::Visitor<'de> for HashVisitor {
            type Value = ContentHash;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a 32-byte hash or lowercase hexadecimal hash text")
            }
            fn visit_newtype_struct<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
                <ContentHash as serde::Deserialize>::deserialize(deserializer)
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse().map_err(E::custom)
            }
            fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
                ContentHash::try_from(value).map_err(E::custom)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
                let mut bytes = [0; 32];
                for byte in &mut bytes {
                    *byte = sequence.next_element()?.ok_or_else(|| serde::de::Error::custom("content hash needs 32 bytes"))?;
                }
                if sequence.next_element::<u8>()?.is_some() {
                    return Err(serde::de::Error::custom("content hash needs exactly 32 bytes"));
                }
                Ok(ContentHash::from_bytes(bytes))
            }
            fn visit_byte_buf<E: serde::de::Error>(self, value: Vec<u8>) -> Result<Self::Value, E> {
                self.visit_bytes(&value)
            }
        }
        if deserializer.is_human_readable() { deserializer.deserialize_any(HashVisitor) } else { deserializer.deserialize_bytes(HashVisitor) }
    }
}

/// Hashes bytes from the reader's current position to EOF using BLAKE3.
pub fn hash_reader(reader: &mut impl Read) -> io::Result<ContentHash> {
    Ok(blake3::Hasher::new().update_reader(reader)?.finalize().into())
}

/// Hash the complete file, regardless of the reader's current position.
pub fn hash_file(reader: &mut (impl Read + Seek)) -> io::Result<ContentHash> {
    reader.seek(SeekFrom::Start(0))?;
    hash_reader(reader)
}

#[cfg(test)]
mod tests;

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::ContentHash;
    use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

    impl ToSql for ContentHash {
        fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
            Ok(ToSqlOutput::Borrowed(ValueRef::Blob(self.as_bytes())))
        }
    }
    impl FromSql for ContentHash {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            Self::try_from(value.as_blob()?).map_err(|error| FromSqlError::Other(Box::new(error)))
        }
    }
}
