use crate::{PerformanceTrace, SyncCredentials, SyncRequestError, TransportError};
use futures_util::StreamExt;
use sync_common::{api::assets::BlobManifestEntry, book_batch as contract, LibraryId};

pub async fn upload_book_batch(client: &reqwest::Client, credentials: &SyncCredentials, library: &LibraryId, entries: &[BlobManifestEntry], length: u64, body: reqwest::Body) -> Result<Option<contract::UploadResponse>, SyncRequestError> {
    let book_bytes = contract::payload_bytes(entries).ok_or_else(|| TransportError::InvalidData("invalid book batch".into()))?;
    let mut trace = PerformanceTrace::new("book_batch_http_upload", "send_body_wait_response");
    log::debug!(target: "sync_performance", "trace_id={} library_id={} books={} book_bytes={book_bytes} request_body_bytes={length}", trace.id(), library, entries.len());
    let endpoint = credentials.endpoint(&format!("api/libraries/{library}/book-batch"));
    let upload_started = web_time::Instant::now();
    let response = client
        .put(endpoint.clone())
        .bearer_auth(credentials.access_token())
        .header("x-bokheim-trace-id", trace.id())
        .header(reqwest::header::ACCEPT, sync_common::transport::MEDIA_TYPE)
        .header(reqwest::header::CONTENT_TYPE, contract::MEDIA_TYPE)
        .header(reqwest::header::CONTENT_LENGTH, length)
        .body(body)
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            // A peer without this route can close while the body is still being
            // streamed, hiding its 404 behind BrokenPipe. Probe without a body;
            // a supported PUT-only route returns 405, an absent route 404.
            // Never turn an ambiguous failure into permanent per-book rejection.
            trace.phase("probe_after_send_error");
            let missing = client.head(endpoint).bearer_auth(credentials.access_token()).send().await.is_ok_and(|response| response.status() == reqwest::StatusCode::NOT_FOUND);
            if missing {
                trace.finish(false);
                return Ok(None);
            }
            return Err(TransportError::Network(error).into());
        }
    };
    let status = response.status();
    if matches!(status, reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED) {
        trace.finish(true);
        return Ok(None);
    }
    if !status.is_success() {
        return Err(TransportError::BadStatus(status, "book batch request failed".into()).into());
    }
    if !response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(sync_common::transport::is_current_media_type) {
        return Err(TransportError::InvalidData("unsupported batch response type".into()).into());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(TransportError::Network)?;
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err(TransportError::InvalidData("oversized batch response".into()).into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let upload_ms = upload_started.elapsed().as_secs_f64() * 1000.0;
    let result: contract::UploadResponse = sync_common::transport::decode(&bytes, 64 * 1024).map_err(|e| TransportError::InvalidData(e.to_string()))?;
    if result.results.len() != entries.len() || result.results.iter().zip(entries).any(|(r, e)| r.content_hash != e.content_hash || !(200..=599).contains(&r.status)) {
        return Err(TransportError::InvalidData("invalid per-book batch results".into()).into());
    }
    let successful_book_bytes: u64 = result.results.iter().zip(entries).filter(|(result, _)| (200..300).contains(&result.status)).map(|(_, entry)| entry.size_bytes).sum();
    log::debug!(target: "sync_performance", "side=client trace_id={} event=book_upload_sample mode=batch book_bytes={book_bytes} successful_book_bytes={successful_book_bytes} upload_ms={upload_ms:.3}", trace.id());
    trace.finish(true);
    Ok(Some(result))
}
