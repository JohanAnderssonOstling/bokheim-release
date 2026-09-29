use super::LibraryClient;
use crate::library::commands::library_requests;

impl LibraryClient {
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn import_owned_book(&self, parent_id: crate::DirId, file_name: String, path: std::path::PathBuf) -> Result<crate::ContentHash, String> {
        self.transport.handle().map_err(|error| error.to_string())?.import_book_path(parent_id, file_name, path).await.map_err(|error| error.to_string())
    }

    /// Explicit imports only; automatic filesystem scanning is excluded.
    pub async fn directory_import_progress(&self) -> Result<Option<library_model::DirectoryImportProgress>, String> {
        self.request(library_requests::DirectoryImportProgress).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn directory_import_progress_for(&self, activity: uuid::Uuid) -> Result<Option<library_model::DirectoryImportProgress>, String> {
        self.transport.handle().map_err(|error| error.to_string())?.directory_import_progress_for(activity).await.map_err(|error| error.to_string())
    }

    /// Browser imports transfer ownership to the shared import coordinator.
    pub async fn import_directory_contents(&self, parent_id: crate::DirId, directory: crate::DirectoryImport) -> Result<Vec<crate::ImportFailure>, String> {
        self.import_selected_directory(parent_id, directory, false, None).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn import_directory_contents_with_activity(&self, parent_id: crate::DirId, directory: crate::DirectoryImport, activity: uuid::Uuid) -> Result<Vec<crate::ImportFailure>, String> {
        self.import_selected_directory(parent_id, directory, false, Some(activity)).await
    }

    pub async fn import_book_bytes(&self, parent_id: crate::DirId, file_name: String, bytes: Vec<u8>) -> Result<crate::ContentHash, String> {
        self.request(library_requests::ImportBookBytes { parent_id, file_name, bytes }).await
    }
    pub async fn import_directory(&self, parent_id: crate::DirId, directory: crate::DirectoryImport) -> Result<Vec<crate::ImportFailure>, String> {
        self.import_selected_directory(parent_id, directory, true, None).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn import_directory_with_activity(&self, parent_id: crate::DirId, directory: crate::DirectoryImport, activity: uuid::Uuid) -> Result<Vec<crate::ImportFailure>, String> {
        self.import_selected_directory(parent_id, directory, true, Some(activity)).await
    }

    async fn import_selected_directory(&self, parent_id: crate::DirId, directory: crate::DirectoryImport, create_root: bool, activity: Option<uuid::Uuid>) -> Result<Vec<crate::ImportFailure>, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.transport.handle().map_err(|error| error.to_string())?.import_selected_directory(parent_id, directory, create_root, activity).await.map_err(|error| error.to_string())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = activity;
            self.transport.import_directory(parent_id, directory, create_root).await
        }
    }
}
