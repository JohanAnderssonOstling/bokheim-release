use std::io::SeekFrom;
use std::sync::atomic::Ordering;
use crate::bandwidth::TransferGovernor;
use crate::extract::{asset_error, AuthUser};
use crate::state::AppState;
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use futures_util::{StreamExt, TryStreamExt};
use server_asset_store::AssetKind;
use std::collections::HashSet;
use sync_common::api::assets::{
    BlobManifestRequest, ThumbnailBatchDownloadEntry, ThumbnailBatchDownloadRequest, ThumbnailBatchDownloadResponse, ThumbnailBatchEntry, ThumbnailBatchUploadRequest, MAX_BLOB_MANIFEST_ENTRIES, MAX_THUMBNAIL_BATCH_BYTES, MAX_THUMBNAIL_BATCH_ENTRIES,
};
use sync_common::{BookId, LibraryId};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::{ReaderStream, StreamReader};

const STREAM_CHUNK_BYTES: usize = 64 * 1024;
const THUMBNAIL_BATCH_READ_CONCURRENCY: usize = 8;

mod ranges;
use ranges::{parse_range, response_headers};
pub(crate) use ranges::stream_asset;

fn required_content_length(headers: &HeaderMap) -> Result<u64, StatusCode> {
    headers.get(header::CONTENT_LENGTH).ok_or(StatusCode::LENGTH_REQUIRED)?.to_str().ok().and_then(|value| value.parse::<u64>().ok()).ok_or(StatusCode::BAD_REQUEST)
}



async fn head(state: &AppState, user_id: &str, library_id: &LibraryId, kind: AssetKind, hash: &BookId) -> Result<Response, StatusCode> {
    let Some(asset) = state.assets.open_from(user_id, library_id, kind, hash, 0).await.map_err(asset_error)? else {
        return Err(StatusCode::NOT_FOUND);
    };
    let mut response = Response::new(Body::empty());
    *response.headers_mut() = response_headers(kind, &asset.content_hash);
    response.headers_mut().insert(header::CONTENT_LENGTH, asset.total_len.into());
    Ok(response)
}

async fn get(state: &AppState, user_id: &str, library_id: &LibraryId, kind: AssetKind, hash: &BookId, request_headers: &HeaderMap) -> Result<Response, StatusCode> {
    let Some(asset) = state.assets.open_from(user_id, library_id, kind, hash, 0).await.map_err(asset_error)? else {
        return Err(StatusCode::NOT_FOUND);
    };
    stream_asset(asset, kind, request_headers, state.transfer_governor.clone(), user_id.to_owned()).await
}

pub(crate) async fn stream_audio_track(mut asset: server_asset_store::StoredAsset, base: u64, length: u64, request_headers: &HeaderMap, governor: TransferGovernor, user_id: String) -> Result<Response, StatusCode> {
    if length == 0 || base.checked_add(length).is_none_or(|end| end > asset.total_len) {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    let etag = format!("\"{}-{base}-{length}\"", asset.content_hash);
    let if_range_matches = request_headers.get(header::IF_RANGE).is_none_or(|value| value.to_str().ok() == Some(etag.as_str()));
    let requested = if if_range_matches { request_headers.get(header::RANGE).map(|value| parse_range(value, length)).transpose()? } else { None };
    let start = requested.map(|range| range.start).unwrap_or(0);
    let end = requested.and_then(|range| range.end).unwrap_or(length - 1).min(length - 1);
    if start >= length || end < start {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    let count = end - start + 1;
    asset.reader.seek(SeekFrom::Start(base + start)).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let stream = ReaderStream::with_capacity(asset.reader.take(count), STREAM_CHUNK_BYTES).and_then(move |chunk| {
        let governor = governor.clone();
        let user_id = user_id.clone();
        async move {
            governor.download(&user_id, chunk.len()).await?;
            Ok(chunk)
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = if requested.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("audio/mpeg"));
    headers.insert(header::ACCEPT_RANGES, header::HeaderValue::from_static("bytes"));
    headers.insert(header::ETAG, header::HeaderValue::from_str(&etag).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?);
    headers.insert(header::CONTENT_LENGTH, count.into());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    if requested.is_some() {
        headers.insert(header::CONTENT_RANGE, header::HeaderValue::from_str(&format!("bytes {start}-{end}/{length}")).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?);
    }
    Ok(response)
}

async fn put(state: &AppState, user_id: &str, library_id: &LibraryId, kind: AssetKind, hash: &BookId, headers: &HeaderMap, body: Body) -> Result<StatusCode, StatusCode> {
    let content_length = required_content_length(headers)?;
    if let Some(limit) = kind.max_bytes() {
        let len = content_length;
        if len > limit {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
    }
    let governor = state.transfer_governor.clone();
    let throttle_ns = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let measured_throttle = throttle_ns.clone();
    let measure = tracing::enabled!(target: "sync_performance", tracing::Level::DEBUG);
    let throttled_user = user_id.to_string();
    let stream = body.into_data_stream().map_err(|error| std::io::Error::other(error.to_string())).and_then(move |chunk| {
        let governor = governor.clone();
        let user_id = throttled_user.clone();
        let measured_throttle = measured_throttle.clone();
        async move {
            let started = measure.then(std::time::Instant::now);
            governor.upload(&user_id, chunk.len()).await?;
            if let Some(started) = started {
                measured_throttle.fetch_add(started.elapsed().as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
            }
            Ok(chunk)
        }
    });
    let reader = StreamReader::new(stream.boxed());
    if kind == AssetKind::Book {
        state.assets.put_book(user_id, library_id, hash, content_length, Box::new(reader)).await.map_err(asset_error)?;
    } else {
        state.assets.put(user_id, library_id, kind, hash, Some(content_length), Box::new(reader)).await.map_err(asset_error)?;
    }
    tracing::debug!(target: "sync_performance", phase = "upload_throttle", bytes = content_length, elapsed_ms = throttle_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0);
    Ok(StatusCode::OK)
}

pub(super) async fn head_blob(State(state): State<AppState>, user: AuthUser, Path((library_id, hash)): Path<(LibraryId, BookId)>) -> Result<Response, StatusCode> {
    head(&state, &user.user_id, &library_id, AssetKind::Book, &hash).await
}

pub(super) async fn get_blob(State(state): State<AppState>, user: AuthUser, Path((library_id, hash)): Path<(LibraryId, BookId)>, headers: HeaderMap) -> Result<Response, StatusCode> {
    get(&state, &user.user_id, &library_id, AssetKind::Book, &hash, &headers).await
}

pub(super) async fn put_blob(State(state): State<AppState>, user: AuthUser, Path((library_id, hash)): Path<(LibraryId, BookId)>, headers: HeaderMap, body: Body) -> Result<StatusCode, StatusCode> {
    put(&state, &user.user_id, &library_id, AssetKind::Book, &hash, &headers, body).await
}

pub(super) async fn post_blob_manifest(State(state): State<AppState>, user: AuthUser, Path(library_id): Path<LibraryId>, headers: HeaderMap, body: Bytes) -> Result<Response, StatusCode> {
    let request: BlobManifestRequest = server_wire_http::decode_request(&headers, &body, wire::MAX_DECODED_REQUEST_BYTES)?;
    if request.blobs.len() > MAX_BLOB_MANIFEST_ENTRIES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let response = state.assets.negotiate_books(&user.user_id, &library_id, request.blobs).await.map_err(asset_error)?;
    server_wire_http::response(&headers, response)
}

pub(super) async fn head_thumbnail(State(state): State<AppState>, user: AuthUser, Path((library_id, hash)): Path<(LibraryId, BookId)>) -> Result<Response, StatusCode> {
    head(&state, &user.user_id, &library_id, AssetKind::Thumbnail, &hash).await
}

pub(super) async fn get_thumbnail(State(state): State<AppState>, user: AuthUser, Path((library_id, hash)): Path<(LibraryId, BookId)>, headers: HeaderMap) -> Result<Response, StatusCode> {
    get(&state, &user.user_id, &library_id, AssetKind::Thumbnail, &hash, &headers).await
}

pub(super) async fn put_thumbnail(State(state): State<AppState>, user: AuthUser, Path((library_id, hash)): Path<(LibraryId, BookId)>, headers: HeaderMap, body: Body) -> Result<StatusCode, StatusCode> {
    let content_length = required_content_length(&headers)?;
    let limit = 2 * sync_common::MAX_THUMBNAIL_BYTES + 64 * 1024;
    if content_length > limit { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
    let _permit = state.thumbnail_uploads.acquire(&user.user_id, content_length).ok_or(StatusCode::TOO_MANY_REQUESTS)?;
    let upload = async {
        let body = to_bytes(body, limit as usize).await.map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
        if body.len() as u64 != content_length { return Err(StatusCode::BAD_REQUEST); }
        let pair: ThumbnailBatchEntry = server_wire_http::decode_request(&headers, &body, limit as usize)?;
        if pair.content_hash != hash { return Err(StatusCode::BAD_REQUEST); }
        state.assets.put_thumbnail_pair(&user.user_id, &library_id, &hash, &pair.bytes, &pair.browse_bytes).await.map_err(asset_error)?;
        Ok(StatusCode::OK)
    };
    tokio::time::timeout(server_asset_store::MAX_THUMBNAIL_UPLOAD_DURATION, upload).await.map_err(|_| StatusCode::REQUEST_TIMEOUT)?
}

pub(super) async fn delete_thumbnail(State(state): State<AppState>, user: AuthUser, Path((library_id, hash)): Path<(LibraryId, BookId)>) -> Result<StatusCode, StatusCode> {
    state.assets.remove_thumbnail(&user.user_id, &library_id, &hash).await.map_err(asset_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn put_thumbnail_batch_inner(state: &AppState, user_id: &str, library_id: &LibraryId, headers: HeaderMap, body: Body, content_length: u64) -> Result<StatusCode, StatusCode> {
    let body = to_bytes(body, MAX_THUMBNAIL_BATCH_BYTES + 1024 * 1024).await.map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    if body.len() as u64 != content_length {
        return Err(StatusCode::BAD_REQUEST);
    }
    let request: ThumbnailBatchUploadRequest = server_wire_http::decode_request(&headers, &body, MAX_THUMBNAIL_BATCH_BYTES + 1024 * 1024)?;
    if request.thumbnails.is_empty() || request.thumbnails.len() > MAX_THUMBNAIL_BATCH_ENTRIES {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut seen = HashSet::with_capacity(request.thumbnails.len());
    let mut total = 0_usize;
    for thumbnail in request.thumbnails {
        if !seen.insert(thumbnail.content_hash.clone()) || thumbnail.bytes.len() > sync_common::MAX_THUMBNAIL_BYTES as usize || thumbnail.browse_bytes.len() > sync_common::MAX_THUMBNAIL_BYTES as usize || thumbnail.bytes.is_empty() || thumbnail.browse_bytes.is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        total = total.checked_add(thumbnail.bytes.len() + thumbnail.browse_bytes.len()).ok_or(StatusCode::PAYLOAD_TOO_LARGE)?;
        if total > MAX_THUMBNAIL_BATCH_BYTES {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        state.assets.put_thumbnail_pair(user_id, library_id, &thumbnail.content_hash, &thumbnail.bytes, &thumbnail.browse_bytes).await.map_err(asset_error)?;
    }
    Ok(StatusCode::OK)
}

pub(super) async fn put_thumbnail_batch(State(state): State<AppState>, user: AuthUser, Path(library_id): Path<LibraryId>, headers: HeaderMap, body: Body) -> Result<StatusCode, StatusCode> {
    let content_length = required_content_length(&headers)?;
    if content_length > (MAX_THUMBNAIL_BATCH_BYTES + 1024 * 1024) as u64 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let _permit = state.thumbnail_uploads.acquire(&user.user_id, content_length).ok_or(StatusCode::TOO_MANY_REQUESTS)?;
    match tokio::time::timeout(server_asset_store::MAX_THUMBNAIL_UPLOAD_DURATION, put_thumbnail_batch_inner(&state, &user.user_id, &library_id, headers, body, content_length)).await {
        Ok(result) => result,
        Err(_) => Err(StatusCode::REQUEST_TIMEOUT),
    }
}

pub(super) async fn post_thumbnail_batch(State(state): State<AppState>, user: AuthUser, Path(library_id): Path<LibraryId>, headers: HeaderMap, body: Bytes) -> Result<Response, StatusCode> {
    let request: ThumbnailBatchDownloadRequest = server_wire_http::decode_request(&headers, &body, wire::MAX_DECODED_REQUEST_BYTES)?;
    if request.content_hashes.is_empty() || request.content_hashes.len() > MAX_THUMBNAIL_BATCH_ENTRIES {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut seen = HashSet::with_capacity(request.content_hashes.len());
    if request.content_hashes.iter().any(|hash| !seen.insert(*hash)) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let assets = &state.assets;
    let user_id = &user.user_id;
    let loaded = futures_util::stream::iter(request.content_hashes.into_iter().map(|hash| async move {
        let pair = assets.thumbnail_pair(user_id, &library_id, &hash).await.map_err(asset_error)?;
        Ok::<_, StatusCode>((hash, pair))
    }))
    .buffer_unordered(THUMBNAIL_BATCH_READ_CONCURRENCY)
    .try_collect::<Vec<_>>()
    .await?;

    let mut response = ThumbnailBatchDownloadResponse { thumbnails: Vec::new(), missing: Vec::new() };
    let mut total = 0_usize;
    for (hash, bytes) in loaded {
        match bytes {
            Some((bytes, browse_bytes)) => {
                total = total.checked_add(bytes.len() + browse_bytes.len()).ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
                if total > MAX_THUMBNAIL_BATCH_BYTES {
                    return Err(StatusCode::INTERNAL_SERVER_ERROR);
                }
                response.thumbnails.push(ThumbnailBatchDownloadEntry { content_hash: hash, bytes, browse_bytes });
            }
            None => response.missing.push(hash),
        }
    }
    server_wire_http::response(&headers, response)
}

pub(super) async fn post_thumbnail_presence(State(state): State<AppState>, user: AuthUser, Path(library_id): Path<LibraryId>, headers: HeaderMap, body: Bytes) -> Result<Response, StatusCode> {
    let request: sync_common::api::assets::ThumbnailPresenceRequest = server_wire_http::decode_request(&headers, &body, wire::MAX_DECODED_REQUEST_BYTES)?;
    if request.content_hashes.is_empty() || request.content_hashes.len() > sync_common::api::assets::MAX_THUMBNAIL_PRESENCE_ENTRIES {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut seen = HashSet::with_capacity(request.content_hashes.len());
    if request.content_hashes.iter().any(|hash| !seen.insert(*hash)) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let revisions = state.assets.thumbnail_presence(&user.user_id, &library_id, &request.content_hashes).await.map_err(asset_error)?;
    let present = revisions.iter().map(|entry| entry.content_hash).collect();
    server_wire_http::response(&headers, sync_common::api::assets::ThumbnailPresenceResponse { present, revisions })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use crate::bandwidth::TransferGovernor;
    use super::{parse_range, response_headers};
    use super::ranges::ByteRange;
    use axum::http::{header, StatusCode};
    use server_asset_store::AssetKind;
    use sync_common::BookId;

    #[tokio::test]
    async fn audio_track_ranges_are_relative_to_the_mp3_entry() {
        let content_hash = BookId::new(&"c".repeat(64));
        let asset = server_asset_store::StoredAsset { reader: Box::new(Cursor::new(b"zip!TRACKaudio!tail".to_vec())), total_len: 19, offset: 0, content_hash };
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::RANGE, "bytes=2-5".parse().unwrap());
        let response = super::stream_audio_track(asset, 4, 10, &headers, TransferGovernor::new(Default::default()), "test".into()).await.unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers().get(header::CONTENT_RANGE).unwrap(), "bytes 2-5/10");
        assert_eq!(response.headers().get(header::CONTENT_TYPE).unwrap(), "audio/mpeg");
        assert_eq!(axum::body::to_bytes(response.into_body(), 1024).await.unwrap().as_ref(), b"ACKa");
    }


    #[tokio::test]
    async fn range_responses_are_bound_to_the_content_hash() {
        let content_hash = BookId::new(&"b".repeat(64));
        let payload = b"0123456789abcdef";
        for (range, validator, expected) in [("bytes=4-7", format!("\"{content_hash}\""), StatusCode::PARTIAL_CONTENT), ("bytes=4-7", "\"old-revision\"".into(), StatusCode::OK)] {
            let asset = server_asset_store::StoredAsset { reader: Box::new(Cursor::new(payload.to_vec())), total_len: payload.len() as u64, offset: 0, content_hash };
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(header::RANGE, range.parse().unwrap());
            headers.insert(header::IF_RANGE, validator.parse().unwrap());
            let response = super::stream_asset(asset, AssetKind::Book, &headers, TransferGovernor::new(Default::default()), "test".into()).await.unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers().get(header::ETAG).unwrap().to_str().unwrap(), format!("\"{content_hash}\""));
            let bytes = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(bytes.as_ref(), if expected == StatusCode::OK { payload.as_slice() } else { &payload[4..8] });
        }
        let asset = server_asset_store::StoredAsset { reader: Box::new(Cursor::new(payload.to_vec())), total_len: payload.len() as u64, offset: 0, content_hash };
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::RANGE, "bytes=18446744073709551615-".parse().unwrap());
        assert_eq!(super::stream_asset(asset, AssetKind::Book, &headers, TransferGovernor::new(Default::default()), "test".into()).await.unwrap_err(), StatusCode::RANGE_NOT_SATISFIABLE);
    }


}
