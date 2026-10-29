//! Synchronous parser adapters over the shared async range reader. JS futures
//! stay on their event loop; parser workers wait using shared-memory wakeups.

use crate::range_reader::{RangeFetcher, RangeReader};
use crate::{AsyncBookSource, BoxedAsyncBookReader, BoxedBookReader, BundleFetch, BundleFetcher, FetchFuture};
use client_platform_web::signal::Signal;
use client_platform_web::transport::range::Source;
use futures_util::io::{AsyncReadExt, AsyncSeekExt, BufReader};
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom};
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;
use sync_wrapper::SyncWrapper;
use wasm_bindgen::prelude::*;

struct WorkerWake(Arc<Signal>);
impl Wake for WorkerWake {
    fn wake(self: Arc<Self>) {
        self.0.notify();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify();
    }
}

/// Poll Rust futures on a parser worker. Observing before polling prevents a
/// completion between the poll and wait from being lost.
fn wait_on_worker<T>(future: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    let ready = Arc::new(Signal::default());
    let waker = Waker::from(Arc::new(WorkerWake(ready.clone())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        let observed = ready.observed();
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => return result,
            Poll::Pending => {
                if ready.wait(observed, Duration::from_secs(35)).map_err(io::Error::other)? == "timed-out" {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "book read timed out"));
                }
            }
        }
    }
}

pub struct BrowserReader {
    reader: SyncWrapper<BoxedAsyncBookReader>,
    // Direct worker capabilities must be checked even when bytes are cached.
    source: Option<Arc<Source>>,
}
impl BrowserReader {
    pub fn new(source: JsValue) -> io::Result<Self> {
        let source = Arc::new(Source::new(source).map_err(io::Error::other)?);
        let capability = source.clone();
        let fetch: RangeFetcher = Box::new(move |offset, length| {
            let result = capability.read(offset, length as u32);
            Box::pin(async move { result })
        });
        let reader = Box::new(BufReader::new(RangeReader::new(source.length(), Vec::new(), fetch)));
        Ok(Self { reader: SyncWrapper::new(reader), source: Some(source) })
    }
}
impl Read for BrowserReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if let Some(source) = &self.source {
            source.check()?;
        }
        wait_on_worker(self.reader.get_mut().read(bytes))
    }
}
impl Seek for BrowserReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        wait_on_worker(self.reader.get_mut().seek(from))
    }
}

pub fn browser_reader<F>(length: u64, prefix: Vec<u8>, fetch: F) -> io::Result<BoxedBookReader>
where
    F: FnMut(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, String>> + 'static,
{
    Signal::check().map_err(io::Error::other)?;
    let source = AsyncBookSource::browser(length, prefix, fetch)?;
    Ok(Box::new(BrowserReader { reader: SyncWrapper::new(source.reader()), source: None }))
}

pub fn browser_bundle_fetch<F>(fetch: F) -> io::Result<BundleFetch>
where
    F: FnMut(Vec<(u64, usize)>) -> FetchFuture<'static, Result<Vec<u8>, String>> + 'static,
{
    Signal::check().map_err(io::Error::other)?;
    let fetch = crate::batch_fetch::browser(fetch);
    Ok(Box::new(BrowserBundleFetch(fetch)))
}

struct BrowserBundleFetch(crate::batch_fetch::BatchFetch);

impl BundleFetcher for BrowserBundleFetch {
    fn fetch(&mut self, ranges: Vec<(u64, usize)>) -> io::Result<Vec<u8>> {
        wait_on_worker((self.0)(ranges))
    }

    #[cfg(feature = "pdf")]
    fn fetch_cancellable(&mut self, ranges: Vec<(u64, usize)>, cancellation: &pdf_view_common::PdfPrefetchCancellation) -> io::Result<Vec<u8>> {
        wait_on_worker(crate::batch_fetch::cancellable(&self.0, ranges, cancellation))
    }
}
