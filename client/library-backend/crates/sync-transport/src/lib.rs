mod book_batch;
pub use book_batch::upload_book_batch;
mod performance;
pub use performance::PerformanceTrace;
mod api;
mod assets;
mod state;

pub use api::{SyncCredentials, TransportError};
pub use assets::{
    blob_endpoint, classify_asset_status, delete_thumbnail, download_blob_stream, download_thumbnail_batch, negotiate_blobs, thumbnail_batch_endpoint, thumbnail_endpoint, thumbnail_presence, thumbnail_presence_endpoint, upload_book_revision_stream, upload_thumbnail_batch,
    validate_streaming_asset_response, AssetResponseError, AssetStatus, BlobDownloadResponse,
};
pub use binary_http::{ServerUrl, ServerUrlError};
pub use state::{compare_state_inventory, exchange_changes, SyncRequestError};
pub use sync_common::{validate_pull_batch, validate_push_response, PullBatchError, PushResponseError, ValidatedPushResponse};

/// Supplies a current access token without making transport own account state.
pub trait CredentialsSource: Send + Sync + std::fmt::Debug {
    fn credentials(&self) -> Option<SyncCredentials>;
}

impl CredentialsSource for std::sync::RwLock<Option<SyncCredentials>> {
    fn credentials(&self) -> Option<SyncCredentials> {
        self.read().ok().and_then(|value| value.clone())
    }
}
