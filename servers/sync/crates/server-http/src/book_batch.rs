use crate::{extract::{asset_error, AuthUser}, state::AppState};
use axum::{body::Body, extract::{Path, State}, http::{header, HeaderMap, StatusCode}, response::Response};
use futures_util::TryStreamExt;
use server_asset_store::AssetReader;
use std::{io, pin::Pin, sync::{Arc, Mutex}, task::{Context, Poll}};
use sync_common::{api::assets::BlobManifestRequest, book_batch as contract, LibraryId};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio_util::io::StreamReader;

// AssetApplication owns its reader. This adapter retains the underlying stream
// between frames without buffering a book or spawning detached upload tasks.
#[derive(Clone)]
struct SharedReader(Arc<Mutex<(AssetReader, u64)>>);
impl SharedReader {
    fn new(reader: AssetReader) -> Self { Self(Arc::new(Mutex::new((reader, 0)))) }
    fn consumed(&self) -> u64 { self.0.lock().unwrap().1 }
}
impl AsyncRead for SharedReader {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let mut state = self.0.lock().unwrap();
        let before = buf.filled().len();
        let result = Pin::new(&mut state.0).poll_read(cx, buf);
        state.1 += (buf.filled().len() - before) as u64;
        result
    }
}

async fn manifest(reader: &mut (impl AsyncRead + Unpin), length: u64) -> Result<BlobManifestRequest, StatusCode> {
    if length > contract::MAX_BATCH_BYTES + contract::MAX_MANIFEST_BYTES as u64 + 4 { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
    let size = reader.read_u32().await.map_err(|_| StatusCode::BAD_REQUEST)? as usize;
    if size == 0 || size > contract::MAX_MANIFEST_BYTES { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    let manifest: BlobManifestRequest = sync_common::transport::decode(&bytes, contract::MAX_MANIFEST_BYTES).map_err(|_| StatusCode::BAD_REQUEST)?;
    let total = contract::payload_bytes(&manifest.blobs).ok_or(StatusCode::BAD_REQUEST)?;
    if total + size as u64 + 4 != length { return Err(StatusCode::BAD_REQUEST); }
    Ok(manifest)
}

async fn drain_frame(reader: &SharedReader, start: u64, length: u64) -> Result<(), StatusCode> {
    let remaining = length.checked_sub(reader.consumed() - start).ok_or(StatusCode::BAD_REQUEST)?;
    let drained = tokio::io::copy(&mut reader.clone().take(remaining), &mut tokio::io::sink()).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    if drained != remaining { return Err(StatusCode::BAD_REQUEST); }
    Ok(())
}

pub(super) async fn upload(State(state): State<AppState>, user: AuthUser, Path(library_id): Path<LibraryId>, headers: HeaderMap, body: Body) -> Result<Response, StatusCode> {
    if headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) != Some(contract::MEDIA_TYPE)
        || headers.get(header::CONTENT_ENCODING).is_some() { return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE); }
    let length = headers.get(header::CONTENT_LENGTH).ok_or(StatusCode::LENGTH_REQUIRED)?.to_str().ok().and_then(|v| v.parse::<u64>().ok()).ok_or(StatusCode::BAD_REQUEST)?;
    let governor = state.transfer_governor.clone();
    let user_id = user.user_id.clone();
    let throttle_ns = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let measured_throttle = throttle_ns.clone();
    let measure = tracing::enabled!(target: "sync_performance", tracing::Level::DEBUG);
    let stream = body.into_data_stream().map_err(io::Error::other).and_then(move |chunk| {
        let governor = governor.clone(); let user_id = user_id.clone();
        let measured_throttle = measured_throttle.clone();
        async move {
            let started = measure.then(std::time::Instant::now);
            governor.upload(&user_id, chunk.len()).await?;
            if let Some(started) = started { measured_throttle.fetch_add(started.elapsed().as_nanos().min(u64::MAX as u128) as u64, std::sync::atomic::Ordering::Relaxed); }
            Ok(chunk)
        }
    });
    let mut reader = SharedReader::new(Box::new(StreamReader::new(Box::pin(stream))));
    let manifest = manifest(&mut reader, length).await?;
    let started = std::time::Instant::now();
    let mut results = Vec::with_capacity(manifest.blobs.len());
    for entry in manifest.blobs {
        let start = reader.consumed();
        let result = state.assets.put_book_revision(&user.user_id, &library_id, &entry.content_hash, &entry.checksum, entry.size_bytes, Box::new(reader.clone().take(entry.size_bytes))).await;
        // Admission may reject before consuming any bytes. Drain exactly this
        // frame so the following book remains independently uploadable.
        drain_frame(&reader, start, entry.size_bytes).await?;
        results.push(contract::UploadResult { content_hash: entry.content_hash, status: result.map(|_| StatusCode::OK).unwrap_or_else(asset_error).as_u16() });
    }
    let mut extra = [0];
    if reader.read(&mut extra).await.map_err(|_| StatusCode::BAD_REQUEST)? != 0 { return Err(StatusCode::BAD_REQUEST); }
    tracing::debug!(target: "sync_performance", phase = "book_batch_upload", books = results.len(), rejected = results.iter().filter(|r| r.status != 200).count(), bytes = length, elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, throttle_ms = throttle_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1_000_000.0);
    server_wire_http::response(&headers, contract::UploadResponse { results })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejected_or_partially_read_frame_does_not_consume_next_book() {
        let reader = SharedReader::new(Box::new(std::io::Cursor::new(b"abcdef".to_vec())));
        let mut frame = reader.clone().take(3);
        let mut first = [0]; frame.read_exact(&mut first).await.unwrap();
        drain_frame(&reader, 0, 3).await.unwrap();
        let mut second = Vec::new(); reader.clone().take(3).read_to_end(&mut second).await.unwrap();
        assert_eq!(second, b"def");
    }
    #[tokio::test]
    async fn truncated_frame_is_rejected() {
        let reader = SharedReader::new(Box::new(std::io::Cursor::new(b"ab".to_vec())));
        assert_eq!(drain_frame(&reader, 0, 3).await, Err(StatusCode::BAD_REQUEST));
    }
    #[tokio::test]
    async fn rejects_oversized_header_before_allocating() {
        let mut reader = std::io::Cursor::new(u32::MAX.to_be_bytes());
        assert!(matches!(manifest(&mut reader, 100).await, Err(StatusCode::PAYLOAD_TOO_LARGE)));
    }

    #[tokio::test]
    async fn manifest_validates_exact_request_length_and_duplicate_books() {
        use sync_common::api::assets::BlobManifestEntry;
        let hash = sync_common::ContentHash::new(&"a".repeat(64));
        let entry = BlobManifestEntry { content_hash: hash, checksum: hash, size_bytes: 3 };
        for duplicate in [false, true] {
            let blobs = if duplicate { vec![entry.clone(), entry.clone()] } else { vec![entry.clone()] };
            let bytes = sync_common::transport::encode(&BlobManifestRequest { blobs }).unwrap();
            let mut frame = (bytes.len() as u32).to_be_bytes().to_vec(); frame.extend(bytes);
            let length = frame.len() as u64 + if duplicate { 6 } else { 3 };
            let result = manifest(&mut std::io::Cursor::new(frame.clone()), length).await;
            assert_eq!(result.is_ok(), !duplicate);
            assert!(manifest(&mut std::io::Cursor::new(frame), length + 1).await.is_err());
        }
    }

    #[tokio::test]
    async fn frame_reader_supports_backpressure_without_buffering_the_batch() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(1);
        let task = tokio::spawn(async move { writer.write_all(b"abcdef").await.unwrap(); });
        let reader = SharedReader::new(Box::new(reader));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            drain_frame(&reader, 0, 3).await.unwrap();
            let mut second = Vec::new(); reader.clone().take(3).read_to_end(&mut second).await.unwrap();
            assert_eq!(second, b"def");
            task.await.unwrap();
        }).await.unwrap();
    }
}
