use super::LibraryClient;
use crate::library::commands::library_requests;
use crate::ThumbnailResolution;

impl LibraryClient {
    pub async fn book_format(&self, content_hash: crate::ContentHash) -> Result<Option<book_model::BookFormat>, String> {
        self.request(library_requests::BookFormat { content_hash }).await
    }

    pub async fn thumbnail(&self, content_hash: crate::ContentHash, resolution: ThumbnailResolution) -> Result<Option<Vec<u8>>, String> {
        let bytes: Option<serde_bytes::ByteBuf> = self.request(library_requests::Thumbnail { content_hash, resolution }).await?;
        Ok(bytes.map(serde_bytes::ByteBuf::into_vec))
    }
    pub async fn download_book(&self, content_hash: crate::ContentHash) -> Result<crate::DownloadState, String> {
        self.request(library_requests::DownloadBook { content_hash }).await
    }
    pub async fn download_folder(&self, query: library_model::LibraryBrowseQuery) -> Result<(), String> {
        self.request(library_requests::DownloadFolder { query }).await
    }
    pub async fn download_subject(&self, query: library_model::LibraryBrowseQuery) -> Result<(), String> {
        self.request(library_requests::DownloadSubject { query }).await
    }
    /// Drops the downloaded files for one book while keeping its entry,
    /// placements, and reading state. Fails when the server holds no copy.
    pub async fn remove_local_book_copy(&self, content_hash: crate::ContentHash) -> Result<crate::DownloadState, String> {
        self.request(library_requests::RemoveLocalBookCopy { content_hash }).await
    }
    pub async fn restore_book(&self, content_hash: crate::ContentHash) -> Result<(), String> {
        self.request(library_requests::RestoreBook { content_hash }).await
    }
    pub async fn purge_book(&self, content_hash: crate::ContentHash) -> Result<(), String> {
        self.request(library_requests::PurgeBook { content_hash }).await
    }
    pub async fn move_book_to_trash(&self, content_hash: crate::ContentHash) -> Result<(), String> {
        self.request(library_requests::MoveBookToTrash { content_hash }).await
    }
    /// Opens the book using the target's reader and source adapter.
    pub async fn resolve_book(&self, content_hash: crate::ContentHash) -> Result<crate::ResolvedBook, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.transport.handle().map_err(|error| error.to_string())?.resolve_book(content_hash).await.map_err(|error| error.to_string())
        }
        #[cfg(target_arch = "wasm32")]
        {
            crate::library::web_reader::resolve_book(self, content_hash).await
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn resolve_audiobook_track(&self, content_hash: crate::ContentHash, index: usize) -> Result<crate::ResolvedBook, String> {
        self.transport.handle().map_err(|error| error.to_string())?.resolve_audiobook_track(content_hash, index).await.map_err(|error| error.to_string())
    }
}
