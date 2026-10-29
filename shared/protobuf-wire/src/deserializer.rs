//! Deserialize the protobuf tree directly. Byte sequences stream u8 values;
//! arbitrary map keys alone retain the original Serde ordering and deduplication.
use crate::protobuf::{self, value::Kind, Value};
use crate::{domain_value, protobuf_value, WireError};
use serde::de::{self, DeserializeSeed, Deserializer as _, Visitor};
use serde_value::{DeserializerError as Error, Value as Key};
use std::collections::{btree_map, BTreeMap};
use std::vec;

pub(crate) fn validate(value: &Value) -> Result<(), WireError> {
    match value.kind.as_ref().ok_or(WireError::InvalidHeader)? {
        Kind::CharValue(value) if char::from_u32(*value).is_none() => return Err(WireError::Decode(format!("invalid protobuf character value {value}"))),
        Kind::OptionValue(value) => {
            if let Some(value) = &value.value {
                validate(value)?;
            }
        }
        Kind::NewtypeValue(value) => validate(value)?,
        Kind::Sequence(value) => {
            for value in &value.values {
                validate(value)?;
            }
        }
        Kind::Map(value) => {
            for entry in &value.entries {
                validate(entry.key.as_ref().ok_or(WireError::InvalidHeader)?)?;
                validate(entry.value.as_ref().ok_or(WireError::InvalidHeader)?)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) struct Deserializer {
    pub value: Value,
    pub byte_fields: bool,
}
fn error(message: &str) -> Error {
    de::Error::custom(message)
}
fn ordered_entries(value: protobuf::Map, byte_fields: bool) -> Result<BTreeMap<Key, Value>, Error> {
    value.entries.into_iter().map(|entry| Ok((domain_value(entry.key.ok_or_else(|| error("missing map key"))?, byte_fields).map_err(|failure| error(&failure.to_string()))?, entry.value.ok_or_else(|| error("missing map value"))?))).collect()
}

impl<'de> de::Deserializer<'de> for Deserializer {
    type Error = Error;
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let byte_fields = self.byte_fields;
        match self.value.kind.ok_or_else(|| error("missing protobuf value kind"))? {
            Kind::BoolValue(value) => visitor.visit_bool(value),
            Kind::U32Value(value) => visitor.visit_u32(value),
            Kind::U64Value(value) => visitor.visit_u64(value),
            Kind::I32Value(value) => visitor.visit_i32(value),
            Kind::I64Value(value) => visitor.visit_i64(value),
            Kind::F32Bits(value) => visitor.visit_f32(f32::from_bits(value)),
            Kind::F64Bits(value) => visitor.visit_f64(f64::from_bits(value)),
            Kind::CharValue(value) => visitor.visit_char(char::from_u32(value).ok_or_else(|| error("invalid protobuf character"))?),
            Kind::StringValue(value) => visitor.visit_string(value),
            Kind::Unit(_) => visitor.visit_unit(),
            Kind::OptionValue(value) => match value.value {
                Some(value) => visitor.visit_some(Self { value: *value, byte_fields }),
                None => visitor.visit_none(),
            },
            Kind::NewtypeValue(value) => visitor.visit_newtype_struct(Self { value: *value, byte_fields }),
            Kind::Sequence(value) => visitor.visit_seq(Sequence { values: value.values.into_iter(), byte_fields }),
            Kind::Map(value) => visitor.visit_map(Mapping { entries: MapEntries::new(value, byte_fields)?, pending: None, byte_fields }),
            Kind::BytesValue(value) if byte_fields && !value.is_empty() => visitor.visit_byte_buf(value),
            Kind::BytesValue(value) => visitor.visit_seq(Bytes(value.into_iter())),
        }
    }
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.value.kind {
            Some(Kind::OptionValue(_) | Kind::Unit(_)) => self.deserialize_any(visitor),
            _ => visitor.visit_some(self),
        }
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(self, name: &'static str, visitor: V) -> Result<V::Value, Error> {
        match self.value.kind {
            Some(Kind::BytesValue(value)) if name == "BokheimTransportBytes" => visitor.visit_newtype_struct(de::value::BytesDeserializer::<Error>::new(&value)),
            Some(Kind::NewtypeValue(value)) => visitor.visit_newtype_struct(Self { value: *value, byte_fields: self.byte_fields }),
            _ => visitor.visit_newtype_struct(self),
        }
    }
    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.value.kind {
            Some(Kind::BytesValue(value)) => visitor.visit_seq(Bytes(value.into_iter())),
            _ => self.deserialize_any(visitor),
        }
    }
    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(self, _: &'static str, _: usize, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }
    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        // The entire protobuf tree was validated before deserialization.
        // Unknown fields need no further traversal or payload copies.
        visitor.visit_unit()
    }
    fn deserialize_enum<V: Visitor<'de>>(self, _: &'static str, _: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        let byte_fields = self.byte_fields;
        let (variant, value) = match self.value.kind {
            Some(Kind::StringValue(value)) => (Key::String(value), None),
            Some(Kind::Map(mut value)) if value.entries.len() == 1 => {
                let entry = value.entries.pop().ok_or_else(|| error("enum requires a single-entry map"))?;
                let key = domain_value(entry.key.ok_or_else(|| error("missing map key"))?, byte_fields).map_err(|failure| error(&failure.to_string()))?;
                (key, Some(entry.value.ok_or_else(|| error("missing map value"))?))
            }
            Some(Kind::Map(value)) => {
                let mut entries = ordered_entries(value, byte_fields)?.into_iter();
                let (key, value) = entries.next().ok_or_else(|| error("enum requires a single-entry map"))?;
                if entries.next().is_some() {
                    return Err(error("enum requires a single-entry map"));
                }
                (key, Some(value))
            }
            _ => return Err(error("enum requires a string or map")),
        };
        visitor.visit_enum(Enumeration { variant, value, byte_fields })
    }
    serde::forward_to_deserialize_any! { bool u8 u16 u32 u64 i8 i16 i32 i64 f32 f64 char str string unit bytes byte_buf map unit_struct struct identifier }
}

struct Sequence {
    values: vec::IntoIter<Value>,
    byte_fields: bool,
}
impl<'de> de::SeqAccess<'de> for Sequence {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.values.next().map(|value| seed.deserialize(Deserializer { value, byte_fields: self.byte_fields })).transpose()
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.values.len())
    }
}
struct Bytes(vec::IntoIter<u8>);
impl<'de> de::SeqAccess<'de> for Bytes {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.0.next().map(|value| seed.deserialize(de::value::U8Deserializer::<Error>::new(value))).transpose()
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.0.len())
    }
}
enum MapEntries {
    Strings(vec::IntoIter<protobuf::MapEntry>),
    Ordered(btree_map::IntoIter<Key, Value>),
}
impl MapEntries {
    fn new(value: protobuf::Map, byte_fields: bool) -> Result<Self, Error> {
        // Struct fields and string maps emitted by our encoder are already
        // strictly ordered. Retain sorting and last-duplicate behavior for
        // other keys and for maps received in a different order.
        let sorted_strings = value
            .entries
            .iter()
            .try_fold(None, |previous, entry| {
                let Some(Kind::StringValue(key)) = entry.key.as_ref().and_then(|key| key.kind.as_ref()) else {
                    return Err(());
                };
                if previous.is_some_and(|previous| previous >= key) {
                    return Err(());
                }
                Ok(Some(key))
            })
            .is_ok();
        Ok(if sorted_strings { Self::Strings(value.entries.into_iter()) } else { Self::Ordered(ordered_entries(value, byte_fields)?.into_iter()) })
    }
    fn next(&mut self) -> Option<Result<(Value, Value), Error>> {
        match self {
            Self::Strings(entries) => entries.next().map(|entry| Ok((entry.key.ok_or_else(|| error("missing map key"))?, entry.value.ok_or_else(|| error("missing map value"))?))),
            Self::Ordered(entries) => entries.next().map(|(key, value)| Ok((protobuf_value(key).map_err(|failure| error(&failure.to_string()))?, value))),
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::Strings(entries) => entries.len(),
            Self::Ordered(entries) => entries.len(),
        }
    }
}
struct Mapping {
    entries: MapEntries,
    pending: Option<Value>,
    byte_fields: bool,
}
impl<'de> de::MapAccess<'de> for Mapping {
    type Error = Error;
    fn next_key_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>, Error> {
        self.entries
            .next()
            .map(|entry| {
                let (key, value) = entry?;
                self.pending = Some(value);
                // Map keys may be ordinary Vec<u8>, even when the value
                // contract uses compact byte fields. Stream those bytes.
                seed.deserialize(Deserializer { value: key, byte_fields: false })
            })
            .transpose()
    }
    fn next_value_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(Deserializer { value: self.pending.take().ok_or_else(|| error("map value has no key"))?, byte_fields: self.byte_fields })
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}
struct Enumeration {
    variant: Key,
    value: Option<Value>,
    byte_fields: bool,
}
impl<'de> de::EnumAccess<'de> for Enumeration {
    type Error = Error;
    type Variant = Variant;
    fn variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<(T::Value, Variant), Error> {
        Ok((seed.deserialize(self.variant)?, Variant { value: self.value, byte_fields: self.byte_fields }))
    }
}
struct Variant {
    value: Option<Value>,
    byte_fields: bool,
}
impl<'de> de::VariantAccess<'de> for Variant {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        match self.value {
            Some(value) => de::Deserialize::deserialize(Deserializer { value, byte_fields: self.byte_fields }),
            None => Ok(()),
        }
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(Deserializer { value: self.value.ok_or_else(|| error("missing newtype variant value"))?, byte_fields: self.byte_fields })
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        let value = self.value.ok_or_else(|| error("missing tuple variant value"))?;
        if !matches!(value.kind, Some(Kind::Sequence(_) | Kind::BytesValue(_))) {
            return Err(error("tuple variant requires a sequence"));
        }
        Deserializer { value, byte_fields: self.byte_fields }.deserialize_seq(visitor)
    }
    fn struct_variant<V: Visitor<'de>>(self, _: &'static [&'static str], visitor: V) -> Result<V::Value, Error> {
        let value = self.value.ok_or_else(|| error("missing struct variant value"))?;
        if !matches!(value.kind, Some(Kind::Map(_))) {
            return Err(error("struct variant requires a map"));
        }
        Deserializer { value, byte_fields: self.byte_fields }.deserialize_map(visitor)
    }
}
