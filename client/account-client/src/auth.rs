use crate::session::{AuthError, Session};
use crate::ServerUrl;
use account_contract::{AuthResponse, EmailRequest, EmailVerificationRequest, LoginRequest, PublicRegistrationRequest, RefreshRequest, ResetPasswordRequest};
use serde::Serialize;

fn session_from_response(server_url: ServerUrl, body: AuthResponse) -> Result<Session, AuthError> {
    Session::try_new_with_tokens(server_url, body.user_id, body.email, body.token, Some(body.expires_at), Some(body.refresh_token), Some(body.refresh_expires_at))
}

async fn post_auth(url: reqwest::Url, request: &impl Serialize) -> Result<AuthResponse, AuthError> {
    Ok(binary_http::send(reqwest::Client::builder().build()?.post(url), request).await?)
}

pub async fn login(server_url: &ServerUrl, email: &str, password: &str) -> Result<Session, AuthError> {
    let response = post_auth(server_url.endpoint("api/auth/login"), &LoginRequest { email: email.to_owned(), password: password.to_owned() }).await?;
    session_from_response(server_url.clone(), response)
}

pub async fn request_public_registration(server_url: &ServerUrl, email: &str, password: &str) -> Result<(), AuthError> {
    Ok(binary_http::send_status(reqwest::Client::builder().build()?.post(server_url.endpoint("api/auth/register")), &PublicRegistrationRequest { email: email.to_owned(), password: password.to_owned() }).await?)
}

pub async fn resend_verification(server_url: &ServerUrl, email: &str) -> Result<(), AuthError> {
    Ok(binary_http::send_status(reqwest::Client::builder().build()?.post(server_url.endpoint("api/auth/resend-verification")), &EmailRequest { email: email.to_owned() }).await?)
}

pub async fn verify_email(server_url: &ServerUrl, email: &str, pin: &str) -> Result<Session, AuthError> {
    let response = post_auth(server_url.endpoint("api/auth/verify-email"), &EmailVerificationRequest { email: email.to_owned(), pin: pin.to_owned() }).await?;
    session_from_response(server_url.clone(), response)
}

pub async fn request_password_reset(server_url: &ServerUrl, email: &str) -> Result<(), AuthError> {
    Ok(binary_http::send_status(reqwest::Client::builder().build()?.post(server_url.endpoint("api/auth/request-password-reset")), &EmailRequest { email: email.to_owned() }).await?)
}

pub async fn reset_password(server_url: &ServerUrl, token: &str, new_password: &str) -> Result<(), AuthError> {
    Ok(binary_http::send_status(reqwest::Client::builder().build()?.post(server_url.endpoint("api/auth/reset-password")), &ResetPasswordRequest { token: token.to_owned(), new_password: new_password.to_owned() }).await?)
}

pub async fn logout(session: &Session) -> Result<(), AuthError> {
    let request = reqwest::Client::builder().build()?.post(session.server_url().endpoint("api/auth/logout"));
    let refresh_token = session.refresh_token().ok_or_else(|| AuthError::InvalidData("session has no refresh token; sign in again".to_owned()))?;
    let response = binary_http::body(request, &RefreshRequest { refresh_token: refresh_token.to_owned() })?.send().await?;
    binary_http::ensure_ok(response).await?;
    Ok(())
}

pub async fn refresh_session(session: &Session) -> Result<Session, AuthError> {
    let refresh_token = session.refresh_token().ok_or_else(|| AuthError::InvalidData("session has no refresh token; sign in again".to_owned()))?;
    if session.refresh_expires_within(0) {
        return Err(AuthError::InvalidData("refresh token has expired; sign in again".to_owned()));
    }
    let response: AuthResponse = binary_http::send(reqwest::Client::builder().build()?.post(session.server_url().endpoint("api/auth/refresh")), &RefreshRequest { refresh_token: refresh_token.to_owned() }).await?;
    let refreshed = session_from_response(session.server_url().clone(), response)?;
    if refreshed.user_id() != session.user_id() {
        return Err(AuthError::InvalidData("refreshed session changed authenticated user".to_owned()));
    }
    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use axum::http::{header, HeaderMap, HeaderValue};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use sync_common::transport as wire;

    const TEST_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[tokio::test]
    async fn refresh_rotates_both_credentials_without_changing_identity() {
        let received_old_refresh = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&received_old_refresh);
        let app = Router::new().route(
            "/api/auth/refresh",
            post(move |headers: HeaderMap, body: Bytes| {
                let observed = Arc::clone(&observed);
                async move {
                    assert!(headers.get(header::CONTENT_ENCODING).is_none());
                    let request: RefreshRequest = wire::decode(&body, wire::MAX_DECODED_REQUEST_BYTES).unwrap();
                    observed.store(request.refresh_token == "b".repeat(43), Ordering::SeqCst);
                    let encoded = wire::encode(&AuthResponse {
                        token: "c".repeat(43),
                        expires_at: 1_900_000_000,
                        refresh_token: "d".repeat(43),
                        refresh_expires_at: 1_910_000_000,
                        user_id: "user-1".to_owned(),
                        email: "reader@example.com".to_owned(),
                    })
                    .unwrap();
                    let mut response = encoded.into_response();
                    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(wire::MEDIA_TYPE));
                    response
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let session =
            Session::try_new_with_tokens(ServerUrl::parse(&format!("http://{address}")).unwrap(), "user-1".to_owned(), "reader@example.com".to_owned(), TEST_TOKEN.to_owned(), Some(1_800_000_000), Some("b".repeat(43)), Some(1_910_000_000))
                .unwrap();

        let refreshed = refresh_session(&session).await.unwrap();

        assert!(received_old_refresh.load(Ordering::SeqCst));
        assert_eq!(refreshed.token(), "c".repeat(43));
        assert_eq!(refreshed.refresh_token(), Some("d".repeat(43).as_str()));
        assert_eq!(refreshed.user_id(), session.user_id());
        assert_eq!(refreshed.email(), session.email());
        server.abort();
    }
}
