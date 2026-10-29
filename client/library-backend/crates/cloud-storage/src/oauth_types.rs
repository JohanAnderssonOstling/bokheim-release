//! Device-owned account data retained even when provider support is disabled.

#[cfg(feature = "google-drive")]
use crate::CloudError;
use serde::{Deserialize, Serialize};
use sync_common::storage::ProviderId;

/// Public desktop client credentials supplied by the distribution. Desktop
/// client secrets are not treated as confidential or used for app identity.
#[derive(Clone)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GoogleTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
}

/// A short-lived grant returned by Google's browser or platform SDK. This is
/// device-owned authentication data and must never enter library sync.
#[derive(Clone, Serialize, Deserialize)]
pub struct GoogleAccessGrant {
    pub access_token: String,
    pub expires_in_seconds: u32,
}

impl std::fmt::Debug for GoogleAccessGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleAccessGrant").field("expires_in_seconds", &self.expires_in_seconds).finish_non_exhaustive()
    }
}

#[cfg(feature = "google-drive")]
impl GoogleAccessGrant {
    pub(crate) fn into_tokens(self, now: u64) -> Result<GoogleTokens, CloudError> {
        if self.access_token.is_empty() || self.access_token.len() > 32 * 1024 || self.access_token.chars().any(char::is_whitespace) || self.access_token.chars().any(char::is_control) || !(1..=86400).contains(&self.expires_in_seconds) {
            return Err(CloudError::InvalidResponse("Google returned an invalid access grant".into()));
        }
        Ok(GoogleTokens { access_token: self.access_token, refresh_token: None, expires_at: now.saturating_add(u64::from(self.expires_in_seconds)) })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoogleAccount {
    pub id: ProviderId,
    pub email: String,
    pub name: Option<String>,
}
