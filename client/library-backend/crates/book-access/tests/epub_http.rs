//! Real server byte-response modules exercised with the desktop book reader.
#![cfg(not(target_arch = "wasm32"))]
#[path = "../../../../../servers/sync/crates/server-http/src/bandwidth.rs"]
mod bandwidth;
#[path = "../../../../../servers/sync/crates/server-http/src/assets/ranges.rs"]
mod ranges;
use bandwidth::TransferGovernor;
use ranges::stream_asset;
#[path = "epub_http/opening.rs"]
mod opening;

#[path = "epub_http/other_formats.rs"]
mod other_formats;

#[cfg(feature = "pdf")]
#[path = "epub_http/pdf_cancellation.rs"]
mod pdf_cancellation;
