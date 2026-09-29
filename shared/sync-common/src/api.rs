//! Shared HTTP wire contracts used by both the client and server transports.
//!
//! These types deliberately contain no HTTP-framework or application logic.
//! Each boundary still authenticates, validates status codes, and maps errors
//! independently.

pub mod assets {
    use crate::ContentHash;
    use serde::{Deserialize, Serialize};
    use std::collections::HashSet;
    use std::fmt;

    pub const MAX_BOOK_BUNDLE_BYTES: usize = 4 * 1024 * 1024;
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct BookRanges {
        pub checksum: ContentHash,
        pub ranges: Vec<(u64, usize)>,
    }
    impl BookRanges {
        pub fn byte_count(&self, length: u64) -> Option<usize> {
            if self.ranges.is_empty() || self.ranges.len() > 128 {
                return None;
            }
            let mut end = 0;
            let mut total = 0usize;
            for &(offset, count) in &self.ranges {
                if count == 0 || offset < end {
                    return None;
                }
                end = offset.checked_add(count as u64)?;
                total = total.checked_add(count)?;
                if end > length || total > MAX_BOOK_BUNDLE_BYTES {
                    return None;
                }
            }
            Some(total)
        }
    }

    /// Ephemeral read capability, never part of saved playback state.
    #[derive(Clone, Serialize, Deserialize)]
    pub struct PlaybackGrant {
        pub path: String,
        pub expires_in_seconds: u32,
        pub checksum: ContentHash,
        pub length: u64,
    }
    impl fmt::Debug for PlaybackGrant {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("PlaybackGrant").field("expires_in_seconds", &self.expires_in_seconds).field("checksum", &self.checksum).finish_non_exhaustive()
        }
    }

    pub const MAX_BLOB_MANIFEST_ENTRIES: usize = 4096;
    pub const MAX_THUMBNAIL_PRESENCE_ENTRIES: usize = MAX_BLOB_MANIFEST_ENTRIES;
    pub const MAX_THUMBNAIL_BATCH_ENTRIES: usize = 16;
    pub const MAX_THUMBNAIL_BATCH_BYTES: usize = MAX_THUMBNAIL_BATCH_ENTRIES * crate::MAX_THUMBNAIL_BYTES as usize;
    pub const MAX_THUMBNAIL_BATCH_DOWNLOAD_BYTES: usize = 2 * MAX_THUMBNAIL_BATCH_BYTES + 1024 * 1024;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ThumbnailPresenceRequest {
        pub content_hashes: Vec<ContentHash>,
    }
    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ThumbnailPresenceResponse {
        pub present: Vec<ContentHash>,
        #[serde(default)]
        pub revisions: Vec<ThumbnailRevision>,
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ThumbnailRevision {
        pub content_hash: ContentHash,
        pub revision: ContentHash,
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct BlobManifestEntry {
        pub content_hash: ContentHash,
        /// Checksum of immutable bytes; may differ from permanent book identity.
        pub checksum: ContentHash,
        pub size_bytes: u64,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct BlobManifestRequest {
        pub blobs: Vec<BlobManifestEntry>,
    }

    #[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum BlobManifestRejectionReason {
        InvalidSize,
        SizeMismatch,
        IdentityMismatch,
        QuotaExceeded,
        UploadInProgress,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct RejectedBlobManifestEntry {
        pub content_hash: ContentHash,
        pub reason: BlobManifestRejectionReason,
    }

    /// Every requested hash appears exactly once across these four collections.
    /// `claimed` means a globally stored, validated blob was charged to this
    /// account atomically; `upload` means the bytes still require the ordinary
    /// quota-checked upload admission path.
    #[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
    pub struct BlobManifestResponse {
        pub owned: Vec<ContentHash>,
        pub claimed: Vec<ContentHash>,
        pub upload: Vec<ContentHash>,
        pub rejected: Vec<RejectedBlobManifestEntry>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BlobManifestValidationError {
        DuplicateRequestHash,
        UnknownOrRepeatedResponseHash,
        OmittedResponseHash,
    }

    impl fmt::Display for BlobManifestValidationError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(match self {
                Self::DuplicateRequestHash => "blob manifest contains duplicate hashes",
                Self::UnknownOrRepeatedResponseHash => "blob manifest response contains an unknown or repeated hash",
                Self::OmittedResponseHash => "blob manifest response omitted a requested hash",
            })
        }
    }

    impl std::error::Error for BlobManifestValidationError {}

    /// Ensures every requested blob is classified exactly once by the server.
    pub fn validate_blob_manifest_response(request: &BlobManifestRequest, response: &BlobManifestResponse) -> Result<(), BlobManifestValidationError> {
        let requested: HashSet<_> = request.blobs.iter().map(|blob| blob.content_hash.clone()).collect();
        if requested.len() != request.blobs.len() {
            return Err(BlobManifestValidationError::DuplicateRequestHash);
        }
        let classified = response.owned.iter().chain(&response.claimed).chain(&response.upload).chain(response.rejected.iter().map(|entry| &entry.content_hash));
        let mut unique = HashSet::with_capacity(requested.len());
        for hash in classified {
            if !requested.contains(hash) || !unique.insert(hash.clone()) {
                return Err(BlobManifestValidationError::UnknownOrRepeatedResponseHash);
            }
        }
        if unique != requested {
            return Err(BlobManifestValidationError::OmittedResponseHash);
        }
        Ok(())
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ThumbnailBatchEntry {
        pub content_hash: ContentHash,
        pub bytes: Vec<u8>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ThumbnailBatchUploadRequest {
        pub thumbnails: Vec<ThumbnailBatchEntry>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ThumbnailBatchDownloadRequest {
        pub content_hashes: Vec<ContentHash>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ThumbnailBatchDownloadResponse {
        pub thumbnails: Vec<ThumbnailBatchDownloadEntry>,
        pub missing: Vec<ContentHash>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct ThumbnailBatchDownloadEntry {
        pub content_hash: ContentHash,
        pub bytes: Vec<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub browse_bytes: Option<Vec<u8>>,
    }
}

pub mod libraries {
    use crate::LibraryId;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct LibrarySummary {
        pub library_id: LibraryId,
        pub library_name: String,
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct LibraryNameRequest {
        pub library_id: LibraryId,
        pub library_name: String,
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct CloudStorageChange {
        pub change_id: uuid::Uuid,
        pub enabled: bool,
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct LibraryCloudStorage {
        pub library_id: LibraryId,
        pub enabled: bool,
    }

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct DeleteLibraryResponse {
        pub library_id: LibraryId,
    }
}

pub mod notifications {
    use crate::LibraryId;
    use serde::{Deserialize, Serialize};

    /// Best-effort wakeups carried by the authenticated WebSocket. Durable
    /// state and cursors remain in the ordinary synchronization protocol.
    /// Empty binary frames are heartbeats; clients echo them without reconciling.
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum NotificationMessage {
        Hello { heartbeat_seconds: u64, timeout_seconds: u64 },
        LibraryChanged { library_id: LibraryId },
        LibrariesChanged,
        Reconcile,
    }
}

#[cfg(test)]
mod tests {
    use super::{assets, libraries, notifications};
    use crate::ContentHash;

    fn round_trip<T>(value: &T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let encoded = crate::transport::encode(value).unwrap();
        crate::transport::decode(&encoded, crate::transport::MAX_DECODED_REQUEST_BYTES).unwrap()
    }

    fn hash(character: char) -> ContentHash {
        ContentHash::new(&character.to_string().repeat(64))
    }

    #[test]
    fn blob_manifest_contract_classifies_each_hash_without_ambiguous_booleans() {
        let owned = hash('a');
        let upload = hash('b');
        let rejected = hash('c');
        let request = assets::BlobManifestRequest {
            blobs: vec![
                assets::BlobManifestEntry { content_hash: owned.clone(), checksum: owned.clone(), size_bytes: 41 },
                assets::BlobManifestEntry { content_hash: upload.clone(), checksum: upload.clone(), size_bytes: 42 },
                assets::BlobManifestEntry { content_hash: rejected.clone(), checksum: rejected.clone(), size_bytes: 43 },
            ],
        };
        assert_eq!(round_trip(&request), request);

        let response = assets::BlobManifestResponse {
            owned: vec![owned.clone()],
            claimed: Vec::new(),
            upload: vec![upload.clone()],
            rejected: vec![assets::RejectedBlobManifestEntry { content_hash: rejected.clone(), reason: assets::BlobManifestRejectionReason::QuotaExceeded }],
        };
        assert_eq!(round_trip(&response), response);
        assert_eq!(assets::validate_blob_manifest_response(&request, &response), Ok(()));

        let incomplete = assets::BlobManifestResponse { upload: vec![upload], ..Default::default() };
        assert_eq!(assets::validate_blob_manifest_response(&request, &incomplete), Err(assets::BlobManifestValidationError::OmittedResponseHash));
    }

    #[test]
    fn thumbnail_batch_contract_round_trips() {
        let thumbnail = assets::ThumbnailBatchEntry { content_hash: hash('d'), bytes: vec![0xff, 0xd8, 0xff] };
        let request = assets::ThumbnailBatchUploadRequest { thumbnails: vec![thumbnail.clone()] };
        assert_eq!(round_trip(&request), request);
        let response = assets::ThumbnailBatchDownloadResponse { thumbnails: vec![assets::ThumbnailBatchDownloadEntry { content_hash: thumbnail.content_hash, bytes: thumbnail.bytes, browse_bytes: Some(vec![0xff, 0xd8, 0xff]) }], missing: vec![hash('e')] };
        let encoded = crate::transport::encode(&response).unwrap();
        assert_eq!(crate::transport::decode::<assets::ThumbnailBatchDownloadResponse>(&encoded, assets::MAX_THUMBNAIL_BATCH_DOWNLOAD_BYTES).unwrap(), response);
        // Old servers omit the browse bytes; old clients ignore the new field.
        let old_response = assets::ThumbnailBatchDownloadResponse { thumbnails: vec![assets::ThumbnailBatchDownloadEntry { content_hash: hash('f'), bytes: vec![0xff, 0xd8, 0xff], browse_bytes: None }], missing: vec![] };
        let encoded_old = crate::transport::encode(&old_response).unwrap();
        assert_eq!(crate::transport::decode::<assets::ThumbnailBatchDownloadResponse>(&encoded_old, assets::MAX_THUMBNAIL_BATCH_DOWNLOAD_BYTES).unwrap(), old_response);
        #[derive(serde::Deserialize)]
        struct OldEntry { content_hash: crate::ContentHash, bytes: Vec<u8> }
        #[derive(serde::Deserialize)]
        struct OldResponse { thumbnails: Vec<OldEntry>, missing: Vec<crate::ContentHash> }
        let old_client: OldResponse = crate::transport::decode(&encoded, assets::MAX_THUMBNAIL_BATCH_DOWNLOAD_BYTES).unwrap();
        assert_eq!(old_client.thumbnails[0].content_hash, hash('d'));
        assert_eq!(old_client.thumbnails[0].bytes, vec![0xff, 0xd8, 0xff]);
        assert_eq!(old_client.missing, vec![hash('e')]);
    }

    #[test]
    fn library_contract_preserves_stable_ids_and_names() {
        let request = libraries::LibraryNameRequest { library_id: crate::LibraryId::parse_str("11111111-1111-4111-8111-111111111111").unwrap(), library_name: "History".to_string() };
        let summary = libraries::LibrarySummary { library_id: request.library_id.clone(), library_name: request.library_name.clone() };
        assert_eq!(round_trip(&request), request);
        assert_eq!(round_trip(&summary), summary);
    }

    #[test]
    fn notification_contract_is_granular_without_carrying_state() {
        let message = notifications::NotificationMessage::LibraryChanged { library_id: crate::LibraryId::parse_str("11111111-1111-4111-8111-111111111111").unwrap() };
        assert_eq!(round_trip(&message), message);
        let encoded = crate::transport::encode(&notifications::NotificationMessage::LibrariesChanged).unwrap();
        assert_eq!(crate::transport::decode::<notifications::NotificationMessage>(&encoded, 1024).unwrap(), notifications::NotificationMessage::LibrariesChanged);
    }
}
