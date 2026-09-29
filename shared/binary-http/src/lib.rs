use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use sync_common::transport as wire;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServerUrl(reqwest::Url);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerUrlError(String);

impl ServerUrl {
    pub fn parse(value: &str) -> Result<Self, ServerUrlError> {
        let mut url = reqwest::Url::parse(value).map_err(|error| ServerUrlError(format!("invalid server URL: {error}")))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || url.cannot_be_a_base() {
            return Err(ServerUrlError("server URL must be an absolute HTTP or HTTPS origin".to_string()));
        }
        let host = url.host_str().expect("host presence checked above");
        let ip_literal = host.strip_prefix('[').and_then(|host| host.strip_suffix(']')).unwrap_or(host);
        let is_loopback = host.eq_ignore_ascii_case("localhost") || ip_literal.parse::<std::net::IpAddr>().is_ok_and(|address| address.is_loopback());
        if url.scheme() == "http" && !is_loopback {
            return Err(ServerUrlError("remote services must use HTTPS; HTTP is allowed only for loopback development".to_string()));
        }
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(ServerUrlError("server URL must not contain credentials, a query, or a fragment".to_string()));
        }
        if url.path() != "/" && !url.path().is_empty() {
            return Err(ServerUrlError("server URL must not contain a path".to_string()));
        }
        url.set_path("/");
        Ok(Self(url))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_url(&self) -> &reqwest::Url {
        &self.0
    }

    pub fn endpoint(&self, path: &str) -> reqwest::Url {
        self.0.join(path.trim_start_matches('/')).expect("validated server origin accepts relative API paths")
    }
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Display for ServerUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for ServerUrlError {}

impl FromStr for ServerUrl {
    type Err = ServerUrlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for ServerUrl {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ServerUrl {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug)]
pub enum Error {
    Network(reqwest::Error),
    InvalidData(String),
    BadStatus(reqwest::StatusCode, String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(error) => write!(formatter, "network error: {error}"),
            Self::InvalidData(message) => write!(formatter, "invalid binary HTTP data: {message}"),
            Self::BadStatus(status, message) => write!(formatter, "server returned {status}: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Network(error) => Some(error),
            Self::InvalidData(_) | Self::BadStatus(_, _) => None,
        }
    }
}

pub fn headers(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.header(reqwest::header::ACCEPT, wire::MEDIA_TYPE)
}

pub fn body<T: Serialize + ?Sized>(request: reqwest::RequestBuilder, value: &T) -> Result<reqwest::RequestBuilder, Error> {
    let encoded = wire::encode(value).map_err(|error| Error::InvalidData(error.to_string()))?;
    Ok(headers(request).header(reqwest::header::CONTENT_TYPE, wire::MEDIA_TYPE).body(encoded))
}

pub async fn send<T: Serialize + ?Sized, R: DeserializeOwned>(request: reqwest::RequestBuilder, value: &T) -> Result<R, Error> {
    decode(body(request, value)?.send().await.map_err(Error::Network)?).await
}

pub async fn send_status<T: Serialize + ?Sized>(request: reqwest::RequestBuilder, value: &T) -> Result<(), Error> {
    ensure_ok(body(request, value)?.send().await.map_err(Error::Network)?).await?;
    Ok(())
}

pub async fn receive<R: DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<R, Error> {
    decode(headers(request).send().await.map_err(Error::Network)?).await
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, Error> {
    let response = ensure_ok(response).await?;
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).unwrap_or_default().to_owned();
    if response.headers().get(reqwest::header::CONTENT_ENCODING).and_then(|value| value.to_str().ok()).map(str::trim).is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("identity")) {
        return Err(Error::InvalidData("compressed API responses are not supported".to_owned()));
    }
    let bytes = response.bytes().await.map_err(Error::Network)?;
    if !wire::is_current_media_type(&content_type) {
        return Err(Error::InvalidData(format!("unsupported API response content type: {content_type}")));
    }
    wire::decode(&bytes, wire::MAX_DECODED_RESPONSE_BYTES).map_err(|error| Error::InvalidData(error.to_string()))
}

pub async fn ensure_ok(response: reqwest::Response) -> Result<reqwest::Response, Error> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    Err(Error::BadStatus(status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_accepts_only_normalized_origins() {
        assert_eq!(ServerUrl::parse("https://example.com").unwrap().as_str(), "https://example.com/");
        assert_eq!(ServerUrl::parse("http://127.0.0.1:8080/").unwrap().as_str(), "http://127.0.0.1:8080/");
        assert_eq!(ServerUrl::parse("http://[::1]:8080/").unwrap().as_str(), "http://[::1]:8080/");
        assert!(ServerUrl::parse("ftp://example.com").is_err());
        assert!(ServerUrl::parse("http://example.com").is_err());
        assert!(ServerUrl::parse("http://192.168.1.10:8080").is_err());
        assert!(ServerUrl::parse("https://user@example.com").is_err());
        assert!(ServerUrl::parse("https://example.com/base").is_err());
        assert!(ServerUrl::parse("https://example.com/?token=x").is_err());
    }

    #[test]
    fn endpoints_are_derived_without_string_concatenation() {
        let server = ServerUrl::parse("https://example.com").unwrap();
        assert_eq!(server.endpoint("api/sync/exchange").as_str(), "https://example.com/api/sync/exchange");
        assert_eq!(server.endpoint("/api/blobs/x").as_str(), "https://example.com/api/blobs/x");
    }
}
