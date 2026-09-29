# Book access

Revision-pinned range reads and bounded caches for book bytes. Applications
supply URLs and credentials for HTTP operations, or authenticated fetch callbacks
for seekable readers. This crate has no backend or account-client dependency. Timers use the shared
`client-platform-runtime` executor.

- `remote_file/`: HTTP range validation, native and browser reader bridges, and
  native audiobook pause/seek/retry control.
- `range_cache.rs`: bounded in-memory fragments for one immutable revision.
- `pdf_bundle.rs` (`pdf` feature): shared page-warmer/parser cache.
- `cache/` (`sqlite-cache` feature): persistent revision checks, eviction, and
  complete-block fetching. SQL lives in `cache/sql/`.
- `storage.rs`: host interfaces for prioritized SQLite commands and background
  tasks. Backend adapters keep application queue ownership outside this crate.

Validate with `cargo test --release -p book-access --all-features --lib`.
