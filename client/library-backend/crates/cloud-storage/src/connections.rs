//! Device-owned connections. No Bokheim account or file-storage service is
//! needed to browse or read from a connected cloud provider.

#[cfg(test)]
use crate::StorageFuture;
use crate::google_drive::{DriveUpload, GoogleDrive, UploadProgress};
use crate::oauth::{GoogleAccessGrant, GoogleAccount, GoogleOAuth, GoogleTokens, PendingGoogleLogin};
use crate::scanner::{ScanCursor, ScanPage};
use crate::{ChangesPage, CloudDirectoryIdentity, CloudEntry, CloudError, CloudFolder, FolderPage};
use reqwest::Url;
use std::{future::Future, sync::Arc};
use sync_common::storage::{CloudProvider, ProviderFile, ProviderId, ProviderScope};
use tokio::sync::Mutex;

pub use crate::connection_types::{ConnectionPersistence, GoogleAuthorization, GoogleConnection};

#[derive(Clone)]
pub struct CloudConnections {
    oauth: Option<GoogleOAuth>,
    persistence: Option<Arc<dyn ConnectionPersistence>>,
    platform_authorization: Option<Arc<dyn GoogleAuthorization>>,
    refresh: Arc<Mutex<()>>,
    google_drive_available: fn() -> bool,
    #[cfg(feature = "test-support")]
    test_endpoints: Arc<std::collections::HashMap<ProviderScope, Url>>,
}

impl Default for CloudConnections {
    fn default() -> Self {
        Self {
            oauth: None,
            persistence: None,
            platform_authorization: None,
            refresh: Default::default(),
            google_drive_available: crate::google_drive_available,
            #[cfg(feature = "test-support")]
            test_endpoints: Default::default(),
        }
    }
}

impl CloudConnections {
    #[cfg(feature = "test-support")]
    pub fn test_google_drive(&self, scope: ProviderScope, endpoint: Url) -> Result<ConnectedDrive, CloudError> {
        self.clone().with_test_google_drive_endpoint(scope.clone(), endpoint)?.google_drive(scope)
    }

    #[cfg(feature = "test-support")]
    pub fn with_test_google_drive_endpoint(mut self, scope: ProviderScope, endpoint: Url) -> Result<Self, CloudError> {
        GoogleDrive::for_test(scope.clone(), endpoint.clone())?;
        Arc::make_mut(&mut self.test_endpoints).insert(scope, endpoint);
        Ok(self)
    }

    pub fn new(oauth: GoogleOAuth, persistence: Arc<dyn ConnectionPersistence>) -> Self {
        Self { oauth: Some(oauth), persistence: Some(persistence), ..Self::default() }
    }

    pub fn platform(persistence: Arc<dyn ConnectionPersistence>, authorization: Arc<dyn GoogleAuthorization>) -> Self {
        Self { persistence: Some(persistence), platform_authorization: Some(authorization), ..Self::default() }
    }

    async fn account(&self, tokens: &GoogleTokens) -> Result<GoogleAccount, CloudError> {
        match &self.oauth {
            Some(oauth) => oauth.account(tokens).await,
            None => crate::oauth::platform_account(tokens).await,
        }
    }

    pub async fn connect_platform_google(&self) -> Result<GoogleAccount, CloudError> {
        self.require_google_drive()?;
        let grant = self.platform_authorization.as_ref().ok_or_else(configuration_error)?.grant(None, true, None).await?;
        self.connect_google_access_grant(grant).await
    }

    fn require_google_drive(&self) -> Result<(), CloudError> {
        if (self.google_drive_available)() { Ok(()) } else { Err(crate::availability::unavailable()) }
    }

    pub fn begin_google_login(&self, redirect: Url) -> Result<PendingGoogleLogin, CloudError> {
        self.require_google_drive()?;
        self.oauth.as_ref().ok_or_else(configuration_error)?.begin_login(redirect)
    }

    pub async fn finish_google_login(&self, login: PendingGoogleLogin, callback: Url) -> Result<GoogleAccount, CloudError> {
        self.require_google_drive()?;
        let (tokens, account) = login.finish(callback, now()?).await?;
        let connection = GoogleConnection { account: account.clone(), tokens };
        if !self.persistence.as_ref().ok_or_else(configuration_error)?.save_google(&connection, None)? {
            return Err(CloudError::Retryable("Could not save the cloud connection".into()));
        }
        Ok(account)
    }

    /// Obtain the account from Google rather than trusting a user ID supplied
    /// by the host page. Token-only grants require another user authorization
    /// after expiry; they do not create a refresh token or a Bokheim session.
    pub async fn connect_google_access_grant(&self, grant: GoogleAccessGrant) -> Result<GoogleAccount, CloudError> {
        self.require_google_drive()?;
        let tokens = grant.into_tokens(now()?)?;
        let account = self.account(&tokens).await?;
        let connection = GoogleConnection { account: account.clone(), tokens };
        if !self.persistence.as_ref().ok_or_else(configuration_error)?.save_google(&connection, None)? {
            return Err(CloudError::Retryable("Could not save the cloud connection".into()));
        }
        Ok(account)
    }

    pub fn google_drive(&self, scope: ProviderScope) -> Result<ConnectedDrive, CloudError> {
        if scope.provider != CloudProvider::GoogleDrive {
            return Err(CloudError::InvalidRequest("This cloud provider is not implemented yet".into()));
        }
        self.require_google_drive()?;
        #[cfg(feature = "test-support")]
        if let Some(endpoint) = self.test_endpoints.get(&scope) {
            return Ok(ConnectedDrive { drive: GoogleDrive::for_test(scope, endpoint.clone())?, connections: self.clone() });
        }
        Ok(ConnectedDrive { drive: GoogleDrive::new(scope)?, connections: self.clone() })
    }

    async fn token(&self, scope: &ProviderScope, rejected: Option<&str>) -> Result<String, CloudError> {
        self.require_google_drive()?;
        let persistence = self.persistence.as_ref().ok_or_else(configuration_error)?;
        let load = || -> Result<GoogleConnection, CloudError> {
            let connection = persistence.load_google(&scope.account_id)?.ok_or(CloudError::AuthenticationRequired)?;
            if connection.account.id != scope.account_id {
                return Err(CloudError::AuthenticationRequired);
            }
            Ok(connection)
        };
        let needs_refresh = |connection: &GoogleConnection, now: u64| connection.tokens.expires_at <= now.saturating_add(60) || rejected.is_some_and(|token| token == connection.tokens.access_token);
        let mut connection = load()?;
        if !needs_refresh(&connection, now()?) {
            return Ok(connection.tokens.access_token);
        }
        let _refresh = self.refresh.lock().await;
        connection = load()?;
        if !needs_refresh(&connection, now()?) {
            return Ok(connection.tokens.access_token);
        }
        let previous = connection.tokens.access_token.clone();
        if let Some(platform) = &self.platform_authorization {
            let grant = platform.grant(Some(&connection.account), false, rejected).await?;
            let tokens = grant.into_tokens(now()?)?;
            if self.account(&tokens).await?.id != connection.account.id {
                return Err(CloudError::AuthenticationRequired);
            }
            connection.tokens = tokens;
        } else {
            connection.tokens = self.oauth.as_ref().ok_or_else(configuration_error)?.refresh(&connection.tokens, now()?).await?;
        }
        if !persistence.save_google(&connection, Some(&previous))? {
            return Err(CloudError::AuthenticationRequired);
        }
        Ok(connection.tokens.access_token)
    }

    async fn authenticated<T, F, Fut>(&self, scope: &ProviderScope, mut operation: F) -> Result<T, CloudError>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<T, CloudError>>,
    {
        let token = self.token(scope, None).await?;
        match operation(token.clone()).await {
            Err(CloudError::AuthenticationRequired) => operation(self.token(scope, Some(&token)).await?).await,
            result => result,
        }
    }
}

#[derive(Clone)]
pub struct ConnectedDrive {
    drive: GoogleDrive,
    connections: CloudConnections,
}

impl ConnectedDrive {
    pub fn scope(&self) -> &ProviderScope {
        self.drive.scope()
    }

    pub async fn list_folder(&self, folder: &ProviderId, page: Option<&str>, folders_only: bool) -> Result<FolderPage, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.list_folder(&token, folder, page, folders_only).await }).await
    }
    pub async fn describe(&self, file: &ProviderId) -> Result<CloudEntry, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.describe(&token, file).await }).await
    }
    pub async fn read_range(&self, file: &ProviderFile, offset: u64, length: usize) -> Result<Vec<u8>, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.read_range(&token, file, offset, length).await }).await
    }
    pub async fn start_checkpoint(&self) -> Result<String, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.start_checkpoint(&token).await }).await
    }
    pub async fn changes(&self, checkpoint: &str) -> Result<ChangesPage, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.changes(&token, checkpoint).await }).await
    }
    pub async fn scan_page(&self, cursor: &ScanCursor) -> Result<ScanPage, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { cursor.next_page(&self.drive, &token).await }).await
    }
    pub async fn create_folder(&self, parent: &ProviderId, name: &str) -> Result<CloudFolder, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.create_folder(&token, parent, name).await }).await
    }
    pub async fn create_library_folder(&self, parent: &ProviderId, name: &str, identity: &CloudDirectoryIdentity) -> Result<CloudFolder, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.create_library_folder(&token, parent, name, Some(identity)).await }).await
    }
    pub async fn allocate_file_id(&self) -> Result<ProviderId, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.allocate_file_id(&token).await }).await
    }
    pub async fn begin_upload(&self, parent: &ProviderId, file: &ProviderId, name: &str, size: u64) -> Result<DriveUpload, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.begin_upload(&token, parent, file, name, size).await }).await
    }
    pub async fn upload_chunk(&self, upload: &DriveUpload, offset: u64, bytes: Vec<u8>) -> Result<UploadProgress, CloudError> {
        self.connections
            .authenticated(self.scope(), |token| {
                let bytes = bytes.clone();
                async move { self.drive.upload_chunk(&token, upload, offset, bytes).await }
            })
            .await
    }
    pub async fn upload_status(&self, upload: &DriveUpload) -> Result<UploadProgress, CloudError> {
        self.connections.authenticated(self.scope(), |token| async move { self.drive.upload_status(&token, upload).await }).await
    }
}

fn configuration_error() -> CloudError {
    CloudError::InvalidRequest("Google Drive sign-in requires a configured OAuth client ID".into())
}
fn now() -> Result<u64, CloudError> {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|duration| duration.as_secs()).map_err(|_| CloudError::InvalidRequest("Device clock is before the Unix epoch".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::GoogleOAuthConfig;
    use std::sync::atomic::{AtomicBool, Ordering};
    use sync_common::storage::CloudProvider;

    struct Saved(std::sync::Mutex<Option<GoogleConnection>>);
    impl ConnectionPersistence for Saved {
        fn load_google(&self, _: &ProviderId) -> Result<Option<GoogleConnection>, CloudError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save_google(&self, connection: &GoogleConnection, previous: Option<&str>) -> Result<bool, CloudError> {
            let mut saved = self.0.lock().unwrap();
            if previous.is_some_and(|token| !saved.as_ref().is_some_and(|saved| saved.tokens.access_token == token)) {
                return Ok(false);
            }
            *saved = Some(connection.clone());
            Ok(true)
        }
    }
    struct Platform {
        saved: Arc<Saved>,
        disconnect: bool,
        rejected: Option<&'static str>,
    }
    impl GoogleAuthorization for Platform {
        fn grant<'a>(&'a self, account: Option<&'a GoogleAccount>, interactive: bool, rejected: Option<&'a str>) -> StorageFuture<'a, GoogleAccessGrant> {
            Box::pin(async move {
                assert!(!interactive, "renewal cannot launch account or consent UI");
                assert_eq!(account.unwrap().id, scope().account_id);
                assert_eq!(rejected, self.rejected);
                if self.disconnect {
                    *self.saved.0.lock().unwrap() = None;
                }
                Ok(GoogleAccessGrant { access_token: "renewed-token".into(), expires_in_seconds: 300 })
            })
        }
    }
    async fn platform_renewal(subject: &'static str, disconnect: bool, rejected: Option<&'static str>) -> (Result<String, CloudError>, Arc<Saved>) {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/userinfo", listener.local_addr().unwrap()).parse().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 1024];
                let count = socket.read(&mut bytes).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&bytes[..count]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(String::from_utf8(request).unwrap().contains("Bearer renewed-token"));
            let body = serde_json::json!({"sub":subject,"email":"reader@example.com","email_verified":true}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let saved = Arc::new(Saved(std::sync::Mutex::new(Some(GoogleConnection {
            account: GoogleAccount { id: scope().account_id, email: "reader@example.com".into(), name: None },
            tokens: GoogleTokens { access_token: "old-token".into(), refresh_token: None, expires_at: if rejected.is_some() { u64::MAX } else { 0 } },
        }))));
        let oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "test-client".into(), client_secret: None }).unwrap().with_test_userinfo(endpoint);
        let mut connections = CloudConnections::platform(saved.clone(), Arc::new(Platform { saved: saved.clone(), disconnect, rejected }));
        connections.oauth = Some(oauth);
        let result = connections.token(&scope(), rejected).await;
        server.await.unwrap();
        (result, saved)
    }
    #[tokio::test]
    async fn platform_renews_expired_and_rejected_tokens_without_a_refresh_token() {
        for rejected in [None, Some("old-token")] {
            let (result, saved) = platform_renewal("account", false, rejected).await;
            assert_eq!(result.unwrap(), "renewed-token");
            let saved = saved.0.lock().unwrap();
            let tokens = &saved.as_ref().unwrap().tokens;
            assert_eq!(tokens.access_token, "renewed-token");
            assert!(tokens.refresh_token.is_none());
        }
    }
    #[tokio::test]
    async fn platform_renewal_cannot_replace_a_different_google_account() {
        let (result, saved) = platform_renewal("another-account", false, None).await;
        assert!(matches!(result, Err(CloudError::AuthenticationRequired)));
        assert_eq!(saved.0.lock().unwrap().as_ref().unwrap().tokens.access_token, "old-token");
    }
    #[tokio::test]
    async fn disconnect_during_platform_renewal_cannot_restore_the_connection() {
        let (result, saved) = platform_renewal("account", true, None).await;
        assert!(matches!(result, Err(CloudError::AuthenticationRequired)));
        assert!(saved.0.lock().unwrap().is_none());
    }

    struct UnusedCredentials;
    impl ConnectionPersistence for UnusedCredentials {
        fn load_google(&self, _: &ProviderId) -> Result<Option<GoogleConnection>, CloudError> {
            panic!("disabled Drive must not access saved credentials")
        }
        fn save_google(&self, _: &GoogleConnection, _: Option<&str>) -> Result<bool, CloudError> {
            panic!("disabled Drive must not save credentials")
        }
    }

    fn connections(available: fn() -> bool) -> CloudConnections {
        let oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "fixture-client".into(), client_secret: None }).unwrap();
        let mut connections = CloudConnections::new(oauth, Arc::new(UnusedCredentials));
        connections.google_drive_available = available;
        connections
    }

    fn scope() -> ProviderScope {
        ProviderScope { provider: CloudProvider::GoogleDrive, account_id: ProviderId::new("account").unwrap(), container_id: ProviderId::new("my_drive").unwrap() }
    }

    fn disabled<T>(result: Result<T, CloudError>) {
        assert!(matches!(result, Err(CloudError::InvalidRequest(message)) if message.contains("Google Play services")));
    }

    #[tokio::test]
    async fn unavailable_device_rejects_login_grants_and_connections_before_accessing_credentials() {
        let connections = connections(|| false);
        disabled(connections.begin_google_login("http://127.0.0.1:1234/callback".parse().unwrap()));
        disabled(connections.google_drive(scope()));
        disabled(connections.connect_google_access_grant(GoogleAccessGrant { access_token: "fixture-token".into(), expires_in_seconds: 3600 }).await);
    }

    #[tokio::test]
    async fn an_existing_connection_rechecks_device_availability_before_each_request() {
        static AVAILABLE: AtomicBool = AtomicBool::new(true);
        let connections = connections(|| AVAILABLE.load(Ordering::Acquire));
        let drive = connections.google_drive(scope()).unwrap();
        AVAILABLE.store(false, Ordering::Release);
        disabled(drive.list_folder(&ProviderId::new("folder").unwrap(), None, false).await);
        AVAILABLE.store(true, Ordering::Release);
        assert!(connections.google_drive(scope()).is_ok());
    }

    #[tokio::test]
    async fn folder_lookup_checks_all_pages_and_ignores_other_parents() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
        let server = tokio::spawn(async move {
            for page in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut bytes = [0; 4096];
                    let count = socket.read(&mut bytes).await.unwrap();
                    assert_ne!(count, 0);
                    request.extend_from_slice(&bytes[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                assert_eq!(request.contains("pageToken=second"), page == 1);
                assert!(request.to_ascii_lowercase().contains("authorization: bearer fixture-token"));
                let folder = |id: &str, name: &str, parent: &str| serde_json::json!({"id":id,"name":name,"parents":[parent],"mimeType":"application/vnd.google-apps.folder"});
                let body = if page == 0 {
                    serde_json::json!({"nextPageToken":"second","files":[folder("z-match", "Shelf", "parent")]})
                } else {
                    serde_json::json!({"files":[folder("a-other-parent", "Shelf", "elsewhere"),folder("b-other-name", "Other", "parent"),folder("m-match", "Shelf", "parent")]})
                }
                .to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let saved = Arc::new(Saved(std::sync::Mutex::new(Some(GoogleConnection {
            account: GoogleAccount { id: scope().account_id, email: "reader@example.com".into(), name: None },
            tokens: GoogleTokens { access_token: "fixture-token".into(), refresh_token: None, expires_at: u64::MAX },
        }))));
        let connections = CloudConnections::new(GoogleOAuth::new(GoogleOAuthConfig { client_id: "fixture".into(), client_secret: None }).unwrap(), saved);
        let drive = ConnectedDrive { drive: GoogleDrive::for_test(scope(), endpoint).unwrap(), connections };
        let found = drive.find_folder(&ProviderId::new("parent").unwrap(), |folder| folder.name == "Shelf").await.unwrap().unwrap();
        assert_eq!(found.id.as_str(), "m-match");
        server.await.unwrap();
    }
}
