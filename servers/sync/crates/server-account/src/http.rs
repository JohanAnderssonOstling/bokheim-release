//! HTTP adapter for account creation, credentials, and sessions.

use crate::core::AuthAttempt;
use crate::postgres::PostgresAccountService;
use account_contract::{
    AuthResponse, EmailRequest, EmailVerificationRequest, LoginRequest, MeResponse, OtherSessionsRevocationResponse, PublicRegistrationRequest, RefreshRequest, ResetPasswordRequest, SessionRevocationResponse, SessionSummary,
};
use axum::async_trait;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::{
    header::{CACHE_CONTROL, SET_COOKIE},
    request::Parts,
    HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

const MAX_AUTH_BODY_BYTES: usize = 16 * 1024;
const GENERIC_ACCOUNT_RESPONSE_FLOOR: std::time::Duration = std::time::Duration::from_millis(300);
const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
const WEB_ACCESS_COOKIE: &str = "__Host-bokheim_access";
const WEB_REFRESH_COOKIE: &str = "__Host-bokheim_refresh";

pub fn auth_error(error: crate::core::AuthError) -> StatusCode {
    match error {
        crate::core::AuthError::InvalidCredentials | crate::core::AuthError::InvalidOneTimeToken => StatusCode::UNAUTHORIZED,
        crate::core::AuthError::EmailTaken => StatusCode::CONFLICT,
        crate::core::AuthError::Forbidden => StatusCode::FORBIDDEN,
        crate::core::AuthError::InvalidPassword { .. } | crate::core::AuthError::InvalidEmail => StatusCode::BAD_REQUEST,
        crate::core::AuthError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        crate::core::AuthError::Internal(reason) => {
            tracing::error!(%reason, "authentication error");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientAddressSource {
    DirectPeer,
    TrustedForwardedFor,
}

#[derive(Clone)]
struct AccountState {
    accounts: Arc<PostgresAccountService>,
    client_address_source: ClientAddressSource,
}

pub fn router(accounts: Arc<PostgresAccountService>, client_address_source: ClientAddressSource) -> Router {
    let state = AccountState { accounts, client_address_source };
    Router::new()
        .route("/api/auth/login", post(post_login).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .route("/api/auth/web/login", post(post_web_login).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .route("/api/auth/web/refresh", post(post_web_refresh))
        .route("/api/auth/web/logout", post(post_web_logout))
        .route("/api/auth/web/me", get(get_web_me))
        .route("/api/auth/web/verify-email", post(post_web_verify_email).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .route("/api/auth/refresh", post(post_refresh).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .route("/api/auth/logout", post(post_logout))
        .route("/api/auth/me", get(get_me))
        .route("/api/auth/sessions", get(get_sessions).delete(delete_other_sessions))
        .route("/api/auth/sessions/:session_id", axum::routing::delete(delete_session))
        .route("/api/auth/register", post(post_public_registration).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .route("/api/auth/resend-verification", post(post_resend_verification).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .route("/api/auth/verify-email", post(post_verify_email).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .route("/api/auth/request-password-reset", post(post_request_password_reset).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .route("/api/auth/reset-password", post(post_reset_password).layer(DefaultBodyLimit::max(MAX_AUTH_BODY_BYTES)))
        .with_state(state)
}

struct AccountUser {
    user_id: String,
    email: String,
}

#[async_trait]
impl FromRequestParts<AccountState> for AccountUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &AccountState) -> Result<Self, Self::Rejection> {
        let token = bearer_token(&parts.headers)?;
        let user = state.accounts.resolve_token(token).await.map_err(auth_error)?;
        Ok(Self { user_id: user.user_id, email: user.email })
    }
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, StatusCode> {
    headers.get(axum::http::header::AUTHORIZATION).and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer ")).filter(|token| !token.is_empty()).ok_or(StatusCode::UNAUTHORIZED)
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get_all(axum::http::header::COOKIE).iter().filter_map(|value| value.to_str().ok()).flat_map(|value| value.split(';')).find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

pub fn web_access_token(headers: &HeaderMap) -> Option<&str> {
    cookie_value(headers, WEB_ACCESS_COOKIE)
}

/// Cookie-authenticated writes are accepted only from a browser which says
/// the request originated at the same origin. `Sec-Fetch-Site` is a forbidden
/// request header, so cross-origin JavaScript cannot forge this value.
pub fn require_same_origin_web_request(method: &Method, headers: &HeaderMap) -> Result<(), StatusCode> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    match headers.get(&SEC_FETCH_SITE).and_then(|value| value.to_str().ok()) {
        Some("same-origin") => Ok(()),
        Some(_) => Err(StatusCode::FORBIDDEN),
        None => exact_origin_matches_request(headers).then_some(()).ok_or(StatusCode::FORBIDDEN),
    }
}

fn exact_origin_matches_request(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(axum::http::header::ORIGIN).and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<axum::http::Uri>().ok()) else {
        return false;
    };
    let Some(scheme) = origin.scheme_str().filter(|scheme| matches!(*scheme, "http" | "https")) else {
        return false;
    };
    let Some(authority) = origin.authority().map(|authority| authority.as_str()) else {
        return false;
    };
    let Some(host) = headers.get(axum::http::header::HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let forwarded_scheme = headers.get(&X_FORWARDED_PROTO).and_then(|value| value.to_str().ok());
    authority.eq_ignore_ascii_case(host) && forwarded_scheme.is_none_or(|forwarded| forwarded == scheme)
}

pub fn append_web_session_cookies(response: &mut Response, issued: &crate::core::IssuedSession) -> Result<(), StatusCode> {
    let now = chrono::Utc::now();
    let access_age = (issued.expires_at - now).num_seconds().max(0);
    let refresh_age = (issued.refresh_expires_at - now).num_seconds().max(0);
    for value in
        [format!("{WEB_ACCESS_COOKIE}={}; Path=/; Max-Age={access_age}; Secure; HttpOnly; SameSite=Lax", issued.token), format!("{WEB_REFRESH_COOKIE}={}; Path=/; Max-Age={refresh_age}; Secure; HttpOnly; SameSite=Lax", issued.refresh_token)]
    {
        response.headers_mut().append(SET_COOKIE, HeaderValue::from_str(&value).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?);
    }
    Ok(())
}

fn expire_session_cookies(response: &mut Response) {
    for name in [WEB_ACCESS_COOKIE, WEB_REFRESH_COOKIE] {
        response.headers_mut().append(SET_COOKIE, HeaderValue::from_str(&format!("{name}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax")).expect("static cookie attributes are valid"));
    }
}

fn web_session_response(headers: &HeaderMap, issued: crate::core::IssuedSession) -> Result<Response, StatusCode> {
    let profile = MeResponse { user_id: issued.user.user_id.clone(), email: issued.user.email.clone() };
    let mut response = server_wire_http::response(headers, profile)?;
    append_web_session_cookies(&mut response, &issued)?;
    response.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub fn client_address(source: ClientAddressSource, headers: &HeaderMap, remote: Option<ConnectInfo<SocketAddr>>) -> Result<IpAddr, StatusCode> {
    match source {
        ClientAddressSource::DirectPeer => remote.map(|ConnectInfo(address)| address.ip()).ok_or(StatusCode::INTERNAL_SERVER_ERROR),
        ClientAddressSource::TrustedForwardedFor => {
            let mut values = headers.get_all(X_FORWARDED_FOR).iter();
            let value = values.next().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
            if values.next().is_some() {
                return Err(StatusCode::BAD_REQUEST);
            }
            let value = value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
            if value.contains(',') {
                return Err(StatusCode::BAD_REQUEST);
            }
            value.trim().parse().map_err(|_| StatusCode::BAD_REQUEST)
        }
    }
}

async fn post_login(State(state): State<AccountState>, headers: HeaderMap, remote: Option<ConnectInfo<SocketAddr>>, body: Bytes) -> Result<Response, StatusCode> {
    let request: LoginRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let address = client_address(state.client_address_source, &headers, remote)?.to_string();
    state.accounts.enforce_rate_limit(AuthAttempt::Login, &address, &request.email).await.map_err(auth_error)?;
    let issued = state.accounts.login(&request.email, &request.password).await.map_err(auth_error)?;
    state.accounts.clear_subject_rate_limit(AuthAttempt::Login, &request.email).await.map_err(auth_error)?;
    server_wire_http::response(&headers, auth_response(issued))
}

async fn post_web_login(State(state): State<AccountState>, headers: HeaderMap, remote: Option<ConnectInfo<SocketAddr>>, body: Bytes) -> Result<Response, StatusCode> {
    require_same_origin_web_request(&Method::POST, &headers)?;
    let request: LoginRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let address = client_address(state.client_address_source, &headers, remote)?.to_string();
    state.accounts.enforce_rate_limit(AuthAttempt::Login, &address, &request.email).await.map_err(auth_error)?;
    let issued = state.accounts.login(&request.email, &request.password).await.map_err(auth_error)?;
    state.accounts.clear_subject_rate_limit(AuthAttempt::Login, &request.email).await.map_err(auth_error)?;
    web_session_response(&headers, issued)
}

async fn post_logout(State(state): State<AccountState>, headers: HeaderMap, body: Bytes) -> Result<StatusCode, StatusCode> {
    let token = match server_wire_http::decode_optional_request::<RefreshRequest>(&headers, &body, MAX_AUTH_BODY_BYTES)? {
        Some(request) => request.refresh_token,
        None => bearer_token(&headers)?.to_owned(),
    };
    state.accounts.logout(&token).await.map_err(auth_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn post_refresh(State(state): State<AccountState>, headers: HeaderMap, body: Bytes) -> Result<Response, StatusCode> {
    let request: RefreshRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let issued = state.accounts.refresh_session(&request.refresh_token).await.map_err(auth_error)?;
    server_wire_http::response(&headers, auth_response(issued))
}

async fn post_web_refresh(State(state): State<AccountState>, headers: HeaderMap) -> Result<Response, StatusCode> {
    require_same_origin_web_request(&Method::POST, &headers)?;
    let refresh_token = cookie_value(&headers, WEB_REFRESH_COOKIE).ok_or(StatusCode::UNAUTHORIZED)?;
    let issued = state.accounts.refresh_session(refresh_token).await.map_err(auth_error)?;
    web_session_response(&headers, issued)
}

async fn post_web_logout(State(state): State<AccountState>, headers: HeaderMap) -> Result<Response, StatusCode> {
    require_same_origin_web_request(&Method::POST, &headers)?;
    if let Some(refresh_token) = cookie_value(&headers, WEB_REFRESH_COOKIE) {
        state.accounts.logout(refresh_token).await.map_err(auth_error)?;
    }
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    expire_session_cookies(&mut response);
    response.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn get_web_me(State(state): State<AccountState>, headers: HeaderMap) -> Result<Response, StatusCode> {
    let token = web_access_token(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = state.accounts.resolve_token(token).await.map_err(auth_error)?;
    let mut response = server_wire_http::response(&headers, MeResponse { user_id: user.user_id, email: user.email })?;
    response.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn post_public_registration(State(state): State<AccountState>, headers: HeaderMap, remote: Option<ConnectInfo<SocketAddr>>, body: Bytes) -> Result<StatusCode, StatusCode> {
    let request: PublicRegistrationRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let started = tokio::time::Instant::now();
    let address = client_address(state.client_address_source, &headers, remote)?.to_string();
    state.accounts.enforce_rate_limit(AuthAttempt::PublicRegistration, &address, &request.email).await.map_err(auth_error)?;
    state.accounts.request_public_registration(&request.email, &request.password).await.map_err(auth_error)?;
    tokio::time::sleep_until(started + GENERIC_ACCOUNT_RESPONSE_FLOOR).await;
    Ok(StatusCode::ACCEPTED)
}

async fn post_resend_verification(State(state): State<AccountState>, headers: HeaderMap, remote: Option<ConnectInfo<SocketAddr>>, body: Bytes) -> Result<StatusCode, StatusCode> {
    let request: EmailRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let started = tokio::time::Instant::now();
    let address = client_address(state.client_address_source, &headers, remote)?.to_string();
    state.accounts.enforce_rate_limit(AuthAttempt::ResendVerification, &address, &request.email).await.map_err(auth_error)?;
    state.accounts.resend_verification(&request.email).await.map_err(auth_error)?;
    tokio::time::sleep_until(started + GENERIC_ACCOUNT_RESPONSE_FLOOR).await;
    Ok(StatusCode::ACCEPTED)
}

async fn post_verify_email(State(state): State<AccountState>, headers: HeaderMap, remote: Option<ConnectInfo<SocketAddr>>, body: Bytes) -> Result<Response, StatusCode> {
    let request: EmailVerificationRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let address = client_address(state.client_address_source, &headers, remote)?.to_string();
    state.accounts.enforce_rate_limit(AuthAttempt::VerifyEmail, &address, &request.email).await.map_err(auth_error)?;
    let issued = state.accounts.verify_email(&request.email, &request.pin).await.map_err(auth_error)?;
    state.accounts.clear_subject_rate_limit(AuthAttempt::VerifyEmail, &request.email).await.map_err(auth_error)?;
    server_wire_http::response(&headers, auth_response(issued))
}

async fn post_web_verify_email(State(state): State<AccountState>, headers: HeaderMap, remote: Option<ConnectInfo<SocketAddr>>, body: Bytes) -> Result<Response, StatusCode> {
    require_same_origin_web_request(&Method::POST, &headers)?;
    let request: EmailVerificationRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let address = client_address(state.client_address_source, &headers, remote)?.to_string();
    state.accounts.enforce_rate_limit(AuthAttempt::VerifyEmail, &address, &request.email).await.map_err(auth_error)?;
    let issued = state.accounts.verify_email(&request.email, &request.pin).await.map_err(auth_error)?;
    state.accounts.clear_subject_rate_limit(AuthAttempt::VerifyEmail, &request.email).await.map_err(auth_error)?;
    web_session_response(&headers, issued)
}

fn auth_response(issued: crate::core::IssuedSession) -> AuthResponse {
    AuthResponse { token: issued.token, expires_at: issued.expires_at.timestamp(), refresh_token: issued.refresh_token, refresh_expires_at: issued.refresh_expires_at.timestamp(), user_id: issued.user.user_id, email: issued.user.email }
}

async fn post_request_password_reset(State(state): State<AccountState>, headers: HeaderMap, remote: Option<ConnectInfo<SocketAddr>>, body: Bytes) -> Result<StatusCode, StatusCode> {
    let request: EmailRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let started = tokio::time::Instant::now();
    let address = client_address(state.client_address_source, &headers, remote)?.to_string();
    state.accounts.enforce_rate_limit(AuthAttempt::RequestPasswordReset, &address, &request.email).await.map_err(auth_error)?;
    state.accounts.request_password_reset(&request.email).await.map_err(auth_error)?;
    tokio::time::sleep_until(started + GENERIC_ACCOUNT_RESPONSE_FLOOR).await;
    Ok(StatusCode::ACCEPTED)
}

async fn post_reset_password(State(state): State<AccountState>, headers: HeaderMap, remote: Option<ConnectInfo<SocketAddr>>, body: Bytes) -> Result<StatusCode, StatusCode> {
    let request: ResetPasswordRequest = server_wire_http::decode_request(&headers, &body, MAX_AUTH_BODY_BYTES)?;
    let address = client_address(state.client_address_source, &headers, remote)?.to_string();
    state.accounts.enforce_rate_limit(AuthAttempt::ResetPassword, &address, "password-reset").await.map_err(auth_error)?;
    state.accounts.reset_password(&request.token, &request.new_password).await.map_err(auth_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_me(headers: HeaderMap, user: AccountUser) -> Result<Response, StatusCode> {
    server_wire_http::response(&headers, MeResponse { user_id: user.user_id, email: user.email })
}

async fn get_sessions(State(state): State<AccountState>, headers: HeaderMap) -> Result<Response, StatusCode> {
    let sessions = state.accounts.list_sessions(bearer_token(&headers)?).await.map_err(auth_error)?;
    server_wire_http::response(
        &headers,
        sessions.into_iter().map(|session| SessionSummary { session_id: session.session_id, created_at: session.created_at.timestamp(), expires_at: session.expires_at.timestamp(), current: session.current }).collect::<Vec<_>>(),
    )
}

async fn delete_session(State(state): State<AccountState>, headers: HeaderMap, Path(session_id): Path<String>) -> Result<Response, StatusCode> {
    if session_id.is_empty() || session_id.len() > 64 || !session_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let revocation = state.accounts.revoke_session(bearer_token(&headers)?, &session_id).await.map_err(auth_error)?;
    server_wire_http::response(&headers, SessionRevocationResponse { revoked: revocation.revoked, signed_out: revocation.signed_out })
}

async fn delete_other_sessions(State(state): State<AccountState>, headers: HeaderMap) -> Result<Response, StatusCode> {
    let revoked = state.accounts.revoke_other_sessions(bearer_token(&headers)?).await.map_err(auth_error)?;
    server_wire_http::response(&headers, OtherSessionsRevocationResponse { revoked })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::COOKIE, HeaderValue};

    #[test]
    fn direct_peer_addresses_ignore_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("203.0.113.9"));
        let peer = "127.0.0.1:1234".parse().unwrap();
        assert_eq!(client_address(ClientAddressSource::DirectPeer, &headers, Some(ConnectInfo(peer))).unwrap(), "127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn trusted_proxy_addresses_require_one_canonical_ip() {
        let mut headers = HeaderMap::new();
        headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("203.0.113.9"));
        assert_eq!(client_address(ClientAddressSource::TrustedForwardedFor, &headers, None).unwrap(), "203.0.113.9".parse::<IpAddr>().unwrap());
        headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("203.0.113.9, 127.0.0.1"));
        assert_eq!(client_address(ClientAddressSource::TrustedForwardedFor, &headers, None), Err(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn web_cookie_authentication_requires_same_origin_for_writes() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("theme=dark; __Host-bokheim_access=access-token; other=value"));
        assert_eq!(web_access_token(&headers), Some("access-token"));
        assert_eq!(require_same_origin_web_request(&Method::POST, &headers), Err(StatusCode::FORBIDDEN));
        headers.insert(SEC_FETCH_SITE, HeaderValue::from_static("cross-site"));
        assert_eq!(require_same_origin_web_request(&Method::DELETE, &headers), Err(StatusCode::FORBIDDEN));
        headers.insert(SEC_FETCH_SITE, HeaderValue::from_static("same-origin"));
        assert_eq!(require_same_origin_web_request(&Method::PUT, &headers), Ok(()));
        headers.remove(SEC_FETCH_SITE);
        headers.insert(axum::http::header::ORIGIN, HeaderValue::from_static("https://app.bokheim.se"));
        headers.insert(axum::http::header::HOST, HeaderValue::from_static("app.bokheim.se"));
        headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static("https"));
        assert_eq!(require_same_origin_web_request(&Method::POST, &headers), Ok(()));
        headers.insert(axum::http::header::ORIGIN, HeaderValue::from_static("https://attacker.example"));
        assert_eq!(require_same_origin_web_request(&Method::POST, &headers), Err(StatusCode::FORBIDDEN));
        assert_eq!(require_same_origin_web_request(&Method::GET, &HeaderMap::new()), Ok(()));
    }
}
