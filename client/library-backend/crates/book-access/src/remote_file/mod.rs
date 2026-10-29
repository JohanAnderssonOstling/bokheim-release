//! Format-independent, seekable remote file access. Cached fragments are never
//! installed as a downloaded asset or treated as a verified complete file.

use crate::BookId;
use http_response::BodyReadError;
use std::fmt::Formatter;
use std::time::Duration;

pub(crate) use crate::range_cache::MAX_BUNDLE_BYTES;
use crate::range_reader::{BLOCK_BYTES, MAX_RANGE_BYTES};
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// ZIP's fixed EOCD record plus its largest legal comment.
pub const ZIP_TAIL_BYTES: usize = 22 + u16::MAX as usize;

mod multipart;

#[cfg(not(target_arch = "wasm32"))]
mod audiobook_stream;
#[cfg(not(target_arch = "wasm32"))]
pub use audiobook_stream::native_audio_reader;
#[cfg(not(target_arch = "wasm32"))]
pub use audiobook_stream::AudioStreamControl;

/// File identity and length, with no URL, credentials, or format knowledge.
/// The owning library resolves and authenticates byte reads.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteFile {
    pub hash: BookId,
    pub length: u64,
    /// Opaque provider revision, pinned for the lifetime of an open reader.
    /// Content-addressed Bokheim files do not need a separate revision.
    #[serde(default)]
    pub revision: Option<String>,
}

impl std::fmt::Debug for RemoteFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteFile").field("length", &self.length).field("hash", &self.hash).finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum RemoteFileError {
    Unauthorized,
    Retryable(String),
    Failed(String),
}
impl std::fmt::Display for RemoteFileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("unauthorized"),
            Self::Retryable(error) | Self::Failed(error) => f.write_str(error),
        }
    }
}
impl std::error::Error for RemoteFileError {}

impl From<String> for RemoteFileError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}
impl From<&str> for RemoteFileError {
    fn from(error: &str) -> Self {
        Self::Failed(error.into())
    }
}

impl RemoteFileError {
    pub fn transport(error: reqwest::Error) -> Self {
        // URL removal also prevents signed URLs or access tokens reaching logs.
        Self::Retryable(error.without_url().to_string())
    }

    pub fn status(status: reqwest::StatusCode) -> Self {
        let message = format!("remote file range request returned HTTP {status}");
        if status == reqwest::StatusCode::UNAUTHORIZED {
            Self::Unauthorized
        } else if status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            Self::Retryable(message)
        } else {
            Self::Failed(message)
        }
    }
}

/// Open with a bounded prefix read so discovering the length also fills the cache.
pub async fn describe(client: &reqwest::Client, url: reqwest::Url, access_token: &str, hash: BookId) -> Result<(RemoteFile, Vec<u8>), RemoteFileError> {
    describe_window(client, url, access_token, hash, BLOCK_BYTES, false).await
}

/// One request path for prefix discovery and revision-pinned subsequent reads.
async fn request_range(client: &reqwest::Client, url: reqwest::Url, token: &str, hash: BookId, offset: u64, length: usize, pin_revision: bool) -> Result<reqwest::Response, RemoteFileError> {
    let end = offset + length as u64 - 1;
    request_ranges(client, url, token, hash, format!("bytes={offset}-{end}"), pin_revision).await
}

async fn request_ranges(client: &reqwest::Client, url: reqwest::Url, token: &str, hash: BookId, range: String, pin_revision: bool) -> Result<reqwest::Response, RemoteFileError> {
    let response = send_ranges(client, url, token, hash, range, pin_revision).await?;
    validate_response(response, hash)
}

async fn send_ranges(client: &reqwest::Client, url: reqwest::Url, token: &str, hash: BookId, range: String, pin_revision: bool) -> Result<reqwest::Response, RemoteFileError> {
    let etag = format!("\"{hash}\"");
    let mut request = client.get(url).bearer_auth(token).header(reqwest::header::RANGE, range);
    if pin_revision {
        request = request.header(reqwest::header::IF_RANGE, &etag);
    }
    request.send().await.map_err(RemoteFileError::transport)
}

fn validate_response(response: reqwest::Response, hash: BookId) -> Result<reqwest::Response, RemoteFileError> {
    let etag = format!("\"{hash}\"");
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        let error = RemoteFileError::status(response.status());
        drop(response.bytes_stream());
        return Err(error);
    }
    if response.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()) != Some(etag.as_str()) {
        drop(response.bytes_stream());
        return Err("remote file revision changed".into());
    }
    Ok(response)
}

/// Discover the immutable file length while fetching the ZIP locator window.
pub async fn describe_tail(client: &reqwest::Client, url: reqwest::Url, token: &str, hash: BookId) -> Result<(RemoteFile, Vec<u8>), RemoteFileError> {
    describe_window(client, url, token, hash, ZIP_TAIL_BYTES, true).await
}

async fn describe_window(client: &reqwest::Client, url: reqwest::Url, token: &str, hash: BookId, window: usize, tail: bool) -> Result<(RemoteFile, Vec<u8>), RemoteFileError> {
    let request = async {
        let range = if tail { format!("bytes=-{window}") } else { format!("bytes=0-{}", window - 1) };
        let response = request_ranges(client, url, token, hash, range, false).await?;
        let length = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit_once('/'))
            .and_then(|(_, total)| total.parse::<u64>().ok())
            .filter(|&n| n > 0 && n <= 9_007_199_254_740_991)
            .ok_or("invalid remote file length")?;
        let source = RemoteFile { hash, length, revision: None };
        let count = length.min(window as u64) as usize;
        let offset = if tail { length - count as u64 } else { 0 };
        let bytes = read_range_response(response, &source, offset, count).await?;
        Ok((source, bytes))
    };
    crate::time::timeout(REQUEST_TIMEOUT, request).await.map_err(|_| RemoteFileError::Retryable("remote file opening request timed out".into()))?
}

/// One bounded HTTP request, concatenating the requested intervals in order.
/// The Bokheim endpoint preserves the already sorted, disjoint range list.
pub async fn fetch_ranges(client: &reqwest::Client, url: reqwest::Url, source: &RemoteFile, token: &str, ranges: Vec<(u64, usize)>) -> Result<Vec<u8>, RemoteFileError> {
    let length = bundle_byte_count(&ranges, source.length).ok_or("invalid book ranges")?;
    let request = async {
        if let [(offset, count)] = ranges.as_slice() {
            let response = request_range(client, url, token, source.hash, *offset, *count, true).await?;
            return read_range_response(response, source, *offset, *count).await;
        }
        let range = format!("bytes={}", ranges.iter().map(|(offset, count)| format!("{offset}-{}", offset + *count as u64 - 1)).collect::<Vec<_>>().join(","));
        let response = send_ranges(client, url.clone(), token, source.hash, range, true).await?;
        if response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            // Older Bokheim servers only accept a single range. Drop the body
            // and retain exact byte selection through the previous transport.
            drop(response);
            return fetch_bundle(client, url, source, token, ranges.clone()).await;
        }
        let response = validate_response(response, source.hash)?;
        multipart::read(response, source.length, &ranges, length).await
    };
    crate::time::timeout(REQUEST_TIMEOUT, request).await.map_err(|_| RemoteFileError::Retryable("remote file ranges timed out".into()))?
}

pub async fn fetch_range(client: &reqwest::Client, url: reqwest::Url, source: &RemoteFile, token: &str, offset: u64, length: usize) -> Result<Vec<u8>, RemoteFileError> {
    if length == 0 || length > MAX_RANGE_BYTES || offset.checked_add(length as u64).is_none_or(|end| end > source.length) {
        return Err("invalid remote file range".into());
    }
    let response = request_range(client, url, token, source.hash, offset, length, true).await?;
    read_range_response(response, source, offset, length).await
}

async fn read_range_response(response: reqwest::Response, source: &RemoteFile, offset: u64, length: usize) -> Result<Vec<u8>, RemoteFileError> {
    let end = offset + length as u64 - 1;
    let expected = format!("bytes {offset}-{end}/{}", source.length);
    if response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        drop(response.bytes_stream());
        return Err("remote file range bounds changed".into());
    }
    read_bounded_response(response, length).await
}

async fn read_bounded_response(response: reqwest::Response, length: usize) -> Result<Vec<u8>, RemoteFileError> {
    let bytes = http_response::read(response, length).await.map_err(|error| match error {
        BodyReadError::Http(error) => RemoteFileError::transport(error),
        BodyReadError::TooLarge { .. } => RemoteFileError::Failed("oversized remote file range".into()),
    })?;
    if bytes.len() != length {
        return Err(RemoteFileError::Retryable("incomplete remote file range".into()));
    }
    Ok(bytes)
}

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{native_bundle_fetch, native_reader};

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::{browser_bundle_fetch, browser_reader, BrowserReader};

/// Validate local bundle bounds before issuing any network requests.
pub(crate) fn bundle_byte_count(ranges: &[(u64, usize)], length: u64) -> Option<usize> {
    range_byte_count(ranges, length, MAX_BUNDLE_BYTES)
}

/// Validate ordered, nonempty ranges for one immutable file. Command and
/// transport callers supply their own admitted byte budget.
pub fn range_byte_count(ranges: &[(u64, usize)], length: u64, max_bytes: usize) -> Option<usize> {
    if ranges.is_empty() || ranges.len() > 128 {
        return None;
    }
    let mut end = 0;
    let mut total = 0usize;
    for &(offset, count) in ranges {
        if count == 0 || offset < end {
            return None;
        }
        end = offset.checked_add(count as u64)?;
        total = total.checked_add(count)?;
        if end > length || total > max_bytes {
            return None;
        }
    }
    Some(total)
}

#[cfg(test)]
mod range_tests {
    use super::range_byte_count;

    #[test]
    fn range_admission_checks_bounds_order_count_and_the_callers_byte_budget() {
        let ranges = [(0, 2), (2, 2), (8, 2)];
        assert_eq!(range_byte_count(&ranges, 10, 6), Some(6));
        assert_eq!(range_byte_count(&ranges, 10, 5), None);
        assert_eq!(range_byte_count(&ranges, 9, 6), None);
        for invalid in [&[][..], &[(0, 0)], &[(2, 2), (0, 2)], &[(0, 3), (2, 2)], &[(u64::MAX, 1)]] {
            assert_eq!(range_byte_count(invalid, u64::MAX, usize::MAX), None, "{invalid:?}");
        }
        let ranges = (0..128).map(|offset| (offset, 1)).collect::<Vec<_>>();
        assert_eq!(range_byte_count(&ranges, 129, 129), Some(128));
        let mut excessive = ranges;
        excessive.push((128, 1));
        assert_eq!(range_byte_count(&excessive, 129, 129), None);
        assert_eq!(range_byte_count(&[(0, 8 * 1024 * 1024)], 8 * 1024 * 1024, 4 * 1024 * 1024), None);
        assert_eq!(range_byte_count(&[(0, 8 * 1024 * 1024)], 8 * 1024 * 1024, 64 * 1024 * 1024), Some(8 * 1024 * 1024));
    }
}

/// Fetch bounded HTTP ranges concurrently, concatenating them in request order.
pub async fn fetch_bundle(client: &reqwest::Client, url: reqwest::Url, source: &RemoteFile, token: &str, ranges: Vec<(u64, usize)>) -> Result<Vec<u8>, RemoteFileError> {
    use futures_util::{StreamExt, TryStreamExt};
    let length = bundle_byte_count(&ranges, source.length).ok_or("invalid book bundle")?;
    let chunks = ranges.into_iter().flat_map(|(offset, count)| (0..count).step_by(MAX_RANGE_BYTES).map(move |start| (offset + start as u64, (count - start).min(MAX_RANGE_BYTES))));
    let mut responses = futures_util::stream::iter(chunks.map(|(offset, count)| fetch_range(client, url.clone(), source, token, offset, count))).buffered(8);
    let mut bytes = Vec::with_capacity(length);
    while let Some(chunk) = responses.try_next().await? {
        bytes.extend(chunk);
    }
    Ok(bytes)
}
