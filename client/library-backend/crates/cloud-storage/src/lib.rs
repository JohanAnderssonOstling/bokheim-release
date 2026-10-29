//! Provider APIs for cloud folder discovery, seekable reads, and committed
//! uploads. No operation in this crate knows a Bokheim storage endpoint.

#[cfg(feature = "google-drive")]
use http_response::BodyReadError;
use operation_failure::Failure;
#[cfg(feature = "google-drive")]
use reqwest::{Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::{future::Future, pin::Pin};
use sync_common::storage::{ProviderFile, ProviderId};
use sync_common::{DirId, LibraryId};

#[cfg(all(feature = "google-drive", any(target_os = "android", test)))]
pub mod android_authorization;
mod availability;
mod connection_types;
mod folders;
#[cfg(feature = "google-drive")]
pub mod connections;
#[cfg(not(feature = "google-drive"))]
#[path = "connections_disabled.rs"]
pub mod connections;
mod google_drive_types;
mod oauth_types;
#[cfg(target_os = "android")]
pub use availability::set_google_play_services_available;
pub use availability::{google_drive_available, require_google_drive};
#[cfg(feature = "google-drive")]
pub mod google_drive;
#[cfg(not(feature = "google-drive"))]
pub mod google_drive {
    pub use crate::google_drive_types::{DriveUpload, UploadProgress};
}
#[cfg(all(feature = "google-drive", not(target_arch = "wasm32")))]
pub mod native_login;
#[cfg(feature = "google-drive")]
pub mod oauth;
#[cfg(not(feature = "google-drive"))]
pub mod oauth {
    pub use crate::oauth_types::{GoogleAccessGrant, GoogleAccount, GoogleOAuthConfig, GoogleTokens};
}
pub mod scanner;

#[cfg(not(target_arch = "wasm32"))]
pub type StorageFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CloudError>> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
pub type StorageFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, CloudError>> + 'a>>;

#[cfg(not(target_arch = "wasm32"))]
pub trait ProviderRuntime: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync> ProviderRuntime for T {}
#[cfg(target_arch = "wasm32")]
pub trait ProviderRuntime {}
#[cfg(target_arch = "wasm32")]
impl<T> ProviderRuntime for T {}

/// Provider-specific authentication stays with the connection owner. Expired
/// credentials produce a typed result so that owner can refresh and retry.
pub trait CloudStorage: ProviderRuntime {
    fn list_folder<'a>(&'a self, token: &'a str, folder: &'a ProviderId, page: Option<&'a str>, folders_only: bool) -> StorageFuture<'a, FolderPage>;
    fn describe<'a>(&'a self, token: &'a str, id: &'a ProviderId) -> StorageFuture<'a, CloudEntry>;
    fn read_range<'a>(&'a self, token: &'a str, file: &'a ProviderFile, offset: u64, length: usize) -> StorageFuture<'a, Vec<u8>>;
    fn start_checkpoint<'a>(&'a self, token: &'a str) -> StorageFuture<'a, String>;
    fn changes<'a>(&'a self, token: &'a str, checkpoint: &'a str) -> StorageFuture<'a, ChangesPage>;
    fn create_folder<'a>(&'a self, token: &'a str, parent: &'a ProviderId, name: &'a str) -> StorageFuture<'a, CloudFolder>;
}

pub const MAX_RANGE_BYTES: usize = 8 * 1024 * 1024;
#[cfg(feature = "google-drive")]
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(feature = "google-drive")]
const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CloudFolder {
    pub id: ProviderId,
    pub name: String,
    pub parent_ids: Vec<ProviderId>,
    #[serde(default)]
    pub directory_hint: Option<CloudDirectoryIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CloudDirectoryIdentity {
    pub library_id: LibraryId,
    pub directory_id: DirId,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CloudBookFile {
    pub file: ProviderFile,
    pub name: String,
    pub parent_ids: Vec<ProviderId>,
    pub mime_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum CloudEntry {
    Folder(CloudFolder),
    File(CloudBookFile),
}

/// An unfinished listing is never evidence of deletion. The scanner commits
/// a complete inventory only after consuming every continuation page.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FolderPage {
    pub entries: Vec<CloudEntry>,
    pub next_page: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum CloudChange {
    Updated(CloudEntry),
    Removed(ProviderId),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangesPage {
    pub changes: Vec<CloudChange>,
    pub next_page: Option<String>,
    /// Checkpoint for a subsequent pass, available only on the final page.
    pub checkpoint: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CloudError {
    AuthenticationRequired,
    PermissionDenied,
    NotFound,
    RevisionChanged,
    CursorExpired,
    StorageFull,
    RateLimited,
    Transport { failure: Failure, message: String },
    Retryable(String),
    InvalidResponse(String),
    InvalidRequest(String),
}

impl fmt::Display for CloudError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthenticationRequired => formatter.write_str("Reconnect your cloud storage account"),
            Self::PermissionDenied => formatter.write_str("Cloud storage denied access to this file or folder"),
            Self::NotFound => formatter.write_str("The cloud file or folder is unavailable"),
            Self::RevisionChanged => formatter.write_str("The cloud file changed; refresh the library and retry"),
            Self::CursorExpired => formatter.write_str("The cloud storage scan checkpoint expired"),
            Self::StorageFull => formatter.write_str("Your cloud storage account is full. Free up space in that account, then retry"),
            Self::RateLimited => formatter.write_str("Cloud storage is limiting requests; wait before retrying"),
            Self::Transport { message, .. } => formatter.write_str(message),
            Self::Retryable(message) | Self::InvalidResponse(message) | Self::InvalidRequest(message) => formatter.write_str(message),
        }
    }
}
impl std::error::Error for CloudError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::StorageFull => &Failure::CloudStorageFull,
            Self::RateLimited => &Failure::RateLimited,
            Self::AuthenticationRequired => &Failure::SignInRequired,
            Self::PermissionDenied => &Failure::PermissionDenied,
            Self::NotFound => &Failure::NotFound,
            Self::Transport { failure, .. } => failure,
            _ => return None,
        })
    }
}

#[cfg(feature = "google-drive")]
impl CloudError {
    pub(crate) fn transport(error: reqwest::Error) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let offline = error.is_connect();
        #[cfg(target_arch = "wasm32")]
        let offline = error.is_request();
        let failure = if error.is_timeout() {
            Failure::TimedOut
        } else if offline {
            Failure::Offline
        } else {
            Failure::ServiceUnavailable
        };
        Self::Transport { failure, message: error.without_url().to_string() }
    }
    pub(crate) fn status(status: StatusCode) -> Self {
        match status {
            StatusCode::UNAUTHORIZED => Self::AuthenticationRequired,
            StatusCode::FORBIDDEN => Self::PermissionDenied,
            StatusCode::NOT_FOUND => Self::NotFound,
            StatusCode::GONE => Self::CursorExpired,
            StatusCode::PRECONDITION_FAILED => Self::RevisionChanged,
            StatusCode::TOO_MANY_REQUESTS => Self::RateLimited,
            StatusCode::INSUFFICIENT_STORAGE => Self::StorageFull,
            status if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT => Self::Retryable(format!("Cloud storage returned HTTP {status}")),
            status => Self::InvalidResponse(format!("Cloud storage returned HTTP {status}")),
        }
    }

    /// Drive uses HTTP 403 for both exhausted storage and request-rate limits.
    /// Inspect bounded machine-readable reasons; never classify message text.
    pub(crate) async fn response(response: Response) -> Self {
        let status = response.status();
        let fallback = Self::status(status);
        if status != StatusCode::FORBIDDEN {
            return fallback;
        }
        let Ok(bytes) = bounded_bytes(response, 64 * 1024).await else { return fallback };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return fallback };
        let reasons = value.pointer("/error/errors").and_then(serde_json::Value::as_array);
        if reasons.is_some_and(|reasons| reasons.iter().any(|error| error.get("reason").and_then(serde_json::Value::as_str) == Some("storageQuotaExceeded"))) {
            Self::StorageFull
        } else if reasons.is_some_and(|reasons| reasons.iter().any(|error| matches!(error.get("reason").and_then(serde_json::Value::as_str), Some("rateLimitExceeded" | "userRateLimitExceeded" | "dailyLimitExceeded" | "quotaExceeded")))) {
            Self::RateLimited
        } else {
            fallback
        }
    }
}

#[cfg(feature = "google-drive")]
pub(crate) async fn bounded_bytes(response: Response, limit: usize) -> Result<Vec<u8>, CloudError> {
    http_response::read(response, limit).await.map_err(|error| match error {
        BodyReadError::Http(error) => CloudError::transport(error),
        BodyReadError::TooLarge { .. } => CloudError::InvalidResponse("Cloud storage response is too large".into()),
    })
}

#[cfg(feature = "google-drive")]
pub(crate) async fn metadata<T: serde::de::DeserializeOwned>(response: Response) -> Result<T, CloudError> {
    if !response.status().is_success() {
        return Err(CloudError::response(response).await);
    }
    let bytes = bounded_bytes(response, MAX_METADATA_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(|_| CloudError::InvalidResponse("Invalid cloud storage metadata".into()))
}
