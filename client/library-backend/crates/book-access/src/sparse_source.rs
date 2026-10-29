//! Revision-scoped exact byte intervals shared by independently seeking cursors.

use std::{
    collections::VecDeque,
    future::Future,
    io::{self, SeekFrom},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{ready, Context, Poll},
};

use futures_util::{
    future::BoxFuture,
    io::{AsyncBufRead, AsyncRead, AsyncSeek},
};

use crate::{
    async_source::RangeFetch,
    batch_fetch::{self, BatchFetch},
    fetch_dispatch,
    range_cache::seek_position,
    remote_file::{bundle_byte_count, MAX_BUNDLE_BYTES},
    BoxedAsyncBookReader, FetchFuture, RemoteFileError,
};

const CACHE_BYTES: usize = 16 * 1024 * 1024;
const PREFETCH_BYTES: usize = 8 * 1024 * 1024;
const CACHE_INTERVALS: usize = 512;
const PREFETCH_INTERVALS: usize = 256;
const MISS_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct Cache {
    intervals: VecDeque<(u64, Arc<[u8]>)>,
    bytes: usize,
}

impl Cache {
    fn insert(&mut self, offset: u64, bytes: Vec<u8>) {
        while self.bytes + bytes.len() > CACHE_BYTES || self.intervals.len() >= CACHE_INTERVALS {
            let Some((_, oldest)) = self.intervals.pop_back() else { break };
            self.bytes -= oldest.len();
        }
        self.bytes += bytes.len();
        self.intervals.push_front((offset, bytes.into()));
    }

    fn at(&self, offset: u64) -> Option<(u64, Arc<[u8]>)> {
        self.intervals.iter().find(|(start, bytes)| offset >= *start && offset - *start < bytes.len() as u64).cloned()
    }

    fn missing(&self, ranges: &[(u64, usize)]) -> Vec<(u64, usize)> {
        let mut cached: Vec<_> = self.intervals.iter().map(|(offset, bytes)| (*offset, *offset + bytes.len() as u64)).collect();
        cached.sort_unstable();
        let mut missing = Vec::new();
        for &(offset, count) in ranges {
            let end = offset + count as u64;
            let mut position = offset;
            for &(start, cached_end) in &cached {
                if cached_end <= position || start >= end {
                    continue;
                }
                if start > position {
                    missing.push((position, (start - position) as usize));
                }
                position = position.max(cached_end).min(end);
            }
            if position < end {
                missing.push((position, (end - position) as usize));
            }
        }
        missing
    }
}

#[derive(Clone)]
pub struct SparseBookSource {
    length: u64,
    cache: Arc<Mutex<Cache>>,
    fetch: RangeFetch,
    batch_fetch: Option<BatchFetch>,
}

impl SparseBookSource {
    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    pub(crate) fn supports_prefetch(&self) -> bool {
        self.batch_fetch.is_some()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn remote<F>(length: u64, seed_offset: u64, seed: Vec<u8>, fetch: F) -> io::Result<Self>
    where
        F: Fn(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send + Sync + 'static,
    {
        Self::new(length, seed_offset, seed, fetch_dispatch::native(move |(offset, count)| fetch(offset, count))?)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn browser<F>(length: u64, seed_offset: u64, seed: Vec<u8>, mut fetch: F) -> io::Result<Self>
    where
        F: FnMut(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, String>> + 'static,
    {
        Self::new(length, seed_offset, seed, fetch_dispatch::browser(move |(offset, count)| fetch(offset, count)))
    }

    fn new(length: u64, seed_offset: u64, seed: Vec<u8>, fetch: RangeFetch) -> io::Result<Self> {
        if seed.is_empty() || seed.len() > MAX_BUNDLE_BYTES || seed_offset.checked_add(seed.len() as u64).is_none_or(|end| end > length) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid sparse book seed"));
        }
        let mut cache = Cache::default();
        cache.insert(seed_offset, seed);
        Ok(Self { length, cache: Arc::new(Mutex::new(cache)), fetch, batch_fetch: None })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_remote_prefetch<F>(mut self, fetch: F) -> io::Result<Self>
    where
        F: Fn(Vec<(u64, usize)>) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send + Sync + 'static,
    {
        self.batch_fetch = Some(batch_fetch::native(fetch)?);
        Ok(self)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn with_browser_prefetch<F>(mut self, fetch: F) -> Self
    where
        F: FnMut(Vec<(u64, usize)>) -> FetchFuture<'static, Result<Vec<u8>, String>> + 'static,
    {
        self.batch_fetch = Some(batch_fetch::browser(fetch));
        self
    }

    pub(crate) async fn prefetch_cached(&self, ranges: &[(u64, usize)]) -> io::Result<()> {
        let Some(fetch) = &self.batch_fetch else { return Ok(()) };
        self.prefetch(ranges, |ranges| {
            let fetch = fetch.clone();
            Box::pin(async move { fetch(ranges).await.map_err(|error| RemoteFileError::Failed(error.to_string())) }) as BoxFuture<'static, _>
        })
        .await
    }

    pub fn reader(&self) -> BoxedAsyncBookReader {
        Box::new(Reader { source: self.clone(), position: 0, buffer_offset: 0, buffer: Arc::from([]), pending: None, next_fetch: None, batch_bytes: MISS_BYTES })
    }

    /// Optional warming stays bounded. Larger/unusual resources retain ordinary
    /// demand reads and the same revision checks as any other range request.
    pub async fn prefetch<F, Response>(&self, ranges: &[(u64, usize)], fetch: F) -> io::Result<()>
    where
        F: Fn(Vec<(u64, usize)>) -> Response,
        Response: Future<Output = Result<Vec<u8>, RemoteFileError>>,
    {
        let mut previous_end = 0;
        for &(offset, count) in ranges {
            if count == 0 || offset < previous_end || offset.checked_add(count as u64).is_none_or(|end| end > self.length) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid sparse prefetch ranges"));
            }
            previous_end = offset + count as u64;
        }
        let missing = self.cache.lock().map_err(|_| io::Error::other("book cache poisoned"))?.missing(ranges);
        let mut batch = Vec::new();
        let mut count = 0;
        let mut total = 0;
        let mut intervals = 0;
        for range in missing {
            if intervals == PREFETCH_INTERVALS {
                break;
            }
            if range.1 > MAX_BUNDLE_BYTES || total + range.1 > PREFETCH_BYTES {
                continue;
            }
            if batch.len() == 128 || count + range.1 > MAX_BUNDLE_BYTES {
                self.fetch_batch(std::mem::take(&mut batch), &fetch).await?;
                count = 0;
            }
            count += range.1;
            total += range.1;
            intervals += 1;
            batch.push(range);
        }
        if !batch.is_empty() {
            self.fetch_batch(batch, &fetch).await?;
        }
        Ok(())
    }

    async fn fetch_batch<F, Response>(&self, ranges: Vec<(u64, usize)>, fetch: &F) -> io::Result<()>
    where
        F: Fn(Vec<(u64, usize)>) -> Response,
        Response: Future<Output = Result<Vec<u8>, RemoteFileError>>,
    {
        let length = bundle_byte_count(&ranges, self.length).ok_or_else(|| io::Error::other("invalid sparse range batch"))?;
        let bytes = fetch(ranges.clone()).await.map_err(io::Error::other)?;
        if bytes.len() != length {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete sparse range batch"));
        }
        let mut cache = self.cache.lock().map_err(|_| io::Error::other("book cache poisoned"))?;
        let mut consumed = 0;
        for (offset, count) in ranges {
            cache.insert(offset, bytes[consumed..consumed + count].to_vec());
            consumed += count;
        }
        Ok(())
    }
}

struct Reader {
    source: SparseBookSource,
    position: u64,
    buffer_offset: u64,
    buffer: Arc<[u8]>,
    pending: Option<(u64, usize, BoxFuture<'static, io::Result<Vec<u8>>>)>,
    next_fetch: Option<u64>,
    batch_bytes: usize,
}

impl AsyncBufRead for Reader {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
        let this = self.get_mut();
        if this.position >= this.source.length {
            return Poll::Ready(Ok(&[]));
        }
        if !(this.position >= this.buffer_offset && this.position - this.buffer_offset < this.buffer.len() as u64) {
            if this.pending.is_some() {
                let (offset, count, result) = {
                    let (offset, count, response) = this.pending.as_mut().unwrap();
                    (*offset, *count, ready!(response.as_mut().poll(cx)))
                };
                this.pending = None;
                let bytes = result?;
                if bytes.len() != count {
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete book range")));
                }
                this.source.cache.lock().map_err(|_| io::Error::other("book cache poisoned"))?.insert(offset, bytes);
                this.next_fetch = Some(offset + count as u64);
                this.batch_bytes = count;
            }
            let cached = {
                let cache = this.source.cache.lock().map_err(|_| io::Error::other("book cache poisoned"))?;
                cache.at(this.position)
            };
            if let Some((offset, bytes)) = cached {
                this.buffer_offset = offset;
                this.buffer = bytes;
            } else {
                let end = {
                    let cache = this.source.cache.lock().map_err(|_| io::Error::other("book cache poisoned"))?;
                    cache.intervals.iter().filter_map(|(offset, _)| (*offset > this.position).then_some(*offset)).min().unwrap_or(this.source.length)
                };
                let batch = if this.next_fetch == Some(this.position) { (this.batch_bytes * 2).min(crate::range_reader::MAX_RANGE_BYTES) } else { MISS_BYTES };
                let count = (end - this.position).min(batch as u64) as usize;
                this.pending = Some((this.position, count, (this.source.fetch)((this.position, count))));
                return Pin::new(this).poll_fill_buf(cx);
            }
        }
        let start = (this.position - this.buffer_offset) as usize;
        Poll::Ready(Ok(&this.buffer[start..]))
    }

    fn consume(self: Pin<&mut Self>, count: usize) {
        self.get_mut().position += count as u64;
    }
}

impl AsyncRead for Reader {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, output: &mut [u8]) -> Poll<io::Result<usize>> {
        if output.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let buffer = ready!(self.as_mut().poll_fill_buf(cx))?;
        let count = output.len().min(buffer.len());
        output[..count].copy_from_slice(&buffer[..count]);
        self.position += count as u64;
        Poll::Ready(Ok(count))
    }
}

impl AsyncSeek for Reader {
    fn poll_seek(self: Pin<&mut Self>, _: &mut Context<'_>, from: SeekFrom) -> Poll<io::Result<u64>> {
        let this = self.get_mut();
        let position = seek_position(this.position, this.source.length, from)?;
        if position != this.position {
            this.next_fetch = None;
            this.batch_bytes = MISS_BYTES;
        }
        this.position = position;
        this.pending = None;
        Poll::Ready(Ok(this.position))
    }
}
