use crate::state::AppState;
use axum::async_trait;
use axum::body::Body;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{header, request::Parts, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const SEC_FETCH_SITE: &str = "sec-fetch-site";
const DEFAULT_HISTORY_HOURS: u16 = 24;
const MAX_HISTORY_HOURS: u16 = 24 * 30;
const DEFAULT_REQUEST_LOG_LIMIT: u16 = 200;
const MAX_REQUEST_LOG_LIMIT: u16 = 1_000;
const MAX_REQUEST_LOG_HOURS: u16 = 24 * 7;
const DEFAULT_ENGAGEMENT_DAYS: u16 = 30;
const MAX_ENGAGEMENT_DAYS: u16 = 90;
const DEFAULT_ACCOUNT_LIMIT: u16 = 200;
const MAX_ACCOUNT_LIMIT: u16 = 1_000;

#[derive(Deserialize)]
pub(crate) struct AdminLogin {
    email: String,
    password: String,
}

pub(crate) struct AdminUser {
    user_id: String,
}

#[async_trait]
impl FromRequestParts<AppState> for AdminUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let bearer = parts.headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Bearer ")).filter(|token| !token.is_empty());
        let token = match bearer {
            Some(token) => token,
            None => {
                if parts.headers.get(SEC_FETCH_SITE).and_then(|value| value.to_str().ok()) != Some("same-origin") {
                    return Err(StatusCode::FORBIDDEN);
                }
                server_account::http::web_access_token(&parts.headers).ok_or(StatusCode::UNAUTHORIZED)?
            }
        };
        let user = state.account.resolve_admin_token(token).await.map_err(server_account::auth_error)?;
        state.account.enforce_rate_limit(server_account::AuthAttempt::AdminRead, "administrator-dashboard", &user.user_id).await.map_err(server_account::auth_error)?;
        Ok(Self { user_id: user.user_id })
    }
}

#[derive(Serialize)]
pub(crate) struct AdminOverview {
    generated_at_ms: i64,
    uptime_seconds: u64,
    in_flight_requests: usize,
    peak_in_flight_requests: usize,
    dropped_traffic_events: u64,
    active_websockets: usize,
    database_connections: u32,
    database_idle_connections: usize,
    registered_users: i64,
    unique_accounts_24h: i64,
    unique_accounts_7d: i64,
    unique_accounts_30d: i64,
    unique_accounts_ever: i64,
    daily_active_accounts: i64,
    weekly_active_accounts: i64,
    monthly_active_accounts: i64,
    new_verified_accounts_30d: i64,
    first_time_accounts_30d: i64,
    returning_accounts_30d: i64,
    sync_accounts_30d: i64,
    auth_only_accounts_30d: i64,
    active_sessions: i64,
    libraries: i64,
    storage_used_bytes: i64,
    storage_reserved_bytes: i64,
    storage_quota_bytes: i64,
    requests_last_hour: i64,
    errors_last_hour: i64,
    request_bytes_last_hour: i64,
    response_bytes_last_hour: i64,
    average_latency_ms_last_hour: f64,
    max_latency_ms_last_hour: i64,
}

#[derive(Deserialize)]
pub(crate) struct TrafficQuery {
    hours: Option<u16>,
}

#[derive(Serialize)]
pub(crate) struct TrafficHistory {
    generated_at_ms: i64,
    hours: u16,
    points: Vec<TrafficPoint>,
}

#[derive(Serialize)]
struct TrafficPoint {
    bucket_ms: i64,
    route_group: String,
    method: String,
    status_class: i16,
    request_count: i64,
    request_bytes: i64,
    response_bytes: i64,
    average_duration_ms: f64,
    max_duration_ms: i64,
}

#[derive(Deserialize)]
pub(crate) struct RequestLogQuery {
    hours: Option<u16>,
    limit: Option<u16>,
}

#[derive(Serialize)]
pub(crate) struct RequestHistory {
    generated_at_ms: i64,
    hours: u16,
    points: Vec<RequestPoint>,
}

#[derive(Serialize)]
struct RequestPoint {
    occurred_at_ms: i64,
    client_ip: String,
    route_group: String,
    method: String,
    status_class: i16,
    request_bytes: i64,
    response_bytes: i64,
    duration_ms: i64,
}

#[derive(Deserialize)]
pub(crate) struct EngagementQuery {
    days: Option<u16>,
}

#[derive(Deserialize)]
pub(crate) struct AuthorityIdentityReviewQuery {
    source_id: Option<String>,
    status: Option<String>,
    limit: Option<u16>,
}

#[derive(Serialize)]
pub(crate) struct EngagementHistory {
    generated_at_ms: i64,
    days: u16,
    points: Vec<EngagementPoint>,
}

#[derive(Serialize)]
struct EngagementPoint {
    date: String,
    active_accounts: i64,
    sync_accounts: i64,
    new_verified_accounts: i64,
}

#[derive(Deserialize)]
pub(crate) struct AccountQuery {
    limit: Option<u16>,
}

#[derive(Serialize)]
pub(crate) struct AccountList {
    generated_at_ms: i64,
    total: i64,
    accounts: Vec<AccountEntry>,
}

#[derive(Serialize)]
struct AccountEntry {
    email: String,
    created_at_ms: i64,
    verified: bool,
    is_admin: bool,
    last_seen_at_ms: Option<i64>,
    request_count: i64,
    libraries: i64,
}

pub(crate) async fn overview(State(state): State<AppState>, admin: AdminUser) -> Result<Response, StatusCode> {
    audit(&state, &admin.user_id, "dashboard_overview_viewed").await?;
    let pool = state.database.pool();
    let registered_users = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users").fetch_one(pool);
    let active_sessions = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM auth_session WHERE refresh_expires_at > now()").fetch_one(pool);
    let libraries = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM libraries WHERE deleted_at IS NULL").fetch_one(pool);
    let account_activity = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "SELECT
            count(*) FILTER (WHERE last_seen_at >= now() - INTERVAL '24 hours')::bigint,
            count(*) FILTER (WHERE last_seen_at >= now() - INTERVAL '7 days')::bigint,
            count(*) FILTER (WHERE last_seen_at >= now() - INTERVAL '30 days')::bigint,
            count(*)::bigint
         FROM admin_account_activity",
    )
    .fetch_one(pool);
    let engagement = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64)>(
        "SELECT
            count(DISTINCT user_id) FILTER (WHERE activity_date = CURRENT_DATE)::bigint,
            count(DISTINCT user_id) FILTER (WHERE activity_date >= CURRENT_DATE - 6)::bigint,
            count(DISTINCT user_id) FILTER (WHERE activity_date >= CURRENT_DATE - 29)::bigint,
            count(DISTINCT user_id) FILTER (WHERE activity_date >= CURRENT_DATE - 29 AND sync_request_count > 0)::bigint,
            (SELECT count(*) FROM users WHERE created_at >= now() - INTERVAL '30 days' AND email IS NOT NULL AND password_hash IS NOT NULL)::bigint,
            (SELECT count(*) FROM admin_account_activity WHERE first_seen_at >= now() - INTERVAL '30 days')::bigint,
            (SELECT count(*) FROM admin_account_activity WHERE first_seen_at < now() - INTERVAL '30 days' AND last_seen_at >= now() - INTERVAL '30 days')::bigint
         FROM admin_account_activity_day",
    )
    .fetch_one(pool);
    let storage = sqlx::query_as::<_, (i64, i64, i64)>("SELECT COALESCE(sum(used_bytes),0)::bigint, COALESCE(sum(reserved_bytes),0)::bigint, COALESCE(sum(quota_bytes),0)::bigint FROM user_storage_account").fetch_one(pool);
    let traffic = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64)>(
        "SELECT COALESCE(sum(request_count),0)::bigint,
                COALESCE(sum(CASE WHEN status_class >= 4 THEN request_count ELSE 0 END),0)::bigint,
                COALESCE(sum(request_bytes),0)::bigint,
                COALESCE(sum(response_bytes),0)::bigint,
                COALESCE(sum(request_count) FILTER (WHERE route_group != '/api/admin'),0)::bigint,
                COALESCE(sum(total_duration_ms) FILTER (WHERE route_group != '/api/admin'),0)::bigint,
                COALESCE(max(max_duration_ms) FILTER (WHERE route_group != '/api/admin'),0)::bigint
         FROM admin_traffic_minute WHERE bucket >= now() - INTERVAL '1 hour'",
    )
    .fetch_one(pool);
    let (registered_users, active_sessions, libraries, account_activity, engagement, storage, traffic) = tokio::try_join!(registered_users, active_sessions, libraries, account_activity, engagement, storage, traffic).map_err(internal)?;
    let live = state.traffic.snapshot();
    let average_latency = if traffic.4 == 0 { 0.0 } else { traffic.5 as f64 / traffic.4 as f64 };
    json_response(AdminOverview {
        generated_at_ms: Utc::now().timestamp_millis(),
        uptime_seconds: live.uptime_seconds,
        in_flight_requests: live.in_flight,
        peak_in_flight_requests: live.peak_in_flight,
        dropped_traffic_events: live.dropped_events,
        active_websockets: state.notifications.active_connections(),
        database_connections: pool.size(),
        database_idle_connections: pool.num_idle(),
        registered_users,
        unique_accounts_24h: account_activity.0,
        unique_accounts_7d: account_activity.1,
        unique_accounts_30d: account_activity.2,
        unique_accounts_ever: account_activity.3,
        daily_active_accounts: engagement.0,
        weekly_active_accounts: engagement.1,
        monthly_active_accounts: engagement.2,
        new_verified_accounts_30d: engagement.4,
        first_time_accounts_30d: engagement.5,
        returning_accounts_30d: engagement.6,
        sync_accounts_30d: engagement.3,
        auth_only_accounts_30d: engagement.2.saturating_sub(engagement.3),
        active_sessions,
        libraries,
        storage_used_bytes: storage.0,
        storage_reserved_bytes: storage.1,
        storage_quota_bytes: storage.2,
        requests_last_hour: traffic.0,
        errors_last_hour: traffic.1,
        request_bytes_last_hour: traffic.2,
        response_bytes_last_hour: traffic.3,
        average_latency_ms_last_hour: average_latency,
        max_latency_ms_last_hour: traffic.6,
    })
}

pub(crate) async fn login(State(state): State<AppState>, headers: HeaderMap, Json(request): Json<AdminLogin>) -> Result<Response, StatusCode> {
    server_account::http::require_same_origin_web_request(&Method::POST, &headers)?;
    state.account.enforce_rate_limit(server_account::AuthAttempt::Login, "administrator-dashboard-login", &request.email).await.map_err(server_account::auth_error)?;
    let issued = state.account.login_admin(&request.email, &request.password).await.map_err(server_account::auth_error)?;
    state.account.clear_subject_rate_limit(server_account::AuthAttempt::Login, &request.email).await.map_err(server_account::auth_error)?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    server_account::http::append_web_session_cookies(&mut response, &issued)?;
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

pub(crate) async fn traffic(State(state): State<AppState>, admin: AdminUser, Query(query): Query<TrafficQuery>) -> Result<Response, StatusCode> {
    audit(&state, &admin.user_id, "dashboard_traffic_viewed").await?;
    let hours = query.hours.unwrap_or(DEFAULT_HISTORY_HOURS);
    if !(1..=MAX_HISTORY_HOURS).contains(&hours) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let rows: Vec<(DateTime<Utc>, String, String, i16, i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT bucket, route_group, method, status_class, request_count, request_bytes, response_bytes, total_duration_ms, max_duration_ms
         FROM admin_traffic_minute
         WHERE bucket >= now() - ($1 * INTERVAL '1 hour')
         ORDER BY bucket, route_group, method, status_class",
    )
    .bind(i64::from(hours))
    .fetch_all(state.database.pool())
    .await
    .map_err(internal)?;
    let points = rows
        .into_iter()
        .map(|row| TrafficPoint {
            bucket_ms: row.0.timestamp_millis(),
            route_group: row.1,
            method: row.2,
            status_class: row.3,
            request_count: row.4,
            request_bytes: row.5,
            response_bytes: row.6,
            average_duration_ms: if row.4 == 0 { 0.0 } else { row.7 as f64 / row.4 as f64 },
            max_duration_ms: row.8,
        })
        .collect();
    json_response(TrafficHistory { generated_at_ms: Utc::now().timestamp_millis(), hours, points })
}

pub(crate) async fn requests(State(state): State<AppState>, admin: AdminUser, Query(query): Query<RequestLogQuery>) -> Result<Response, StatusCode> {
    audit(&state, &admin.user_id, "dashboard_request_ips_viewed").await?;
    let hours = query.hours.unwrap_or(DEFAULT_HISTORY_HOURS);
    let limit = query.limit.unwrap_or(DEFAULT_REQUEST_LOG_LIMIT);
    if !(1..=MAX_REQUEST_LOG_HOURS).contains(&hours) || !(1..=MAX_REQUEST_LOG_LIMIT).contains(&limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let rows: Vec<(DateTime<Utc>, String, String, String, i16, i64, i64, i64)> = sqlx::query_as(
        "SELECT occurred_at, host(client_ip), route_group, method, status_class, request_bytes, response_bytes, duration_ms
         FROM admin_request_ip
         WHERE occurred_at >= now() - ($1 * INTERVAL '1 hour')
         ORDER BY occurred_at DESC
         LIMIT $2",
    )
    .bind(i64::from(hours))
    .bind(i64::from(limit))
    .fetch_all(state.database.pool())
    .await
    .map_err(internal)?;
    let points = rows
        .into_iter()
        .map(|row| RequestPoint { occurred_at_ms: row.0.timestamp_millis(), client_ip: row.1, route_group: row.2, method: row.3, status_class: row.4, request_bytes: row.5, response_bytes: row.6, duration_ms: row.7 })
        .collect();
    json_response(RequestHistory { generated_at_ms: Utc::now().timestamp_millis(), hours, points })
}

pub(crate) async fn engagement(State(state): State<AppState>, admin: AdminUser, Query(query): Query<EngagementQuery>) -> Result<Response, StatusCode> {
    audit(&state, &admin.user_id, "dashboard_engagement_viewed").await?;
    let days = query.days.unwrap_or(DEFAULT_ENGAGEMENT_DAYS);
    if !(1..=MAX_ENGAGEMENT_DAYS).contains(&days) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "WITH requested_days AS (
             SELECT generate_series(CURRENT_DATE - ($1::integer - 1), CURRENT_DATE, INTERVAL '1 day')::date AS day
         ), daily_activity AS (
             SELECT activity_date,
                    count(DISTINCT user_id)::bigint AS active_accounts,
                    count(DISTINCT user_id) FILTER (WHERE sync_request_count > 0)::bigint AS sync_accounts
             FROM admin_account_activity_day
             WHERE activity_date >= CURRENT_DATE - ($1::integer - 1)
             GROUP BY activity_date
         ), daily_accounts AS (
             SELECT created_at::date AS day, count(*)::bigint AS new_verified_accounts
             FROM users
             WHERE created_at >= CURRENT_DATE - ($1::integer - 1)
               AND email IS NOT NULL AND password_hash IS NOT NULL
             GROUP BY created_at::date
         )
         SELECT requested_days.day::text,
                COALESCE(daily_activity.active_accounts, 0)::bigint,
                COALESCE(daily_activity.sync_accounts, 0)::bigint,
                COALESCE(daily_accounts.new_verified_accounts, 0)::bigint
         FROM requested_days
         LEFT JOIN daily_activity ON daily_activity.activity_date = requested_days.day
         LEFT JOIN daily_accounts ON daily_accounts.day = requested_days.day
         ORDER BY requested_days.day",
    )
    .bind(i32::from(days))
    .fetch_all(state.database.pool())
    .await
    .map_err(internal)?;
    let points = rows.into_iter().map(|row| EngagementPoint { date: row.0, active_accounts: row.1, sync_accounts: row.2, new_verified_accounts: row.3 }).collect();
    json_response(EngagementHistory { generated_at_ms: Utc::now().timestamp_millis(), days, points })
}

/// Unlike every other dashboard query this one returns account identities, so
/// each read is audited and the newest registrations come first.
pub(crate) async fn accounts(State(state): State<AppState>, admin: AdminUser, Query(query): Query<AccountQuery>) -> Result<Response, StatusCode> {
    audit(&state, &admin.user_id, "dashboard_account_emails_viewed").await?;
    let limit = query.limit.unwrap_or(DEFAULT_ACCOUNT_LIMIT);
    if !(1..=MAX_ACCOUNT_LIMIT).contains(&limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let pool = state.database.pool();
    let total = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE email IS NOT NULL").fetch_one(pool);
    let rows = sqlx::query_as::<_, (String, DateTime<Utc>, bool, bool, Option<DateTime<Utc>>, i64, i64)>(
        "SELECT users.email,
                users.created_at,
                users.password_hash IS NOT NULL,
                users.is_admin,
                admin_account_activity.last_seen_at,
                COALESCE(admin_account_activity.request_count, 0)::bigint,
                (SELECT count(*) FROM libraries WHERE libraries.user_id = users.id AND libraries.deleted_at IS NULL)::bigint
         FROM users
         LEFT JOIN admin_account_activity ON admin_account_activity.user_id = users.id
         WHERE users.email IS NOT NULL
         ORDER BY users.created_at DESC
         LIMIT $1",
    )
    .bind(i64::from(limit))
    .fetch_all(pool);
    let (total, rows) = tokio::try_join!(total, rows).map_err(internal)?;
    let accounts = rows
        .into_iter()
        .map(|row| AccountEntry { email: row.0, created_at_ms: row.1.timestamp_millis(), verified: row.2, is_admin: row.3, last_seen_at_ms: row.4.map(|seen| seen.timestamp_millis()), request_count: row.5, libraries: row.6 })
        .collect();
    json_response(AccountList { generated_at_ms: Utc::now().timestamp_millis(), total, accounts })
}

pub(crate) async fn services(State(state): State<AppState>, admin: AdminUser) -> Result<Response, StatusCode> {
    audit(&state, &admin.user_id, "dashboard_services_viewed").await?;
    json_response(state.operational_services.snapshot().await)
}

pub(crate) async fn authority_process_action(State(state): State<AppState>, admin: AdminUser, Path(process): Path<String>, Json(action): Json<serde_json::Value>) -> Result<Response, StatusCode> {
    if !crate::operations::is_supported_authority_process(&process)
        || !action.get("action").and_then(serde_json::Value::as_str).is_some_and(|action| matches!(action, "pause" | "resume" | "run_now"))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    audit(&state, &admin.user_id, "dashboard_authority_process_changed").await?;
    let response = state.operational_services.authority_process_action(&process, &action).await.map_err(remote_admin_error)?;
    json_response(response)
}

pub(crate) async fn authority_identity_reviews(State(state): State<AppState>, admin: AdminUser, Query(query): Query<AuthorityIdentityReviewQuery>) -> Result<Response, StatusCode> {
    audit(&state, &admin.user_id, "dashboard_authority_identity_reviews_viewed").await?;
    let limit = query.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let response = state.operational_services.authority_identity_reviews(query.source_id.as_deref(), query.status.as_deref(), limit).await.map_err(remote_admin_error)?;
    json_response(response)
}

pub(crate) async fn authority_identity_review_action(State(state): State<AppState>, admin: AdminUser, Path(review_id): Path<String>, Json(action): Json<serde_json::Value>) -> Result<Response, StatusCode> {
    if review_id.len() != 64 || !review_id.bytes().all(|byte| byte.is_ascii_hexdigit()) || !action.is_object() {
        return Err(StatusCode::BAD_REQUEST);
    }
    audit(&state, &admin.user_id, "dashboard_authority_identity_review_changed").await?;
    let response = state.operational_services.authority_identity_review_action(&review_id, &action).await.map_err(remote_admin_error)?;
    json_response(response)
}

async fn audit(state: &AppState, administrator_user_id: &str, event_type: &'static str) -> Result<(), StatusCode> {
    sqlx::query(
        "INSERT INTO admin_audit_event(administrator_user_id, event_type, window_started_at)
         VALUES ($1,$2,date_trunc('hour', now()))
         ON CONFLICT (administrator_user_id, event_type, window_started_at) DO NOTHING",
    )
    .bind(administrator_user_id)
    .bind(event_type)
    .execute(state.database.pool())
    .await
    .map_err(internal)?;
    Ok(())
}

fn json_response(value: impl Serialize) -> Result<Response, StatusCode> {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

fn internal(error: sqlx::Error) -> StatusCode {
    tracing::error!(%error, "administrator dashboard query failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn remote_admin_error(error: String) -> StatusCode {
    tracing::warn!(%error, "administrator authority review request failed");
    StatusCode::BAD_GATEWAY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traffic_history_window_is_bounded() {
        assert!((1..=MAX_HISTORY_HOURS).contains(&1));
        assert!((1..=MAX_HISTORY_HOURS).contains(&720));
        assert!(!(1..=MAX_HISTORY_HOURS).contains(&0));
        assert!(!(1..=MAX_HISTORY_HOURS).contains(&721));
        assert!((1..=MAX_REQUEST_LOG_HOURS).contains(&168));
        assert!(!(1..=MAX_REQUEST_LOG_HOURS).contains(&169));
        assert!((1..=MAX_REQUEST_LOG_LIMIT).contains(&1_000));
        assert!(!(1..=MAX_REQUEST_LOG_LIMIT).contains(&1_001));
        assert!((1..=MAX_ENGAGEMENT_DAYS).contains(&90));
        assert!(!(1..=MAX_ENGAGEMENT_DAYS).contains(&0));
        assert!(!(1..=MAX_ENGAGEMENT_DAYS).contains(&91));
        assert!((1..=MAX_ACCOUNT_LIMIT).contains(&1_000));
        assert!(!(1..=MAX_ACCOUNT_LIMIT).contains(&0));
        assert!(!(1..=MAX_ACCOUNT_LIMIT).contains(&1_001));
    }
}
