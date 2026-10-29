use super::{internal, normalized_email, valid_password_size, PostgresAccountService};
use crate::core::AuthError;

impl PostgresAccountService {
    /// Permanently removes the identity and all rows owned through cascading
    /// foreign keys, including sessions, libraries, sync state and blob claims.
    /// Wakes asset cleanup after the ownership records have been committed.
    pub async fn delete_account(&self, email: &str, password: &str) -> Result<(), AuthError> {
        let email = normalized_email(email).ok().filter(|_| valid_password_size(password));
        let Some(email) = email else {
            self.verify_password_dummy("invalid-credentials".to_owned()).await?;
            return Err(AuthError::InvalidCredentials);
        };
        let row: Option<(String, String)> = sqlx::query_as("SELECT id, password_hash FROM users WHERE email=$1 AND password_hash IS NOT NULL").bind(&email).fetch_optional(&self.pool).await.map_err(internal)?;
        let Some((user_id, password_hash)) = row else {
            self.verify_password_dummy(password.to_owned()).await?;
            return Err(AuthError::InvalidCredentials);
        };
        if !self.verify_password(password.to_owned(), password_hash.clone()).await? {
            return Err(AuthError::InvalidCredentials);
        }

        let mut transaction = self.pool.begin().await.map_err(internal)?;
        // Recheck the hash in the mutation so a concurrent password reset
        // cannot authorize deletion using the previous password.
        let deleted = sqlx::query("DELETE FROM users WHERE id=$1 AND password_hash=$2").bind(&user_id).bind(&password_hash).execute(&mut *transaction).await.map_err(internal)?.rows_affected();
        if deleted != 1 {
            return Err(AuthError::InvalidCredentials);
        }
        // These email-addressed records have no foreign key to the user.
        sqlx::query("DELETE FROM account_email_outbox WHERE recipient=$1").bind(&email).execute(&mut *transaction).await.map_err(internal)?;
        sqlx::query("DELETE FROM pending_registration WHERE email=$1").bind(&email).execute(&mut *transaction).await.map_err(internal)?;
        transaction.commit().await.map_err(internal)?;
        self.identity_cache.invalidate_user(&user_id);
        self.cleanup.notify_one();
        Ok(())
    }
}
