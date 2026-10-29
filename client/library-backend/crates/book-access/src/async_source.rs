//! Independent async cursors over one immutable book revision.

use std::io;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::io::{AsyncBufRead, AsyncSeek, BufReader};

use crate::fetch_dispatch;
use crate::range_reader::{validate_prefix, RangeReader};
use crate::SparseBookSource;

pub trait AsyncBookReader: AsyncBufRead + AsyncSeek + Unpin + Send {}
impl<T: AsyncBufRead + AsyncSeek + Unpin + Send> AsyncBookReader for T {}
pub type BoxedAsyncBookReader = Box<dyn AsyncBookReader>;
pub(crate) type RangeFetch = fetch_dispatch::Fetch<(u64, usize)>;

/// A cheap, cloneable source with independent cursor positions. Range sources
/// keep per-reader block caches; prewarmed sparse sources share exact intervals.
/// Fetches must refer to the same revision for the source lifetime.
#[derive(Clone)]
pub struct AsyncBookSource {
    length: u64,
    mode: ReaderMode,
}

#[derive(Clone)]
enum ReaderMode {
    Range { prefix: Arc<Vec<u8>>, fetch: RangeFetch },
    Sparse(SparseBookSource),
}

impl From<SparseBookSource> for AsyncBookSource {
    fn from(source: SparseBookSource) -> Self {
        Self { length: source.length(), mode: ReaderMode::Sparse(source) }
    }
}

impl AsyncBookSource {
    pub fn from_ranges<F>(length: u64, prefix: Vec<u8>, fetch: F) -> io::Result<Self>
    where
        F: Fn(u64, usize) -> BoxFuture<'static, io::Result<Vec<u8>>> + Send + Sync + 'static,
    {
        Self::from_fetch(length, prefix, Arc::new(move |(offset, length)| fetch(offset, length)))
    }

    fn from_fetch(length: u64, prefix: Vec<u8>, fetch: RangeFetch) -> io::Result<Self> {
        if !prefix.is_empty() {
            validate_prefix(length, &prefix)?;
        }
        Ok(Self { length, mode: ReaderMode::Range { prefix: Arc::new(prefix), fetch } })
    }

    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn supports_prefetch(&self) -> bool {
        matches!(&self.mode, ReaderMode::Sparse(source) if source.supports_prefetch())
    }

    /// Warm exact ranges when the source supports batching; ordinary local and
    /// external range readers keep their existing demand-read behavior.
    pub async fn prefetch(&self, ranges: Vec<(u64, usize)>) -> io::Result<()> {
        match &self.mode {
            ReaderMode::Sparse(source) => source.prefetch_cached(&ranges).await,
            ReaderMode::Range { .. } => Ok(()),
        }
    }

    pub fn reader(&self) -> BoxedAsyncBookReader {
        match &self.mode {
            ReaderMode::Range { prefix, fetch } => {
                let fetch = fetch.clone();
                Box::new(BufReader::new(RangeReader::new(self.length, prefix.as_ref().clone(), Box::new(move |offset, length| fetch((offset, length))))))
            }
            ReaderMode::Sparse(source) => source.reader(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use crate::{FetchFuture, RemoteFileError};
    use std::fs::File;
    #[cfg(unix)]
    use std::os::unix::fs::FileExt;
    #[cfg(windows)]
    use std::os::windows::fs::FileExt;
    use tokio::runtime::Handle;

    impl AsyncBookSource {
        /// Reuse the caller's I/O runtime. Polling the resulting reader does not
        /// require a Tokio context and never creates a runtime of its own.
        pub fn remote<F>(length: u64, prefix: Vec<u8>, fetch: F) -> io::Result<Self>
        where
            F: Fn(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> + Send + Sync + 'static,
        {
            Self::from_fetch(length, prefix, fetch_dispatch::native(move |(offset, count)| fetch(offset, count))?)
        }

        /// Positional reads on a pinned file handle, using the existing blocking
        /// pool. Independent ZIP cursors never share a mutable file position.
        pub fn from_file(file: File) -> io::Result<Self> {
            let length = file.metadata()?.len();
            let file = Arc::new(file);
            let handle = Handle::try_current().map_err(io::Error::other)?;
            Self::from_ranges(length, Vec::new(), move |offset, length| {
                let file = file.clone();
                let task = handle.spawn_blocking(move || {
                    let mut bytes = vec![0; length];
                    let mut read = 0;
                    while read < length {
                        #[cfg(unix)]
                        let result = file.read_at(&mut bytes[read..], offset + read as u64);
                        #[cfg(windows)]
                        let result = file.seek_read(&mut bytes[read..], offset + read as u64);
                        match result {
                            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "book file was truncated")),
                            Ok(count) => read += count,
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                            Err(error) => return Err(error),
                        }
                    }
                    Ok(bytes)
                });
                Box::pin(async move { task.await.map_err(io::Error::other)? })
            })
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use crate::FetchFuture;

    impl AsyncBookSource {
        /// Keep JS futures on the owning event loop. Readers on background
        /// workers await channel replies; they never block or poll a JS future.
        pub fn browser<F>(length: u64, prefix: Vec<u8>, mut fetch: F) -> io::Result<Self>
        where
            F: FnMut(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, String>> + 'static,
        {
            Self::from_fetch(length, prefix, fetch_dispatch::browser(move |(offset, count)| fetch(offset, count)))
        }
    }
}
