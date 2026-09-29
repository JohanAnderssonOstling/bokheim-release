//! Account capability injected into library-owned remote workers.

use std::sync::Arc;

pub use account_client::AccountSessionError;

pub type SharedSyncCredentials = Arc<dyn sync_transport::CredentialsSource>;

#[derive(Clone)]
pub struct LibraryAccount {
    current: Arc<dyn Fn() -> Result<Option<account_client::Session>, AccountSessionError> + Send + Sync>,
    require: Arc<dyn Fn() -> Result<account_client::Session, AccountSessionError> + Send + Sync>,
    subscribe: Arc<dyn Fn() -> tokio::sync::watch::Receiver<u64> + Send + Sync>,
    refresh_if_needed: Arc<dyn Fn() -> account_client::SessionFuture<'static, Result<Option<account_client::Session>, AccountSessionError>> + Send + Sync>,
    refresh_after_unauthorized: Arc<dyn Fn(String) -> account_client::SessionFuture<'static, Result<Option<account_client::Session>, AccountSessionError>> + Send + Sync>,
    create_library: Arc<dyn Fn(sync_common::LibraryId, String) -> account_client::SessionFuture<'static, Result<(), AccountSessionError>> + Send + Sync>,
    credentials: SharedSyncCredentials,
}

impl std::fmt::Debug for LibraryAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("LibraryAccount").finish_non_exhaustive()
    }
}

impl LibraryAccount {
    pub fn current(&self) -> Result<Option<account_client::Session>, AccountSessionError> {
        (self.current)()
    }
    pub fn require(&self) -> Result<account_client::Session, AccountSessionError> {
        (self.require)()
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        (self.subscribe)()
    }
    pub fn refresh_if_needed(&self) -> account_client::SessionFuture<'static, Result<Option<account_client::Session>, AccountSessionError>> {
        (self.refresh_if_needed)()
    }
    pub fn refresh_after_unauthorized(&self, token: impl Into<String>) -> account_client::SessionFuture<'static, Result<Option<account_client::Session>, AccountSessionError>> {
        (self.refresh_after_unauthorized)(token.into())
    }
    pub fn create_library(&self, library_id: sync_common::LibraryId, name: String) -> account_client::SessionFuture<'static, Result<(), AccountSessionError>> {
        (self.create_library)(library_id, name)
    }
    pub fn credentials(&self) -> SharedSyncCredentials {
        self.credentials.clone()
    }
}

#[derive(Clone, Debug)]
pub struct LibraryAccountSession<P: account_client::SessionPersistence>(account_client::AccountSession<P>);

impl<P: account_client::SessionPersistence> LibraryAccountSession<P> {
    pub fn load(persistence: P, server: account_client::ServerUrl) -> Result<Self, AccountSessionError> {
        account_client::AccountSession::load(persistence, server).map(Self)
    }
    pub fn shared_sync_credentials(&self) -> SharedSyncCredentials
    where
        Self: sync_transport::CredentialsSource + 'static,
    {
        Arc::new(self.clone())
    }
}

impl<P> LibraryAccountSession<P>
where
    P: account_client::SessionPersistence + Send + Sync + 'static,
{
    pub fn library_account(&self) -> LibraryAccount {
        let current = self.clone();
        let require = self.clone();
        let subscribe = self.clone();
        let refresh_if_needed = self.clone();
        let refresh_after_unauthorized = self.clone();
        let create_library = self.clone();
        LibraryAccount {
            current: Arc::new(move || current.current()),
            require: Arc::new(move || require.require()),
            subscribe: Arc::new(move || subscribe.subscribe()),
            refresh_if_needed: Arc::new(move || {
                let account = refresh_if_needed.clone();
                Box::pin(async move { account.refresh_if_needed().await })
            }),
            refresh_after_unauthorized: Arc::new(move |token| {
                let account = refresh_after_unauthorized.clone();
                Box::pin(async move { account.refresh_after_unauthorized(&token).await })
            }),
            create_library: Arc::new(move |library_id, name| {
                let account = create_library.clone();
                Box::pin(async move { account.request(|session| Box::pin(account_client::create_library(session, library_id, name.clone()))).await.map(|_| ()) })
            }),
            credentials: self.shared_sync_credentials(),
        }
    }
}

impl<P: account_client::SessionPersistence> std::ops::Deref for LibraryAccountSession<P> {
    type Target = account_client::AccountSession<P>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<P> sync_transport::CredentialsSource for LibraryAccountSession<P>
where
    P: account_client::SessionPersistence + Send + Sync,
{
    fn credentials(&self) -> Option<sync_transport::SyncCredentials> {
        self.credentials_session().map(|session| sync_transport::SyncCredentials::new(session.server_url().clone(), session.token()))
    }
}
