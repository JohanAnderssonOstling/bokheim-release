use super::LibraryClient;
use crate::library::commands::library_requests;

impl LibraryClient {
    pub async fn restore_directory(&self, directory_id: crate::DirId, parent_id: Option<crate::DirId>) -> Result<String, String> {
        self.request(library_requests::RestoreDirectory { directory_id, parent_id }).await
    }

    pub async fn folder_destinations(&self) -> Result<Vec<library_model::LibraryFolderDestination>, String> {
        self.request(library_requests::FolderDestinations).await
    }
    pub async fn trash(&self) -> Result<library_model::LibraryTrashView, String> {
        self.request(library_requests::Trash).await
    }
    pub async fn purge_directory(&self, directory_id: crate::DirId) -> Result<(), String> {
        self.request(library_requests::PurgeDirectory { directory_id }).await
    }
    pub async fn empty_trash(&self) -> Result<(), String> {
        self.request(library_requests::EmptyTrash).await
    }
    pub async fn create_directory(&self, parent_id: crate::DirId, name: String) -> Result<(), String> {
        self.request(library_requests::CreateDirectory { parent_id, name }).await
    }
    pub async fn rename_directory(&self, directory_id: crate::DirId, name: String) -> Result<String, String> {
        self.request(library_requests::RenameDirectory { directory_id, name }).await
    }
    pub async fn move_directory(&self, directory_id: crate::DirId, parent_id: crate::DirId) -> Result<String, String> {
        self.request(library_requests::MoveDirectory { directory_id, parent_id }).await
    }
    pub async fn copy_directory(&self, directory_id: crate::DirId, parent_id: crate::DirId) -> Result<crate::DirId, String> {
        self.request(library_requests::CopyDirectory { directory_id, parent_id }).await
    }
    pub async fn restore_book_placement(&self, content_hash: crate::ContentHash, directory_id: crate::DirId) -> Result<(), String> {
        self.request(library_requests::RestoreBookPlacement { content_hash, directory_id }).await
    }
    pub async fn move_directory_to_trash(&self, directory_id: crate::DirId) -> Result<(), String> {
        self.request(library_requests::MoveDirectoryToTrash { directory_id }).await
    }
    /// Drops the downloaded copies under one folder while keeping every
    /// entry. Books the server does not hold keep their bytes. Returns how
    /// many copies were evicted.
    pub async fn evict_folder_downloads(&self, directory_id: crate::DirId) -> Result<usize, String> {
        self.request(library_requests::EvictFolderDownloads { directory_id }).await
    }
    pub async fn copy_book_to_directory(&self, content_hash: crate::ContentHash, source_id: crate::DirId, destination_id: crate::DirId) -> Result<String, String> {
        self.request(library_requests::CopyBookToDirectory { content_hash, source_id, destination_id }).await
    }
    pub async fn move_book_to_directory(&self, content_hash: crate::ContentHash, source_id: crate::DirId, destination_id: crate::DirId) -> Result<String, String> {
        self.request(library_requests::MoveBookToDirectory { content_hash, source_id, destination_id }).await
    }
    pub async fn remove_book_from_directory(&self, content_hash: crate::ContentHash, source_id: crate::DirId) -> Result<bool, String> {
        self.request(library_requests::RemoveBookFromDirectory { content_hash, source_id }).await
    }
}
