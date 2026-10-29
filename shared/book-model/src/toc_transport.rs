//! Compact TOC transport, shared by navigation documents and enrichment answers.
//! Only protobuf transport changes; JSON and persisted bincode retain their
//! original entry representation, which remains readable over transport too.
use crate::BookTocEntry;
use prost::Message;
use protobuf_wire::compact::{self, Codec};
use serde::{Deserializer, Serializer};

const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 64;
const MAX_BYTES: usize = protobuf_wire::MAX_DECODED_REQUEST_BYTES;

#[derive(Clone, PartialEq, Message)]
struct Entries {
    #[prost(message, repeated, tag = "1")]
    entries: Vec<Entry>,
}

#[derive(Clone, PartialEq, Message)]
struct Entry {
    #[prost(string, tag = "1")]
    title: String,
    #[prost(string, tag = "2")]
    target: String,
    #[prost(message, repeated, tag = "3")]
    children: Vec<Entry>,
}

struct Toc;
impl Codec for Toc {
    type Value = Vec<BookTocEntry>;
    fn pack(value: &Self::Value) -> Result<Vec<u8>, String> {
        validate(value)?;
        fn entry(value: &BookTocEntry) -> Entry {
            Entry { title: value.title.clone(), target: value.target.clone(), children: value.children.iter().map(entry).collect() }
        }
        let packed = Entries { entries: value.iter().map(entry).collect() };
        if packed.encoded_len() > MAX_BYTES {
            return Err("TOC bytes exceed their limit".into());
        }
        Ok(packed.encode_to_vec())
    }
    fn unpack(bytes: &[u8]) -> Result<Self::Value, String> {
        if bytes.len() > MAX_BYTES {
            return Err("TOC bytes exceed their limit".into());
        }
        let packed = Entries::decode(bytes).map_err(|error| error.to_string())?;
        fn entry(value: Entry) -> BookTocEntry {
            BookTocEntry { title: value.title, target: value.target, children: value.children.into_iter().map(entry).collect() }
        }
        let entries: Vec<_> = packed.entries.into_iter().map(entry).collect();
        validate(&entries)?;
        Ok(entries)
    }
}

fn validate(entries: &[BookTocEntry]) -> Result<(), String> {
    fn visit(entries: &[BookTocEntry], depth: usize, count: &mut usize, text_bytes: &mut usize) -> Result<(), String> {
        if !entries.is_empty() && depth > MAX_DEPTH {
            return Err("TOC nesting exceeds its limit".into());
        }
        *count += entries.len();
        if *count > MAX_ENTRIES {
            return Err("TOC entry count exceeds its limit".into());
        }
        for entry in entries {
            *text_bytes += entry.title.len() + entry.target.len();
            if *text_bytes > MAX_BYTES {
                return Err("TOC text exceeds its limit".into());
            }
            visit(&entry.children, depth + 1, count, text_bytes)?;
        }
        Ok(())
    }
    visit(entries, 1, &mut 0, &mut 0)
}

pub fn serialize<S: Serializer>(entries: &Vec<BookTocEntry>, serializer: S) -> Result<S::Ok, S::Error> {
    compact::serialize::<Toc, S>(entries, serializer)
}
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<BookTocEntry>, D::Error> {
    compact::deserialize::<Toc, D>(deserializer)
}

pub mod optional {
    use super::*;
    pub fn serialize<S: Serializer>(entries: &Option<Vec<BookTocEntry>>, serializer: S) -> Result<S::Ok, S::Error> {
        compact::optional::serialize::<Toc, S>(entries, serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Vec<BookTocEntry>>, D::Error> {
        compact::optional::deserialize::<Toc, D>(deserializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudiobookTrack, BookTocDocument};
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    struct Previous<'a> {
        entries: &'a [BookTocEntry],
        duration_ms: Option<u64>,
        tracks: &'a Option<Vec<AudiobookTrack>>,
    }

    fn entries() -> Vec<BookTocEntry> {
        vec![
            BookTocEntry {
                title: "Del I — Återkomsten 📚".into(),
                target: "Text/chapter.xhtml#section-1".into(),
                children: vec![BookTocEntry { title: "Kapitel 1".into(), target: "pdf-page:12".into(), children: vec![BookTocEntry::default()] }],
            },
            BookTocEntry { title: "Del II".into(), target: "audiobook:123456789".into(), children: vec![] },
        ]
    }

    #[test]
    fn compact_toc_preserves_hierarchy_json_and_persisted_bytes() {
        for entries in [vec![], entries()] {
            let document = BookTocDocument::entries(entries);
            let previous = Previous { entries: &document.entries, duration_ms: document.duration_ms, tracks: &document.tracks };
            let persisted = wire::encode(&previous).unwrap();
            assert_eq!(persisted, wire::encode(&document).unwrap());
            assert_eq!(wire::decode::<BookTocDocument>(&persisted, 4096).unwrap(), document);
            let json = serde_json::to_vec(&previous).unwrap();
            assert_eq!(json, serde_json::to_vec(&document).unwrap());
            assert_eq!(serde_json::from_slice::<BookTocDocument>(&json).unwrap(), document);
            let packed = protobuf_wire::encode(&document).unwrap();
            assert_eq!(packed.len(), protobuf_wire::encoded_len(&document).unwrap());
            assert_eq!(protobuf_wire::decode::<BookTocDocument>(&packed, 4096).unwrap(), document);
            assert_eq!(protobuf_wire::decode_byte_fields::<BookTocDocument>(&packed, 4096).unwrap(), document);
            assert_eq!(protobuf_wire::representation::<_, BookTocDocument>(&document).unwrap(), document);
            let mut legacy = protobuf_wire::encode(&previous).unwrap();
            legacy[1] = 30;
            assert_eq!(protobuf_wire::decode_state::<BookTocDocument>(&legacy, 4096).unwrap(), document);
        }
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Optional {
        #[serde(default, with = "super::optional")]
        toc: Option<Vec<BookTocEntry>>,
    }

    #[test]
    fn optional_toc_preserves_absence_and_empty_documents() {
        #[derive(Serialize)]
        struct Old<'a> {
            toc: &'a Option<Vec<BookTocEntry>>,
        }
        assert_eq!(serde_json::from_str::<Optional>("{}").unwrap(), Optional { toc: None });
        for toc in [None, Some(vec![]), Some(entries())] {
            let value = Optional { toc };
            let old = Old { toc: &value.toc };
            assert_eq!(wire::encode(&value).unwrap(), wire::encode(&old).unwrap());
            assert_eq!(serde_json::to_vec(&value).unwrap(), serde_json::to_vec(&old).unwrap());
            let packed = protobuf_wire::encode(&value).unwrap();
            assert_eq!(protobuf_wire::decode::<Optional>(&packed, 4096).unwrap(), value);
            assert_eq!(protobuf_wire::representation::<_, Optional>(&value).unwrap(), value);
            let legacy = protobuf_wire::encode(&old).unwrap();
            assert_eq!(protobuf_wire::decode_state::<Optional>(&legacy, 4096).unwrap(), value);
        }
    }

    fn nested(depth: usize) -> Vec<BookTocEntry> {
        (0..depth).fold(vec![], |children, _| vec![BookTocEntry { children, ..BookTocEntry::default() }])
    }

    #[test]
    fn malformed_and_excessive_packed_tocs_are_rejected() {
        // Truncated message and invalid UTF-8 in a title.
        for bytes in [&[0xff][..], &[0x0a, 0x03, 0x0a, 0x01, 0xff][..]] {
            assert!(Toc::unpack(bytes).is_err());
        }
        assert!(Toc::unpack(&[]).unwrap().is_empty());
        let deepest = nested(MAX_DEPTH);
        assert_eq!(Toc::unpack(&Toc::pack(&deepest).unwrap()).unwrap(), deepest);
        assert!(Toc::pack(&nested(MAX_DEPTH + 1)).is_err());
        let deep_wire = (0..MAX_DEPTH + 1).fold(vec![], |children, _| vec![Entry { children, ..Entry::default() }]);
        assert!(Toc::unpack(&Entries { entries: deep_wire }.encode_to_vec()).is_err());
        assert!(Toc::pack(&vec![BookTocEntry::default(); MAX_ENTRIES + 1]).is_err());
        let many = Entries { entries: vec![Entry::default(); MAX_ENTRIES + 1] };
        assert!(Toc::unpack(&many.encode_to_vec()).is_err());
        assert!(Toc::unpack(&vec![0; MAX_BYTES + 1]).is_err());
        assert!(Toc::pack(&vec![BookTocEntry { title: "a".repeat(MAX_BYTES + 1), ..BookTocEntry::default() }]).is_err());
    }

    #[test]
    fn compact_toc_removes_generic_field_and_empty_children_overhead() {
        let document = BookTocDocument::entries(vec![BookTocEntry { title: "Chapter".into(), target: "chapter.xhtml#start".into(), children: vec![] }; 1000]);
        let previous = Previous { entries: &document.entries, duration_ms: None, tracks: &None };
        let old = protobuf_wire::encoded_len(&previous).unwrap();
        let compact = protobuf_wire::encoded_len(&document).unwrap();
        assert!(compact * 2 < old, "compact={compact} previous={old}");
    }
}
