//! Errors retain their concrete source while they travel inside the backend.
use std::{error::Error, fmt, sync::Arc};

#[derive(Clone, Debug)]
pub enum BackendError {
    Message(String),
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
        let mut source: Option<&(dyn Error + 'static)> = Some(self);
        while let Some(error) = source {
            if matches!(error.downcast_ref::<Self>(), Some(Self::Transport(_) | Self::TransportFailure(_))) {
                causes.push(ErrorCause::Transport);
            } else if let Some(error) = error.downcast_ref::<std::io::Error>() {
                causes.push(ErrorCause::Io { kind: format!("{:?}", error.kind()), os_code: error.raw_os_error() });
            } else if let Some(rusqlite::Error::SqliteFailure(code, _)) = error.downcast_ref::<rusqlite::Error>() {
                causes.push(ErrorCause::Sqlite { extended_code: code.extended_code });
            } else if let Some(Self::Remote(report)) = error.downcast_ref::<Self>() {
                causes.extend(report.causes.clone());
                break;
            } else {
                causes.push(ErrorCause::Diagnostic { message: error.to_string() });
            }
            source = error.source();
        }
        ErrorReport { message: self.to_string(), causes }
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
            Self::Message(message) | Self::Transport(message) => formatter.write_str(message),
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
            Self::Message(_) | Self::Transport(_) | Self::Remote(_) => None,
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
mod tests {
    use super::*;
    #[test]
    fn operation_preserves_the_concrete_error_across_clones() {
        let error = BackendError::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let cloned = error.clone();
        let source = cloned.source().unwrap().downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
    }

    fn roundtrip(error: &BackendError) -> BackendError {
        crate::wire::decode_worker_message(&crate::wire::encode_worker_message(error).unwrap()).unwrap()
    }

    #[test]
    fn worker_roundtrip_preserves_io_details_and_context() {
        let error = BackendError::from(std::io::Error::from_raw_os_error(13)).context("opening library");
        let remote = roundtrip(&error);
        assert_eq!(remote.report(), error.report());
        assert!(remote.report().causes.iter().any(|cause| matches!(cause, ErrorCause::Io { os_code: Some(13), .. })));
        assert_eq!(roundtrip(&remote).report(), error.report());
    }

    #[test]
    fn worker_roundtrip_preserves_sqlite_codes() {
        let source = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(2067), None);
        let error = BackendError::from(source);
        assert!(roundtrip(&error).report().causes.contains(&ErrorCause::Sqlite { extended_code: 2067 }));
    }

    #[test]
    fn transport_classification_survives_wrapping_and_worker_hops() {
        let error = BackendError::operation(BackendError::transport("disconnected")).context("import");
        assert!(error.is_transport());
        assert!(roundtrip(&roundtrip(&error)).is_transport());
        let source = BackendError::from(std::io::Error::from_raw_os_error(13)).into_transport();
        let remote = roundtrip(&source);
        assert!(remote.is_transport());
        assert!(remote.report().causes.iter().any(|cause| matches!(cause, ErrorCause::Io { os_code: Some(13), .. })));
        assert!(!roundtrip(&BackendError::message("ordinary failure")).is_transport());
    }
}
