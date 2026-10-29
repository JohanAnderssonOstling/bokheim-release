//! Errors retain their concrete source while they travel inside the backend.

pub use operation_failure::{Classified, Failure, WifiFailure};
use std::{error::Error, fmt, sync::Arc};

#[derive(Clone, Debug)]
pub enum BackendError {
    Message(String),
    /// An error its origin classified for the user; see [`Failure`].
    Classified { failure: Failure, message: String },
    Transport(String),
    TransportFailure(Arc<Self>),
    Operation(Arc<dyn Error + Send + Sync>),
    Remote(ErrorReport),
    Context { message: String, source: Arc<Self> },
}

/// Serializable diagnostics preserve machine-readable causes across workers.
/// Native calls retain the actual errors; serialization happens only in the
/// worker protocol, not when an error is wrapped or propagated.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ErrorReport {
    pub message: String,
    pub causes: Vec<ErrorCause>,
    /// The outermost classification found along the cause chain.
    pub failure: Option<Failure>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum ErrorCause {
    Transport,
    Io { kind: String, os_code: Option<i32> },
    Sqlite { extended_code: i32 },
    Diagnostic { message: String },
}

impl BackendError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
    pub fn classified(failure: Failure, message: impl Into<String>) -> Self {
        Self::Classified { failure, message: message.into() }
    }
    /// What the user can do about this error: the outermost classification
    /// along its cause chain, or `None` when nothing classified it.
    pub fn failure(&self) -> Option<Failure> {
        self.report().failure
    }
}

impl Classified for BackendError {
    fn failure(&self) -> Option<Failure> {
        BackendError::failure(self)
    }
}

impl BackendError {
    pub fn transport(message: impl Into<String>) -> Self {
        Self::Transport(message.into())
    }
    pub fn into_transport(self) -> Self {
        Self::TransportFailure(Arc::new(self))
    }
    pub fn is_transport(&self) -> bool {
        let mut source: Option<&(dyn Error + 'static)> = Some(self);
        while let Some(error) = source {
            match error.downcast_ref::<Self>() {
                Some(Self::Transport(_) | Self::TransportFailure(_)) => return true,
                Some(Self::Remote(report)) if report.causes.contains(&ErrorCause::Transport) => return true,
                _ => {}
            }
            source = error.source();
        }
        false
    }
    pub fn operation(error: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self::Operation(Arc::from(error.into()))
    }
    pub fn context(self, message: impl Into<String>) -> Self {
        Self::Context { message: message.into(), source: Arc::new(self) }
    }

    pub fn report(&self) -> ErrorReport {
        if let Self::Remote(report) = self {
            return report.clone();
        }
        let mut causes = Vec::new();
        let mut failure = None;
        let mut source: Option<&(dyn Error + 'static)> = Some(self);
        while let Some(error) = source {
            if matches!(error.downcast_ref::<Self>(), Some(Self::Transport(_) | Self::TransportFailure(_))) {
                causes.push(ErrorCause::Transport);
            } else if let Some(Self::Classified { failure: classified, message }) = error.downcast_ref::<Self>() {
                failure = failure.or(Some(*classified));
                causes.push(ErrorCause::Diagnostic { message: message.clone() });
            } else if let Some(classified) = error.downcast_ref::<Failure>() {
                failure = failure.or(Some(*classified));
                causes.push(ErrorCause::Diagnostic { message: classified.to_string() });
            } else if let Some(error) = error.downcast_ref::<std::io::Error>() {
                failure = failure.or(Failure::from_io(error.kind()));
                causes.push(ErrorCause::Io { kind: format!("{:?}", error.kind()), os_code: error.raw_os_error() });
            } else if let Some(rusqlite::Error::SqliteFailure(code, _)) = error.downcast_ref::<rusqlite::Error>() {
                failure = failure.or(sqlite_failure(code.code));
                causes.push(ErrorCause::Sqlite { extended_code: code.extended_code });
            } else if let Some(failure_of_request) = http_failure(error) {
                failure = failure.or(Some(failure_of_request));
                causes.push(ErrorCause::Diagnostic { message: error.to_string() });
            } else if let Some(Self::Remote(report)) = error.downcast_ref::<Self>() {
                failure = failure.or(report.failure);
                causes.extend(report.causes.clone());
                break;
            } else {
                causes.push(ErrorCause::Diagnostic { message: error.to_string() });
            }
            source = error.source();
        }
        ErrorReport { message: self.to_string(), causes, failure }
    }
}

#[cfg(feature = "http")]
impl BackendError {
    /// A failed HTTP request, classified by what reqwest knows about it.
    pub fn http(context: impl fmt::Display, error: reqwest::Error) -> Self {
        let message = format!("{context}: {error}");
        match request_failure(&error) {
            Some(failure) => Self::classified(failure, message),
            None => Self::message(message),
        }
    }
}

/// A timeout, no connection, or the status the service answered with.
#[cfg(feature = "http")]
fn request_failure(error: &reqwest::Error) -> Option<Failure> {
    // A browser fetch that got no response at all is a request error.
    #[cfg(not(target_arch = "wasm32"))]
    let unreachable = error.is_connect();
    #[cfg(target_arch = "wasm32")]
    let unreachable = error.is_request();
    if error.is_timeout() {
        Some(Failure::TimedOut)
    } else if unreachable {
        Some(Failure::Offline)
    } else {
        error.status().and_then(|status| Failure::from_http_status(status.as_u16()))
    }
}

/// Classifies a reqwest error met while walking a cause chain.
fn http_failure(error: &(dyn Error + 'static)) -> Option<Failure> {
    #[cfg(feature = "http")]
    return error.downcast_ref::<reqwest::Error>().and_then(request_failure);
    #[cfg(not(feature = "http"))]
    {
        let _ = error;
        None
    }
}

fn sqlite_failure(code: rusqlite::ErrorCode) -> Option<Failure> {
    match code {
        rusqlite::ErrorCode::DiskFull => Some(Failure::StorageFull),
        rusqlite::ErrorCode::ReadOnly => Some(Failure::ReadOnlyStorage),
        rusqlite::ErrorCode::PermissionDenied => Some(Failure::PermissionDenied),
        _ => None,
    }
}

impl serde::Serialize for BackendError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.report().serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for BackendError {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::Remote(ErrorReport::deserialize(deserializer)?))
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) | Self::Transport(message) | Self::Classified { message, .. } => formatter.write_str(message),
            Self::Operation(error) => error.fmt(formatter),
            Self::TransportFailure(error) => error.fmt(formatter),
            Self::Remote(report) => formatter.write_str(&report.message),
            Self::Context { message, source } => write!(formatter, "{message}: {source}"),
        }
    }
}

impl Error for BackendError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Operation(error) => Some(error.as_ref()),
            Self::TransportFailure(error) => Some(error.as_ref()),
            Self::Context { source, .. } => Some(source.as_ref()),
            Self::Message(_) | Self::Classified { .. } | Self::Transport(_) | Self::Remote(_) => None,
        }
    }
}

impl From<String> for BackendError {
    fn from(error: String) -> Self {
        Self::Message(error)
    }
}
impl From<&str> for BackendError {
    fn from(error: &str) -> Self {
        Self::Message(error.to_owned())
    }
}
impl From<Box<dyn Error + Send + Sync>> for BackendError {
    fn from(error: Box<dyn Error + Send + Sync>) -> Self {
        Self::Operation(Arc::from(error))
    }
}

macro_rules! backend_error_from {
    ($($error:ty),+ $(,)?) => {$(
        impl From<$error> for BackendError {
            fn from(error: $error) -> Self { Self::operation(error) }
        }
    )+};
}
backend_error_from!(std::io::Error, rusqlite::Error, client_platform_runtime::executor::BackendExecutorError, std::time::SystemTimeError,);

#[cfg(test)]
mod classified_cause_tests {
    use super::*;

    #[test]
    fn local_and_cloud_space_failures_survive_context_and_worker_serialization() {
        for failure in [Failure::StorageFull, Failure::CloudStorageFull] {
            let error = BackendError::operation(failure).context("transfer remains pending");
            assert_eq!(error.failure(), Some(failure));
            let bytes = rmp_serde::to_vec_named(&error).unwrap();
            let received: BackendError = rmp_serde::from_slice(&bytes).unwrap();
            assert_eq!(received.failure(), Some(failure));
        }
    }
}
