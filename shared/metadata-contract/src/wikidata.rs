//! Source evidence is separate from accepted author identity identifiers.
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
pub struct WikidataBookEvidence {
    #[prost(string, tag = "1")]
    pub wikidata_id: String,
    #[prost(string, repeated, tag = "2")]
    pub work_ids: Vec<String>,
    #[prost(string, optional, tag = "3")]
    pub title: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub description: Option<String>,
    #[prost(message, repeated, tag = "5")]
    pub authors: Vec<WikidataAuthorEvidence>,
    #[prost(message, repeated, tag = "6")]
    pub classifications: Vec<WikidataClassificationEvidence>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
pub struct WikidataAuthorEvidence {
    #[prost(string, tag = "1")]
    pub wikidata_id: String,
    #[prost(string, optional, tag = "2")]
    pub name: Option<String>,
    #[prost(string, repeated, tag = "3")]
    pub aliases: Vec<String>,
    #[prost(sint32, optional, tag = "4")]
    pub birth_year: Option<i32>,
    #[prost(sint32, optional, tag = "5")]
    pub death_year: Option<i32>,
    #[prost(message, repeated, tag = "6")]
    pub identifiers: Vec<WikidataAuthorityId>,
    #[prost(string, optional, tag = "7")]
    pub image: Option<String>,
    #[prost(string, tag = "8")]
    pub source_entity: String,
    /// unlinked, authority, corroborated, provisional, ambiguous, or conflict.
    #[prost(string, tag = "9")]
    pub identity_match: String,
    #[prost(string, optional, tag = "10")]
    pub open_library_author_id: Option<String>,
    #[prost(uint32, tag = "11")]
    pub shared_work_count: u32,
}
#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
pub struct WikidataAuthorityId {
    #[prost(string, tag = "1")]
    pub authority: String,
    #[prost(string, tag = "2")]
    pub value: String,
}
#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
pub struct WikidataClassificationEvidence {
    #[prost(string, tag = "1")]
    pub scheme: String,
    #[prost(string, tag = "2")]
    pub notation: String,
    #[prost(string, tag = "3")]
    pub source_entity: String,
    #[prost(string, tag = "4")]
    pub property: String,
    #[prost(bool, tag = "5")]
    pub inferred: bool,
}
