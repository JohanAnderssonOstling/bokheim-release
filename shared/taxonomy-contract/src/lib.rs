use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const API_VERSION: &str = "v1";
pub const SNAPSHOT_MEDIA_TYPE: &str = "application/vnd.bokheim.taxonomy+sqlite3; version=1";
pub const MAX_CODE_BYTES: usize = 256;
pub const MAX_CODES_PER_REQUEST: usize = 512;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaxonomySystem {
    Bisac,
    Lcc,
}

impl TaxonomySystem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bisac => "bisac",
            Self::Lcc => "lcc",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyDescription {
    pub api_version: String,
    pub release_id: u64,
    pub format_version: u32,
    pub taxonomy_name: String,
    pub concept_count: u64,
    pub edge_count: u64,
    pub selector_count: u64,
    pub root_count: u64,
    pub snapshot_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyParent {
    pub concept_id: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyConcept {
    pub concept_id: i64,
    pub preferred_label: String,
    pub parents: Vec<TaxonomyParent>,
    pub source_selectors: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomySlice {
    pub release_id: u64,
    pub requested_concept_ids: Vec<i64>,
    /// Parent concepts always precede their descendants.
    pub concepts: Vec<TaxonomyConcept>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyResolution {
    pub release_id: u64,
    pub system: TaxonomySystem,
    pub code: String,
    pub matched_concept_ids: Vec<i64>,
    /// Ancestor-closed and ordered with parents before descendants.
    pub concepts: Vec<TaxonomyConcept>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TaxonomyCode {
    pub system: TaxonomySystem,
    pub code: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyBatchRequest {
    /// Locally unsupported codes collected across one assignment operation.
    pub codes: Vec<TaxonomyCode>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyCodeResolution {
    pub system: TaxonomySystem,
    pub code: String,
    pub matched_concept_ids: Vec<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaxonomyBatchResponse {
    pub release_id: u64,
    /// One entry per unique requested system/code pair, in deterministic order.
    pub results: Vec<TaxonomyCodeResolution>,
    /// The shared ancestor-closed union for every result, without duplication.
    pub concepts: Vec<TaxonomyConcept>,
}
