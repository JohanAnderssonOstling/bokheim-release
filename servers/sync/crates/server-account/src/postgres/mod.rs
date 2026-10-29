//! PostgreSQL adapter for Bokheim accounts, credentials, and sessions.

use std::sync::PoisonError;
mod crypto;
mod deletion;
mod outbox;
mod public_registration;
mod rate_limit;

use crate::core::{AccountSession, AuthAttempt, AuthError, AuthenticatedUser, IssuedSession, SessionRevocation};
use account_contract::{EMAIL_MAX_BYTES, EMAIL_VERIFICATION_PIN_DIGITS, PASSWORD_MAX_BYTES};
use chrono::{DateTime, Duration, Utc};
use server_postgres::PostgresDatabase;
use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::Notify;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration as StdDuration, Instant};

pub use crypto::AccountTokenKey;

const ACCESS_TOKEN_TTL_HOURS: i64 = 1;
const REFRESH_TOKEN_TTL_DAYS: i64 = 90;
const TOKEN_LEN: usize = 43;
const MAX_CONCURRENT_PASSWORD_OPERATIONS: usize = 4;
const IDENTITY_CACHE_MAX_ENTRIES: usize = 65_536;
const IDENTITY_CACHE_TTL: StdDuration = StdDuration::from_secs(30);

fn internal(error: impl std::fmt::Display) -> AuthError {
    AuthError::Internal(error.to_string())
}

pub struct PostgresAccountService {
    pool: PgPool,
    cleanup: Arc<Notify>,
    token_protector: crypto::TokenProtector,
    password_permits: Arc<tokio::sync::Semaphore>,
    identity_cache: IdentityCache,
}

#[derive(Clone)]
struct CachedIdentity {
    user: AuthenticatedUser,
    session_id: Option<String>,
    token_expires_at: DateTime<Utc>,
    cache_expires_at: Instant,
}

#[derive(Default)]
struct IdentityCacheState {
    revision: u64,
    entries: HashMap<String, CachedIdentity>,
}

struct IdentityCache {
    state: RwLock<IdentityCacheState>,
    max_entries: usize,
    ttl: StdDuration,
}

impl IdentityCache {
    fn new(max_entries: usize, ttl: StdDuration) -> Self {
        Self { state: RwLock::new(IdentityCacheState::default()), max_entries, ttl }
    }

    /// Returns the cache revision observed with the miss. An invalidation that
    /// races the following database query changes this revision and prevents
    /// the stale query result from being inserted afterward.
    fn lookup(&self, token_hash: &str) -> (Option<AuthenticatedUser>, u64) {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let wall_now = Utc::now();
        let user = state.entries.get(token_hash).filter(|entry| entry.cache_expires_at > now && entry.token_expires_at > wall_now).map(|entry| entry.user.clone());
        (user, state.revision)
    }

    fn insert_if_revision(&self, revision: u64, token_hash: String, user: AuthenticatedUser, session_id: Option<String>, token_expires_at: DateTime<Utc>) {
        if self.max_entries == 0 || self.ttl.is_zero() {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        if state.revision != revision {
            return;
        }
        let now = Instant::now();
        let wall_now = Utc::now();
        state.entries.retain(|_, cached| cached.cache_expires_at > now && cached.token_expires_at > wall_now);
        if state.entries.len() >= self.max_entries && !state.entries.contains_key(&token_hash) {
            if let Some(evicted) = state.entries.keys().next().cloned() {
                state.entries.remove(&evicted);
            }
        }
        let cache_expires_at = Instant::now().checked_add(self.ttl).unwrap_or_else(Instant::now);
        state.entries.insert(token_hash, CachedIdentity { user, session_id, token_expires_at, cache_expires_at });
    }

    fn invalidate_token(&self, token_hash: &str) {
        self.invalidate(|hash, _| hash != token_hash);
    }

    fn invalidate_session(&self, session_id: &str) {
        self.invalidate(|_, entry| entry.session_id.as_deref() != Some(session_id));
    }

    fn invalidate_user(&self, user_id: &str) {
        self.invalidate(|_, entry| entry.user.user_id != user_id);
    }

    fn invalidate_other_sessions(&self, user_id: &str, preserved_session_id: &str) {
        self.invalidate(|_, entry| entry.user.user_id != user_id || entry.session_id.as_deref() == Some(preserved_session_id));
    }

    fn invalidate(&self, keep: impl Fn(&str, &CachedIdentity) -> bool) {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        state.revision = state.revision.wrapping_add(1);
        state.entries.retain(|hash, entry| keep(hash, entry));
    }

}

impl PostgresAccountService {
    pub fn new(database: PostgresDatabase, token_key: AccountTokenKey) -> Self {
        Self::with_identity_cache(database, token_key, IDENTITY_CACHE_MAX_ENTRIES, IDENTITY_CACHE_TTL)
    }

    fn with_identity_cache(database: PostgresDatabase, token_key: AccountTokenKey, max_entries: usize, ttl: StdDuration) -> Self {
        let pool = database.pool().clone();
        Self {
            pool,
            cleanup: database.asset_cleanup_signal(),
            token_protector: crypto::TokenProtector::new(token_key),
            password_permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PASSWORD_OPERATIONS)),
            identity_cache: IdentityCache::new(max_entries, ttl),
        }
    }

    async fn hash_password(&self, password: String) -> Result<String, AuthError> {
        let permit = self.password_permits.clone().acquire_owned().await.map_err(internal)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crypto::hash_password(&password)
        })
        .await
        .map_err(internal)?
    }

    async fn verify_password(&self, password: String, hash: String) -> Result<bool, AuthError> {
        let permit = self.password_permits.clone().acquire_owned().await.map_err(internal)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crypto::verify_password(&password, &hash)
        })
        .await
        .map_err(internal)
    }

    async fn verify_password_dummy(&self, password: String) -> Result<(), AuthError> {
        let permit = self.password_permits.clone().acquire_owned().await.map_err(internal)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crypto::verify_password_dummy(&password);
        })
        .await
        .map_err(internal)
    }
}

struct IssuedTokens {
    access_token: String,
    access_expires_at: DateTime<Utc>,
    refresh_token: String,
    refresh_expires_at: DateTime<Utc>,
}

async fn issue_token_pair(transaction: &mut Transaction<'_, Postgres>, user_id: &str) -> Result<IssuedTokens, AuthError> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let (access_token, access_hash) = crypto::mint_token();
    let (refresh_token, refresh_hash) = crypto::mint_token();
    let access_expires_at = Utc::now() + Duration::hours(ACCESS_TOKEN_TTL_HOURS);
    let refresh_expires_at = Utc::now() + Duration::days(REFRESH_TOKEN_TTL_DAYS);
    sqlx::query("INSERT INTO auth_session (id, user_id, refresh_token_hash, refresh_expires_at) VALUES ($1, $2, $3, $4)")
        .bind(&session_id)
        .bind(user_id)
        .bind(refresh_hash)
        .bind(refresh_expires_at)
        .execute(&mut **transaction)
        .await
        .map_err(internal)?;
    sqlx::query("INSERT INTO auth_token (token_hash, user_id, expires_at, session_id) VALUES ($1, $2, $3, $4)").bind(access_hash).bind(user_id).bind(access_expires_at).bind(session_id).execute(&mut **transaction).await.map_err(internal)?;
    Ok(IssuedTokens { access_token, access_expires_at, refresh_token, refresh_expires_at })
}

fn issued_session(tokens: IssuedTokens, user: AuthenticatedUser) -> IssuedSession {
    IssuedSession { token: tokens.access_token, expires_at: tokens.access_expires_at, refresh_token: tokens.refresh_token, refresh_expires_at: tokens.refresh_expires_at, user }
}

fn valid_domain(domain: &str) -> bool {
    domain.len() <= 253
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                && label.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn normalized_email(email: &str) -> Result<String, AuthError> {
    let email = email.trim();
    if email.len() > EMAIL_MAX_BYTES || !email.is_ascii() || email.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(AuthError::InvalidEmail);
    }
    let Some((local, domain)) = email.rsplit_once('@') else {
        return Err(AuthError::InvalidEmail);
    };
    if local.is_empty()
        || local.len() > 64
        || local.contains('@')
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&byte))
    {
        return Err(AuthError::InvalidEmail);
    }
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    if !valid_domain(&domain) {
        return Err(AuthError::InvalidEmail);
    }
    Ok(format!("{}@{domain}", local.to_ascii_lowercase()))
}

fn valid_password_size(password: &str) -> bool {
    !password.is_empty() && password.len() <= PASSWORD_MAX_BYTES
}

fn valid_urlsafe_secret(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_verification_pin(value: &str) -> bool {
    value.len() == EMAIL_VERIFICATION_PIN_DIGITS && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn email_taken(error: &sqlx::Error) -> bool {
    error.as_database_error().and_then(|error| error.code()).as_deref() == Some("23505")
}

impl PostgresAccountService {
    pub async fn resolve_token(&self, token: &str) -> Result<AuthenticatedUser, AuthError> {
        if !valid_urlsafe_secret(token, TOKEN_LEN) {
            return Err(AuthError::InvalidCredentials);
        }
        let token_hash = crypto::sha256_hex(token.as_bytes());
        let (cached, cache_revision) = self.identity_cache.lookup(&token_hash);
        if let Some(user) = cached {
            return Ok(user);
        }
        let row: Option<(String, String, DateTime<Utc>, Option<String>)> = sqlx::query_as(
            "SELECT users.id, users.email,
                    auth_token.expires_at, auth_token.session_id
             FROM auth_token
             JOIN users ON users.id = auth_token.user_id
             WHERE auth_token.token_hash = $1
               AND auth_token.expires_at > now()",
        )
        .bind(&token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        let Some((user_id, email, token_expires_at, session_id)) = row else {
            return Err(AuthError::InvalidCredentials);
        };
        let user = AuthenticatedUser { user_id, email };
        self.identity_cache.insert_if_revision(cache_revision, token_hash, user.clone(), session_id, token_expires_at);
        Ok(user)
    }
}

impl PostgresAccountService {
    pub async fn enforce_rate_limit(&self, attempt: AuthAttempt, network_key: &str, subject: &str) -> Result<(), AuthError> {
        self.enforce_rate_limit_impl(attempt, network_key, subject).await
    }

    pub async fn clear_subject_rate_limit(&self, attempt: AuthAttempt, subject: &str) -> Result<(), AuthError> {
        self.clear_subject_rate_limit_impl(attempt, subject).await
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<IssuedSession, AuthError> {
        let Ok(email) = normalized_email(email) else {
            self.verify_password_dummy(if valid_password_size(password) { password.to_owned() } else { "invalid-password-size".to_string() }).await?;
            return Err(AuthError::InvalidCredentials);
        };
        if !valid_password_size(password) {
            self.verify_password_dummy("invalid-password-size".to_string()).await?;
            return Err(AuthError::InvalidCredentials);
        }
        let row: Option<(String, String, String)> =
            sqlx::query_as("SELECT id, email, password_hash FROM users WHERE email = $1 AND password_hash IS NOT NULL").bind(&email).fetch_optional(&self.pool).await.map_err(internal)?;
        let Some((user_id, stored_email, password_hash)) = row else {
            self.verify_password_dummy(password.to_owned()).await?;
            return Err(AuthError::InvalidCredentials);
        };
        if !self.verify_password(password.to_owned(), password_hash).await? {
            return Err(AuthError::InvalidCredentials);
        }
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let tokens = issue_token_pair(&mut transaction, &user_id).await?;
        transaction.commit().await.map_err(internal)?;
        Ok(issued_session(tokens, AuthenticatedUser { user_id, email: stored_email }))
    }

    pub async fn refresh_session(&self, refresh_token: &str) -> Result<IssuedSession, AuthError> {
        if !valid_urlsafe_secret(refresh_token, TOKEN_LEN) {
            return Err(AuthError::InvalidCredentials);
        }
        let old_refresh_hash = crypto::sha256_hex(refresh_token.as_bytes());
        let (new_access_token, new_access_hash) = crypto::mint_token();
        let (new_refresh_token, new_refresh_hash) = crypto::mint_token();
        let access_expires_at = Utc::now() + Duration::hours(ACCESS_TOKEN_TTL_HOURS);
        let refresh_expires_at = Utc::now() + Duration::days(REFRESH_TOKEN_TTL_DAYS);
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        sqlx::query(
            "DELETE FROM auth_consumed_refresh_token
             WHERE token_hash IN (
                 SELECT token_hash
                 FROM auth_consumed_refresh_token
                 WHERE expires_at <= now()
                 ORDER BY expires_at
                 LIMIT 256
             )",
        )
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let session: Option<(String, String, String)> = sqlx::query_as(
            "SELECT auth_session.id, users.id, users.email
             FROM auth_session
             JOIN users ON users.id = auth_session.user_id
             WHERE auth_session.refresh_token_hash = $1
               AND auth_session.refresh_expires_at > now()
             FOR UPDATE OF auth_session",
        )
        .bind(&old_refresh_hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let Some((session_id, user_id, email)) = session else {
            let reused_session: Option<(String, String)> = sqlx::query_as(
                "SELECT auth_session.id, auth_session.user_id
                 FROM auth_consumed_refresh_token
                 JOIN auth_session ON auth_session.id = auth_consumed_refresh_token.session_id
                 WHERE auth_consumed_refresh_token.token_hash = $1
                   AND auth_consumed_refresh_token.expires_at > now()
                   AND auth_session.refresh_expires_at > now()
                 FOR UPDATE OF auth_session",
            )
            .bind(&old_refresh_hash)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(internal)?;
            let Some((session_id, user_id)) = reused_session else {
                return Err(AuthError::InvalidCredentials);
            };
            sqlx::query("INSERT INTO auth_security_event (user_id, session_id, event_type) VALUES ($1, $2, 'refresh_token_reuse')").bind(&user_id).bind(&session_id).execute(&mut *transaction).await.map_err(internal)?;
            sqlx::query("DELETE FROM auth_session WHERE id = $1").bind(&session_id).execute(&mut *transaction).await.map_err(internal)?;
            transaction.commit().await.map_err(internal)?;
            self.identity_cache.invalidate_session(&session_id);
            tracing::warn!(user_id = %user_id, session_id = %session_id, "revoked authentication session after refresh token reuse");
            return Err(AuthError::InvalidCredentials);
        };
        sqlx::query("INSERT INTO auth_consumed_refresh_token (token_hash, session_id, expires_at) VALUES ($1, $2, $3)")
            .bind(&old_refresh_hash)
            .bind(&session_id)
            .bind(refresh_expires_at)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        sqlx::query("UPDATE auth_consumed_refresh_token SET expires_at = $2 WHERE session_id = $1").bind(&session_id).bind(refresh_expires_at).execute(&mut *transaction).await.map_err(internal)?;
        sqlx::query("UPDATE auth_session SET refresh_token_hash = $2, refresh_expires_at = $3 WHERE id = $1").bind(&session_id).bind(new_refresh_hash).bind(refresh_expires_at).execute(&mut *transaction).await.map_err(internal)?;
        sqlx::query("DELETE FROM auth_token WHERE session_id = $1").bind(&session_id).execute(&mut *transaction).await.map_err(internal)?;
        sqlx::query("INSERT INTO auth_token (token_hash, user_id, expires_at, session_id) VALUES ($1, $2, $3, $4)")
            .bind(new_access_hash)
            .bind(&user_id)
            .bind(access_expires_at)
            .bind(&session_id)
            .execute(&mut *transaction)
            .await
            .map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        self.identity_cache.invalidate_session(&session_id);
        Ok(issued_session(IssuedTokens { access_token: new_access_token, access_expires_at, refresh_token: new_refresh_token, refresh_expires_at }, AuthenticatedUser { user_id, email }))
    }

    pub async fn logout(&self, token: &str) -> Result<(), AuthError> {
        if !valid_urlsafe_secret(token, TOKEN_LEN) {
            return Err(AuthError::InvalidCredentials);
        }
        let token_hash = crypto::sha256_hex(token.as_bytes());
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let mut session_id: Option<String> = sqlx::query_scalar("SELECT id FROM auth_session WHERE refresh_token_hash = $1").bind(&token_hash).fetch_optional(&mut *transaction).await.map_err(internal)?;
        if session_id.is_none() {
            session_id = sqlx::query_scalar::<_, Option<String>>("SELECT session_id FROM auth_token WHERE token_hash = $1").bind(&token_hash).fetch_optional(&mut *transaction).await.map_err(internal)?.flatten();
        }
        if let Some(session_id) = session_id.as_deref() {
            sqlx::query("DELETE FROM auth_session WHERE id = $1").bind(session_id).execute(&mut *transaction).await.map_err(internal)?;
        } else {
            sqlx::query("DELETE FROM auth_token WHERE token_hash = $1").bind(&token_hash).execute(&mut *transaction).await.map_err(internal)?;
        }
        transaction.commit().await.map_err(internal)?;
        match session_id {
            Some(session_id) => self.identity_cache.invalidate_session(&session_id),
            None => self.identity_cache.invalidate_token(&token_hash),
        }
        Ok(())
    }

    pub async fn list_sessions(&self, access_token: &str) -> Result<Vec<AccountSession>, AuthError> {
        if !valid_urlsafe_secret(access_token, TOKEN_LEN) {
            return Err(AuthError::InvalidCredentials);
        }
        let access_hash = crypto::sha256_hex(access_token.as_bytes());
        let sessions: Vec<(String, DateTime<Utc>, DateTime<Utc>, bool)> = sqlx::query_as(
            "SELECT sessions.id,
                    sessions.created_at,
                    sessions.refresh_expires_at,
                    sessions.id = current_token.session_id
             FROM auth_token AS current_token
             JOIN auth_session AS sessions ON sessions.user_id = current_token.user_id
             WHERE current_token.token_hash = $1
               AND current_token.expires_at > now()
               AND current_token.session_id IS NOT NULL
               AND sessions.refresh_expires_at > now()
             ORDER BY sessions.id = current_token.session_id DESC,
                      sessions.created_at DESC,
                      sessions.id",
        )
        .bind(access_hash)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        if sessions.is_empty() {
            return Err(AuthError::InvalidCredentials);
        }
        Ok(sessions.into_iter().map(|(session_id, created_at, expires_at, current)| AccountSession { session_id, created_at, expires_at, current }).collect())
    }

    pub async fn revoke_session(&self, access_token: &str, session_id: &str) -> Result<SessionRevocation, AuthError> {
        if !valid_urlsafe_secret(access_token, TOKEN_LEN) {
            return Err(AuthError::InvalidCredentials);
        }
        let access_hash = crypto::sha256_hex(access_token.as_bytes());
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let current: Option<(String, String)> = sqlx::query_as(
            "SELECT user_id, session_id
             FROM auth_token
             WHERE token_hash = $1
               AND expires_at > now()
               AND session_id IS NOT NULL
             FOR UPDATE",
        )
        .bind(access_hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let Some((user_id, current_session_id)) = current else {
            return Err(AuthError::InvalidCredentials);
        };
        let deleted = sqlx::query("DELETE FROM auth_session WHERE id = $1 AND user_id = $2").bind(session_id).bind(&user_id).execute(&mut *transaction).await.map_err(internal)?.rows_affected() == 1;
        transaction.commit().await.map_err(internal)?;
        if deleted {
            self.identity_cache.invalidate_session(session_id);
        }
        Ok(SessionRevocation { revoked: deleted, signed_out: deleted && session_id == current_session_id })
    }

    pub async fn revoke_other_sessions(&self, access_token: &str) -> Result<u64, AuthError> {
        if !valid_urlsafe_secret(access_token, TOKEN_LEN) {
            return Err(AuthError::InvalidCredentials);
        }
        let access_hash = crypto::sha256_hex(access_token.as_bytes());
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let current: Option<(String, String)> = sqlx::query_as(
            "SELECT user_id, session_id
             FROM auth_token
             WHERE token_hash = $1
               AND expires_at > now()
               AND session_id IS NOT NULL
             FOR UPDATE",
        )
        .bind(access_hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(internal)?;
        let Some((user_id, current_session_id)) = current else {
            return Err(AuthError::InvalidCredentials);
        };
        let revoked = sqlx::query("DELETE FROM auth_session WHERE user_id = $1 AND id <> $2").bind(&user_id).bind(&current_session_id).execute(&mut *transaction).await.map_err(internal)?.rows_affected();
        transaction.commit().await.map_err(internal)?;
        if revoked != 0 {
            self.identity_cache.invalidate_other_sessions(&user_id, &current_session_id);
        }
        Ok(revoked)
    }

    pub async fn ensure_configured_user(&self, email: &str, password: &str) -> Result<AuthenticatedUser, AuthError> {
        self.ensure_configured_user_impl(email, password).await
    }

    pub async fn request_public_registration(&self, email: &str, password: &str) -> Result<(), AuthError> {
        self.request_public_registration_impl(email, password).await
    }

    pub async fn resend_verification(&self, email: &str) -> Result<(), AuthError> {
        self.resend_verification_impl(email).await
    }

    pub async fn verify_email(&self, email: &str, pin: &str, password: &str) -> Result<IssuedSession, AuthError> {
        self.verify_email_impl(email, pin, password).await
    }

    pub async fn request_password_reset(&self, email: &str) -> Result<(), AuthError> {
        self.request_password_reset_impl(email).await
    }

    pub async fn reset_password(&self, token: &str, new_password: &str) -> Result<(), AuthError> {
        self.reset_password_impl(token, new_password).await
    }
}
