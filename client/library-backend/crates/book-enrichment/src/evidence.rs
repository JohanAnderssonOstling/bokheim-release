use serde::{Deserialize, Serialize};
pub const MAX_SECTION_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IsbnScope {
    ElectronicEdition,
    RelatedPrintEdition,
    Unspecified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsbnEvidence {
    pub isbn: String,
    pub scope: IsbnScope,
    pub format: Option<String>,
}

/// Evidence is anchored to one page/section, never joined across unrelated pages.
/// Related/unspecified ISBNs are candidates for verified authority lookup, not identities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionEvidence {
    pub location: String,
    pub accepted: bool,
    pub reason: String,
    pub isbns: Vec<IsbnEvidence>,
    pub lcc: Vec<String>,
}
