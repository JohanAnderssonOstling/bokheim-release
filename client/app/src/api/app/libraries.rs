use super::AppClient;
use crate::runtime::AppCommand;
use crate::{BackendError, LibraryEntry, LibraryFolderError};
use sync_common::LibraryId;
use tokio::sync::watch;

impl AppClient {
    pub async fn copy_library_storage(&self, library_id: LibraryId, bokheim: bool) -> Result<LibraryEntry, BackendError> {
        self.request(AppCommand::CopyLibraryStorage { library_id, bokheim }).await
    }

    pub async fn google_cloud_accounts(&self) -> Result<Vec<crate::GoogleAccount>, BackendError> { self.request(AppCommand::GoogleCloudAccounts).await }
    pub async fn google_cloud_web_client_id(&self) -> Result<String, BackendError> { self.request(AppCommand::GoogleCloudWebClientId).await }
    pub async fn connect_google_cloud_access_grant(&self, grant: crate::GoogleAccessGrant) -> Result<crate::GoogleAccount, BackendError> { self.request(AppCommand::ConnectGoogleCloudAccessGrant { grant }).await }
    pub async fn begin_google_cloud_login(&self) -> Result<crate::CloudLoginRequest, BackendError> { self.request(AppCommand::BeginGoogleCloudLogin).await }
    pub async fn wait_google_cloud_login(&self, attempt_id: uuid::Uuid) -> Result<crate::GoogleAccount, BackendError> { self.request(AppCommand::WaitGoogleCloudLogin { attempt_id }).await }
    pub async fn disconnect_google_cloud_account(&self, account: crate::ProviderId) -> Result<(), BackendError> { self.request(AppCommand::DisconnectGoogleCloudAccount { account }).await }
    pub async fn cloud_folders(&self, scope: crate::ProviderScope, folder: Option<crate::ProviderId>, page: Option<String>) -> Result<crate::CloudFolderListing, BackendError> { self.request(AppCommand::CloudFolders { scope, folder, page }).await }
    pub async fn create_cloud_folder(&self, scope: crate::ProviderScope, parent: crate::ProviderId, name: String) -> Result<crate::CloudFolder, BackendError> { self.request(AppCommand::CreateCloudFolder { scope, parent, name }).await }
    pub async fn create_cloud_library(&self, name: String, root: crate::CloudLibraryRoot) -> Result<LibraryEntry, BackendError> { self.request(AppCommand::CreateCloudLibrary { name, root }).await }
    /// Subscribe before reading the initial list. Unread changes coalesce;
    /// await `changed()` and fetch `libraries()` for the current entries.
    pub async fn library_list_changes(&self) -> Result<watch::Receiver<()>, BackendError> {
        self.transport.library_list_changes_transport().await
    }

    pub async fn libraries(&self) -> Result<Vec<LibraryEntry>, BackendError> {
        self.request(AppCommand::Libraries).await
    }

    /// Resolve a destination only when the user imports or downloads a book.
    pub async fn library_for_import(&self, selected: Option<LibraryId>) -> Result<library_backend::LibraryClient, BackendError> {
        let explicitly_selected = selected.is_some();
        let id = match selected {
            Some(id) => id,
            None => {
                let library: LibraryEntry = self.request(AppCommand::EnsureImportLibrary).await?;
                *library.library_id()
            }
        };
        let library = self.library(id);
        if explicitly_selected {
            library.book_count().await.map_err(|error| error.context("selected library is unavailable"))?;
        }
        Ok(library)
    }

    pub async fn set_library_asset_storage(&self, library_id: LibraryId, enabled: bool) -> Result<(), BackendError> {
        self.request(AppCommand::SetLibraryAssetStorage { library_id, enabled }).await
    }

    pub async fn estimate_library_folders(&self, locators: Vec<String>) -> Result<(u64, u64, u64), BackendError> {
        self.request(AppCommand::EstimateLibraryFolders { locators }).await
    }

    pub async fn create_library_with_asset_storage(&self, name: String, asset_storage_enabled: bool) -> Result<LibraryEntry, BackendError> {
        self.request(AppCommand::CreateLibraryWithAssetStorage { name, asset_storage_enabled }).await
    }

    pub async fn create_libraries_with_asset_storage(&self, locators: Vec<String>, asset_storage_enabled: bool) -> Result<(Vec<LibraryId>, Vec<LibraryFolderError>), BackendError> {
        self.request(AppCommand::CreateLibrariesWithAssetStorage { locators, asset_storage_enabled }).await
    }

    pub async fn rename_library(&self, library_id: LibraryId, name: String) -> Result<(), BackendError> {
        self.request(AppCommand::RenameLibrary { library_id, name }).await
    }

    pub async fn delete_library(&self, library_id: LibraryId) -> Result<(), BackendError> {
        self.request(AppCommand::DeleteLibrary { library_id }).await
    }
}
