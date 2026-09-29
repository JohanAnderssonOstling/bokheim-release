//! Device-global application services and library lifecycle management.

mod account;
mod core;
pub(crate) mod data;
mod libraries;
mod notifications;
pub mod preferences;

pub use account_client::{ServerUrl, ServerUrlError};

pub const APP_NAME: &str = "Bokheim";
pub const APP_NAME_LOWER: &str = "bokheim";

pub const API_SERVER_ORIGIN: &str = "https://api.bokheim.se";
pub const METADATA_SERVER_ORIGIN: &str = "https://meta.bokheim.se";

pub use crate::api::AppClient;
pub use crate::library::{LibrarySyncState, LibrarySyncStatus};
pub use crate::runtime::{
    decode_worker_message, encode_worker_message, AccountSnapshot, AppStartupState, AppWorkerClientMessage, AppWorkerRequest, AppWorkerStartupMessage, ResolvedBookData, ThumbnailResolution, WorkerCommand, WorkerDispatchResult,
    WorkerPayload, WorkerSubscription,
};
pub use crate::BackendError;
pub use account::{AccountStatus, AccountStorageUsage, LocalLibraryStorageUsage, ServerLibraryStorageUsage};
pub(crate) use core::AppBackend;
pub use core::LibraryListState;
pub(crate) use data::BackendContext;
pub use data::{AppDataLocation, LibraryEntry};

pub fn api_server_url() -> ServerUrl {
    ServerUrl::parse(API_SERVER_ORIGIN).expect("Bokheim API origin is a valid HTTPS origin")
}

pub fn metadata_server_url() -> ServerUrl {
    ServerUrl::parse(METADATA_SERVER_ORIGIN).expect("Bokheim metadata origin is a valid HTTPS origin")
}

#[cfg(all(target_arch = "wasm32", feature = "web-runtime-tests"))]
pub async fn notification_socket_contract(url: &str, token: &str) -> Result<u32, String> {
    notifications::socket_contract(url, token).await
}
