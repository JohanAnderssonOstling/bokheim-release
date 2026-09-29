use server_asset_store::AssetError;
use sqlx::{PgPool, Postgres, Row, Transaction};
use sync_common::api::assets::{BlobManifestEntry, BlobManifestRejectionReason, BlobManifestResponse, RejectedBlobManifestEntry};
use sync_common::{ContentHash, LibraryId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BookUploadReservation {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) library_id: LibraryId,
    pub(crate) hash: ContentHash,
    pub(crate) expected_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BookUploadAdmission {
    AlreadyOwned,
    Reserved(BookUploadReservation),
}

// Five minutes of grace keeps an active upload from racing reservation expiry
// at its HTTP deadline.
const UPLOAD_RESERVATION_TTL_SECONDS: f64 = (server_asset_store::MAX_BOOK_UPLOAD_DURATION.as_secs() + 5 * 60) as f64;

#[derive(Clone, Copy)]
struct AccountSnapshot {
    limit: i64,
    used: i64,
    reserved: i64,
}

impl AccountSnapshot {
    fn quota_exceeded(self, requested: u64) -> AssetError {
        AssetError::QuotaExceeded { limit: self.limit.max(0) as u64, used: self.used.max(0) as u64, reserved: self.reserved.max(0) as u64, requested }
    }
}

fn storage(error: impl std::fmt::Display) -> AssetError {
    AssetError::Storage(error.to_string())
}

pub(crate) async fn configure_user_storage_quota(database: &crate::PostgresDatabase, user_id: &str, quota_bytes: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO user_storage_account(user_id, quota_bytes) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET quota_bytes=EXCLUDED.quota_bytes
         RETURNING quota_bytes",
    )
    .bind(user_id)
    .bind(quota_bytes)
    .fetch_one(database.pool())
    .await
}

fn size_i64(size: u64) -> Result<i64, AssetError> {
    i64::try_from(size).map_err(|_| AssetError::Storage("asset size does not fit the quota ledger".to_string()))
}

async fn require_library_owner(transaction: &mut Transaction<'_, Postgres>, user_id: &str, library_id: &LibraryId) -> Result<(), AssetError> {
    let library_id = library_id.to_string();
    let authorized = sqlx::query_scalar::<_, bool>(include_str!("sql/assets/library_owned_by_user.sql")).bind(library_id).bind(user_id).fetch_one(&mut **transaction).await.map_err(storage)?;
    if authorized {
        Ok(())
    } else {
        Err(AssetError::Forbidden)
    }
}

async fn require_cloud_storage(transaction: &mut Transaction<'_, Postgres>, user: &str, library: &LibraryId) -> Result<(), AssetError> {
    let enabled: Option<bool> = sqlx::query_scalar(include_str!("sql/cloud_storage/require_enabled.sql")).bind(library.to_string()).bind(user).fetch_optional(&mut **transaction).await.map_err(storage)?;
    if enabled == Some(true) { Ok(()) } else { Err(AssetError::Forbidden) }
}

async fn account_snapshot(transaction: &mut Transaction<'_, Postgres>, user_id: &str) -> Result<AccountSnapshot, AssetError> {
    let phase_started = std::time::Instant::now();
    sqlx::query_file!("src/sql/account/ensure_exists.sql", user_id).execute(&mut **transaction).await.map_err(storage)?;
    let row = sqlx::query(include_str!("sql/account/select.sql")).bind(user_id).fetch_one(&mut **transaction).await.map_err(storage)?;
    tracing::debug!(target: "sync_performance", phase = "account_ensure_snapshot", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
    Ok(AccountSnapshot { limit: row.try_get("quota_bytes").map_err(storage)?, used: row.try_get("used_bytes").map_err(storage)?, reserved: row.try_get("reserved_bytes").map_err(storage)? })
}

async fn expire_reservations(transaction: &mut Transaction<'_, Postgres>, user_id: &str) -> Result<i64, AssetError> {
    let released: i64 = sqlx::query_scalar(include_str!("sql/assets/expire_reservations.sql")).bind(user_id).fetch_one(&mut **transaction).await.map_err(storage)?;
    if released > 0 {
        sqlx::query_file!("src/sql/assets/adjust_reserved.sql", user_id, -released).execute(&mut **transaction).await.map_err(storage)?;
    }
    Ok(released)
}

async fn begin_authorized_transaction<'a>(pool: &'a PgPool, user_id: &str, library_id: &LibraryId) -> Result<Transaction<'a, Postgres>, AssetError> {
    let phase_started = std::time::Instant::now();
    let mut transaction = pool.begin().await.map_err(storage)?;
    tracing::debug!(target: "sync_performance", phase = "asset_pool_acquire_and_begin", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
    let phase_started = std::time::Instant::now();
    require_library_owner(&mut transaction, user_id, library_id).await?;
    tracing::debug!(target: "sync_performance", phase = "asset_library_authorize", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
    Ok(transaction)
}

async fn prepare_account(transaction: &mut Transaction<'_, Postgres>, user_id: &str) -> Result<AccountSnapshot, AssetError> {
    let mut account = account_snapshot(transaction, user_id).await?;
    account.reserved -= expire_reservations(transaction, user_id).await?;
    Ok(account)
}

pub(super) async fn storage_usage(pool: &PgPool, user_id: &str) -> Result<crate::StorageUsage, AssetError> {
    let mut transaction = pool.begin().await.map_err(storage)?;
    let account = prepare_account(&mut transaction, user_id).await?;
    transaction.commit().await.map_err(storage)?;
    Ok(crate::StorageUsage { used_bytes: account.used.max(0) as u64, reserved_bytes: account.reserved.max(0) as u64, quota_bytes: account.limit.max(0) as u64 })
}

/// Splits the charged bytes across the libraries that hold them.
///
/// Attribution is a partition, not a per-library total: see the query for why a
/// deduplicated quota cannot be summed per library. Libraries holding nothing
/// are absent rather than reported as zero; the caller knows which libraries
/// exist and the client renders the rest.
pub(super) async fn library_storage_usage(pool: &PgPool, user_id: &str) -> Result<Vec<crate::LibraryStorageUsage>, AssetError> {
    let rows = sqlx::query(include_str!("sql/assets/library_storage_usage.sql")).bind(user_id).fetch_all(pool).await.map_err(storage)?;
    rows.into_iter()
        .map(|row| {
            let library_id: String = row.try_get("library_id").map_err(storage)?;
            let used_bytes: i64 = row.try_get("used_bytes").map_err(storage)?;
            let library_id = library_id.parse::<LibraryId>().map_err(|error| AssetError::Storage(format!("stored library id {library_id} is not a valid identifier: {error}")))?;
            Ok(crate::LibraryStorageUsage { library_id, used_bytes: used_bytes.max(0) as u64 })
        })
        .collect()
}

async fn insert_blob_object(transaction: &mut Transaction<'_, Postgres>, hash: &ContentHash, size_bytes: i64) -> Result<(), AssetError> {
    let hash_str = hash.as_str();
    let inserted = sqlx::query_file_scalar!("src/sql/assets/insert_blob_object.sql", hash_str, size_bytes).fetch_optional(&mut **transaction).await.map_err(storage)?;
    if inserted.is_some() {
        Ok(())
    } else {
        Err(AssetError::Storage(format!("stored size for content hash {hash} is inconsistent")))
    }
}

/// `verified` contains checksums whose stored permanent identity was checked by
/// the application before acquiring this transaction's account writer lock.
pub(super) async fn negotiate_books(pool: &PgPool, user_id: &str, library_id: &LibraryId, blobs: &[BlobManifestEntry], verified: &std::collections::HashSet<ContentHash>) -> Result<BlobManifestResponse, AssetError> {
    let mut transaction = begin_authorized_transaction(pool, user_id, library_id).await?;
    require_cloud_storage(&mut transaction, user_id, library_id).await?;
    let mut seen = std::collections::HashSet::with_capacity(blobs.len());
    if blobs.iter().any(|blob| !seen.insert(blob.content_hash.clone())) {
        return Err(AssetError::InvalidInput("book manifest contains a duplicate content hash".to_string()));
    }
    let identities: std::collections::HashMap<_, _> = blobs.iter().map(|blob| (blob.checksum, blob.content_hash)).collect();
    if identities.len() != blobs.len() {
        return Err(AssetError::InvalidInput("book manifest contains a duplicate checksum".into()));
    }

    let mut response = BlobManifestResponse::default();
    let mut hashes = Vec::with_capacity(blobs.len());
    let mut sizes = Vec::with_capacity(blobs.len());
    for blob in blobs {
        if blob.size_bytes == 0 {
            response.rejected.push(RejectedBlobManifestEntry { content_hash: blob.content_hash.clone(), reason: BlobManifestRejectionReason::InvalidSize });
            continue;
        }
        hashes.push(blob.checksum.as_str().to_string());
        sizes.push(size_i64(blob.size_bytes)?);
    }
    if hashes.is_empty() {
        transaction.commit().await.map_err(storage)?;
        return Ok(response);
    }

    let account = prepare_account(&mut transaction, user_id).await?;
    let rows = sqlx::query(include_str!("sql/assets/inspect_book_manifest.sql")).bind(&hashes).bind(&sizes).bind(user_id).fetch_all(&mut *transaction).await.map_err(storage)?;

    let mut available = account.limit.saturating_sub(account.used).saturating_sub(account.reserved).max(0);
    let mut claimed_hashes = Vec::<String>::new();
    let mut claimed_sizes = Vec::new();
    for row in rows {
        let raw_hash: String = row.try_get("content_hash").map_err(storage)?;
        let requested_size: i64 = row.try_get("size_bytes").map_err(storage)?;
        let stored_size: Option<i64> = row.try_get("stored_size_bytes").map_err(storage)?;
        let owned: bool = row.try_get("owned").map_err(storage)?;
        let reservation_id: Option<String> = row.try_get("reservation_id").map_err(storage)?;
        let checksum = ContentHash::new(&raw_hash);
        let hash = identities[&checksum];
        if stored_size.is_some_and(|size| size != requested_size) {
            response.rejected.push(RejectedBlobManifestEntry { content_hash: hash, reason: BlobManifestRejectionReason::SizeMismatch });
            continue;
        }
        if owned {
            if verified.contains(&checksum) { response.owned.push(hash); }
            else { response.upload.push(hash); }
            continue;
        }
        if reservation_id.is_some() {
            response.rejected.push(RejectedBlobManifestEntry { content_hash: hash, reason: BlobManifestRejectionReason::UploadInProgress });
            continue;
        }
        if let Some(stored_size) = stored_size.filter(|_| verified.contains(&checksum)) {
            if stored_size != requested_size {
                response.rejected.push(RejectedBlobManifestEntry { content_hash: hash, reason: BlobManifestRejectionReason::SizeMismatch });
            } else if requested_size > available {
                response.rejected.push(RejectedBlobManifestEntry { content_hash: hash, reason: BlobManifestRejectionReason::QuotaExceeded });
            } else {
                available -= requested_size;
                claimed_hashes.push(raw_hash);
                claimed_sizes.push(requested_size);
                response.claimed.push(hash);
            }
            continue;
        }
        if requested_size > available {
            response.rejected.push(RejectedBlobManifestEntry { content_hash: hash, reason: BlobManifestRejectionReason::QuotaExceeded });
        } else {
            // This is an admission preview. The ordinary PUT reservation makes
            // the quota decision durable immediately before bytes are accepted.
            available -= requested_size;
            response.upload.push(hash);
        }
    }

    if !claimed_hashes.is_empty() {
        let row = sqlx::query(include_str!("sql/assets/claim_book_manifest.sql")).bind(user_id).bind(&claimed_hashes).bind(&claimed_sizes).fetch_one(&mut *transaction).await.map_err(storage)?;
        let accepted: bool = row.try_get("accepted").map_err(storage)?;
        if !accepted {
            return Err(account.quota_exceeded(claimed_sizes.iter().sum::<i64>().max(0) as u64));
        }
    }
    for identity in response.owned.iter().chain(&response.claimed) {
        let blob = blobs.iter().find(|blob| blob.content_hash == *identity).expect("response derives from manifest");
        record_library_claim(&mut transaction, user_id, library_id, identity, &blob.checksum).await?;
        bind_book_revision(&mut transaction, user_id, identity, &blob.checksum).await?;
    }
    transaction.commit().await.map_err(storage)?;
    Ok(response)
}

pub(super) async fn claim_existing_book(pool: &PgPool, user_id: &str, library_id: &LibraryId, hash: &ContentHash, size_bytes: u64) -> Result<(), AssetError> {
    let size_bytes = size_i64(size_bytes)?;
    let mut transaction = begin_authorized_transaction(pool, user_id, library_id).await?;
    let _ = prepare_account(&mut transaction, user_id).await?;

    let hash_str = hash.as_str();
    if sqlx::query_file_scalar!("src/sql/assets/charge_exists.sql", user_id, hash_str).fetch_one(&mut *transaction).await.map_err(storage)?.unwrap_or(false) {
        transaction.commit().await.map_err(storage)?;
        return Ok(());
    }
    require_cloud_storage(&mut transaction, user_id, library_id).await?;
    let referenced: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM user_blob_reference
             WHERE user_id=$1 AND content_hash=$2 AND reference_count > 0
         )",
    )
    .bind(user_id)
    .bind(hash_str)
    .fetch_one(&mut *transaction)
    .await
    .map_err(storage)?;
    if !referenced {
        return Err(AssetError::Forbidden);
    }

    let active_reservation = sqlx::query_file_scalar!("src/sql/assets/delete_reservation_by_content_returning_size.sql", user_id, hash_str).fetch_optional(&mut *transaction).await.map_err(storage)?;
    if let Some(released) = active_reservation {
        sqlx::query_file!("src/sql/assets/adjust_reserved.sql", user_id, -released).execute(&mut *transaction).await.map_err(storage)?;
    }

    insert_blob_object(&mut transaction, hash, size_bytes).await?;
    let charged = sqlx::query_file!("src/sql/assets/insert_charge_idempotent.sql", user_id, hash_str).execute(&mut *transaction).await.map_err(storage)?.rows_affected() == 1;
    if charged {
        let account = sqlx::query(include_str!("sql/assets/claim_used_capacity.sql")).bind(user_id).bind(size_bytes).fetch_optional(&mut *transaction).await.map_err(storage)?;
        if account.is_none() {
            return Err(account_snapshot(&mut transaction, user_id).await?.quota_exceeded(size_bytes as u64));
        }
    }
    transaction.commit().await.map_err(storage)
}

pub(super) async fn reserve_book_upload(pool: &PgPool, user_id: &str, library_id: &LibraryId, hash: &ContentHash, size_bytes: u64) -> Result<BookUploadAdmission, AssetError> {
    let size_i64 = size_i64(size_bytes)?;
    let mut transaction = begin_authorized_transaction(pool, user_id, library_id).await?;
    require_cloud_storage(&mut transaction, user_id, library_id).await?;
    let _ = prepare_account(&mut transaction, user_id).await?;
    let hash_str = hash.as_str();
    if sqlx::query_file_scalar!("src/sql/assets/charge_exists.sql", user_id, hash_str).fetch_one(&mut *transaction).await.map_err(storage)?.unwrap_or(false) {
        transaction.commit().await.map_err(storage)?;
        return Ok(BookUploadAdmission::AlreadyOwned);
    }
    if sqlx::query_file_scalar!("src/sql/assets/reservation_exists.sql", user_id, hash_str).fetch_one(&mut *transaction).await.map_err(storage)?.unwrap_or(false) {
        return Err(AssetError::UploadInProgress);
    }
    let reserved = sqlx::query(include_str!("sql/assets/reserve_capacity.sql")).bind(user_id).bind(size_i64).fetch_optional(&mut *transaction).await.map_err(storage)?;
    if reserved.is_none() {
        return Err(account_snapshot(&mut transaction, user_id).await?.quota_exceeded(size_bytes));
    }
    let reservation = BookUploadReservation { id: uuid::Uuid::new_v4().to_string(), user_id: user_id.to_string(), library_id: library_id.clone(), hash: hash.clone(), expected_size_bytes: size_bytes };
    let library_id_str = library_id.to_string();
    let inserted = sqlx::query(include_str!("sql/assets/insert_reservation.sql")).bind(&reservation.id).bind(user_id).bind(library_id_str).bind(hash_str).bind(size_i64).bind(UPLOAD_RESERVATION_TTL_SECONDS).execute(&mut *transaction).await;
    if let Err(error) = inserted {
        if error.as_database_error().and_then(|database| database.code()).as_deref() == Some("23505") {
            return Err(AssetError::UploadInProgress);
        }
        return Err(storage(error));
    }
    transaction.commit().await.map_err(storage)?;
    Ok(BookUploadAdmission::Reserved(reservation))
}

pub(super) async fn finish_book_upload(pool: &PgPool, reservation: &BookUploadReservation, actual_size_bytes: u64) -> Result<(), AssetError> {
    if actual_size_bytes != reservation.expected_size_bytes {
        return Err(AssetError::LengthMismatch { expected: reservation.expected_size_bytes, actual: actual_size_bytes });
    }
    let actual_size_i64 = size_i64(actual_size_bytes)?;
    let mut transaction = pool.begin().await.map_err(storage)?;
    if let Err(error) = require_library_owner(&mut transaction, &reservation.user_id, &reservation.library_id).await {
        drop(transaction);
        abort_book_upload(pool, reservation).await?;
        return Err(error);
    }
    require_cloud_storage(&mut transaction, &reservation.user_id, &reservation.library_id).await?;
    let _ = account_snapshot(&mut transaction, &reservation.user_id).await?;
    let reservation_hash = reservation.hash.as_str();
    let stored_size: Option<i64> =
        sqlx::query_scalar(include_str!("sql/assets/delete_reservation_by_id_and_user_returning_size.sql")).bind(&reservation.id).bind(&reservation.user_id).fetch_optional(&mut *transaction).await.map_err(storage)?;
    let Some(stored_size) = stored_size else {
        let already_charged = sqlx::query_file_scalar!("src/sql/assets/charge_exists.sql", reservation.user_id, reservation_hash).fetch_one(&mut *transaction).await.map_err(storage)?.unwrap_or(false);
        if already_charged {
            record_library_claim(&mut transaction, &reservation.user_id, &reservation.library_id, &reservation.hash, &reservation.hash).await?;
    transaction.commit().await.map_err(storage)?;
            return Ok(());
        }
        return Err(AssetError::Storage("book upload reservation no longer exists".to_string()));
    };
    if stored_size != actual_size_i64 {
        return Err(AssetError::LengthMismatch { expected: stored_size.max(0) as u64, actual: actual_size_bytes });
    }
    insert_blob_object(&mut transaction, &reservation.hash, actual_size_i64).await?;
    let inserted = sqlx::query_file!("src/sql/assets/insert_charge_idempotent.sql", reservation.user_id, reservation_hash).execute(&mut *transaction).await.map_err(storage)?.rows_affected();
    let charged = inserted == 1;
    sqlx::query_file!("src/sql/assets/finish_upload_update_account.sql", reservation.user_id, stored_size, charged).execute(&mut *transaction).await.map_err(storage)?;
    record_library_claim(&mut transaction, &reservation.user_id, &reservation.library_id, &reservation.hash, &reservation.hash).await?;
    transaction.commit().await.map_err(storage)
}

pub(super) async fn abort_book_upload(pool: &PgPool, reservation: &BookUploadReservation) -> Result<(), AssetError> {
    let mut transaction = pool.begin().await.map_err(storage)?;
    let _ = account_snapshot(&mut transaction, &reservation.user_id).await?;
    let released = sqlx::query_file_scalar!("src/sql/assets/delete_reservation_by_id_and_user_returning_size.sql", reservation.id, reservation.user_id).fetch_optional(&mut *transaction).await.map_err(storage)?;
    if let Some(released) = released {
        sqlx::query_file!("src/sql/assets/adjust_reserved.sql", reservation.user_id, -released).execute(&mut *transaction).await.map_err(storage)?;
    }
    transaction.commit().await.map_err(storage)
}

// Thumbnails are keyed by the book identity, not by their own bytes.
// A synced lifecycle entry is enough: the book file may still be uploading.
async fn require_thumbnail_book(transaction: &mut Transaction<'_, Postgres>, library_id: &LibraryId, hash: &ContentHash, size_bytes: u64) -> Result<(), AssetError> {
    if size_bytes > sync_common::MAX_THUMBNAIL_BYTES {
        return Err(AssetError::TooLarge { limit: sync_common::MAX_THUMBNAIL_BYTES });
    }
    let present: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sync_state WHERE library_id=$1
         AND kind='book_lifecycle' AND present AND content_hash=$2 FOR SHARE)",
    )
    .bind(library_id.to_string())
    .bind(hash.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage)?;
    if !present {
        return Err(AssetError::Forbidden);
    }
    Ok(())
}

/// Checks thumbnail access without reserving or charging user storage.
pub(super) async fn validate_thumbnail(pool: &PgPool, user_id: &str, library_id: &LibraryId, hash: &ContentHash, size_bytes: u64) -> Result<(), AssetError> {
    let mut transaction = begin_authorized_transaction(pool, user_id, library_id).await?;
    require_thumbnail_book(&mut transaction, library_id, hash, size_bytes).await?;
    transaction.commit().await.map_err(storage)
}

pub(super) async fn validate_thumbnail_upload(pool: &PgPool, user_id: &str, library_id: &LibraryId, hash: &ContentHash, size_bytes: u64) -> Result<(), AssetError> {
    let mut transaction = begin_authorized_transaction(pool, user_id, library_id).await?;
    require_cloud_storage(&mut transaction, user_id, library_id).await?;
    require_thumbnail_book(&mut transaction, library_id, hash, size_bytes).await?;
    transaction.commit().await.map_err(storage)
}

/// Applies reference-count changes atomically with a winning lifecycle write.
/// Placement mutations are deliberately absent: a book may be present in any
/// number of directories but owns exactly one content blob reference.
pub(super) async fn apply_lifecycle_reference_deltas(transaction: &mut Transaction<'_, Postgres>, user_id: &str, deltas: impl IntoIterator<Item = (ContentHash, i64)>) -> Result<(), sqlx::Error> {
    let mut merged = std::collections::BTreeMap::<String, i64>::new();
    for (hash, delta) in deltas {
        if delta != 0 {
            *merged.entry(hash.to_string()).or_default() += delta;
        }
    }
    let (hashes, values): (Vec<_>, Vec<_>) = merged.into_iter().filter(|(_, delta)| *delta != 0).unzip();
    if hashes.is_empty() {
        return Ok(());
    }
    // Serialize revision replacement and reference removal for this account.
    sqlx::query("SELECT user_id FROM user_storage_account WHERE user_id=$1 FOR UPDATE").bind(user_id).fetch_optional(&mut **transaction).await?;
    let row = sqlx::query(include_str!("sql/assets/apply_blob_reference_deltas.sql")).bind(user_id).bind(&hashes).bind(&values).fetch_one(&mut **transaction).await?;
    let valid: bool = row.try_get("valid")?;
    if !valid {
        return Err(sqlx::Error::Protocol("lifecycle blob reference underflow".to_owned()));
    }
    let obsolete: Vec<String> = sqlx::query_scalar(include_str!("sql/cloud_storage/delete_unreferenced_revisions.sql"))
        .bind(user_id).bind(&hashes).fetch_all(&mut **transaction).await?;
    if !obsolete.is_empty() {
        sqlx::query(include_str!("sql/cloud_storage/release_obsolete_revision_charges.sql"))
            .bind(user_id).bind(obsolete).execute(&mut **transaction).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Database fixtures model objects already verified by the application.
    async fn negotiate_books(pool: &PgPool, user: &str, library: &LibraryId, blobs: &[BlobManifestEntry]) -> Result<BlobManifestResponse, AssetError> {
        super::negotiate_books(pool, user, library, blobs, &blobs.iter().map(|blob| blob.checksum).collect()).await
    }

    fn hash() -> ContentHash {
        let raw = uuid::Uuid::new_v4().simple().to_string().repeat(2);
        ContentHash::new(&raw)
    }

    async fn seed_user(pool: &PgPool, quota: i64) -> (String, LibraryId) {
        let user_id = uuid::Uuid::new_v4().to_string();
        let email = format!("quota-{}@example.com", uuid::Uuid::new_v4().simple());
        let library_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO users(id, email) VALUES ($1, $2)").bind(&user_id).bind(email).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO libraries(id, user_id, name) VALUES ($1, $2, 'Books')").bind(library_id.to_string()).bind(&user_id).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO user_storage_account(user_id, quota_bytes) VALUES ($1, $2)").bind(&user_id).bind(quota).execute(pool).await.unwrap();
        (user_id, library_id)
    }

    async fn reference(pool: &PgPool, _user_id: &str, library_id: &LibraryId, hash: &ContentHash, replica_seq: i64) {
        sqlx::query(
            "INSERT INTO sync_state(
                 library_id,kind,entity_key,entity_subkey,value,present,content_hash,
                 changed_at,replica_id,replica_seq,version_rank,event_id
             ) VALUES($1,'book_lifecycle',$2,'',$3,TRUE,$4,1,$5,$6,0,$7)",
        )
        .bind(library_id.to_string())
        .bind(format!("book-{replica_seq}"))
        .bind(Vec::<u8>::new())
        .bind(hash.as_str())
        .bind(format!("replica-{replica_seq}"))
        .bind(replica_seq)
        .bind(uuid::Uuid::new_v4().to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn grant_blob_reference(pool: &PgPool, user_id: &str, hash: &ContentHash) {
        let mut transaction = pool.begin().await.unwrap();
        apply_lifecycle_reference_deltas(&mut transaction, user_id, [(hash.clone(), 1)]).await.unwrap();
        transaction.commit().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn quota_is_transactional_deduplicated_per_user_and_reservations_release() {
        let Some(pool) = crate::test_support::isolated_pool("assets", 8).await else { return };

        let shared = hash();
        let (first_user, first_library) = seed_user(&pool, 100).await;
        let (second_user, second_library) = seed_user(&pool, 100).await;

        grant_blob_reference(&pool, &first_user, &shared).await;
        claim_existing_book(&pool, &first_user, &first_library, &shared, 60).await.unwrap();
        claim_existing_book(&pool, &first_user, &first_library, &shared, 60).await.unwrap();
        assert!(matches!(claim_existing_book(&pool, &second_user, &first_library, &shared, 60).await, Err(AssetError::Forbidden)));
        let invalid_manifest = [BlobManifestEntry { content_hash: hash(), checksum: hash(), size_bytes: 0 }];
        assert!(matches!(negotiate_books(&pool, &second_user, &first_library, &invalid_manifest).await, Err(AssetError::Forbidden)));
        assert!(matches!(claim_existing_book(&pool, &second_user, &second_library, &shared, 60).await, Err(AssetError::Forbidden)));
        grant_blob_reference(&pool, &second_user, &shared).await;
        claim_existing_book(&pool, &second_user, &second_library, &shared, 60).await.unwrap();
        let first_used: i64 = sqlx::query_scalar("SELECT used_bytes FROM user_storage_account WHERE user_id=$1").bind(&first_user).fetch_one(&pool).await.unwrap();
        let second_used: i64 = sqlx::query_scalar("SELECT used_bytes FROM user_storage_account WHERE user_id=$1").bind(&second_user).fetch_one(&pool).await.unwrap();
        let physical_objects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blob_object WHERE content_hash=$1").bind(shared.as_str()).fetch_one(&pool).await.unwrap();
        assert_eq!((first_used, second_used, physical_objects), (60, 60, 1));
        assert_eq!(storage_usage(&pool, &first_user).await.unwrap(), crate::StorageUsage { used_bytes: 60, reserved_bytes: 0, quota_bytes: 100 });

        let too_large = hash();
        grant_blob_reference(&pool, &first_user, &too_large).await;
        assert!(matches!(claim_existing_book(&pool, &first_user, &first_library, &too_large, 41).await, Err(AssetError::QuotaExceeded { .. })));

        let expiring = hash();
        let expired_reservation = match reserve_book_upload(&pool, &first_user, &first_library, &expiring, 20).await.unwrap() {
            BookUploadAdmission::Reserved(reservation) => reservation,
            BookUploadAdmission::AlreadyOwned => panic!("a new hash must not already be charged"),
        };
        sqlx::query("UPDATE book_upload_reservation SET expires_at=now()-interval '1 second' WHERE id=$1").bind(&expired_reservation.id).execute(&pool).await.unwrap();
        let replacement_hash = hash();
        let replacement = match reserve_book_upload(&pool, &first_user, &first_library, &replacement_hash, 15).await.unwrap() {
            BookUploadAdmission::Reserved(reservation) => reservation,
            BookUploadAdmission::AlreadyOwned => panic!("a new hash must not already be charged"),
        };
        let reserved_after_expiry: i64 = sqlx::query_scalar("SELECT reserved_bytes FROM user_storage_account WHERE user_id=$1").bind(&first_user).fetch_one(&pool).await.unwrap();
        assert_eq!(reserved_after_expiry, 15);
        let active_manifest = negotiate_books(&pool, &first_user, &first_library, &[BlobManifestEntry { content_hash: replacement_hash, checksum: replacement_hash, size_bytes: 15 }]).await.unwrap();
        assert_eq!(active_manifest.rejected.len(), 1);
        assert_eq!(active_manifest.rejected[0].reason, BlobManifestRejectionReason::UploadInProgress);
        abort_book_upload(&pool, &replacement).await.unwrap();

        let (concurrent_user, concurrent_library) = seed_user(&pool, 100).await;
        let left = hash();
        let right = hash();
        reference(&pool, &concurrent_user, &concurrent_library, &left, 1).await;
        reference(&pool, &concurrent_user, &concurrent_library, &right, 2).await;
        let mut initial_projection = pool.begin().await.unwrap();
        apply_lifecycle_reference_deltas(&mut initial_projection, &concurrent_user, [(left.clone(), 1), (right.clone(), 1)]).await.unwrap();
        initial_projection.commit().await.unwrap();
        let left_attempt = reserve_book_upload(&pool, &concurrent_user, &concurrent_library, &left, 60);
        let right_attempt = reserve_book_upload(&pool, &concurrent_user, &concurrent_library, &right, 60);
        let (left_result, right_result) = tokio::join!(left_attempt, right_attempt);
        let reservation = match (left_result, right_result) {
            (Ok(BookUploadAdmission::Reserved(reservation)), Err(AssetError::QuotaExceeded { .. })) | (Err(AssetError::QuotaExceeded { .. }), Ok(BookUploadAdmission::Reserved(reservation))) => reservation,
            results => panic!("exactly one concurrent reservation must fit: {results:?}"),
        };
        finish_book_upload(&pool, &reservation, 60).await.unwrap();
        let account: (i64, i64) = sqlx::query_as("SELECT used_bytes, reserved_bytes FROM user_storage_account WHERE user_id=$1").bind(&concurrent_user).fetch_one(&pool).await.unwrap();
        assert_eq!(account, (60, 0));

        let placement_seq = if reservation.hash == left { 1_i64 } else { 2_i64 };
        sqlx::query(
            "UPDATE sync_state
             SET present=FALSE, content_hash=NULL, changed_at=2, replica_id='remover',replica_seq=99,
                 version_rank=1, event_id=$3, server_seq=DEFAULT
             WHERE library_id=$1 AND kind='book_lifecycle' AND entity_key=$2 AND entity_subkey=''",
        )
        .bind(concurrent_library.to_string())
        .bind(format!("book-{placement_seq}"))
        .bind(uuid::Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .unwrap();
        let mut reconciliation = pool.begin().await.unwrap();
        sqlx::query(include_str!("sql/cloud_storage/delete_claim.sql")).bind(concurrent_library.to_string()).bind(reservation.hash.as_str()).execute(&mut *reconciliation).await.unwrap();
        apply_lifecycle_reference_deltas(&mut reconciliation, &concurrent_user, [(reservation.hash.clone(), -1)]).await.unwrap();
        reconciliation.commit().await.unwrap();
        let used_after_removal: i64 = sqlx::query_scalar("SELECT used_bytes FROM user_storage_account WHERE user_id=$1").bind(&concurrent_user).fetch_one(&pool).await.unwrap();
        assert_eq!(used_after_removal, 0);

        let abort_hash = if reservation.hash == left { right } else { left };
        let abort_reservation = match reserve_book_upload(&pool, &concurrent_user, &concurrent_library, &abort_hash, 60).await.unwrap() {
            BookUploadAdmission::Reserved(reservation) => reservation,
            BookUploadAdmission::AlreadyOwned => panic!("the unuploaded hash must not already be charged"),
        };
        abort_book_upload(&pool, &abort_reservation).await.unwrap();
        let reserved_after_abort: i64 = sqlx::query_scalar("SELECT reserved_bytes FROM user_storage_account WHERE user_id=$1").bind(&concurrent_user).fetch_one(&pool).await.unwrap();
        assert_eq!(reserved_after_abort, 0);

        sqlx::query("DELETE FROM users WHERE id=ANY($1)").bind(vec![first_user, second_user, concurrent_user]).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn thumbnails_are_free_but_require_a_live_book_and_bounded_size() {
        let Some(pool) = crate::test_support::isolated_pool("free_thumbnails", 4).await else { return };
        let (user, library) = seed_user(&pool, 0).await;
        let book = hash();
        assert!(matches!(validate_thumbnail(&pool, &user, &library, &book, 60).await, Err(AssetError::Forbidden)));
        reference(&pool, &user, &library, &book, 1).await;
        validate_thumbnail(&pool, &user, &library, &book, sync_common::MAX_THUMBNAIL_BYTES).await.unwrap();
        assert!(matches!(validate_thumbnail(&pool, &user, &library, &book, sync_common::MAX_THUMBNAIL_BYTES + 1).await, Err(AssetError::TooLarge { .. })));
        let (other_user, other_library) = seed_user(&pool, 0).await;
        assert!(matches!(validate_thumbnail(&pool, &other_user, &other_library, &book, 60).await, Err(AssetError::Forbidden)));
        assert!(matches!(validate_thumbnail(&pool, &other_user, &library, &book, 60).await, Err(AssetError::Forbidden)));
        sqlx::query("UPDATE sync_state SET present=FALSE, changed_at=changed_at+1 WHERE library_id=$1").bind(library.to_string()).execute(&pool).await.unwrap();
        assert!(matches!(validate_thumbnail(&pool, &user, &library, &book, 60).await, Err(AssetError::Forbidden)));
        assert_eq!(storage_usage(&pool, &user).await.unwrap(), crate::StorageUsage { used_bytes: 0, reserved_bytes: 0, quota_bytes: 0 });
        assert!(library_storage_usage(&pool, &user).await.unwrap().is_empty());
        sqlx::query("DELETE FROM users WHERE id=ANY($1)").bind(vec![user, other_user]).execute(&pool).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SYNC_E2E_DATABASE_URL"]
    async fn book_manifest_claims_stored_blobs_in_one_account_transaction_and_classifies_every_hash() {
        let Some(pool) = crate::test_support::isolated_pool("assets", 4).await else { return };
        let (user_id, library_id) = seed_user(&pool, 100).await;
        let owned = hash();
        let claimable = hash();
        let missing = hash();
        let mismatch = hash();
        let over_quota = hash();

        for content_hash in [&owned, &claimable, &missing, &mismatch, &over_quota] {
            grant_blob_reference(&pool, &user_id, content_hash).await;
        }
        claim_existing_book(&pool, &user_id, &library_id, &owned, 10).await.unwrap();
        for (content_hash, size) in [(&claimable, 20_i64), (&mismatch, 5_i64), (&over_quota, 80_i64)] {
            sqlx::query("INSERT INTO blob_object(content_hash, size_bytes) VALUES ($1, $2)").bind(content_hash.as_str()).bind(size).execute(&pool).await.unwrap();
        }
        let response = negotiate_books(
            &pool,
            &user_id,
            &library_id,
            &[
                BlobManifestEntry { content_hash: owned.clone(), checksum: owned.clone(), size_bytes: 10 },
                BlobManifestEntry { content_hash: claimable.clone(), checksum: claimable.clone(), size_bytes: 20 },
                BlobManifestEntry { content_hash: missing.clone(), checksum: missing.clone(), size_bytes: 30 },
                BlobManifestEntry { content_hash: mismatch.clone(), checksum: mismatch.clone(), size_bytes: 6 },
                BlobManifestEntry { content_hash: over_quota.clone(), checksum: over_quota.clone(), size_bytes: 80 },
            ],
        )
        .await
        .unwrap();

        assert_eq!(response.owned, vec![owned]);
        assert_eq!(response.claimed, vec![claimable]);
        assert_eq!(response.upload, vec![missing]);
        assert_eq!(response.rejected.len(), 2);
        assert_eq!(response.rejected[0].content_hash, mismatch);
        assert_eq!(response.rejected[0].reason, BlobManifestRejectionReason::SizeMismatch);
        assert_eq!(response.rejected[1].content_hash, over_quota);
        assert_eq!(response.rejected[1].reason, BlobManifestRejectionReason::QuotaExceeded);

        let account: (i64, i64) = sqlx::query_as("SELECT used_bytes, reserved_bytes FROM user_storage_account WHERE user_id=$1").bind(&user_id).fetch_one(&pool).await.unwrap();
        let charges: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_blob_charge WHERE user_id=$1").bind(&user_id).fetch_one(&pool).await.unwrap();
        assert_eq!(account, (30, 0));
        assert_eq!(charges, 2);
        // A missing physical file can be repaired without charging the same
        // account twice, even when it has no remaining quota.
        sqlx::query("UPDATE user_storage_account SET quota_bytes=used_bytes WHERE user_id=$1").bind(&user_id).execute(&pool).await.unwrap();
        let repair = super::negotiate_books(&pool, &user_id, &library_id,
            &[BlobManifestEntry { content_hash: owned, checksum: owned, size_bytes: 10 }], &std::collections::HashSet::new()).await.unwrap();
        assert_eq!(repair.upload, vec![owned]);
        assert!(repair.rejected.is_empty());

        let duplicate = BlobManifestEntry { content_hash: hash(), checksum: hash(), size_bytes: 1 };
        assert!(matches!(negotiate_books(&pool, &user_id, &library_id, &[duplicate.clone(), duplicate]).await, Err(AssetError::InvalidInput(_))));
        sqlx::query("DELETE FROM users WHERE id=$1").bind(user_id).execute(&pool).await.unwrap();
    }
}

/// Resolve only this account's revision; other accounts retain their own bytes.
pub(super) async fn book_revision(pool: &PgPool, user: &str, identity: &ContentHash) -> Result<ContentHash, AssetError> {
    let value: Option<String> = sqlx::query_scalar("SELECT blob_hash FROM user_book_revision WHERE user_id=$1 AND content_hash=$2")
        .bind(user).bind(identity.as_str()).fetch_optional(pool).await.map_err(storage)?;
    value.map(|value| value.parse().map_err(storage)).transpose().map(|value| value.unwrap_or(*identity))
}

async fn record_library_claim(tx: &mut Transaction<'_, Postgres>, user: &str, library: &LibraryId, identity: &ContentHash, blob: &ContentHash) -> Result<(), AssetError> {
    sqlx::query(include_str!("sql/cloud_storage/claim.sql")).bind(library.to_string()).bind(user).bind(identity.as_str()).bind(blob.as_str()).execute(&mut **tx).await.map_err(storage)?;
    Ok(())
}

async fn bind_book_revision(transaction: &mut Transaction<'_, Postgres>, user: &str, identity: &ContentHash, checksum: &ContentHash) -> Result<(), AssetError> {
    let charged: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_blob_charge WHERE user_id=$1 AND content_hash=$2)")
        .bind(user).bind(checksum.as_str()).fetch_one(&mut **transaction).await.map_err(storage)?;
    if !charged { return Err(AssetError::Forbidden); }
    let previous: Option<String> = sqlx::query_scalar("SELECT blob_hash FROM user_book_revision WHERE user_id=$1 AND content_hash=$2")
        .bind(user).bind(identity.as_str()).fetch_optional(&mut **transaction).await.map_err(storage)?;
    sqlx::query("INSERT INTO user_book_revision(user_id, content_hash, blob_hash) VALUES ($1,$2,$3) ON CONFLICT(user_id,content_hash) DO UPDATE SET blob_hash=EXCLUDED.blob_hash")
        .bind(user).bind(identity.as_str()).bind(checksum.as_str()).execute(&mut **transaction).await.map_err(storage)?;
    let previous = previous.unwrap_or_else(|| identity.to_string());
    if previous != checksum.as_str() {
        sqlx::query(include_str!("sql/cloud_storage/replace_claim_revision.sql")).bind(user).bind(identity.as_str()).bind(checksum.as_str()).execute(&mut **transaction).await.map_err(storage)?;
        // Keep a charge while another book still uses the same blob.
        let released: Option<i64> = sqlx::query_scalar(include_str!("sql/cloud_storage/release_replaced_blob_charge.sql"))
            .bind(user).bind(previous).fetch_optional(&mut **transaction).await.map_err(storage)?;
        if let Some(size) = released {
            sqlx::query("UPDATE user_storage_account SET used_bytes=used_bytes-$2 WHERE user_id=$1").bind(user).bind(size).execute(&mut **transaction).await.map_err(storage)?;
        }
    }
    Ok(())
}

/// Charge the verified blob and publish its identity mapping in one transaction.
pub(super) async fn finish_book_revision_upload(pool: &PgPool, user: &str, library: &LibraryId, identity: &ContentHash, checksum: &ContentHash, admission: &BookUploadAdmission, actual: u64) -> Result<(), AssetError> {
    let mut transaction = begin_authorized_transaction(pool, user, library).await?;
    require_cloud_storage(&mut transaction, user, library).await?;
    // A library deletion cannot pass between authorization and book.
    let live = sqlx::query("SELECT id FROM libraries WHERE id=$1 AND user_id=$2 AND deleted_at IS NULL FOR SHARE")
        .bind(library.to_string()).bind(user).fetch_optional(&mut *transaction).await.map_err(storage)?.is_some();
    if !live { return Err(AssetError::Forbidden); }
    let _ = account_snapshot(&mut transaction, user).await?;
    let phase_started = std::time::Instant::now();
    sqlx::query("SELECT user_id FROM user_storage_account WHERE user_id=$1 FOR UPDATE").bind(user).fetch_one(&mut *transaction).await.map_err(storage)?;
    tracing::debug!(target: "sync_performance", phase = "upload_account_lock", elapsed_ms = phase_started.elapsed().as_secs_f64() * 1000.0);
    if let BookUploadAdmission::Reserved(reservation) = admission {
        if actual != reservation.expected_size_bytes { return Err(AssetError::LengthMismatch { expected: reservation.expected_size_bytes, actual }); }
        let released: Option<i64> = sqlx::query_scalar(include_str!("sql/assets/delete_reservation_by_id_and_user_returning_size.sql"))
            .bind(&reservation.id).bind(user).fetch_optional(&mut *transaction).await.map_err(storage)?;
        let Some(released) = released else { return Err(AssetError::Storage("book upload reservation expired".into())); };
        if released != size_i64(actual)? { return Err(AssetError::LengthMismatch { expected: released.max(0) as u64, actual }); }
        insert_blob_object(&mut transaction, checksum, released).await?;
        let charged = sqlx::query(include_str!("sql/assets/insert_charge_idempotent.sql")).bind(user).bind(checksum.as_str()).execute(&mut *transaction).await.map_err(storage)?.rows_affected() == 1;
        sqlx::query(include_str!("sql/assets/finish_upload_update_account.sql")).bind(user).bind(released).bind(charged).execute(&mut *transaction).await.map_err(storage)?;
    }
    record_library_claim(&mut transaction, user, library, identity, checksum).await?;
    bind_book_revision(&mut transaction, user, identity, checksum).await?;
    transaction.commit().await.map_err(storage)
}
