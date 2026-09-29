//! The PDF reader surface.

pub(crate) mod marks;
pub(crate) mod render;
pub(crate) mod search;
pub(crate) mod state;
pub(crate) mod toc;

pub(crate) use render::{PdfReaderView, bind_pdf_keys};
