use async_trait::async_trait;
use resend_rs::types::CreateEmailBaseOptions;
use resend_rs::{Config, Resend};
use server_account::{AccountEmailKind, AccountEmailSender, PostgresAccountService, QueuedAccountEmail};
use std::io;
use std::sync::Arc;
use std::time::Duration;

const RESEND_API_URL: &str = "https://api.resend.com";
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const ERROR_RETRY_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) struct ResendEmailSender {
    client: Resend,
    from: String,
}

struct EmailContent {
    subject: &'static str,
    text: String,
    html: String,
}

impl ResendEmailSender {
    pub(crate) fn from_config(api_key: Option<String>, from: Option<String>) -> Result<Option<Self>, io::Error> {
        let api_key = api_key.filter(|value| !value.trim().is_empty());
        let from = from.filter(|value| !value.trim().is_empty());
        let (api_key, from) = match (api_key, from) {
            (None, None) => return Ok(None),
            (Some(api_key), Some(from)) => (api_key, from),
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_RESEND_API_KEY and BOKHEIM_EMAIL_FROM must be configured together")),
        };
        if api_key.contains(['\r', '\n']) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_RESEND_API_KEY contains invalid characters"));
        }
        if from.len() > 320 || from.contains(['\r', '\n']) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_EMAIL_FROM is invalid"));
        }

        let http_client = reqwest_13::Client::builder().redirect(reqwest_13::redirect::Policy::none()).timeout(Duration::from_secs(15)).build().map_err(io::Error::other)?;
        let base_url = RESEND_API_URL.parse().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let client = Resend::with_config(Config::builder(api_key).base_url(base_url).client(http_client).build());
        Ok(Some(Self { client, from }))
    }

    fn content(email: &QueuedAccountEmail) -> EmailContent {
        let (subject, introduction) = match email.kind {
            AccountEmailKind::VerifyEmail => ("Verify your Bokheim account", "Enter this six-digit verification PIN in Bokheim to finish creating your account:"),
            AccountEmailKind::ResetPassword => ("Reset your Bokheim password", "Enter this password-reset token in Bokheim:"),
        };
        let text = format!("{introduction}\n\n{}\n\nIf you did not request this, you can ignore this email.", email.token);
        let html = format!("<p>{introduction}</p><p><code style=\"font-size:1.15em\">{}</code></p><p>If you did not request this, you can ignore this email.</p>", email.token);
        EmailContent { subject, text, html }
    }
}

#[async_trait]
impl AccountEmailSender for ResendEmailSender {
    async fn send(&self, email: &QueuedAccountEmail) -> Result<(), String> {
        let content = Self::content(email);
        let request = CreateEmailBaseOptions::new(&self.from, [&email.recipient], content.subject).with_text(&content.text).with_html(&content.html).with_idempotency_key(&email.id);
        self.client.emails.send(request).await.map(|_| ()).map_err(|error| format!("Resend request failed: {error}"))
    }
}

pub(crate) async fn run_email_worker(accounts: Arc<PostgresAccountService>, sender: ResendEmailSender) {
    let worker_id = format!("resend-{}", uuid::Uuid::new_v4());
    loop {
        let email = match accounts.claim_email(&worker_id).await {
            Ok(Some(email)) => email,
            Ok(None) => {
                tokio::time::sleep(IDLE_POLL_INTERVAL).await;
                continue;
            }
            Err(error) => {
                tracing::error!(%error, "failed to claim account email");
                tokio::time::sleep(ERROR_RETRY_INTERVAL).await;
                continue;
            }
        };

        match sender.send(&email).await {
            Ok(()) => {
                if let Err(error) = accounts.complete_email(&worker_id, &email.id).await {
                    tracing::error!(%error, email_id = %email.id, "failed to complete delivered account email");
                }
            }
            Err(error) => {
                tracing::warn!(email_id = %email.id, "account email delivery failed");
                if let Err(retry_error) = accounts.retry_email(&worker_id, &email.id, &error).await {
                    tracing::error!(%retry_error, email_id = %email.id, "failed to schedule account email retry");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_messages_contain_the_token_and_expected_recipient() {
        let email = QueuedAccountEmail { id: "email-id".to_owned(), recipient: "reader@example.com".to_owned(), kind: AccountEmailKind::VerifyEmail, token: "012345".to_owned() };
        let content = ResendEmailSender::content(&email);
        assert_eq!(content.subject, "Verify your Bokheim account");
        assert!(content.text.contains("verification PIN"));
        assert!(content.text.contains("012345"));
        assert!(content.html.contains("012345"));
    }

    #[test]
    fn configuration_is_all_or_nothing_and_rejects_header_injection() {
        assert!(ResendEmailSender::from_config(None, None).unwrap().is_none());
        assert!(ResendEmailSender::from_config(Some("re_test".to_owned()), None).is_err());
        assert!(ResendEmailSender::from_config(Some("bad\nkey".to_owned()), Some("accounts@bokheim.se".to_owned())).is_err());
        assert!(ResendEmailSender::from_config(Some("re_test".to_owned()), Some("bad\nfrom".to_owned())).is_err());
    }
}
