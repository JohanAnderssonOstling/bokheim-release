use super::LibraryClient;
use crate::library::{LibraryCommand, ReadyCommand};
use crate::{BackendError, BookId, DirId};

impl LibraryClient {
    pub async fn restore_directory(&self, directory_id: DirId, parent_id: Option<DirId>) -> Result<String, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::RestoreDirectory { directory_id, parent_id })).await
    }

    pub async fn folder_destinations(&self) -> Result<Vec<library_model::LibraryFolderDestination>, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::FolderDestinations)).await
    }

    pub async fn purge_directory(&self, directory_id: DirId) -> Result<(), BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::PurgeDirectory { directory_id })).await
    }
    pub async fn empty_trash(&self) -> Result<(), BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::EmptyTrash)).await
    }
    pub async fn create_directory(&self, parent_id: DirId, name: String) -> Result<(), BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::CreateDirectory { parent_id, name })).await
    }
    pub async fn rename_directory(&self, directory_id: DirId, name: String) -> Result<String, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::RenameDirectory { directory_id, name })).await
    }
    pub async fn move_directory(&self, directory_id: DirId, parent_id: DirId) -> Result<String, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::MoveDirectory { directory_id, parent_id })).await
    }
    pub async fn copy_directory(&self, directory_id: DirId, parent_id: DirId) -> Result<DirId, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::CopyDirectory { directory_id, parent_id })).await
    }
    pub async fn restore_book_placement(&self, content_hash: BookId, directory_id: DirId) -> Result<(), BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::RestoreBookPlacement { content_hash, directory_id })).await
    }
    pub async fn move_directory_to_trash(&self, directory_id: DirId) -> Result<(), BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::MoveDirectoryToTrash { directory_id })).await
    }
    /// Drops the downloaded copies under one folder while keeping every
    /// entry. Books the server does not hold keep their bytes. Returns how
    /// many copies were evicted.
    pub async fn evict_folder_downloads(&self, directory_id: DirId) -> Result<usize, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::EvictFolderDownloads { directory_id })).await
    }

    pub async fn copy_book_to_directory(&self, content_hash: BookId, source_id: Option<DirId>, destination_id: DirId) -> Result<bool, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::CopyBookToDirectory { content_hash, source_id, destination_id })).await
    }
    /// Moves a placement and reports whether the destination was created, for undo.
    pub async fn move_book_placement(&self, content_hash: BookId, source_id: DirId, destination_id: DirId) -> Result<bool, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::MoveBookPlacement { content_hash, source_id, destination_id })).await
    }
    pub async fn remove_book_from_directory(&self, content_hash: BookId, source_id: DirId) -> Result<bool, BackendError> {
        self.request(LibraryCommand::Ready(ReadyCommand::RemoveBookFromDirectory { content_hash, source_id })).await
    }
}
