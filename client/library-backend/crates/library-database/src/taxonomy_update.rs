//! Recompute all assignments from stored evidence, including previously unmatched books.
use crate::{Database, DatabaseError};
use rusqlite::OptionalExtension;

pub(crate) fn revision(db: &rusqlite::Connection) -> Result<String, DatabaseError> {
    let content: Option<String> = db.query_row("SELECT value FROM curated.taxonomy_meta WHERE key='client_revision'", [], |r| r.get(0)).optional()?;
    Ok(format!("{}:{}", content.as_deref().unwrap_or(subject_projection::BUNDLED_UNIFIED_TAXONOMY_REVISION_ID), subject_projection::UNIFIED_MATCHER_VERSION))
}

impl Database {
    pub(crate) fn refresh_taxonomy_assignments(&self) -> Result<(), DatabaseError> {
        let revision = revision(&self.connection)?;
        self.connection.execute_batch("CREATE TABLE IF NOT EXISTS local_taxonomy_projection(id INTEGER PRIMARY KEY CHECK(id=1), revision TEXT NOT NULL)")?;
        let old: Option<String> = self.connection.query_row("SELECT revision FROM local_taxonomy_projection WHERE id=1", [], |r| r.get(0)).optional()?;
        if old.as_deref() == Some(&revision) {
            return self.ensure_subject_cache();
        }
        let tx = self.connection.unchecked_transaction()?;
        crate::transactions::with_remote_origin(&tx, |origin| {
            origin.mark(&tx)?;
            let tx = &tx;
            tx.execute_batch("DELETE FROM source_concept_mapping; DELETE FROM subject_code_mapping;")?;
            // Keyset batches keep memory bounded without excluding unmatched books.
            let mut cursor = 0i64;
            loop {
                let rows = tx.prepare("SELECT row_id,book_metadata FROM book WHERE row_id>?1 ORDER BY row_id LIMIT 128")?.query_map([cursor], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                if rows.is_empty() {
                    break;
                }
                for (id, raw) in rows {
                    let metadata = crate::sync::decode_book_metadata(&raw)?;
                    let projected = crate::sync::apply::project_book(&metadata, raw)?;
                    crate::sync::apply::write_subject_projection(tx, id, &projected)?;
                    cursor = id;
                }
            }
            tx.execute("UPDATE subject_browse_state SET built_revision=-1 WHERE id=1", [])?;
            tx.execute("INSERT INTO local_taxonomy_projection VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision", [&revision])?;
            self.refresh_subject_cache_in_transaction(tx)?;
            Ok(())
        })?;
        tx.commit()?;
        Ok(())
    }

    /// Prepare an isolated database copy before a host promotes an approved update.
    pub fn prepare_update(path: &std::path::Path) -> Result<(), DatabaseError> {
        let db = Self::open(path)?;
        db.initialize_library()?;
        let integrity: String = db.connection.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(DatabaseError::message(integrity));
        }
        let violations: i64 = db.connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
        if violations != 0 {
            return Err(DatabaseError::message("migrated library has invalid relationships"));
        }
        db.connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_removes_stale_assignments_without_changing_canonical_data_and_is_idempotent() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let raw = sync_common::wire::encode(&book_model::BookMetadata::default()).unwrap();
        db.connection.execute("INSERT INTO book(row_id,content_hash,book_metadata) VALUES(1,?1,?2)", rusqlite::params!["a".repeat(64), raw]).unwrap();
        db.connection.execute("INSERT INTO book_unified_concept VALUES(1,123456,1)", []).unwrap();
        db.connection.execute("DELETE FROM local_taxonomy_projection", []).unwrap();
        let outbox: i64 = db.connection.query_row("SELECT count(*) FROM sync_outbox", [], |r| r.get(0)).unwrap();
        db.refresh_taxonomy_assignments().unwrap();
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_unified_concept", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(db.connection.query_row("SELECT book_metadata FROM book", [], |r| r.get::<_, Vec<u8>>(0)).unwrap(), raw);
        assert_eq!(db.connection.query_row("SELECT count(*) FROM sync_outbox", [], |r| r.get::<_, i64>(0)).unwrap(), outbox);
        let changes = db.connection.total_changes();
        db.refresh_taxonomy_assignments().unwrap();
        assert_eq!(db.connection.total_changes(), changes);
    }
    #[test]
    fn failure_rolls_back_every_assignment_and_revision() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let raw = sync_common::wire::encode(&book_model::BookMetadata::default()).unwrap();
        db.connection.execute("INSERT INTO book(row_id,content_hash,book_metadata) VALUES(1,?1,?2)", rusqlite::params!["a".repeat(64), raw]).unwrap();
        db.connection.execute("INSERT INTO book_unified_concept VALUES(1,123456,1)", []).unwrap();
        db.connection.execute("INSERT INTO book(row_id,content_hash,book_metadata) VALUES(2,?1,X'FF')", ["b".repeat(64)]).unwrap();
        db.connection.execute("DELETE FROM local_taxonomy_projection", []).unwrap();
        assert!(db.refresh_taxonomy_assignments().is_err());
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_unified_concept", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(db.connection.query_row("SELECT count(*) FROM local_taxonomy_projection", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}
