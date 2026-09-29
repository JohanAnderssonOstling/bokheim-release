use crate::extract::{asset_error, sync_error, AuthUser};
use crate::state::AppState;
use account_contract::{AccountSnapshotResponse, LibraryStorageUsage, StorageUsageResponse};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;

pub(crate) async fn get_snapshot(State(state): State<AppState>, user: AuthUser, headers: HeaderMap) -> Result<Response, StatusCode> {
    let user_id = &user.user_id;
    let (libraries, deleted_library_ids, storage, library_storage) = tokio::try_join!(
        async { state.sync.list_libraries(user_id).await.map_err(sync_error) },
        async { state.sync.list_deleted_libraries(user_id).await.map_err(sync_error) },
        async { state.assets.storage_usage(user_id).await.map_err(asset_error) },
        async { state.assets.library_storage_usage(user_id).await.map_err(asset_error) },
    )?;
    let storage = StorageUsageResponse { used_bytes: storage.used_bytes, reserved_bytes: storage.reserved_bytes, quota_bytes: storage.quota_bytes };
    let library_storage = library_storage.into_iter().map(|entry| LibraryStorageUsage { library_id: entry.library_id.to_string(), used_bytes: entry.used_bytes }).collect();
    server_wire_http::response(&headers, AccountSnapshotResponse { libraries, deleted_library_ids, storage, library_storage })
}
