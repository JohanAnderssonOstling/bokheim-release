//! Native parser bridge borrowing the application’s I/O runtime.

use super::{RemoteFileError, REQUEST_TIMEOUT};
use crate::range_reader::{validate_prefix, RangeFetcher, RangeReader};
use crate::{BoxedBookReader, BundleFetch, BundleFetcher, FetchFuture};
use std::{
    io::{self, Read, Seek, SeekFrom},
    time::Duration,
};
use sync_wrapper::SyncWrapper;
use tokio::runtime::Handle;
use tokio_util::{
    compat::{Compat, FuturesAsyncReadCompatExt},
    io::SyncIoBridge,
};

type AsyncFetch = Box<dyn FnMut(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send>;

struct NativeReader {
    // The synchronous parser has exclusive access when reading or seeking.
    // SyncWrapper satisfies BoxedBookReader's Sync bound without a lock.
    bridge: SyncWrapper<SyncIoBridge<Compat<RangeReader>>>,
}

impl Read for NativeReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.bridge.get_mut().read(output)
    }
}

impl Seek for NativeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.bridge.get_mut().seek(from)
    }
}

/// Construct on the application's runtime; perform synchronous reads on a
/// parser thread. The application must keep its runtime alive while reading.
pub fn native_reader<F>(length: u64, prefix: Vec<u8>, fetch: F) -> io::Result<BoxedBookReader>
where
    F: FnMut(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send + 'static,
{
    open(length, prefix, Box::new(fetch), REQUEST_TIMEOUT)
}

/// Construct on the I/O runtime, then invoke only from a parser thread.
pub fn native_bundle_fetch<F>(fetch: F) -> io::Result<BundleFetch>
where
    F: Fn(Vec<(u64, usize)>) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send + Sync + 'static,
{
    let handle = Handle::try_current().map_err(io::Error::other)?;
    let fetch = crate::batch_fetch::native(fetch)?;
    Ok(Box::new(NativeBundleFetch { handle, fetch }))
}

struct NativeBundleFetch {
    handle: Handle,
    fetch: crate::batch_fetch::BatchFetch,
}

impl BundleFetcher for NativeBundleFetch {
    fn fetch(&mut self, ranges: Vec<(u64, usize)>) -> io::Result<Vec<u8>> {
        self.handle.block_on((self.fetch)(ranges))
    }

    #[cfg(feature = "pdf")]
    fn fetch_cancellable(&mut self, ranges: Vec<(u64, usize)>, cancellation: &pdf_view_common::PdfPrefetchCancellation) -> io::Result<Vec<u8>> {
        self.handle.block_on(crate::batch_fetch::cancellable(&self.fetch, ranges, cancellation))
    }
}

fn open(length: u64, prefix: Vec<u8>, mut fetch: AsyncFetch, timeout: Duration) -> io::Result<BoxedBookReader> {
    validate_prefix(length, &prefix)?;
    let handle = Handle::try_current().map_err(io::Error::other)?;
    let fetch: RangeFetcher = Box::new(move |offset, length| {
        let response = fetch(offset, length);
        Box::pin(async move { tokio::time::timeout(timeout, response).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "remote file read timed out"))?.map_err(io::Error::other) })
    });
    let reader = RangeReader::new(length, prefix, fetch);
    Ok(blocking_reader(reader, handle))
}

pub(super) fn blocking_reader(reader: RangeReader, handle: Handle) -> BoxedBookReader {
    let bridge = SyncIoBridge::new_with_handle(reader.compat(), handle);
    Box::new(NativeReader { bridge: SyncWrapper::new(bridge) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_reader::BLOCK_BYTES;
    use std::sync::{Arc, Mutex};

    #[test]
    fn seeks_reuse_cached_blocks_and_read_ahead_grows() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let _context = runtime.enter();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let length = 8 * BLOCK_BYTES as u64 + 3;
        let mut reader = native_reader(length, vec![0; BLOCK_BYTES], move |offset, length| {
            recorded.lock().unwrap().push((offset, length));
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok((offset..offset + length as u64).map(|n| (n / BLOCK_BYTES as u64) as u8).collect())
            })
        })
        .unwrap();
        let mut bytes = vec![0; 4 * BLOCK_BYTES];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(*requests.lock().unwrap(), vec![(BLOCK_BYTES as u64, BLOCK_BYTES), (2 * BLOCK_BYTES as u64, 2 * BLOCK_BYTES)]);
        assert!(bytes.chunks(BLOCK_BYTES).enumerate().all(|(i, block)| block.iter().all(|&b| b == i as u8)));
        reader.seek(SeekFrom::Start(BLOCK_BYTES as u64 + 7)).unwrap();
        reader.read_exact(&mut bytes[..3]).unwrap();
        assert_eq!(&bytes[..3], &[1; 3]);
        assert_eq!(requests.lock().unwrap().len(), 2);
        reader.seek(SeekFrom::End(-3)).unwrap();
        reader.read_exact(&mut bytes[..3]).unwrap();
        assert_eq!(&bytes[..3], &[8; 3]);
        assert_eq!(reader.read(&mut bytes[..1]).unwrap(), 0);
        assert!(reader.seek(SeekFrom::Start(0)).is_ok());
        assert_eq!(reader.seek(SeekFrom::Current(-1)).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn incomplete_response_does_not_advance_position_or_poison_cache() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let _context = runtime.enter();
        let mut calls = 0;
        let mut reader = native_reader(2 * BLOCK_BYTES as u64, vec![0; BLOCK_BYTES], move |_, length| {
            calls += 1;
            let count = if calls == 1 { length - 1 } else { length };
            Box::pin(async move { Ok(vec![9; count]) })
        })
        .unwrap();
        reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
        assert_eq!(reader.read(&mut [0]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(reader.stream_position().unwrap(), BLOCK_BYTES as u64);
        let mut bytes = [0];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [9]);
    }

    #[test]
    fn stalled_fetch_times_out_and_can_be_retried() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let _context = runtime.enter();
        let mut calls = 0;
        let mut reader = open(
            2 * BLOCK_BYTES as u64,
            vec![0; BLOCK_BYTES],
            Box::new(move |_, length| {
                calls += 1;
                let stall = calls == 1;
                Box::pin(async move {
                    if stall {
                        std::future::pending::<()>().await;
                    }
                    Ok(vec![5; length])
                })
            }),
            Duration::from_millis(10),
        )
        .unwrap();
        reader.seek(SeekFrom::Start(BLOCK_BYTES as u64)).unwrap();
        assert_eq!(reader.read(&mut [0]).unwrap_err().kind(), io::ErrorKind::TimedOut);
        let mut bytes = [0];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [5]);
    }

    #[tokio::test]
    async fn construction_and_drop_are_safe_inside_async_context() {
        let reader = native_reader(BLOCK_BYTES as u64, vec![0; BLOCK_BYTES], |_, _| Box::pin(async { panic!("prefix should be cached") })).unwrap();
        drop(reader);
    }

    #[cfg(feature = "pdf")]
    #[tokio::test]
    async fn cancelling_a_bundle_wakes_the_parser_and_drops_a_stalled_transport() {
        use pdf_view_common::PdfPrefetchCancellation;
        use tokio::sync::Notify;

        struct TransportLifetime(Arc<Notify>);
        impl Drop for TransportLifetime {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(Notify::new());
        let (transport_started, transport_dropped) = (started.clone(), dropped.clone());
        let mut fetch = native_bundle_fetch(move |_| {
            let (started, dropped) = (transport_started.clone(), transport_dropped.clone());
            Box::pin(async move {
                let _owner = TransportLifetime(dropped);
                started.notify_one();
                std::future::pending().await
            })
        })
        .unwrap();
        let cancellation = PdfPrefetchCancellation::default();
        let pending = cancellation.clone();
        let parser = tokio::task::spawn_blocking(move || fetch.fetch_cancellable(vec![(0, 64)], &pending));
        tokio::time::timeout(Duration::from_secs(3), started.notified()).await.unwrap();
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(3), parser).await.unwrap().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        tokio::time::timeout(Duration::from_secs(3), dropped.notified()).await.expect("the I/O task must drop its stalled transport after the parser releases the response");
    }
}
