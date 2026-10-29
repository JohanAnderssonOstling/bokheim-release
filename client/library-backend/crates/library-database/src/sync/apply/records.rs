//! Completed immutable book additions. The server accepts the
//! first publication; its deliveries are authoritative, regardless of clocks.
use super::registers::{next_sequence, read_merge_state, schedule};
use crate::DatabaseError;
use crate::SqlParam;
use library_replica::LibraryStateEntry;
use rusqlite::{Connection, OptionalExtension};
use sync_common::{ConflictRank, StateKey, VersionKey, Versioned, wire};

pub(crate) fn is_record(kind: &str) -> bool {
    kind == "book_add"
}

pub(crate) fn snapshot(conn: &Connection, kind: &str, key: &StateKey, subkey: &str) -> Result<Option<(Vec<u8>, VersionKey)>, DatabaseError> {
    let stored: Option<(Vec<u8>, Vec<u8>)> = conn.records_snapshot(&kind, &key, &subkey, |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    stored.map(|(body, token)| Ok((body, wire::decode(&token, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?))).transpose()
}

pub(crate) fn publication_version(conn: &Connection, kind: &str, key: &StateKey, subkey: &str) -> Result<Option<VersionKey>, DatabaseError> {
    let token: Option<Vec<u8>> = conn.records_publication_version(&kind, &key, &subkey, |row| row.get(0)).optional()?;
    token.map(|token| wire::decode(&token, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)).transpose()
}

/// Keep an offline addition locally until it has a definitive enrichment answer.
pub(crate) fn stage(conn: &Connection, hash: &sync_common::BookId, value: library_replica::BookAddition) -> Result<(), DatabaseError> {
    if value.enrichment.is_some() {
        return Err(DatabaseError::message("completed addition cannot be staged"));
    }
    if conn.records_contains(&"book_add", &hash, |row| row.get::<_, bool>(0))? {
        return Ok(());
    }
    let body = wire::encode(&LibraryStateEntry::BookAdd { content_hash: *hash, value }).map_err(DatabaseError::operation)?;
    if conn.records_stage(&hash, &body)? != 0 {
        conn.records_mark_projection_dirty(&"book_add", &hash)?;
    }
    Ok(())
}

/// Insert once. Sequence allocation identifies the publication, not an edit.
pub(crate) fn insert(conn: &Connection, body: &LibraryStateEntry) -> Result<(), DatabaseError> {
    let address = body.address();
    if conn.records_contains(&address.kind, &address.entity_key, |r| r.get::<_, bool>(0))? {
        return Ok(());
    }
    let replica: String = conn.records_replica_id(|r| r.get(0))?;
    // The shared transport still carries a VersionKey. Its timestamp is fixed
    // and its rank is Update; this path never reads or advances an edit clock.
    let token = VersionKey::new(0, ConflictRank::Update, replica.parse().map_err(DatabaseError::operation)?, next_sequence(conn)?);
    store(conn, body, &token)?;
    schedule(conn, &address.kind, &address.entity_key, "", Some(&token))
}

/// Adopt the server's accepted record. An earlier local candidate can differ.
pub(crate) fn confirm(conn: &Connection, state: &Versioned<LibraryStateEntry>) -> Result<bool, DatabaseError> {
    let body = &state.value;
    body.validate().map_err(DatabaseError::operation)?;
    if state.version.conflict_rank != ConflictRank::Update {
        return Err(DatabaseError::message("immutable publication has an invalid transport rank"));
    }
    let hash = body.book().ok_or_else(|| DatabaseError::message("immutable record has no book"))?;
    let address = body.address();
    let current = read_merge_state(conn, &address.kind, &address.entity_key, "", Some(&hash))?;
    if !current.can_merge || !store(conn, body, &state.version)? {
        return Ok(false);
    }
    if current.queued {
        schedule(conn, &address.kind, &address.entity_key, "", None)?;
    }
    Ok(true)
}

fn store(conn: &Connection, body: &LibraryStateEntry, token: &VersionKey) -> Result<bool, DatabaseError> {
    let address = body.address();
    let bytes = wire::encode(body).map_err(DatabaseError::operation)?;
    let publication = wire::encode(token).map_err(DatabaseError::operation)?;
    let changed = conn.records_store(&address.kind, &address.entity_key, &bytes, &publication)? != 0;
    if changed {
        conn.records_reset_cover_check(&(&address.entity_key))?;
        conn.records_discard_draft(&(&address.entity_key))?;
        conn.records_mark_projection_dirty(&address.kind, &address.entity_key)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::super::{commit, registers::record_local};
    use super::*;
    use crate::Database;
    use library_replica::EnrichmentResult;
    use sync_common::BookId;

    #[test]
    fn immutable_publication_survives_reopen_without_a_mutable_version_or_duplicate_sequence() {
        let path = std::env::temp_dir().join(format!("immutable-publication-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(path.to_str().unwrap()).unwrap();
        db.initialize_library().unwrap();
        let hash = BookId::new(&"d".repeat(64));
        let tx = db.connection.unchecked_transaction().unwrap();
        tx.fixtures_insert_book_7(&hash).unwrap();
        let body = LibraryStateEntry::BookAdd {
            content_hash: hash,
            value: library_replica::BookAddition {
                facts: library_replica::BookFacts { added_at: 1, format: book_model::BookFormat::Epub },
                record: book_model::BookRecord { metadata: Default::default(), description: "x".repeat(64 * 1024) },
                navigation: None,
                pdf: None,
                recording: None,
                enrichment: Some(EnrichmentResult::NoMatch),
            },
        };
        record_local(&tx, body.clone()).unwrap();
        let sequence: i64 = tx.fixtures_read_sync_clock_sequence(|r| r.get(0)).unwrap();
        record_local(&tx, body).unwrap();
        assert_eq!(tx.fixtures_read_sync_clock_sequence(|r| r.get::<_, i64>(0)).unwrap(), sequence);
        assert_eq!(tx.fixtures_read_sync_state_version_count_2(|r| r.get::<_, i64>(0)).unwrap(), 0);
        commit(tx).unwrap();
        let pending = db.sync_publishable_mutations().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state.version.changed_at, 0);
        drop(db);
        let reopened = Database::open(path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.sync_publishable_mutations().unwrap(), pending);
        reopened.sync_acknowledge_mutations(&[pending[0].publisher_seq]).unwrap();
        assert_eq!(reopened.sync_enqueue_missing_state_cells(&[pending[0].state.value.address()]).unwrap(), 1);
        let recovered = reopened.sync_publishable_mutations().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].state, pending[0].state);
        assert_ne!(recovered[0].publisher_seq, pending[0].publisher_seq);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
}

include_sqlite_sql::include_sql!("src/sync/sql/records_operations.sql");
#[cfg(test)]
use crate::sql_fixtures::FixturesSql;
