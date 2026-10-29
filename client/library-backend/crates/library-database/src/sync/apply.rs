//! Pull-page commit: wire decoding plus one atomic apply transaction.

use crate::query_methods::query_rows;
pub(crate) use crate::sync::{SyncApplySql, invalid_integer_column, nonnegative_u64, parse_uuid_column, read_pull_cursor, storage_i64, write_pull_cursor};
#[cfg(test)]
pub use crate::transactions::commit;
pub(crate) use crate::{Database, DatabaseError};
pub(crate) use library_replica::{LibraryStateEntry, VersionKey};
use std::collections::BTreeSet;
use subject_projection::{most_specific_concept_ids, project_subject_concepts};
use sync_common::StateKey;
pub(crate) use sync_common::{PullStateResponse, ReplicaSeq};

#[derive(Clone, Debug)]
pub(crate) struct StoredVersion(pub(crate) VersionKey);

impl StoredVersion {
    pub(crate) fn decode(changed_at: i64, conflict_rank: i64, actor: String, actor_seq: i64, columns: [usize; 4]) -> rusqlite::Result<Option<Self>> {
        if changed_at == 0 && actor.is_empty() && actor_seq == 0 {
            return Ok(None);
        }
        let changed_at = nonnegative_u64(changed_at, columns[0], "change timestamp")?;
        let conflict_rank = u8::try_from(conflict_rank).ok().and_then(|rank| sync_common::ConflictRank::try_from(rank).ok()).ok_or_else(|| invalid_integer_column(columns[1], "conflict rank is outside the protocol"))?;
        let actor_seq = ReplicaSeq::new(nonnegative_u64(actor_seq, columns[3], "actor sequence")?).map_err(|_| invalid_integer_column(columns[3], "actor sequence must be positive"))?;
        let actor = parse_uuid_column(actor, columns[2])?;
        Ok(Some(Self(VersionKey::new(changed_at, conflict_rank, actor, actor_seq))))
    }
}

pub(crate) fn read_version(conn: &rusqlite::Connection, kind: &str, key: &StateKey, subkey: &str) -> Result<Option<VersionKey>, DatabaseError> {
    let mut stored = None;
    conn.read_sync_state_version_select(kind, key, subkey, |row| {
        stored = StoredVersion::decode(row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, [0, 1, 2, 3])?.map(|version| version.0);
        Ok(())
    })?;
    Ok(stored)
}

/// Mutable registers carry an ordered version; immutable records retain an
/// opaque transport token solely for acknowledgements and exact republication.
pub(crate) fn publication_version(conn: &rusqlite::Connection, kind: &str, key: &StateKey, subkey: &str) -> Result<Option<VersionKey>, DatabaseError> {
    if records::is_record(kind) {
        return records::publication_version(conn, kind, key, subkey);
    }
    read_version(conn, kind, key, subkey)
}

/// Decoded wire values and state identities only. Never snapshots database
/// state: winning versions and lifecycle checks still run under the writer.
pub(crate) struct PreparedRemoteChanges {
    pub(crate) changes: Vec<sync_common::Versioned<LibraryStateEntry>>,
}

pub(crate) fn sync_state_identity(body: &LibraryStateEntry) -> (&'static str, StateKey, String) {
    let address = body.address();
    (body.kind(), address.entity_key, address.entity_subkey)
}

/// CPU-only book projection, built before the writer runs its
/// statements. Mirrors the enrichment write path: identical inputs project
/// identical rows.
#[derive(Clone)]
pub(crate) struct PreparedBook {
    pub(crate) metadata: book_model::BookMetadata,
    pub(crate) subject_codes: Vec<subject_projection::ProjectedSubjectCode>,
    pub(crate) concepts: Vec<(usize, i64)>,
}

pub(crate) fn project_book(metadata: &book_model::BookMetadata) -> Result<PreparedBook, DatabaseError> {
    let invalid = |error: String| DatabaseError::message(format!("book projection failed: {error}"));
    let mut metadata = metadata.clone();
    metadata.infer_embedded_subject_isbns().map_err(|e| invalid(e.to_string()))?;
    let subject_codes = subject_projection::project_subject_codes(&metadata.subjects);
    let concepts = project_subject_concepts(&subject_codes);
    Ok(PreparedBook { metadata, subject_codes, concepts })
}

pub(crate) mod pull;

pub(crate) mod records;
pub(crate) mod registers;

/// Replace a book's complete local subject projection inside the caller's transaction.
pub(crate) fn write_subject_projection(tx: &rusqlite::Transaction<'_>, book_id: i64, projected: &PreparedBook) -> Result<(), DatabaseError> {
    let concepts = retained_subject_concepts(tx, book_id, projected)?;

    tx.subject_projection_clear_subjects(book_id)?;
    // Deleting assignments also deletes their evidence through the foreign key.
    tx.subject_projection_clear_concepts(book_id)?;
    for (position, subject) in projected.metadata.subjects.iter().enumerate() {
        tx.subject_projection_insert_subject(book_id, position as i64, subject.name(), subject.source(), subject.authority(), subject.code())?;
    }
    for (index, concept) in concepts {
        tx.subject_projection_insert_concept(book_id, concept, subject_projection::UNIFIED_MATCHER_VERSION)?;
        let code = &projected.subject_codes[index];
        for position in code.evidence_positions() {
            tx.subject_projection_insert_evidence(book_id, concept, *position as i64, code.system_id(), code.code())?;
        }
    }
    Ok(())
}

/// Resolve manual identities while their old evidence is still present. Both
/// single-book edits and batched projections use the same retention rules.
pub(crate) fn retained_subject_concepts(tx: &rusqlite::Transaction<'_>, book_id: i64, projected: &PreparedBook) -> Result<Vec<(usize, i64)>, DatabaseError> {
    let mut concepts = projected.concepts.clone();
    // Read retained manual identities before deleting their old evidence.
    // Their source paths may have changed after a taxonomy rename/reparenting.
    for (index, code) in projected.subject_codes.iter().enumerate() {
        if code.system_id() != subject_projection::UNIFIED_SYSTEM_ID {
            continue;
        }
        let mut retained = Vec::new();
        for position in code.evidence_positions() {
            retained.extend(query_rows(|callback| tx.subject_projection_retained_manual_concepts(book_id, *position as i64, code.code(), callback), |row| row.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?);
        }
        if !retained.is_empty() {
            concepts.retain(|(i, _)| *i != index);
            concepts.extend(retained.into_iter().map(|concept| (index, concept)));
        }
    }

    // Retained manual identities may have moved since the initial projection.
    let excluded = projected.subject_codes.iter().flat_map(|code| code.excluded_concept_ids.iter().copied()).collect::<BTreeSet<_>>();
    concepts.retain(|(_, concept)| !excluded.contains(concept));
    let retained = most_specific_concept_ids(concepts.iter().map(|(_, concept)| *concept));
    concepts.retain(|(_, concept)| retained.contains(concept));

    Ok(concepts)
}

include_sqlite_sql::include_sql!("src/sql/subject_projection.sql");

#[cfg(test)]
mod subject_projection_tests {
    use super::*;
    use crate::sql_fixtures::FixturesSql;
    use book_model::{BookMetadata, BookSubject};
    use subject_projection::ProjectedSubjectCode;

    fn database() -> (Database, i64, i64) {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let hash = sync_common::BookId::new(&"d".repeat(64));
        db.connection.fixtures_insert_book(&hash).unwrap();
        let book = db.connection.fixtures_read_book_row_id(&hash, |row| row.get(0)).unwrap();
        let concept = db.connection.fixtures_first_curated_concept(|row| row.get(0)).unwrap();
        (db, book, concept)
    }

    fn prepared(concept: i64, system: &str) -> PreparedBook {
        PreparedBook {
            metadata: BookMetadata { subjects: vec![BookSubject::new(None, "Subject", "test", None, None).unwrap()], ..BookMetadata::default() },
            subject_codes: vec![ProjectedSubjectCode { system_id: system.into(), code: "Original path".into(), evidence_positions: vec![0], excluded_concept_ids: Vec::new() }],
            // Different matches may lead to the same assignment and evidence.
            concepts: vec![(0, concept), (0, concept)],
        }
    }

    fn counts(db: &Database, book: i64) -> (i64, i64, i64) {
        db.connection.fixtures_subject_projection_counts(book, |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
    }

    #[test]
    fn replacement_deduplicates_clears_stale_rows_and_rolls_back_failed_inserts() {
        let (db, book, concept) = database();
        let projected = prepared(concept, "bisac");
        db.write(|tx| write_subject_projection(tx, book, &projected)).unwrap();
        db.write(|tx| write_subject_projection(tx, book, &projected)).unwrap();
        assert_eq!(counts(&db, book), (1, 1, 1));

        // This fails after deleting the old projection and inserting subjects.
        let invalid = prepared(0, "bisac");
        assert!(db.write(|tx| write_subject_projection(tx, book, &invalid)).is_err());
        assert_eq!(counts(&db, book), (1, 1, 1));
        assert_eq!(db.connection.fixtures_subject_projection_concept(book, |row| row.get::<_, i64>(0)).unwrap(), concept);

        let mut unmatched = projected;
        unmatched.concepts.clear();
        db.write(|tx| write_subject_projection(tx, book, &unmatched)).unwrap();
        assert_eq!(counts(&db, book), (1, 0, 0));
        let empty = project_book(&BookMetadata::default()).unwrap();
        db.write(|tx| write_subject_projection(tx, book, &empty)).unwrap();
        assert_eq!(counts(&db, book), (0, 0, 0));
    }

    #[test]
    fn replacement_retains_manual_identity_before_clearing_evidence() {
        let (db, book, concept) = database();
        let original = prepared(concept, subject_projection::UNIFIED_SYSTEM_ID);
        db.write(|tx| write_subject_projection(tx, book, &original)).unwrap();
        // The same source path now resolves differently after a taxonomy edit.
        let changed = prepared(concept + 1, subject_projection::UNIFIED_SYSTEM_ID);
        db.write(|tx| write_subject_projection(tx, book, &changed)).unwrap();
        assert_eq!(counts(&db, book), (1, 1, 1));
        assert_eq!(db.connection.fixtures_subject_projection_concept(book, |row| row.get::<_, i64>(0)).unwrap(), concept);
    }

    #[test]
    fn most_specific_assignments_are_persisted_without_deleting_source_subjects() {
        let (db, book, _) = database();
        let metadata = BookMetadata {
            subjects: vec![
                BookSubject::new(None, "PZ3", "metadata:work", Some("lcc".into()), Some("PZ3".into())).unwrap(),
                BookSubject::new(None, "PR4550", "metadata:work", Some("lcc".into()), Some("PR4550".into())).unwrap(),
                BookSubject::new(None, "Mystery", "manual:subject", Some("unified".into()), Some("Literature / Genres / Mystery & Thrillers / Mystery & Detective".into())).unwrap(),
            ],
            ..Default::default()
        };
        let projected = project_book(&metadata).unwrap();
        assert_eq!(projected.metadata.subjects, metadata.subjects);
        db.write(|tx| write_subject_projection(tx, book, &projected)).unwrap();
        let ids = query_rows(|callback| db.connection.fixtures_subject_projection_concepts(book, callback), |row| row.get::<_, i64>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(ids, [3915, 4034]);
        assert_eq!(counts(&db, book), (3, 2, 2));
    }

    #[test]
    fn curated_exclusions_preserve_raw_classifications_and_override_retained_identity() {
        let (db, book, concept) = database();
        let original = prepared(concept, subject_projection::UNIFIED_SYSTEM_ID);
        db.write(|tx| write_subject_projection(tx, book, &original)).unwrap();
        let mut excluded = original;
        excluded.subject_codes[0].excluded_concept_ids.push(concept);
        db.write(|tx| write_subject_projection(tx, book, &excluded)).unwrap();
        assert_eq!(counts(&db, book), (1, 0, 0));

        let metadata = BookMetadata {
            subjects: vec![
                BookSubject::new(None, "PZ3", "metadata:work", Some("lcc".into()), Some("PZ3".into())).unwrap(),
                BookSubject::new(None, "Mystery", "manual:subject", Some("unified".into()), Some("Literature / Genres / Mystery & Thrillers / Mystery & Detective".into())).unwrap(),
                BookSubject::new(None, "English", subject_projection::MANUAL_SUBJECT_EXCLUSION_SOURCE, Some("unified".into()), Some("Literature / By Language / English".into())).unwrap(),
            ],
            ..Default::default()
        };
        let projected = project_book(&metadata).unwrap();
        db.write(|tx| write_subject_projection(tx, book, &projected)).unwrap();
        let ids = query_rows(|callback| db.connection.fixtures_subject_projection_concepts(book, callback), |row| row.get::<_, i64>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(ids, [4034]);
        assert_eq!(counts(&db, book), (3, 1, 1));
    }
}
