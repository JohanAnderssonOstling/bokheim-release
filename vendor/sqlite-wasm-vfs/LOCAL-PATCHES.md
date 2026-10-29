# Local changes to sqlite-wasm-vfs 0.2.0

The OPFS synchronous access-handle VFS counts open SQLite handles per filename.
Upstream tracks only a set of names: closing one of several read-only
connections removes the name while other connections still use it. That breaks
the pause guard and triggers a debug assertion when another connection closes.

`src/open_files.rs` provides reference counts used by `xOpen` and `xClose`.
The file can be tested independently with `rustc --test` on the native host.
