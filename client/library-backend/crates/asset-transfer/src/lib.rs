//! Asset transfer jobs, scheduling, retry policy, and progress aggregation.

pub mod policy;
pub use library_runtime::{ClaimedTransferJob, HistoryPersistence, LibraryTransfers, MessagePackTransferHistory, TransferJob, TransferOperation, TransferOrigin, TransferProducer, TransferQueue, TransferState, TransferStatus};
pub use sync_transport::{download_blob_stream, download_thumbnail_batch, negotiate_blobs, upload_book_revision_stream, upload_thumbnail_batch, validate_streaming_asset_response, AssetResponseError, BlobDownloadResponse};

pub fn should_download_thumbnail(book_is_local: bool, thumbnail_is_local: bool, thumbnail_is_known_remote: bool, thumbnail_generation_pending: bool) -> bool {
    !thumbnail_is_local && !thumbnail_generation_pending && (!book_is_local || thumbnail_is_known_remote)
}

mod error;
pub use error::TransferError;
pub mod books;
pub mod deadline;

pub mod planning;
pub mod thumbnail_jobs;
pub mod thumbnails;
pub mod transfer;

pub mod runner;
