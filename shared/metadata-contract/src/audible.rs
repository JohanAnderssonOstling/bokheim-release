//! Recording evidence shared by scanning, Audible lookup and chapter alignment.
//! Recording release dates remain separate from original book dates.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingEvidence {
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub narrators: Vec<String>,
    #[serde(default)]
    pub asins: Vec<String>,
    #[serde(default)]
    pub isbns: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    /// Raw file date: it must not be promoted to a recording release date.
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub recording_release_date: Option<String>,
    /// Four-digit recording release year, when the source supplies only a year.
    #[serde(default)]
    pub recording_release_year: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// The backend sends bibliographic evidence, never local paths or content hashes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupRequest {
    pub title: String,
    pub authors: Vec<String>,
    pub duration_ms: u64,
    pub recording: RecordingEvidence,
    pub chapters: Vec<Chapter>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub asin: String,
    pub region: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub narrators: Vec<String>,
    pub publisher: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub release_date: Option<String>,
    pub isbns: Vec<String>,
    pub abridged: Option<bool>,
    pub provider_duration_ms: Option<u64>,
    /// Measured from the provider chapter tree, not rounded provider minutes.
    pub duration_ms: Option<u64>,
    pub chapters: Vec<Chapter>,
    pub source: DiscoverySource,
    /// Provider provenance is independent of how the ASIN was discovered.
    #[serde(default = "default_metadata_provider")]
    pub metadata_provider: String,
    #[serde(default)]
    pub chapter_provider: Option<String>,
}

fn default_metadata_provider() -> String {
    "audible".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySource {
    EmbeddedAsin,
    Api,
    Website,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LookupResponse {
    pub status: LookupStatus,
    pub candidates: Vec<Candidate>,
    pub selected_asin: Option<String>,
    pub selected_region: Option<String>,
    pub chapter_plan: Option<ChapterPlan>,
    pub used_website_fallback: bool,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupStatus {
    NoMatch,
    Unavailable,
    Ambiguous,
    Supported,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChapterPlan {
    pub method: AlignmentMethod,
    pub chapters: Vec<Chapter>,
    pub renamed: usize,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentMethod {
    EqualCountNames,
    SectionAnchors,
    DurationPattern,
    ImportedBoundaries,
}

pub mod selection;

pub const MAX_LOOKUP_REQUEST_BYTES: usize = 2 * 1024 * 1024;

pub mod chapters;

/// Bump whenever lookup or chapter policy requires reevaluating durable results.
pub const POLICY_VERSION: &str = "audible_v2";
