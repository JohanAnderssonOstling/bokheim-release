//! Disposable book metadata shared by local and remote purge operations.
use crate::DatabaseError;
use crate::shared_sql::SharedSql;

/// Remove the disposable book row while keeping sync intent and retryable file
/// cleanup independent of its lifetime. Caller uses remote origin.
pub(crate) fn remove_book(tx: &rusqlite::Transaction<'_>, hash: &str) -> Result<(), DatabaseError> {
    // Capture materialized and logical paths before cascading placements away.
    tx.execute(
        "INSERT OR IGNORE INTO local_file_work(operation,content_hash,relative_path)
         SELECT 'trash',?1,relative_path FROM local_file_projection WHERE content_hash=?1
         UNION SELECT 'trash',?1,'/' || CASE WHEN p.path='' THEN bd.file_name ELSE p.path || '/' || bd.file_name END
         FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id JOIN dir_paths p ON p.id=bd.dir_id
         WHERE b.content_hash=?1",
        [hash],
    )?;
    tx.shared_cancel_book_restore(hash)?;
    for table in ["audiobook_metadata", "audiobook_track_index", "book_toc", "book_reading_entry"] {
        tx.execute(&format!("DELETE FROM {table} WHERE content_hash=?1"), [hash])?;
    }
    tx.execute("DELETE FROM remote_asset WHERE hash=?1", [hash])?;
    tx.execute("DELETE FROM download_requests WHERE target_key=?1", [hash])?;
    // Pending publications and canonical fields belong to this book too.
    // Keep only the lifecycle cell: it carries the purge to offline replicas.
    tx.execute("DELETE FROM sync_outbox WHERE (state_kind,state_key,state_subkey) IN (SELECT state_kind,state_key,state_subkey FROM sync_state_version WHERE book_key=?1 AND state_kind!='book_lifecycle')", [hash])?;
    tx.execute("DELETE FROM sync_state_version WHERE book_key=?1 AND state_kind!='book_lifecycle'", [hash])?;
    // All owned metadata, placements and device-local ingestion state cascade.
    if tx.execute("DELETE FROM book WHERE content_hash=?1", [hash])? != 0 {
        tx.shared_purge_book_record_insert(hash)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, transactions::WriteOutcome};

    #[test]
    fn purge_cleanup_is_scoped_and_rolls_back_if_a_later_delete_fails() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        // This fixture owns derived rows directly; do not enqueue canonical edits.
        db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
        for hash in ["a".repeat(64), "b".repeat(64)] {
            db.connection.execute("INSERT INTO book(content_hash,title,added_at,format) VALUES (?1,'Book',1,'epub')", [hash]).unwrap();
            let id = db.connection.last_insert_rowid();
            db.connection.execute("INSERT INTO book_language(book_row_id,position,language_tag) VALUES (?1,0,'en')", [id]).unwrap();
            db.connection.execute("INSERT INTO book_subject(book_row_id,position,name,source) VALUES (?1,0,'Subject','test')", [id]).unwrap();
        }
        let first: i64 = db.connection.query_row("SELECT row_id FROM book WHERE content_hash=?1", ["a".repeat(64)], |r| r.get(0)).unwrap();
        db.connection.execute_batch("CREATE TEMP TRIGGER reject_purge BEFORE DELETE ON book_subject BEGIN SELECT RAISE(ABORT,'injected purge failure'); END;").unwrap();
        let failed = db.with_write_transaction(|tx| {
            remove_book(tx, &"a".repeat(64))?;
            Ok(WriteOutcome::Commit(()))
        });
        assert!(failed.is_err());
        assert_eq!(db.connection.query_row("SELECT COUNT(*) FROM book_language", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        db.connection.execute_batch("DROP TRIGGER reject_purge;").unwrap();
        db.with_write_transaction(|tx| {
            remove_book(tx, &"a".repeat(64))?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        for table in ["book_language", "book_subject"] {
            assert_eq!(db.connection.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE book_row_id=?1"), [first], |r| r.get::<_, i64>(0)).unwrap(), 0);
            assert_eq!(db.connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
        assert_eq!(db.connection.query_row("SELECT COUNT(*) FROM book", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "only the purged row is deleted");
    }
    fn add(db: &Database, hash: &str) {
        db.connection.execute("INSERT INTO book(content_hash,title,format,added_at) VALUES(?1,'Book','epub',1)", [hash]).unwrap();
    }

    fn count(db: &Database) -> i64 {
        db.connection.query_row("SELECT count(*) FROM book", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn purge_deletes_the_row_and_children_but_preserves_cleanup_and_sync_intent() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let hash = sync_common::ContentHash::new(&"a".repeat(64));
        add(&db, hash.as_str());
        db.connection.execute("INSERT INTO book_reading_entry VALUES(?1,'chapter')", [hash.as_str()]).unwrap();
        db.trash_book(&hash).unwrap();
        db.purge_book(&hash).unwrap();
        assert_eq!(count(&db), 0);
        assert!(db.restore_book_placement(&hash, &sync_common::ROOT_DIR_ID).is_err());
        assert!(db.has_pending_purge_work().unwrap());
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_reading_entry", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert!(db.sync_publishable_mutations().unwrap().iter().any(|m| matches!(m.body, library_replica::MutationBody::BookLifecycle { value: library_replica::BookLifecycleState::Purged, .. })));
        assert!(!db.connection.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
        // A failed placement cleanup keeps its later purge job pending.
        db.seed_file_work("trash", &hash, "/pending.epub");
        assert!(db.purge_book_snapshot(&hash).unwrap().is_none());
        db.connection.execute("DELETE FROM local_file_work", []).unwrap();
        let old = db.purge_book_snapshot(&hash).unwrap().unwrap();
        assert!(old.remove_bytes);
        db.seed_purge_work(&hash);
        db.acknowledge_purge_book(old).unwrap();
        assert!(db.has_pending_purge_work().unwrap(), "old acknowledgement cannot discard newer cleanup");
        add(&db, hash.as_str());
        let stale = db.purge_book_snapshot(&hash).unwrap().unwrap();
        assert!(!stale.remove_bytes, "old cleanup must preserve an explicitly readded book");
        db.acknowledge_purge_book(stale).unwrap();
        assert_eq!(count(&db), 1);
    }

}
