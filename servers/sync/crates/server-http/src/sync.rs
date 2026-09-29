use crate::extract::{sync_error, AuthUser};
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use sync_common::api::libraries::{DeleteLibraryResponse, LibraryNameRequest};
use sync_common::api::notifications::NotificationMessage;
use sync_common::LibraryId;
use sync_common::{StateInventoryRequest, SyncExchangeRequest};

pub(super) async fn create_library(State(state): State<AppState>, user: AuthUser, headers: HeaderMap, body: Bytes) -> Result<Response, axum::http::StatusCode> {
    let request: LibraryNameRequest = server_wire_http::decode_request(&headers, &body, wire::MAX_DECODED_REQUEST_BYTES)?;
    let outcome = state.sync.create_library(&user.user_id, &request).await.map_err(sync_error)?.ok_or_else(|| sync_error(server_postgres::SyncError::LibraryOwnershipConflict))?;
    if outcome.changed {
        state.notifications.publish(&user.user_id, Some(user.credential), NotificationMessage::LibrariesChanged);
    }
    server_wire_http::response(&headers, outcome.library)
}

pub(super) async fn rename_library(State(state): State<AppState>, user: AuthUser, headers: HeaderMap, body: Bytes) -> Result<Response, axum::http::StatusCode> {
    let request: LibraryNameRequest = server_wire_http::decode_request(&headers, &body, wire::MAX_DECODED_REQUEST_BYTES)?;
    let outcome = state.sync.rename_library(&user.user_id, &request).await.map_err(sync_error)?.ok_or_else(|| sync_error(server_postgres::SyncError::LibraryOwnershipConflict))?;
    if outcome.changed {
        state.notifications.publish(&user.user_id, Some(user.credential), NotificationMessage::LibrariesChanged);
    }
    server_wire_http::response(&headers, outcome.library)
}

pub(super) async fn delete_library(State(state): State<AppState>, user: AuthUser, headers: HeaderMap, Path(library_id): Path<String>) -> Result<Response, axum::http::StatusCode> {
    let library_id = LibraryId::parse_str(&library_id).map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    if state.sync.delete_library(&user.user_id, &library_id).await.map_err(sync_error)? {
        state.notifications.publish(&user.user_id, Some(user.credential), NotificationMessage::LibrariesChanged);
    }
    server_wire_http::response(&headers, DeleteLibraryResponse { library_id })
}

pub(super) async fn exchange_events(State(state): State<AppState>, user: AuthUser, headers: HeaderMap, body: Bytes) -> Result<Response, axum::http::StatusCode> {
    let started = std::time::Instant::now();
    let request: SyncExchangeRequest = server_wire_http::decode_request(&headers, &body, wire::MAX_DECODED_REQUEST_BYTES)?;
    tracing::debug!(target: "sync_performance", phase = "decode_exchange", elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, library_id = %request.library_id, push_count = request.mutations.len(), cursor = ?request.cursor);
    let library_id = request.library_id.clone();
    let started = std::time::Instant::now();
    let response = state.sync.exchange(&user.user_id, &request).await.map_err(sync_error)?;
    tracing::debug!(target: "sync_performance", phase = "database_exchange", elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, accepted = response.push.accepted.len(), rejected = response.push.rejected.len(), has_more = response.pull.has_more);
    if !response.push.accepted.is_empty() {
        state.notifications.publish(&user.user_id, Some(user.credential.clone()), NotificationMessage::LibraryChanged { library_id });
    }
    server_wire_http::response(&headers, response)
}

pub(super) async fn compare_state_inventory(State(state): State<AppState>, user: AuthUser, headers: HeaderMap, body: Bytes) -> Result<Response, axum::http::StatusCode> {
    let request: StateInventoryRequest = server_wire_http::decode_request(&headers, &body, wire::MAX_DECODED_REQUEST_BYTES)?;
    let response = state.sync.compare_state_inventory(&user.user_id, &request).await.map_err(sync_error)?;
    server_wire_http::response(&headers, response)
}

pub(super) async fn cloud_storage(State(state): State<AppState>, user: AuthUser, headers: HeaderMap) -> Result<Response, axum::http::StatusCode> {
    server_wire_http::response(&headers, state.sync.cloud_storage(&user.user_id).await.map_err(sync_error)?)
}

pub(super) async fn set_cloud_storage(State(state): State<AppState>, user: AuthUser, headers: HeaderMap, Path(id): Path<String>, body: Bytes) -> Result<Response, axum::http::StatusCode> {
    let id = LibraryId::parse_str(&id).map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    let change: sync_common::api::libraries::CloudStorageChange = server_wire_http::decode_request(&headers, &body, wire::MAX_DECODED_REQUEST_BYTES)?;
    let result = state.sync.set_cloud_storage(&user.user_id, &id, change.change_id, change.enabled).await.map_err(sync_error)?;
    state.notifications.publish(&user.user_id, None, NotificationMessage::LibrariesChanged);
    server_wire_http::response(&headers, result)
}
