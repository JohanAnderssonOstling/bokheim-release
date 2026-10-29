//! Shared book-opening policy. Hosts provide local readers and supported
//! streaming mechanisms; metadata selection and remote access are common.

use crate::BackendError;
use crate::BoxedBookReader;
use crate::{BookId, LibrarySession};
use book_model::BookFormat;
use client_platform_runtime::startup_profile::StartupProfile;
use library_database::LocalBookInfo;

#[cfg(target_arch = "wasm32")]
pub(crate) mod browser;

#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod host;
#[cfg(target_arch = "wasm32")]
#[path = "web.rs"]
mod host;
mod remote_archive;
mod remote_pdf;
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
}
pub struct DocumentBook {
    pub pdf_page_source: Option<std::sync::Arc<dyn pdf_view_common::PdfPageSource>>,
    pub pdf_metadata: Option<pdf_view_common::PdfReaderMetadata>,
    pub path: String,
    pub format: BookFormat,
    pub source: DocumentSource,
}
impl DocumentBook {
    pub fn new(path: String, format: BookFormat, reader: BoxedBookReader) -> Self {
        Self { pdf_page_source: None, pdf_metadata: None, path, format, source: DocumentSource::Reader(reader) }
    }
}

/// Text books and comics share asynchronous sources; PDF retains its seekable input.
pub enum DocumentSource {
    Reader(BoxedBookReader),
    Async(book_access::AsyncBookSource),
}

impl DocumentSource {
    pub fn into_reader(self) -> Result<BoxedBookReader, String> {
        match self {
            Self::Reader(reader) => Ok(reader),
            Self::Async(_) => Err("expected a synchronous document reader".into()),
        }
    }
}

impl DocumentBook {
    pub fn from_async_source(path: String, format: BookFormat, source: book_access::AsyncBookSource) -> Self {
        Self { pdf_page_source: None, pdf_metadata: None, path, format, source: DocumentSource::Async(source) }
    }
}

struct RemoteDocument {
    source: crate::library::RemoteFile,
    prefix: Vec<u8>,
    pdf_metadata: Option<pdf_view_common::PdfReaderMetadata>,
}
async fn remote_document(library: &LibrarySession, hash: BookId) -> Result<RemoteDocument, BackendError> {
    let (source, prefix) = library.describe_remote_file(hash).await.map_err(BackendError::operation)?;
    let pdf_metadata = library.db.pdf_metadata(&hash)?;
    library.remote_book_access(&source)?;
    Ok(RemoteDocument { source, prefix, pdf_metadata })
}

impl LibrarySession {
    pub(crate) async fn prepare_book_for_reader(&self, hash: BookId) -> Result<Opened, BackendError> {
        let mut profile = StartupProfile::new("book_lookup_resolve");
        let info = self.db.local_book_info(&hash)?;
        profile.mark("local_book_info");
        if let Some(book) = host::local(self, hash, &info).await? {
            profile.mark("local_source");
            return Ok(book);
        }
        host::remote(self, hash, &info).await
    }
}

#[cfg(target_arch = "wasm32")]
pub mod web_reader;
