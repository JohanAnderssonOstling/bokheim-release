//! Browser fixtures for production remote ZIP readers and shared range batching.
#![cfg(target_arch = "wasm32")]

use book_access::remote_file;
use book_content::{comic::ComicBook, Book, BookFormat};
use book_model::BookFormat as ContainerFormat;
use library_backend::{
    library::{
        commands::LibraryReply,
        web_reader::{resolve_book, BookTransport},
        LibraryCommand, PendingCommand,
    },
    BackendError, BookId, BookSource, ResolvedBookData,
};
use serde_json::Value;
use wasm_bindgen::prelude::*;

#[derive(Clone)]
struct Transport {
    client: reqwest::Client,
    url: reqwest::Url,
    format: ContainerFormat,
}

impl BookTransport for Transport {
    async fn request<T: LibraryReply>(&self, command: LibraryCommand) -> Result<T, BackendError> {
        let value = match command {
            LibraryCommand::Pending(PendingCommand::OpenRemoteAudio { .. }) => Value::Null,
            LibraryCommand::Pending(PendingCommand::ReadFileRange { source, offset, length }) => {
                let bytes = remote_file::fetch_range(&self.client, self.url.clone(), &source, "fixture", offset, length).await.map_err(BackendError::operation)?;
                serde_json::to_value(serde_bytes::ByteBuf::from(bytes)).map_err(BackendError::operation)?
            }
            LibraryCommand::Pending(PendingCommand::ReadFileRanges { source, ranges }) => {
                let bytes = remote_file::fetch_ranges(&self.client, self.url.clone(), &source, "fixture", ranges).await.map_err(BackendError::operation)?;
                serde_json::to_value(serde_bytes::ByteBuf::from(bytes)).map_err(BackendError::operation)?
            }
            _ => return Err(BackendError::message("unexpected browser EPUB command")),
        };
        serde_json::from_value(value).map_err(BackendError::operation)
    }

    async fn subscribe_book(&self, hash: BookId) -> Result<(async_channel::Receiver<()>, ResolvedBookData), BackendError> {
        let (source, tail) = remote_file::describe_tail(&self.client, self.url.clone(), "fixture", hash).await.map_err(BackendError::operation)?;
        let resolved = ResolvedBookData { format: self.format, pdf_metadata: None, source: BookSource::RemoteArchive { source, tail } };
        // Exercise the worker wire representation as well as the reader path.
        let resolved = serde_json::from_slice(&serde_json::to_vec(&resolved).map_err(BackendError::operation)?).map_err(BackendError::operation)?;
        let (_, lease) = async_channel::unbounded();
        Ok((lease, resolved))
    }
}

#[wasm_bindgen(js_name = checkEpubOpening)]
pub async fn check(url: String) -> Result<String, JsValue> {
    let result = async {
        let transport = Transport { client: reqwest::Client::new(), url: url.parse().map_err(BackendError::operation)?, format: ContainerFormat::Epub };
        let book = resolve_book(&transport, BookId::new(&"d".repeat(64))).await?.into_document()?;
        let library_backend::library::DocumentSource::Async(source) = book.source else { return Err(BackendError::message("expected asynchronous EPUB")) };
        let book = Book::open(BookFormat::Epub, move || source.reader()).await.map_err(BackendError::operation)?.prepare_reading().await.map_err(BackendError::operation)?;
        assert_eq!(book.document_uris().len(), 1);
        assert_eq!(book.resources.read_bytes(&book.document_uris()[0]).unwrap(), b"<html><body>Exact remote chapter</body></html>");
        assert_eq!(book.resources.read_bytes("notes.xhtml").unwrap(), b"<html><body>Auxiliary notes</body></html>");
        assert_eq!(book.resources.read_bytes("style.css").unwrap(), b"body { color: black; }");
        Ok::<_, BackendError>(serde_json::json!({"documents": book.document_uris(), "eager_text": true}).to_string())
    }
    .await;
    result.map_err(|error| JsValue::from_str(&error.to_string()))
}

#[wasm_bindgen(js_name = checkCbzOpening)]
pub async fn check_cbz(url: String, deleted: bool) -> Result<String, JsValue> {
    let result = async {
        let transport = Transport { client: reqwest::Client::new(), url: url.parse().map_err(BackendError::operation)?, format: ContainerFormat::Cbz };
        let book = resolve_book(&transport, BookId::new(&"d".repeat(64))).await?.into_document()?;
        let library_backend::library::DocumentSource::Async(source) = book.source else { return Err(BackendError::message("expected asynchronous CBZ")) };
        assert!(source.supports_prefetch());
        let prefetch = source.clone();
        let book = ComicBook::open_with_prefetch(
            move || source.reader(),
            1,
            move |ranges| {
                let source = prefetch.clone();
                async move { source.prefetch(ranges).await }
            },
        )
        .await
        .map_err(BackendError::operation)?;
        assert_eq!(book.metadata.title, "Remote comic");
        let reader = book.into_reader().await.map_err(BackendError::operation)?;
        let expected = if deleted { 10 } else { 2 };
        for _ in 0..2 {
            let bytes = reader.read_page(1).await.map_err(BackendError::operation)?;
            assert_eq!(bytes, vec![expected; 512 * 1024]);
        }
        Ok::<_, BackendError>(serde_json::json!({"resumed_page": 1, "pages": reader.pages.len(), "cached_reread": true}).to_string())
    }
    .await;
    result.map_err(|error| JsValue::from_str(&error.to_string()))
}
