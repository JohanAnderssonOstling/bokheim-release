//! Stable transfer failures shared by execution, queues and browser RPC.

use client_runtime::BackendError;
use std::fmt;

const AUTHENTICATION_REQUIRED_MESSAGE: &str = "authentication access token was rejected";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum TransferError {
    AuthenticationRequired,
    Retryable(BackendError),
    Rejected(BackendError),
}

impl TransferError {
    pub fn retryable(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Retryable(BackendError::operation(error))
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected(BackendError::message(message))
    }

    /// What the user can do about this failure, when that is known.
    pub fn failure(&self) -> Option<client_runtime::Failure> {
        match self {
            Self::AuthenticationRequired => Some(client_runtime::Failure::SignInRequired),
            Self::Retryable(error) | Self::Rejected(error) => error.failure(),
        }
    }

    /// Admission failures caused by the book or account are durable
    /// until the local file changes or the account's quota changes. An upload
    /// already in progress is different: another worker may release it, so it
    /// remains retryable.
    pub fn from_upload_admission(reason: sync_common::api::assets::BlobManifestRejectionReason) -> Self {
        use sync_common::api::assets::BlobManifestRejectionReason;
        let message = format!("server rejected upload admission: {reason:?}");
        match reason {
            BlobManifestRejectionReason::UploadInProgress => Self::retryable(message),
            BlobManifestRejectionReason::QuotaExceeded => Self::Rejected(BackendError::classified(client_runtime::Failure::CloudStorageFull, message)),
            BlobManifestRejectionReason::InvalidSize | BlobManifestRejectionReason::SizeMismatch => Self::rejected(message),
        }
    }
}

impl fmt::Display for TransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthenticationRequired => formatter.write_str(AUTHENTICATION_REQUIRED_MESSAGE),
            Self::Retryable(message) | Self::Rejected(message) => message.fmt(formatter),
        }
    }
}

impl std::error::Error for TransferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AuthenticationRequired => None,
            Self::Retryable(error) | Self::Rejected(error) => Some(error),
        }
    }
}

impl From<sync_transport::AssetResponseError> for TransferError {
    fn from(error: sync_transport::AssetResponseError) -> Self {
        match error {
            sync_transport::AssetResponseError::AuthenticationRequired => Self::AuthenticationRequired,
            sync_transport::AssetResponseError::StorageQuotaExceeded => Self::Retryable(BackendError::classified(client_runtime::Failure::CloudStorageFull, "book storage quota exceeded")),
            sync_transport::AssetResponseError::Retryable(error) => Self::Retryable(BackendError::operation(error)),
            sync_transport::AssetResponseError::Rejected(error) => Self::Rejected(BackendError::operation(error)),
            sync_transport::AssetResponseError::Wire(error) => Self::retryable(error),
            sync_transport::AssetResponseError::Url(error) => Self::Rejected(BackendError::operation(error)),
        }
    }
}

impl From<std::io::Error> for TransferError {
    fn from(error: std::io::Error) -> Self {
        Self::retryable(error)
    }
}

impl From<rusqlite::Error> for TransferError {
    fn from(error: rusqlite::Error) -> Self {
        Self::retryable(error)
    }
}

impl From<library_database::DatabaseError> for TransferError {
    fn from(error: library_database::DatabaseError) -> Self {
        Self::retryable(error)
    }
}

impl From<library_files::AssetStoreError> for TransferError {
    fn from(error: library_files::AssetStoreError) -> Self {
        Self::retryable(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_runtime::Failure;
    use sync_common::api::assets::BlobManifestRejectionReason;

    #[test]
    fn account_quota_remains_distinct_from_local_disk_and_invalid_books() {
        let admission = TransferError::from_upload_admission(BlobManifestRejectionReason::QuotaExceeded);
        assert!(matches!(admission, TransferError::Rejected(_)));
        assert_eq!(admission.failure(), Some(Failure::CloudStorageFull));
        let interrupted = TransferError::from(sync_transport::AssetResponseError::StorageQuotaExceeded);
        assert!(matches!(interrupted, TransferError::Retryable(_)));
        assert_eq!(interrupted.failure(), Some(Failure::CloudStorageFull));
        assert_eq!(TransferError::from_upload_admission(BlobManifestRejectionReason::InvalidSize).failure(), None);
        assert_eq!(TransferError::from(std::io::Error::from(std::io::ErrorKind::StorageFull)).failure(), Some(Failure::StorageFull));
    }
}
