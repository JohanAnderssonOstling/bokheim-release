//! Typed book-record transport shared by synchronization and enrichment.
//! The existing model remains the JSON, storage, and legacy transport schema.
use crate::{BookDetails, BookMetadata, BookRecord, BookSubject, Contributor, ContributorId, Identifier, LanguageTag, MarcRelatorCode, Scheme, Scope};
use prost::Message;
use protobuf_wire::compact::{self, Codec};
use serde::{Deserializer, Serializer};

#[derive(Clone, PartialEq, Message)]
struct Record {
    #[prost(string, tag = "1")]
    title: String,
    #[prost(string, optional, tag = "2")]
    subtitle: Option<String>,
    #[prost(message, repeated, tag = "3")]
    contributors: Vec<Credit>,
    #[prost(message, optional, tag = "4")]
    book: Option<Metadata>,
    #[prost(string, tag = "5")]
    description: String,
}

#[derive(Clone, PartialEq, Message)]
struct Credit {
    #[prost(bytes = "vec", tag = "1")]
    id: Vec<u8>,
    #[prost(string, tag = "2")]
    name: String,
    #[prost(bytes = "vec", tag = "3")]
    role: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
struct Metadata {
    #[prost(message, repeated, tag = "1")]
    identifiers: Vec<Identity>,
    #[prost(string, repeated, tag = "2")]
    publishers: Vec<String>,
    #[prost(string, repeated, tag = "3")]
    languages: Vec<String>,
    #[prost(sint32, optional, tag = "4")]
    year: Option<i32>,
    #[prost(message, repeated, tag = "5")]
    subjects: Vec<Subject>,
}

#[derive(Clone, PartialEq, Message)]
struct Identity {
    #[prost(string, tag = "1")]
    value: String,
    // Canonical built-in labels avoid coupling this format to Rust enum order.
    #[prost(string, tag = "2")]
    scheme: String,
    #[prost(uint32, tag = "3")]
    scope: u32,
    // Preserve custom schemes exactly, including a label matching a built-in.
    #[prost(bool, tag = "4")]
    custom_scheme: bool,
}

#[derive(Clone, PartialEq, Message)]
struct Subject {
    #[prost(string, optional, tag = "1")]
    id: Option<String>,
    #[prost(string, tag = "2")]
    name: String,
    #[prost(string, optional, tag = "3")]
    sort_name: Option<String>,
    #[prost(string, tag = "4")]
    source: String,
    #[prost(string, optional, tag = "5")]
    authority: Option<String>,
    #[prost(string, optional, tag = "6")]
    code: Option<String>,
}

struct Book;
impl Codec for Book {
    type Value = BookRecord;

    fn pack(value: &BookRecord) -> Result<Vec<u8>, String> {
        let record = Record {
            title: value.title.clone(),
            subtitle: value.subtitle.clone(),
            contributors: value.contributors.iter().map(|credit| Credit { id: credit.contributor_id().as_bytes().to_vec(), name: credit.name().into(), role: credit.role_code().0.to_vec() }).collect(),
            book: Some(Metadata {
                identifiers: value
                    .book
                    .identifiers
                    .iter()
                    .map(|id| Identity {
                        value: id.value.clone(),
                        scheme: id.scheme.label().unwrap_or_default().into(),
                        scope: match id.scope {
                            Scope::Book => 0,
                            Scope::Edition => 1,
                            Scope::Work => 2,
                        },
                        custom_scheme: matches!(id.scheme, Scheme::Other(_)),
                    })
                    .collect(),
                publishers: value.book.publishers.clone(),
                languages: value.book.languages.iter().map(|language| language.as_str().into()).collect(),
                year: value.book.year,
                subjects: value
                    .book
                    .subjects
                    .iter()
                    .map(|subject| Subject { id: subject.id.clone(), name: subject.name.clone(), sort_name: subject.sort_name.clone(), source: subject.source.clone(), authority: subject.authority.clone(), code: subject.code.clone() })
                    .collect(),
            }),
            description: value.description.clone(),
        };
        if record.encoded_len() > protobuf_wire::MAX_DECODED_REQUEST_BYTES {
            return Err("book record exceeds its transport limit".into());
        }
        Ok(record.encode_to_vec())
    }

    fn unpack(bytes: &[u8]) -> Result<BookRecord, String> {
        if bytes.len() > protobuf_wire::MAX_DECODED_REQUEST_BYTES {
            return Err("book record exceeds its transport limit".into());
        }
        let record = Record::decode(bytes).map_err(|error| error.to_string())?;
        let book = record.book.ok_or("missing bibliographic metadata")?;
        let contributors = record
            .contributors
            .into_iter()
            .map(|credit| {
                let id = ContributorId::from_slice(&credit.id).map_err(|error| error.to_string())?;
                let role = MarcRelatorCode(credit.role.try_into().map_err(|_| "invalid MARC relator length")?);
                Contributor::with_id(id, credit.name, role).map_err(|error| error.to_string())
            })
            .collect::<Result<_, String>>()?;
        let identifiers = book
            .identifiers
            .into_iter()
            .map(|id| {
                let scheme = if id.custom_scheme {
                    Scheme::Other(id.scheme)
                } else {
                    let scheme = Scheme::from_label(Some(&id.scheme));
                    if matches!(scheme, Scheme::Other(_)) || scheme.label().unwrap_or_default() != id.scheme {
                        return Err("invalid built-in identifier scheme".into());
                    }
                    scheme
                };
                let scope = match id.scope {
                    0 => Scope::Book,
                    1 => Scope::Edition,
                    2 => Scope::Work,
                    _ => return Err("invalid identifier scope".into()),
                };
                Identifier::new(id.value, scheme, scope).map_err(|error| error.to_string())
            })
            .collect::<Result<_, String>>()?;
        let languages = book.languages.into_iter().map(|language| LanguageTag::parse(language).map_err(|error| error.to_string())).collect::<Result<_, _>>()?;
        let subjects =
            book.subjects.into_iter().map(|subject| BookSubject::with_details(subject.id, subject.name, subject.sort_name, subject.source, subject.authority, subject.code).map_err(|error| error.to_string())).collect::<Result<_, _>>()?;
        Ok(BookRecord {
            metadata: BookDetails { title: record.title, subtitle: record.subtitle, contributors, book: BookMetadata { identifiers, publishers: book.publishers, languages, year: book.year, subjects } },
            description: record.description,
        })
    }
}

pub fn serialize<S: Serializer>(record: &BookRecord, serializer: S) -> Result<S::Ok, S::Error> {
    compact::serialize::<Book, S>(record, serializer)
}
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BookRecord, D::Error> {
    compact::deserialize::<Book, D>(deserializer)
}

pub mod optional {
    use super::*;
    pub fn serialize<S: Serializer>(record: &Option<BookRecord>, serializer: S) -> Result<S::Ok, S::Error> {
        compact::optional::serialize::<Book, S>(record, serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<BookRecord>, D::Error> {
        compact::optional::deserialize::<Book, D>(deserializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Compact {
        #[serde(with = "super")]
        record: BookRecord,
    }
    #[derive(Serialize)]
    struct Previous<'a> {
        record: &'a BookRecord,
    }

    fn record() -> BookRecord {
        BookRecord {
            metadata: BookDetails {
                title: "Återkomsten 📚".into(),
                subtitle: Some(String::new()),
                contributors: vec![Contributor::with_id(ContributorId::from_u128(42), "Test Author", MarcRelatorCode([0, 128, 255])).unwrap()],
                book: BookMetadata {
                    identifiers: [
                        Scheme::Unspecified,
                        Scheme::Isbn,
                        Scheme::Asin,
                        Scheme::Doi,
                        Scheme::Issn,
                        Scheme::Oclc,
                        Scheme::Lccn,
                        Scheme::Uuid,
                        Scheme::Calibre,
                        Scheme::Google,
                        Scheme::Goodreads,
                        Scheme::Kobo,
                        Scheme::Other("isbn".into()),
                        Scheme::Other(String::new()),
                        Scheme::Other("Custom:Case".into()),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, scheme)| Identifier::new("source value", scheme, [Scope::Book, Scope::Edition, Scope::Work][index % 3]).unwrap())
                    .collect(),
                    publishers: vec!["Publisher".into(), String::new()],
                    languages: vec![LanguageTag::parse("sv-SE").unwrap(), LanguageTag::parse("zh-Hant").unwrap()],
                    year: Some(i32::MIN),
                    subjects: vec![BookSubject::with_details(Some("subject-id".into()), "Fiction", Some("Fiction, historical".into()), "provider", Some("bisac".into()), Some("FIC000000".into())).unwrap()],
                },
            },
            description: "A description\nwith formatting and Unicode: é.".into(),
        }
    }

    #[test]
    fn typed_records_preserve_every_value_and_existing_storage_and_json() {
        let mut no_optionals = record();
        no_optionals.subtitle = None;
        no_optionals.book.year = None;
        no_optionals.book.subjects = vec![BookSubject::new(None, "Subject", "file", None, None).unwrap()];
        let mut max_year = record();
        max_year.book.year = Some(i32::MAX);
        for record in [BookRecord { metadata: BookDetails::default(), description: String::new() }, record(), no_optionals, max_year] {
            let previous = Previous { record: &record };
            let value = Compact { record: record.clone() };
            let stored = wire::encode(&previous).unwrap();
            assert_eq!(stored, wire::encode(&value).unwrap());
            assert_eq!(wire::decode::<Compact>(&stored, 65536).unwrap(), value);
            let json = serde_json::to_vec(&previous).unwrap();
            assert_eq!(json, serde_json::to_vec(&value).unwrap());
            assert_eq!(serde_json::from_slice::<Compact>(&json).unwrap(), value);
            let bytes = protobuf_wire::encode(&value).unwrap();
            assert_eq!(bytes.len(), protobuf_wire::encoded_len(&value).unwrap());
            assert_eq!(protobuf_wire::decode::<Compact>(&bytes, 65536).unwrap(), value);
            assert_eq!(protobuf_wire::decode_byte_fields::<Compact>(&bytes, 65536).unwrap(), value);
            assert_eq!(protobuf_wire::representation::<_, Compact>(&value).unwrap(), value);
            let mut legacy = protobuf_wire::encode(&previous).unwrap();
            legacy[1] = 30;
            assert_eq!(protobuf_wire::decode_state::<Compact>(&legacy, 65536).unwrap(), value);
        }
    }

    #[test]
    fn optional_records_preserve_missing_null_and_present_values() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Optional {
            #[serde(default, with = "super::optional")]
            record: Option<BookRecord>,
        }
        assert_eq!(serde_json::from_str::<Optional>("{}").unwrap(), Optional { record: None });
        for record in [None, Some(record())] {
            let value = Optional { record };
            let bytes = protobuf_wire::encode(&value).unwrap();
            assert_eq!(protobuf_wire::decode::<Optional>(&bytes, 65536).unwrap(), value);
            assert_eq!(protobuf_wire::representation::<_, Optional>(&value).unwrap(), value);
            let json = serde_json::to_vec(&value).unwrap();
            assert_eq!(serde_json::from_slice::<Optional>(&json).unwrap(), value);
            let stored = wire::encode(&value).unwrap();
            assert_eq!(wire::decode::<Optional>(&stored, 65536).unwrap(), value);
        }
    }

    #[test]
    fn compact_records_retain_domain_validation() {
        let original = Record::decode(Book::pack(&record()).unwrap().as_slice()).unwrap();
        for mutate in [
            (|value: &mut Record| value.book = None) as fn(&mut Record),
            |value| value.contributors[0].id.pop().map(|_| ()).unwrap(),
            |value| value.contributors[0].role.push(0),
            |value| value.contributors[0].name.clear(),
            |value| value.book.as_mut().unwrap().identifiers[0].scope = 99,
            |value| value.book.as_mut().unwrap().identifiers[0].scheme = "unknown".into(),
            |value| value.book.as_mut().unwrap().identifiers[0].value.clear(),
            |value| value.book.as_mut().unwrap().languages = vec!["invalid!".into()],
            |value| value.book.as_mut().unwrap().subjects[0].source.clear(),
        ] {
            let mut invalid = original.clone();
            mutate(&mut invalid);
            assert!(Book::unpack(&invalid.encode_to_vec()).is_err());
        }
        assert!(Book::unpack(&[0xff]).is_err());
        assert!(Book::unpack(&vec![0; protobuf_wire::MAX_DECODED_REQUEST_BYTES + 1]).is_err());
        let mut oversized = record();
        oversized.description = "x".repeat(protobuf_wire::MAX_DECODED_REQUEST_BYTES);
        assert!(Book::pack(&oversized).is_err());
    }

    #[test]
    fn typed_metadata_removes_generic_map_overhead() {
        let value = Compact { record: record() };
        let previous = Previous { record: &value.record };
        let compact = protobuf_wire::encoded_len(&value).unwrap();
        let previous = protobuf_wire::encoded_len(&previous).unwrap();
        assert!(compact * 2 < previous, "compact={compact} previous={previous}");
    }
}
