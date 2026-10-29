//! Library synchronization: the storage-independent push/pull engine, and the
//! orchestration that drives it against the library database, its asset
//! store and the transfer executor.

mod asset_sync;
mod engine;
mod error;
mod library_sync;
mod recovery;
mod state_sync;
#[cfg(target_arch = "wasm32")]
mod web_coordinator;

pub use error::{SyncError, SyncResult};
pub use library_sync::{LibrarySync, LibrarySyncConfig};
#[cfg(target_arch = "wasm32")]
pub use web_coordinator::run;
#[cfg(target_arch = "wasm32")]
pub(crate) use web_coordinator::synchronize;
