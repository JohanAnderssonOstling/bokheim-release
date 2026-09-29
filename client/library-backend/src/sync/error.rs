use asset_transfer::TransferError;
use library_database::DatabaseError;

pub(crate) type SyncResult<T> = Result<T, SyncError>;

#[derive(Debug)]
pub(crate) enum SyncError {
    AuthenticationRequired,
    Failed(String),
}

impl SyncError {
    pub(super) fn failed(error: impl std::fmt::Display) -> Self {
        Self::Failed(error.to_string())
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthenticationRequired => formatter.write_str("synchronization authentication is required"),
            Self::Failed(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<sync_transport::SyncRequestError> for SyncError {
    fn from(error: sync_transport::SyncRequestError) -> Self {
        sync_engine::SyncError::from(error).into()
    }
}

impl From<TransferError> for SyncError {
    fn from(error: TransferError) -> Self {
        match error {
            TransferError::AuthenticationRequired => Self::AuthenticationRequired,
            error @ (TransferError::Retryable(_) | TransferError::Rejected(_)) => Self::failed(error),
        }
    }
}

macro_rules! failed_from {
    ($($error:ty),+ $(,)?) => {$(
        impl From<$error> for SyncError {
            fn from(error: $error) -> Self {
                Self::failed(error)
            }
        }
    )+};
}

failed_from!(Box<dyn std::error::Error>, &'static str, DatabaseError, library_files::asset_store::AssetStoreError, rusqlite::Error, std::io::Error, web_time::SystemTimeError, std::num::TryFromIntError,);

impl From<sync_engine::SyncError> for SyncError {
    fn from(error: sync_engine::SyncError) -> Self {
        match error {
            sync_engine::SyncError::AuthenticationRequired => Self::AuthenticationRequired,
            error @ sync_engine::SyncError::CursorRejected => Self::failed(error),
            sync_engine::SyncError::Failed(message) => Self::Failed(message),
        }
    }
}
