//! Integration API for launchers, browser workers, and runtime probes.
//! Views use the clients and result types at the crate root instead.

pub use client_platform_runtime::startup_profile;
#[cfg(target_os = "android")]
pub use cloud_storage::set_google_play_services_available;
#[cfg(all(feature = "google-drive", target_os = "android"))]
pub use cloud_storage::{android_authorization, CloudError as CloudAuthorizationError, oauth::GoogleAccessGrant as AndroidGoogleAccessGrant};

#[cfg(not(target_arch = "wasm32"))]
pub use crate::api::BackendLaunch;
#[cfg(target_arch = "wasm32")]
pub use crate::runtime::BackendWorkerHost;
pub use crate::runtime::{decode_worker_message, encode_worker_message, AppWorkerClientMessage, AppWorkerRequest, AppWorkerStartupMessage, WorkerCommand, WorkerDispatchResult, WorkerPayload, WorkerSubscription};
#[cfg(target_os = "android")]
pub use library_opds::browser::android as android_browser;
#[cfg(not(target_arch = "wasm32"))]
pub use library_opds::browser::configure as configure_opds_browser;
#[cfg(not(target_arch = "wasm32"))]
pub use library_opds::browser::BrowserFeedback;
#[cfg(not(target_arch = "wasm32"))]
pub use library_opds::browser::address as browser_address;
#[cfg(not(target_arch = "wasm32"))]
pub use library_registry::LIBRARY_MANIFEST_FILENAME;

/// Browser worker startup, ports, and transport integration.
#[cfg(target_arch = "wasm32")]
pub mod web {
    pub use crate::web::*;
}

/// CPU worker requests and execution.
pub mod cpu {
    #[cfg(target_arch = "wasm32")]
    pub use crate::runtime::bisac::install as initialize_bisac_reference;
    #[cfg(target_arch = "wasm32")]
    pub use crate::runtime::taxonomy::install_bundled_matching as initialize_bundled_taxonomy;
    pub use cpu_host::{execute, execute_book_with_mobi, BookTask, CpuRequest, CpuResponse, ThumbnailBytes};
}

/// Mounted Kobo discovery.
#[cfg(not(target_arch = "wasm32"))]
pub mod kobo {
    pub use client_platform_kobo::{detect_kobo_devices, KoboDevice};
}

/// Gate background side effects during a host-owned update startup trial.
pub use client_platform_runtime::executor::defer_background_work;
#[cfg(not(target_arch = "wasm32"))]
pub use client_platform_runtime::executor::{defer_library_maintenance, LibraryMaintenancePermit};
pub use client_platform_runtime::transfers;
