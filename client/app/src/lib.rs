//! Frontend API: application/library clients, view data, and reader capabilities.
//! Platform launchers and worker entry points use [`host`].
extern crate core;

pub mod host;

mod api;
mod app;
#[cfg(not(target_arch = "wasm32"))]
mod kobo_transfer;
mod library_client;
#[cfg(test)]
pub(crate) use app::data as home;
use client_platform_runtime::executor;
pub(crate) use library_backend::integration as notification_interest;
mod platform;
pub(crate) mod runtime;

pub use app::{api_server_url, metadata_server_url, ServerUrl, ServerUrlError, API_SERVER_ORIGIN, METADATA_SERVER_ORIGIN};
#[cfg(test)]
pub(crate) use app::{decode_worker_message, encode_worker_message, AppWorkerRequest, WorkerDispatchResult, WorkerPayload};
pub use app::{AccountSnapshot, AppClient, AppStartupState, ThumbnailResolution};
pub use app::{AccountStatus, AccountStorageUsage, AppDataLocation, LibraryEntry, LocalLibraryStorageUsage, ServerLibraryStorageUsage};
pub use app::{APP_NAME, APP_NAME_LOWER};
pub use client_runtime::BackendError;
/// Compatibility exports for clients that still import library-owned modules
/// through the application crate. New code should depend on `library-backend`.
pub(crate) use library_backend::{asset_store, library, sync};
pub use library_backend::{
    BlobKey, BlobKind, BookDownloadStatus, BookPlacement, BookReader, BookSource, BoxedBookReader, ContentHash, DirId, DirectoryImport, DirectoryImportFile, DirectoryImportReader, DirectoryImportReaderFuture, DownloadProgress,
    DownloadProgressError, DownloadState, ImportFailure, ImportFailureKind, ImportProgress, ImportSource, ImportState, LibraryBrowseData, LibraryCommand, LibraryHandle, LibraryId, LibrarySession, LibrarySyncState, LibrarySyncStatus,
    LibraryTransfers, RelativeBookPath, RelativeBookPathError, RelativeDirPath, RelativeDirPathError, ResolvedBookData, ScanProgress, TransferJobKind, TransferOrigin, TransferState, TransferStatus, ROOT_DIR_ID,
};
pub(crate) type DeviceAccountSession = library_backend::integration::LibraryAccountSession<app::BackendContext>;

#[cfg(target_arch = "wasm32")]
pub(crate) use platform::web_storage::initialize as initialize_web_storage;

/// Initialize the logging system. Call this once at the start of your application.
///
/// Example:
/// ```no_run
/// app::init_logging();
/// ```
pub fn init_logging() {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Warn)
        .filter_module("wgpu", log::LevelFilter::Warn)
        .filter_module("wgpu_core", log::LevelFilter::Warn)
        .filter_module("wgpu_hal", log::LevelFilter::Warn)
        .filter_module("zbus", log::LevelFilter::Warn)
        .filter_module("ashpd", log::LevelFilter::Warn)
        .init();
    log::info!("Logging initialized");
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use api::BackendLaunch;
#[cfg(test)]
pub(crate) use app::{AppBackend, BackendContext};

#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(all(feature = "kobo", not(target_arch = "wasm32")))]
pub use api::app::BatteryStatus;
#[cfg(all(feature = "kobo", target_os = "linux"))]
pub use api::app::{BluetoothDevice, BluetoothRequest, BluetoothStatus, WifiNetwork, WifiPhase, WifiRequest, WifiSecurity, WifiSnapshot};
#[cfg(not(target_arch = "wasm32"))]
pub use library::AudioStreamControl;
#[cfg(target_arch = "wasm32")]
pub use library_backend::library::browser_audiobook::{BrowserAudiobook, PlaybackLease};
pub use library_backend::library::{AudiobookBook, DirectPlayback, DocumentBook, PlaybackLocation, PlaybackMetadata, PlaybackSource, ResolvedBook};
