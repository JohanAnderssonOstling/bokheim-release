use account_contract::{
    AccountSnapshotResponse, AuthResponse, EmailRequest, EmailVerificationRequest, LoginRequest, MeResponse, OtherSessionsRevocationResponse, PublicRegistrationRequest, RefreshRequest, ResetPasswordRequest, SessionRevocationResponse,
    SessionSummary, StorageUsageResponse,
};
use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::Router;
use server_account::{AccountEmailKind, AccountTokenKey, ClientAddressSource, PostgresAccountService};
use std::sync::Arc;
use tower::ServiceExt;

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

fn session_cookie(response: &axum::response::Response) -> String {
    response.headers().get_all(header::SET_COOKIE).iter().map(|value| value.to_str().unwrap().split(';').next().unwrap()).collect::<Vec<_>>().join("; ")
}

fn accepted(request: axum::http::request::Builder) -> axum::http::request::Builder {
    request.header(header::ACCEPT, wire::MEDIA_TYPE)
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn public_registration_verification_and_reset_are_end_to_end_safe() {
    let database_url = std::env::var("SYNC_E2E_DATABASE_URL").expect("run through Backend/tests/run_sync_e2e.sh");
    let database = server_postgres::connect(&database_url).await.unwrap();
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
    let unique = uuid::Uuid::new_v4().simple().to_string();
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
    assert_eq!(post(&app, "/api/auth/verify-email", &EmailVerificationRequest { email: "other@example.com".to_owned(), pin: verification.token.clone() }, None).await.status(), StatusCode::UNAUTHORIZED);

    // Existing account identifiers receive the same generic response and do
    // not create another pending account or leak an email.
    let verification_request = EmailVerificationRequest { email: email.clone(), pin: verification.token };
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
    let wrong_request = EmailVerificationRequest { email: locked_email.clone(), pin: wrong_pin.to_owned() };
    for attempt in 0..5 {
        let address = format!("198.51.100.{}", attempt + 1);
        assert_eq!(post_from(&app, "/api/auth/verify-email", &wrong_request, None, &address).await.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(post_from(&app, "/api/auth/verify-email", &wrong_request, None, "198.51.100.6").await.status(), StatusCode::TOO_MANY_REQUESTS);
    let locked_correct = EmailVerificationRequest { email: locked_email, pin: locked_verification.token };
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

    let web_login = app
        .clone()
        .oneshot(
            Request::post("/api/auth/web/login")
                .header(header::CONTENT_TYPE, wire::MEDIA_TYPE)
                .header(header::ACCEPT, wire::MEDIA_TYPE)
                .header("x-forwarded-for", "203.0.113.21")
                .header("sec-fetch-site", "same-origin")
                .body(Body::from(wire::encode(&valid_login).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(web_login.status(), StatusCode::OK);
    let cookies = session_cookie(&web_login);
    assert!(cookies.contains("__Host-bokheim_access="));
    assert!(cookies.contains("__Host-bokheim_refresh="));
    let web_profile: MeResponse = response_body(web_login).await;
    assert_eq!(web_profile.email, email);
    let restored = app.clone().oneshot(accepted(Request::get("/api/auth/web/me")).header(header::COOKIE, &cookies).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response_body::<MeResponse>(restored).await.email, email);
    let web_logout = app.clone().oneshot(accepted(Request::post("/api/auth/web/logout")).header(header::COOKIE, &cookies).header("sec-fetch-site", "same-origin").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(web_logout.status(), StatusCode::NO_CONTENT);
    assert_eq!(web_logout.headers().get_all(header::SET_COOKIE).iter().filter(|value| value.to_str().unwrap().contains("Max-Age=0")).count(), 2);
    assert_eq!(app.clone().oneshot(accepted(Request::get("/api/auth/web/me")).header(header::COOKIE, &cookies).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::UNAUTHORIZED);

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

    let database = server_postgres::connect(&database_url).await.unwrap();
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

    let ordinary_admin_attempt = app.clone().oneshot(Request::get("/api/admin/overview").header(header::AUTHORIZATION, format!("Bearer {}", configured_session.token)).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(ordinary_admin_attempt.status(), StatusCode::FORBIDDEN);
    let admin_login_json = serde_json::to_vec(&serde_json::json!({ "email": configured_email, "password": original_password })).unwrap();
    let ordinary_admin_login =
        app.clone().oneshot(Request::post("/api/admin/login").header(header::CONTENT_TYPE, "application/json").header("sec-fetch-site", "same-origin").body(Body::from(admin_login_json.clone())).unwrap()).await.unwrap();
    assert_eq!(ordinary_admin_login.status(), StatusCode::UNAUTHORIZED);
    accounts.ensure_configured_admin(&configured_email, original_password).await.unwrap();
    let admin_overview =
        app.clone().oneshot(Request::get("/api/admin/overview").header(header::AUTHORIZATION, format!("Bearer {}", configured_session.token)).header(header::ACCEPT, "application/json").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(admin_overview.status(), StatusCode::OK);
    assert_eq!(admin_overview.headers()[header::CACHE_CONTROL], "no-store");
    let overview: serde_json::Value = serde_json::from_slice(&to_bytes(admin_overview.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert!(overview["registered_users"].as_i64().is_some_and(|users| users >= 2));

    assert_eq!(app.clone().oneshot(Request::post("/api/admin/login").header(header::CONTENT_TYPE, "application/json").body(Body::from(admin_login_json.clone())).unwrap()).await.unwrap().status(), StatusCode::FORBIDDEN);
    let admin_web_login = app.clone().oneshot(Request::post("/api/admin/login").header(header::CONTENT_TYPE, "application/json").header("sec-fetch-site", "same-origin").body(Body::from(admin_login_json)).unwrap()).await.unwrap();
    assert_eq!(admin_web_login.status(), StatusCode::NO_CONTENT);
    let admin_cookies = session_cookie(&admin_web_login);
    assert_eq!(app.clone().oneshot(Request::get("/api/admin/traffic?hours=1").header(header::COOKIE, &admin_cookies).body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::FORBIDDEN);
    let admin_traffic =
        app.clone().oneshot(Request::get("/api/admin/traffic?hours=1").header(header::COOKIE, &admin_cookies).header("sec-fetch-site", "same-origin").header("x-forwarded-for", "203.0.113.22").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(admin_traffic.status(), StatusCode::OK);
    let traffic: serde_json::Value = serde_json::from_slice(&to_bytes(admin_traffic.into_body(), 256 * 1024).await.unwrap()).unwrap();
    assert_eq!(traffic["hours"], 1);
    assert!(traffic["points"].is_array());
    let sync_activity = app.clone().oneshot(accepted(Request::get("/api/account/snapshot")).header(header::AUTHORIZATION, format!("Bearer {}", configured_session.token)).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(sync_activity.status(), StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let configured_activity: i64 = sqlx::query_scalar("SELECT request_count FROM admin_account_activity WHERE user_id=$1").bind(&configured.user_id).fetch_one(database.pool()).await.unwrap();
    assert!(configured_activity > 0, "successful authenticated use must contribute to the unique-account count");
    let daily_activity: (i64, i64) =
        sqlx::query_as("SELECT request_count, sync_request_count FROM admin_account_activity_day WHERE user_id=$1 AND activity_date=CURRENT_DATE").bind(&configured.user_id).fetch_one(database.pool()).await.unwrap();
    assert!(daily_activity.0 > 0);
    assert!(daily_activity.1 > 0, "authenticated synchronization must be classified as sync engagement");
    let engagement = app.clone().oneshot(Request::get("/api/admin/engagement?days=30").header(header::COOKIE, &admin_cookies).header("sec-fetch-site", "same-origin").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(engagement.status(), StatusCode::OK);
    let engagement: serde_json::Value = serde_json::from_slice(&to_bytes(engagement.into_body(), 256 * 1024).await.unwrap()).unwrap();
    assert_eq!(engagement["days"], 30);
    assert_eq!(engagement["points"].as_array().map(Vec::len), Some(30));
    assert!(engagement["points"].as_array().unwrap().iter().any(|point| point["sync_accounts"].as_i64().is_some_and(|accounts| accounts > 0)));
    let registered_accounts = app.clone().oneshot(Request::get("/api/admin/accounts?limit=200").header(header::COOKIE, &admin_cookies).header("sec-fetch-site", "same-origin").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(registered_accounts.status(), StatusCode::OK);
    assert_eq!(registered_accounts.headers()[header::CACHE_CONTROL], "no-store");
    let registered_accounts: serde_json::Value = serde_json::from_slice(&to_bytes(registered_accounts.into_body(), 256 * 1024).await.unwrap()).unwrap();
    assert!(registered_accounts["total"].as_i64().is_some_and(|total| total >= 2));
    let listed = registered_accounts["accounts"].as_array().unwrap();
    let configured_entry = listed.iter().find(|account| account["email"] == configured_email).expect("a registered email address must be listed");
    assert_eq!(configured_entry["verified"], true);
    assert_eq!(configured_entry["is_admin"], true);
    assert!(configured_entry["last_seen_at_ms"].as_i64().is_some(), "authenticated use must be reflected in the account list");
    assert_eq!(app.clone().oneshot(Request::get("/api/admin/accounts?limit=0").header(header::COOKIE, &admin_cookies).header("sec-fetch-site", "same-origin").body(Body::empty()).unwrap()).await.unwrap().status(), StatusCode::BAD_REQUEST);
    let services = app.clone().oneshot(Request::get("/api/admin/services").header(header::COOKIE, &admin_cookies).header("sec-fetch-site", "same-origin").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(services.status(), StatusCode::OK);
    let services: serde_json::Value = serde_json::from_slice(&to_bytes(services.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert!(services["authority"]["available"].is_boolean());
    assert!(services["metadata"]["available"].is_boolean());
    let request_history = app
        .clone()
        .oneshot(Request::get("/api/admin/requests?hours=1&limit=1000").header(header::COOKIE, &admin_cookies).header("sec-fetch-site", "same-origin").header("x-forwarded-for", "203.0.113.22").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(request_history.status(), StatusCode::OK);
    let request_history: serde_json::Value = serde_json::from_slice(&to_bytes(request_history.into_body(), 256 * 1024).await.unwrap()).unwrap();
    assert!(request_history["points"].as_array().is_some_and(|points| points.iter().any(|point| point["client_ip"] == "203.0.113.22")));
    let audit_events: i64 = sqlx::query_scalar("SELECT count(*) FROM admin_audit_event WHERE administrator_user_id=$1").bind(&configured.user_id).fetch_one(database.pool()).await.unwrap();
    assert_eq!(audit_events, 6);
    assert!(sqlx::query("DELETE FROM admin_audit_event WHERE administrator_user_id=$1").bind(&configured.user_id).execute(database.pool()).await.is_err(), "administrator audit history must be append-only at the database boundary");

    let rotated_password = "configured-user-rotated-password";
    accounts.ensure_configured_user(&configured_email, rotated_password).await.unwrap();
    assert!(accounts.resolve_token(&configured_session.token).await.is_err());
    assert!(accounts.login(&configured_email, original_password).await.is_err());
    assert_eq!(accounts.login(&configured_email, rotated_password).await.unwrap().user.user_id, configured.user_id);
}
