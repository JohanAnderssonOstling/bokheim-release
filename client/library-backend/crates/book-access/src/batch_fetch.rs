//! Async batch dispatch owned by the platform's existing I/O executor.

#[cfg(feature = "pdf")]
use std::io;

use crate::fetch_dispatch;

pub(crate) type BatchFetch = fetch_dispatch::Fetch<Vec<(u64, usize)>>;

#[cfg(feature = "pdf")]
pub(crate) async fn cancellable(fetch: &BatchFetch, ranges: Vec<(u64, usize)>, cancellation: &pdf_view_common::PdfPrefetchCancellation) -> io::Result<Vec<u8>> {
    use futures_util::future::{select, Either};

    if cancellation.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "PDF prefetch canceled"));
    }
    let response = fetch(ranges);
    match select(Box::pin(cancellation.cancelled()), response).await {
        Either::Left(_) => Err(io::Error::new(io::ErrorKind::Interrupted, "PDF prefetch canceled")),
        Either::Right((result, _)) => result,
    }
    // Dropping the response closes its oneshot receiver. send_response then
    // drops the HTTP future, including its response body or browser reply port.
}

#[cfg(target_arch = "wasm32")]
pub(crate) use fetch_dispatch::browser;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use fetch_dispatch::native;
