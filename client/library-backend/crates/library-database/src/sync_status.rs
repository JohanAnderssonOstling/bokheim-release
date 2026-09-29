//! Presentation state derived from one library's durable synchronization data.

use serde::{Deserialize, Serialize};
use sync_common::LibraryId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySyncState {
    Checking,
    Unavailable,
    Receiving,
    LocalOnly,
    Synced,
    SyncPending,
    RemoteMissing,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct LibrarySyncStatus {
    #[serde(default = "default_asset_storage_enabled")]
    pub asset_storage_enabled: bool,
    pub library_id: LibraryId,
    pub state: LibrarySyncState,
    pub has_unsynced_changes: bool,
    pub last_synced_at: Option<u64>,
}

const fn default_asset_storage_enabled() -> bool {
    true
}

impl LibrarySyncStatus {
    pub const fn needs_attention(&self) -> bool {
        !matches!(self.state, LibrarySyncState::Synced) || self.has_unsynced_changes || self.last_synced_at.is_none()
    }
    /// Display placeholder for a library the backend reported no status for,
    /// built from library data alone. The caller picks the state: whether an
    /// unknown library is "checking" or "local only" depends on sign-in,
    /// which the library does not know.
    pub fn placeholder(library_id: LibraryId, state: LibrarySyncState) -> Self {
        Self { asset_storage_enabled: default_asset_storage_enabled(), library_id, state, has_unsynced_changes: false, last_synced_at: None }
    }
}
