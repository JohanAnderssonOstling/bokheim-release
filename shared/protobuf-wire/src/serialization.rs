//! One Serde traversal for protobuf values, exact lengths and representations.
//! Backends choose storage; adapters, compound shapes and map rules are shared.
use crate::protobuf::value::Kind;
use crate::value_serializer;
use serde::ser::{self, Serialize};
use serde_value::{SerializerError, Value as Key};
use std::collections::BTreeMap;
use std::marker::PhantomData;

#[derive(Clone, Copy)]
pub(crate) enum Scalar {
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Char(char),
}
impl Scalar {
    #[inline]
    pub(crate) fn byte(self) -> Option<u8> {
        if let Self::U8(value) = self {
            Some(value)
        } else {
            None
        }
    }
    #[inline]
    pub(crate) fn kind(self) -> Kind {
        match self {
            Self::Bool(value) => Kind::BoolValue(value),
            Self::U8(value) => Kind::U32Value(value.into()),
            Self::U16(value) => Kind::U32Value(value.into()),
            Self::U32(value) => Kind::U32Value(value),
            Self::U64(value) => Kind::U64Value(value),
            Self::I8(value) => Kind::I32Value(value.into()),
            Self::I16(value) => Kind::I32Value(value.into()),
            Self::I32(value) => Kind::I32Value(value),
            Self::I64(value) => Kind::I64Value(value),
            Self::F32(value) => Kind::F32Bits(value.to_bits()),
            Self::F64(value) => Kind::F64Bits(value.to_bits()),
            Self::Char(value) => Kind::CharValue(value.into()),
        }
    }
}

pub(crate) trait SequenceOutput<T> {
    fn push(&mut self, value: T) -> Result<(), SerializerError>;
    fn finish(self) -> Result<T, SerializerError>;
}

pub(crate) trait Backend: Sized {
    type Output;
    type Sequence: SequenceOutput<Self::Output>;
    type FieldKey: Ord;
    fn scalar(value: Scalar) -> Self::Output;
    fn text(value: &str) -> Result<Self::Output, SerializerError>;
    fn bytes(value: &[u8]) -> Result<Self::Output, SerializerError>;
    fn unit() -> Result<Self::Output, SerializerError>;
    fn optional(value: Option<Self::Output>) -> Result<Self::Output, SerializerError>;
    fn newtype(value: Self::Output) -> Result<Self::Output, SerializerError>;
    fn sequence(capacity: usize) -> Self::Sequence;
    fn map(values: BTreeMap<Key, Self::Output>) -> Result<Self::Output, SerializerError>;
    fn field_key(name: &'static str) -> Self::FieldKey;
    fn fields(values: BTreeMap<Self::FieldKey, Self::Output>) -> Result<Self::Output, SerializerError>;
    fn variant(name: &str, value: Self::Output) -> Result<Self::Output, SerializerError> {
        Self::map(BTreeMap::from([(Key::String(name.to_owned()), value)]))
    }
    fn finish_variant(name: Option<&str>, value: Self::Output) -> Result<Self::Output, SerializerError> {
        match name {
            Some(name) => Self::variant(name, value),
            None => Ok(value),
        }
    }
}

pub(crate) struct Serializer<B> {
    binary: bool,
    backend: PhantomData<B>,
}
impl<B> Serializer<B> {
    pub(crate) fn new(binary: bool) -> Self {
        Self { binary, backend: PhantomData }
    }
}
pub(crate) struct Sequence<B: Backend> {
    output: B::Sequence,
    variant: Option<&'static str>,
    binary: bool,
}
pub(crate) struct Mapping<B: Backend> {
    values: BTreeMap<Key, B::Output>,
    key: Option<Key>,
    binary: bool,
}
pub(crate) struct Structure<B: Backend> {
    values: BTreeMap<B::FieldKey, B::Output>,
    variant: Option<&'static str>,
    binary: bool,
}

macro_rules! scalar { ($($method:ident($ty:ty) => $variant:ident),*) => { $(fn $method(self, value: $ty) -> Result<B::Output, SerializerError> { Ok(B::scalar(Scalar::$variant(value))) })* }; }
impl<B: Backend> ser::Serializer for Serializer<B> {
    type Ok = B::Output;
    type Error = SerializerError;
    type SerializeSeq = Sequence<B>;
    type SerializeTuple = Sequence<B>;
    type SerializeTupleStruct = Sequence<B>;
    type SerializeTupleVariant = Sequence<B>;
    type SerializeMap = Mapping<B>;
    type SerializeStruct = Structure<B>;
    type SerializeStructVariant = Structure<B>;
    scalar!(serialize_bool(bool)=>Bool, serialize_i8(i8)=>I8, serialize_i16(i16)=>I16, serialize_i32(i32)=>I32, serialize_i64(i64)=>I64,
        serialize_u8(u8)=>U8, serialize_u16(u16)=>U16, serialize_u32(u32)=>U32, serialize_u64(u64)=>U64, serialize_f32(f32)=>F32, serialize_f64(f64)=>F64, serialize_char(char)=>Char);
    fn is_human_readable(&self) -> bool {
        !self.binary
    }
    fn serialize_str(self, value: &str) -> Result<B::Output, SerializerError> {
        B::text(value)
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<B::Output, SerializerError> {
        B::bytes(value)
    }
    fn serialize_none(self) -> Result<B::Output, SerializerError> {
        B::optional(None)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<B::Output, SerializerError> {
        B::optional(Some(value.serialize(self)?))
    }
    fn serialize_unit(self) -> Result<B::Output, SerializerError> {
        B::unit()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<B::Output, SerializerError> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, variant: &'static str) -> Result<B::Output, SerializerError> {
        B::text(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, name: &'static str, value: &T) -> Result<B::Output, SerializerError> {
        // Opt-in transport payloads choose compact bytes in binary mode while
        // JSON serializers keep their ordinary, human-readable representation.
        // All three backends (encoding, counting, representation) share this.
        if matches!(name, "ContentHash" | "BookId" | "BokheimTransportBytes") {
            value.serialize(Self::new(true))
        } else {
            B::newtype(value.serialize(self)?)
        }
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, _: u32, variant: &'static str, value: &T) -> Result<B::Output, SerializerError> {
        B::variant(variant, value.serialize(self)?)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Sequence<B>, SerializerError> {
        Ok(Sequence { output: B::sequence(len.unwrap_or_default()), variant: None, binary: self.binary })
    }
    fn serialize_tuple(self, len: usize) -> Result<Sequence<B>, SerializerError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<Sequence<B>, SerializerError> {
        self.serialize_tuple(len)
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, variant: &'static str, len: usize) -> Result<Sequence<B>, SerializerError> {
        Ok(Sequence { variant: Some(variant), ..self.serialize_tuple(len)? })
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Mapping<B>, SerializerError> {
        Ok(Mapping { values: BTreeMap::new(), key: None, binary: self.binary })
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Structure<B>, SerializerError> {
        Ok(Structure { values: BTreeMap::new(), variant: None, binary: self.binary })
    }
    fn serialize_struct_variant(self, name: &'static str, _: u32, variant: &'static str, len: usize) -> Result<Structure<B>, SerializerError> {
        Ok(Structure { variant: Some(variant), ..self.serialize_struct(name, len)? })
    }
}
impl<B: Backend> Sequence<B> {
    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializerError> {
        self.output.push(value.serialize(Serializer::<B>::new(self.binary))?)
    }
    fn finish(self) -> Result<B::Output, SerializerError> {
        B::finish_variant(self.variant, self.output.finish()?)
    }
}
macro_rules! sequence {
    ($trait:ident,$method:ident) => {
        impl<B: Backend> ser::$trait for Sequence<B> {
            type Ok = B::Output;
            type Error = SerializerError;
            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializerError> {
                self.push(value)
            }
            fn end(self) -> Result<B::Output, SerializerError> {
                self.finish()
            }
        }
    };
}
sequence!(SerializeSeq, serialize_element);
sequence!(SerializeTuple, serialize_element);
sequence!(SerializeTupleStruct, serialize_field);
sequence!(SerializeTupleVariant, serialize_field);
impl<B: Backend> Structure<B> {
    fn field<T: Serialize + ?Sized>(&mut self, name: &'static str, value: &T) -> Result<(), SerializerError> {
        self.values.insert(B::field_key(name), value.serialize(Serializer::<B>::new(self.binary))?);
        Ok(())
    }
    fn finish(self) -> Result<B::Output, SerializerError> {
        B::finish_variant(self.variant, B::fields(self.values)?)
    }
}
impl<B: Backend> ser::SerializeMap for Mapping<B> {
    type Ok = B::Output;
    type Error = SerializerError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializerError> {
        self.key = Some(value_serializer::to_value_with_mode(value, self.binary)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerializerError> {
        let key = self.key.take().ok_or_else(|| <SerializerError as ser::Error>::custom("map value has no key"))?;
        self.values.insert(key, value.serialize(Serializer::<B>::new(self.binary))?);
        Ok(())
    }
    fn end(self) -> Result<B::Output, SerializerError> {
        B::map(self.values)
    }
}
macro_rules! structure {
    ($trait:ident) => {
        impl<B: Backend> ser::$trait for Structure<B> {
            type Ok = B::Output;
            type Error = SerializerError;
            fn serialize_field<T: Serialize + ?Sized>(&mut self, name: &'static str, value: &T) -> Result<(), SerializerError> {
                self.field(name, value)
            }
            fn end(self) -> Result<B::Output, SerializerError> {
                self.finish()
            }
        }
    };
}
structure!(SerializeStruct);
structure!(SerializeStructVariant);
