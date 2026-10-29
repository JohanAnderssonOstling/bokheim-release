//! Persisted provider transfers can still be decoded by builds without Drive.

use reqwest::Url;
use serde::{Deserialize, Serialize};
use sync_common::storage::{ProviderFile, ProviderId, ProviderScope};

/// Persist before transferring bytes. The URL is a capability and deliberately
/// has no Debug implementation; callers must keep it out of logs and sync.
#[derive(Clone, Deserialize, Serialize)]
pub struct DriveUpload {
    pub(crate) url: Url,
    pub(crate) scope: ProviderScope,
    pub(crate) file_id: ProviderId,
    pub(crate) size_bytes: u64,
}

impl DriveUpload {
    pub fn matches_file(&self, scope: &ProviderScope, file: &ProviderId, size_bytes: u64) -> bool {
        &self.scope == scope && &self.file_id == file && self.size_bytes == size_bytes
    }
}

pub enum UploadProgress {
    Incomplete { confirmed_bytes: u64 },
    Complete(ProviderFile),
}
