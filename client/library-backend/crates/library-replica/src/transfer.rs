use sync_common::{ContentHash, LibraryId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobKind {
    Book,
    Thumbnail,
}

impl BlobKind {
    pub const fn storage(self) -> &'static str {
        match self {
            Self::Book => "book",
            Self::Thumbnail => "thumbnail",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlobKey {
    kind: BlobKind,
    content_hash: ContentHash,
}

impl BlobKey {
    pub fn new(kind: BlobKind, content_hash: ContentHash) -> Self {
        Self { kind, content_hash }
    }
    pub const fn kind(&self) -> BlobKind {
        self.kind
    }
    pub fn content_hash(&self) -> &ContentHash {
        &self.content_hash
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferJobKind {
    UploadBook,
    UploadThumbnail,
    DownloadBook,
    DownloadThumbnail,
}

impl TransferJobKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::UploadBook => "Upload book",
            Self::UploadThumbnail => "Upload thumbnail",
            Self::DownloadBook => "Download book",
            Self::DownloadThumbnail => "Download thumbnail",
        }
    }
    pub const fn batch_label(self) -> &'static str {
        match self {
            Self::UploadBook => "Uploading books",
            Self::UploadThumbnail => "Uploading thumbnails",
            Self::DownloadBook => "Downloading books",
            Self::DownloadThumbnail => "Downloading thumbnails",
        }
    }
    pub const fn storage(self) -> &'static str {
        match self {
            Self::UploadBook => "upload_blob",
            Self::UploadThumbnail => "upload_thumbnail",
            Self::DownloadBook => "download_blob",
            Self::DownloadThumbnail => "download_thumbnail",
        }
    }
    pub fn parse_storage(value: &str) -> Option<Self> {
        match value {
            "upload_blob" => Some(Self::UploadBook),
            "upload_thumbnail" => Some(Self::UploadThumbnail),
            "download_blob" => Some(Self::DownloadBook),
            "download_thumbnail" => Some(Self::DownloadThumbnail),
            _ => None,
        }
    }
    pub const fn blob_kind(self) -> BlobKind {
        match self {
            Self::UploadBook | Self::DownloadBook => BlobKind::Book,
            Self::UploadThumbnail | Self::DownloadThumbnail => BlobKind::Thumbnail,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    Queued,
    Running,
    Retrying,
    Completed,
    Failed,
}

impl TransferState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Retrying => "Retrying",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferOrigin {
    #[default]
    Background,
    UserInitiated,
}

impl TransferOrigin {
    pub const fn storage(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::UserInitiated => "user_initiated",
        }
    }
    pub fn parse_storage(value: &str) -> Option<Self> {
        match value {
            "background" => Some(Self::Background),
            "user_initiated" => Some(Self::UserInitiated),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferStatus {
    pub id: String,
    pub kind: TransferJobKind,
    pub content_hash: ContentHash,
    pub state: TransferState,
    pub origin: TransferOrigin,
    pub attempts: u32,
    pub completed_items: u32,
    pub total_items: u32,
    pub last_error: Option<String>,
    /// The failed item, distinct from the batch representative in old history.
    #[serde(default)]
    pub failed_content_hash: Option<ContentHash>,
    /// Display name resolved from library metadata, never from diagnostic text.
    #[serde(default)]
    pub file_name: Option<String>,
    pub next_retry_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Progress through supported books in selected folders.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ScanProgress {
    pub total: u64,
    pub succeeded: u64,
    pub failed: u64,
}

/// A user-level workflow; individual transfer jobs retain their own retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LibraryOperationKind {
    Import,
    Sync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LibraryOperationState {
    Running,
    Waiting,
    Completed,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LibraryOperation {
    pub id: String,
    pub kind: LibraryOperationKind,
    pub state: LibraryOperationState,
    pub scanning: bool,
    /// This workflow includes account sync, rather than only local import work.
    #[serde(default)]
    pub requires_sync: bool,
    #[serde(default)]
    pub scan_failures: Vec<(String, String)>,
    pub books: u64,
    pub uploaded_books: u64,
    pub pending_uploads: u64,
    /// Generated covers that still need to reach cloud storage.
    #[serde(default)]
    pub pending_covers: u64,
    pub pending_changes: u64,
    pub pending_thumbnails: u64,
    pub failures: u64,
    pub needs_attention: bool,
    pub waiting_reason: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LibraryTransfers {
    /// Some pending books were refused because account storage is full.
    #[serde(default)]
    pub storage_quota_blocked: bool,
    #[serde(default)]
    pub operation: Option<LibraryOperation>,
    pub library_id: LibraryId,
    pub library_name: String,
    pub scanner_running: bool,
    #[serde(default)]
    pub thumbnail_work_running: bool,
    #[serde(default)]
    pub thumbnail_progress: Option<ScanProgress>,
    #[serde(default)]
    pub thumbnail_waiting: bool,
    /// Known cloud thumbnails ready on this device, and known cloud thumbnails total.
    #[serde(default)]
    pub thumbnail_download_coverage: (u64, u64),
    #[serde(default)]
    pub metadata_sync_running: bool,
    #[serde(default)]
    pub preparing_transfers: bool,
    #[serde(default)]
    pub total_books: u64,
    #[serde(default)]
    pub uploaded_books: u64,
    #[serde(default)]
    pub scan_progress: Option<ScanProgress>,
    #[serde(default)]
    pub file_work_pending: bool,
    #[serde(default)]
    pub file_work_error: Option<String>,
    pub transfers: Vec<TransferStatus>,
    pub history: Vec<TransferStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistence_values_are_closed_and_round_trip() {
        for kind in [TransferJobKind::UploadBook, TransferJobKind::UploadThumbnail, TransferJobKind::DownloadBook, TransferJobKind::DownloadThumbnail] {
            assert_eq!(TransferJobKind::parse_storage(kind.storage()), Some(kind));
        }
        assert_eq!(TransferJobKind::parse_storage("upload"), None);
    }
}
