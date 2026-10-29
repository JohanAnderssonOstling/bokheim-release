//! The tab obtains a short-lived Google grant; the device backend verifies
//! its account and owns the connection used by library workers.

use crate::{AppClient, BackendError, GoogleAccessGrant, GoogleAccount};
pub use client_platform_web::google_login::GoogleLoginCancellation;
use client_platform_web::google_login::{GoogleLogin, GoogleLoginAttempt};

pub struct GoogleCloudLogin(GoogleLogin);
impl GoogleCloudLogin {
    pub async fn prepare(backend: &AppClient) -> Result<Self, BackendError> {
        let client = backend.google_cloud_web_client_id().await?;
        GoogleLogin::prepare(&client).map(Self).map_err(BackendError::message)
    }
    pub fn begin(&self) -> Result<GoogleCloudLoginAttempt, BackendError> {
        self.0.begin().map(GoogleCloudLoginAttempt).map_err(BackendError::message)
    }
}

pub struct GoogleCloudLoginAttempt(GoogleLoginAttempt);
impl GoogleCloudLoginAttempt {
    pub fn cancellation(&self) -> GoogleLoginCancellation {
        self.0.cancellation()
    }

    pub async fn finish(self, backend: &AppClient) -> Result<GoogleAccount, BackendError> {
        let value = self.0.wait().await.map_err(BackendError::message)?;
        let json = js_sys::JSON::stringify(&value).ok().and_then(|value| value.as_string()).ok_or_else(invalid_grant)?;
        let grant: GoogleAccessGrant = serde_json::from_str(&json).map_err(|_| invalid_grant())?;
        backend.connect_google_cloud_access_grant(grant).await
    }
}

fn invalid_grant() -> BackendError {
    BackendError::message("Google returned an invalid access grant")
}
