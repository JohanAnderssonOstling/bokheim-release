//! Transfer workflow: planning, snapshot drivers, operation status.

use crate::shared_sql::SharedSql;
use crate::*;
// ---- planning ----
// Typed asset-planning reads and conditional settlements.
//
// Transfer orchestration receives durable snapshots from here; it never
// checks out SQLite or chooses transaction boundaries itself.

use super::TransfersSql;
use sync_common::ContentHash;

#[derive(Debug)]
pub struct AssetPlanningPage {
    pub rows: Vec<AssetCandidate>,
    pub through: i64,
}

#[derive(Clone, Debug)]
pub struct BookUploadSnapshot {
    pub local_versions: Vec<(String, String, String)>,
    pub intent: Option<BookUploadIntent>,
}

#[derive(Clone, Debug)]
pub struct BookUploadCompletion {
    pub content_hash: ContentHash,
    pub local_versions: Vec<(String, String, String)>,
    pub intent: BookUploadIntent,
}

/// A rejected asset transfer whose persistence must be settled atomically.
pub enum RejectedAsset<'a> {
    BookDownload,
    BookUpload { intent: Option<&'a BookUploadIntent> },
    ThumbnailUpload,
}

impl Database {
    /// Records why a transfer job was rejected, or clears the outstanding
    /// download request. A stale upload intent (superseded since the job was
    /// planned) is left alone rather than recording a rejection for it.
    pub fn settle_rejected_asset(&self, hash: &ContentHash, asset: RejectedAsset<'_>, reason: &str) -> Result<(), DatabaseError> {
        Self::settle_rejected_asset_on(&self.connection, hash, asset, reason)
    }

    pub(super) fn settle_rejected_asset_on(connection: &rusqlite::Connection, hash: &ContentHash, asset: RejectedAsset<'_>, reason: &str) -> Result<(), DatabaseError> {
        match asset {
            RejectedAsset::BookDownload => {
                connection.shared_transfer_complete_download(hash.as_str())?;
            }
            RejectedAsset::BookUpload { intent } => {
                if intent.is_none() || Self::upload_intent_of(connection, hash)?.as_ref() == intent {
                    connection.transfer_record_book_rejection(hash.as_str(), reason)?;
                }
            }
            RejectedAsset::ThumbnailUpload => {
                connection.transfer_record_thumbnail_rejection(hash.as_str(), reason)?;
            }
        }
        Ok(())
    }

    pub fn asset_planning_page(&self, after: i64, through: Option<i64>) -> Result<AssetPlanningPage, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut watermark = 0;
        transaction.transfer_work_watermark(|row| {
            watermark = row.get(0)?;
            Ok(())
        })?;
        let through = through.unwrap_or(watermark);
        let rows = self.candidate_page_of(after, through)?;
        transaction.commit()?;
        Ok(AssetPlanningPage { rows, through })
    }

    pub fn settle_asset_candidates(&self, settled: &[(i64, bool)]) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if !settled.is_empty() {
            transaction.transfer_settle_candidates(&serde_json::to_string(settled).map_err(DatabaseError::operation)?)?;
        }
        transaction.commit().map_err(Into::into)
    }

    /// Durable asset-work entries in one id window, oldest first.
    pub fn asset_work_page(&self, after: i64, through: i64) -> Vec<(i64, ContentHash)> {
        let mut page = Vec::new();
        self.connection
            .asset_work_page(after, through, |row| {
                page.push((row.get::<_, i64>(0)?, ContentHash::new(&row.get::<_, String>(1)?)));
                Ok(())
            })
            .expect("asset_work_page query");
        page
    }
}

#[cfg(test)]
mod planning_tests {
    use crate::*;

    struct TestDatabase {
        database: Database,
        path: std::path::PathBuf,
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn open_test_database() -> TestDatabase {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("library-database-asset-planning-{}-{id}", std::process::id()));
        let database = Database::open(&path).expect("open test database");
        database.initialize_library().expect("initialize test library");
        TestDatabase { database, path }
    }

    #[test]
    fn rejection_reason_needs_a_queued_upload() {
        let holder = open_test_database();
        let database = &holder.database;
        let hash = ContentHash::new(&"0".repeat(64));
        assert_eq!(database.book_upload_rejection_reason(&hash).expect("read reason"), None);
        database.settle_rejected_asset(&hash, RejectedAsset::BookUpload { intent: None }, "quota").expect("settle rejection");
        // The rejection is recorded, but without a live upload row there is
        // nothing to report it against — matching the ported query.
        assert_eq!(database.book_upload_rejection_reason(&hash).expect("reread reason"), None);
    }
}
