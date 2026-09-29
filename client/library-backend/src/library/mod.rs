//! API and shared resources scoped to one concrete library.

pub mod discovery;
pub(crate) use library_runtime::events;

mod annotations;
mod browse;
pub mod commands;
mod directory;
mod open;
mod session;
pub mod subscriptions;
mod thumbnails;
mod workers;
pub use open::LibraryOpenConfig;
mod handle;
mod io;
mod jobs;
mod management;
mod pending;
mod reading;
mod resources;
#[cfg(not(target_arch = "wasm32"))]
pub use book_access::remote_file::AudioStreamControl;
pub use book_access::remote_file::RemoteFile;
mod audible_enrichment;
mod book;
#[cfg(target_arch = "wasm32")]
#[path = "../browser_audiobook.rs"]
pub mod browser_audiobook;
#[cfg(not(target_arch = "wasm32"))]
mod directory_import;
mod enrichment;
mod operations;
mod playback;
mod preparation;
pub(crate) mod remote_access;
mod transfer_status;

use library_replica::filenames;

pub use book::{AudiobookBook, DocumentBook, ResolvedBook};
#[cfg(target_arch = "wasm32")]
pub use browser_audiobook::{BrowserAudiobook, PlaybackLease};
pub use commands::LibraryCommand;
pub use directory::LibraryDirectory;
pub use handle::{LibraryAuthorsView, LibraryFolderEntry, LibraryFolderView, LibraryHierarchyEntry, LibraryHierarchyView, LibrarySubjectEntry, LibrarySubjectView};
pub use handle::{LibraryHandle, LibrarySession};
pub use io::PreparedDirectoryImport;
pub use io::{BookReader, BoxedBookReader, DirectoryImport, DirectoryImportFile, DirectoryImportReader, DirectoryImportReaderFuture, ImportSource};
pub use library_database::sync_status::{LibrarySyncState, LibrarySyncStatus};
pub use library_database::LibraryBrowseData;
pub use library_model::{BookDownloadStatus, DownloadProgress, DownloadProgressError, DownloadState};
pub use playback::{DirectPlayback, PlaybackLocation, PlaybackMetadata, PlaybackSource};
pub use resources::LibraryRuntimeSpec;

#[cfg(not(target_arch = "wasm32"))]
mod import_native;
#[cfg(target_arch = "wasm32")]
mod import_web;
#[cfg(target_arch = "wasm32")]
pub mod web_import;

#[cfg(target_arch = "wasm32")]
pub use book::web_reader;
