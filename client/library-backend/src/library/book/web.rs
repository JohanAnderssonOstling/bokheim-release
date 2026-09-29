//! Browser book capabilities and media URLs with their owning leases.
use super::browser::BookLease;
use super::*;
use crate::book::{BookSource, ResolvedBookData};
pub(crate) type Opened = (ResolvedBookData, BookLease);

pub(super) fn supports_streaming(format: BookFormat) -> bool {
    matches!(format, BookFormat::Epub | BookFormat::Pdf | BookFormat::M4b | BookFormat::Mp3Folder)
}
pub(super) async fn local(library: &LibrarySession, hash: ContentHash, info: &BookInfo) -> Result<Option<Opened>, BackendError> {
    let Some(file) = library.assets.playback_reader(&hash).await.map_err(BackendError::operation)? else { return Ok(None) };
    if matches!(info.format, BookFormat::M4b | BookFormat::Mp3Folder) {
        return audio(library, hash, Some(file)).await.map(Some);
    }
    let pdf_metadata = match file.checksum().map_err(BackendError::operation)? {
        Some(checksum) => pdf_metadata(library, hash, checksum).await?,
        None => None,
    };
    let (source, lease) = super::browser::register_local_reader(*library.id(), file).map_err(BackendError::operation)?;
    Ok(Some((ResolvedBookData { pdf_metadata, format: info.format, source }, lease)))
}
pub(super) async fn remote(library: &LibrarySession, hash: ContentHash, info: &BookInfo) -> Result<Opened, BackendError> {
    if matches!(info.format, BookFormat::M4b | BookFormat::Mp3Folder) {
        return audio(library, hash, None).await;
    }
    let RemoteDocument { source, prefix, pdf_metadata } = remote_document(library, hash).await?;
    Ok((ResolvedBookData { pdf_metadata, format: info.format, source: BookSource::Remote { source, prefix } }, BookLease::None))
}
async fn audio(library: &LibrarySession, hash: ContentHash, file: Option<client_platform_web::web_storage::FileReader>) -> Result<Opened, BackendError> {
    let (source, lease) = crate::library::browser_audiobook::prepare_with_file(library, hash, file).await.map_err(BackendError::operation)?;
    let format = library.db.book_format(&hash).map_err(BackendError::operation)?.unwrap_or(BookFormat::M4b);
    Ok((ResolvedBookData { pdf_metadata: None, format, source: BookSource::Audiobook(source) }, BookLease::Audiobook(lease)))
}

pub type AudiobookBook = crate::library::browser_audiobook::BrowserAudiobook;
