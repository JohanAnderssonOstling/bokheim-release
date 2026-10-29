//! Generic bounded byte ranges; no knowledge of book containers.

use std::io::{self, SeekFrom};

use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use futures_util::stream;
use server_asset_store::{AssetKind, StoredAsset};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{parse_range, response_headers, TransferGovernor, STREAM_CHUNK_BYTES};

const MAX_RANGES: usize = 128;
const MAX_BYTES: u64 = 4 * 1024 * 1024;

pub(super) async fn response(mut asset: StoredAsset, headers: &HeaderMap, governor: TransferGovernor, user: String) -> Result<Response, StatusCode> {
    let value = headers.get(header::RANGE).ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?.to_str().map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
    let raw = value.strip_prefix("bytes=").ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?;
    let mut ranges = Vec::new();
    let mut total = 0_u64;
    let mut previous_end = 0;
    for part in raw.split(',') {
        if ranges.len() == MAX_RANGES {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
        let value = format!("bytes={}", part.trim()).parse().map_err(|_| StatusCode::RANGE_NOT_SATISFIABLE)?;
        let range = parse_range(&value, asset.total_len)?;
        let end = range.end.unwrap_or_else(|| asset.total_len.saturating_sub(1)).min(asset.total_len.saturating_sub(1));
        if range.start >= asset.total_len || end < range.start || (!ranges.is_empty() && range.start < previous_end) {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
        let length = end - range.start + 1;
        total = total.checked_add(length).filter(|&n| n <= MAX_BYTES).ok_or(StatusCode::RANGE_NOT_SATISFIABLE)?;
        previous_end = end + 1;
        ranges.push((range.start, length));
    }
    // A random boundary prevents file contents from being chosen to impersonate
    // a part separator. The reader validates each part's explicit byte length.
    let boundary = format!("bokheim-{}", uuid::Uuid::new_v4().simple());
    let mut response_headers = response_headers(AssetKind::Book, &asset.content_hash);
    response_headers.insert(header::CONTENT_TYPE, format!("multipart/byteranges; boundary={boundary}").parse().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?);
    let mut parts = Vec::new();
    let mut wire_length = 0_u64;
    for &(offset, length) in &ranges {
        let prefix = format!("--{boundary}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {offset}-{}/{}\r\n\r\n", offset + length - 1, asset.total_len);
        wire_length += prefix.len() as u64 + length + 2;
        parts.push((offset, length, prefix));
    }
    let closing = format!("--{boundary}--\r\n");
    wire_length += closing.len() as u64;
    response_headers.insert(header::CONTENT_LENGTH, wire_length.into());
    // Seek and stream one bounded chunk at a time. No full bundle allocation.
    asset.reader.seek(SeekFrom::Start(parts[0].0)).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let body = stream::try_unfold((asset, parts.into_iter(), None::<u64>, closing, false, governor, user), |(mut asset, mut parts, remaining, closing, done, governor, user)| async move {
        let (bytes, remaining, done) = if let Some(remaining) = remaining {
            if remaining == 0 {
                (Bytes::from_static(b"\r\n"), None, done)
            } else {
                let mut bytes = vec![0; remaining.min(STREAM_CHUNK_BYTES as u64) as usize];
                asset.reader.read_exact(&mut bytes).await?;
                governor.download(&user, bytes.len()).await?;
                let count = bytes.len() as u64;
                (Bytes::from(bytes), Some(remaining - count), done)
            }
        } else if let Some((offset, length, prefix)) = parts.next() {
            asset.reader.seek(SeekFrom::Start(offset)).await?;
            (Bytes::from(prefix), Some(length), done)
        } else if !done {
            (Bytes::copy_from_slice(closing.as_bytes()), None, true)
        } else {
            return Ok::<_, io::Error>(None);
        };
        Ok(Some((bytes, (asset, parts, remaining, closing, done, governor, user))))
    });
    let mut response = Response::new(Body::from_stream(body));
    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    *response.headers_mut() = response_headers;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use sync_common::BookId;

    #[tokio::test]
    async fn rejects_overlapping_unsorted_excessive_and_out_of_bounds_ranges() {
        let excessive = format!("bytes={}", (0..129).map(|n| format!("{}-{}", n * 2, n * 2)).collect::<Vec<_>>().join(","));
        for value in ["bytes=0-5,4-8", "bytes=8-9,0-1", "bytes=0-1,9999999-", "bytes=0-4194304,4194305-4194305", "bytes=0-1,18446744073709551615-", &excessive] {
            let asset = StoredAsset { reader: Box::new(Cursor::new(Vec::new())), total_len: 8 * 1024 * 1024, offset: 0, content_hash: BookId::new(&"f".repeat(64)) };
            let mut headers = HeaderMap::new();
            headers.insert(header::RANGE, value.parse().unwrap());
            assert_eq!(response(asset, &headers, TransferGovernor::new(Default::default()), "test".into()).await.unwrap_err(), StatusCode::RANGE_NOT_SATISFIABLE, "{value}");
        }
    }

    #[tokio::test]
    async fn multipart_stream_fails_on_truncated_storage() {
        let asset = StoredAsset { reader: Box::new(Cursor::new(b"abc".to_vec())), total_len: 10, offset: 0, content_hash: BookId::new(&"f".repeat(64)) };
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=0-1,8-9".parse().unwrap());
        let response = response(asset, &headers, TransferGovernor::new(Default::default()), "test".into()).await.unwrap();
        assert!(!response.headers().contains_key(header::CONTENT_RANGE));
        assert!(axum::body::to_bytes(response.into_body(), 4096).await.is_err());
    }
}
