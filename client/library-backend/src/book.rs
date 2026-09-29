//! Serializable book sources produced by a library session.

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct ResolvedBookData {
    pub pdf_metadata: Option<pdf_view_common::PdfReaderMetadata>,
    pub format: book_model::BookFormat,
    pub source: BookSource,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum BookSource {
    Local {
        capability: String,
        length: u64,
    },
    Remote {
        source: crate::library::RemoteFile,
        #[serde(with = "serde_bytes")]
        prefix: Vec<u8>,
    },
    Audiobook(crate::library::PlaybackSource),
    /// Native wire adapters can still return owned bytes; browsers use capabilities.
    Inline(#[serde(with = "serde_bytes")] Vec<u8>),
}
