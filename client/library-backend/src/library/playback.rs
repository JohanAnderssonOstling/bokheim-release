//! Values exchanged with a running library session.
//!
//! They intentionally contain no application, executor, storage, or account
//! objects.

use serde::{Deserialize, Serialize};
use sync_common::BookId;

#[derive(Clone, Serialize, Deserialize)]
pub struct DirectPlayback {
    pub url: String,
    pub expires_in_seconds: u32,
    pub content_hash: BookId,
    pub length: u64,
}

impl std::fmt::Debug for DirectPlayback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectPlayback").field("content_hash", &self.content_hash).finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PlaybackLocation {
    LocalUrl(String),
    LocalTracks(Vec<String>),
    Direct(DirectPlayback),
    /// Revision-pinned bytes read by the library's external provider adapter.
    Ranges(super::RemoteFile),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaybackSource {
    pub metadata: library_model::PlaybackMetadata,
    pub location: PlaybackLocation,
}
