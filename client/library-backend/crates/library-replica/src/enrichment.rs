use book_model::{BookRecord, BookTocEntry};
use metadata_contract::WorkPopularity;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A definitive lookup outcome; its metadata is already applied to the book.
/// Failures are device-local retry work.
#[derive(Debug, Clone, PartialEq)]
pub enum EnrichmentResult {
    Found { toc: Option<Vec<BookTocEntry>>, cover_id: Option<String>, popularity: Option<WorkPopularity> },
    NoMatch,
}

// Historical variants are read only. Append the finalized answer so existing
// bincode additions remain readable without changing their surrounding layout.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Encoded<R, T, C, P> {
    Found { record: R, toc: T, cover_id: C },
    NoMatch,
    FoundWithPopularity { record: R, toc: T, cover_id: C, popularity: P },
    Applied { toc: T, cover_id: C, popularity: Option<P> },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "EnrichmentResult", rename_all = "snake_case")]
enum HumanEnrichment {
    Found {
        #[serde(default, with = "book_model::toc_transport::optional")]
        toc: Option<Vec<BookTocEntry>>,
        cover_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        popularity: Option<WorkPopularity>,
    },
    NoMatch,
}

impl Serialize for EnrichmentResult {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            return HumanEnrichment::serialize(self, serializer);
        }
        let encoded = match self {
            Self::Found { toc, cover_id, popularity } => Encoded::<&BookRecord, _, _, _>::Applied { toc, cover_id, popularity: popularity.as_ref() },
            Self::NoMatch => Encoded::NoMatch,
        };
        encoded.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EnrichmentResult {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        DecodedEnrichment::deserialize(deserializer).map(|decoded| decoded.result)
    }
}

/// A historical second record is retained only long enough for BookAddition's
/// decoder to merge it into the canonical book record.
pub(crate) struct DecodedEnrichment {
    pub record: Option<BookRecord>,
    pub result: EnrichmentResult,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum HumanInput {
    Found {
        #[serde(default)]
        record: Option<BookRecord>,
        #[serde(default, with = "book_model::toc_transport::optional")]
        toc: Option<Vec<BookTocEntry>>,
        cover_id: Option<String>,
        #[serde(default)]
        popularity: Option<WorkPopularity>,
    },
    NoMatch,
}

impl<'de> Deserialize<'de> for DecodedEnrichment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (record, result) = if deserializer.is_human_readable() {
            match HumanInput::deserialize(deserializer)? {
                HumanInput::Found { record, toc, cover_id, popularity } => (record, EnrichmentResult::Found { toc, cover_id, popularity }),
                HumanInput::NoMatch => (None, EnrichmentResult::NoMatch),
            }
        } else {
            match Encoded::deserialize(deserializer)? {
                Encoded::Found { record, toc, cover_id } => (Some(record), EnrichmentResult::Found { toc, cover_id, popularity: None }),
                Encoded::FoundWithPopularity { record, toc, cover_id, popularity } => (Some(record), EnrichmentResult::Found { toc, cover_id, popularity: Some(popularity) }),
                Encoded::Applied { toc, cover_id, popularity } => (None, EnrichmentResult::Found { toc, cover_id, popularity }),
                Encoded::NoMatch => (None, EnrichmentResult::NoMatch),
            }
        };
        Ok(Self { record, result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The schema used before popularity was added, independent of the adapter.
    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum LegacyEnrichment {
        Found { record: BookRecord, toc: Option<Vec<BookTocEntry>>, cover_id: Option<String> },
        NoMatch,
    }

    #[test]
    fn original_saved_answers_retain_the_record_for_the_addition_decoder() {
        let record = BookRecord { metadata: Default::default(), description: String::new() };
        let original = LegacyEnrichment::Found { record: record.clone(), toc: None, cover_id: None };
        let bytes = sync_common::wire::encode(&original).unwrap();
        let decoded: DecodedEnrichment = sync_common::wire::decode(&bytes, 4096).unwrap();
        assert_eq!(decoded.record, Some(record));
        let result = decoded.result;
        assert_eq!(result, EnrichmentResult::Found { toc: None, cover_id: None, popularity: None });
        assert_ne!(sync_common::wire::encode(&result).unwrap(), bytes);
        let transport = sync_common::transport::encode(&original).unwrap();
        assert_eq!(sync_common::transport::decode::<EnrichmentResult>(&transport, 4096).unwrap(), result);
        assert_eq!(sync_common::wire::encode(&EnrichmentResult::NoMatch).unwrap(), sync_common::wire::encode(&LegacyEnrichment::NoMatch).unwrap());
    }

    #[test]
    fn popularity_survives_storage_and_sync_encodings() {
        let result = EnrichmentResult::Found { toc: None, cover_id: None, popularity: Some(WorkPopularity { open_library_work_id: "OL10W".into(), rating_count: 12, snapshot_date: "2026-09-01".into() }) };
        let bytes = sync_common::wire::encode(&result).unwrap();
        assert_eq!(sync_common::wire::decode::<EnrichmentResult>(&bytes, 4096).unwrap(), result);
        let bytes = sync_common::transport::encode(&result).unwrap();
        assert_eq!(sync_common::transport::decode::<EnrichmentResult>(&bytes, 4096).unwrap(), result);
        assert!(sync_common::transport::decode::<LegacyEnrichment>(&bytes, 4096).is_err());
    }

    #[test]
    fn enriched_toc_uses_shared_compaction_and_reads_previous_answers() {
        use sync_common::{transport, wire};
        let toc = Some(vec![BookTocEntry { title: "Part I".into(), target: "audiobook:0".into(), children: vec![BookTocEntry { title: "Chapter".into(), target: "audiobook:30000".into(), children: vec![] }] }]);
        let result = EnrichmentResult::Found { toc: toc.clone(), cover_id: None, popularity: None };
        let bytes = transport::encode(&result).unwrap();
        assert_eq!(transport::decode::<EnrichmentResult>(&bytes, 4096).unwrap(), result);
        assert_eq!(transport::encoded_len(&result).unwrap(), bytes.len());
        assert_eq!(transport::representation::<_, EnrichmentResult>(&result).unwrap(), result);
        assert!(!bytes.windows("children".len()).any(|window| window == b"children"));
        let stored = wire::encode(&result).unwrap();
        let previous = Encoded::<&BookRecord, _, _, WorkPopularity>::Applied { toc: &toc, cover_id: &None::<String>, popularity: None };
        assert_eq!(stored, wire::encode(&previous).unwrap());
        assert_eq!(wire::decode::<EnrichmentResult>(&stored, 4096).unwrap(), result);
        let provider = provider();
        let legacy = LegacyHuman::Found { record: &provider, toc, cover_id: None, popularity: None };
        let legacy_bytes = transport::encode(&legacy).unwrap();
        assert_eq!(transport::decode::<EnrichmentResult>(&legacy_bytes, 4096).unwrap(), result);
        let json = serde_json::to_vec(&result).unwrap();
        assert_eq!(serde_json::from_slice::<EnrichmentResult>(&json).unwrap(), result);
        assert!(String::from_utf8(json).unwrap().contains("children"));
    }

    #[derive(Serialize)]
    struct LegacyAddition<'a, E> {
        facts: &'a crate::BookFacts,
        record: &'a BookRecord,
        navigation: &'a Option<book_model::BookTocDocument>,
        pdf: &'a Option<pdf_view_common::PdfReaderMetadata>,
        recording: &'a Option<metadata_contract::audible::LookupRequest>,
        enrichment: Option<E>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum LegacyHuman<'a> {
        Found { record: &'a BookRecord, toc: Option<Vec<BookTocEntry>>, cover_id: Option<String>, popularity: Option<WorkPopularity> },
    }

    fn draft() -> crate::BookAddition {
        let mut record = BookRecord { metadata: Default::default(), description: "File description".into() };
        record.title = "File title".into();
        record.subtitle = Some("File subtitle".into());
        record.book.year = Some(1999);
        crate::BookAddition { facts: crate::BookFacts { added_at: 1, format: book_model::BookFormat::Epub }, record, navigation: None, pdf: None, recording: None, enrichment: None }
    }

    fn provider() -> BookRecord {
        use book_model::{BookSubject, Contributor, ContributorId, AUTHOR_MARC_RELATOR_CODE};
        let mut record = BookRecord { metadata: Default::default(), description: "Provider description".into() };
        record.title = "Provider title".into();
        record.subtitle = Some("Provider subtitle".into());
        record.book.year = Some(2005);
        record.book.publishers = vec!["Provider publisher".into()];
        record.book.subjects = vec![BookSubject::new(None, "Provider subject", "provider", None, None).unwrap()];
        record.contributors = vec![Contributor::with_id(ContributorId::from_u128(1), "Provider author", AUTHOR_MARC_RELATOR_CODE).unwrap()];
        record
    }

    fn assert_effective_record(addition: &crate::BookAddition, description: &str, year: i32) {
        assert_eq!(addition.record.title, "File title");
        assert_eq!(addition.record.subtitle.as_deref(), Some("File subtitle"));
        assert_eq!(addition.record.description, description);
        assert_eq!(addition.record.book.year, Some(year));
        assert_eq!(addition.record.book.publishers, ["Provider publisher"]);
        assert_eq!(addition.record.book.subjects[0].name(), "Provider subject");
        assert_eq!(addition.record.contributors[0].name(), "Provider author");
    }

    #[test]
    fn enrichment_updates_one_record_and_preserves_file_precedence() {
        for has_file_values in [false, true] {
            let mut addition = draft();
            if !has_file_values {
                addition.record.description = " \n".into();
                addition.record.book.year = None;
            }
            addition.apply_enrichment(provider(), EnrichmentResult::Found { toc: None, cover_id: Some("cover".into()), popularity: None });
            assert_effective_record(&addition, if has_file_values { "File description" } else { "Provider description" }, if has_file_values { 1999 } else { 2005 });
            let json = serde_json::to_value(&addition).unwrap();
            assert!(json["enrichment"]["found"].get("record").is_none());
            assert_eq!(serde_json::from_value::<crate::BookAddition>(json).unwrap(), addition);
            let persisted = sync_common::wire::encode(&addition).unwrap();
            assert_eq!(sync_common::wire::decode::<crate::BookAddition>(&persisted, 100_000).unwrap(), addition);
            let transport = sync_common::transport::encode(&addition).unwrap();
            assert_eq!(sync_common::transport::decode::<crate::BookAddition>(&transport, 100_000).unwrap(), addition);
            assert_eq!(sync_common::transport::representation::<_, crate::BookAddition>(&addition).unwrap(), addition);
        }
    }

    #[test]
    fn old_two_record_additions_merge_while_reading_storage_and_sync() {
        let addition = draft();
        let provider = provider();
        let popularity = WorkPopularity { open_library_work_id: "OL10W".into(), rating_count: 12, snapshot_date: "2026-09-01".into() };
        for with_popularity in [false, true] {
            let enrichment: Encoded<&BookRecord, Option<Vec<BookTocEntry>>, Option<String>, WorkPopularity> = if with_popularity {
                Encoded::FoundWithPopularity { record: &provider, toc: None, cover_id: Some("cover".into()), popularity: popularity.clone() }
            } else {
                Encoded::Found { record: &provider, toc: None, cover_id: Some("cover".into()) }
            };
            let legacy = LegacyAddition { facts: &addition.facts, record: &addition.record, navigation: &addition.navigation, pdf: &addition.pdf, recording: &addition.recording, enrichment: Some(enrichment) };
            let persisted = sync_common::wire::encode(&legacy).unwrap();
            let upgraded: crate::BookAddition = sync_common::wire::decode(&persisted, 100_000).unwrap();
            assert_effective_record(&upgraded, "File description", 1999);
            let new_bytes = sync_common::wire::encode(&upgraded).unwrap();
            assert_ne!(new_bytes, persisted);
            assert_eq!(sync_common::wire::decode::<crate::BookAddition>(&new_bytes, 100_000).unwrap(), upgraded);
            let human_legacy = LegacyAddition {
                facts: &addition.facts,
                record: &addition.record,
                navigation: &addition.navigation,
                pdf: &addition.pdf,
                recording: &addition.recording,
                enrichment: Some(LegacyHuman::Found { record: &provider, toc: None, cover_id: Some("cover".into()), popularity: with_popularity.then_some(popularity.clone()) }),
            };
            let mut old_transport = sync_common::transport::encode(&human_legacy).unwrap();
            old_transport[1] = 30;
            assert_eq!(sync_common::transport::decode_state::<crate::BookAddition>(&old_transport, 100_000).unwrap(), upgraded);
            assert_eq!(serde_json::from_value::<crate::BookAddition>(serde_json::to_value(&human_legacy).unwrap()).unwrap(), upgraded);
            assert!(matches!(&upgraded.enrichment, Some(EnrichmentResult::Found { cover_id: Some(id), popularity: value, .. }) if id=="cover" && value.is_some()==with_popularity));
        }
    }

    #[test]
    fn unanswered_drafts_and_no_match_keep_their_original_record() {
        let mut addition = draft();
        for result in [None, Some(EnrichmentResult::NoMatch)] {
            addition.enrichment = result;
            let bytes = sync_common::wire::encode(&addition).unwrap();
            assert_eq!(sync_common::wire::decode::<crate::BookAddition>(&bytes, 100_000).unwrap(), addition);
        }
    }
}
