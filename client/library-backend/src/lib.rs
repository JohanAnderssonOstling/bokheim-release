//! Resource-bearing backend for one concrete library.
//!
//! The application crate owns device registry, account policy, and startup.
//! This crate owns database-backed library sessions, synchronization, imports,
//! book access, and their workers.

extern crate core;

pub use client_platform_runtime::executor;

pub mod asset_store;
mod asset_workflow;
pub mod book;
pub mod client;
pub use book_model::{AgentId, AnnotationAnchor, AnnotationStyle, AuthorId, BookFormat, EpubCfi, ReadProgress, ReaderAnnotation};
pub use client::LibraryClient;
pub use library::{AudiobookBook, DocumentBook, ResolvedBook};
pub mod integration;
pub mod updates;
pub mod library;
pub mod sync;
#[cfg(test)]
mod test_database;

pub use account_client::ServerUrl;
pub use asset_transfer::{LibraryTransfers, TransferOrigin, TransferState, TransferStatus};
pub use client_runtime::BackendError;
pub use import_pipeline::{ImportFailure, ImportFailureKind, ImportProgress, ImportState};
pub use library::{
    BookDownloadStatus, BookReader, BoxedBookReader, DirectoryImport, DirectoryImportFile, DirectoryImportReader, DirectoryImportReaderFuture, DownloadProgress, DownloadProgressError, DownloadState, ImportSource, LibraryBrowseData,
    LibrarySyncState, LibrarySyncStatus,
};
pub use library_database::{BookPlacement, RelativeBookPath, RelativeBookPathError, RelativeDirPath, RelativeDirPathError};
pub use library_model::{LibraryBrowseQuery, LibraryHomeView, LibraryUpdate};
pub use library_replica::{BlobKey, BlobKind, FolderDestinationPolicy, LibraryOperation, LibraryOperationKind, LibraryOperationState, ScanProgress, TransferJobKind};
pub use sync_common::{ContentHash, DirId, LibraryId, ROOT_DIR_ID};
/// The database belongs to a concrete library session, not the filesystem adapter.
pub(crate) const APP_HIDDEN_DIR: &str = ".bokheim";
pub(crate) const APP_HIDDEN_LIBRARY_DB_PATH: &str = ".bokheim/library.db";
mod thumbnail_resolution;
pub use thumbnail_resolution::ThumbnailResolution;

pub(crate) use library_runtime::interest as notification_interest;

pub use book::{BookSource, ResolvedBookData};
pub use library::{LibraryCommand, LibraryHandle, LibrarySession};

/// Process-wide CPU host default: the current platform's host. Sessions hold
/// explicit handles; launchers override the default with
/// [`cpu_host::set_cpu_host`] before opening libraries.
pub(crate) fn default_cpu_host() -> std::sync::Arc<dyn cpu_host::CpuHost> {
    #[cfg(target_arch = "wasm32")]
    {
        std::sync::Arc::new(client_platform_web::cpu_host::WebHost::default())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::sync::Arc::new(client_platform_native::cpu_host::NativeHost::default())
    }
}

/// The installed host, installing the platform default first when none is set.
pub(crate) fn cpu_host() -> std::sync::Arc<dyn cpu_host::CpuHost> {
    cpu_host::cpu_host_or_init(default_cpu_host)
}
