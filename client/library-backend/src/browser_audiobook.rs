//! Local media retains a storage lease; remote media uses expiring direct URLs.
pub use crate::library::{PlaybackLocation, PlaybackMetadata, PlaybackSource};
use crate::{ContentHash, LibrarySession};
use cpu_host::{BookOperation, Inspect};
use client_platform_web::transport::range::Server as RangeServer;
pub type PlaybackLease = client_platform_web::audiobook::PlaybackLease;
pub type BrowserAudiobook = client_platform_web::audiobook::BrowserAudiobook<PlaybackMetadata, crate::library::DirectPlayback>;

pub(crate) async fn prepare(library: &LibrarySession, hash: ContentHash) -> Result<(PlaybackSource, PlaybackLease), crate::BackendError> {
    let file = library.assets.playback_reader(&hash).await.map_err(crate::BackendError::operation)?;
    prepare_with_file(library, hash, file).await
}

/// Browser-host command used when a local audiobook is absent.  The format
/// check and storage probe remain inside the owning library module.
pub async fn open_remote_audio(library: &LibrarySession, hash: ContentHash) -> Result<Option<PlaybackSource>, crate::BackendError> {
    let format = library.db.book_format(&hash).map_err(crate::BackendError::operation)?;
    if !matches!(format, Some(book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder)) || library.assets.playback_reader(&hash).await.map_err(crate::BackendError::operation)?.is_some() {
        return Ok(None);
    }
    let (source, _lease) = prepare(library, hash).await?;
    Ok(Some(source))
}

pub(crate) async fn prepare_with_file(library: &LibrarySession, hash: ContentHash, file: Option<client_platform_web::web_storage::FileReader>) -> Result<(PlaybackSource, PlaybackLease), crate::BackendError> {
    let format = library.db.book_format(&hash).map_err(crate::BackendError::operation)?;
    if !matches!(format, Some(book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder)) {
        return Err("book is not an audiobook".into());
    }
    let snapshot = library.db.audiobook_playback_snapshot(&hash).map_err(crate::BackendError::operation)?;
    let (title, author) = (snapshot.title, snapshot.author);
    let mut tracks = library.db.audiobook_tracks(&hash).map_err(crate::BackendError::operation)?;
    let remote = if file.is_none() && (format == Some(book_model::BookFormat::Mp3Folder) || snapshot.cached.is_none()) { Some(library.describe_remote_file(hash).await?.0) } else { None };
    let current_checksum = if format == Some(book_model::BookFormat::Mp3Folder) {
        match (&file, &remote) {
            (Some(file), _) => file.checksum().map_err(crate::BackendError::message)?,
            (_, Some(remote)) => Some(remote.checksum),
            _ => None,
        }
    } else { None };
    let cached = snapshot.cached.filter(|_| format != Some(book_model::BookFormat::Mp3Folder) ||
        current_checksum.is_some_and(|checksum| book_model::tracks_match_archive(&tracks, checksum))).map(|metadata| (metadata.duration_ms, metadata.chapters, metadata.narrator));
    let (duration_ms, chapters, narrator) = if let Some(cached) = cached {
        cached
    } else {
        let format = format.unwrap();
        let source_name = if format == book_model::BookFormat::Mp3Folder { title.clone() } else { format!("{title}.m4b") };
        let inspection = book(library, hash, file.as_ref(), remote.as_ref(), Inspect { source_name, format, checksum: current_checksum }).await?;
        let audio = inspection.audiobook.ok_or("missing audiobook metadata")?;
        if format == book_model::BookFormat::Mp3Folder && current_checksum.is_some_and(|checksum| !book_model::tracks_match_archive(&audio.tracks, checksum)) {
            return Err("audiobook archive changed during inspection".into());
        }
        let committed = library.db.commit_audiobook_metadata(&hash, &audio).map_err(crate::BackendError::operation)?;
        tracks = library.db.audiobook_tracks(&hash).map_err(crate::BackendError::operation)?;
        (committed.duration_ms, committed.chapters, committed.narrator)
    };
    // Browser playback currently runs without artwork. In particular, opening
    // a remote book must not wait on optional thumbnail extraction or storage.
    let cover = Vec::new();
    let (location, local_lease) = match file {
        Some(file) if format == Some(book_model::BookFormat::Mp3Folder) => {
            if tracks.is_empty() { return Err("downloaded MP3 folder has no track index".into()); }
            let (urls, lease) = client_platform_web::transport::resources::local_track_media_lease(file, &tracks).await.map_err(crate::BackendError::operation)?;
            (PlaybackLocation::LocalTracks(urls), Some(lease))
        }
        Some(file) => {
            let (url, lease) = client_platform_web::transport::resources::local_media_lease(file).await.map_err(crate::BackendError::operation)?;
            (PlaybackLocation::LocalUrl(url), Some(lease))
        }
        None => {
            let source = library.direct_audio_source(hash).await?;
            if format == Some(book_model::BookFormat::Mp3Folder) && !book_model::tracks_match_archive(&tracks, source.checksum) {
                return Err("audiobook track index does not match the playback archive".into());
            }
            (PlaybackLocation::Direct(source), None)
        },
    };
    let lease = PlaybackLease::local(local_lease);
    Ok((PlaybackSource { metadata: PlaybackMetadata { format: format.unwrap(), title, author, narrator, duration_ms, chapters, tracks, cover }, location }, lease))
}

async fn book<C: BookOperation>(library: &LibrarySession, hash: ContentHash, file: Option<&client_platform_web::web_storage::FileReader>, remote: Option<&crate::library::RemoteFile>, task: C) -> Result<C::Reply, crate::BackendError> {
    if let Some(file) = file {
        client_platform_web::cpu_host::run_book_operation(task, file.fork()).await.map_err(crate::BackendError::operation)
    } else if let Some(remote) = remote {
        // Only parser-requested ranges cross to the CPU worker. The library
        // retains authentication and pins every read to this remote revision.
        let account = library.sync().and_then(|sync| sync.account()).ok_or("library account access is unavailable")?;
        let library_id = *library.id();
        let remote = (*remote).clone();
        let client = reqwest::Client::new();
        let source = RangeServer::asynchronous(remote.length, move |offset, length| {
            let account = account.clone();
            let remote = remote.clone();
            let client = client.clone();
            async move { crate::library::remote_access::read_remote_file(&client, &account, library_id, &remote, offset, length).await }
        })
        .map_err(crate::BackendError::operation)?;
        client_platform_web::cpu_host::run_book_operation_with_source(task, source.source()).await.map_err(crate::BackendError::operation)
    } else {
        let reader = library.assets.prepare_reader(crate::BlobKind::Book, &hash).await.map_err(crate::BackendError::operation)?.ok_or("audiobook file disappeared")?;
        client_platform_web::cpu_host::run_book_operation(task, reader).await.map_err(crate::BackendError::operation)
    }
}
