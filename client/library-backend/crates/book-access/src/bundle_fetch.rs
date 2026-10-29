use std::io;

#[cfg(feature = "pdf")]
use pdf_view_common::PdfPrefetchCancellation;

pub type BundleFetch = Box<dyn BundleFetcher>;

/// Synchronous parser access to the platform's async batch transport.
pub trait BundleFetcher: Send + Sync {
    fn fetch(&mut self, ranges: Vec<(u64, usize)>) -> io::Result<Vec<u8>>;

    #[cfg(feature = "pdf")]
    fn fetch_cancellable(&mut self, ranges: Vec<(u64, usize)>, cancellation: &PdfPrefetchCancellation) -> io::Result<Vec<u8>> {
        if cancellation.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "PDF prefetch canceled"));
        }
        self.fetch(ranges)
    }
}

impl<F> BundleFetcher for F
where
    F: FnMut(Vec<(u64, usize)>) -> io::Result<Vec<u8>> + Send + Sync,
{
    fn fetch(&mut self, ranges: Vec<(u64, usize)>) -> io::Result<Vec<u8>> {
        self(ranges)
    }
}
