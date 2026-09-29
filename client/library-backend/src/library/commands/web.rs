use crate::library::book::browser;
use crate::library::browser_audiobook;
use crate::library::LibrarySession;

pub(super) async fn open_remote_audio(library: &LibrarySession, content_hash: crate::ContentHash) -> Result<Option<crate::library::PlaybackSource>, crate::BackendError> {
    browser_audiobook::open_remote_audio(library, content_hash).await
}

pub(super) fn read_book_range(library: &LibrarySession, capability: &str, offset: u64, length: usize) -> Result<serde_bytes::ByteBuf, crate::BackendError> {
    browser::read_local_range(*library.id(), capability, offset, length).map(serde_bytes::ByteBuf::from).map_err(crate::BackendError::operation)
}

pub(super) fn cleanup_staged_import(library: &LibrarySession, job: &str, files: u32) -> Result<(), crate::BackendError> {
    uuid::Uuid::parse_str(job).map_err(crate::BackendError::operation)?;
    let prefix = format!("__libraries/{}/imports/{job}/", library.id());
    client_platform_web::web_storage::cleanup_staged_import(&prefix, files).map_err(crate::BackendError::operation)
}
