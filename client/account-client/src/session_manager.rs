use std::fmt;
use std::sync::{Arc, RwLock};

#[cfg(not(target_arch = "wasm32"))]
pub type SessionFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
pub type SessionFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

/// Persistence supplied by the application; writes complete before session readers are notified.
pub trait SessionPersistence: Clone + std::fmt::Debug {
    fn load_account_session(&self) -> std::io::Result<Option<crate::Session>>;
    fn save_account_session(&self, session: &crate::Session) -> std::io::Result<()>;
    fn clear_account_session(&self) -> std::io::Result<()>;
}
use crate::ServerUrl;

const EXPIRY_SAFETY_WINDOW_SECONDS: i64 = 60;
const REFRESH_WINDOW_SECONDS: i64 = 15 * 60;

#[derive(Debug, Default)]
struct SessionState {
    session: Option<crate::Session>,
    generation: u64,
}

#[derive(Debug)]
struct SharedSession(RwLock<SessionState>);

#[derive(Debug)]
pub enum AccountSessionError {
    Authentication(crate::AuthError),
    Persistence(std::io::Error),
    Poisoned,
    Changed,
}

impl fmt::Display for AccountSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication(error) => error.fmt(formatter),
            Self::Persistence(error) => error.fmt(formatter),
            Self::Poisoned => formatter.write_str("account session lock is poisoned"),
            Self::Changed => formatter.write_str("account changed while the request was in progress"),
        }
    }
}

impl std::error::Error for AccountSessionError {}

#[derive(Clone, Debug)]
pub struct AccountSession<P: SessionPersistence> {
    persistence: P,
    server: ServerUrl,
    state: Arc<SharedSession>,
    refresh: Arc<tokio::sync::Mutex<()>>,
    changes: tokio::sync::watch::Sender<u64>,
}

impl<P: SessionPersistence> AccountSession<P> {
    pub fn load(persistence: P, server: ServerUrl) -> Result<Self, AccountSessionError> {
        let mut session = persistence.load_account_session().map_err(AccountSessionError::Persistence)?;
        if session.as_ref().is_some_and(|session| !Self::is_usable(session, &server)) {
            persistence.clear_account_session().map_err(AccountSessionError::Persistence)?;
            session = None;
        }
        Ok(Self { persistence, server, state: Arc::new(SharedSession(RwLock::new(SessionState { session, generation: 0 }))), refresh: Arc::new(tokio::sync::Mutex::new(())), changes: tokio::sync::watch::channel(0).0 })
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Read the latest usable session without performing persistence or refresh work.
    pub fn credentials_session(&self) -> Option<crate::Session> {
        self.state.0.read().ok()?.session.as_ref().filter(|session| !Self::is_expired(session)).cloned()
    }

    pub fn restore_if_empty(&self) -> Result<Option<crate::Session>, AccountSessionError> {
        let mut state = self.state.0.write().map_err(|_| AccountSessionError::Poisoned)?;
        if state.session.is_none() {
            if let Some(restored) = self.persistence.load_account_session().map_err(AccountSessionError::Persistence)? {
                let usable = Self::is_usable(&restored, &self.server);
                self.replace(&mut state, usable.then_some(restored))?;
            }
        }
        self.remove_expired(&mut state)?;
        Ok(state.session.clone())
    }

    pub fn current(&self) -> Result<Option<crate::Session>, AccountSessionError> {
        let mut state = self.state.0.write().map_err(|_| AccountSessionError::Poisoned)?;
        self.remove_expired(&mut state)?;
        Ok(state.session.clone())
    }

    fn remove_expired(&self, state: &mut SessionState) -> Result<(), AccountSessionError> {
        if state.session.as_ref().is_some_and(Self::is_expired) {
            self.replace(state, None)?;
        }
        Ok(())
    }

    pub fn require(&self) -> Result<crate::Session, AccountSessionError> {
        self.current()?.ok_or_else(|| AccountSessionError::Authentication(crate::AuthError::InvalidData("not signed in".to_owned())))
    }

    pub fn set(&self, session: crate::Session) -> Result<(), AccountSessionError> {
        let mut state = self.state.0.write().map_err(|_| AccountSessionError::Poisoned)?;
        self.replace(&mut state, Some(session))
    }

    pub fn take(&self) -> Result<Option<crate::Session>, AccountSessionError> {
        let mut state = self.state.0.write().map_err(|_| AccountSessionError::Poisoned)?;
        let previous = state.session.clone();
        self.replace(&mut state, None)?;
        Ok(previous)
    }

    /// The state lock covers persistence as well as book to token readers.
    fn replace(&self, state: &mut SessionState, session: Option<crate::Session>) -> Result<(), AccountSessionError> {
        if let Some(session) = &session {
            if !Self::is_usable(session, &self.server) {
                return Err(AccountSessionError::Authentication(crate::AuthError::InvalidData("cannot adopt an expired session or a session from another server".to_owned())));
            }
            self.persistence.save_account_session(session).map_err(AccountSessionError::Persistence)?;
        } else {
            self.persistence.clear_account_session().map_err(AccountSessionError::Persistence)?;
        }
        state.session = session;
        state.generation += 1;
        self.changes.send_replace(state.generation);
        Ok(())
    }

    pub async fn refresh_if_needed(&self) -> Result<Option<crate::Session>, AccountSessionError> {
        self.refresh(None).await
    }

    pub async fn refresh_after_unauthorized(&self, rejected_token: &str) -> Result<Option<crate::Session>, AccountSessionError> {
        self.refresh(Some(rejected_token)).await
    }

    async fn refresh(&self, rejected_token: Option<&str>) -> Result<Option<crate::Session>, AccountSessionError> {
        self.refresh_using(rejected_token, |session| async move { crate::refresh_session(&session).await }).await
    }

    async fn refresh_using<F, Fut>(&self, rejected_token: Option<&str>, refresh: F) -> Result<Option<crate::Session>, AccountSessionError>
    where
        F: FnOnce(crate::Session) -> Fut,
        Fut: std::future::Future<Output = Result<crate::Session, crate::AuthError>>,
    {
        let _guard = self.refresh.lock().await;
        let (session, generation) = {
            let mut state = self.state.0.write().map_err(|_| AccountSessionError::Poisoned)?;
            self.remove_expired(&mut state)?;
            let Some(session) = state.session.clone() else { return Ok(None) };
            (session, state.generation)
        };
        if rejected_token.is_some_and(|token| token != session.token()) || (rejected_token.is_none() && !session.expires_within(REFRESH_WINDOW_SECONDS)) {
            return Ok(Some(session));
        }
        if session.refresh_token().is_none() {
            if rejected_token.is_some() || session.expires_within(EXPIRY_SAFETY_WINDOW_SECONDS) {
                return self.finish_refresh(generation, Ok(None));
            }
            return Ok(Some(session));
        }
        let result = refresh(session).await.map(Some);
        self.finish_refresh(generation, result)
    }

    fn finish_refresh(&self, generation: u64, result: Result<Option<crate::Session>, crate::AuthError>) -> Result<Option<crate::Session>, AccountSessionError> {
        let mut state = self.state.0.write().map_err(|_| AccountSessionError::Poisoned)?;
        self.remove_expired(&mut state)?;
        // Neither a delayed success nor a rejection may modify a newer login/logout.
        if state.generation != generation {
            return Err(AccountSessionError::Changed);
        }
        match result {
            Ok(session) => self.replace(&mut state, session)?,
            Err(error) => {
                if error.is_unauthorized() || matches!(error, crate::AuthError::InvalidData(_)) {
                    self.replace(&mut state, None)?;
                }
                return Err(AccountSessionError::Authentication(error));
            }
        }
        Ok(state.session.clone())
    }

    pub async fn request<T>(&self, mut request: impl for<'a> FnMut(&'a crate::Session) -> SessionFuture<'a, Result<T, crate::AuthError>>) -> Result<T, AccountSessionError> {
        let session = self.refresh_if_needed().await?.ok_or_else(|| AccountSessionError::Authentication(crate::AuthError::InvalidData("not signed in".to_owned())))?;
        match request(&session).await {
            Ok(value) => Ok(value),
            Err(error) if error.is_unauthorized() => {
                let refreshed = self.refresh_after_unauthorized(session.token()).await?.ok_or_else(|| AccountSessionError::Authentication(crate::AuthError::InvalidData("authentication expired; sign in again".to_owned())))?;
                request(&refreshed).await.map_err(AccountSessionError::Authentication)
            }
            Err(error) => Err(AccountSessionError::Authentication(error)),
        }
    }

    fn is_usable(session: &crate::Session, server: &ServerUrl) -> bool {
        session.server_url() == server && !Self::is_expired(session)
    }

    fn is_expired(session: &crate::Session) -> bool {
        if session.refresh_token().is_some() {
            session.refresh_expires_within(EXPIRY_SAFETY_WINDOW_SECONDS)
        } else {
            session.expires_within(EXPIRY_SAFETY_WINDOW_SECONDS)
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Default)]
    struct MemoryPersistence(Arc<RwLock<Option<crate::Session>>>);
    impl SessionPersistence for MemoryPersistence {
        fn load_account_session(&self) -> std::io::Result<Option<crate::Session>> {
            Ok(self.0.read().unwrap().clone())
        }
        fn save_account_session(&self, session: &crate::Session) -> std::io::Result<()> {
            *self.0.write().unwrap() = Some(session.clone());
            Ok(())
        }
        fn clear_account_session(&self) -> std::io::Result<()> {
            *self.0.write().unwrap() = None;
            Ok(())
        }
    }

    fn session(token: char) -> crate::Session {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        crate::Session::try_new_with_tokens("https://example.com".parse().unwrap(), "user".into(), "reader@example.com".into(), token.to_string().repeat(43), Some(now + 120), Some("r".repeat(43)), Some(now + 3600)).unwrap()
    }

    #[tokio::test]
    async fn rejected_current_token_refreshes_early_but_stale_rejections_reuse_the_session() {
        let persistence = MemoryPersistence::default();
        let account = AccountSession::load(persistence, "https://example.com".parse().unwrap()).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let initial = crate::Session::try_new_with_tokens("https://example.com".parse().unwrap(), "user".into(), "reader@example.com".into(), "a".repeat(43), Some(now + 3600), Some("r".repeat(43)), Some(now + 7200)).unwrap();
        let rejected_token = initial.token().to_owned();
        account.set(initial).unwrap();
        account.refresh_using(None, |_| async { panic!("a fresh session needs no scheduled refresh") }).await.unwrap();
        let refreshed = account.refresh_using(Some(&rejected_token), |_| async { Ok(session('b')) }).await.unwrap().unwrap();
        assert_eq!(refreshed.token(), session('b').token());
        let reused = account.refresh_using(Some(&rejected_token), |_| async { panic!("a stale rejection must reuse the newer token") }).await.unwrap().unwrap();
        assert_eq!(reused.token(), refreshed.token());
    }

    #[tokio::test]
    async fn delayed_refresh_cannot_restore_logout_or_overwrite_login() {
        for replacement in [None, Some(session('c'))] {
            for rejected in [false, true] {
                let persistence = MemoryPersistence::default();
                let account = AccountSession::load(persistence.clone(), "https://example.com".parse().unwrap()).unwrap();
                account.set(session('a')).unwrap();
                let credentials = account.clone();
                let (started_tx, started_rx) = tokio::sync::oneshot::channel();
                let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
                let refresh = account.refresh_using(None, |_| async move {
                    started_tx.send(()).unwrap();
                    resume_rx.await.unwrap();
                    if rejected {
                        Err(crate::AuthError::InvalidData("refresh rejected".into()))
                    } else {
                        Ok(session('b'))
                    }
                });
                let supersede = async {
                    started_rx.await.unwrap();
                    account.take().unwrap();
                    if let Some(replacement) = replacement.clone() {
                        account.set(replacement).unwrap();
                    }
                    resume_tx.send(()).unwrap();
                };
                let (result, ()) = tokio::join!(refresh, supersede);
                let expected = replacement.as_ref().map(|session| session.token().to_owned());
                assert!(matches!(result, Err(AccountSessionError::Changed)));
                assert_eq!(account.current().unwrap().map(|session| session.token().to_owned()), expected);
                assert_eq!(persistence.load_account_session().unwrap().map(|session| session.token().to_owned()), expected);
                assert_eq!(credentials.credentials_session().map(|credentials| credentials.token().to_owned()), expected);
            }
        }
    }

    #[tokio::test]
    async fn refresh_publishes_to_existing_credentials_readers_and_persistence() {
        let persistence = MemoryPersistence::default();
        let account = AccountSession::load(persistence.clone(), "https://example.com".parse().unwrap()).unwrap();
        let credentials = account.clone();
        account.set(session('a')).unwrap();
        account.refresh_using(None, |_| async { Ok(session('b')) }).await.unwrap();
        assert_eq!(credentials.credentials_session().unwrap().token(), session('b').token());
        assert_eq!(persistence.load_account_session().unwrap().unwrap().token(), session('b').token());
        account.refresh_using(None, |_| async { Err(crate::AuthError::InvalidData("revoked".into())) }).await.unwrap_err();
        assert!(account.current().unwrap().is_none());
        assert!(persistence.load_account_session().unwrap().is_none());
        assert!(credentials.credentials_session().is_none());
    }
}
