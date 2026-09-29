use content_address::ContentHash;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct DownloadProgress(f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadProgressError;

impl fmt::Display for DownloadProgressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("download progress must be finite and between zero and one")
    }
}

impl std::error::Error for DownloadProgressError {}

impl DownloadProgress {
    pub const ZERO: Self = Self(0.0);

    pub fn new(value: f32) -> Result<Self, DownloadProgressError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(DownloadProgressError)
        }
    }

    pub fn from_ratio(completed: u64, total: u64) -> Self {
        if total == 0 {
            Self::ZERO
        } else {
            Self((completed as f32 / total as f32).clamp(0.0, 1.0))
        }
    }

    pub const fn value(self) -> f32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub enum DownloadState {
    NotDownloaded,
    Queued,
    Downloading(DownloadProgress),
    Downloaded,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BookDownloadStatus {
    pub content_hash: ContentHash,
    pub state: DownloadState,
}

impl BookDownloadStatus {
    pub fn new(content_hash: ContentHash, state: DownloadState) -> Self {
        Self { content_hash, state }
    }
}

#[cfg(test)]
mod tests {
    use super::DownloadProgress;

    #[test]
    fn progress_rejects_non_finite_and_out_of_range_values() {
        assert!(DownloadProgress::new(f32::NAN).is_err());
        assert!(DownloadProgress::new(-0.1).is_err());
        assert!(DownloadProgress::new(1.1).is_err());
        assert_eq!(DownloadProgress::new(0.5).unwrap().value(), 0.5);
        assert_eq!(DownloadProgress::from_ratio(3, 2).value(), 1.0);
    }
}
