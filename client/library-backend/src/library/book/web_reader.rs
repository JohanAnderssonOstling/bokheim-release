//! Browser book-opening policy over library commands.

use crate::library::{PendingCommand, ReadyCommand};

use std::future::Future;

use crate::library::PlaybackLocation::Direct;
use crate::library::ResolvedBook;
use crate::BookId;
pub use book_access::remote_file::BrowserReader as Reader;
use book_access::{FetchFuture, SparseBookSource};
use book_model::BookFormat;
pub use client_platform_web::transport::range::connect;

use crate::library::commands::{LibraryCommand, LibraryReply};
use crate::{BackendError, BookSource, ResolvedBookData};
use client_platform_web::transport::media::RemoteMedia;
use client_runtime::Failure;
use wasm_bindgen::JsValue;

/// Library-scoped command transport supplied by the application host.
pub trait BookTransport: Clone + 'static {
    fn request<T: LibraryReply>(&self, command: LibraryCommand) -> impl Future<Output = Result<T, BackendError>>;
    fn subscribe_book(&self, content_hash: BookId) -> impl Future<Output = Result<(async_channel::Receiver<()>, ResolvedBookData), BackendError>>;
}

/// Keep the subscription alive for every in-flight read as well as the fetcher.
fn range_fetch<T: BookTransport>(library: T, lease: Option<async_channel::Receiver<()>>, command: impl Fn(u64, usize) -> LibraryCommand + 'static) -> impl Fn(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, String>> {
    move |offset, length| {
        let library = library.clone();
        let lease = lease.clone();
        let command = command(offset, length);
        Box::pin(async move {
            let _lease = lease;
            let bytes: serde_bytes::ByteBuf = library.request(command).await.map_err(|error| error.to_string())?;
            Ok(bytes.into_vec())
        })
    }
}

async fn audiobook<T: BookTransport>(source: crate::library::PlaybackSource, lease: Option<async_channel::Receiver<()>>, library: T, content_hash: BookId) -> Result<ResolvedBook, BackendError> {
    let renewal_library = library.clone();
    let renew = matches!(&source.location, Direct(_)).then(|| {
        client_platform_web::transport::resources::AsyncCallback::new(move || {
            let library = renewal_library.clone();
            async move {
                let direct: crate::library::DirectPlayback = library.request(LibraryCommand::Pending(PendingCommand::RenewRemoteAudio { content_hash })).await.map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
                let json = serde_json::to_string(&direct).map_err(|error| wasm_bindgen::JsValue::from_str(&format!("Invalid direct playback configuration: {error}")))?;
                js_sys::JSON::parse(&json)
            }
        })
    });
    let mut ranges = Vec::new();
    let (url, direct, local_tracks) = match source.location {
        crate::library::PlaybackLocation::LocalUrl(url) => (url, None, Vec::new()),
        crate::library::PlaybackLocation::LocalTracks(tracks) => (String::new(), None, tracks),
        Direct(direct) => (direct.url.clone(), Some(direct), Vec::new()),
        crate::library::PlaybackLocation::Ranges(file) => {
            if source.metadata.tracks.is_empty() {
                let media = remote_media(library, file.clone(), 0, file.length, "audio/mp4", lease.clone()).await?;
                let url = media.url();
                ranges.push(media);
                (url, None, Vec::new())
            } else {
                let mut urls = Vec::with_capacity(source.metadata.tracks.len());
                for track in &source.metadata.tracks {
                    if !track.valid() || track.archive_hash != Some(file.hash) || track.offset.checked_add(track.length).is_none_or(|end| end > file.length) {
                        return Err("audiobook track exceeds its provider archive".into());
                    }
                    let media = remote_media(library.clone(), file.clone(), track.offset, track.length, "audio/mpeg", lease.clone()).await?;
                    urls.push(media.url());
                    ranges.push(media);
                }
                (String::new(), None, urls)
            }
        }
    };
    Ok(ResolvedBook::Audiobook(crate::library::browser_audiobook::BrowserAudiobook::new(source.metadata, url, local_tracks, direct, lease, renew).with_ranges(ranges)))
}

async fn remote_media<T: BookTransport>(library: T, source: crate::library::RemoteFile, base: u64, length: u64, content_type: &str, lease: Option<async_channel::Receiver<()>>) -> Result<RemoteMedia, BackendError> {
    RemoteMedia::new(length, content_type, move |offset, length| {
        let library = library.clone();
        let source = source.clone();
        let lease = lease.clone();
        async move {
            let _lease = lease;
            let offset = base.checked_add(offset).ok_or_else(|| JsValue::from_str("invalid audiobook offset"))?;
            let bytes: serde_bytes::ByteBuf = library.request(LibraryCommand::Pending(PendingCommand::ReadFileRange { source, offset, length })).await.map_err(media_range_error)?;
            Ok(bytes.into_vec())
        }
    })
    .await
    .map_err(|error| BackendError::message(format!("Could not prepare browser audio: {error:?}")))
}

fn media_range_error(error: BackendError) -> JsValue {
    let retryable = matches!(error.failure(), Some(Failure::Offline | Failure::TimedOut | Failure::ServiceUnavailable | Failure::RateLimited));
    let value = js_sys::Error::new(&error.to_string());
    let _ = js_sys::Reflect::set(value.as_ref(), &"retryable".into(), &retryable.into());
    value.into()
}

pub async fn resolve_book<T: BookTransport>(library: &T, content_hash: BookId) -> Result<ResolvedBook, BackendError> {
    if let Some(source) = library.request(LibraryCommand::Pending(PendingCommand::OpenRemoteAudio { content_hash })).await? {
        return audiobook(source, None, library.clone(), content_hash).await;
    }
    let (lease, resolved) = crate::executor::timeout(std::time::Duration::from_secs(65), library.subscribe_book(content_hash)).await.map_err(|_| BackendError::classified(Failure::TimedOut, "opening the book source timed out"))??;
    if matches!(resolved.format, BookFormat::Epub | BookFormat::Mobi | BookFormat::Fb2 | BookFormat::Cbz) {
        let source = match resolved.source {
            BookSource::Local { capability, length } => {
                let fetch = range_fetch(library.clone(), Some(lease), move |offset, length| LibraryCommand::Ready(ReadyCommand::ReadBookRange { capability: capability.clone(), offset, length }));
                book_access::AsyncBookSource::browser(length, Vec::new(), fetch)?
            }
            BookSource::RemoteArchive { source, tail } if matches!(resolved.format, BookFormat::Epub | BookFormat::Cbz) => {
                let range_source = source.clone();
                let fetch = range_fetch(library.clone(), None, move |offset, length| LibraryCommand::Pending(PendingCommand::ReadFileRange { source: range_source.clone(), offset, length }));
                let seed_offset = source.length.checked_sub(tail.len() as u64).ok_or("invalid ZIP tail")?;
                let sparse = SparseBookSource::browser(source.length, seed_offset, tail, fetch)?;
                let batch_library = library.clone();
                let batch_source = source.clone();
                let sparse = sparse.with_browser_prefetch(move |ranges| {
                    let library = batch_library.clone();
                    let source = batch_source.clone();
                    Box::pin(async move {
                        let bytes: serde_bytes::ByteBuf = library.request(LibraryCommand::Pending(PendingCommand::ReadFileRanges { source, ranges })).await.map_err(|error| error.to_string())?;
                        Ok(bytes.into_vec())
                    })
                });
                if resolved.format == BookFormat::Cbz {
                    book_access::AsyncBookSource::from(sparse)
                } else {
                    super::remote_archive::prepare_epub(sparse).await?
                }
            }
            BookSource::Remote { source, prefix } => {
                let length = source.length;
                let fetch = range_fetch(library.clone(), Some(lease), move |offset, length| LibraryCommand::Pending(PendingCommand::ReadFileRange { source: source.clone(), offset, length }));
                book_access::AsyncBookSource::browser(length, prefix, fetch)?
            }
            _ => return Err("browser received an invalid book source".into()),
        };
        return Ok(ResolvedBook::Document(crate::library::DocumentBook::from_async_source(String::new(), resolved.format, source)));
    }
    let mut pdf_page_source = None;
    let reader: crate::BoxedBookReader = match resolved.source {
        BookSource::Audiobook(source) => return audiobook(source, Some(lease), library.clone(), content_hash).await,
        BookSource::Local { capability, length } => {
            let fetch = range_fetch(library.clone(), Some(lease), move |offset, length| LibraryCommand::Ready(ReadyCommand::ReadBookRange { capability: capability.clone(), offset, length }));
            book_access::remote_file::browser_reader(length, Vec::new(), fetch)?
        }
        BookSource::Remote { source, prefix } => {
            let library = library.clone();
            let index = super::remote_pdf::index(resolved.pdf_metadata.as_ref(), content_hash, source.length);
            if index.is_some() || (resolved.format == BookFormat::Pdf && source.revision.is_none()) {
                let multipart = source.revision.is_none();
                let length = source.length;
                let fetch = book_access::remote_file::browser_bundle_fetch(move |ranges| {
                    let library = library.clone();
                    let source = source.clone();
                    Box::pin(async move {
                        let command = if multipart { PendingCommand::ReadFileRanges { source, ranges } } else { PendingCommand::ReadFileBundle { source, ranges } };
                        let bytes: serde_bytes::ByteBuf = library.request(LibraryCommand::Pending(command)).await.map_err(|error| error.to_string())?;
                        Ok(bytes.into_vec())
                    })
                })?;
                if let Some(index) = index {
                    let (reader, hook) = book_access::pdf_bundle::open(index, prefix, fetch).map_err(|error| error.to_string())?;
                    pdf_page_source = Some(hook);
                    reader
                } else {
                    book_access::pdf_bundle::open_unindexed(length, prefix, fetch).map_err(|error| error.to_string())?
                }
            } else {
                let length = source.length;
                let fetch = range_fetch(library, None, move |offset, length| LibraryCommand::Pending(PendingCommand::ReadFileRange { source: source.clone(), offset, length }));
                book_access::remote_file::browser_reader(length, prefix, fetch)?
            }
        }
        BookSource::RemoteArchive { .. } => return Err("browser received a ZIP source for another format".into()),
        BookSource::Inline(_) => return Err("browser received bytes instead of a book source".into()),
    };
    let mut document = crate::library::DocumentBook::new(String::new(), resolved.format, reader);
    document.pdf_metadata = resolved.pdf_metadata;
    document.pdf_page_source = pdf_page_source;
    Ok(ResolvedBook::Document(document))
}
