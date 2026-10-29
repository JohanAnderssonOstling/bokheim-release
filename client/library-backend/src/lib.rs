//! Resource-bearing backend for one concrete library.
//!
//! The application crate owns device registry, account policy, and startup.
//! This crate owns database-backed library sessions, synchronization, imports,
//! book access, and their workers.

extern crate core;

#[cfg(all(target_arch = "wasm32", feature = "browser-cloud-checks"))]
pub use library::cloud::browser_checks::run as check_browser_cloud;
#[cfg(all(target_arch = "wasm32", feature = "browser-cloud-checks"))]
pub use library::cloud::browser_checks::install_taxonomy as prepare_browser_cloud_checks;

pub use client_platform_runtime::executor;

pub mod asset_store;
pub mod book;
pub mod client;
pub use book_model::{AgentId, Annotation, AnnotationAnchor, AnnotationStyle, AuthorId, BookFormat, EpubCfi, ReadProgress};
pub use client::LibraryClient;
pub use library::{AudiobookBook, DocumentBook, DocumentSource, ResolvedBook};
pub use library_database::torrents::BookTorrent;
pub mod integration;
pub mod library;

pub use account_client::Url;
pub use client_runtime::BackendError;
pub use library::{
    BookDownloadStatus, BookReader, BoxedBookReader, DirectoryImport, DirectoryImportFile, DirectoryImportReader, DirectoryImportReaderFuture, DownloadProgress, DownloadProgressError, DownloadState, ImportSource, LibraryBrowseData,
    LibrarySyncState, LibrarySyncStatus,
};
pub use library_import::{ImportFailure, ImportFailureKind};

pub use library::subscriptions::LibraryUpdates;
pub use library_model::{LibraryBrowseQuery, LibraryUpdate};
pub use sync_common::{BookId, DirId, LibraryId, ROOT_DIR_ID};
mod thumbnail_resolution;
pub use library_catalog::evict_cover_caches as evict_catalog_cover_caches;
pub use thumbnail_resolution::ThumbnailResolution;

pub(crate) use library_runtime::interest as notification_interest;

pub use book::{BookSource, ResolvedBookData};
pub use library::{LibraryCommand, LibraryHandle, LibrarySession, PendingCommand, ReadyCommand};
