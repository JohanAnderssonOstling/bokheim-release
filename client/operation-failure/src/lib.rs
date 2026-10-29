//! Why an operation failed, in terms the user can act on.
//!
//! An error is classified where it arises, travels with the error across the
//! backend worker boundary, and is worded only by the UI. Nothing reads an
//! error's text to decide what happened.

use serde::{Deserialize, Serialize};
use std::fmt;

/// An error that may know why it happened. The UI words it from
/// [`Classified::failure`] and logs its `Display` text as the diagnostic.
pub trait Classified: fmt::Display {
    fn failure(&self) -> Option<Failure>;
}

/// Text from a source that does not classify its errors, such as a decoder.
impl Classified for str {
    fn failure(&self) -> Option<Failure> {
        None
    }
}

impl Classified for String {
    fn failure(&self) -> Option<Failure> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Failure {
    /// The device or browser storage has no room left.
    StorageFull,
    ReadOnlyStorage,
    PermissionDenied,
    /// Another browser tab holds the library's storage.
    StorageBusy,
    LibraryFolderUnavailable,
    /// No connection to the service could be made.
    Offline,
    TimedOut,
    /// The service refused the credentials it was given (HTTP 401).
    Unauthorized,
    /// The service refused access, or blocked the request (HTTP 403).
    Forbidden,
    /// What was asked for is gone (HTTP 404/410, or no longer listed).
    NotFound,
    /// The service rejected the request as invalid (HTTP 400/409/422).
    Rejected,
    RateLimited,
    ServiceUnavailable,
    /// No usable session; the user must sign in.
    SignInRequired,
    /// The account changed underneath the operation.
    AccountChanged,
    /// The text entered is not a usable web address.
    InvalidAddress,
    /// The address answers, but not with a supported book catalog.
    NotACatalog,
    /// The catalog refused the username and password the user entered.
    CatalogCredentialsRejected,
    /// The system browser component needed for this is not installed.
    WebViewMissing,
    DrmProtected,
    /// A book file is corrupt, empty, unsupported, or not what was expected.
    UnreadableBook,
    Wifi(WifiFailure),
    /// The cloud storage account has no room for more files.
    CloudStorageFull,
}

// Origins can expose a classified cause without depending on the backend.
// Diagnostics use the variant name; user-facing wording belongs to the UI.
impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { write!(formatter, "{self:?}") }
}
impl std::error::Error for Failure {}

impl Failure {
    /// The classification a service's HTTP status implies, if any.
    pub fn from_http_status(status: u16) -> Option<Self> {
        match status {
            401 => Some(Self::Unauthorized),
            403 => Some(Self::Forbidden),
            404 | 410 => Some(Self::NotFound),
            400 | 409 | 422 => Some(Self::Rejected),
            429 => Some(Self::RateLimited),
            500..=599 => Some(Self::ServiceUnavailable),
            _ => None,
        }
    }

    /// The classification an I/O error kind implies, if any. The standard
    /// library already maps ENOSPC, EDQUOT, EROFS, EACCES and their Windows
    /// counterparts onto these kinds.
    pub fn from_io(kind: std::io::ErrorKind) -> Option<Self> {
        use std::io::ErrorKind;
        match kind {
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Some(Self::StorageFull),
            ErrorKind::ReadOnlyFilesystem => Some(Self::ReadOnlyStorage),
            ErrorKind::PermissionDenied => Some(Self::PermissionDenied),
            ErrorKind::TimedOut => Some(Self::TimedOut),
            ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::NotConnected | ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable | ErrorKind::NetworkDown => Some(Self::Offline),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WifiFailure {
    /// A WPA password must be 8 to 63 printable ASCII characters.
    InvalidPassword,
    AuthenticationFailed,
    UnsupportedSecurity,
    NetworkGone,
    InvalidNetworkName,
    ScanTimedOut,
    NoAddress,
    ConnectTimedOut,
    Sleeping,
    PowerControlUnavailable,
}
