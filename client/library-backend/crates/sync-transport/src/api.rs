use serde::de::DeserializeOwned;
use serde::Serialize;

#[derive(Debug)]
pub enum TransportError {
    Network(reqwest::Error),
    InvalidData(String),
    BadStatus(reqwest::StatusCode, String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(error) => write!(formatter, "network error: {error}"),
            Self::InvalidData(message) => write!(formatter, "invalid synchronization data: {message}"),
            Self::BadStatus(status, message) => write!(formatter, "server returned {status}: {message}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<reqwest::Error> for TransportError {
    fn from(error: reqwest::Error) -> Self {
        Self::Network(error)
    }
}

impl From<binary_http::Error> for TransportError {
    fn from(error: binary_http::Error) -> Self {
        match error {
            binary_http::Error::Network(error) => Self::Network(error),
            binary_http::Error::InvalidData(message) => Self::InvalidData(message),
            binary_http::Error::BadStatus(status, message) => Self::BadStatus(status, message),
        }
    }
}

pub(crate) async fn send<T: Serialize + ?Sized, R: DeserializeOwned>(request: reqwest::RequestBuilder, value: &T) -> Result<R, TransportError> {
    let mut trace = crate::PerformanceTrace::new("binary_request", "encode_request_response_decode");
    send_with_trace(request, value, &mut trace).await
}

pub(crate) async fn send_with_trace<T: Serialize + ?Sized, R: DeserializeOwned>(request: reqwest::RequestBuilder, value: &T, trace: &mut crate::PerformanceTrace) -> Result<R, TransportError> {
    let request = request.header("x-bokheim-trace-id", trace.id());
    let result = binary_http::send(request, value).await.map_err(Into::into);
    trace.finish(result.is_ok());
    result
}

#[derive(Clone, Debug)]
pub struct SyncCredentials {
    server_url: binary_http::ServerUrl,
    access_token: String,
}

impl SyncCredentials {
    pub fn new(server_url: binary_http::ServerUrl, access_token: impl Into<String>) -> Self {
        Self { server_url, access_token: access_token.into() }
    }

    pub fn server_url(&self) -> &reqwest::Url {
        self.server_url.as_url()
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn endpoint(&self, path: &str) -> reqwest::Url {
        self.server_url.endpoint(path)
    }
}
