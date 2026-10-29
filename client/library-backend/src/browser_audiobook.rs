//! Local media retains a storage lease. Remote media uses its library's storage
//! provider, with authenticated ranges for external cloud libraries.

use crate::BackendError;
pub use crate::library::{PlaybackLocation, PlaybackSource};
use crate::{BookId, LibrarySession};
use library_model::PlaybackMetadata;
pub type PlaybackLease = client_platform_web::audiobook::PlaybackLease;
pub type BrowserAudiobook = client_platform_web::audiobook::BrowserAudiobook<PlaybackMetadata, crate::library::DirectPlayback>;

pub(crate) async fn prepare(library: &LibrarySession, hash: BookId) -> Result<(PlaybackSource, PlaybackLease), BackendError> {
    let file = playback_file(library, hash).await?;
    prepare_with_file(library, hash, file).await
}

pub(crate) async fn playback_file(library: &LibrarySession, hash: BookId) -> Result<Option<client_platform_web::web_storage::FileReader>, BackendError> {
    if library.db.storage_policy()?.external().is_some() && !library.db.is_book_downloaded(&hash)? {
        return Ok(None);
    }
    library.assets.playback_reader(&hash).await.map_err(BackendError::operation)
}

/// Browser-host command used when a local audiobook is absent.  The format
/// check and storage probe remain inside the owning library module.
pub async fn open_remote_audio(library: &LibrarySession, hash: BookId) -> Result<Option<PlaybackSource>, BackendError> {
    let format = library.db.book_format(&hash).map_err(BackendError::operation)?;
    if !matches!(format, Some(book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder)) || playback_file(library, hash).await?.is_some() {
        return Ok(None);
    }
    let (source, _lease) = prepare(library, hash).await?;
    Ok(Some(source))
}

pub(crate) async fn prepare_with_file(library: &LibrarySession, hash: BookId, file: Option<client_platform_web::web_storage::FileReader>) -> Result<(PlaybackSource, PlaybackLease), BackendError> {
    let format = library.db.book_format(&hash).map_err(BackendError::operation)?;
    if !matches!(format, Some(book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder)) {
        return Err("book is not an audiobook".into());
    }
    let metadata = library.db.audiobook_playback_metadata(&hash).map_err(BackendError::operation)?;
    let external = library.db.storage_policy()?.external().is_some();
    let remote = if file.is_none() && external {
        let file = library.db.provider_file(&hash)?.ok_or("cloud file reference is unavailable; refresh this library")?;
        let source = crate::library::RemoteFile { hash, length: file.size_bytes, revision: Some(file.revision.as_str().to_owned()) };
        library.remote_book_access(&source)?;
        Some(source)
    } else if file.is_none() && (format == Some(book_model::BookFormat::Mp3Folder) || metadata.is_none()) {
        Some(library.describe_remote_file(hash).await.map_err(BackendError::operation)?.0)
    } else {
        None
    };
    let current_hash = if format == Some(book_model::BookFormat::Mp3Folder) {
        match (&file, &remote) {
            (Some(file), _) => file.content_hash().map_err(BackendError::message)?,
            (_, Some(remote)) => Some(remote.hash),
            _ => None,
        }
    } else {
        None
    };
    // Browser playback currently runs without artwork. In particular, opening
    // a remote book must not wait on optional thumbnail extraction or storage,
    // so the cover stays empty.
    let metadata = metadata
        .filter(|metadata| format != Some(book_model::BookFormat::Mp3Folder) || current_hash.is_some_and(|content_hash| book_model::tracks_match_archive(&metadata.tracks, content_hash)))
        .ok_or("audiobook addition has no valid playback facts")?;
    let (location, local_lease) = match file {
        Some(file) if format == Some(book_model::BookFormat::Mp3Folder) => {
            if metadata.tracks.is_empty() {
                return Err("downloaded MP3 folder has no track index".into());
            }
            let (urls, lease) = client_platform_web::transport::resources::local_track_media_lease(file, &metadata.tracks).await.map_err(BackendError::operation)?;
            (PlaybackLocation::LocalTracks(urls), Some(lease))
        }
        Some(file) => {
            let (url, lease) = client_platform_web::transport::resources::local_media_lease(file).await.map_err(BackendError::operation)?;
            (PlaybackLocation::LocalUrl(url), Some(lease))
        }
        None if external => {
            let source = remote.ok_or("cloud audiobook reference is unavailable")?;
            for track in &metadata.tracks {
                if !track.valid() || track.archive_hash != Some(source.hash) || track.offset.checked_add(track.length).is_none_or(|end| end > source.length) {
                    return Err("audiobook track exceeds its provider archive".into());
                }
            }
            (PlaybackLocation::Ranges(source), None)
        }
        None => {
            let source = library.direct_audio_source(hash).await?;
            if format == Some(book_model::BookFormat::Mp3Folder) && !book_model::tracks_match_archive(&metadata.tracks, source.content_hash) {
                return Err("audiobook track index does not match the playback archive".into());
            }
            (PlaybackLocation::Direct(source), None)
        }
    };
    let lease = PlaybackLease::local(local_lease);
    Ok((PlaybackSource { metadata, location }, lease))
}
