//! Container-neutral book adapters for the native HTML renderer.
//!
//! The adapters expose source formats as stable virtual documents and resources;
//! they never create an intermediate EPUB.

mod formats;

pub use formats::{BookLayout, BookSource, BookSourceFormat};
