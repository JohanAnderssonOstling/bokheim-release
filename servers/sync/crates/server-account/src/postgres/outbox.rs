use super::{internal, PostgresAccountService};
use crate::core::{AccountEmailKind, AuthError, QueuedAccountEmail};

impl PostgresAccountService {
    pub async fn claim_email(&self, worker_id: &str) -> Result<Option<QueuedAccountEmail>, AuthError> {
        let row: Option<(String, String, String, String)> = sqlx::query_as(
            "UPDATE account_email_outbox
             SET claimed_by=$1, claim_expires_at=now() + INTERVAL '5 minutes', attempts=attempts+1
             WHERE id=(
                 SELECT id FROM account_email_outbox
                 WHERE available_at <= now()
                   AND attempts < 20
                   AND (claimed_by IS NULL OR claim_expires_at <= now())
                 ORDER BY created_at
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             RETURNING id, recipient, message_kind, token_ciphertext",
        )
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.map(|(id, recipient, kind_name, token_ciphertext)| {
            let kind = match kind_name.as_str() {
                "verify_email" => AccountEmailKind::VerifyEmail,
                "reset_password" => AccountEmailKind::ResetPassword,
                _ => return Err(AuthError::Internal("email outbox contains an unknown message kind".to_string())),
            };
            let token = self.token_protector.decrypt_outbox_token(&id, &recipient, &kind_name, &token_ciphertext)?;
            Ok(QueuedAccountEmail { id, recipient, kind, token })
        })
        .transpose()
    }

    pub async fn complete_email(&self, worker_id: &str, email_id: &str) -> Result<(), AuthError> {
        sqlx::query("DELETE FROM account_email_outbox WHERE id=$1 AND claimed_by=$2").bind(email_id).bind(worker_id).execute(&self.pool).await.map_err(internal)?;
        Ok(())
    }

    pub async fn retry_email(&self, worker_id: &str, email_id: &str, error: &str) -> Result<(), AuthError> {
        let error: String = error.chars().take(512).collect();
        sqlx::query(
            "UPDATE account_email_outbox
             SET claimed_by=NULL,
                 claim_expires_at=NULL,
                 available_at=now() + INTERVAL '1 minute',
                 last_error=$3,
                 recipient=CASE WHEN attempts >= 20 THEN '[redacted]' ELSE recipient END,
                 token_ciphertext=CASE WHEN attempts >= 20 THEN '' ELSE token_ciphertext END
             WHERE id=$1 AND claimed_by=$2",
        )
        .bind(email_id)
        .bind(worker_id)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }
}
