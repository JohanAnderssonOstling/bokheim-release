//! Bounded, process-local, read-only capabilities for native browser media.
//! Restart invalidates them; the authenticated client obtains a fresh grant.
use crate::{assets::{stream_asset, stream_audio_track}, extract::{asset_error, AuthUser}, state::AppState};
use axum::{extract::{Path, State}, http::{header, HeaderMap, Method, StatusCode}, response::Response};
use server_asset_store::AssetKind;
use std::{collections::HashMap, sync::{Arc, Mutex}, time::{Duration, Instant}};
use sync_common::{ContentHash, LibraryId, api::assets::PlaybackGrant};

const LIFETIME: Duration = Duration::from_secs(15 * 60);
const MAX_GRANTS: usize = 4096;
const MAX_PER_ACCOUNT: usize = 16;

#[derive(Clone)]
struct Grant {
    user: String,
    credential: Arc<str>,
    library: LibraryId,
    hash: ContentHash,
    checksum: ContentHash,
    length: u64,
    expires: Instant,
}

#[derive(Default)]
pub(crate) struct Grants(Mutex<HashMap<String, Grant>>);
impl Grants {
    fn insert(&self, grant: Grant, now: Instant) -> Result<PlaybackGrant, StatusCode> {
        let mut entries = self.0.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        entries.retain(|_, value| value.expires > now);
        if entries.len() >= MAX_GRANTS || entries.values().filter(|value| value.user == grant.user).count() >= MAX_PER_ACCOUNT {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        // Two random UUIDs provide 244 random bits without exposing credentials.
        let ticket = format!("{}{}", LibraryId::new_v4().simple(), LibraryId::new_v4().simple());
        let reply = PlaybackGrant { path: format!("/api/playback/{ticket}"), expires_in_seconds: LIFETIME.as_secs() as u32, checksum: grant.checksum, length: grant.length };
        entries.insert(ticket, grant);
        Ok(reply)
    }
    fn get(&self, ticket: &str, now: Instant) -> Result<Grant, StatusCode> {
        let mut entries = self.0.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        entries.retain(|_, value| value.expires > now);
        entries.get(ticket).cloned().ok_or(StatusCode::GONE)
    }
}

pub(crate) async fn issue(State(state): State<AppState>, user: AuthUser, Path((library, hash)): Path<(LibraryId, ContentHash)>, headers: HeaderMap) -> Result<Response, StatusCode> {
    // Storage rechecks account/library ownership before issuing or serving.
    let asset = state.assets.open_from(&user.user_id, &library, AssetKind::Book, &hash, 0).await.map_err(asset_error)?.ok_or(StatusCode::NOT_FOUND)?;
    let now = Instant::now();
    let reply = state.playback.insert(Grant { user: user.user_id, credential: user.credential, library, hash, checksum: asset.checksum, length: asset.total_len, expires: now + LIFETIME }, now)?;
    let mut response = server_wire_http::response(&headers, reply)?;
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(crate) async fn read(State(state): State<AppState>, Path(ticket): Path<String>, method: Method, headers: HeaderMap) -> Result<Response, StatusCode> {
    let grant = state.playback.get(&ticket, Instant::now())?;
    // Logout, revocation and library removal must also invalidate access.
    let session = state.account.resolve_token(&grant.credential).await.map_err(server_account::auth_error)?;
    if session.user_id != grant.user { return Err(StatusCode::FORBIDDEN); }
    let asset = state.assets.open_from(&grant.user, &grant.library, AssetKind::Book, &grant.hash, 0).await.map_err(asset_error)?.ok_or(StatusCode::NOT_FOUND)?;
    if asset.checksum != grant.checksum { return Err(StatusCode::PRECONDITION_FAILED); }
    let mut response = stream_asset(asset, AssetKind::Book, &headers, state.transfer_governor.clone(), grant.user).await?;
    response.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("audio/mp4"));
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("private, no-store"));
    response.headers_mut().insert(header::REFERRER_POLICY, header::HeaderValue::from_static("no-referrer"));
    response.headers_mut().insert(header::HeaderName::from_static("cross-origin-resource-policy"), header::HeaderValue::from_static("cross-origin"));
    if method == Method::HEAD { *response.body_mut() = axum::body::Body::empty(); }
    Ok(response)
}

pub(crate) async fn read_track(State(state): State<AppState>, Path((ticket, index)): Path<(String, usize)>, method: Method, headers: HeaderMap) -> Result<Response, StatusCode> {
    if index >= audiobook_folder::MAX_TRACKS { return Err(StatusCode::NOT_FOUND); }
    let grant = state.playback.get(&ticket, Instant::now())?;
    let session = state.account.resolve_token(&grant.credential).await.map_err(server_account::auth_error)?;
    if session.user_id != grant.user { return Err(StatusCode::FORBIDDEN); }
    let (asset, track) = state.assets.audiobook_track(&grant.user, &grant.library, &grant.hash, index).await.map_err(asset_error)?.ok_or(StatusCode::NOT_FOUND)?;
    if asset.checksum != grant.checksum { return Err(StatusCode::PRECONDITION_FAILED); }
    let mut response = stream_audio_track(asset, track.offset, track.length, &headers, state.transfer_governor.clone(), grant.user).await?;
    response.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("private, no-store"));
    response.headers_mut().insert(header::REFERRER_POLICY, header::HeaderValue::from_static("no-referrer"));
    response.headers_mut().insert(header::HeaderName::from_static("cross-origin-resource-policy"), header::HeaderValue::from_static("cross-origin"));
    if method == Method::HEAD { *response.body_mut() = axum::body::Body::empty(); }
    Ok(response)
}

pub(crate) async fn revoke(State(state): State<AppState>, Path(ticket): Path<String>) -> Result<StatusCode, StatusCode> {
    state.playback.0.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.remove(&ticket);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grant(now: Instant) -> Grant {
        Grant { user: "reader".into(), credential: Arc::from("never-in-url"), library: LibraryId::new_v4(), hash: "a".repeat(64).parse().unwrap(), checksum: "b".repeat(64).parse().unwrap(), length: 1024, expires: now + LIFETIME }
    }
    #[test]
    fn capabilities_are_scoped_expiring_unique_and_bounded() {
        let grants = Grants::default(); let now = Instant::now(); let original = grant(now);
        let first = grants.insert(original.clone(), now).unwrap();
        assert!(!first.path.contains("never-in-url"));
        let ticket = first.path.strip_prefix("/api/playback/").unwrap();
        let resolved = grants.get(ticket, now).unwrap();
        assert_eq!(resolved.library, original.library); assert_eq!(resolved.hash, original.hash); assert_eq!(resolved.checksum, original.checksum);
        for _ in 1..MAX_PER_ACCOUNT { assert_ne!(grants.insert(grant(now), now).unwrap().path, first.path); }
        assert_eq!(grants.insert(grant(now), now).unwrap_err(), StatusCode::TOO_MANY_REQUESTS);
        assert!(matches!(grants.get(ticket, now + LIFETIME), Err(StatusCode::GONE)));
        assert!(grants.insert(grant(now + LIFETIME), now + LIFETIME).is_ok());
        assert!(grants.get("unknown", now).is_err());
    }
}
