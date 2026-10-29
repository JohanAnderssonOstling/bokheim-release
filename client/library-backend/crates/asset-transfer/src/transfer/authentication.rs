//! Library account renewal around a transfer operation, with at most one retry.
use crate::TransferError;
use std::future::Future;

impl super::AssetExecutor {
    /// Capture credentials and the user before work so a retry cannot adopt
    /// an account that signed in while the operation was in flight.
    pub(super) async fn refreshed<T, F, Fut>(&self, mut operation: F) -> Result<T, TransferError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, TransferError>>,
    {
        let credentials = self.credentials()?;
        let user = self.account.as_ref().and_then(|account| account.current().ok().flatten()).map(|session| session.user_id().to_owned());
        match operation().await {
            Err(TransferError::AuthenticationRequired) => {}
            result => return result,
        }
        let Some(account) = &self.account else { return Err(TransferError::AuthenticationRequired) };
        let refreshed = account.refresh_after_unauthorized(credentials.access_token()).await.map_err(TransferError::retryable)?.ok_or(TransferError::AuthenticationRequired)?;
        if user.as_deref() != Some(refreshed.user_id()) {
            return Err(TransferError::AuthenticationRequired);
        }
        self.check_transfer_account(&credentials)?;
        operation().await
    }
}
