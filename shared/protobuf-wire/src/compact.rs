//! Opt-in typed byte payloads inside the shared transport. JSON and persisted
//! serializers keep the domain representation; readers also accept that older
//! representation. Codecs only define the packed bytes for their own values.
use serde::de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, marker::PhantomData};

const TRANSPORT_BYTES: &str = "BokheimTransportBytes";

pub trait Codec {
    type Value: Serialize + DeserializeOwned;
    fn pack(value: &Self::Value) -> Result<Vec<u8>, String>;
    fn unpack(bytes: &[u8]) -> Result<Self::Value, String>;
}

pub fn serialize<C: Codec, S: Serializer>(value: &C::Value, serializer: S) -> Result<S::Ok, S::Error> {
    Transport::<C>(value).serialize(serializer)
}

pub fn deserialize<'de, C: Codec, D: Deserializer<'de>>(deserializer: D) -> Result<C::Value, D::Error> {
    Decoded::<C>::deserialize(deserializer).map(|value| value.0)
}

pub mod optional {
    use super::*;
    pub fn serialize<C: Codec, S: Serializer>(value: &Option<C::Value>, serializer: S) -> Result<S::Ok, S::Error> {
        value.as_ref().map(Transport::<C>).serialize(serializer)
    }
    pub fn deserialize<'de, C: Codec, D: Deserializer<'de>>(deserializer: D) -> Result<Option<C::Value>, D::Error> {
        Option::<Decoded<C>>::deserialize(deserializer).map(|value| value.map(|value| value.0))
    }
}

struct Transport<'a, C: Codec>(&'a C::Value);
struct Payload<'a, C: Codec>(&'a C::Value);
impl<C: Codec> Serialize for Transport<'_, C> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_newtype_struct(TRANSPORT_BYTES, &Payload::<C>(self.0))
        } else {
            self.0.serialize(serializer)
        }
    }
}
impl<C: Codec> Serialize for Payload<'_, C> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            self.0.serialize(serializer)
        } else {
            serializer.serialize_bytes(&C::pack(self.0).map_err(serde::ser::Error::custom)?)
        }
    }
}

struct Decoded<C: Codec>(C::Value);
impl<'de, C: Codec> Deserialize<'de> for Decoded<C> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if !deserializer.is_human_readable() {
            return C::Value::deserialize(deserializer).map(Self);
        }
        struct PackedVisitor<C>(PhantomData<C>);
        impl<'de, C: Codec> Visitor<'de> for PackedVisitor<C> {
            type Value = Decoded<C>;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("compact transport bytes or the previous domain representation")
            }
            fn visit_newtype_struct<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
                deserializer.deserialize_any(self)
            }
            fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
                C::unpack(bytes).map(Decoded).map_err(E::custom)
            }
            fn visit_byte_buf<E: de::Error>(self, bytes: Vec<u8>) -> Result<Self::Value, E> {
                self.visit_bytes(&bytes)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, sequence: A) -> Result<Self::Value, A::Error> {
                C::Value::deserialize(de::value::SeqAccessDeserializer::new(sequence)).map(Decoded)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                C::Value::deserialize(de::value::MapAccessDeserializer::new(map)).map(Decoded)
            }
        }
        deserializer.deserialize_newtype_struct(TRANSPORT_BYTES, PackedVisitor(PhantomData))
    }
}
