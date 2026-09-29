//! Format-independent, seekable remote file access. Cached fragments are never
//! installed as a downloaded asset or treated as a verified complete file.

use crate::range_cache::RangeCache;
use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

const BLOCK_BYTES: usize = 64 * 1024;
const CACHE_BLOCKS: usize = 64;
const MAX_RANGE_BYTES: usize = 4 * BLOCK_BYTES;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    pub hash: crate::ContentHash,
    pub length: u64,
    pub checksum: crate::ContentHash,
}

impl std::fmt::Debug for RemoteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("unauthorized"),
            Self::Retryable(error) | Self::Failed(error) => f.write_str(error),
        }
    }
}
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
pub async fn describe(url: reqwest::Url, access_token: String, hash: crate::ContentHash) -> Result<(RemoteFile, Vec<u8>), RemoteFileError> {
    let request = async {
        let response = reqwest::Client::new().get(url).bearer_auth(&access_token).header(reqwest::header::RANGE, format!("bytes=0-{}", BLOCK_BYTES - 1)).send().await.map_err(RemoteFileError::transport)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            drop(response.bytes_stream());
            return Err(RemoteFileError::Unauthorized);
        }
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            let status = response.status();
            drop(response.bytes_stream());
            return Err(RemoteFileError::status(status));
        }
        // Book identity survives metadata edits; ETag identifies the
        // exact uploaded bytes. Pin that revision for every subsequent range.
        let checksum = response.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix('"')).and_then(|v| v.strip_suffix('"')).and_then(|v| v.parse::<crate::ContentHash>().ok());
        let Some(checksum) = checksum else {
            drop(response.bytes_stream());
            return Err("invalid remote file revision".into());
        };
        let etag = format!("\"{checksum}\"");
        let length = response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.rsplit_once('/')).and_then(|(_, total)| total.parse::<u64>().ok()).filter(|&n| n > 0 && n <= 9_007_199_254_740_991);
        let Some(length) = length else {
            drop(response.bytes_stream());
            return Err("invalid remote file length".into());
        };
        let count = length.min(BLOCK_BYTES as u64) as usize;
        let expected = format!("bytes 0-{}/{length}", count - 1);
        if response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) != Some(expected.as_str()) || response.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()) != Some(etag.as_str()) {
            drop(response.bytes_stream());
            return Err("remote file range identity or bounds changed".into());
        }
        let bytes = read_bounded_response(response, count).await?;
        Ok((RemoteFile { hash, length, checksum }, bytes))
    };
    crate::time::timeout(REQUEST_TIMEOUT, request).await.map_err(|_| RemoteFileError::Retryable("remote file opening request timed out".to_owned()))?
}

type Fetch = Box<dyn FnMut(u64, usize) -> io::Result<Vec<u8>> + Send + Sync>;

struct RangeReader {
    length: u64,
    position: u64,
    cache: RangeCache,
    fetch: Fetch,
    next_fetch_offset: Option<u64>,
    batch_blocks: usize,
}

fn validate_prefix(length: u64, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() != length.min(BLOCK_BYTES as u64) as usize || bytes.is_empty() {
        return Err("invalid remote file prefix".into());
    }
    Ok(())
}

impl RangeReader {
    fn with_prefix(mut self, bytes: Vec<u8>) -> Self {
        self.cache.insert(0, bytes);
        self
    }

    fn new(length: u64, fetch: Fetch) -> Self {
        Self { length, position: 0, cache: RangeCache::new(CACHE_BLOCKS), fetch, next_fetch_offset: None, batch_blocks: 1 }
    }
}

impl Read for RangeReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let offset = self.position / BLOCK_BYTES as u64 * BLOCK_BYTES as u64;
        if !self.cache.contains(self.position) {
            // Grow only after a contiguous cache miss. Random seeks reset the
            // window, keeping ZIP metadata reads small. Stop before a cached
            // block so speculative bytes never duplicate existing fragments.
            let mut blocks = if self.next_fetch_offset == Some(offset) { (self.batch_blocks * 2).min(MAX_RANGE_BYTES / BLOCK_BYTES) } else { 1 };
            for ahead in 1..blocks {
                let distance = (ahead * BLOCK_BYTES) as u64;
                if distance >= self.length - offset {
                    break;
                }
                let next = offset + distance;
                if self.cache.contains(next) {
                    blocks = ahead;
                    break;
                }
            }
            let length = (self.length - offset).min((blocks * BLOCK_BYTES) as u64) as usize;
            let bytes = (self.fetch)(offset, length)?;
            if bytes.len() != length {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete remote file range"));
            }
            // A single block can take ownership of the response allocation.
            // Split larger batches so evicting a block releases its allocation
            // independently of the neighbouring cached blocks.
            if bytes.len() <= BLOCK_BYTES {
                self.cache.insert(offset, bytes);
            } else {
                for (index, chunk) in bytes.chunks(BLOCK_BYTES).enumerate().rev() {
                    self.cache.insert(offset + (index * BLOCK_BYTES) as u64, chunk.to_vec());
                }
            }
            self.next_fetch_offset = Some(offset + length as u64);
            self.batch_blocks = blocks;
        }
        let count = self.cache.read(self.position, output);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::Current(offset) => self.position as i128 + offset as i128,
            SeekFrom::End(offset) => self.length as i128 + offset as i128,
        };
        let position = u64::try_from(position).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid remote file seek"))?;
        if position != self.position {
            self.next_fetch_offset = None;
            self.batch_blocks = 1;
        }
        self.position = position;
        Ok(self.position)
    }
}

pub async fn fetch_range(client: &reqwest::Client, url: reqwest::Url, source: &RemoteFile, token: &str, offset: u64, length: usize) -> Result<Vec<u8>, RemoteFileError> {
    if length == 0 || length > MAX_RANGE_BYTES || offset.checked_add(length as u64).is_none_or(|end| end > source.length) {
        return Err("invalid remote file range".into());
    }
    let etag = format!("\"{}\"", source.checksum);
    let end = offset + length as u64 - 1;
    let response = client.get(url).bearer_auth(token).header(reqwest::header::RANGE, format!("bytes={offset}-{end}")).header(reqwest::header::IF_RANGE, &etag).send().await.map_err(RemoteFileError::transport)?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        drop(response.bytes_stream());
        return Err(RemoteFileError::Unauthorized);
    }
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        let status = response.status();
        // Explicitly cancel the readable body as well as the fetch request.
        // Browsers may otherwise continue filling their HTTP cache.
        drop(response.bytes_stream());
        return Err(RemoteFileError::status(status));
    }
    let expected = format!("bytes {offset}-{end}/{}", source.length);
    if response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) != Some(expected.as_str()) || response.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()) != Some(etag.as_str()) {
        drop(response.bytes_stream());
        return Err("remote file range identity or bounds changed".into());
    }
    read_bounded_response(response, length).await
}

async fn read_bounded_response(response: reqwest::Response, length: usize) -> Result<Vec<u8>, RemoteFileError> {
    use futures_util::StreamExt;
    let mut bytes = Vec::with_capacity(length);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(RemoteFileError::transport)?;
        if chunk.len() > length - bytes.len() {
            return Err("oversized remote file range".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() != length {
        return Err(RemoteFileError::Retryable("incomplete remote file range".into()));
    }
    Ok(bytes)
}

/// The parser sees only a seekable file. The owner supplies authenticated,
/// bounded byte reads, independently of the file's format.
#[cfg(not(target_arch = "wasm32"))]
pub fn native_reader<F>(length: u64, initial_bytes: Vec<u8>, mut fetch: F) -> Result<crate::BoxedBookReader, String>
where
    F: FnMut(u64, usize) -> crate::FetchFuture<'static, Result<Vec<u8>, String>> + Send + 'static,
{
    validate_prefix(length, &initial_bytes)?;
    let mut fetch = native_bundle_fetch(move |ranges| {
        let (offset, length) = ranges[0];
        fetch(offset, length)
    })?;
    Ok(Box::new(RangeReader::new(length, Box::new(move |offset, length| fetch(vec![(offset, length)]))).with_prefix(initial_bytes)))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn native_bundle_fetch<F>(mut fetch: F) -> Result<crate::BundleFetch, String>
where
    F: FnMut(Vec<(u64, usize)>) -> crate::FetchFuture<'static, Result<Vec<u8>, String>> + Send + 'static,
{
    type Request = (Vec<(u64, usize)>, std::sync::mpsc::SyncSender<Result<Vec<u8>, String>>);
    let (send, receive) = std::sync::mpsc::sync_channel::<Request>(1);
    std::thread::Builder::new()
        .name("remote-file".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            while let Ok((ranges, result)) = receive.recv() {
                let response = match &runtime {
                    Ok(runtime) => runtime.block_on(async { tokio::time::timeout(REQUEST_TIMEOUT, fetch(ranges)).await.map_err(|_| "remote file read timed out".to_owned())? }),
                    Err(error) => Err(error.to_string()),
                };
                let _ = result.send(response);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Box::new(move |ranges| {
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        send.send((ranges, reply)).map_err(|_| io::Error::other("remote file reader stopped"))?;
        result.recv_timeout(REQUEST_TIMEOUT + Duration::from_secs(2)).map_err(io::Error::other)?.map_err(io::Error::other)
    }))
}

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::{browser_bundle_fetch, browser_reader, BrowserReader};

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

pub async fn fetch_bundle(client: &reqwest::Client, mut url: reqwest::Url, source: &RemoteFile, token: &str, ranges: Vec<(u64, usize)>) -> Result<Vec<u8>, RemoteFileError> {
    let request = sync_common::api::assets::BookRanges { checksum: source.checksum, ranges };
    let length = request.byte_count(source.length).ok_or("invalid book bundle")?;
    let single_url = url.clone();
    url.set_path(&format!("{}/ranges", url.path()));
    let body = sync_common::transport::encode(&request).map_err(|e| RemoteFileError::Failed(e.to_string()))?;
    let response = client.post(url).bearer_auth(token).header(reqwest::header::CONTENT_TYPE, sync_common::transport::MEDIA_TYPE).body(body).send().await.map_err(RemoteFileError::transport)?;
    // Older servers retain their bounded, revision-checked range transport.
    if matches!(response.status(), reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED) {
        drop(response.bytes_stream());
        let mut bytes = Vec::with_capacity(length);
        for (mut offset, mut count) in request.ranges {
            while count > 0 {
                let size = count.min(MAX_RANGE_BYTES);
                bytes.extend(fetch_range(client, single_url.clone(), source, token, offset, size).await?);
                offset += size as u64;
                count -= size;
            }
        }
        return Ok(bytes);
    }
    if response.status() != reqwest::StatusCode::OK {
        let status = response.status();
        drop(response.bytes_stream());
        return Err(RemoteFileError::status(status));
    }
    let etag = format!("\"{}\"", source.checksum);
    if response.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()) != Some(etag.as_str()) {
        drop(response.bytes_stream());
        return Err("book bundle revision changed".into());
    }
    read_bounded_response(response, length).await
}
