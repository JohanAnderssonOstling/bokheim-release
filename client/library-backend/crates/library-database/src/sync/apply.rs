//! Pull-page commit: wire decoding plus one atomic apply transaction.

use crate::annotations::AnnotationsSql;
use crate::import::ImportCommitSql;
use crate::shared_sql::SharedSql;
use crate::sync::{
    PullCursorKey, SyncApplySql, bounded_u8, identifier_scheme_label, identifier_scope_label, invalid_text_column, nonnegative_u64, normalized_description, parse_revision, parse_uuid_column, read_cursor_revision, storage_i64,
    toc_entry_count, write_cursor_revision,
};
use crate::{Database, DatabaseError};
use library_replica::{DirectoryIntent, DirectoryLifecycleState, MutationBody};
use sync_common::{MutationId, PullStateResponse};

#[derive(Clone, Debug)]
pub(crate) struct StoredVersion(library_replica::VersionKey);

impl StoredVersion {
    fn decode(changed_at: i64, conflict_rank: i64, actor: String, actor_seq: i64, mutation_id: String, columns: [usize; 5]) -> rusqlite::Result<Option<Self>> {
        if changed_at == 0 && actor.is_empty() && actor_seq == 0 && mutation_id.is_empty() {
            return Ok(None);
        }
        let mutation_id = MutationId::parse(&mutation_id).map_err(|error| invalid_text_column(columns[4], error.to_string()))?;
        let changed_at = nonnegative_u64(changed_at, columns[0], "change timestamp")?;
        let conflict_rank = bounded_u8(conflict_rank, columns[1], "conflict rank")?;
        let actor_seq = nonnegative_u64(actor_seq, columns[3], "actor sequence")?;
        let actor = parse_uuid_column(actor, columns[2])?;
        Ok(Some(Self(library_replica::VersionKey::new(changed_at, conflict_rank, actor, actor_seq, mutation_id))))
    }

    fn into_key(self) -> library_replica::VersionKey {
        self.0
    }

    fn decode_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Option<library_replica::VersionKey>> {
        Self::decode(row.get(offset)?, row.get(offset + 1)?, row.get(offset + 2)?, row.get(offset + 3)?, row.get(offset + 4)?, [offset, offset + 1, offset + 2, offset + 3, offset + 4]).map(|version| version.map(Self::into_key))
    }
}

fn read_version(conn: &rusqlite::Connection, kind: &str, key: &str, subkey: &str) -> Result<Option<library_replica::VersionKey>, DatabaseError> {
    let mut stored = None;
    conn.read_sync_state_version_select(kind, key, subkey, |row| {
        stored = StoredVersion::decode_row(row, 0)?;
        Ok(())
    })?;
    Ok(stored)
}

/// Decoded wire values and state identities only. Never snapshots database
/// state: winning versions and lifecycle checks still run under the writer.
struct PreparedRemoteChanges<'a> {
    changes: Vec<(&'a sync_common::ServerMutation, MutationBody)>,
}

fn prepare_remote_changes(changes: &[sync_common::ServerMutation]) -> Result<PreparedRemoteChanges<'_>, Box<dyn std::error::Error + Send + Sync>> {
    let mut prepared = Vec::with_capacity(changes.len());
    for change in changes {
        if let Some(body) = MutationBody::from_wire(&change.mutation)? {
            prepared.push((change, body));
        } else {
            log::debug!("Skipping unknown synchronized state kind {}", change.mutation.kind);
        }
    }
    Ok(PreparedRemoteChanges { changes: prepared })
}

pub(crate) fn sync_state_identity(body: &MutationBody) -> (&'static str, String, String) {
    match body {
        MutationBody::DirectoryName { dir_id, .. } => ("directory_name", dir_id.to_string(), String::new()),
        MutationBody::DirectoryParent { dir_id, .. } => ("directory_parent", dir_id.to_string(), String::new()),
        MutationBody::DirectoryLifecycle { dir_id, .. } => ("directory_lifecycle", dir_id.to_string(), String::new()),
        MutationBody::BookFacts { content_hash, .. } => ("book_facts", content_hash.to_string(), String::new()),
        MutationBody::BookLifecycle { content_hash, .. } => ("book_lifecycle", content_hash.to_string(), String::new()),
        MutationBody::Placement { dir_id, content_hash, .. } => ("placement", content_hash.to_string(), dir_id.to_string()),
        MutationBody::ReadingPosition { content_hash, .. } => ("reading_position", content_hash.to_string(), String::new()),
        MutationBody::Annotation { annotation_id, .. } => ("annotation", annotation_id.clone(), String::new()),
        MutationBody::Metadata { content_hash, .. } => ("metadata", content_hash.to_string(), String::new()),
        MutationBody::PdfReaderMetadata { content_hash, value } => ("pdf_reader_metadata", content_hash.to_string(), value.checksum.clone()),
        MutationBody::Description { content_hash, .. } => ("description", content_hash.to_string(), String::new()),
        MutationBody::BookToc { content_hash, .. } => ("book_toc", content_hash.to_string(), String::new()),
    }
}

/// CPU-only book projection, built before the writer runs its
/// statements. Mirrors the enrichment write path: identical inputs project
/// identical rows.
#[derive(Clone)]
pub(crate) struct PreparedBook {
    pub(crate) metadata: book_model::BookMetadata,
    pub(crate) encoded: Vec<u8>,
    pub(crate) subject_codes: Vec<subject_projection::ProjectedSubjectCode>,
    pub(crate) assignments: Vec<subject_projection::ProjectedSubjectAssignment>,
    pub(crate) concepts: Vec<(usize, i64, String)>,
    pub(crate) references: Vec<(String, Option<String>, Option<String>)>,
    pub(crate) classification_rows: Vec<ClassificationRows>,
}

pub(crate) fn project_book(metadata: &book_model::BookMetadata, encoded: Vec<u8>) -> Result<PreparedBook, DatabaseError> {
    let mut timing = crate::timing::PhaseTimer::new("project_book");
    let invalid = |error: String| DatabaseError::message(format!("book projection failed: {error}"));
    let mut metadata = metadata.clone();
    let identifier_count = metadata.identifiers.len();
    metadata.infer_embedded_subject_isbns().map_err(|e| invalid(e.to_string()))?;
    let encoded = if metadata.identifiers.len() != identifier_count { sync_common::wire::encode(&metadata).map_err(DatabaseError::operation)? } else { encoded };
    timing.mark("clone_and_infer_isbns");
    let subject_codes = subject_projection::project_subject_codes(&metadata.subjects);
    timing.mark("subject_codes");
    let assignments = subject_projection::project_subjects(&metadata.subjects);
    timing.mark("subject_assignments");
    let mut concepts = Vec::new();
    for (index, code) in subject_codes.iter().enumerate() {
        for path in subject_projection::unified_subject_paths(code.system_id(), code.code()) {
            concepts.push((index, subject_projection::unified_concept_id(&path).expect("projected subject exists in the taxonomy"), path));
        }
    }
    timing.mark("unified_concepts");
    let references = book_enrichment::related_isbn::lookup_candidates(&metadata.identifiers).into_iter().map(|isbn| (isbn, None, None)).collect();
    timing.mark("isbn_references");
    let mut prepared = PreparedBook { metadata, encoded, subject_codes, assignments, concepts, references, classification_rows: Vec::new() };
    prepared.classification_rows = prepare_classification_rows(&prepared);
    timing.mark("classification_rows");
    Ok(prepared)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Cell {
    Null,
    Integer(i64),
    Text(String),
}

impl From<i64> for Cell {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}

impl From<&str> for Cell {
    fn from(v: &str) -> Self {
        Self::Text(v.into())
    }
}

impl From<Option<&str>> for Cell {
    fn from(v: Option<&str>) -> Self {
        v.map_or(Self::Null, Self::from)
    }
}

impl rusqlite::types::ToSql for Cell {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, Value, ValueRef};
        Ok(match self {
            Self::Null => ToSqlOutput::Owned(Value::Null),
            Self::Integer(v) => ToSqlOutput::Owned(Value::Integer(*v)),
            Self::Text(v) => ToSqlOutput::Borrowed(ValueRef::Text(v.as_bytes())),
        })
    }
}

#[derive(Clone)]
pub(crate) struct ClassificationRows {
    pub(crate) table: &'static str,
    pub(crate) keys: usize,
    pub(crate) width: usize,
    pub(crate) desired: std::collections::BTreeMap<Vec<Cell>, Vec<Cell>>,
}

pub(crate) fn prepare_classification_rows(prepared: &PreparedBook) -> Vec<ClassificationRows> {
    let subjects = prepared.metadata.subjects.iter().enumerate().map(|(i, s)| vec![(i as i64).into(), s.name().into(), s.source().into(), s.authority().into(), s.code().into()]).collect();
    let mut codes = Vec::new();
    for code in &prepared.subject_codes {
        for pos in code.evidence_positions() {
            codes.push(vec![(*pos as i64).into(), code.system_id().into(), code.code().into()]);
        }
    }
    let mut assignments = Vec::new();
    let mut evidence = Vec::new();
    for assignment in &prepared.assignments {
        assignments.push(vec![assignment.system_id().into(), assignment.subject_path().into(), assignment.matcher_version().into()]);
        for pos in assignment.evidence_positions() {
            evidence.push(vec![assignment.system_id().into(), assignment.subject_path().into(), (*pos as i64).into()]);
        }
    }
    let mut concepts = Vec::new();
    let mut unified_evidence = Vec::new();
    for (index, concept, _) in &prepared.concepts {
        concepts.push(vec![(*concept).into(), subject_projection::UNIFIED_MATCHER_VERSION.into()]);
        let code = &prepared.subject_codes[*index];
        for pos in code.evidence_positions() {
            unified_evidence.push(vec![(*concept).into(), (*pos as i64).into(), code.system_id().into(), code.code().into()]);
        }
    }
    fn rows(table: &'static str, keys: usize, width: usize, rows: Vec<Vec<Cell>>) -> ClassificationRows {
        ClassificationRows { table, keys, width, desired: rows.into_iter().map(|v| (v[..keys].to_vec(), v)).collect() }
    }
    vec![
        rows("book_subject", 1, 5, subjects),
        rows("book_subject_code", 3, 3, codes),
        rows("book_subject_assignment", 2, 3, assignments),
        rows("book_subject_assignment_evidence", 3, 3, evidence),
        rows("book_unified_concept", 1, 2, concepts),
        rows("book_unified_concept_evidence", 4, 4, unified_evidence),
    ]
}

pub(crate) fn source_subject_version(system_id: &str) -> &'static str {
    match system_id {
        subject_projection::BISAC_SYSTEM_ID => subject_projection::BISAC_VERSION,
        subject_projection::LCC_SYSTEM_ID => subject_projection::LCC_VERSION,
        _ => "external",
    }
}

pub(crate) fn source_mapping_type(system_id: &str) -> &'static str {
    match system_id {
        subject_projection::BISAC_SYSTEM_ID => "exact",
        subject_projection::LCC_SYSTEM_ID => "broader",
        _ => "related",
    }
}

/// Persist canonical metadata and its normalized projections atomically.
///
/// Enrichment stages reload the canonical blob, so updating projections alone
/// would discard evidence when the next stage writes the book.
/// Callers own the transaction and everything around the projection
/// (origins, outbox, placements, cursor).
pub(crate) fn project_prepared_book(tx: &rusqlite::Transaction<'_>, hash: &str, prepared_book: &book_model::BookMetadata, encoded: Vec<u8>) -> Result<(), DatabaseError> {
    let projected = project_book(prepared_book, encoded)?;
    tx.shared_update_canonical_metadata(hash, &projected.encoded)?;
    write_book_projection(tx, hash, &projected)
}

/// Apply CPU-prepared projections without repeating taxonomy matching inside
/// the writer transaction. Identity resolution remains with the caller.
pub(crate) fn write_book_projection(tx: &rusqlite::Transaction<'_>, hash: &str, projected: &PreparedBook) -> Result<(), DatabaseError> {
    let mut book_id: Option<i64> = None;
    tx.shared_apply_remote_book_purged_select(hash, |r| {
        book_id = r.get(0)?;
        Ok(())
    })
    .map_err(DatabaseError::operation)?;
    let book_id = book_id.ok_or(rusqlite::Error::QueryReturnedNoRows).map_err(DatabaseError::operation)?;
    // Every projection below is an unconditional clear-and-reinsert (or a
    // desired/existing reconciliation): idempotent regardless of whether this
    // book's metadata actually changed, so no prior-state comparison drives it.
    tx.shared_purge_book_identifier(book_id).map_err(DatabaseError::operation)?;
    for (position, identifier) in projected.metadata.identifiers.iter().enumerate() {
        tx.insert_book_identifier(book_id, position as i64, &identifier_scheme_label(identifier.scheme()), identifier.value(), identifier.canonical_value().as_deref(), identifier_scope_label(identifier.scope()))
            .map_err(DatabaseError::operation)?;
    }
    // Languages are a projection of the complete winning metadata value.
    tx.execute("DELETE FROM book_language WHERE book_row_id=?1", [book_id])?;
    for (position, language) in projected.metadata.languages.iter().enumerate() {
        tx.insert_book_language(book_id, position as i64, language.as_str()).map_err(DatabaseError::operation)?;
    }
    write_subject_projection(tx, book_id, projected)?;
    for (kind, values) in [("enrichment_isbn", projected.references.clone())] {
        tx.meta_replace_prepared_projections_delete(book_id, kind).map_err(DatabaseError::operation)?;
        for (position, (value, qualifier, secondary)) in values.iter().enumerate() {
            tx.insert_book_value(book_id, kind, position as i64, value, qualifier.as_deref(), secondary.as_deref()).map_err(DatabaseError::operation)?;
        }
    }
    Ok(())
}

mod pull;
pub(crate) use pull::rebuild_remote_directories;

pub(crate) mod registers;
pub(crate) use pull::project_dirty;
pub(crate) use registers::commit;

/// Reconcile derived subject rows without changing canonical metadata or other projections.
pub(crate) fn write_subject_projection(tx: &rusqlite::Transaction<'_>, book_id: i64, projected: &PreparedBook) -> Result<(), DatabaseError> {
    // Manual subject paths are legacy canonical evidence. Retain their stable
    // concept IDs across renames/reparenting without rewriting synchronized metadata.
    let mut adjusted;
    let projected = if projected.subject_codes.iter().any(|c| c.system_id()==subject_projection::UNIFIED_SYSTEM_ID) {
        adjusted=projected.clone();
        for (index,code) in adjusted.subject_codes.iter().enumerate() {
            if code.system_id()!=subject_projection::UNIFIED_SYSTEM_ID {continue;}
            let mut retained=Vec::new();
            for position in code.evidence_positions() {
                let mut query=tx.prepare("SELECT DISTINCT e.concept_id,p.path FROM book_unified_concept_evidence e JOIN curated.unified_concept_paths p ON p.concept_id=e.concept_id WHERE e.book_row_id=?1 AND e.subject_position=?2 AND e.source_system_id='unified' AND e.source_code=?3")?;
                retained.extend(query.query_map(rusqlite::params![book_id,*position as i64,code.code()],|r|Ok((index,r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?);
            }
            if !retained.is_empty() { adjusted.concepts.retain(|(i,_,_)| *i!=index); adjusted.concepts.extend(retained); }
        }
        adjusted.classification_rows=prepare_classification_rows(&adjusted);
        &adjusted
    } else {projected};

    {
        for assignment in &projected.assignments {
            if assignment.system_id() == subject_projection::BISAC_SYSTEM_ID {
                // These library-local FK targets are separate from the attached
                // unified taxonomy. Materialize the assigned node and ancestors
                // in the same transaction before inserting its book assignment.
                let system = subject_projection::bisac_subject_system();
                tx.ensure_bisac_system(system.version(), assignment.matcher_version())?;
                let mut path = Some(assignment.subject_path());
                while let Some(current) = path {
                    let node = system.nodes().iter().find(|node| node.path() == current).ok_or_else(|| DatabaseError::message(format!("unknown BISAC subject path: {current}")))?;
                    tx.ensure_bisac_node(node.path(), node.parent_path().unwrap_or(""), node.name(), node.code())?;
                    path = node.parent_path();
                }
            } else {
                let components: Vec<&str> = assignment.subject_path().split(" / ").map(str::trim).filter(|component| !component.is_empty()).collect();
                tx.meta_ensure_external_subject_nodes_insert(assignment.system_id()).map_err(DatabaseError::operation)?;
                let mut path = String::new();
                let mut parent_path = String::new();
                for component in components {
                    if !path.is_empty() {
                        path.push_str(" / ");
                    }
                    path.push_str(component);
                    tx.meta_ensure_external_subject_nodes_insert_2(assignment.system_id(), &path, &parent_path, component).map_err(DatabaseError::operation)?;
                    parent_path.clone_from(&path);
                }
                tx.meta_ensure_external_subject_nodes_update(assignment.system_id(), assignment.matcher_version()).map_err(DatabaseError::operation)?;
            }
        }
        for (index, concept, path) in &projected.concepts {
            let code = &projected.subject_codes[*index];
            let version = source_subject_version(code.system_id());
            tx.apply_insert(code.system_id(), version, code.code()).map_err(DatabaseError::operation)?;
            tx.apply_insert_2(code.system_id(), version, code.code(), *concept, source_mapping_type(code.system_id()), subject_projection::UNIFIED_MATCHER_VERSION).map_err(DatabaseError::operation)?;
            tx.apply_insert_3(code.system_id(), code.code(), path, subject_projection::UNIFIED_MATCHER_VERSION).map_err(DatabaseError::operation)?;
        }
        for rows in &projected.classification_rows {
            let (select_sql, delete_sql, insert_sql): (&str, &str, &str) = match rows.table {
                "book_subject" => (include_str!("sql/reconcile/book_subject_select.sql"), include_str!("sql/reconcile/book_subject_delete.sql"), include_str!("sql/reconcile/book_subject_insert.sql")),
                "book_subject_code" => (include_str!("sql/reconcile/book_subject_code_select.sql"), include_str!("sql/reconcile/book_subject_code_delete.sql"), include_str!("sql/reconcile/book_subject_code_insert.sql")),
                "book_subject_assignment" => {
                    (include_str!("sql/reconcile/book_subject_assignment_select.sql"), include_str!("sql/reconcile/book_subject_assignment_delete.sql"), include_str!("sql/reconcile/book_subject_assignment_insert.sql"))
                }
                "book_subject_assignment_evidence" => (
                    include_str!("sql/reconcile/book_subject_assignment_evidence_select.sql"),
                    include_str!("sql/reconcile/book_subject_assignment_evidence_delete.sql"),
                    include_str!("sql/reconcile/book_subject_assignment_evidence_insert.sql"),
                ),
                "book_unified_concept" => (include_str!("sql/reconcile/book_unified_concept_select.sql"), include_str!("sql/reconcile/book_unified_concept_delete.sql"), include_str!("sql/reconcile/book_unified_concept_insert.sql")),
                "book_unified_concept_evidence" => {
                    (include_str!("sql/reconcile/book_unified_concept_evidence_select.sql"), include_str!("sql/reconcile/book_unified_concept_evidence_delete.sql"), include_str!("sql/reconcile/book_unified_concept_evidence_insert.sql"))
                }
                _ => return Err(DatabaseError::message("unknown classification table")),
            };
            let existing: std::collections::BTreeMap<Vec<Cell>, Vec<Cell>> = tx
                .prepare_cached(select_sql)
                .map_err(DatabaseError::operation)?
                .query_map([book_id], |r| {
                    (0..rows.width)
                        .map(|i| match r.get_ref(i)? {
                            rusqlite::types::ValueRef::Null => Ok(Cell::Null),
                            rusqlite::types::ValueRef::Integer(v) => Ok(Cell::Integer(v)),
                            rusqlite::types::ValueRef::Text(v) => Ok(Cell::Text(std::str::from_utf8(v).map_err(|error| invalid_text_column(i, error.to_string()))?.into())),
                            _ => Err(invalid_text_column(i, "unexpected classification value")),
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(DatabaseError::operation)?
                .collect::<Result<Vec<_>, rusqlite::Error>>()
                .map_err(DatabaseError::operation)?
                .into_iter()
                .map(|v| (v[..rows.keys].to_vec(), v))
                .collect();
            for key in existing.keys().filter(|key| !rows.desired.contains_key(*key)) {
                tx.prepare_cached(delete_sql).map_err(DatabaseError::operation)?.execute(rusqlite::params_from_iter(std::iter::once(Cell::Integer(book_id)).chain(key.iter().cloned()))).map_err(DatabaseError::operation)?;
            }
            for (key, values) in &rows.desired {
                if existing.get(key) == Some(values) {
                    continue;
                }
                tx.prepare_cached(insert_sql).map_err(DatabaseError::operation)?.execute(rusqlite::params_from_iter(std::iter::once(Cell::Integer(book_id)).chain(values.iter().cloned()))).map_err(DatabaseError::operation)?;
            }
        }
    }
    Ok(())
}
