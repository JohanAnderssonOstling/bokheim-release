//! In-process transfer scheduling and its durable completed-history log.
//!
//! This layer is independent of book storage and network execution.
mod history;
mod queue;

pub use history::{HistoryPersistence, MessagePackTransferHistory};
use library_replica::{BookPlacement, BookUploadIntent, TransferJobKind};
pub use library_replica::{LibraryTransfers, TransferOrigin, TransferState, TransferStatus};
pub use queue::{ClaimedTransferJob, TransferProducer, TransferQueue};
use sync_common::ContentHash;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TransferJob {
    pub operation: TransferOperation,
    pub origin: TransferOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TransferOperation {
    UploadBlob { content_hash: ContentHash, intent: Option<BookUploadIntent>, negotiated: bool },
    UploadThumbnail { thumbnail_hash: ContentHash },
    DownloadBlob { content_hash: ContentHash, placement: BookPlacement },
    DownloadThumbnail { thumbnail_hash: ContentHash },
}

impl TransferJob {
    pub fn upload_blob(content_hash: ContentHash, origin: TransferOrigin) -> Self {
        Self { operation: TransferOperation::UploadBlob { content_hash, intent: None, negotiated: false }, origin }
    }
    pub fn upload_book_intent(intent: BookUploadIntent, negotiated: bool, origin: TransferOrigin) -> Self {
        Self { operation: TransferOperation::UploadBlob { content_hash: intent.content_hash, intent: Some(intent), negotiated }, origin }
    }
    pub fn upload_thumbnail(thumbnail_hash: ContentHash, origin: TransferOrigin) -> Self {
        Self { operation: TransferOperation::UploadThumbnail { thumbnail_hash }, origin }
    }
    pub fn download_blob(content_hash: ContentHash, placement: BookPlacement, origin: TransferOrigin) -> Self {
        Self { operation: TransferOperation::DownloadBlob { content_hash, placement }, origin }
    }
    pub fn download_thumbnail(thumbnail_hash: ContentHash, origin: TransferOrigin) -> Self {
        Self { operation: TransferOperation::DownloadThumbnail { thumbnail_hash }, origin }
    }
    pub fn batch_kind(&self) -> Option<TransferJobKind> {
        match self.operation {
            TransferOperation::UploadBlob { .. } => Some(TransferJobKind::UploadBook),
            TransferOperation::UploadThumbnail { .. } => Some(TransferJobKind::UploadThumbnail),
            TransferOperation::DownloadThumbnail { .. } => Some(TransferJobKind::DownloadThumbnail),
            _ => None,
        }
    }
    pub const fn origin(&self) -> TransferOrigin {
        self.origin
    }
    pub fn status_fields(&self) -> (TransferJobKind, &ContentHash) {
        match &self.operation {
            TransferOperation::UploadBlob { content_hash, .. } => (TransferJobKind::UploadBook, content_hash),
            TransferOperation::UploadThumbnail { thumbnail_hash } => (TransferJobKind::UploadThumbnail, thumbnail_hash),
            TransferOperation::DownloadBlob { content_hash, .. } => (TransferJobKind::DownloadBook, content_hash),
            TransferOperation::DownloadThumbnail { thumbnail_hash } => (TransferJobKind::DownloadThumbnail, thumbnail_hash),
        }
    }
    pub fn priority(&self) -> (u8, u8, u64) {
        let origin = if self.origin == TransferOrigin::UserInitiated { 0 } else { 1 };
        match self.operation {
            TransferOperation::DownloadThumbnail { .. } | TransferOperation::UploadThumbnail { .. } => (0, origin, 0),
            TransferOperation::UploadBlob { .. } => (1, origin, 0),
            TransferOperation::DownloadBlob { .. } => (1, origin, u64::MAX / 2),
        }
    }
}
