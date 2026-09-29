use crate::assets::{self, BookUploadAdmission};
use crate::{sync_store, PostgresDatabase};
use server_asset_store::{AssetError, AssetKind, AssetReader, FsAssetStore, StoredAsset};
use sqlx::PgPool;
use std::sync::Arc;
use sync_common::api::assets::{BlobManifestEntry, BlobManifestResponse};
use sync_common::ContentHash;
use tokio::io::AsyncReadExt;

async fn consume_upload(mut reader: AssetReader, expected_len: u64) -> Result<(), AssetError> {
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).await.map_err(|error| AssetError::InvalidInput(error.to_string()))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > expected_len {
            return Err(AssetError::LengthMismatch { expected: expected_len, actual: total });
        }
    }
    if total != expected_len {
        return Err(AssetError::LengthMismatch { expected: expected_len, actual: total });
    }
    Ok(())
}

pub struct AssetApplication {
    pool: PgPool,
    store: Arc<FsAssetStore>,
}

struct BookReservationCleanup {
    pool: PgPool,
    reservation: Option<crate::assets::BookUploadReservation>,
}

impl BookReservationCleanup {
    fn new(pool: PgPool, reservation: crate::assets::BookUploadReservation) -> Self {
        Self { pool, reservation: Some(reservation) }
    }

    fn disarm(&mut self) {
        self.reservation = None;
    }
}

impl Drop for BookReservationCleanup {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else { return };
        let pool = self.pool.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = assets::abort_book_upload(&pool, &reservation).await;
            });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageUsage {
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub quota_bytes: u64,
}

/// One library's share of the charged bytes. The shares partition
/// [`StorageUsage::used_bytes`]; they are not independent per-library totals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryStorageUsage {
    pub library_id: sync_common::LibraryId,
    pub used_bytes: u64,
}

impl AssetApplication {
    pub fn new(database: PostgresDatabase, store: Arc<FsAssetStore>) -> Self {
        Self { pool: database.pool().clone(), store }
    }

    pub async fn thumbnail_presence(&self, user_id: &str, library_id: &sync_common::LibraryId, hashes: &[ContentHash]) -> Result<Vec<sync_common::api::assets::ThumbnailRevision>, AssetError> {
        self.require_library(user_id, library_id).await?;
        let mut present = Vec::new();
        for hash in hashes {
            if let Some(revision) = self.store.thumbnail_revision(user_id, hash).await? {
                present.push(sync_common::api::assets::ThumbnailRevision { content_hash: *hash, revision });
            }
        }
        Ok(present)
    }

    pub async fn remove_thumbnail(&self, user_id: &str, library_id: &sync_common::LibraryId, hash: &ContentHash) -> Result<(), AssetError> {
        assets::validate_thumbnail(&self.pool, user_id, library_id, hash, 0).await?;
        self.store.remove_thumbnail(user_id, hash).await
    }

    async fn require_library(&self, user_id: &str, library_id: &sync_common::LibraryId) -> Result<(), AssetError> {
        let owned = sync_store::owns_library(&self.pool, user_id, library_id).await.map_err(|error| AssetError::Storage(error.to_string()))?;
        if owned {
            Ok(())
        } else {
            Err(AssetError::Forbidden)
        }
    }

    async fn has_book_entitlement(&self, user_id: &str, hash: &ContentHash) -> Result<bool, AssetError> {
        sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1 FROM user_blob_charge
                 WHERE user_id=$1 AND content_hash=$2
                 UNION ALL
                 SELECT 1 FROM user_book_revision
                 WHERE user_id=$1 AND content_hash=$2
                 UNION ALL
                 SELECT 1 FROM user_blob_reference
                 WHERE user_id=$1 AND content_hash=$2 AND reference_count > 0
             )",
        )
        .bind(user_id)
        .bind(hash.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|error| AssetError::Storage(error.to_string()))
    }

    async fn claim_existing_book(&self, user_id: &str, library_id: &sync_common::LibraryId, hash: &ContentHash) -> Result<Option<u64>, AssetError> {
        let Some(size) = self.store.size(user_id, AssetKind::Book, hash).await? else {
            return Ok(None);
        };
        assets::claim_existing_book(&self.pool, user_id, library_id, hash, size).await?;
        Ok(Some(size))
    }

    pub async fn negotiate_books(&self, user_id: &str, library_id: &sync_common::LibraryId, blobs: Vec<BlobManifestEntry>) -> Result<BlobManifestResponse, AssetError> {
        self.require_library(user_id, library_id).await?;
        if blobs.len() > sync_common::api::assets::MAX_BLOB_MANIFEST_ENTRIES {
            return Err(AssetError::InvalidInput("book manifest is too large".into()));
        }
        let mut identities = std::collections::HashSet::new();
        let mut checksums = std::collections::HashSet::new();
        if blobs.iter().any(|blob| !identities.insert(blob.content_hash) || !checksums.insert(blob.checksum)) {
            return Err(AssetError::InvalidInput("book manifest contains duplicate identities or checksums".into()));
        }
        let phase_started = std::time::Instant::now();
        let manifest_count = blobs.len();
        let mut verified = std::collections::HashSet::new();
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        // Only inspect previously verified immutable objects, outside the writer
        // transaction. A blob appearing later falls back to upload admission.
        for blob in blobs {
            if self.store.size(user_id, AssetKind::Book, &blob.checksum).await?.is_some() {
                if self.store.book_identity(&blob.checksum).await? != blob.content_hash {
                    rejected.push(sync_common::api::assets::RejectedBlobManifestEntry {
                        content_hash: blob.content_hash,
                        reason: sync_common::api::assets::BlobManifestRejectionReason::IdentityMismatch,
                    });
                    continue;
                }
                verified.insert(blob.checksum);
            }
            accepted.push(blob);
        }
        tracing::debug!(target: "sync_performance", phase = "manifest_inspect_files", count = manifest_count, elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        let phase_started = std::time::Instant::now();
        let mut response = assets::negotiate_books(&self.pool, user_id, library_id, &accepted, &verified).await?;
        tracing::debug!(target: "sync_performance", phase = "manifest_database", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        response.rejected.extend(rejected);
        Ok(response)
    }

    pub async fn storage_usage(&self, user_id: &str) -> Result<StorageUsage, AssetError> {
        assets::storage_usage(&self.pool, user_id).await
    }

    pub async fn library_storage_usage(&self, user_id: &str) -> Result<Vec<LibraryStorageUsage>, AssetError> {
        assets::library_storage_usage(&self.pool, user_id).await
    }

    pub async fn open_from(&self, user_id: &str, library_id: &sync_common::LibraryId, kind: AssetKind, hash: &ContentHash, offset: u64) -> Result<Option<StoredAsset>, AssetError> {
        self.require_library(user_id, library_id).await?;
        let resolved_hash = if kind == AssetKind::Book { assets::book_revision(&self.pool, user_id, hash).await? } else { *hash };
        if kind == AssetKind::Book {
            if !self.has_book_entitlement(user_id, hash).await? {
                return Ok(None);
            }
            match self.claim_existing_book(user_id, library_id, &resolved_hash).await {
                Ok(Some(_)) => {}
                Ok(None) | Err(AssetError::Forbidden) => return Ok(None),
                Err(error) => return Err(error),
            }
        } else {
            let Some(size) = self.store.size(user_id, kind, hash).await? else {
                return Ok(None);
            };
            assets::validate_thumbnail(&self.pool, user_id, library_id, hash, size).await?;
        }
        self.store.open_from(user_id, kind, &resolved_hash, offset).await
    }

    pub async fn audiobook_track(&self, user_id: &str, library_id: &sync_common::LibraryId, hash: &ContentHash, index: usize) -> Result<Option<(StoredAsset, audiobook_folder::ArchivedTrack)>, AssetError> {
        let Some(asset) = self.open_from(user_id, library_id, AssetKind::Book, hash, 0).await? else { return Ok(None) };
        let track = self.store.book_tracks(&asset.checksum).await?.into_iter().nth(index);
        Ok(track.map(|track| (asset, track)))
    }

    pub async fn put_book_revision(&self, user_id: &str, library_id: &sync_common::LibraryId, identity: &ContentHash, checksum: &ContentHash, length: u64, reader: AssetReader) -> Result<(), AssetError> {
        self.require_library(user_id, library_id).await?;
        // Reserve and verify actual bytes even when an object is already owned.
        let phase_started = std::time::Instant::now();
        let admission = assets::reserve_book_upload(&self.pool, user_id, library_id, checksum, length).await?;
        tracing::debug!(target: "sync_performance", phase = "upload_reservation", library_id = %library_id, bytes = length, elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
        let mut cleanup = match &admission {
            BookUploadAdmission::Reserved(reservation) => Some(BookReservationCleanup::new(self.pool.clone(), reservation.clone())),
            BookUploadAdmission::AlreadyOwned => None,
        };
        let result = async {
            let actual = self.store.put(user_id, AssetKind::Book, checksum, Some(length), reader).await?;
            let phase_started = std::time::Instant::now();
            if self.store.book_identity(checksum).await? != *identity { return Err(AssetError::HashMismatch); }
            tracing::debug!(target: "sync_performance", phase = "verify_book_identity", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
            let phase_started = std::time::Instant::now();
            let result = assets::finish_book_revision_upload(&self.pool, user_id, library_id, identity, checksum, &admission, actual).await;
            tracing::debug!(target: "sync_performance", phase = "commit_upload_accounting", ok = result.is_ok(), elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
            result
        }.await;
        if result.is_err() {
            if let BookUploadAdmission::Reserved(reservation) = &admission {
                assets::abort_book_upload(&self.pool, reservation).await?;
            }
        }
        if let Some(cleanup) = &mut cleanup { cleanup.disarm(); }
        result
    }

    pub async fn put(&self, user_id: &str, library_id: &sync_common::LibraryId, kind: AssetKind, hash: &ContentHash, expected_len: Option<u64>, reader: AssetReader) -> Result<(), AssetError> {
        self.require_library(user_id, library_id).await?;
        let expected_len = expected_len.ok_or(AssetError::LengthRequired)?;
        if kind.max_bytes().is_some_and(|limit| expected_len > limit) {
            return Err(AssetError::TooLarge { limit: kind.max_bytes().unwrap_or(u64::MAX) });
        }
        if kind == AssetKind::ThumbnailBrowse {
            let master_size = self.store.size(user_id, AssetKind::Thumbnail, hash).await?.ok_or_else(|| AssetError::InvalidInput("browse thumbnail requires a stored source".into()))?;
            assets::validate_thumbnail(&self.pool, user_id, library_id, hash, master_size).await?;
            self.store.put(user_id, kind, hash, Some(expected_len), reader).await?;
            return Ok(());
        }
        if kind == AssetKind::Thumbnail {
            assets::validate_thumbnail_upload(&self.pool, user_id, library_id, hash, expected_len).await?;
            let actual_size = self.store.put(user_id, kind, hash, Some(expected_len), reader).await?;
            // Recheck the lifecycle after streaming, in case the book was removed.
            return assets::validate_thumbnail_upload(&self.pool, user_id, library_id, hash, actual_size).await;
        }
        let reservation = match assets::reserve_book_upload(&self.pool, user_id, library_id, hash, expected_len).await? {
            BookUploadAdmission::AlreadyOwned => return consume_upload(reader, expected_len).await,
            BookUploadAdmission::Reserved(reservation) => reservation,
        };
        match self.store.put(user_id, kind, hash, Some(expected_len), reader).await {
            Ok(actual_size) => assets::finish_book_upload(&self.pool, &reservation, actual_size).await,
            Err(error) => {
                if let Err(abort_error) = assets::abort_book_upload(&self.pool, &reservation).await {
                    return Err(AssetError::Storage(format!("{error}; additionally failed to release quota reservation: {abort_error}")));
                }
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{consume_upload, AssetApplication};
    use crate::{assets, PostgresDatabase};
    use server_asset_store::{AssetError, AssetKind, AssetReader, FsAssetStore};
    use std::sync::Arc;
    use sync_common::api::assets::BlobManifestEntry;
    use sync_common::ContentHash;

    fn upload(bytes: Vec<u8>) -> AssetReader {
        Box::new(std::io::Cursor::new(bytes))
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn mutable_book_revisions_keep_identity_verify_bytes_and_isolate_accounts() {
        let pool = crate::test_support::isolated_pool("book_revisions", 4).await.expect("test database required");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("book.m4b");
        std::fs::write(&path, include_bytes!("../../../../../client/app/tests/fixtures/embedded-cover.m4b")).unwrap();
        let identity = book_identity::ensure(&path).unwrap();
        let first = std::fs::read(&path).unwrap();
        let first_checksum = ContentHash::new(blake3::hash(&first).to_hex().as_str());
        assert_ne!(identity, first_checksum);
        let mut second = first.clone(); second.extend_from_slice(b"\0\0\0\x0cfreedata");
        let second_checksum = ContentHash::new(blake3::hash(&second).to_hex().as_str());
        let store = Arc::new(FsAssetStore::new(temp.path().join("books"), temp.path().join("thumbnails")));
        store.initialize().await.unwrap();
        let application = AssetApplication::new(PostgresDatabase::from_pool(pool.clone()), store);
        let mut accounts = Vec::new();
        for _ in 0..2 {
            let user = uuid::Uuid::new_v4().to_string(); let library = uuid::Uuid::new_v4();
            sqlx::query("INSERT INTO users(id,email) VALUES($1,$2)").bind(&user).bind(format!("{user}@example.com")).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO libraries(id,user_id,name) VALUES($1,$2,'Books')").bind(library.to_string()).bind(&user).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO user_blob_reference(user_id,content_hash,reference_count) VALUES($1,$2,1)").bind(&user).bind(identity.as_str()).execute(&pool).await.unwrap();
            accounts.push((user, library));
        }
        let (user, library) = &accounts[0];
        application.put_book_revision(user, library, &identity, &first_checksum, first.len() as u64, upload(first.clone())).await.unwrap();
        let usage = application.storage_usage(user).await.unwrap(); assert_eq!(usage.used_bytes, first.len() as u64); assert_eq!(usage.reserved_bytes, 0);
        // Identical permanent identities in another account cannot redirect ours.
        let (other_user, other_library) = &accounts[1];
        application.put_book_revision(other_user, other_library, &identity, &second_checksum, second.len() as u64, upload(second.clone())).await.unwrap();
        use tokio::io::AsyncReadExt as _;
        let mut asset = application.open_from(user, library, AssetKind::Book, &identity, 0).await.unwrap().unwrap();
        let mut bytes = Vec::new(); asset.reader.read_to_end(&mut bytes).await.unwrap(); assert_eq!(bytes, first); assert_eq!(asset.checksum, first_checksum);
        // A bad body cannot replace a good revision, including deduplicated uploads.
        assert!(matches!(application.put_book_revision(user, library, &identity, &second_checksum, first.len() as u64, upload(first.clone())).await, Err(AssetError::HashMismatch)));
        assert_eq!(application.storage_usage(user).await.unwrap().reserved_bytes, 0);
        assert!(matches!(application.put_book_revision(user, library, &second_checksum, &first_checksum, first.len() as u64, upload(first.clone())).await, Err(AssetError::HashMismatch)));
        let missing = ContentHash::new(&"f".repeat(64));
        let rejected = application.negotiate_books(user, library, vec![BlobManifestEntry {
            content_hash: second_checksum, checksum: first_checksum, size_bytes: first.len() as u64,
        }, BlobManifestEntry { content_hash: missing, checksum: missing, size_bytes: 1 }]).await.unwrap();
        assert_eq!(rejected.rejected[0].reason, sync_common::api::assets::BlobManifestRejectionReason::IdentityMismatch);
        assert_eq!(rejected.upload, vec![missing], "one invalid identity must not block unrelated uploads");
        let manifest = vec![BlobManifestEntry { content_hash: identity, checksum: second_checksum, size_bytes: second.len() as u64 }];
        assert!(matches!(application.negotiate_books(other_user, library, manifest.clone()).await, Err(AssetError::Forbidden)));
        let reused = application.negotiate_books(user, library, manifest.clone()).await.unwrap();
        assert_eq!(reused.claimed, vec![identity]);
        assert!(reused.upload.is_empty(), "reuse another account's revision without transferring bytes");
        let repeated = application.negotiate_books(user, library, manifest).await.unwrap();
        assert_eq!(repeated.owned, vec![identity]);
        let usage = application.storage_usage(user).await.unwrap(); assert_eq!(usage.used_bytes, second.len() as u64); assert_eq!(usage.reserved_bytes, 0);
        let mut asset = application.open_from(user, library, AssetKind::Book, &identity, 0).await.unwrap().unwrap();
        let mut bytes = Vec::new(); asset.reader.read_to_end(&mut bytes).await.unwrap(); assert_eq!(bytes, second); assert_eq!(asset.checksum, second_checksum);
        let mut third = second.clone(); third.extend_from_slice(b"\0\0\0\x0cfreemore");
        let third_checksum = ContentHash::new(blake3::hash(&third).to_hex().as_str());
        let (one, two) = tokio::join!(
            application.put_book_revision(user, library, &identity, &first_checksum, first.len() as u64, upload(first.clone())),
            application.put_book_revision(user, library, &identity, &third_checksum, third.len() as u64, upload(third))
        );
        one.unwrap(); two.unwrap();
        let current = application.open_from(user, library, AssetKind::Book, &identity, 0).await.unwrap().unwrap();
        assert_eq!(application.storage_usage(user).await.unwrap().used_bytes, current.total_len, "concurrent revisions must release every superseded charge");
        let charged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_blob_charge WHERE user_id=$1").bind(user).fetch_one(&pool).await.unwrap();
        assert_eq!(charged, 1);
        let mut transaction = pool.begin().await.unwrap();
        // Mirror lifecycle removal: release this library's admission claim too.
        sqlx::query(include_str!("sql/cloud_storage/delete_claim.sql")).bind(library.to_string()).bind(identity.as_str()).execute(&mut *transaction).await.unwrap();
        assets::apply_lifecycle_reference_deltas(&mut transaction, user, [(identity, -1)]).await.unwrap(); transaction.commit().await.unwrap();
        assert_eq!(application.storage_usage(user).await.unwrap().used_bytes, 0);
        assert!(application.open_from(user, library, AssetKind::Book, &identity, 0).await.unwrap().is_none());
        assert!(application.open_from(other_user, other_library, AssetKind::Book, &identity, 0).await.unwrap().is_some());
    }


    #[tokio::test]
    async fn deduplicated_uploads_are_consumed_and_length_checked() {
        assert!(consume_upload(upload(vec![0; 128 * 1024]), 128 * 1024).await.is_ok());
        assert!(matches!(consume_upload(upload(vec![0; 9]), 8).await, Err(AssetError::LengthMismatch { .. })));
        assert!(matches!(consume_upload(upload(vec![0; 7]), 8).await, Err(AssetError::LengthMismatch { .. })));
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn globally_stored_book_requires_authorized_claim_or_reference() {
        let Some(pool) = crate::test_support::isolated_pool("asset_application_access", 4).await else { return };
        let user_id = uuid::Uuid::new_v4().to_string();
        let library_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO users(id, email) VALUES ($1, $2)").bind(&user_id).bind(format!("asset-access-{}@example.com", uuid::Uuid::new_v4().simple())).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO libraries(id, user_id, name) VALUES ($1, $2, 'Books')").bind(library_id.to_string()).bind(&user_id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO user_storage_account(user_id, quota_bytes) VALUES ($1, 1024)").bind(&user_id).execute(&pool).await.unwrap();

        let bytes = b"another account's globally deduplicated book".to_vec();
        let hash = ContentHash::new(blake3::hash(&bytes).to_hex().as_str());
        let size = bytes.len() as u64;
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(FsAssetStore::new(temp.path().join("books"), temp.path().join("thumbnails")));
        store.initialize().await.unwrap();
        store.put("owner", AssetKind::Book, &hash, Some(size), upload(bytes)).await.unwrap();
        sqlx::query("INSERT INTO blob_object(content_hash, size_bytes) VALUES ($1, $2)").bind(hash.as_str()).bind(size as i64).execute(&pool).await.unwrap();

        let application = AssetApplication::new(PostgresDatabase::from_pool(pool.clone()), store);
        assert!(application.open_from(&user_id, &library_id, AssetKind::Book, &hash, 0).await.unwrap().is_none());
        let manifest = application.negotiate_books(&user_id, &library_id, vec![BlobManifestEntry { content_hash: hash.clone(), checksum: hash.clone(), size_bytes: size }]).await.unwrap();
        assert!(manifest.upload.is_empty());
        assert_eq!(manifest.claimed, vec![hash.clone()]);
        assert!(application.open_from(&user_id, &library_id, AssetKind::Book, &hash, 0).await.unwrap().is_some());
        assert_eq!(application.storage_usage(&user_id).await.unwrap().used_bytes, size);

        let mut transaction = pool.begin().await.unwrap();
        assets::apply_lifecycle_reference_deltas(&mut transaction, &user_id, [(hash.clone(), 1)]).await.unwrap();
        transaction.commit().await.unwrap();
        assert!(application.open_from(&user_id, &library_id, AssetKind::Book, &hash, 0).await.unwrap().is_some());
    }
}
