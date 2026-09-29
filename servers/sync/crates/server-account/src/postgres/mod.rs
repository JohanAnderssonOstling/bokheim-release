//! PostgreSQL adapter for Bokheim accounts, credentials, and sessions.

mod activity;
mod crypto;
mod outbox;
mod public_registration;
mod rate_limit;

use crate::core::{AccountSession, AuthAttempt, AuthError, AuthenticatedUser, IssuedSession, SessionRevocation};
use account_contract::{EMAIL_MAX_BYTES, EMAIL_VERIFICATION_PIN_DIGITS, PASSWORD_MAX_BYTES};
use chrono::{DateTime, Duration, Utc};
use server_postgres::PostgresDatabase;
use sqlx::{PgPool, Postgres, Transaction};
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
    activity: activity::AccountActivityRecorder,
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
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let wall_now = Utc::now();
        let user = state.entries.get(token_hash).filter(|entry| entry.cache_expires_at > now && entry.token_expires_at > wall_now).map(|entry| entry.user.clone());
        (user, state.revision)
    }

    fn insert_if_revision(&self, revision: u64, token_hash: String, user: AuthenticatedUser, session_id: Option<String>, token_expires_at: DateTime<Utc>) {
        if self.max_entries == 0 || self.ttl.is_zero() {
            return;
        }
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.revision = state.revision.wrapping_add(1);
        state.entries.retain(|hash, entry| keep(hash, entry));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner).entries.len()
    }
}

impl PostgresAccountService {
    pub fn new(database: PostgresDatabase, token_key: AccountTokenKey) -> Self {
        Self::with_identity_cache(database, token_key, IDENTITY_CACHE_MAX_ENTRIES, IDENTITY_CACHE_TTL)
    }

    fn with_identity_cache(database: PostgresDatabase, token_key: AccountTokenKey, max_entries: usize, ttl: StdDuration) -> Self {
        let pool = database.pool().clone();
        Self {
            activity: activity::AccountActivityRecorder::new(pool.clone()),
            pool,
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
        self.resolve_token_with_activity(token, true).await
    }

    async fn resolve_token_with_activity(&self, token: &str, record_activity: bool) -> Result<AuthenticatedUser, AuthError> {
        if !valid_urlsafe_secret(token, TOKEN_LEN) {
            return Err(AuthError::InvalidCredentials);
        }
        let token_hash = crypto::sha256_hex(token.as_bytes());
        let (cached, cache_revision) = self.identity_cache.lookup(&token_hash);
        if let Some(user) = cached {
            if record_activity {
                self.activity.record_authenticated(&user.user_id);
            }
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
        if record_activity {
            self.activity.record_authenticated(&user.user_id);
        }
        Ok(user)
    }

    pub fn record_sync_activity(&self, user_id: &str) {
        self.activity.record_sync(user_id);
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
        self.login_with_admin_requirement(email, password, false).await
    }

    pub async fn login_admin(&self, email: &str, password: &str) -> Result<IssuedSession, AuthError> {
        self.login_with_admin_requirement(email, password, true).await
    }

    async fn login_with_admin_requirement(&self, email: &str, password: &str, require_admin: bool) -> Result<IssuedSession, AuthError> {
        let Ok(email) = normalized_email(email) else {
            self.verify_password_dummy(if valid_password_size(password) { password.to_owned() } else { "invalid-password-size".to_string() }).await?;
            return Err(AuthError::InvalidCredentials);
        };
        if !valid_password_size(password) {
            self.verify_password_dummy("invalid-password-size".to_string()).await?;
            return Err(AuthError::InvalidCredentials);
        }
        let row: Option<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, email, password_hash, is_admin FROM users WHERE email = $1 AND password_hash IS NOT NULL").bind(&email).fetch_optional(&self.pool).await.map_err(internal)?;
        let Some((user_id, stored_email, password_hash, is_admin)) = row else {
            self.verify_password_dummy(password.to_owned()).await?;
            return Err(AuthError::InvalidCredentials);
        };
        if !self.verify_password(password.to_owned(), password_hash).await? {
            return Err(AuthError::InvalidCredentials);
        }
        if require_admin && !is_admin {
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

    pub async fn ensure_configured_admin(&self, email: &str, password: &str) -> Result<AuthenticatedUser, AuthError> {
        public_registration::validate_password(password)?;
        let user = self.ensure_configured_user_impl(email, password).await?;
        sqlx::query("UPDATE users SET is_admin=true WHERE id=$1").bind(&user.user_id).execute(&self.pool).await.map_err(internal)?;
        Ok(user)
    }

    pub async fn resolve_admin_token(&self, token: &str) -> Result<AuthenticatedUser, AuthError> {
        // Reading the operations dashboard is administration, not product
        // engagement, and must not make an otherwise inactive account active.
        let user = self.resolve_token_with_activity(token, false).await?;
        let is_admin: bool = sqlx::query_scalar("SELECT is_admin FROM users WHERE id=$1").bind(&user.user_id).fetch_optional(&self.pool).await.map_err(internal)?.unwrap_or(false);
        if !is_admin {
            return Err(AuthError::Forbidden);
        }
        Ok(user)
    }

    pub async fn request_public_registration(&self, email: &str, password: &str) -> Result<(), AuthError> {
        self.request_public_registration_impl(email, password).await
    }

    pub async fn resend_verification(&self, email: &str) -> Result<(), AuthError> {
        self.resend_verification_impl(email).await
    }

    pub async fn verify_email(&self, email: &str, pin: &str) -> Result<IssuedSession, AuthError> {
        self.verify_email_impl(email, pin).await
    }

    pub async fn request_password_reset(&self, email: &str) -> Result<(), AuthError> {
        self.request_password_reset_impl(email).await
    }

    pub async fn reset_password(&self, token: &str, new_password: &str) -> Result<(), AuthError> {
        self.reset_password_impl(token, new_password).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    fn cached_identity(user_id: &str) -> AuthenticatedUser {
        AuthenticatedUser { user_id: user_id.to_string(), email: format!("user-{user_id}@example.com") }
    }

    /// Baseline schema application is intentionally not repeatable -- each
    /// test gets its own schema via `search_path` so
    /// tests can run against the same SYNC_E2E_DATABASE_URL without
    /// colliding on tables the previous test already created.
    async fn isolated_pool(max_connections: u32) -> sqlx::PgPool {
        let database_url = std::env::var("SYNC_E2E_DATABASE_URL").expect("SYNC_E2E_DATABASE_URL must be set");
        let root = PgPoolOptions::new().max_connections(1).connect(&database_url).await.unwrap();
        let schema = format!("account_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&root).await.unwrap();
        drop(root);
        let separator = if database_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{database_url}{separator}options=-csearch_path%3D{schema}");
        let pool = PgPoolOptions::new().max_connections(max_connections).connect(&scoped_url).await.unwrap();
        server_postgres::migrate(&PostgresDatabase::from_pool(pool.clone())).await.unwrap();
        pool
    }

    #[test]
    fn identity_cache_is_bounded_expires_and_never_repopulates_across_invalidation() {
        let cache = IdentityCache::new(2, StdDuration::from_secs(30));
        for index in 0..3 {
            let hash = format!("hash-{index}");
            let (_, revision) = cache.lookup(&hash);
            cache.insert_if_revision(revision, hash, cached_identity(&format!("u{index}")), Some(format!("s{index}")), Utc::now() + Duration::hours(1));
        }
        assert_eq!(cache.len(), 2);

        let (_, stale_revision) = cache.lookup("racing-hash");
        cache.invalidate_session("unrelated-session");
        cache.insert_if_revision(stale_revision, "racing-hash".to_string(), cached_identity("racing-user"), Some("racing-session".to_string()), Utc::now() + Duration::hours(1));
        assert!(cache.lookup("racing-hash").0.is_none(), "a lookup started before revocation must not repopulate afterward");

        let (_, current_revision) = cache.lookup("expired-hash");
        cache.insert_if_revision(current_revision, "expired-hash".to_string(), cached_identity("expired-user"), Some("expired-session".to_string()), Utc::now() - Duration::seconds(1));
        assert!(cache.lookup("expired-hash").0.is_none());
    }

    #[test]
    fn authentication_inputs_are_bounded_before_expensive_work() {
        assert!(!valid_password_size(&"x".repeat(PASSWORD_MAX_BYTES + 1)));
        assert_eq!(normalized_email(" Reader@Example.COM ").unwrap(), "reader@example.com");
        assert!(matches!(normalized_email("reader@example"), Err(AuthError::InvalidEmail)));
    }

    #[test]
    fn opaque_tokens_have_one_canonical_wire_shape() {
        let (token, _) = crypto::mint_token();
        assert!(valid_urlsafe_secret(&token, TOKEN_LEN));
        assert!(!valid_urlsafe_secret("", TOKEN_LEN));
        assert!(!valid_urlsafe_secret(&format!("{token} "), TOKEN_LEN));
        assert!(!valid_urlsafe_secret(&"a".repeat(TOKEN_LEN + 1), TOKEN_LEN));
    }

    #[test]
    fn verification_pins_require_exactly_six_ascii_digits() {
        assert!(valid_verification_pin("012345"));
        assert!(!valid_verification_pin("12345"));
        assert!(!valid_verification_pin("1234567"));
        assert!(!valid_verification_pin("12345a"));
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn resolving_a_session_is_read_only_and_never_extends_its_absolute_expiry() {
        let pool = isolated_pool(1).await;
        let activity_column_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1
                 FROM information_schema.columns
                 WHERE table_schema = current_schema()
                   AND table_name = 'auth_token'
                   AND column_name = 'last_used_at'
             )",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!activity_column_exists, "the migrated session schema must not retain unused activity state");
        let service = PostgresAccountService::new(PostgresDatabase::from_pool(pool.clone()), crypto::AccountTokenKey::test_key());
        let user_id = uuid::Uuid::new_v4().to_string();
        let email = format!("auth-test-{}@example.com", uuid::Uuid::new_v4().simple());
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)").bind(&user_id).bind(&email).execute(&pool).await.unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let tokens = issue_token_pair(&mut transaction, &user_id).await.unwrap();
        transaction.commit().await.unwrap();
        let token = tokens.access_token;
        let refresh_token = tokens.refresh_token;
        let token_hash = crypto::sha256_hex(token.as_bytes());
        let stored_before: (DateTime<Utc>, String) = sqlx::query_as("SELECT expires_at, xmin::text FROM auth_token WHERE token_hash = $1").bind(&token_hash).fetch_one(&pool).await.unwrap();

        let resolved = service.resolve_token(&token).await.unwrap();
        assert_eq!(resolved.user_id, user_id);
        assert_eq!(service.identity_cache.len(), 1, "the first database resolution must populate the bounded cache");
        let stored_after: (DateTime<Utc>, String) = sqlx::query_as("SELECT expires_at, xmin::text FROM auth_token WHERE token_hash = $1").bind(&token_hash).fetch_one(&pool).await.unwrap();
        assert_eq!(stored_after, stored_before, "token resolution must not create a new PostgreSQL row version");

        let refreshed = service.refresh_session(&refresh_token).await.unwrap();
        assert_ne!(refreshed.token, token);
        assert_eq!(service.identity_cache.len(), 0, "refresh rotation must evict the previous session identity");
        assert!(matches!(service.resolve_token(&token).await, Err(AuthError::InvalidCredentials)));
        assert_eq!(service.resolve_token(&refreshed.token).await.unwrap().user_id, user_id);
        let refreshed_hash = crypto::sha256_hex(refreshed.token.as_bytes());
        sqlx::query("UPDATE auth_token SET expires_at = now() - INTERVAL '1 second' WHERE token_hash = $1").bind(&refreshed_hash).execute(&pool).await.unwrap();
        // Direct database administration bypasses service-level invalidation;
        // clearing the process cache models the restart/admin coordination that
        // must accompany such an out-of-band credential mutation.
        service.identity_cache.invalidate_token(&refreshed_hash);
        assert!(matches!(service.resolve_token(&refreshed.token).await, Err(AuthError::InvalidCredentials)));
        service.logout(&refreshed.refresh_token).await.unwrap();
        assert!(matches!(service.refresh_session(&refreshed.refresh_token).await, Err(AuthError::InvalidCredentials)));
        sqlx::query("DELETE FROM users WHERE id = $1").bind(&user_id).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn reusing_a_rotated_refresh_token_revokes_its_session_family() {
        let pool = isolated_pool(3).await;
        let database = PostgresDatabase::from_pool(pool.clone());
        let service = PostgresAccountService::new(database, crypto::AccountTokenKey::test_key());
        let user_id = uuid::Uuid::new_v4().to_string();
        let email = format!("refresh-reuse-{}@example.com", uuid::Uuid::new_v4().simple());
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)").bind(&user_id).bind(&email).execute(&pool).await.unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let original = issue_token_pair(&mut transaction, &user_id).await.unwrap();
        let other_device = issue_token_pair(&mut transaction, &user_id).await.unwrap();
        transaction.commit().await.unwrap();
        let original_refresh_hash = crypto::sha256_hex(original.refresh_token.as_bytes());
        let session_id: String = sqlx::query_scalar("SELECT id FROM auth_session WHERE refresh_token_hash = $1").bind(&original_refresh_hash).fetch_one(&pool).await.unwrap();

        let attacker = service.refresh_session(&original.refresh_token).await.unwrap();
        assert_eq!(service.resolve_token(&attacker.token).await.unwrap().user_id, user_id);
        assert!(matches!(service.refresh_session(&original.refresh_token).await, Err(AuthError::InvalidCredentials)));

        assert!(matches!(service.resolve_token(&attacker.token).await, Err(AuthError::InvalidCredentials)));
        assert!(matches!(service.refresh_session(&attacker.refresh_token).await, Err(AuthError::InvalidCredentials)));
        assert_eq!(service.resolve_token(&other_device.access_token).await.unwrap().user_id, user_id, "reuse must not revoke a separate device session");
        let active_sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_session WHERE id = $1").bind(&session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(active_sessions, 0);
        let consumed_tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_consumed_refresh_token WHERE session_id = $1").bind(&session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(consumed_tokens, 0, "revoking a family must remove its credential history");
        let event: (String, String, String) = sqlx::query_as("SELECT user_id, session_id, event_type FROM auth_security_event WHERE session_id = $1").bind(&session_id).fetch_one(&pool).await.unwrap();
        assert_eq!(event, (user_id.clone(), session_id, "refresh_token_reuse".to_string()));
        let credential_columns: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name = 'auth_security_event'
               AND (column_name LIKE '%token%' OR column_name LIKE '%hash%' OR column_name LIKE '%secret%')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(credential_columns, 0, "security events must not store credentials or credential hashes");
        sqlx::query("DELETE FROM users WHERE id = $1").bind(&user_id).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn expired_consumed_refresh_token_history_is_garbage_collected() {
        let pool = isolated_pool(2).await;
        let database = PostgresDatabase::from_pool(pool.clone());
        let service = PostgresAccountService::new(database, crypto::AccountTokenKey::test_key());
        let user_id = uuid::Uuid::new_v4().to_string();
        let email = format!("refresh-gc-{}@example.com", uuid::Uuid::new_v4().simple());
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)").bind(&user_id).bind(&email).execute(&pool).await.unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let original = issue_token_pair(&mut transaction, &user_id).await.unwrap();
        transaction.commit().await.unwrap();
        let original_refresh_hash = crypto::sha256_hex(original.refresh_token.as_bytes());

        let rotated = service.refresh_session(&original.refresh_token).await.unwrap();
        sqlx::query("UPDATE auth_consumed_refresh_token SET expires_at = now() - INTERVAL '1 second' WHERE token_hash = $1").bind(&original_refresh_hash).execute(&pool).await.unwrap();
        service.refresh_session(&rotated.refresh_token).await.unwrap();
        let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_consumed_refresh_token WHERE token_hash = $1").bind(&original_refresh_hash).fetch_one(&pool).await.unwrap();
        assert_eq!(retained, 0);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(&user_id).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn session_management_is_owned_scoped_and_preserves_the_current_session() {
        let pool = isolated_pool(3).await;
        let database = PostgresDatabase::from_pool(pool.clone());
        let service = PostgresAccountService::new(database, crypto::AccountTokenKey::test_key());
        let user_id = uuid::Uuid::new_v4().to_string();
        let other_user_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2), ($3, $4)")
            .bind(&user_id)
            .bind(format!("sessions-{}@example.com", uuid::Uuid::new_v4().simple()))
            .bind(&other_user_id)
            .bind(format!("sessions-other-{}@example.com", uuid::Uuid::new_v4().simple()))
            .execute(&pool)
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let current = issue_token_pair(&mut transaction, &user_id).await.unwrap();
        let selected = issue_token_pair(&mut transaction, &user_id).await.unwrap();
        let remaining = issue_token_pair(&mut transaction, &user_id).await.unwrap();
        let foreign = issue_token_pair(&mut transaction, &other_user_id).await.unwrap();
        transaction.commit().await.unwrap();
        let current_hash = crypto::sha256_hex(current.access_token.as_bytes());
        let selected_hash = crypto::sha256_hex(selected.access_token.as_bytes());
        let foreign_hash = crypto::sha256_hex(foreign.access_token.as_bytes());
        let current_session_id: String = sqlx::query_scalar("SELECT session_id FROM auth_token WHERE token_hash = $1").bind(current_hash).fetch_one(&pool).await.unwrap();
        let selected_session_id: String = sqlx::query_scalar("SELECT session_id FROM auth_token WHERE token_hash = $1").bind(selected_hash).fetch_one(&pool).await.unwrap();
        let foreign_session_id: String = sqlx::query_scalar("SELECT session_id FROM auth_token WHERE token_hash = $1").bind(foreign_hash).fetch_one(&pool).await.unwrap();

        let sessions = service.list_sessions(&current.access_token).await.unwrap();
        assert_eq!(sessions.len(), 3);
        assert_eq!(sessions.iter().filter(|session| session.current).count(), 1);
        assert!(sessions.iter().any(|session| session.current && session.session_id == current_session_id));
        assert!(!sessions.iter().any(|session| session.session_id == foreign_session_id));
        service.resolve_token(&current.access_token).await.unwrap();
        service.resolve_token(&selected.access_token).await.unwrap();
        service.resolve_token(&remaining.access_token).await.unwrap();

        let foreign_attempt = service.revoke_session(&current.access_token, &foreign_session_id).await.unwrap();
        assert_eq!(foreign_attempt, SessionRevocation { revoked: false, signed_out: false });
        assert!(service.resolve_token(&foreign.access_token).await.is_ok());

        let selected_revocation = service.revoke_session(&current.access_token, &selected_session_id).await.unwrap();
        assert_eq!(selected_revocation, SessionRevocation { revoked: true, signed_out: false });
        assert!(matches!(service.resolve_token(&selected.access_token).await, Err(AuthError::InvalidCredentials)));
        assert!(service.resolve_token(&current.access_token).await.is_ok());

        assert_eq!(service.revoke_other_sessions(&current.access_token).await.unwrap(), 1);
        assert!(matches!(service.resolve_token(&remaining.access_token).await, Err(AuthError::InvalidCredentials)));
        assert!(service.resolve_token(&current.access_token).await.is_ok());
        assert_eq!(service.list_sessions(&current.access_token).await.unwrap().len(), 1);

        let current_revocation = service.revoke_session(&current.access_token, &current_session_id).await.unwrap();
        assert_eq!(current_revocation, SessionRevocation { revoked: true, signed_out: true });
        assert!(matches!(service.resolve_token(&current.access_token).await, Err(AuthError::InvalidCredentials)));
        assert!(matches!(service.list_sessions(&current.access_token).await, Err(AuthError::InvalidCredentials)));
        sqlx::query("DELETE FROM users WHERE id = $1 OR id = $2").bind(&user_id).bind(&other_user_id).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn concurrent_refresh_token_use_leaves_no_winning_credentials() {
        let pool = isolated_pool(4).await;
        let database = PostgresDatabase::from_pool(pool.clone());
        let first_service = PostgresAccountService::new(database.clone(), crypto::AccountTokenKey::test_key());
        let second_service = PostgresAccountService::new(database, crypto::AccountTokenKey::test_key());
        let user_id = uuid::Uuid::new_v4().to_string();
        let email = format!("refresh-race-{}@example.com", uuid::Uuid::new_v4().simple());
        sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)").bind(&user_id).bind(&email).execute(&pool).await.unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let original = issue_token_pair(&mut transaction, &user_id).await.unwrap();
        transaction.commit().await.unwrap();

        let (first, second) = tokio::join!(first_service.refresh_session(&original.refresh_token), second_service.refresh_session(&original.refresh_token));
        let winner = match (first, second) {
            (Ok(winner), Err(AuthError::InvalidCredentials)) | (Err(AuthError::InvalidCredentials), Ok(winner)) => winner,
            (Ok(_), Ok(_)) => panic!("both concurrent refreshes succeeded"),
            (Ok(_), Err(error)) | (Err(error), Ok(_)) => panic!("concurrent refresh returned an unexpected error: {error}"),
            (Err(first), Err(second)) => panic!("both concurrent refreshes failed: {first}; {second}"),
        };
        assert!(matches!(first_service.resolve_token(&winner.token).await, Err(AuthError::InvalidCredentials)));
        assert!(matches!(first_service.refresh_session(&winner.refresh_token).await, Err(AuthError::InvalidCredentials)));
        let active_sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_session WHERE user_id = $1").bind(&user_id).fetch_one(&pool).await.unwrap();
        assert_eq!(active_sessions, 0);
        let reuse_events: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_security_event WHERE user_id = $1 AND event_type = 'refresh_token_reuse'").bind(&user_id).fetch_one(&pool).await.unwrap();
        assert_eq!(reuse_events, 1);
        sqlx::query("DELETE FROM users WHERE id = $1").bind(&user_id).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn authentication_rate_limits_survive_service_reconstruction() {
        let pool = isolated_pool(2).await;
        let database = PostgresDatabase::from_pool(pool);
        let service = PostgresAccountService::new(database.clone(), crypto::AccountTokenKey::test_key());
        let unique = uuid::Uuid::new_v4().to_string();
        for _ in 0..8 {
            service.enforce_rate_limit(AuthAttempt::Login, &unique, &unique).await.unwrap();
        }
        let reconstructed = PostgresAccountService::new(database, crypto::AccountTokenKey::test_key());
        assert!(matches!(reconstructed.enforce_rate_limit(AuthAttempt::Login, &unique, &unique).await, Err(AuthError::RateLimited)));
        reconstructed.clear_subject_rate_limit(AuthAttempt::Login, &unique).await.unwrap();
        reconstructed.enforce_rate_limit(AuthAttempt::Login, &unique, &unique).await.unwrap();
    }
}
