//! Native filesystem discovery primitives.
//!
//! This crate intentionally contains no library database owner, event sink,
//! autonomous scan loop, watcher, or retry policy. Callers supply an immutable
//! database snapshot plus a CPU host for inspection; the session conditionally
//! applies the returned data through `library-database`.
//!
//! Native filesystem scanning only; consumers gate usage to non-wasm targets.
pub mod estimate;
pub mod filesystem_scan;

pub use estimate::estimate_folders_cancellable;
