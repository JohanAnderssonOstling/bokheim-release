use super::{crypto, internal, PostgresAccountService};
use crate::core::{AuthAttempt, AuthError};

#[derive(Clone, Copy)]
struct Limits {
    label: &'static str,
    window_seconds: i64,
    network_attempts: i32,
    subject_attempts: Option<i32>,
}

fn limits(attempt: AuthAttempt) -> Limits {
    match attempt {
        AuthAttempt::Login => Limits { label: "login", window_seconds: 300, network_attempts: 100, subject_attempts: Some(8) },
        AuthAttempt::PublicRegistration => Limits { label: "register", window_seconds: 3600, network_attempts: 10, subject_attempts: Some(3) },
        AuthAttempt::ResendVerification => Limits { label: "resend", window_seconds: 3600, network_attempts: 20, subject_attempts: Some(3) },
        AuthAttempt::VerifyEmail => Limits { label: "verify", window_seconds: 3600, network_attempts: 60, subject_attempts: Some(5) },
        AuthAttempt::RequestPasswordReset => Limits { label: "reset_request", window_seconds: 3600, network_attempts: 20, subject_attempts: Some(3) },
        AuthAttempt::ResetPassword => Limits { label: "reset", window_seconds: 3600, network_attempts: 60, subject_attempts: None },
        AuthAttempt::AdminRead => Limits { label: "admin_read", window_seconds: 60, network_attempts: 240, subject_attempts: Some(120) },
    }
}

fn bucket(label: &str, dimension: &str, value: &str) -> String {
    crypto::sha256_hex(format!("{label}\0{dimension}\0{}", value.trim().to_ascii_lowercase()).as_bytes())
}

async fn increment(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>, key: &str, window_seconds: i64) -> Result<i32, AuthError> {
    sqlx::query_scalar(
        "INSERT INTO account_rate_limit(bucket_key, window_started_at, attempts)
         VALUES ($1, now(), 1)
         ON CONFLICT (bucket_key) DO UPDATE SET
             window_started_at = CASE
                 WHEN account_rate_limit.window_started_at <= now() - ($2 * INTERVAL '1 second') THEN now()
                 ELSE account_rate_limit.window_started_at
             END,
             attempts = CASE
                 WHEN account_rate_limit.window_started_at <= now() - ($2 * INTERVAL '1 second') THEN 1
                 ELSE account_rate_limit.attempts + 1
             END
         RETURNING attempts",
    )
    .bind(key)
    .bind(window_seconds)
    .fetch_one(&mut **transaction)
    .await
    .map_err(internal)
}

impl PostgresAccountService {
    pub(super) async fn enforce_rate_limit_impl(&self, attempt: AuthAttempt, network_key: &str, subject: &str) -> Result<(), AuthError> {
        let limits = limits(attempt);
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        sqlx::query(
            "DELETE FROM account_rate_limit
             WHERE bucket_key IN (
                 SELECT bucket_key FROM account_rate_limit
                 WHERE window_started_at < now() - INTERVAL '2 days'
                 ORDER BY window_started_at
                 LIMIT 100
             )",
        )
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        let network = bucket(limits.label, "network", network_key);
        let network_count = increment(&mut transaction, &network, limits.window_seconds).await?;
        let subject_count = if limits.subject_attempts.is_some() {
            let subject = bucket(limits.label, "subject", subject);
            Some(increment(&mut transaction, &subject, limits.window_seconds).await?)
        } else {
            None
        };
        transaction.commit().await.map_err(internal)?;
        if network_count > limits.network_attempts || subject_count.zip(limits.subject_attempts).is_some_and(|(actual, limit)| actual > limit) {
            return Err(AuthError::RateLimited);
        }
        Ok(())
    }

    pub(super) async fn clear_subject_rate_limit_impl(&self, attempt: AuthAttempt, subject: &str) -> Result<(), AuthError> {
        let limits = limits(attempt);
        if limits.subject_attempts.is_none() {
            return Ok(());
        }
        let key = bucket(limits.label, "subject", subject);
        sqlx::query("DELETE FROM account_rate_limit WHERE bucket_key=$1").bind(key).execute(&self.pool).await.map_err(internal)?;
        Ok(())
    }
}
