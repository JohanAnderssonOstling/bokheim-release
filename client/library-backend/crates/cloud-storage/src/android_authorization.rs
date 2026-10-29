//! Native authorization requests contain no OAuth client configuration. Google
//! Play services owns account selection, consent, and access-token renewal.
use crate::{
    CloudError, StorageFuture,
    connections::GoogleAuthorization,
    oauth::{GoogleAccessGrant, GoogleAccount},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

pub struct AuthorizationRequest {
    pub id: u64,
    pub email: Option<String>,
    pub interactive: bool,
    pub rejected_token: Option<String>,
}
type Handler = Arc<dyn Fn(AuthorizationRequest) -> Result<(), CloudError> + Send + Sync>;
type Pending = HashMap<u64, oneshot::Sender<Result<GoogleAccessGrant, CloudError>>>;
static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);
static PENDING: LazyLock<Mutex<Pending>> = LazyLock::new(Default::default);
static NEXT: AtomicU64 = AtomicU64::new(1);

pub fn configure(handler: Handler) {
    *HANDLER.lock().unwrap_or_else(PoisonError::into_inner) = Some(handler);
}
pub fn complete(id: u64, result: Result<GoogleAccessGrant, CloudError>) {
    let sender = PENDING.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
    if let Some(sender) = sender {
        let _ = sender.send(result);
    }
}
struct PendingGuard(u64);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        PENDING.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.0);
    }
}
pub struct AndroidGoogleAuthorization;
impl GoogleAuthorization for AndroidGoogleAuthorization {
    fn grant<'a>(&'a self, account: Option<&'a GoogleAccount>, interactive: bool, rejected: Option<&'a str>) -> StorageFuture<'a, GoogleAccessGrant> {
        Box::pin(async move {
            crate::require_google_drive()?;
            let handler = HANDLER.lock().unwrap_or_else(PoisonError::into_inner).clone().ok_or_else(|| CloudError::InvalidRequest("Android Google authorization is unavailable".into()))?;
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            if id == 0 || id > i64::MAX as u64 {
                return Err(CloudError::InvalidRequest("Google authorization request limit reached".into()));
            }
            let (sender, result) = oneshot::channel();
            {
                let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
                if pending.len() >= 16 {
                    return Err(CloudError::Retryable("Another Google authorization request is in progress".into()));
                }
                pending.insert(id, sender);
            }
            let _pending = PendingGuard(id);
            handler(AuthorizationRequest { id, email: account.map(|account| account.email.clone()), interactive, rejected_token: rejected.map(str::to_owned) })?;
            tokio::time::timeout(Duration::from_secs(300), result).await.map_err(|_| CloudError::Retryable("Google authorization timed out; connect again".into()))?.map_err(|_| CloudError::AuthenticationRequired)?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::GoogleAccount;
    use sync_common::storage::ProviderId;
    static SERIAL: Mutex<()> = Mutex::new(());
    #[tokio::test]
    async fn refresh_is_bound_to_the_account_and_carries_the_rejected_token_without_prompting() {
        let _serial = SERIAL.lock().unwrap();
        configure(Arc::new(|request| {
            assert_eq!(request.email.as_deref(), Some("reader@example.com"));
            assert!(!request.interactive);
            assert_eq!(request.rejected_token.as_deref(), Some("old-token"));
            complete(request.id, Ok(GoogleAccessGrant { access_token: "new-token".into(), expires_in_seconds: 300 }));
            Ok(())
        }));
        let account = GoogleAccount { id: ProviderId::new("account").unwrap(), email: "reader@example.com".into(), name: None };
        let grant = AndroidGoogleAuthorization.grant(Some(&account), false, Some("old-token")).await.unwrap();
        assert_eq!(grant.access_token, "new-token");
        assert!(PENDING.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn a_failed_launch_does_not_leave_a_request_or_accept_a_late_callback() {
        let _serial = SERIAL.lock().unwrap();
        let captured = Arc::new(AtomicU64::new(0));
        let id = captured.clone();
        configure(Arc::new(move |request| {
            id.store(request.id, Ordering::Relaxed);
            Err(CloudError::AuthenticationRequired)
        }));
        assert!(matches!(AndroidGoogleAuthorization.grant(None, true, None).await, Err(CloudError::AuthenticationRequired)));
        assert!(PENDING.lock().unwrap().is_empty());
        complete(captured.load(Ordering::Relaxed), Ok(GoogleAccessGrant { access_token: "late-token".into(), expires_in_seconds: 300 }));
        assert!(PENDING.lock().unwrap().is_empty());
    }
}
