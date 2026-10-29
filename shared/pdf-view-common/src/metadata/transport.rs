//! Compact PDF numeric data shared by every host. The transport marker is
//! interpreted by protobuf-wire; JSON and persisted bincode retain their
//! original schemas. Readers also accept the previous named-field payloads.
use super::{PdfByteRange, PdfDependencyIndex, PdfStoredPage};
use prost::Message;
use protobuf_wire::compact::{self, Codec};
#[cfg(test)]
use serde::Serialize;
use serde::{Deserializer, Serializer};
use std::collections::HashMap;
const MAX_PAGES: usize = 32767;
const MAX_RANGES: usize = 100_000;

// Numeric protobuf columns avoid one named map and several Value wrappers per
// page. A single value broadcasts to the entire column; omitted rotations are
// all zero. Dimension bits preserve f32 values exactly.
#[derive(Clone, PartialEq, Message)]
struct Pages {
    #[prost(uint32, tag = "1")]
    count: u32,
    #[prost(fixed32, repeated, packed = "true", tag = "2")]
    widths: Vec<u32>,
    #[prost(fixed32, repeated, packed = "true", tag = "3")]
    heights: Vec<u32>,
    #[prost(uint32, repeated, packed = "true", tag = "4")]
    rotations: Vec<u32>,
}

// Shared PDF objects frequently occur in many pages. Store each (offset,length)
// pair once, and encode each page as references into that dictionary.
#[derive(Clone, PartialEq, Message)]
struct Dependencies {
    #[prost(uint64, tag = "1")]
    length: u64,
    #[prost(uint32, tag = "2")]
    startup_count: u32,
    #[prost(uint32, repeated, packed = "true", tag = "3")]
    page_counts: Vec<u32>,
    #[prost(uint64, repeated, packed = "true", tag = "4")]
    ranges: Vec<u64>,
    #[prost(uint32, repeated, packed = "true", tag = "5")]
    references: Vec<u32>,
}

fn column(mut values: Vec<u32>) -> Vec<u32> {
    if values.first().is_some_and(|first| values.iter().all(|value| value == first)) {
        values.truncate(1);
    }
    values
}

impl Codec for Pages {
    type Value = Vec<PdfStoredPage>;
    fn pack(value: &Self::Value) -> Result<Vec<u8>, String> {
        if value.len() > MAX_PAGES {
            return Err("too many PDF pages".into());
        }
        let rotations = column(value.iter().map(|page| u32::from(page.rotation)).collect());
        Ok(Pages {
            count: value.len() as u32,
            widths: column(value.iter().map(|page| page.width.to_bits()).collect()),
            heights: column(value.iter().map(|page| page.height.to_bits()).collect()),
            rotations: if rotations == [0] { Vec::new() } else { rotations },
        }
        .encode_to_vec())
    }

    fn unpack(bytes: &[u8]) -> Result<Self::Value, String> {
        if bytes.len() > MAX_PAGES * 12 + 64 {
            return Err("PDF page columns exceed their limit".into());
        }
        let pages = Pages::decode(bytes).map_err(|error| error.to_string())?;
        let count = pages.count as usize;
        let valid_column = |values: &[u32]| values.len() == count || (count > 0 && values.len() == 1);
        if count > MAX_PAGES || !valid_column(&pages.widths) || !valid_column(&pages.heights) || (!pages.rotations.is_empty() && !valid_column(&pages.rotations)) {
            return Err("invalid PDF page column lengths".into());
        }
        if pages.rotations.iter().any(|rotation| *rotation > u16::MAX.into()) {
            return Err("PDF rotation exceeds its storage range".into());
        }
        let at = |values: &[u32], index: usize| values[if values.len() == 1 { 0 } else { index }];
        Ok((0..count)
            .map(|index| PdfStoredPage { width: f32::from_bits(at(&pages.widths, index)), height: f32::from_bits(at(&pages.heights, index)), rotation: if pages.rotations.is_empty() { 0 } else { at(&pages.rotations, index) as u16 } })
            .collect())
    }
}

impl Codec for Dependencies {
    type Value = PdfDependencyIndex;
    fn pack(value: &Self::Value) -> Result<Vec<u8>, String> {
        let count = value.startup.len().saturating_add(value.pages.iter().map(Vec::len).sum::<usize>());
        if value.pages.len() > MAX_PAGES || count > MAX_RANGES {
            return Err("PDF dependency index exceeds its limit".into());
        }
        let mut dictionary = HashMap::new();
        let mut ranges = Vec::new();
        let references = value
            .startup
            .iter()
            .chain(value.pages.iter().flatten())
            .map(|range| {
                *dictionary.entry((range.offset, range.length)).or_insert_with(|| {
                    let index = (ranges.len() / 2) as u32;
                    ranges.extend([range.offset, range.length]);
                    index
                })
            })
            .collect();
        Ok(Dependencies { length: value.length, startup_count: value.startup.len() as u32, page_counts: value.pages.iter().map(|page| page.len() as u32).collect(), ranges, references }.encode_to_vec())
    }

    fn unpack(bytes: &[u8]) -> Result<Self::Value, String> {
        if bytes.len() > MAX_RANGES * 24 + MAX_PAGES * 4 + 64 {
            return Err("PDF dependency bytes exceed their limit".into());
        }
        let index = Dependencies::decode(bytes).map_err(|error| error.to_string())?;
        let expected = index.page_counts.iter().try_fold(index.startup_count as usize, |count, next| count.checked_add(*next as usize));
        if index.page_counts.len() > MAX_PAGES || index.ranges.len() % 2 != 0 || index.ranges.len() / 2 > MAX_RANGES || index.references.len() > MAX_RANGES || expected != Some(index.references.len()) {
            return Err("invalid PDF dependency column lengths".into());
        }
        let dictionary = index.ranges.chunks_exact(2).map(|pair| PdfByteRange { offset: pair[0], length: pair[1] }).collect::<Vec<_>>();
        if index.references.iter().any(|reference| *reference as usize >= dictionary.len()) {
            return Err("PDF dependency references an absent range".into());
        }
        let mut references = index.references.into_iter().map(|reference| dictionary[reference as usize]);
        let startup = references.by_ref().take(index.startup_count as usize).collect();
        let pages = index.page_counts.into_iter().map(|count| references.by_ref().take(count as usize).collect()).collect();
        Ok(PdfDependencyIndex { length: index.length, startup, pages })
    }
}

pub(super) mod pages {
    use super::*;
    pub fn serialize<S: Serializer>(pages: &Vec<PdfStoredPage>, serializer: S) -> Result<S::Ok, S::Error> {
        compact::serialize::<Pages, S>(pages, serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<PdfStoredPage>, D::Error> {
        compact::deserialize::<Pages, D>(deserializer)
    }
}

pub(super) mod dependencies {
    use super::*;
    pub fn serialize<S: Serializer>(index: &Option<PdfDependencyIndex>, serializer: S) -> Result<S::Ok, S::Error> {
        compact::optional::serialize::<Dependencies, S>(index, serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<PdfDependencyIndex>, D::Error> {
        compact::optional::deserialize::<Dependencies, D>(deserializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PdfReaderMetadata;

    // The unadapted model locks down existing JSON, persisted bytes, and the
    // previous HTTP representation independently of the new field adapters.
    #[derive(Serialize)]
    struct Previous<'a> {
        version: u32,
        content_hash: &'a str,
        pages: &'a [PdfStoredPage],
        skippable_pages: &'a Option<Vec<usize>>,
        annotations: &'a [crate::PdfStoredAnnotation],
        dependencies: &'a Option<PdfDependencyIndex>,
    }
    fn previous(metadata: &PdfReaderMetadata) -> Previous<'_> {
        Previous { version: metadata.version, content_hash: &metadata.content_hash, pages: &metadata.pages, skippable_pages: &metadata.skippable_pages, annotations: &metadata.annotations, dependencies: &metadata.dependencies }
    }
    fn metadata(count: usize) -> PdfReaderMetadata {
        let shared = PdfByteRange { offset: 128, length: 256 };
        PdfReaderMetadata {
            version: 1,
            content_hash: "a".repeat(64),
            pages: vec![PdfStoredPage { width: 600.0, height: 800.0, rotation: 0 }; count],
            skippable_pages: Some(Vec::new()),
            annotations: Vec::new(),
            dependencies: Some(PdfDependencyIndex { length: 1_000_000, startup: vec![shared], pages: (0..count).map(|page| vec![shared, PdfByteRange { offset: 4096 + page as u64 * 512, length: 128 }]).collect() }),
        }
    }

    #[test]
    fn compact_payload_round_trips_without_changing_json_or_persisted_bytes() {
        for count in [0, 1, 2, 127, 128, 1000] {
            let metadata = metadata(count);
            let old = previous(&metadata);
            assert_eq!(serde_json::to_vec(&metadata).unwrap(), serde_json::to_vec(&old).unwrap());
            assert_eq!(wire::encode(&metadata).unwrap(), wire::encode(&old).unwrap());
            let persisted = wire::encode(&old).unwrap();
            assert_eq!(wire::decode::<PdfReaderMetadata>(&persisted, 4_000_000).unwrap(), metadata);
            let json = serde_json::to_vec(&old).unwrap();
            assert_eq!(serde_json::from_slice::<PdfReaderMetadata>(&json).unwrap(), metadata);
            let compact = protobuf_wire::encode(&metadata).unwrap();
            assert_eq!(compact.len(), protobuf_wire::encoded_len(&metadata).unwrap());
            assert_eq!(protobuf_wire::decode::<PdfReaderMetadata>(&compact, 4_000_000).unwrap(), metadata);
            assert_eq!(protobuf_wire::decode_byte_fields::<PdfReaderMetadata>(&compact, 4_000_000).unwrap(), metadata);
            assert_eq!(protobuf_wire::representation::<_, PdfReaderMetadata>(&metadata).unwrap(), metadata);
            let mut legacy = protobuf_wire::encode(&old).unwrap();
            assert_eq!(legacy[1], protobuf_wire::version() as u8);
            legacy[1] = 30;
            assert_eq!(protobuf_wire::decode_state::<PdfReaderMetadata>(&legacy, 4_000_000).unwrap(), metadata);
            assert!(protobuf_wire::decode::<PdfReaderMetadata>(&legacy, 4_000_000).is_err());
        }
        let mut no_index = metadata(1);
        no_index.dependencies = None;
        let bytes = protobuf_wire::encode(&no_index).unwrap();
        assert_eq!(protobuf_wire::decode::<PdfReaderMetadata>(&bytes, 4096).unwrap(), no_index);
    }

    #[test]
    fn page_columns_preserve_float_bits_rotations_and_mixed_dimensions() {
        let pages =
            (0..100).map(|index| PdfStoredPage { width: f32::from_bits(0x43ff_0000 + index), height: if index % 2 == 0 { -0.0 } else { f32::from_bits(0x7fc0_0001) }, rotation: [0, 90, 180, 270][index as usize % 4] }).collect::<Vec<_>>();
        let decoded = Pages::unpack(&Pages::pack(&pages).unwrap()).unwrap();
        for (before, after) in pages.iter().zip(decoded) {
            assert_eq!(before.width.to_bits(), after.width.to_bits());
            assert_eq!(before.height.to_bits(), after.height.to_bits());
            assert_eq!(before.rotation, after.rotation);
        }
        assert!(Pages::pack(&vec![PdfStoredPage { width: 1.0, height: 2.0, rotation: 0 }; MAX_PAGES + 1]).is_err());
    }

    #[test]
    fn range_dictionary_preserves_grouping_order_duplicates_and_large_offsets() {
        let range = PdfByteRange { offset: u64::MAX - 100, length: 50 };
        let index = PdfDependencyIndex { length: u64::MAX, startup: vec![range, range], pages: vec![vec![], vec![range], vec![range, range]] };
        let bytes = Dependencies::pack(&index).unwrap();
        assert_eq!(Dependencies::decode(bytes.as_slice()).unwrap().ranges, vec![range.offset, range.length]);
        assert_eq!(Dependencies::unpack(&bytes).unwrap(), index);
    }

    #[test]
    fn malformed_columns_and_range_references_are_rejected() {
        let pages = Pages { count: 3, widths: vec![1, 2], heights: vec![3], rotations: vec![] };
        assert!(Pages::unpack(&pages.encode_to_vec()).is_err());
        let pages = Pages { count: 1, widths: vec![1], heights: vec![2], rotations: vec![u32::MAX] };
        assert!(Pages::unpack(&pages.encode_to_vec()).is_err());
        let valid = Dependencies { length: 100, startup_count: 0, page_counts: vec![1], ranges: vec![0, 20], references: vec![0] };
        for malformed in [
            Dependencies { references: vec![1], ..valid.clone() },
            Dependencies { ranges: vec![0], ..valid.clone() },
            Dependencies { startup_count: u32::MAX, ..valid.clone() },
            Dependencies { page_counts: vec![u32::MAX], ..valid.clone() },
            Dependencies { page_counts: vec![1; MAX_PAGES + 1], ..valid.clone() },
        ] {
            assert!(Dependencies::unpack(&malformed.encode_to_vec()).is_err());
        }
        assert!(Pages::unpack(&[0xff]).is_err());
        assert!(Dependencies::unpack(&[0xff]).is_err());
        // Empty bytes denote an empty page column set, rather than legacy
        // numeric elements; normal semantic metadata validation still applies.
        assert!(Pages::unpack(&[]).unwrap().is_empty());
    }

    #[test]
    fn representative_pdf_removes_repeated_fields_and_shared_ranges() {
        let metadata = metadata(1000);
        let old_size = protobuf_wire::encode(&previous(&metadata)).unwrap().len();
        let bytes = protobuf_wire::encode(&metadata).unwrap();
        assert!(bytes.len() * 10 < old_size, "compact={} previous={old_size}", bytes.len());
        assert_eq!(protobuf_wire::decode::<PdfReaderMetadata>(&bytes, 4_000_000).unwrap(), metadata);
    }
}
