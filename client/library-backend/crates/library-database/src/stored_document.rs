//! Whole-document projections use the shared persisted codec. Older caches
//! retain JSON until the corresponding record is projected again.

use crate::DatabaseError;
use serde::{de::DeserializeOwned, Serialize};
use sync_common::wire;

pub(crate) fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, DatabaseError> {
    wire::encode(value).map_err(DatabaseError::operation)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, DatabaseError> {
    if bytes.starts_with(b"BKHM") {
        wire::decode(bytes, wire::MAX_DECODED_RESPONSE_BYTES).map_err(DatabaseError::operation)
    } else {
        serde_json::from_slice(bytes).map_err(DatabaseError::operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::BookTocEntry;
    use pdf_view_common::{PdfReaderMetadata, PdfStoredPage};

    #[test]
    fn binary_and_legacy_json_preserve_navigation_and_pdf_documents() {
        let toc = vec![BookTocEntry { title: "Part I".into(), target: "part.xhtml".into(), children: vec![BookTocEntry::default()] }];
        for bytes in [encode(&toc).unwrap(), serde_json::to_vec(&toc).unwrap()] {
            assert_eq!(decode::<Vec<BookTocEntry>>(&bytes).unwrap(), toc);
        }
        let pdf = PdfReaderMetadata { version: 1, content_hash: "a".repeat(64), pages: vec![PdfStoredPage { width: 595.0, height: 842.0, rotation: 0 }; 100], skippable_pages: Some(vec![1, 3]), annotations: vec![], dependencies: None };
        let binary = encode(&pdf).unwrap();
        let json = serde_json::to_vec(&pdf).unwrap();
        assert!(binary.len() * 3 < json.len());
        assert_eq!(decode::<PdfReaderMetadata>(&binary).unwrap(), pdf);
        assert_eq!(decode::<PdfReaderMetadata>(&json).unwrap(), pdf);
        let mut corrupt = binary;
        corrupt.push(0);
        assert!(decode::<PdfReaderMetadata>(&corrupt).is_err());
    }
}
