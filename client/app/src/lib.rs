//! Application clients, account state, and startup configuration.
//! Platform launchers and worker entry points use [`host`].
pub mod host;
pub use cloud_storage::google_drive_available;

mod api;
mod app;
mod library_client;
mod library_folder_error;
pub use library_folder_error::{LibraryFolderError, LibraryFolderStep};
use client_platform_runtime::executor;
pub(crate) use library_backend::integration as notification_interest;
mod platform;
pub(crate) mod runtime;

pub use app::{API_SERVER_ORIGIN, METADATA_SERVER_ORIGIN, Url, api_server_url, metadata_server_url};
pub use app::{APP_NAME, APP_NAME_LOWER};
pub use app::{AccountSnapshot, AppClient, AppStartupState};
pub use app::{AccountStatus, AppDataLocation, LibraryEntry, LocalLibraryStorageUsage, ServerLibraryStorageUsage};
pub use app::{CloudFolderListing, CloudLoginRequest};
pub use cloud_storage::{CloudEntry, CloudFolder, oauth::GoogleAccount};
pub use cloud_storage::oauth::GoogleAccessGrant;
pub use sync_common::storage::{CloudLibraryRoot, CloudProvider, LibraryStorage, ProviderId, ProviderScope};
pub use client_runtime::{BackendError, Classified, Failure, WifiFailure};
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
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use app::BackendContext;

#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(all(feature = "kobo", not(target_arch = "wasm32")))]
pub use api::app::{BatteryReading, BatteryStatus};
#[cfg(all(feature = "kobo", target_os = "linux"))]
pub use api::app::{BluetoothDevice, BluetoothRequest, BluetoothStatus, WifiNetwork, WifiPhase, WifiRequest, WifiSecurity, WifiSnapshot};
