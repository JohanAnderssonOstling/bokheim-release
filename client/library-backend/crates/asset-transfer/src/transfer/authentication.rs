//! Library account renewal around a transfer operation, with at most one retry.
use crate::TransferError;

#[derive(Clone, Copy)]
pub(super) enum RefreshScope {
    /// Planning/book requires credentials before starting and validates the server binding.
    Planned,
    /// Queued execution may settle local work without credentials; revoked sessions pause it.
    Queued,
}

/// The pre-flight half of a refreshable operation: credentials and user are
/// captured before any work so a refresh for another session cannot silently
/// adopt the operation.
pub(super) struct RefreshGuard {
    planned: bool,
    credentials: Option<sync_transport::SyncCredentials>,
    user: Option<String>,
}

impl super::worker::TransferWorker {
    pub(super) fn refresh_guard(&self, scope: RefreshScope) -> Result<RefreshGuard, TransferError> {
        let planned = matches!(scope, RefreshScope::Planned);
        let credentials = self.credentials.credentials();
        if planned && credentials.is_none() {
            return Err(TransferError::AuthenticationRequired);
        }
        // Capture the live session user so a refresh for another session
        // cannot silently adopt the operation.
        Ok(RefreshGuard { planned, credentials, user: self.live_user_id() })
    }

    /// Credential refresh after an attempt reports revoked authentication.
    /// Call sites run their attempt, call this, then run the attempt again:
    /// the outcome accumulator is reborrowed sequentially per attempt, which
    /// a retry closure could not lend out of its captures.
    pub(super) async fn refresh_after_auth(&self, guard: RefreshGuard) -> Result<(), TransferError> {
        let (Some(account), Some(credentials)) = (&self.account, guard.credentials) else {
            return Err(TransferError::AuthenticationRequired);
        };
        match account.refresh_after_unauthorized(credentials.access_token()).await {
            Ok(Some(session)) if guard.user.as_deref() == Some(session.user_id()) => {
                if guard.planned {
                    self.check_transfer_account(&credentials)?;
                }
                Ok(())
            }
            Ok(_) => Err(TransferError::AuthenticationRequired),
            Err(_) if !guard.planned && self.credentials.credentials().is_none() => Err(TransferError::AuthenticationRequired),
            Err(error) => Err(TransferError::retryable(error)),
        }
    }
}
