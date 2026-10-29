//! Google authorization-code flow with PKCE. The host opens the system browser
//! and supplies the callback; access/refresh tokens belong only to this device.

use crate::REQUEST_TIMEOUT;
use crate::{CloudError, metadata};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sync_common::storage::ProviderId;

pub use crate::oauth_types::{GoogleAccessGrant, GoogleAccount, GoogleOAuthConfig, GoogleTokens};

#[derive(Clone)]
pub struct GoogleOAuth {
    config: GoogleOAuthConfig,
    client: Client,
    authorization: Url,
    token: Url,
    userinfo: Url,
}

/// Neither a pending login nor a token set implements Debug: they contain
/// secrets and must never be logged or included in metadata synchronization.
pub struct PendingGoogleLogin {
    oauth: GoogleOAuth,
    redirect: Url,
    verifier: String,
    state: String,
}

impl GoogleOAuth {
    pub fn new(config: GoogleOAuthConfig) -> Result<Self, CloudError> {
        if config.client_id.trim().is_empty() || config.client_id.chars().any(char::is_control) {
            return Err(CloudError::InvalidRequest("Google Drive sign-in requires a configured OAuth client ID".into()));
        }
        let builder = Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder.redirect(reqwest::redirect::Policy::none());
        Ok(Self {
            config,
            client: builder.build().map_err(CloudError::transport)?,
            authorization: Url::parse("https://accounts.google.com/o/oauth2/v2/auth").expect("static authorization URL"),
            token: Url::parse("https://oauth2.googleapis.com/token").expect("static token URL"),
            userinfo: Url::parse("https://openidconnect.googleapis.com/v1/userinfo").expect("static user information URL"),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_userinfo(mut self, url: Url) -> Self {
        self.userinfo = url;
        self
    }

    pub fn begin_login(&self, redirect: Url) -> Result<PendingGoogleLogin, CloudError> {
        // Native hosts use an ephemeral loopback port. Browser hosts use their
        // registered HTTPS callback. Never authorize an arbitrary HTTP origin.
        let loopback = redirect.scheme() == "http" && matches!(redirect.host_str(), Some("127.0.0.1" | "[::1]" | "::1"));
        if (!loopback && redirect.scheme() != "https") || !redirect.username().is_empty() || redirect.password().is_some() || redirect.query().is_some() || redirect.fragment().is_some() {
            return Err(CloudError::InvalidRequest("Invalid Google authorization callback".into()));
        }
        Ok(PendingGoogleLogin { oauth: self.clone(), redirect, verifier: random_value()?, state: random_value()? })
    }

    pub async fn refresh(&self, previous: &GoogleTokens, now: u64) -> Result<GoogleTokens, CloudError> {
        let refresh = previous.refresh_token.as_deref().ok_or(CloudError::AuthenticationRequired)?;
        let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", self.config.client_id.as_str())];
        if let Some(secret) = &self.config.client_secret {
            form.push(("client_secret", secret));
        }
        let mut tokens = self.exchange(&form, now).await?;
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = previous.refresh_token.clone();
        }
        Ok(tokens)
    }

    pub async fn account(&self, tokens: &GoogleTokens) -> Result<GoogleAccount, CloudError> {
        account_from_tokens(&self.client, self.userinfo.clone(), tokens).await
    }

    async fn exchange(&self, form: &[(&str, &str)], now: u64) -> Result<GoogleTokens, CloudError> {
        let response = self.client.post(self.token.clone()).form(form).timeout(REQUEST_TIMEOUT).send().await.map_err(CloudError::transport)?;
        if response.status() == reqwest::StatusCode::BAD_REQUEST {
            return Err(CloudError::AuthenticationRequired);
        }
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            refresh_token: Option<String>,
            expires_in: u64,
            token_type: String,
            #[serde(default)]
            scope: String,
        }
        let token: TokenResponse = metadata(response).await?;
        if token.access_token.is_empty() || token.access_token.len() > 32 * 1024 || token.expires_in == 0 || !token.token_type.eq_ignore_ascii_case("bearer") {
            return Err(CloudError::InvalidResponse("Invalid Google authorization token response".into()));
        }
        // Google can return a successful code exchange after the user leaves
        // Drive unchecked on the granular consent screen. Never save that as
        // a usable Drive connection or refresh it into one later.
        if !token.scope.split_ascii_whitespace().any(|scope| scope == "https://www.googleapis.com/auth/drive") {
            return Err(CloudError::PermissionDenied);
        }
        Ok(GoogleTokens { access_token: token.access_token, refresh_token: token.refresh_token, expires_at: now.saturating_add(token.expires_in) })
    }
}

pub(crate) async fn platform_account(tokens: &GoogleTokens) -> Result<GoogleAccount, CloudError> {
    let builder = Client::builder();
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder.redirect(reqwest::redirect::Policy::none());
    let client = builder.build().map_err(CloudError::transport)?;
    account_from_tokens(&client, Url::parse("https://openidconnect.googleapis.com/v1/userinfo").expect("static Google userinfo URL"), tokens).await
}

async fn account_from_tokens(client: &Client, userinfo: Url, tokens: &GoogleTokens) -> Result<GoogleAccount, CloudError> {
    #[derive(Deserialize)]
    struct UserInfo {
        sub: ProviderId,
        email: String,
        name: Option<String>,
        email_verified: bool,
    }
    let response = client.get(userinfo).bearer_auth(&tokens.access_token).timeout(REQUEST_TIMEOUT).send().await.map_err(CloudError::transport)?;
    let user: UserInfo = metadata(response).await?;
    if !user.email_verified || user.email.trim().is_empty() {
        return Err(CloudError::InvalidResponse("Google did not return a verified account".into()));
    }
    Ok(GoogleAccount { id: user.sub, email: user.email, name: user.name })
}

impl PendingGoogleLogin {
    pub fn authorization_url(&self) -> Url {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()));
        let mut url = self.oauth.authorization.clone();
        url.query_pairs_mut().extend_pairs([
            ("client_id", self.oauth.config.client_id.as_str()),
            ("redirect_uri", self.redirect.as_str()),
            ("response_type", "code"),
            ("scope", "openid email https://www.googleapis.com/auth/drive"),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", &self.state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        url
    }

    /// Validate locally before making the one-time token exchange. The host
    /// should keep listening after unrelated loopback requests such as favicon.
    pub fn callback_code(&self, callback: &Url) -> Result<String, CloudError> {
        if callback.origin() != self.redirect.origin() || callback.path() != self.redirect.path() || callback.fragment().is_some() {
            return Err(CloudError::InvalidRequest("Google sign-in callback does not match this login".into()));
        }
        let pairs: Vec<_> = callback.query_pairs().collect();
        let states = pairs.iter().filter(|(key, _)| key == "state").map(|(_, value)| value.as_ref()).collect::<Vec<_>>();
        if states != [self.state.as_str()] {
            return Err(CloudError::InvalidRequest("Google sign-in callback state does not match this login".into()));
        }
        if pairs.iter().any(|(key, _)| key == "error") {
            return Err(CloudError::PermissionDenied);
        }
        let codes = pairs.iter().filter(|(key, _)| key == "code").map(|(_, value)| value.as_ref()).collect::<Vec<_>>();
        match codes.as_slice() {
            [code] if !code.is_empty() => Ok((*code).to_owned()),
            _ => Err(CloudError::InvalidRequest("Google sign-in callback is missing its authorization code".into())),
        }
    }

    pub async fn finish(self, callback: Url, now: u64) -> Result<(GoogleTokens, GoogleAccount), CloudError> {
        let code = self.callback_code(&callback)?;
        let mut form = vec![("grant_type", "authorization_code"), ("code", code.as_str()), ("client_id", self.oauth.config.client_id.as_str()), ("redirect_uri", self.redirect.as_str()), ("code_verifier", self.verifier.as_str())];
        if let Some(secret) = &self.oauth.config.client_secret {
            form.push(("client_secret", secret));
        }
        let tokens = self.oauth.exchange(&form, now).await?;
        let account = self.oauth.account(&tokens).await?;
        Ok((tokens, account))
    }
}

fn random_value() -> Result<String, CloudError> {
    let mut random = [0; 32];
    getrandom::fill(&mut random).map_err(|_| CloudError::InvalidRequest("Could not obtain secure randomness for cloud sign-in".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(random))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::{CloudConnections, ConnectionPersistence, GoogleConnection};
    use std::sync::{Arc, Mutex};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[derive(Default)]
    struct Connections(Mutex<Option<GoogleConnection>>);
    impl ConnectionPersistence for Connections {
        fn load_google(&self, account: &ProviderId) -> Result<Option<GoogleConnection>, CloudError> {
            Ok(self.0.lock().unwrap().clone().filter(|connection| &connection.account.id == account))
        }
        fn save_google(&self, connection: &GoogleConnection, _: Option<&str>) -> Result<bool, CloudError> {
            *self.0.lock().unwrap() = Some(connection.clone());
            Ok(true)
        }
    }

    async fn native_consent(scope: Option<&str>, complete: bool) -> (Result<GoogleAccount, CloudError>, Arc<Connections>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut token = serde_json::json!({"access_token":"fixture-google-token", "refresh_token":"fixture-refresh", "expires_in":3600, "token_type":"Bearer"});
        if let Some(scope) = scope {
            token["scope"] = scope.into();
        }
        let mut responses = vec![token.to_string()];
        if complete {
            responses.push(serde_json::json!({"sub":"verified-google-account", "email":"reader@example.com", "email_verified":true}).to_string());
        }
        let server = tokio::spawn(async move {
            for body in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 2048];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0);
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        break;
                    }
                }
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let mut oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "desktop.apps.googleusercontent.com".into(), client_secret: None }).unwrap();
        oauth.token = format!("http://{address}/token").parse().unwrap();
        oauth.userinfo = format!("http://{address}/userinfo").parse().unwrap();
        let saved = Arc::new(Connections::default());
        let connections = CloudConnections::new(oauth, saved.clone());
        let login = connections.begin_google_login("http://127.0.0.1:43210/callback".parse().unwrap()).unwrap();
        let callback = format!("http://127.0.0.1:43210/callback?state={}&code=fixture-code", login.state).parse().unwrap();
        let result = connections.finish_google_login(login, callback).await;
        server.await.unwrap();
        (result, saved)
    }

    #[tokio::test]
    async fn partial_native_consent_never_saves_a_drive_connection() {
        for scope in [None, Some("openid email"), Some("openid email https://www.googleapis.com/auth/drive.file")] {
            let (result, saved) = native_consent(scope, false).await;
            assert!(matches!(result, Err(CloudError::PermissionDenied)));
            assert!(saved.0.lock().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn full_native_drive_consent_saves_the_verified_account() {
        let (account, saved) = native_consent(Some("openid https://www.googleapis.com/auth/drive https://www.googleapis.com/auth/userinfo.email"), true).await;
        assert_eq!(account.unwrap().id.as_str(), "verified-google-account");
        assert!(saved.0.lock().unwrap().as_ref().unwrap().tokens.refresh_token.is_some());
    }

    async fn connect_sdk_grant(verified: bool) -> (Result<GoogleAccount, CloudError>, Arc<Connections>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert!(request.starts_with("get /userinfo "));
            assert!(request.contains("authorization: bearer fixture-google-token"));
            let body = serde_json::json!({"sub":"verified-google-account", "email":"reader@example.com", "email_verified":verified, "name":"Reader"}).to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let mut oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "web.apps.googleusercontent.com".into(), client_secret: None }).unwrap();
        oauth.userinfo = format!("http://{address}/userinfo").parse().unwrap();
        let saved = Arc::new(Connections::default());
        let connections = CloudConnections::new(oauth, saved.clone());
        let result = connections.connect_google_access_grant(GoogleAccessGrant { access_token: "fixture-google-token".into(), expires_in_seconds: 3600 }).await;
        server.await.unwrap();
        (result, saved)
    }

    #[tokio::test]
    async fn platform_grant_saves_only_the_google_verified_account_without_a_refresh_token() {
        let (account, saved) = connect_sdk_grant(true).await;
        let account = account.unwrap();
        assert_eq!(account.id.as_str(), "verified-google-account");
        let connection = saved.0.lock().unwrap().clone().unwrap();
        assert_eq!(connection.account, account);
        assert_eq!(connection.tokens.access_token, "fixture-google-token");
        assert!(connection.tokens.refresh_token.is_none());
        let oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "web.apps.googleusercontent.com".into(), client_secret: None }).unwrap();
        assert!(matches!(oauth.refresh(&connection.tokens, connection.tokens.expires_at).await, Err(CloudError::AuthenticationRequired)));
    }

    #[tokio::test]
    async fn platform_grant_cannot_save_an_unverified_google_account() {
        let (result, saved) = connect_sdk_grant(false).await;
        assert!(matches!(result, Err(CloudError::InvalidResponse(_))));
        assert!(saved.0.lock().unwrap().is_none());
    }

    #[test]
    fn platform_grant_rejects_invalid_tokens_and_lifetimes_without_logging_credentials() {
        let grant = GoogleAccessGrant { access_token: "fixture-google-token".into(), expires_in_seconds: 3600 };
        assert!(!format!("{grant:?}").contains(&grant.access_token));
        for seconds in [0, 86401, u32::MAX] {
            assert!(GoogleAccessGrant { expires_in_seconds: seconds, ..grant.clone() }.into_tokens(10).is_err());
        }
        for token in ["", "bearer token", "line\nbreak", "control\u{7f}"] {
            assert!(GoogleAccessGrant { access_token: token.into(), ..grant.clone() }.into_tokens(10).is_err());
        }
        assert_eq!(grant.into_tokens(10).unwrap().expires_at, 3610);
    }

    #[test]
    fn login_uses_pkce_and_rejects_unrelated_or_duplicate_callback_parameters() {
        let oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "desktop.apps.googleusercontent.com".into(), client_secret: None }).unwrap();
        let login = oauth.begin_login(Url::parse("http://127.0.0.1:12345/oauth/callback").unwrap()).unwrap();
        let url = login.authorization_url();
        assert!(!url.as_str().contains(&login.verifier));
        assert!(url.query_pairs().any(|(key, value)| key == "code_challenge_method" && value == "S256"));
        let callback = Url::parse(&format!("http://127.0.0.1:12345/oauth/callback?state={}&code=code", login.state)).unwrap();
        assert_eq!(login.callback_code(&callback).unwrap(), "code");
        let mut wrong = callback.clone();
        wrong.set_port(Some(12346)).unwrap();
        assert!(login.callback_code(&wrong).is_err());
        let mut duplicate = callback;
        duplicate.query_pairs_mut().append_pair("state", &login.state);
        assert!(login.callback_code(&duplicate).is_err());
        let second = oauth.begin_login(Url::parse("http://127.0.0.1:12345/oauth/callback").unwrap()).unwrap();
        assert_ne!(login.state, second.state);
        assert_ne!(login.verifier, second.verifier);
        assert!(oauth.begin_login(Url::parse("http://example.com/callback").unwrap()).is_err());
    }
}
