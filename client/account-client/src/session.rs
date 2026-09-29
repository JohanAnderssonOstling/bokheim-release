use crate::ServerUrl;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum AuthError {
    Network(reqwest::Error),
    Io(std::io::Error),
    InvalidData(String),
    BadStatus(reqwest::StatusCode, String),
    Binding(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(error) => write!(formatter, "network error: {error}"),
            Self::Io(error) => write!(formatter, "io error: {error}"),
            Self::InvalidData(message) => write!(formatter, "invalid authentication data: {message}"),
            Self::BadStatus(status, message) => write!(formatter, "server returned {status}: {message}"),
            Self::Binding(message) => message.fmt(formatter),
        }
    }
}

impl std::error::Error for AuthError {}

impl AuthError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::BadStatus(status, _) if *status == reqwest::StatusCode::NOT_FOUND)
    }

    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::BadStatus(status, _) if *status == reqwest::StatusCode::UNAUTHORIZED)
    }
}

impl From<reqwest::Error> for AuthError {
    fn from(error: reqwest::Error) -> Self {
        Self::Network(error)
    }
}

impl From<binary_http::Error> for AuthError {
    fn from(error: binary_http::Error) -> Self {
        match error {
            binary_http::Error::Network(error) => Self::Network(error),
            binary_http::Error::InvalidData(message) => Self::InvalidData(message),
            binary_http::Error::BadStatus(status, message) => Self::BadStatus(status, message),
        }
    }
}

impl From<std::io::Error> for AuthError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone)]
pub struct Session {
    server_url: ServerUrl,
    user_id: String,
    email: String,
    token: String,
    expires_at: Option<i64>,
    refresh_token: Option<String>,
    refresh_expires_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct StoredSession {
    server_url: ServerUrl,
    user_id: String,
    email: String,
    token: String,
    #[serde(deserialize_with = "Option::deserialize")]
    expires_at: Option<i64>,
    #[serde(deserialize_with = "Option::deserialize")]
    refresh_token: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    refresh_expires_at: Option<i64>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("server_url", &self.server_url)
            .field("user_id", &self.user_id)
            .field("email", &self.email)
            .field("token", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "[redacted]"))
            .field("refresh_expires_at", &self.refresh_expires_at)
            .finish()
    }
}

impl Serialize for Session {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        StoredSession {
            server_url: self.server_url.clone(),
            user_id: self.user_id.clone(),
            email: self.email.clone(),
            token: self.token.clone(),
            expires_at: self.expires_at,
            refresh_token: self.refresh_token.clone(),
            refresh_expires_at: self.refresh_expires_at,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Session {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = StoredSession::deserialize(deserializer)?;
        Self::try_new_with_tokens(stored.server_url, stored.user_id, stored.email, stored.token, stored.expires_at, stored.refresh_token, stored.refresh_expires_at).map_err(serde::de::Error::custom)
    }
}

pub(crate) fn validate_identity(value: &str, name: &str) -> Result<(), AuthError> {
    if value.is_empty() || value.len() > 255 || value.chars().any(|character| character.is_control() || character == '/' || character == '\\') {
        return Err(AuthError::InvalidData(format!("{name} must be non-empty, bounded text without control characters or path separators")));
    }
    Ok(())
}

pub fn validate_user_id(value: &str) -> Result<(), AuthError> {
    validate_identity(value, "user id")
}

pub(crate) fn validate_email(value: &str) -> Result<(), AuthError> {
    if value.is_empty() || value.len() > account_contract::EMAIL_MAX_BYTES {
        return Err(AuthError::InvalidData("email must be a non-empty, bounded address".to_string()));
    }
    Ok(())
}

fn validate_token(value: &str) -> Result<(), AuthError> {
    if value.len() != 43 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
        return Err(AuthError::InvalidData("bearer token has an invalid shape".to_string()));
    }
    Ok(())
}

impl Session {
    pub fn try_new(server_url: ServerUrl, user_id: String, email: String, token: String) -> Result<Self, AuthError> {
        Self::try_new_with_expiry(server_url, user_id, email, token, None)
    }

    pub fn try_new_with_expiry(server_url: ServerUrl, user_id: String, email: String, token: String, expires_at: Option<i64>) -> Result<Self, AuthError> {
        Self::try_new_with_tokens(server_url, user_id, email, token, expires_at, None, None)
    }

    pub fn try_new_with_tokens(server_url: ServerUrl, user_id: String, email: String, token: String, expires_at: Option<i64>, refresh_token: Option<String>, refresh_expires_at: Option<i64>) -> Result<Self, AuthError> {
        validate_identity(&user_id, "user id")?;
        validate_email(&email)?;
        validate_token(&token)?;
        if expires_at.is_some_and(|expires_at| expires_at <= 0) {
            return Err(AuthError::InvalidData("session expiry must be a positive Unix timestamp".to_string()));
        }
        match (&refresh_token, refresh_expires_at) {
            (Some(refresh_token), Some(refresh_expires_at)) => {
                validate_token(refresh_token)?;
                if refresh_expires_at <= 0 {
                    return Err(AuthError::InvalidData("refresh expiry must be a positive Unix timestamp".to_string()));
                }
            }
            (None, None) => {}
            _ => return Err(AuthError::InvalidData("refresh token and expiry must be stored together".to_string())),
        }
        Ok(Self { server_url, user_id, email, token, expires_at, refresh_token, refresh_expires_at })
    }

    pub fn server_url(&self) -> &ServerUrl {
        &self.server_url
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    pub fn refresh_expires_at(&self) -> Option<i64> {
        self.refresh_expires_at
    }

    pub fn expires_within(&self, seconds: i64) -> bool {
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|duration| duration.as_secs() as i64).unwrap_or(i64::MAX);
        self.expires_at.is_some_and(|expires_at| expires_at <= now.saturating_add(seconds.max(0)))
    }

    pub fn refresh_expires_within(&self, seconds: i64) -> bool {
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|duration| duration.as_secs() as i64).unwrap_or(i64::MAX);
        self.refresh_expires_at.is_some_and(|expires_at| expires_at <= now.saturating_add(seconds.max(0)))
    }

    pub fn with_server_url(&self, server_url: ServerUrl) -> Result<Self, AuthError> {
        Self::try_new_with_tokens(server_url, self.user_id.clone(), self.email.clone(), self.token.clone(), self.expires_at, self.refresh_token.clone(), self.refresh_expires_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn deserialization_validates_every_field() {
        let valid = serde_json::json!({ "server_url": "https://example.com", "user_id": "user-1", "email": "reader@example.com", "token": TEST_TOKEN, "expires_at": null, "refresh_token": null, "refresh_expires_at": null });
        let session: Session = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(session.server_url().as_str(), "https://example.com/");
        for (field, invalid) in [("server_url", "file:///tmp/server"), ("user_id", "bad/id"), ("email", &"a".repeat(account_contract::EMAIL_MAX_BYTES + 1)), ("token", "short")] {
            let mut value = valid.clone();
            value[field] = serde_json::Value::String(invalid.to_string());
            assert!(serde_json::from_value::<Session>(value).is_err());
        }
    }

    #[test]
    fn debug_redacts_token_and_expiry_is_validated() {
        let session = Session::try_new(ServerUrl::parse("https://example.com").unwrap(), "user-1".into(), "reader@example.com".into(), TEST_TOKEN.into()).unwrap();
        assert!(!format!("{session:?}").contains(TEST_TOKEN));
        let invalid = serde_json::json!({ "server_url": "https://example.com", "user_id": "user-1", "email": "reader@example.com", "token": TEST_TOKEN, "expires_at": 0, "refresh_token": null, "refresh_expires_at": null });
        assert!(serde_json::from_value::<Session>(invalid).is_err());
    }

    #[test]
    fn refresh_credentials_round_trip_and_remain_redacted() {
        let session = Session::try_new_with_tokens(ServerUrl::parse("https://example.com").unwrap(), "user-1".into(), "reader@example.com".into(), TEST_TOKEN.into(), Some(100), Some("b".repeat(43)), Some(200)).unwrap();
        let stored = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&stored).unwrap();
        assert_eq!(restored.refresh_token(), Some("b".repeat(43).as_str()));
        let debug = format!("{session:?}");
        assert!(!debug.contains(TEST_TOKEN));
        assert!(!debug.contains(&"b".repeat(43)));
        assert!(Session::try_new_with_tokens(ServerUrl::parse("https://example.com").unwrap(), "user-1".into(), "reader@example.com".into(), TEST_TOKEN.into(), Some(100), Some("b".repeat(43)), None).is_err());
    }
}
