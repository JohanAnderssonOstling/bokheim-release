use account_contract::{
    AccountSnapshotResponse, AuthResponse, EmailRequest, EmailVerificationRequest, LoginRequest, OtherSessionsRevocationResponse, PublicRegistrationRequest, RefreshRequest, ResetPasswordRequest, SessionRevocationResponse,
    SessionSummary, StorageUsageResponse,
};
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::Router;
use server_account::{AccountEmailKind, AccountTokenKey, ClientAddressSource, PostgresAccountService};
use server_postgres::PostgresDatabase;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

async fn post<T: serde::Serialize + ?Sized>(app: &Router, path: &str, body: &T, bearer: Option<&str>) -> axum::response::Response {
    post_from(app, path, body, bearer, "203.0.113.20").await
}

async fn post_from<T: serde::Serialize + ?Sized>(app: &Router, path: &str, body: &T, bearer: Option<&str>, address: &str) -> axum::response::Response {
    let encoded = wire::encode(body).unwrap();
    let mut request = Request::builder().method("POST").uri(path).header(header::CONTENT_TYPE, wire::MEDIA_TYPE).header(header::ACCEPT, wire::MEDIA_TYPE).header("x-forwarded-for", address);
    if let Some(token) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    app.clone().oneshot(request.body(Body::from(encoded)).unwrap()).await.unwrap()
}

async fn auth_response(response: axum::response::Response) -> AuthResponse {
    assert!(response.status().is_success());
    wire::decode(&to_bytes(response.into_body(), 64 * 1024).await.unwrap(), 64 * 1024).unwrap()
}

async fn response_body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    assert!(response.status().is_success());
    wire::decode(&to_bytes(response.into_body(), 64 * 1024).await.unwrap(), 64 * 1024).unwrap()
}

fn accepted(request: axum::http::request::Builder) -> axum::http::request::Builder {
    request.header(header::ACCEPT, wire::MEDIA_TYPE)
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn public_registration_verification_and_reset_are_end_to_end_safe() {
    let database_url = std::env::var("SYNC_E2E_DATABASE_URL").expect("run through Backend/tests/run_sync_e2e.sh");
    let admin = PgPool::connect(&database_url).await.unwrap();
    let schema = format!("public_registration_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await.unwrap();
    let search_path = format!("SET search_path TO {schema}");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let statement = search_path.clone();
            Box::pin(async move { sqlx::query(&statement).execute(connection).await.map(|_| ()) })
        })
        .connect(&database_url)
        .await
        .unwrap();
    let database = PostgresDatabase::from_pool(pool);
    server_postgres::migrate(&database).await.unwrap();
    let accounts = Arc::new(PostgresAccountService::new(database.clone(), AccountTokenKey::from_base64("WlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlo=").unwrap()));
    let disk = tempfile::tempdir().unwrap();
    let app = sync_server::app(
        database.clone(),
        accounts.clone(),
        sync_server::AppConfig { books: disk.path().join("books"), thumbnails: disk.path().join("thumbnails"), client_address_source: ClientAddressSource::TrustedForwardedFor, ..Default::default() },
    )
    .await
    .unwrap();
    let unique = Uuid::new_v4().simple().to_string();
    let email = format!("public-{unique}@example.com");
    let original_password = "correct-horse-public-password";

    let registration = PublicRegistrationRequest { email: email.clone(), password: original_password.to_owned() };
    let registered = post(&app, "/api/auth/register", &registration, None).await;
    assert_eq!(registered.status(), StatusCode::ACCEPTED);
    let stored_ciphertext: String = sqlx::query_scalar("SELECT token_ciphertext FROM account_email_outbox WHERE recipient=$1 AND message_kind='verify_email'").bind(&email).fetch_one(database.pool()).await.unwrap();
    assert!(stored_ciphertext.starts_with("v1."));
    let verification = accounts.claim_email("registration-test").await.unwrap().unwrap();
    assert_eq!(verification.kind, AccountEmailKind::VerifyEmail);
    assert_eq!(verification.recipient, email);
    assert_eq!(verification.token.len(), account_contract::EMAIL_VERIFICATION_PIN_DIGITS);
    assert!(verification.token.bytes().all(|byte| byte.is_ascii_digit()));
    assert_ne!(stored_ciphertext, verification.token, "PostgreSQL must not contain the recoverable PIN in plaintext");
    assert_eq!(post(&app, "/api/auth/verify-email", &EmailVerificationRequest { email: "other@example.com".to_owned(), pin: verification.token.clone(), password: original_password.to_owned() }, None).await.status(), StatusCode::UNAUTHORIZED);
    // The mailed PIN alone does not verify: whoever enters it must also know
    // the pending registration's password, or they would adopt another's.
    let foreign_password = EmailVerificationRequest { email: email.clone(), pin: verification.token.clone(), password: "someone-elses-chosen-password".to_owned() };
    assert_eq!(post(&app, "/api/auth/verify-email", &foreign_password, None).await.status(), StatusCode::UNAUTHORIZED);

    // Existing account identifiers receive the same generic response and do
    // not create another pending account or leak an email.
    let verification_request = EmailVerificationRequest { email: email.clone(), pin: verification.token, password: original_password.to_owned() };
    let verified = auth_response(post(&app, "/api/auth/verify-email", &verification_request, None).await).await;
    assert_eq!(verified.email, email);
    assert_eq!(post(&app, "/api/auth/verify-email", &verification_request, None).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(post(&app, "/api/auth/register", &registration, None).await.status(), StatusCode::ACCEPTED);
    assert!(accounts.claim_email("registration-test").await.unwrap().is_none());
    assert_eq!(app.clone().oneshot(accepted(Request::get("/api/auth/me")).header(header::AUTHORIZATION, format!("Bearer {}", verified.token)).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::OK);

    // A six-digit PIN has intentionally low entropy, so attempts are also
    // persistently bounded by normalized email address, independent of IP.
    let locked_email = format!("pin-limit-{unique}@example.com");
    let locked_registration = PublicRegistrationRequest { email: locked_email.clone(), password: original_password.to_owned() };
    assert_eq!(post(&app, "/api/auth/register", &locked_registration, None).await.status(), StatusCode::ACCEPTED);
    let locked_verification = accounts.claim_email("registration-test").await.unwrap().unwrap();
    let wrong_pin = if locked_verification.token == "000000" { "000001" } else { "000000" };
    let wrong_request = EmailVerificationRequest { email: locked_email.clone(), pin: wrong_pin.to_owned(), password: original_password.to_owned() };
    for attempt in 0..5 {
        let address = format!("198.51.100.{}", attempt + 1);
        assert_eq!(post_from(&app, "/api/auth/verify-email", &wrong_request, None, &address).await.status(), StatusCode::UNAUTHORIZED);
    }
    // A trailing-dot spelling names the same account and shares its bucket.
    let respelled = EmailVerificationRequest { email: format!("{locked_email}."), pin: wrong_pin.to_owned(), password: original_password.to_owned() };
    assert_eq!(post_from(&app, "/api/auth/verify-email", &respelled, None, "198.51.100.6").await.status(), StatusCode::TOO_MANY_REQUESTS);
    let locked_correct = EmailVerificationRequest { email: locked_email, pin: locked_verification.token, password: original_password.to_owned() };
    assert_eq!(post_from(&app, "/api/auth/verify-email", &locked_correct, None, "198.51.100.7").await.status(), StatusCode::TOO_MANY_REQUESTS);

    assert_eq!(post(&app, "/api/auth/request-password-reset", &EmailRequest { email: "unknown@example.com".to_owned() }, None).await.status(), StatusCode::ACCEPTED);
    assert_eq!(post(&app, "/api/auth/request-password-reset", &EmailRequest { email: email.clone() }, None).await.status(), StatusCode::ACCEPTED);
    let reset = accounts.claim_email("registration-test").await.unwrap().unwrap();
    assert_eq!(reset.kind, AccountEmailKind::ResetPassword);
    let new_password = "a-different-correct-horse-password";
    let reset_request = ResetPasswordRequest { token: reset.token, new_password: new_password.to_owned() };
    assert_eq!(post(&app, "/api/auth/reset-password", &reset_request, None).await.status(), StatusCode::NO_CONTENT);

    // Reset is single-use and revokes every pre-reset session.
    assert_eq!(post(&app, "/api/auth/reset-password", &reset_request, None).await.status(), StatusCode::UNAUTHORIZED);
    let old_session = app.clone().oneshot(Request::builder().uri("/api/auth/me").header(header::AUTHORIZATION, format!("Bearer {}", verified.token)).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(old_session.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(post(&app, "/api/auth/refresh", &RefreshRequest { refresh_token: verified.refresh_token.clone() }, None).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(post(&app, "/api/auth/login", &LoginRequest { email: email.clone(), password: original_password.to_owned() }, None).await.status(), StatusCode::UNAUTHORIZED);
    let valid_login = LoginRequest { email: email.clone(), password: new_password.to_owned() };

    let current = auth_response(post(&app, "/api/auth/login", &valid_login, None).await).await;
    let other = auth_response(post(&app, "/api/auth/login", &valid_login, None).await).await;
    let sessions_response = app.clone().oneshot(accepted(Request::get("/api/auth/sessions")).header(header::AUTHORIZATION, format!("Bearer {}", current.token)).body(Body::empty()).unwrap()).await.unwrap();
    let sessions: Vec<SessionSummary> = response_body(sessions_response).await;
    assert_eq!(sessions.len(), 2);
    let current_session = sessions.iter().find(|session| session.current).unwrap();
    let other_session = sessions.iter().find(|session| !session.current).unwrap();
    assert_eq!(app.clone().oneshot(accepted(Request::get("/api/auth/me")).header(header::AUTHORIZATION, format!("Bearer {}", other.token)).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::OK);

    let revoked: SessionRevocationResponse = response_body(
        app.clone().oneshot(accepted(Request::delete(format!("/api/auth/sessions/{}", other_session.session_id))).header(header::AUTHORIZATION, format!("Bearer {}", current.token)).body(Body::empty()).unwrap()).await.unwrap(),
    )
    .await;
    assert!(revoked.revoked);
    assert!(!revoked.signed_out);
    assert_eq!(app.clone().oneshot(Request::get("/api/auth/me").header(header::AUTHORIZATION, format!("Bearer {}", other.token)).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::UNAUTHORIZED);

    let third = auth_response(post(&app, "/api/auth/login", &valid_login, None).await).await;
    assert_eq!(app.clone().oneshot(accepted(Request::get("/api/auth/me")).header(header::AUTHORIZATION, format!("Bearer {}", third.token)).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::OK);
    let others: OtherSessionsRevocationResponse =
        response_body(app.clone().oneshot(accepted(Request::delete("/api/auth/sessions")).header(header::AUTHORIZATION, format!("Bearer {}", current.token)).body(Body::empty()).unwrap()).await.unwrap()).await;
    assert_eq!(others.revoked, 1);
    assert_eq!(app.clone().oneshot(Request::get("/api/auth/me").header(header::AUTHORIZATION, format!("Bearer {}", third.token)).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(app.clone().oneshot(accepted(Request::get("/api/auth/me")).header(header::AUTHORIZATION, format!("Bearer {}", current.token)).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::OK);

    let signed_out: SessionRevocationResponse = response_body(
        app.clone().oneshot(accepted(Request::delete(format!("/api/auth/sessions/{}", current_session.session_id))).header(header::AUTHORIZATION, format!("Bearer {}", current.token)).body(Body::empty()).unwrap()).await.unwrap(),
    )
    .await;
    assert!(signed_out.signed_out);
    assert_eq!(app.clone().oneshot(Request::get("/api/auth/me").header(header::AUTHORIZATION, format!("Bearer {}", current.token)).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::UNAUTHORIZED);

    let configured_email = format!("configured-{unique}@example.com");
    let configured = accounts.ensure_configured_user(&configured_email, original_password).await.unwrap();
    let configured_session = accounts.login(&configured_email, original_password).await.unwrap();
    let same = accounts.ensure_configured_user(&configured_email, original_password).await.unwrap();
    assert_eq!(same.user_id, configured.user_id);
    assert_eq!(accounts.resolve_token(&configured_session.token).await.unwrap().user_id, configured.user_id);

    let quota = 1_000_i64 * 1_000 * 1_000 * 1_000;
    assert_eq!(server_postgres::configure_user_storage_quota(&database, &configured.user_id, quota).await.unwrap(), quota);
    let snapshot: AccountSnapshotResponse =
        response_body(app.clone().oneshot(accepted(Request::get("/api/account/snapshot")).header(header::AUTHORIZATION, format!("Bearer {}", configured_session.token)).body(Body::empty()).unwrap()).await.unwrap()).await;
    assert_eq!(snapshot.storage, StorageUsageResponse { used_bytes: 0, reserved_bytes: 0, quota_bytes: quota as u64 });
    assert!(snapshot.libraries.is_empty());
    assert!(snapshot.deleted_library_ids.is_empty());
    // An account holding nothing reports no library shares at all rather than a
    // row of zeroes: the client knows which libraries exist and fills the rest.
    assert!(snapshot.library_storage.is_empty());
    for obsolete in ["/api/account/storage", "/api/account/storage/libraries"] {
        let response = app
            .clone()
            .oneshot(accepted(Request::get(obsolete)).header(header::AUTHORIZATION, format!("Bearer {}", configured_session.token)).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "obsolete account endpoint must be removed: {obsolete}");
    }
    for obsolete in ["/api/libraries", "/api/libraries/deleted"] {
        let response = app
            .clone()
            .oneshot(accepted(Request::get(obsolete)).header(header::AUTHORIZATION, format!("Bearer {}", configured_session.token)).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "obsolete library read endpoint must be removed: {obsolete}");
    }

    let rotated_password = "configured-user-rotated-password";
    accounts.ensure_configured_user(&configured_email, rotated_password).await.unwrap();
    assert!(accounts.resolve_token(&configured_session.token).await.is_err());
    assert!(accounts.login(&configured_email, original_password).await.is_err());
    assert_eq!(accounts.login(&configured_email, rotated_password).await.unwrap().user.user_id, configured.user_id);
    database.pool().close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE")).execute(&admin).await.unwrap();
    admin.close().await;
}
