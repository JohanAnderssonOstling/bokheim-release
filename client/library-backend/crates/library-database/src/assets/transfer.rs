// ---- io ----
// Transfer snapshot in, outcome commit out.
//
// One drain reads everything it needs up front and records nothing until
// the orchestrator (the session, which owns this database) commits the
// returned outcome. Sync works only on these values: no connection,
// transaction, or callback crosses the boundary in either direction.
//
// Commit replays the guarded writes in one transaction, so a changed
// placement is still rejected and a later error rolls back the whole drain.
use super::{BookUploadCompletion, BookUploadSnapshot};
use super::{RejectedAsset, TransfersSql};
use crate::enrichment::EnrichmentSql;
use crate::shared_sql::SharedSql;
use crate::transactions::WriteOutcome;
use crate::{BookPlacement, BookUploadIntent, Database, DatabaseError, NativeBookCompletion, NativeBookSnapshot, RelativeBookPath};
use std::collections::HashMap;
use sync_common::{ContentHash, DirId};

/// One live placement's transfer inputs, keyed by its stored path.
#[derive(Clone, Debug)]
pub struct PlacementTransferEntry {
    pub content_hash: ContentHash,
    pub rel_path: String,
    pub download_directory: Option<DirId>,
    pub placement_current: bool,
    pub book: Option<NativeBookSnapshot>,
}

/// Point-in-time inputs for one transfer drain.
#[derive(Clone, Debug, Default)]
pub struct TransferSnapshot {
    pub assets: HashMap<ContentHash, AssetTransferState>,
    pub placements: Vec<PlacementTransferEntry>,
}

/// Point-in-time state for one content hash during a transfer drain.
#[derive(Clone, Debug)]
pub struct AssetTransferState {
    pub upload: BookUploadSnapshot,
    pub upload_paths: Vec<String>,
    pub thumbnail_allowed: bool,
}

struct DatabaseTransferSnapshot {
    assets: HashMap<ContentHash, AssetTransferState>,
    placement_paths: HashMap<ContentHash, Vec<String>>,
}

impl TransferSnapshot {
    pub fn asset_for(&self, hash: &ContentHash) -> Option<&AssetTransferState> {
        self.assets.get(hash)
    }

    pub fn placement_for(&self, hash: &ContentHash, rel_path: &str) -> Option<&PlacementTransferEntry> {
        self.placements.iter().find(|entry| entry.content_hash == *hash && entry.rel_path == rel_path)
    }
}

/// One durable transfer effect, recorded after the drain.
#[derive(Clone, Debug)]
pub enum TransferWrite {
    SettleManifest { intents: Vec<BookUploadIntent>, present_remotely: Vec<ContentHash>, rejected: Vec<(ContentHash, String)> },
    CompleteUploadedBook { hash: ContentHash, local_versions: Vec<(String, String, String)>, intent: Option<BookUploadIntent> },
    CompleteUploadedBatch { completions: Vec<BookUploadCompletion> },
    CompleteDownload { hash: ContentHash },
    CancelDownload { hash: ContentHash },
    CompleteRetainedDownload { hash: ContentHash, rel_path: String },
    SettleRejectedUpload { hash: ContentHash, intent: Option<BookUploadIntent>, reason: String },
    SettleRejectedThumbnail { hash: ContentHash, reason: String },
    SettleAvailability { available: Vec<ContentHash>, missing: Vec<ContentHash> },
    RecordUploadedThumbnail { hash: ContentHash },
    CompleteDownloadedThumbnail { hash: ContentHash },
    ForgetRemoteThumbnail { hash: ContentHash },
    ConfirmThumbnailAvailable { hash: ContentHash },
    RequestMissingThumbnail { hash: ContentHash },
    CompleteDownloadedBook { completion: NativeBookCompletion, thumbnail_set_exists: bool },
    CompleteLocalBookRequest { hash: ContentHash },
}

/// Accumulated durable effects of one drain, in execution order.
#[derive(Clone, Debug, Default)]
pub struct TransferOutcome {
    pub writes: Vec<TransferWrite>,
}

impl TransferOutcome {
    pub fn record(&mut self, write: TransferWrite) {
        self.writes.push(write);
    }
}

impl Database {
    fn read_transfer_database_snapshot(&self, hashes: &[ContentHash]) -> Result<DatabaseTransferSnapshot, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let requested = serde_json::to_string(&hashes.iter().map(ContentHash::as_str).collect::<Vec<_>>()).map_err(DatabaseError::operation)?;
        let mut assets: HashMap<ContentHash, AssetTransferState> =
            hashes.iter().map(|hash| (hash.clone(), AssetTransferState { upload: BookUploadSnapshot { local_versions: Vec::new(), intent: None }, upload_paths: Vec::new(), thumbnail_allowed: false })).collect();

        transaction.transfer_snapshot_versions(&requested, |row| {
            let hash = ContentHash::new(&row.get::<_, String>(0)?);
            if let Some(asset) = assets.get_mut(&hash) {
                asset.upload.local_versions.push((row.get(1)?, row.get(2)?, row.get(3)?));
            }
            Ok(())
        })?;
        transaction.transfer_snapshot_intents(&requested, |row| {
            let hash = ContentHash::new(&row.get::<_, String>(0)?);
            if let Some(asset) = assets.get_mut(&hash) {
                asset.upload.intent = Some(BookUploadIntent { content_hash: hash, id: row.get(1)?, checksum: ContentHash::new(&row.get::<_, String>(2)?), size_bytes: row.get::<_, i64>(3)? as u64 });
            }
            Ok(())
        })?;
        transaction.transfer_snapshot_paths(&requested, |row| {
            let hash = ContentHash::new(&row.get::<_, String>(0)?);
            if let Some(asset) = assets.get_mut(&hash) {
                asset.upload_paths.push(row.get(1)?);
            }
            Ok(())
        })?;
        transaction.transfer_snapshot_thumbnail_allowed(&requested, |row| {
            let hash = ContentHash::new(&row.get::<_, String>(0)?);
            if let Some(asset) = assets.get_mut(&hash) {
                asset.thumbnail_allowed = row.get(1)?;
            }
            Ok(())
        })?;

        let mut placement_paths: HashMap<ContentHash, Vec<String>> = HashMap::new();
        transaction.transfer_snapshot_placements(&requested, |row| {
            let hash = ContentHash::new(&row.get::<_, String>(0)?);
            placement_paths.entry(hash).or_default().push(row.get(1)?);
            Ok(())
        })?;
        transaction.commit()?;
        Ok(DatabaseTransferSnapshot { assets, placement_paths })
    }

    /// Reads every transfer input for the given hashes: upload intents and
    /// snapshots, book paths, rejection reasons, thumbnail allowance,
    /// and per-placement download/book state.
    pub fn transfer_snapshot(&self, hashes: &[ContentHash]) -> Result<TransferSnapshot, DatabaseError> {
        let database = self.read_transfer_database_snapshot(hashes)?;
        let mut snapshot = TransferSnapshot { assets: database.assets, placements: Vec::new() };
        for hash in hashes {
            for rel_path in database.placement_paths.get(hash).into_iter().flatten() {
                let placement = BookPlacement::new(hash.clone(), RelativeBookPath::parse(&rel_path).map_err(|error| DatabaseError::message(format!("stored placement path is not readable: {error:?}")))?);
                snapshot.placements.push(PlacementTransferEntry {
                    content_hash: hash.clone(),
                    rel_path: rel_path.clone(),
                    download_directory: self.native_download_directory(&placement, hash)?,
                    placement_current: self.book_placement_is_current(&placement, hash)?,
                    book: self.native_book_snapshot(&placement, hash)?,
                });
            }
        }
        Ok(snapshot)
    }

    /// Replays one drain's durable effects in order in a single transaction.
    /// Every replayed write remains guarded for credential-refresh retries.
    ///
    /// Returns whether every completed upload's local version was still
    /// current at commit time. A queued batch retries a stale member on its
    /// next planning pass regardless; a caller committing one explicit job
    /// synchronously (e.g. [`crate::assets::AssetStore`]'s single-book
    /// upload) has no later pass and must surface staleness as an error.
    pub fn commit_transfer_outcome(&self, outcome: &TransferOutcome) -> Result<bool, DatabaseError> {
        if outcome.writes.is_empty() {
            return Ok(true);
        }
        self.with_write_transaction(|transaction| {
            let mut all_current = true;
            for write in &outcome.writes {
                match write {
                    TransferWrite::SettleManifest { intents, present_remotely, rejected } => {
                        Self::settle_book_manifest_in_transaction(transaction, intents, present_remotely, rejected)?;
                    }
                    TransferWrite::CompleteUploadedBook { hash, local_versions, intent } => {
                        if !Self::complete_book_upload_in_transaction(transaction, hash, local_versions, intent.as_ref())? {
                            all_current = false;
                        }
                    }
                    TransferWrite::CompleteUploadedBatch { completions } => {
                        for completion in completions {
                            if !Self::complete_book_upload_in_transaction(transaction, &completion.content_hash, &completion.local_versions, Some(&completion.intent))? {
                                all_current = false;
                            }
                        }
                    }
                    TransferWrite::CompleteDownload { hash } => {
                        transaction.shared_transfer_complete_download(hash.as_str())?;
                    }
                    TransferWrite::CancelDownload { hash } => {
                        Self::cancel_book_download_on(transaction, hash)?;
                    }
                    TransferWrite::CompleteRetainedDownload { hash, rel_path } => {
                        let placement = BookPlacement::new(hash.clone(), RelativeBookPath::parse(rel_path).map_err(|error| DatabaseError::message(format!("stored placement path is not readable: {error:?}")))?);
                        Self::complete_retained_download_on(transaction, hash, &placement)?;
                    }
                    TransferWrite::SettleRejectedUpload { hash, intent, reason } => {
                        Self::settle_rejected_asset_on(transaction, hash, RejectedAsset::BookUpload { intent: intent.as_ref() }, reason)?;
                    }
                    TransferWrite::SettleRejectedThumbnail { hash, reason } => {
                        Self::settle_rejected_asset_on(transaction, hash, RejectedAsset::ThumbnailUpload, reason)?;
                    }
                    TransferWrite::SettleAvailability { available, missing } => {
                        Self::settle_thumbnail_availability_on(transaction, available, missing)?;
                    }
                    TransferWrite::RecordUploadedThumbnail { hash } => {
                        transaction.thumbnail_record_remote(hash.as_str())?;
                    }
                    TransferWrite::CompleteDownloadedThumbnail { hash } => {
                        Self::complete_downloaded_thumbnail_on(transaction, hash)?;
                    }
                    TransferWrite::ForgetRemoteThumbnail { hash } => {
                        transaction.thumbnail_forget_remote(hash.as_str())?;
                    }
                    TransferWrite::ConfirmThumbnailAvailable { hash } => {
                        transaction.thumbnail_set_result(hash.as_str(), "ready")?;
                    }
                    TransferWrite::RequestMissingThumbnail { hash } => {
                        transaction.thumbnail_request_missing(hash.as_str())?;
                    }
                    TransferWrite::CompleteDownloadedBook { completion, thumbnail_set_exists } => {
                        Self::complete_downloaded_book_in_transaction(transaction, completion, *thumbnail_set_exists)?;
                    }
                    TransferWrite::CompleteLocalBookRequest { hash } => {
                        Self::complete_local_book_request_on(transaction, hash)?;
                    }
                }
            }
            Ok(WriteOutcome::Commit(all_current))
        })
    }
}

#[cfg(test)]
mod commit_tests {
    use super::*;

    #[test]
    fn later_invalid_write_rolls_back_earlier_transfer_effects() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let hash = ContentHash::new(&"a".repeat(64));
        let mut outcome = TransferOutcome::default();
        outcome.record(TransferWrite::RecordUploadedThumbnail { hash });
        outcome.record(TransferWrite::CompleteRetainedDownload { hash, rel_path: "../invalid".to_owned() });

        assert!(db.commit_transfer_outcome(&outcome).is_err());
        assert!(!db.has_remote_thumbnail(&hash).unwrap());
        assert!(db.connection.is_autocommit());
    }
}
