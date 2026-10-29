//! Where a library's books and folders live: folder organization, and the
//! database-backed placement policy that decides which storage operations
//! (file moves, folder replay, eviction, staging and publishing imports) are
//! required. `library-files` performs the storage operations themselves.

use std::sync::Arc;

use library_database::Database;
use library_files::AssetStore;
use library_runtime::events::LibraryEventRouter;

mod asset_workflow;
#[cfg(not(target_arch = "wasm32"))]
mod directory_replay;
mod management;
#[cfg(not(target_arch = "wasm32"))]
mod native_edit;

#[cfg(not(target_arch = "wasm32"))]
pub use asset_workflow::prepare_directory;
pub use asset_workflow::{evict_local_book, publish_import, run_file_jobs, run_file_jobs_for, stage_import};
#[cfg(not(target_arch = "wasm32"))]
pub use directory_replay::replay as replay_directories;

/// Folder organization for one library: each change is a database write
/// followed by a contents notification.
pub struct LibraryPlacement {
    pub database: Arc<Database>,
    pub assets: AssetStore,
    pub events: LibraryEventRouter,
}
