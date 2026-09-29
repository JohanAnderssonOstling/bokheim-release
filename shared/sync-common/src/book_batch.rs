//! Streaming book upload framing: u32 big-endian manifest length, encoded
//! BlobManifestRequest, then each book's exact bytes in manifest order.
use crate::api::assets::BlobManifestEntry;
use crate::ContentHash;
use serde::{Deserialize, Serialize};

pub const MEDIA_TYPE: &str = "application/vnd.bokheim.book-batch.v1";
pub const MAX_BOOKS: usize = 16;
pub const MAX_BOOK_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_BATCH_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024;

pub fn payload_bytes(entries: &[BlobManifestEntry]) -> Option<u64> {
    if entries.is_empty() || entries.len() > MAX_BOOKS {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for entry in entries {
        if entry.size_bytes == 0 || entry.size_bytes > MAX_BOOK_BYTES || !seen.insert(entry.content_hash) {
            return None;
        }
        total = total.checked_add(entry.size_bytes)?;
        if total > MAX_BATCH_BYTES {
            return None;
        }
    }
    Some(total)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadResult {
    pub content_hash: ContentHash,
    pub status: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadResponse {
    pub results: Vec<UploadResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(n: usize, size_bytes: u64) -> BlobManifestEntry {
        let hash = ContentHash::new(&format!("{n:064x}"));
        BlobManifestEntry { content_hash: hash, checksum: hash, size_bytes }
    }
    #[test]
    fn limits_cover_count_bytes_and_duplicate_identities() {
        assert_eq!(payload_bytes(&(0..4).map(|n| entry(n, MAX_BOOK_BYTES)).collect::<Vec<_>>()), Some(MAX_BATCH_BYTES));
        assert_eq!(payload_bytes(&(0..5).map(|n| entry(n, MAX_BOOK_BYTES)).collect::<Vec<_>>()), None);
        assert_eq!(payload_bytes(&(0..17).map(|n| entry(n, 1)).collect::<Vec<_>>()), None);
        assert_eq!(payload_bytes(&[entry(1, 0)]), None);
        assert_eq!(payload_bytes(&[entry(1, MAX_BOOK_BYTES + 1)]), None);
        assert_eq!(payload_bytes(&[entry(1, 1), entry(1, 1)]), None);
        assert_eq!(payload_bytes(&[]), None);
    }
}
