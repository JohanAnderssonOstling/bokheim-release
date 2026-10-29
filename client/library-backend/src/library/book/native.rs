//! Native book readers, filesystem availability, and playback resources.

use super::*;
use crate::library::DocumentBook;
use crate::library::ResolvedBook;
use crate::DownloadState;
use book_access::{FetchFuture, RemoteFileError};
pub(crate) type Opened = ResolvedBook;

pub(super) async fn local(library: &LibrarySession, hash: BookId, info: &LocalBookInfo) -> Result<Option<Opened>, BackendError> {
    let mut profile = StartupProfile::new("book_lookup_local");
    let state = library.persistent_download_state(hash)?;
    profile.mark("download_state");
    if !matches!(state, DownloadState::Downloaded) {
        return Ok(None);
    }
    if info.format == BookFormat::Mp3Folder {
        return library.prepare_audiobook_track(hash, 0).await.map(Some);
    }
    let mut path = info.path.clone().ok_or_else(|| BackendError::message("book placement not found"))?;
    let format = info.format;
    if !matches!(format, BookFormat::Epub | BookFormat::Pdf | BookFormat::Mobi | BookFormat::M4b | BookFormat::Fb2 | BookFormat::Cbz) {
        return Err(format!("the {} format is no longer supported", format.canonical_extension()).into());
    }
    if matches!(format, BookFormat::Epub | BookFormat::Mobi | BookFormat::Fb2 | BookFormat::Cbz) {
        profile.mark("format_and_path");
        let mut source = library.assets.open_async_book_at_path(path.clone()).await.map_err(BackendError::operation)?;
        profile.mark("open_async_source");
        if source.is_none() {
            if let Some(fresh) = refresh_local_path(library, hash).await? {
                path = fresh;
                source = library.assets.open_async_book_at_path(path.clone()).await.map_err(BackendError::operation)?;
            }
        }
        let source = source.ok_or_else(|| "book content is not downloaded")?;
        return Ok(Some(ResolvedBook::Document(DocumentBook::from_async_source(path.to_string(), format, source))));
    }
    #[cfg(target_os = "android")]
    let playback_file = if format == BookFormat::M4b { library.assets.open_playback_file(&path)? } else { None };
    #[cfg(target_os = "android")]
    let reader = if let Some(file) = &playback_file { Some(Box::new(file.try_clone()?) as crate::BoxedBookReader) } else { library.assets.open_book_at_path(path.clone()).await? };
    #[cfg(not(target_os = "android"))]
    let mut reader = library.assets.open_book_at_path(path.clone()).await.map_err(BackendError::operation)?;
    #[cfg(not(target_os = "android"))]
    if reader.is_none() {
        if let Some(fresh) = refresh_local_path(library, hash).await? {
            path = fresh;
            reader = library.assets.open_book_at_path(path.clone()).await.map_err(BackendError::operation)?;
        }
    }
    let reader = reader.ok_or_else(|| "book content is not downloaded")?;
    let book = if format == BookFormat::M4b {
        ResolvedBook::Audiobook(crate::library::AudiobookBook {
            format,
            path: path.to_string(),
            reader,
            stream_control: None,
            #[cfg(target_os = "android")]
            playback_file,
        })
    } else {
        let metadata = if format == BookFormat::Pdf { library.db.local_pdf_metadata(&hash, path.as_str())? } else { None };
        let mut document = DocumentBook::new(path.to_string(), format, reader);
        document.pdf_metadata = metadata;
        ResolvedBook::Document(document)
    };
    Ok(Some(book))
}

// A placement move can land between reading `info.path` and opening it.
// Drain this hash's pending work and retry once against the fresh path.
async fn refresh_local_path(library: &LibrarySession, content_hash: BookId) -> Result<Option<library_model::RelativeBookPath>, BackendError> {
    let _ = library_placement::run_file_jobs_for(&library.assets, &library.db, &content_hash).await;
    Ok(library.db.local_book_info(&content_hash)?.path)
}

pub(super) async fn remote(library: &LibrarySession, hash: BookId, info: &LocalBookInfo) -> Result<Opened, BackendError> {
    if info.format == BookFormat::Mp3Folder {
        return library.prepare_audiobook_track(hash, 0).await;
    }
    let path = info.path.clone().ok_or_else(|| BackendError::message("book placement not found"))?;
    if matches!(info.format, BookFormat::Epub | BookFormat::Cbz) && library.db.storage_policy()?.external().is_none() {
        return super::remote_archive::open(library, hash, path.to_string(), info.format).await;
    }
    let RemoteDocument { source, prefix, pdf_metadata } = remote_document(library, hash).await?;
    let length = source.length;
    let access = library.remote_book_access(&source)?;
    if info.format == BookFormat::Pdf && library.db.storage_policy()?.external().is_none() {
        let fetch = book_access::remote_file::native_bundle_fetch(access.batch_fetch()).map_err(BackendError::operation)?;
        let (reader, page_source) = if let Some(index) = super::remote_pdf::index(pdf_metadata.as_ref(), hash, length) {
            let (reader, source) = book_access::pdf_bundle::open(index, prefix, fetch).map_err(BackendError::operation)?;
            (reader, Some(source))
        } else {
            (book_access::pdf_bundle::open_unindexed(length, prefix, fetch).map_err(BackendError::operation)?, None)
        };
        let mut book = DocumentBook::new(path.to_string(), info.format, reader);
        book.pdf_page_source = page_source;
        book.pdf_metadata = pdf_metadata;
        return Ok(ResolvedBook::Document(book));
    }
    if info.format == BookFormat::M4b {
        let (reader, stream_control) = book_access::remote_file::native_audio_reader(length, prefix, access.fetch()).map_err(BackendError::operation)?;
        return Ok(ResolvedBook::Audiobook(AudiobookBook {
            format: info.format,
            path: path.to_string(),
            reader,
            stream_control: Some(stream_control),
            #[cfg(target_os = "android")]
            playback_file: None,
        }));
    }
    if matches!(info.format, BookFormat::Epub | BookFormat::Mobi | BookFormat::Fb2 | BookFormat::Cbz) {
        let source = access.async_source(length, prefix)?;
        return Ok(ResolvedBook::Document(DocumentBook::from_async_source(path.to_string(), info.format, source)));
    }
    let reader = book_access::remote_file::native_reader(length, prefix, access.fetch()).map_err(BackendError::operation)?;
    let mut document = DocumentBook::new(path.to_string(), info.format, reader);
    document.pdf_metadata = pdf_metadata;
    Ok(ResolvedBook::Document(document))
}

impl LibrarySession {
    pub(crate) async fn prepare_audiobook_track(&self, hash: BookId, index: usize) -> Result<ResolvedBook, BackendError> {
        let info = self.db.local_book_info(&hash)?;
        if info.format != BookFormat::Mp3Folder {
            return Err(BackendError::message("book is not an MP3 folder"));
        }
        let provider_file = self.db.provider_file(&hash)?;
        let track = self.db.audiobook_tracks(&hash)?.into_iter().nth(index).ok_or_else(|| BackendError::message("audiobook track is unavailable"))?;
        if !track.valid() {
            return Err(BackendError::message("audiobook track index is invalid"));
        }
        let mut path = info.path.ok_or_else(|| BackendError::message("audiobook placement is unavailable"))?;
        if matches!(self.persistent_download_state(hash)?, DownloadState::Downloaded) {
            let mut reader = self.assets.open_audiobook_track(path.clone(), track.name.clone()).await.map_err(BackendError::operation)?;
            if reader.is_none() {
                if let Some(fresh) = refresh_local_path(self, hash).await? {
                    path = fresh;
                    reader = self.assets.open_audiobook_track(path.clone(), track.name.clone()).await.map_err(BackendError::operation)?;
                }
            }
            if let Some(reader) = reader {
                return Ok(ResolvedBook::Audiobook(AudiobookBook {
                    format: BookFormat::Mp3Folder,
                    path: path.to_string(),
                    reader,
                    stream_control: None,
                    #[cfg(target_os = "android")]
                    playback_file: None,
                }));
            }
        }
        let source = match provider_file {
            Some(file) => crate::library::RemoteFile { hash, length: file.size_bytes, revision: Some(file.revision.as_str().to_owned()) },
            None => self.describe_remote_file(hash).await.map_err(BackendError::operation)?.0,
        };
        if track.archive_hash != Some(source.hash) {
            return Err(BackendError::message("audiobook track index does not match the remote archive; sync the book metadata and retry"));
        }
        if track.offset.checked_add(track.length).is_none_or(|end| end > source.length) {
            return Err(BackendError::message("audiobook track exceeds remote archive"));
        }
        let access = self.remote_book_access(&source)?;
        let prefix = access.read(track.offset, track.length.min(64 * 1024) as usize).await.map_err(BackendError::operation)?;
        let base = track.offset;
        let length = track.length;
        let fetch = move |offset: u64, count: usize| {
            let access = access.clone();
            Box::pin(async move {
                if offset.checked_add(count as u64).is_none_or(|end| end > length) {
                    return Err(RemoteFileError::Failed("audiobook track range is invalid".into()));
                }
                access.read(base + offset, count).await
            }) as FetchFuture<'static, Result<Vec<u8>, RemoteFileError>>
        };
        let (reader, stream_control) = book_access::remote_file::native_audio_reader(length, prefix, fetch).map_err(BackendError::operation)?;
        Ok(ResolvedBook::Audiobook(AudiobookBook {
            format: BookFormat::Mp3Folder,
            path: path.to_string(),
            reader,
            stream_control: Some(stream_control),
            #[cfg(target_os = "android")]
            playback_file: None,
        }))
    }
}

pub struct AudiobookBook {
    pub format: BookFormat,
    pub path: String,
    pub reader: crate::library::BoxedBookReader,
    pub stream_control: Option<crate::library::AudioStreamControl>,
    #[cfg(target_os = "android")]
    pub playback_file: Option<std::fs::File>,
}
