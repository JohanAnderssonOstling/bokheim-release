//! Preserve library metadata and offline access without linking provider APIs.

use crate::availability::unavailable;
pub use crate::connection_types::{ConnectionPersistence, GoogleAuthorization, GoogleConnection};
use crate::google_drive::{DriveUpload, UploadProgress};
use crate::oauth::{GoogleAccessGrant, GoogleAccount};
use crate::scanner::{ScanCursor, ScanPage};
use crate::{ChangesPage, CloudDirectoryIdentity, CloudEntry, CloudError, CloudFolder, FolderPage};
use sync_common::storage::{ProviderFile, ProviderId, ProviderScope};

#[derive(Clone, Default)]
pub struct CloudConnections;

impl CloudConnections {
    pub fn google_drive(&self, _scope: ProviderScope) -> Result<ConnectedDrive, CloudError> {
        Err(unavailable())
    }

    pub async fn connect_google_access_grant(&self, _grant: GoogleAccessGrant) -> Result<GoogleAccount, CloudError> {
        Err(unavailable())
    }
}

/// Cannot be constructed when the integration is disabled. Keeping its API
/// lets existing libraries retain their provider references and local copies.
#[derive(Clone)]
pub struct ConnectedDrive {
    scope: ProviderScope,
}

impl ConnectedDrive {
    pub fn scope(&self) -> &ProviderScope {
        &self.scope
    }

    pub async fn list_folder(&self, _folder: &ProviderId, _page: Option<&str>, _folders_only: bool) -> Result<FolderPage, CloudError> {
        Err(unavailable())
    }
    pub async fn describe(&self, _file: &ProviderId) -> Result<CloudEntry, CloudError> {
        Err(unavailable())
    }
    pub async fn read_range(&self, _file: &ProviderFile, _offset: u64, _length: usize) -> Result<Vec<u8>, CloudError> {
        Err(unavailable())
    }
    pub async fn start_checkpoint(&self) -> Result<String, CloudError> {
        Err(unavailable())
    }
    pub async fn changes(&self, _checkpoint: &str) -> Result<ChangesPage, CloudError> {
        Err(unavailable())
    }
    pub async fn scan_page(&self, _cursor: &ScanCursor) -> Result<ScanPage, CloudError> {
        Err(unavailable())
    }
    pub async fn create_folder(&self, _parent: &ProviderId, _name: &str) -> Result<CloudFolder, CloudError> {
        Err(unavailable())
    }
    pub async fn create_library_folder(&self, _parent: &ProviderId, _name: &str, _identity: &CloudDirectoryIdentity) -> Result<CloudFolder, CloudError> {
        Err(unavailable())
    }
    pub async fn allocate_file_id(&self) -> Result<ProviderId, CloudError> {
        Err(unavailable())
    }
    pub async fn begin_upload(&self, _parent: &ProviderId, _file: &ProviderId, _name: &str, _size: u64) -> Result<DriveUpload, CloudError> {
        Err(unavailable())
    }
    pub async fn upload_chunk(&self, _upload: &DriveUpload, _offset: u64, _bytes: Vec<u8>) -> Result<UploadProgress, CloudError> {
        Err(unavailable())
    }
    pub async fn upload_status(&self, _upload: &DriveUpload) -> Result<UploadProgress, CloudError> {
        Err(unavailable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sync_common::storage::CloudProvider;

    #[test]
    fn disabled_build_rejects_drive_before_credentials_are_needed() {
        assert!(!crate::google_drive_available());
        let scope = ProviderScope { provider: CloudProvider::GoogleDrive, account_id: ProviderId::new("saved-account").unwrap(), container_id: ProviderId::new("my_drive").unwrap() };
        assert!(matches!(CloudConnections.google_drive(scope), Err(CloudError::InvalidRequest(message)) if message.contains("not enabled in this build")));
        assert!(crate::require_google_drive().is_err());
    }
}
