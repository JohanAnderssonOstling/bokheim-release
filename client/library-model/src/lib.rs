//! Portable library values and validation rules.
//!
//! This crate deliberately has no filesystem, synchronization, networking, UI,
//! or runtime dependencies.
//!
//! It re-exports exactly one item, `BookTocEntry`, which is defined once in
//! `book-model` and passed through here so existing paths keep working.
//! A crate that needs any other part of a book's vocabulary depends
//! on `book-model`, a taxonomy's on `subject-projection`: passing those
//! through here made every dependant look like it needed a library when
//! it needed a book, and hid which layer a type actually belongs to.

mod download;
mod import;
pub use import::{DirectoryImportProgress, ImportFileProgress, ImportFileStage};
mod view;

pub use download::{BookDownloadStatus, DownloadProgress, DownloadProgressError, DownloadState};
pub use view::*;

pub use book_model::BookTocEntry;
