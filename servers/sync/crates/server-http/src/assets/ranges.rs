//! Seekable generic file responses, shared by the server and HTTP client tests.
use crate::bandwidth::TransferGovernor;
use axum::{
    body::Body,
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use futures_util::TryStreamExt;
use server_asset_store::AssetKind;
use std::io::SeekFrom;
use sync_common::BookId;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
const STREAM_CHUNK_BYTES: usize = 64 * 1024;
#[path = "multipart.rs"]
mod multipart;
pub(super) fn response_headers(kind: AssetKind, hash: &BookId) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_RANGES, header::HeaderValue::from_static("bytes"));
    if kind == AssetKind::Book {
        headers.insert(header::ETAG, header::HeaderValue::from_str(&format!("\"{hash}\"")).expect("validated content hashes produce valid ETags"));
    }
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(match kind {
            AssetKind::Book => "application/octet-stream",
            AssetKind::Thumbnail | AssetKind::ThumbnailBrowse => "image/jpeg",
        }),
    );
    if matches!(kind, AssetKind::Thumbnail | AssetKind::ThumbnailBrowse) {
        headers.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("private, no-store, no-transform"));
    }
    headers
}

pub(crate) async fn stream_asset(mut asset: server_asset_store::StoredAsset, kind: AssetKind, request_headers: &HeaderMap, governor: TransferGovernor, user_id: String) -> Result<Response, StatusCode> {
    let headers = response_headers(kind, &asset.content_hash);
    let if_range_matches = request_headers.get(header::IF_RANGE).is_none_or(|value| Some(value) == headers.get(header::ETAG));
    if kind == AssetKind::Book && if_range_matches && request_headers.get(header::RANGE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.contains(',')) {
        return multipart::response(asset, request_headers, governor, user_id).await;
    }
    let requested_range = if if_range_matches { request_headers.get(header::RANGE).map(|value| parse_range(value, asset.total_len)).transpose()? } else { None };
    let offset = requested_range.map(|range| range.start).unwrap_or(0);
    if requested_range.is_some() && asset.total_len == 0 {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    let throttled_user = user_id.to_string();
    let response_end = requested_range.and_then(|range| range.end).unwrap_or_else(|| asset.total_len.saturating_sub(1));
    if offset >= asset.total_len || response_end < offset {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    asset.reader.seek(SeekFrom::Start(offset)).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let response_len = response_end.min(asset.total_len.saturating_sub(1)) - offset + 1;
    let stream = ReaderStream::with_capacity(asset.reader.take(response_len), STREAM_CHUNK_BYTES).and_then(move |chunk| {
        let governor = governor.clone();
        let user_id = throttled_user.clone();
        async move {
            governor.download(&user_id, chunk.len()).await?;
            Ok(chunk)
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = if requested_range.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };
    *response.headers_mut() = response_headers(kind, &asset.content_hash);
    response.headers_mut().insert(header::CONTENT_LENGTH, response_len.into());
    if requested_range.is_some() {
        let end = offset + response_len - 1;
        let value = format!("bytes {offset}-{end}/{}", asset.total_len);
        response.headers_mut().insert(header::CONTENT_RANGE, header::HeaderValue::from_str(&value).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?);
    }
    Ok(response)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ByteRange {
    pub(super) start: u64,
    pub(super) end: Option<u64>,
}

pub(super) fn parse_range(value: &header::HeaderValue, total_len: u64) -> Result<ByteRange, StatusCode> {
    let value = value.to_str().map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
    let Some(raw) = value.strip_prefix("bytes=") else { return Err(StatusCode::RANGE_NOT_SATISFIABLE) };
    if raw.contains(',') {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    let Some((start, end)) = raw.split_once('-') else { return Err(StatusCode::RANGE_NOT_SATISFIABLE) };
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
        if suffix == 0 || total_len == 0 {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
        return Ok(ByteRange { start: total_len.saturating_sub(suffix), end: None });
    }
    let start = start.parse::<u64>().map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
    let end = (!end.is_empty()).then(|| end.parse::<u64>().map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)).transpose()?;
    if end.is_some_and(|end| end < start) {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    Ok(ByteRange { start, end })
}
