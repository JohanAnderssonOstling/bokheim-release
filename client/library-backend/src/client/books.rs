use super::LibraryClient;
use crate::library::{LibraryCommand, PendingCommand, ReadyCommand};
use crate::ThumbnailResolution;
use crate::{BackendError, BookId, DownloadState, ResolvedBook};
use library_model::CardKey;

impl LibraryClient {
    pub async fn book_format(&self, content_hash: BookId) -> Result<Option<book_model::BookFormat>, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::BookFormat { content_hash })).await
    }

    pub async fn thumbnail(&self, content_hash: BookId, resolution: ThumbnailResolution) -> Result<Option<Vec<u8>>, BackendError> {
        let bytes: Option<serde_bytes::ByteBuf> = self.request(LibraryCommand::Pending(PendingCommand::Thumbnail { content_hash, resolution })).await?;
        Ok(bytes.map(serde_bytes::ByteBuf::into_vec))
    }
    pub async fn external_catalog_cover(&self, entry: CardKey, resolution: ThumbnailResolution) -> Result<Option<Vec<u8>>, BackendError> {
        let bytes: Option<serde_bytes::ByteBuf> = self.request(LibraryCommand::Pending(PendingCommand::ExternalCatalogCover { entry, resolution })).await?;
        Ok(bytes.map(serde_bytes::ByteBuf::into_vec))
    }
    pub async fn download_state(&self, content_hash: BookId) -> Result<DownloadState, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::DownloadState { content_hash })).await
    }
    pub async fn download_book(&self, content_hash: BookId) -> Result<DownloadState, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::DownloadBook { content_hash })).await
    }

    /// Drops the downloaded files for one book while keeping its entry,
    /// placements, and reading state. Fails when the server holds no copy.
    pub async fn remove_local_book_copy(&self, content_hash: BookId) -> Result<DownloadState, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::RemoveLocalBookCopy { content_hash })).await
    }
    pub async fn restore_book(&self, content_hash: BookId) -> Result<(), BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::RestoreBook { content_hash })).await
    }
    pub async fn purge_book(&self, content_hash: BookId) -> Result<(), BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::PurgeBook { content_hash })).await
    }
    pub async fn move_book_to_trash(&self, content_hash: BookId) -> Result<(), BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::MoveBookToTrash { content_hash })).await
    }
    /// Opens the book using the target's reader and source adapter.
    pub async fn resolve_book(&self, content_hash: BookId) -> Result<ResolvedBook, BackendError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use client_platform_runtime::startup_profile::StartupProfile;
            let mut profile = StartupProfile::new("book_lookup_client");
            let handle = self.transport.handle().await?;
            profile.mark("transport_handle");
            let result = handle.resolve_book(content_hash).await;
            profile.mark("actor_roundtrip");
            result
        }
        #[cfg(target_arch = "wasm32")]
        {
            crate::library::web_reader::resolve_book(self, content_hash).await
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn resolve_audiobook_track(&self, content_hash: BookId, index: usize) -> Result<ResolvedBook, BackendError> {
        self.transport.handle().await?.resolve_audiobook_track(content_hash, index).await
    }
}
