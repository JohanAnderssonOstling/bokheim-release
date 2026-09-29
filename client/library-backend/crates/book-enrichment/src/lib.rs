//! Portable enrichment policy shared by client and authority ingestion.
//! Storage, scheduling, file decoding, and network transport belong to callers.
pub mod evidence;
pub mod filename_identity;
pub mod filename_isbn;
pub mod related_isbn;
mod subject_pipeline;
pub mod title_identity;
use book_model::{from_metadata_value, BookMetadata, Scheme};
use serde::{Deserialize, Serialize};
pub use subject_pipeline::{isbn_key, SubjectCode, SubjectPhase, SubjectPipeline, SubjectStep};

/// Library-wide enrichment waves. Hosts may execute a wave using their own
/// storage and workers, but must not start a later wave while an earlier wave
/// still has candidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnrichmentWave {
    CheapMetadata,
    TitleAuthor,
}

impl EnrichmentWave {
    pub const ALL: [Self; 2] = [Self::CheapMetadata, Self::TitleAuthor];

    pub const fn next(self) -> Option<Self> {
        match self {
            Self::CheapMetadata => Some(Self::TitleAuthor),
            Self::TitleAuthor => None,
        }
    }
}

/// Shared decisions; each host supplies its own storage and provider adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupPlan {
    pub subject_complete: bool,
    pub subject_isbns: Vec<String>,
    pub author_isbns: Vec<String>,
    pub needs_page_inspection: bool,
}
impl LookupPlan {
    pub fn from_metadata(metadata: &BookMetadata) -> Self {
        let printed = related_isbn::lookup_candidates(&metadata.identifiers);
        let embedded =
            metadata.identifiers.iter().filter(|id| id.scheme() == &Scheme::Isbn).filter_map(|id| id.canonical_value().as_deref().and_then(from_metadata_value)).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        let isbns = if printed.is_empty() { embedded } else { printed };
        let subject_complete = has_resolved_lcc(metadata);
        Self { needs_page_inspection: !subject_complete && isbns.is_empty(), subject_isbns: if subject_complete { Vec::new() } else { isbns.clone() }, author_isbns: isbns, subject_complete }
    }
}

pub(crate) fn has_resolved_lcc(metadata: &BookMetadata) -> bool {
    metadata.subjects.iter().any(|s| s.authority().is_some_and(|a| a.eq_ignore_ascii_case("lcc")) && s.code().is_some_and(|c| !subject_projection::unified_lcc_subject_paths(c).is_empty()))
}

/// Initial import should defer expensive page work while metadata ISBN lookup
/// is pending. This is NOT subject completion: SubjectPipeline requests page
/// inspection if that lookup finishes without resolved LCC.
pub fn defer_initial_page_inspection(metadata: &BookMetadata) -> bool {
    !LookupPlan::from_metadata(metadata).needs_page_inspection
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usable_metadata_skips_inspection_and_supplies_both_lookups() {
        let mut metadata = BookMetadata::default();
        assert!(LookupPlan::from_metadata(&metadata).needs_page_inspection);
        metadata.identifiers.push(book_model::Identifier::new("0000000000000", Scheme::Isbn, book_model::Scope::Book).unwrap());
        assert!(LookupPlan::from_metadata(&metadata).needs_page_inspection);
        metadata.identifiers.push(book_model::Identifier::new("9781934356555", Scheme::Isbn, book_model::Scope::Book).unwrap());
        let plan = LookupPlan::from_metadata(&metadata);
        assert!(!plan.needs_page_inspection);
        assert_eq!(plan.subject_isbns, ["9781934356555"]);
        assert_eq!(plan.author_isbns, plan.subject_isbns);
    }

    #[test]
    fn whole_values_and_explicit_wrappers() {
        for value in ["978-1-934356-55-5", "978 1 934356 55 5", "urn:isbn:9781934356555", "URN|ISBN|9781934356555"] {
            assert_eq!(from_metadata_value(value).as_deref(), Some("9781934356555"));
        }
        assert_eq!(from_metadata_value("urn:uuid:isbn 0-330-26272-6").as_deref(), Some("0330262726"));
    }
    #[test]
    fn survey_false_positives_are_rejected() {
        for value in [
            "sh2008111148",
            "sh2008106306",
            "7872d3e9-8394-4530-8a39-773505e19c9b",
            "{606DBDD5-6984-4832-9CC6-A289A9696E70}",
            "frus1977-80v27-2025-08-21T21:06:50.413Z",
            "x9780199588503_450.jpg",
            "0000000000000",
            "0000000000",
            "9781934356556",
            "https://example.org/9781934356555",
            "urn:9781934356555",
        ] {
            assert_eq!(from_metadata_value(value), None, "{value}");
        }
    }
}

/// Reserve CPU capacity for interactive work and bound image-decoding memory.
pub fn background_worker_limit(available_cpus: usize) -> usize {
    (available_cpus / 2).clamp(1, 4)
}

pub mod resolution;

mod filename_identifiers;

/// Invalidate checkpoints when subject extraction or persistence changes.
pub const SUBJECT_PIPELINE_REVISION: i64 = 10;

pub mod filename_lookup;
