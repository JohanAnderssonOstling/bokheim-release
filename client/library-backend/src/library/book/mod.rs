//! Shared book-opening policy. Hosts provide local readers and supported
//! streaming mechanisms; metadata selection and download fallback are common.
use crate::BackendError;
use crate::{ContentHash, DownloadState, LibrarySession};
use book_model::BookFormat;

#[cfg(target_arch = "wasm32")]
pub(crate) mod browser;

#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod host;
#[cfg(target_arch = "wasm32")]
#[path = "web.rs"]
mod host;
pub use host::AudiobookBook;
pub(crate) use host::Opened;

/// Opening a document and opening audio produce different usable resources.
pub enum ResolvedBook {
    Document(DocumentBook),
    Audiobook(AudiobookBook),
}
impl ResolvedBook {
    pub fn format(&self) -> BookFormat {
        match self {
            Self::Document(book) => book.format,
            #[cfg(not(target_arch = "wasm32"))]
            Self::Audiobook(book) => book.format,
            #[cfg(target_arch = "wasm32")]
            Self::Audiobook(book) => book.metadata.format,
        }
    }
    pub fn into_document(self) -> Result<DocumentBook, String> {
        match self {
            Self::Document(book) => Ok(book),
            Self::Audiobook(_) => Err("expected a document".into()),
        }
    }
    pub fn into_audiobook(self) -> Result<AudiobookBook, String> {
        match self {
            Self::Audiobook(book) => Ok(book),
            Self::Document(_) => Err("expected an audiobook".into()),
        }
    }
}
pub struct DocumentBook {
    pub pdf_page_source: Option<std::sync::Arc<dyn pdf_view_common::PdfPageSource>>,
    pub pdf_metadata: Option<pdf_view_common::PdfReaderMetadata>,
    pub path: String,
    pub format: BookFormat,
    pub reader: crate::BoxedBookReader,
}
impl DocumentBook {
    pub fn new(path: String, format: BookFormat, reader: crate::BoxedBookReader) -> Self {
        Self { pdf_page_source: None, pdf_metadata: None, path, format, reader }
    }
}

struct BookInfo {
    format: BookFormat,
    path: Option<String>,
}
async fn book_info(library: &LibrarySession, hash: ContentHash) -> Result<BookInfo, BackendError> {
    let info = library.db.local_book_info(&hash)?;
    Ok(BookInfo { format: info.format, path: info.path })
}

struct RemoteDocument {
    source: crate::library::RemoteFile,
    prefix: Vec<u8>,
    pdf_metadata: Option<pdf_view_common::PdfReaderMetadata>,
}
async fn pdf_metadata(library: &LibrarySession, hash: ContentHash, checksum: ContentHash) -> Result<Option<pdf_view_common::PdfReaderMetadata>, BackendError> {
    library.db.pdf_metadata(&hash, &checksum).map_err(Into::into)
}
async fn remote_document(library: &LibrarySession, hash: ContentHash) -> Result<RemoteDocument, BackendError> {
    let (source, prefix) = library.describe_remote_file(hash).await.map_err(BackendError::operation)?;
    let pdf_metadata = pdf_metadata(library, hash, source.checksum).await?;
    Ok(RemoteDocument { source, prefix, pdf_metadata })
}

impl LibrarySession {
    pub(crate) async fn prepare_book_for_reader(&self, hash: ContentHash) -> Result<Opened, BackendError> {
        let info = book_info(self, hash).await?;
        if let Some(book) = host::local(self, hash, &info).await? {
            return Ok(book);
        }
        if host::supports_streaming(info.format) {
            return host::remote(self, hash, &info).await;
        }
        match self.request_download(hash).await? {
            DownloadState::Downloaded => host::local(self, hash, &info).await?.ok_or_else(|| BackendError::message("book is unavailable")),
            DownloadState::NotDownloaded => Err(BackendError::message(format!("book is unavailable for download: {hash}"))),
            DownloadState::Queued | DownloadState::Downloading(_) => Err(BackendError::message(format!("book download did not complete: {hash}"))),
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub mod web_reader;
