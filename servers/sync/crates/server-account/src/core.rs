//! Account contracts shared by account transports and identity consumers.
//!
//! The concrete PostgreSQL account service owns identity resolution, account
//! creation, credentials, session mutation, and the email outbox.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedUser {
    pub user_id: String,
    pub email: String,
}

/// A newly issued opaque server-side session. The raw token crosses the
/// boundary exactly once; persistence adapters store only its digest.
#[derive(Clone)]
pub struct IssuedSession {
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub refresh_token: String,
    pub refresh_expires_at: DateTime<Utc>,
    pub user: AuthenticatedUser,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSession {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionRevocation {
    pub revoked: bool,
    pub signed_out: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthAttempt {
    Login,
    PublicRegistration,
    ResendVerification,
    VerifyEmail,
    RequestPasswordReset,
    ResetPassword,
    AdminRead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountEmailKind {
    VerifyEmail,
    ResetPassword,
}

/// Sensitive, short-lived mail work. Implementations must not log `token` and
/// must remove successfully delivered rows from persistent queues.
pub struct QueuedAccountEmail {
    pub id: String,
    pub recipient: String,
    pub kind: AccountEmailKind,
    pub token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("email already taken")]
    EmailTaken,
    #[error("forbidden")]
    Forbidden,
    #[error("password must be at least {min_characters} characters and at most {max_bytes} bytes")]
    InvalidPassword { min_characters: usize, max_bytes: usize },
    #[error("email address is invalid")]
    InvalidEmail,
    #[error("invalid or expired one-time token")]
    InvalidOneTimeToken,
    #[error("too many authentication attempts")]
    RateLimited,
    #[error("authentication service failed: {0}")]
    Internal(String),
}

#[async_trait]
pub trait AccountEmailSender: Send + Sync {
    /// Implemented by an external email provider adapter. Account code never
    /// interprets provider credentials or performs SMTP/sendmail delivery.
    async fn send(&self, email: &QueuedAccountEmail) -> Result<(), String>;
}
