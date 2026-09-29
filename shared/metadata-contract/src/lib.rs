pub mod wikidata;
pub use wikidata::*;
pub mod audible;
pub mod authority_subjects;
pub mod identity_evidence;
pub mod matching;
pub mod subject_consensus;
pub mod year_consensus;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const MEDIA_TYPE_V2: &str = "application/vnd.bokheim.metadata+protobuf; version=2";
pub const MEDIA_TYPE_V2_BASE: &str = "application/vnd.bokheim.metadata+protobuf";
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ISBNS_PER_REQUEST: usize = 256;
pub const MAX_EDITION_QUERIES_PER_REQUEST: usize = 128;

mod protobuf_v2 {
    // Generated from proto/metadata_v2.proto by prost-build 0.14.4.
    include!("bokheim.metadata.v2.rs");
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClassificationRequest {
    pub isbns: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClassificationResponse {
    pub snapshot: SnapshotDescription,
    pub results: Vec<IsbnClassificationResult>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MetadataEnrichmentRequest {
    pub isbns: Vec<String>,
}

/// Bibliographic evidence for missing ISBNs or classification recovery after
/// an embedded ISBN returns no authority match.
/// The opaque query id lets callers correlate batched results without sending
/// a local book identifier to the metadata service.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EditionIdentityRequest {
    pub queries: Vec<EditionIdentityQuery>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EditionIdentityQuery {
    pub query_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub publishers: Vec<String>,
    pub book_year: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EditionIdentityResponse {
    pub snapshot: SnapshotDescription,
    pub results: Vec<EditionIdentityResult>,
}

/// A classification-only authority match does not assert an ISBN or OL identity.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthoritySubjectMatch {
    #[serde(default)]
    pub main_title: String,
    pub provider_id: String,
    pub record_ids: Vec<String>,
    pub title: String,
    pub authors: Vec<String>,
    pub classifications: Vec<Classification>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EditionIdentityResult {
    pub query_id: String,
    pub status: EditionIdentityStatus,
    /// Present only for a uniquely supported edition identity.
    pub matched: Option<EditionIdentityMatch>,
    /// Bounded alternatives for an ambiguous result. Empty for other statuses.
    #[serde(default)]
    pub candidates: Vec<EditionIdentityMatch>,
    /// True when a common title produced too many candidates to return safely.
    #[serde(default)]
    pub candidates_truncated: bool,
    #[serde(default)]
    pub authority_subjects: Option<AuthoritySubjectMatch>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditionIdentityStatus {
    Matched,
    NoMatch,
    Ambiguous,
    InsufficientMetadata,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EditionIdentityMatch {
    pub canonical_isbn13: String,
    pub open_library_edition_id: String,
    pub open_library_work_id: Option<String>,
    pub title: String,
    pub work_title: Option<String>,
    pub title_match: EditionTitleMatch,
    pub authors: Vec<String>,
    pub publishers: Vec<String>,
    pub book_year: Option<i32>,
    pub matched_publisher: bool,
    pub matched_book_year: bool,
    pub classifications: Vec<Classification>,
    #[serde(default)]
    pub subtitle: Option<String>,
}

impl EditionIdentityMatch {
    pub fn full_title(&self) -> String {
        match self.subtitle.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(subtitle) if !matching::bibliographic_match_key(&self.title).ends_with(&matching::bibliographic_match_key(subtitle)) => format!("{}: {}", self.title, subtitle),
            _ => self.title.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditionTitleMatch {
    Edition,
    Work,
    Both,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MetadataEnrichmentResponse {
    pub snapshot: SnapshotDescription,
    pub results: Vec<IsbnMetadataResult>,
}

/// Rich bibliographic enrichment. This is deliberately a separate wire value
/// from [`MetadataEnrichmentResponse`], so deployed version-1 clients remain
/// byte-compatible.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RichMetadataEnrichmentResponse {
    pub snapshot: SnapshotDescription,
    pub results: Vec<RichIsbnMetadataResult>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RichIsbnMetadataResult {
    #[serde(default)]
    pub wikidata_books: Vec<WikidataBookEvidence>,
    pub requested_isbn: String,
    pub canonical_isbn13: Option<String>,
    pub status: LookupStatus,
    pub matches: Vec<RichWorkMetadataMatch>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RichWorkMetadataMatch {
    pub open_library_work_id: Option<String>,
    pub exact_edition_ids: Vec<String>,
    pub classifications: Vec<Classification>,
    pub authors: Vec<AuthorMetadata>,
    /// Plain-text description from the matched Open Library work.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub subjects: Vec<SubjectHeading>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SubjectHeading {
    pub source: String,
    pub authority: String,
    pub components: Vec<String>,
    pub authority_uri: Option<String>,
}

/// Merges Open Library evidence for every exact ISBN attached to one book.
/// Both the client library and authority enrichment use this policy so an
/// ambiguous ISBN can contribute consensus evidence without being mistaken
/// for a unique edition identity.
pub fn merge_rich_work_matches<'a>(matches: impl IntoIterator<Item = &'a RichWorkMetadataMatch>, similarity_keys: impl Fn(ClassificationScheme, &str) -> Vec<String>) -> RichWorkMetadataMatch {
    let mut work_ids = BTreeSet::new();
    let mut edition_ids = BTreeSet::new();
    let mut classifications = Vec::new();
    let mut authors = BTreeMap::<(u32, String), AuthorMetadata>::new();
    let mut descriptions = BTreeSet::new();
    let mut subjects = BTreeSet::new();
    for matched in matches {
        work_ids.extend(matched.open_library_work_id.iter().cloned());
        edition_ids.extend(matched.exact_edition_ids.iter().cloned());
        classifications.extend(matched.classifications.iter().cloned());
        for author in &matched.authors {
            authors
                .entry((author.position, author.identity_key()))
                .and_modify(|existing| {
                    if existing.name.is_none() {
                        existing.name = author.name.clone();
                    }
                    for identifier in &author.identifiers {
                        if !existing.identifiers.contains(identifier) {
                            existing.identifiers.push(identifier.clone());
                        }
                    }
                    existing.identifiers.sort();
                    existing.source = existing.source.min(author.source);
                })
                .or_insert_with(|| author.clone());
        }
        descriptions.extend(matched.description.iter().cloned());
        subjects.extend(matched.subjects.iter().cloned());
    }
    let classifications = filter_classification_outliers(classifications, similarity_keys);
    RichWorkMetadataMatch {
        open_library_work_id: work_ids.into_iter().next(),
        exact_edition_ids: edition_ids.into_iter().collect(),
        classifications,
        authors: authors.into_values().collect(),
        // Ambiguous ISBNs can point at unrelated works. Only expose a
        // description when all available Open Library evidence agrees.
        description: (descriptions.len() == 1).then(|| descriptions.into_iter().next().expect("one description exists")),
        subjects: subjects.into_iter().collect(),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SnapshotDescription {
    pub dump_date: String,
    pub imported_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IsbnClassificationResult {
    pub requested_isbn: String,
    pub canonical_isbn13: Option<String>,
    pub status: LookupStatus,
    pub matches: Vec<WorkClassificationMatch>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IsbnMetadataResult {
    pub requested_isbn: String,
    pub canonical_isbn13: Option<String>,
    pub status: LookupStatus,
    pub matches: Vec<WorkMetadataMatch>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupStatus {
    Matched,
    NoMatch,
    InvalidIsbn,
    Ambiguous,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkClassificationMatch {
    pub open_library_work_id: Option<String>,
    pub exact_edition_ids: Vec<String>,
    pub classifications: Vec<Classification>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkMetadataMatch {
    pub open_library_work_id: Option<String>,
    pub exact_edition_ids: Vec<String>,
    pub classifications: Vec<Classification>,
    pub authors: Vec<AuthorMetadata>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorMetadata {
    pub open_library_author_id: String,
    pub name: Option<String>,
    pub identifiers: Vec<AuthorIdentifier>,
    pub source: AuthorSource,
    pub position: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AuthorIdentifier {
    pub authority: String,
    pub value: String,
}

impl AuthorMetadata {
    /// Wikidata-only authors have no Open Library identifier.
    pub fn identity_key(&self) -> String {
        if !self.open_library_author_id.is_empty() {
            return format!("openlibrary:{}", self.open_library_author_id);
        }
        self.identifiers.iter().find(|id| id.authority == "wikidata").map(|id| format!("wikidata:{}", id.value)).unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorSource {
    ExactEdition,
    Work,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Classification {
    // Persisted with bincode: even empty fields must retain their position.
    #[serde(default)]
    pub evidence: Vec<ClassificationEvidence>,
    pub scheme: ClassificationScheme,
    pub notation: String,
    pub source: ClassificationSource,
}

/// Provenance for a classification recovered across duplicate work records.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ClassificationEvidence {
    pub method: String,
    pub isbn13: String,
    pub open_library_work_id: String,
    pub open_library_edition_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationScheme {
    LibraryOfCongress,
    /// Dewey Decimal classification.
    DeweyDecimal,
    Bisac,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationSource {
    ExactEdition,
    Work,
}

/// Normalizes a bibliographic call number to the stable class notation used
/// for cross-edition consensus. LCC cutters and item years must not create
/// separate votes. Dewey segmentation marks are normalized.
pub fn classification_consensus_notation(scheme: ClassificationScheme, notation: &str) -> Option<String> {
    match scheme {
        ClassificationScheme::DeweyDecimal => {
            let value = notation.trim().replace('/', "");
            let bytes = value.as_bytes();
            let valid = bytes.len() >= 3 && bytes[..3].iter().all(u8::is_ascii_digit) && (bytes.len() == 3 || (bytes[3] == b'.' && bytes.len() > 4 && bytes[4..].iter().all(u8::is_ascii_digit)));
            valid.then_some(value)
        }
        ClassificationScheme::LibraryOfCongress => {
            let value = notation.trim();
            let bytes = value.as_bytes();
            let mut cursor = 0;
            while cursor < bytes.len() && cursor < 3 && bytes[cursor].is_ascii_alphabetic() {
                cursor += 1;
            }
            if cursor == 0 || bytes.get(cursor).is_some_and(u8::is_ascii_alphabetic) {
                return None;
            }
            let letters = value[..cursor].to_ascii_uppercase();
            while bytes.get(cursor).is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'\'') {
                cursor += 1;
            }
            let number_start = cursor;
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
            if cursor == number_start {
                return None;
            }
            if bytes.get(cursor) == Some(&b'.') {
                let decimal_start = cursor + 1;
                let mut decimal_end = decimal_start;
                while bytes.get(decimal_end).is_some_and(u8::is_ascii_digit) {
                    decimal_end += 1;
                }
                if decimal_end > decimal_start {
                    cursor = decimal_end;
                }
            }
            Some(format!("{letters}{}", &value[number_start..cursor]))
        }
        ClassificationScheme::Bisac => {
            let value = notation.trim().to_ascii_uppercase();
            (value.len() == 9 && value.as_bytes()[..3].iter().all(u8::is_ascii_alphabetic) && value.as_bytes()[3..].iter().all(u8::is_ascii_digit)).then_some(value)
        }
    }
}

/// Accumulates exact-edition classification support across alternative ISBN
/// matches without allowing a repeated work-wide union to manufacture votes.
#[derive(Debug, Default)]
pub struct EditionClassificationConsensus {
    support: BTreeMap<(ClassificationScheme, String), BTreeSet<String>>,
}

impl EditionClassificationConsensus {
    /// Removes exact-edition classifications from alternative matches and
    /// records each normalized classification once per independently queried
    /// edition group. Work-level evidence remains on the matches.
    pub fn record_alternative_matches(&mut self, matches: &mut [RichWorkMetadataMatch], requested_isbn: &str) {
        for matched in matches {
            let edition_group = if matched.exact_edition_ids.is_empty() {
                format!("isbn:{requested_isbn}")
            } else {
                let mut edition_ids = matched.exact_edition_ids.clone();
                edition_ids.sort();
                edition_ids.dedup();
                edition_ids.join(",")
            };
            matched.classifications.retain(|classification| {
                if classification.source == ClassificationSource::ExactEdition {
                    if let Some(notation) = classification_consensus_notation(classification.scheme, &classification.notation) {
                        self.support.entry((classification.scheme, notation)).or_default().insert(edition_group.clone());
                    }
                    false
                } else {
                    true
                }
            });
            for author in &mut matched.authors {
                if author.source == AuthorSource::ExactEdition {
                    author.source = AuthorSource::Work;
                }
            }
        }
    }

    /// Returns normalized classifications supported by at least two distinct
    /// edition groups.
    pub fn classifications(self) -> Vec<Classification> {
        self.support.into_iter().filter_map(|((scheme, notation), edition_groups)| (edition_groups.len() >= 2).then_some(Classification { evidence: Vec::new(), scheme, notation, source: ClassificationSource::Work })).collect()
    }
}

/// Returns normalized classifications supported by at least two exact
/// editions. Duplicate formatting variants within one edition count once.
pub fn exact_edition_classification_consensus<'a, E, C>(editions: E) -> Vec<Classification>
where
    E: IntoIterator<Item = C>,
    C: IntoIterator<Item = &'a (ClassificationScheme, String)>,
{
    let mut support = BTreeMap::<(ClassificationScheme, String), usize>::new();
    for classifications in editions {
        let normalized = classifications.into_iter().filter_map(|(scheme, notation)| classification_consensus_notation(*scheme, notation).map(|notation| (*scheme, notation))).collect::<BTreeSet<_>>();
        for classification in normalized {
            *support.entry(classification).or_default() += 1;
        }
    }
    support.into_iter().filter_map(|((scheme, notation), votes)| (votes >= 2).then_some(Classification { evidence: Vec::new(), scheme, notation, source: ClassificationSource::Work })).collect()
}

/// Keeps classification consensus across editions while removing small,
/// distant taxonomy neighborhoods from a sufficiently large evidence set.
/// Exact-edition classifications are always retained. Unmapped values are
/// retained conservatively.
pub fn filter_classification_outliers(classifications: impl IntoIterator<Item = Classification>, similarity_keys: impl Fn(ClassificationScheme, &str) -> Vec<String>) -> Vec<Classification> {
    let mut evidence = BTreeMap::<(ClassificationScheme, String), BTreeSet<ClassificationEvidence>>::new();
    let mut unique = BTreeMap::<(ClassificationScheme, String), ClassificationSource>::new();
    for classification in classifications {
        evidence.entry((classification.scheme, classification.notation.clone())).or_default().extend(classification.evidence);
        unique.entry((classification.scheme, classification.notation)).and_modify(|source| *source = (*source).min(classification.source)).or_insert(classification.source);
    }

    let mut classification_keys = BTreeMap::<(ClassificationScheme, String), Vec<String>>::new();
    let mut family_counts = BTreeMap::<String, usize>::new();
    let mut mapped_total = 0_usize;
    for (scheme, notation) in unique.keys() {
        let keys = similarity_keys(*scheme, notation).into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        if !keys.is_empty() {
            mapped_total += 1;
            for key in &keys {
                *family_counts.entry(key.clone()).or_default() += 1;
            }
        }
        classification_keys.insert((*scheme, notation.clone()), keys);
    }

    let strongest = family_counts.values().copied().max().unwrap_or_default();
    let minimum_support = 2_usize.max(mapped_total.div_ceil(20));

    unique
        .into_iter()
        .filter_map(|((scheme, notation), source)| {
            let keep = if source == ClassificationSource::ExactEdition {
                true
            } else {
                let keys = classification_keys.get(&(scheme, notation.clone())).map(Vec::as_slice).unwrap_or_default();
                keys.is_empty() || mapped_total < 6 || strongest < 2 || keys.iter().any(|key| family_counts.get(key).copied().unwrap_or_default() >= minimum_support)
            };
            keep.then(|| Classification { evidence: evidence.remove(&(scheme, notation.clone())).unwrap_or_default().into_iter().collect(), scheme, notation, source })
        })
        .collect()
}

#[derive(Debug, Eq, PartialEq)]
pub enum WireError {
    TooLarge { limit: usize },
    Encode(String),
    Decode(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(formatter, "metadata body exceeds the {limit}-byte limit"),
            Self::Encode(reason) => write!(formatter, "could not encode metadata body: {reason}"),
            Self::Decode(reason) => write!(formatter, "could not decode metadata body: {reason}"),
        }
    }
}

impl std::error::Error for WireError {}

pub fn encode_v2_request(value: &MetadataEnrichmentRequest) -> Result<Vec<u8>, WireError> {
    let message = protobuf_v2::EnrichmentRequest { isbns: value.isbns.clone() };
    encode_protobuf(&message)
}

pub fn decode_v2_request(body: &[u8], limit: usize) -> Result<MetadataEnrichmentRequest, WireError> {
    check_size(body, limit)?;
    let message = protobuf_v2::EnrichmentRequest::decode(body).map_err(|error| WireError::Decode(error.to_string()))?;
    Ok(MetadataEnrichmentRequest { isbns: message.isbns })
}

pub fn encode_v2_response(value: &RichMetadataEnrichmentResponse) -> Result<Vec<u8>, WireError> {
    encode_protobuf(&protobuf_v2::EnrichmentResponse::from(value))
}

pub fn decode_v2_response(body: &[u8], limit: usize) -> Result<RichMetadataEnrichmentResponse, WireError> {
    check_size(body, limit)?;
    protobuf_v2::EnrichmentResponse::decode(body).map_err(|error| WireError::Decode(error.to_string()))?.try_into()
}

pub fn encode_v2_edition_identity_request(value: &EditionIdentityRequest) -> Result<Vec<u8>, WireError> {
    encode_protobuf(&protobuf_v2::EditionIdentityRequest {
        queries: value
            .queries
            .iter()
            .map(|query| protobuf_v2::EditionIdentityQuery { query_id: query.query_id.clone(), title: query.title.clone(), authors: query.authors.clone(), publishers: query.publishers.clone(), book_year: query.book_year })
            .collect(),
    })
}

pub fn decode_v2_edition_identity_request(body: &[u8], limit: usize) -> Result<EditionIdentityRequest, WireError> {
    check_size(body, limit)?;
    let value = protobuf_v2::EditionIdentityRequest::decode(body).map_err(|error| WireError::Decode(error.to_string()))?;
    Ok(EditionIdentityRequest {
        queries: value.queries.into_iter().map(|query| EditionIdentityQuery { query_id: query.query_id, title: query.title, authors: query.authors, publishers: query.publishers, book_year: query.book_year }).collect(),
    })
}

pub fn encode_v2_edition_identity_response(value: &EditionIdentityResponse) -> Result<Vec<u8>, WireError> {
    encode_protobuf(&protobuf_v2::EditionIdentityResponse {
        snapshot: Some(protobuf_v2::Snapshot { dump_date: value.snapshot.dump_date.clone(), imported_at_ms: value.snapshot.imported_at_ms }),
        results: value.results.iter().map(protobuf_edition_identity_result).collect(),
    })
}

pub fn decode_v2_edition_identity_response(body: &[u8], limit: usize) -> Result<EditionIdentityResponse, WireError> {
    check_size(body, limit)?;
    let value = protobuf_v2::EditionIdentityResponse::decode(body).map_err(|error| WireError::Decode(error.to_string()))?;
    let snapshot = value.snapshot.ok_or_else(|| WireError::Decode("protobuf edition identity response omitted snapshot".to_owned()))?;
    Ok(EditionIdentityResponse { snapshot: SnapshotDescription { dump_date: snapshot.dump_date, imported_at_ms: snapshot.imported_at_ms }, results: value.results.into_iter().map(domain_edition_identity_result).collect::<Result<_, _>>()? })
}

fn protobuf_edition_identity_result(value: &EditionIdentityResult) -> protobuf_v2::EditionIdentityResult {
    protobuf_v2::EditionIdentityResult {
        query_id: value.query_id.clone(),
        status: match value.status {
            EditionIdentityStatus::Matched => 1,
            EditionIdentityStatus::NoMatch => 2,
            EditionIdentityStatus::Ambiguous => 3,
            EditionIdentityStatus::InsufficientMetadata => 4,
        },
        matched: value.matched.as_ref().map(protobuf_edition_identity_match),
        candidates: value.candidates.iter().map(protobuf_edition_identity_match).collect(),
        authority_subjects: value.authority_subjects.as_ref().map(|m| protobuf_v2::AuthoritySubjectMatch {
            main_title: m.main_title.clone(),
            provider_id: m.provider_id.clone(),
            record_ids: m.record_ids.clone(),
            title: m.title.clone(),
            authors: m.authors.clone(),
            classifications: m.classifications.iter().map(protobuf_classification).collect(),
        }),
        candidates_truncated: value.candidates_truncated,
    }
}

fn protobuf_edition_identity_match(value: &EditionIdentityMatch) -> protobuf_v2::EditionIdentityMatch {
    protobuf_v2::EditionIdentityMatch {
        canonical_isbn13: value.canonical_isbn13.clone(),
        open_library_edition_id: value.open_library_edition_id.clone(),
        open_library_work_id: value.open_library_work_id.clone(),
        title: value.title.clone(),
        work_title: value.work_title.clone(),
        subtitle: value.subtitle.clone(),
        title_match: match value.title_match {
            EditionTitleMatch::Edition => 1,
            EditionTitleMatch::Work => 2,
            EditionTitleMatch::Both => 3,
        },
        authors: value.authors.clone(),
        publishers: value.publishers.clone(),
        book_year: value.book_year,
        matched_publisher: value.matched_publisher,
        matched_book_year: value.matched_book_year,
        classifications: value.classifications.iter().map(protobuf_classification).collect(),
    }
}

fn protobuf_classification(value: &Classification) -> protobuf_v2::Classification {
    protobuf_v2::Classification {
        evidence: value.evidence.iter().map(protobuf_evidence).collect(),
        scheme: match value.scheme {
            ClassificationScheme::LibraryOfCongress => 1,
            ClassificationScheme::DeweyDecimal => 2,
            ClassificationScheme::Bisac => 3,
        },
        notation: value.notation.clone(),
        source: match value.source {
            ClassificationSource::ExactEdition => 1,
            ClassificationSource::Work => 2,
        },
    }
}

fn domain_classification(value: protobuf_v2::Classification) -> Result<Classification, WireError> {
    Ok(Classification {
        evidence: value.evidence.into_iter().map(domain_evidence).collect(),
        scheme: match value.scheme {
            1 => ClassificationScheme::LibraryOfCongress,
            2 => ClassificationScheme::DeweyDecimal,
            3 => ClassificationScheme::Bisac,
            scheme => return Err(invalid_enum("classification scheme", scheme)),
        },
        notation: value.notation,
        source: match value.source {
            1 => ClassificationSource::ExactEdition,
            2 => ClassificationSource::Work,
            source => return Err(invalid_enum("classification source", source)),
        },
    })
}

fn domain_edition_identity_result(value: protobuf_v2::EditionIdentityResult) -> Result<EditionIdentityResult, WireError> {
    Ok(EditionIdentityResult {
        query_id: value.query_id,
        status: match value.status {
            1 => EditionIdentityStatus::Matched,
            2 => EditionIdentityStatus::NoMatch,
            3 => EditionIdentityStatus::Ambiguous,
            4 => EditionIdentityStatus::InsufficientMetadata,
            status => return Err(invalid_enum("edition identity status", status)),
        },
        matched: value.matched.map(domain_edition_identity_match).transpose()?,
        candidates: value.candidates.into_iter().map(domain_edition_identity_match).collect::<Result<_, _>>()?,
        authority_subjects: value
            .authority_subjects
            .map(|m| -> Result<AuthoritySubjectMatch, WireError> {
                Ok(AuthoritySubjectMatch {
                    main_title: m.main_title,
                    provider_id: m.provider_id,
                    record_ids: m.record_ids,
                    title: m.title,
                    authors: m.authors,
                    classifications: m.classifications.into_iter().map(domain_classification).collect::<Result<_, _>>()?,
                })
            })
            .transpose()?,
        candidates_truncated: value.candidates_truncated,
    })
}

fn domain_edition_identity_match(value: protobuf_v2::EditionIdentityMatch) -> Result<EditionIdentityMatch, WireError> {
    Ok(EditionIdentityMatch {
        canonical_isbn13: value.canonical_isbn13,
        open_library_edition_id: value.open_library_edition_id,
        open_library_work_id: value.open_library_work_id,
        title: value.title,
        work_title: value.work_title,
        subtitle: value.subtitle,
        title_match: match value.title_match {
            1 => EditionTitleMatch::Edition,
            2 => EditionTitleMatch::Work,
            3 => EditionTitleMatch::Both,
            title_match => return Err(invalid_enum("edition title match", title_match)),
        },
        authors: value.authors,
        publishers: value.publishers,
        book_year: value.book_year,
        matched_publisher: value.matched_publisher,
        matched_book_year: value.matched_book_year,
        classifications: value.classifications.into_iter().map(domain_classification).collect::<Result<_, _>>()?,
    })
}

fn encode_protobuf(message: &impl Message) -> Result<Vec<u8>, WireError> {
    let mut body = Vec::with_capacity(message.encoded_len());
    message.encode(&mut body).map_err(|error| WireError::Encode(error.to_string()))?;
    Ok(body)
}

fn check_size(body: &[u8], limit: usize) -> Result<(), WireError> {
    if body.len() > limit {
        Err(WireError::TooLarge { limit })
    } else {
        Ok(())
    }
}

pub fn is_binary_media_type_v2(value: &str) -> bool {
    let mut fields = value.split(';');
    if !fields.next().is_some_and(|value| value.trim().eq_ignore_ascii_case(MEDIA_TYPE_V2_BASE)) {
        return false;
    }
    fields.any(|field| field.split_once('=').is_some_and(|(name, value)| name.trim().eq_ignore_ascii_case("version") && value.trim() == "2"))
}

impl From<&RichMetadataEnrichmentResponse> for protobuf_v2::EnrichmentResponse {
    fn from(value: &RichMetadataEnrichmentResponse) -> Self {
        Self { snapshot: Some(protobuf_v2::Snapshot { dump_date: value.snapshot.dump_date.clone(), imported_at_ms: value.snapshot.imported_at_ms }), results: value.results.iter().map(protobuf_result).collect() }
    }
}

fn protobuf_result(value: &RichIsbnMetadataResult) -> protobuf_v2::IsbnResult {
    protobuf_v2::IsbnResult {
        wikidata_books: value.wikidata_books.clone(),
        requested_isbn: value.requested_isbn.clone(),
        canonical_isbn13: value.canonical_isbn13.clone(),
        status: lookup_status_number(value.status),
        matches: value.matches.iter().map(protobuf_match).collect(),
    }
}

fn protobuf_match(value: &RichWorkMetadataMatch) -> protobuf_v2::WorkMatch {
    protobuf_v2::WorkMatch {
        open_library_work_id: value.open_library_work_id.clone(),
        exact_edition_ids: value.exact_edition_ids.clone(),
        classifications: value
            .classifications
            .iter()
            .map(|classification| protobuf_v2::Classification {
                evidence: classification.evidence.iter().map(protobuf_evidence).collect(),
                scheme: match classification.scheme {
                    ClassificationScheme::LibraryOfCongress => 1,
                    ClassificationScheme::DeweyDecimal => 2,
                    ClassificationScheme::Bisac => 3,
                },
                notation: classification.notation.clone(),
                source: match classification.source {
                    ClassificationSource::ExactEdition => 1,
                    ClassificationSource::Work => 2,
                },
            })
            .collect(),
        authors: value
            .authors
            .iter()
            .map(|author| protobuf_v2::Author {
                open_library_author_id: author.open_library_author_id.clone(),
                name: author.name.clone(),
                identifiers: author.identifiers.iter().map(|identifier| protobuf_v2::AuthorIdentifier { authority: identifier.authority.clone(), value: identifier.value.clone() }).collect(),
                source: match author.source {
                    AuthorSource::ExactEdition => 1,
                    AuthorSource::Work => 2,
                },
                position: author.position,
            })
            .collect(),
        description: value.description.clone(),
        subjects: value
            .subjects
            .iter()
            .map(|subject| protobuf_v2::SubjectHeading { source: subject.source.clone(), authority: subject.authority.clone(), components: subject.components.clone(), authority_uri: subject.authority_uri.clone() })
            .collect(),
    }
}

fn lookup_status_number(value: LookupStatus) -> i32 {
    match value {
        LookupStatus::Matched => 1,
        LookupStatus::NoMatch => 2,
        LookupStatus::InvalidIsbn => 3,
        LookupStatus::Ambiguous => 4,
    }
}

impl TryFrom<protobuf_v2::EnrichmentResponse> for RichMetadataEnrichmentResponse {
    type Error = WireError;

    fn try_from(value: protobuf_v2::EnrichmentResponse) -> Result<Self, Self::Error> {
        let snapshot = value.snapshot.ok_or_else(|| WireError::Decode("protobuf response omitted snapshot".to_owned()))?;
        Ok(Self { snapshot: SnapshotDescription { dump_date: snapshot.dump_date, imported_at_ms: snapshot.imported_at_ms }, results: value.results.into_iter().map(domain_result).collect::<Result<_, _>>()? })
    }
}

fn domain_result(value: protobuf_v2::IsbnResult) -> Result<RichIsbnMetadataResult, WireError> {
    Ok(RichIsbnMetadataResult {
        wikidata_books: value.wikidata_books,
        requested_isbn: value.requested_isbn,
        canonical_isbn13: value.canonical_isbn13,
        status: domain_lookup_status(value.status)?,
        matches: value.matches.into_iter().map(domain_match).collect::<Result<_, _>>()?,
    })
}

fn domain_match(value: protobuf_v2::WorkMatch) -> Result<RichWorkMetadataMatch, WireError> {
    Ok(RichWorkMetadataMatch {
        open_library_work_id: value.open_library_work_id,
        exact_edition_ids: value.exact_edition_ids,
        classifications: value
            .classifications
            .into_iter()
            .map(|classification| {
                Ok(Classification {
                    evidence: classification.evidence.into_iter().map(domain_evidence).collect(),
                    scheme: match classification.scheme {
                        1 => ClassificationScheme::LibraryOfCongress,
                        2 => ClassificationScheme::DeweyDecimal,
                        3 => ClassificationScheme::Bisac,
                        value => return Err(invalid_enum("classification scheme", value)),
                    },
                    notation: classification.notation,
                    source: match classification.source {
                        1 => ClassificationSource::ExactEdition,
                        2 => ClassificationSource::Work,
                        value => return Err(invalid_enum("classification source", value)),
                    },
                })
            })
            .collect::<Result<_, _>>()?,
        authors: value
            .authors
            .into_iter()
            .map(|author| {
                Ok(AuthorMetadata {
                    open_library_author_id: author.open_library_author_id,
                    name: author.name,
                    identifiers: author.identifiers.into_iter().map(|identifier| AuthorIdentifier { authority: identifier.authority, value: identifier.value }).collect(),
                    source: match author.source {
                        1 => AuthorSource::ExactEdition,
                        2 => AuthorSource::Work,
                        value => return Err(invalid_enum("author source", value)),
                    },
                    position: author.position,
                })
            })
            .collect::<Result<_, _>>()?,
        description: value.description,
        subjects: value.subjects.into_iter().map(|subject| SubjectHeading { source: subject.source, authority: subject.authority, components: subject.components, authority_uri: subject.authority_uri }).collect(),
    })
}

fn domain_lookup_status(value: i32) -> Result<LookupStatus, WireError> {
    match value {
        1 => Ok(LookupStatus::Matched),
        2 => Ok(LookupStatus::NoMatch),
        3 => Ok(LookupStatus::InvalidIsbn),
        4 => Ok(LookupStatus::Ambiguous),
        value => Err(invalid_enum("lookup status", value)),
    }
}

fn invalid_enum(name: &str, value: i32) -> WireError {
    WireError::Decode(format!("protobuf {name} has unsupported value {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifications_round_trip_through_binary_storage() {
        for scheme in [ClassificationScheme::LibraryOfCongress, ClassificationScheme::DeweyDecimal, ClassificationScheme::Bisac] {
            for evidence in [Vec::new(), vec![ClassificationEvidence { method: "exact".to_owned(), isbn13: "9780306406157".to_owned(), open_library_work_id: "OL10W".to_owned(), open_library_edition_id: "OL1M".to_owned() }]] {
                let value = Classification { evidence, scheme, notation: "QA76.73".to_owned(), source: ClassificationSource::Work };
                let bytes = wire::encode(&value).unwrap();
                assert_eq!(wire::decode::<Classification>(&bytes, MAX_RESPONSE_BYTES).unwrap(), value);
            }
        }
    }

    #[test]
    fn classification_consensus_drops_small_distant_families_but_keeps_exact_evidence() {
        let mut classifications = (1..=8).map(|number| Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: format!("PA40{number}"), source: ClassificationSource::Work }).collect::<Vec<_>>();
        classifications.extend([
            Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "B580".to_owned(), source: ClassificationSource::ExactEdition },
            Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "PQ3434".to_owned(), source: ClassificationSource::Work },
            Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "HQ75.6".to_owned(), source: ClassificationSource::Work },
        ]);

        let filtered = filter_classification_outliers(classifications, |_, notation| {
            let key = if notation.starts_with("PA") {
                "Humanities / Literature / Classical Greek & Latin Literatures"
            } else if notation.starts_with("B") {
                "Humanities / Philosophy / History, Traditions & Regions"
            } else if notation.starts_with("PQ") {
                "Humanities / Literature / Romance-Language Literatures"
            } else {
                "Social Sciences / Society / Social Topics"
            };
            vec![key.to_owned()]
        });
        assert_eq!(filtered.len(), 9);
        assert!(filtered.iter().any(|classification| classification.notation == "B580"));
        assert!(!filtered.iter().any(|classification| classification.notation == "PQ3434"));
        assert!(!filtered.iter().any(|classification| classification.notation == "HQ75.6"));
    }

    #[test]
    fn exact_edition_consensus_keeps_multiple_supported_branches_per_scheme() {
        let editions = [
            [(ClassificationScheme::LibraryOfCongress, "QA76.6".to_owned()), (ClassificationScheme::DeweyDecimal, "005.1".to_owned())].into_iter().collect::<BTreeSet<_>>(),
            [(ClassificationScheme::LibraryOfCongress, "QA76.6.A1 2001".to_owned()), (ClassificationScheme::DeweyDecimal, "005.1".to_owned())].into_iter().collect(),
            [(ClassificationScheme::LibraryOfCongress, "PN1993".to_owned()), (ClassificationScheme::DeweyDecimal, "791.43".to_owned())].into_iter().collect(),
            [(ClassificationScheme::LibraryOfCongress, "PN1993.A2".to_owned()), (ClassificationScheme::DeweyDecimal, "791.43".to_owned())].into_iter().collect(),
            [(ClassificationScheme::LibraryOfCongress, "B580".to_owned()), (ClassificationScheme::DeweyDecimal, "999".to_owned())].into_iter().collect(),
        ];

        let retained = exact_edition_classification_consensus(editions.iter().map(BTreeSet::iter));
        let notations = retained.iter().map(|classification| classification.notation.as_str()).collect::<BTreeSet<_>>();
        assert!(notations.contains("QA76.6"));
        assert!(notations.contains("PN1993"));
        assert!(notations.contains("005.1"));
        assert!(notations.contains("791.43"));
        assert!(!notations.contains("B580"));
        assert!(!notations.contains("999"));
    }

    #[test]
    fn classification_consensus_normalizes_item_specific_call_numbers() {
        assert_eq!(classification_consensus_notation(ClassificationScheme::LibraryOfCongress, "PA4025.A5 R6 1999").as_deref(), Some("PA4025"));
        assert_eq!(classification_consensus_notation(ClassificationScheme::LibraryOfCongress, "PA4025.A5W56 2018eb").as_deref(), Some("PA4025"));
        assert_eq!(classification_consensus_notation(ClassificationScheme::DeweyDecimal, "883/.01").as_deref(), Some("883.01"));
    }

    #[test]
    fn alternative_edition_consensus_removes_singletons_and_demotes_other_evidence() {
        let matched = |work: &str, edition: &str, notation: &str| RichWorkMetadataMatch {
            open_library_work_id: Some(work.to_owned()),
            exact_edition_ids: vec![edition.to_owned()],
            classifications: vec![Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: notation.to_owned(), source: ClassificationSource::ExactEdition }],
            authors: vec![AuthorMetadata { open_library_author_id: "OL1A".to_owned(), name: None, identifiers: Vec::new(), source: AuthorSource::ExactEdition, position: 0 }],
            description: None,
            subjects: Vec::new(),
        };
        let mut consensus = EditionClassificationConsensus::default();
        let mut first = vec![matched("OL1W", "OL1M", "PR4034.P7 1995")];
        let mut second = vec![matched("OL2W", "OL2M", "PR4034 .P7 2006")];
        let mut singleton = vec![matched("OL3W", "OL3M", "PS3558.A4758 P75t 1999")];
        consensus.record_alternative_matches(&mut first, "9780000000001");
        consensus.record_alternative_matches(&mut second, "9780000000002");
        consensus.record_alternative_matches(&mut singleton, "9780000000003");

        assert!(first[0].classifications.is_empty());
        assert_eq!(first[0].authors[0].source, AuthorSource::Work);
        assert_eq!(consensus.classifications(), vec![Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "PR4034".to_owned(), source: ClassificationSource::Work }]);
    }

    #[test]
    fn rich_match_merging_combines_split_work_records() {
        let first = RichWorkMetadataMatch {
            open_library_work_id: Some("OL1W".to_owned()),
            exact_edition_ids: vec!["OL1M".to_owned()],
            classifications: vec![Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "GN496".to_owned(), source: ClassificationSource::ExactEdition }],
            authors: Vec::new(),
            description: None,
            subjects: Vec::new(),
        };
        let second = RichWorkMetadataMatch {
            open_library_work_id: Some("OL2W".to_owned()),
            exact_edition_ids: vec!["OL2M".to_owned()],
            classifications: vec![Classification { evidence: Vec::new(), scheme: ClassificationScheme::DeweyDecimal, notation: "306".to_owned(), source: ClassificationSource::Work }],
            authors: Vec::new(),
            description: None,
            subjects: Vec::new(),
        };

        let merged = merge_rich_work_matches([&first, &second], |_, _| Vec::new());
        assert_eq!(merged.exact_edition_ids, ["OL1M", "OL2M"]);
        assert_eq!(merged.classifications.len(), 2);
        assert!(merged.classifications.iter().any(|c| c.scheme == ClassificationScheme::DeweyDecimal));
    }

    #[test]
    fn protobuf_v2_round_trip_preserves_rich_match() {
        let value = RichMetadataEnrichmentResponse {
            snapshot: SnapshotDescription { dump_date: "2026-08-01".to_owned(), imported_at_ms: 42 },
            results: vec![RichIsbnMetadataResult {
                wikidata_books: Vec::new(),
                requested_isbn: "9780306406157".to_owned(),
                canonical_isbn13: Some("9780306406157".to_owned()),
                status: LookupStatus::Matched,
                matches: vec![RichWorkMetadataMatch {
                    open_library_work_id: Some("OL10W".to_owned()),
                    exact_edition_ids: vec!["OL1M".to_owned()],
                    classifications: vec![
                        Classification { evidence: Vec::new(), scheme: ClassificationScheme::DeweyDecimal, notation: "883".to_owned(), source: ClassificationSource::Work },
                        Classification { evidence: Vec::new(), scheme: ClassificationScheme::Bisac, notation: "HIS015000".to_owned(), source: ClassificationSource::Work },
                    ],
                    authors: vec![AuthorMetadata {
                        open_library_author_id: "OL20A".to_owned(),
                        name: Some("Alex Smith".to_owned()),
                        identifiers: vec![AuthorIdentifier { authority: "viaf".to_owned(), value: "123".to_owned() }],
                        source: AuthorSource::Work,
                        position: 0,
                    }],
                    description: Some("A description from Open Library.".to_owned()),
                    subjects: vec![SubjectHeading { source: "library_of_congress".to_owned(), authority: "lcsh".to_owned(), components: vec!["Books".to_owned(), "History".to_owned()], authority_uri: None }],
                }],
            }],
        };
        let mut encoded = encode_v2_response(&value).unwrap();
        // A future scalar field 99 must be ignored by this version.
        encoded.extend_from_slice(&[0x98, 0x06, 0x01]);
        let decoded = decode_v2_response(&encoded, MAX_RESPONSE_BYTES).unwrap();
        assert_eq!(decoded.snapshot.dump_date, "2026-08-01");
        assert_eq!(decoded.results[0].matches[0].classifications, value.results[0].matches[0].classifications);
        assert_eq!(decoded.results[0].matches[0].authors, value.results[0].matches[0].authors);
        assert_eq!(decoded.results[0].matches[0].description, value.results[0].matches[0].description);

        let request = MetadataEnrichmentRequest { isbns: vec!["9780306406157".to_owned()] };
        assert_eq!(decode_v2_request(&encode_v2_request(&request).unwrap(), MAX_REQUEST_BYTES).unwrap().isbns, request.isbns);
    }

    #[test]
    fn protobuf_v2_round_trip_preserves_edition_review_candidates() {
        let candidate = EditionIdentityMatch {
            subtitle: None,
            canonical_isbn13: "9780306406157".to_owned(),
            open_library_edition_id: "OL1M".to_owned(),
            open_library_work_id: Some("OL10W".to_owned()),
            title: "Edition title".to_owned(),
            work_title: Some("Work title".to_owned()),
            title_match: EditionTitleMatch::Work,
            authors: vec!["Alex Smith".to_owned()],
            publishers: vec!["North Wind Press".to_owned()],
            book_year: Some(2004),
            matched_publisher: true,
            matched_book_year: false,
            classifications: vec![Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "QA76.6".to_owned(), source: ClassificationSource::Work }],
        };
        let value = EditionIdentityResponse {
            snapshot: SnapshotDescription { dump_date: "2026-08-01".to_owned(), imported_at_ms: 42 },
            results: vec![EditionIdentityResult { authority_subjects: None, query_id: "book-1".to_owned(), status: EditionIdentityStatus::Ambiguous, matched: None, candidates: vec![candidate.clone()], candidates_truncated: true }],
        };
        let decoded = decode_v2_edition_identity_response(&encode_v2_edition_identity_response(&value).unwrap(), MAX_RESPONSE_BYTES).unwrap();
        assert_eq!(decoded.results[0].status, EditionIdentityStatus::Ambiguous);
        assert_eq!(decoded.results[0].candidates[0].title_match, EditionTitleMatch::Work);
        assert_eq!(decoded.results[0].candidates[0].work_title, candidate.work_title);
        assert_eq!(decoded.results[0].candidates[0].classifications, candidate.classifications);
        assert!(decoded.results[0].candidates_truncated);

        let request = EditionIdentityRequest { queries: vec![EditionIdentityQuery { query_id: "book-1".to_owned(), title: "Work title".to_owned(), authors: vec!["Alex Smith".to_owned()], publishers: Vec::new(), book_year: Some(2004) }] };
        let decoded = decode_v2_edition_identity_request(&encode_v2_edition_identity_request(&request).unwrap(), MAX_REQUEST_BYTES).unwrap();
        assert_eq!(decoded.queries[0].title, "Work title");
    }
}

fn protobuf_evidence(value: &ClassificationEvidence) -> protobuf_v2::ClassificationEvidence {
    protobuf_v2::ClassificationEvidence { method: value.method.clone(), isbn13: value.isbn13.clone(), open_library_work_id: value.open_library_work_id.clone(), open_library_edition_id: value.open_library_edition_id.clone() }
}
fn domain_evidence(value: protobuf_v2::ClassificationEvidence) -> ClassificationEvidence {
    ClassificationEvidence { method: value.method, isbn13: value.isbn13, open_library_work_id: value.open_library_work_id, open_library_edition_id: value.open_library_edition_id }
}
