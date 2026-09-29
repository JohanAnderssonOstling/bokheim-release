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

    /// Admission failures caused by the book or account are durable
    /// until the local file changes or the account's quota changes. An upload
    /// already in progress is different: another worker may release it, so it
    /// remains retryable.
    pub fn from_upload_admission(reason: sync_common::api::assets::BlobManifestRejectionReason) -> Self {
        use sync_common::api::assets::BlobManifestRejectionReason;
        let message = format!("server rejected upload admission: {reason:?}");
        match reason {
            BlobManifestRejectionReason::UploadInProgress => Self::retryable(message),
            BlobManifestRejectionReason::InvalidSize | BlobManifestRejectionReason::SizeMismatch | BlobManifestRejectionReason::IdentityMismatch | BlobManifestRejectionReason::QuotaExceeded => Self::rejected(message),
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
            sync_transport::AssetResponseError::StorageQuotaExceeded => Self::retryable("book storage quota exceeded"),
            sync_transport::AssetResponseError::Retryable(error) => Self::Retryable(BackendError::operation(error)),
            sync_transport::AssetResponseError::Rejected(error) => Self::Rejected(BackendError::operation(error)),
            sync_transport::AssetResponseError::Wire(error) => Self::retryable(error),
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
    use std::error::Error;

    #[test]
    fn retryable_retains_its_source_when_cloned() {
        let error = TransferError::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let cloned = error.clone();
        let source = cloned.source().unwrap().source().unwrap().downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn worker_roundtrip_preserves_retry_policy_and_io_details() {
        let error = TransferError::from(std::io::Error::from_raw_os_error(13));
        let bytes = client_runtime::wire::encode_worker_message(&error).unwrap();
        let restored: TransferError = client_runtime::wire::decode_worker_message(&bytes).unwrap();
        let TransferError::Retryable(source) = restored else { panic!("retry policy changed") };
        assert!(source.report().causes.iter().any(|cause| matches!(cause, client_runtime::ErrorCause::Io { os_code: Some(13), .. })));
    }
}
