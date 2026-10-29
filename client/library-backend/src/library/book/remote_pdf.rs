//! Only immutable, matching PDF metadata may drive range prefetch.

use book_access::BookId;
use pdf_view_common::{PdfDependencyIndex, PdfReaderMetadata};

pub(super) fn index(metadata: Option<&PdfReaderMetadata>, hash: BookId, length: u64) -> Option<PdfDependencyIndex> {
    let metadata = metadata.filter(|metadata| metadata.valid_for(&hash.to_string()))?;
    metadata.dependencies.as_ref().filter(|index| index.length == length).cloned()
}
