//! Bringing books into a library: dropped files and folders placed on disk
//! and imported in place, files a caller already stored, and browser
//! selected folders. Every route validates the destination, inspects the
//! book, publishes its bytes, and commits the addition the same way.
//!
//! The session brackets each import (see `library-backend`) so contents
//! notifications wait for its one completion announcement.

use std::sync::Arc;

use library_database::Database;
use library_files::AssetStore;
use library_placement::LibraryPlacement;
use library_runtime::events::LibraryEventRouter;

mod catalog;
mod import;
mod transfer;
pub mod import_status;
#[cfg(not(target_arch = "wasm32"))]
mod dropped_paths;
#[cfg(not(target_arch = "wasm32"))]
mod import_native;
mod provider_uploads;
#[cfg(target_arch = "wasm32")]
mod selected;

pub use import_status::{ImportFailure, ImportFailureKind};

#[cfg(not(target_arch = "wasm32"))]
pub use client_platform_native::filesystem::{ImportReader as DirectoryImportReader, ImportReaderFuture as DirectoryImportReaderFuture, ImportSource};
#[cfg(target_arch = "wasm32")]
pub use client_platform_web::transport::import_io::{ImportReader as DirectoryImportReader, ImportReaderFuture as DirectoryImportReaderFuture, ImportSource};

pub struct LibraryImport {
    pub library_id: sync_common::LibraryId,
    pub cloud: cloud_storage::connections::CloudConnections,
    pub provider_upload_lock: tokio::sync::Mutex<()>,
    pub database: Arc<Database>,
    pub assets: AssetStore,
    pub cpu: Arc<dyn cpu_host::CpuHost>,
    pub events: LibraryEventRouter,
    pub placement: Arc<LibraryPlacement>,
    /// The library folder dropped files are placed into.
    #[cfg(not(target_arch = "wasm32"))]
    pub root: String,
}

/// A selected directory tree whose files can be opened one at a time while
/// the backend imports them.
pub struct DirectoryImport {
    pub name: String,
    pub directories: Vec<Vec<String>>,
    pub files: Vec<DirectoryImportFile>,
}

/// A source capability and its path within the selected tree.
pub struct DirectoryImportFile {
    pub path: Vec<String>,
    pub source: ImportSource,
}

/// The folders created for a selected tree, by path, before any file is opened.
#[cfg(target_arch = "wasm32")]
pub(crate) struct PreparedDirectoryImport {
    pub(crate) directories: std::collections::HashMap<Vec<String>, sync_common::DirId>,
    pub(crate) failures: Vec<ImportFailure>,
}
