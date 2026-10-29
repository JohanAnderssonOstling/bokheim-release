//! Local import commits: inspection projections, placements, observations.

use crate::SqlParam;
use crate::contributors::{ContributorNameMatcher, resolve_credits};
use crate::placements::directory_relative_path;
use crate::shared_sql::SharedSql;
use crate::sync::apply::registers::{canonical_body, record_local};
use crate::sync::{normalized_description, toc_entry_count, unix_millis};
use crate::transfer::BookTransferState;
use crate::{Database, DatabaseError};
use include_sqlite_sql::include_sql;

// ---- commit ----
// Import commits share transaction-scoped identity and projection primitives.
//
// The backend orchestrates everything outside storage: validation, staging,
// inspection, leases, filesystem book, and thumbnail/file existence
// checks arrive as plain data. This method verifies the destination, records
// the inspection projection, the placement, and the book observation
// atomically. Shared write helpers use the caller-owned transaction.
use crate::books::audiobook::navigation_from_chapters;

include_sql!("src/import/sql/import_commit.sql");
use book_model::{BookRecord, BookTocDocument};
use library_replica::{BookAddition, BookFacts, LibraryStateEntry};
use sync_common::{BookId, DirId, StateKey};

/// Snapshot in for one import commit. All filesystem truth (book
/// outcome, thumbnail need, restore paths) is resolved by the caller.
pub struct ImportCommit {
    pub parent_id: DirId,
    pub file_name: String,
    pub format: book_model::BookFormat,
    pub content_hash: BookId,
    pub size_bytes: u64,
    pub inspection_version: i64,
    pub inspection: Option<book_inspection::InspectedBook>,
    /// Filesystem book outcome, recorded verbatim.
    pub published: library_model::PublishedStagedBook,
    pub request_thumbnail: bool,
    pub restore_paths: Vec<String>,
}

/// Typed, validated file metadata prepared before acquiring the database writer.
pub(crate) struct PreparedInspection {
    metadata: book_model::BookRecord,
    navigation: Option<BookTocDocument>,
    recording: Option<metadata_contract::audible::LookupRequest>,
    pdf: Option<pdf_view_common::PdfReaderMetadata>,
}

pub(crate) fn provider_addition(prepared: PreparedInspection, format: book_model::BookFormat, added_at: i64) -> Result<BookAddition, DatabaseError> {
    Ok(BookAddition {
        facts: BookFacts { added_at: u64::try_from(added_at).map_err(DatabaseError::operation)?, format },
        record: prepared.metadata, navigation: prepared.navigation, recording: prepared.recording, pdf: prepared.pdf,
        enrichment: Some(library_replica::EnrichmentResult::NoMatch),
    })
}

pub(crate) fn prepare_inspection(inspection: &book_inspection::InspectedBook, content_hash: &BookId, format: book_model::BookFormat) -> Result<PreparedInspection, DatabaseError> {
    let mut metadata = inspection.metadata.clone();
    metadata.description = normalized_description(&metadata.description);
    // Empty navigation still records that inspection found no entries.
    let navigation = if let Some(audio) = &inspection.audiobook {
        i64::try_from(audio.duration_ms).map_err(|_| DatabaseError::message("audiobook duration exceeds SQLite integer range"))?;
        Some(BookTocDocument { entries: navigation_from_chapters(&audio.chapters), duration_ms: Some(audio.duration_ms), tracks: (!audio.tracks.is_empty()).then(|| audio.tracks.clone()) })
    } else {
        match format {
            book_model::BookFormat::Epub | book_model::BookFormat::Mobi | book_model::BookFormat::Pdf | book_model::BookFormat::Fb2 | book_model::BookFormat::Cbz => Some(BookTocDocument::entries(inspection.toc.clone())),
            book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder => None,
        }
    };
    if let Some(navigation) = &navigation {
        i64::try_from(toc_entry_count(&navigation.entries)).map_err(|_| DatabaseError::message("table of contents entry count exceeds SQLite integer range"))?;
    }
    let pdf = inspection.pdf.clone();
    if pdf.as_ref().is_some_and(|pdf| !pdf.valid_for(&content_hash.to_hex())) {
        return Err(DatabaseError::message("PDF ingestion content hash or metadata is invalid"));
    }
    let recording = inspection.audiobook.as_ref().map(|audio| audio.audible_lookup_request());
    Ok(PreparedInspection { metadata, navigation, recording, pdf })
}

impl Database {
    /// Whether a book already has an inspection projection at this exact
    /// format and inspection version, so the caller can skip re-inspecting it.
    pub fn has_import_inspection(&self, hash: &BookId, format: book_model::BookFormat, inspection_version: i64) -> Result<bool, DatabaseError> {
        let mut inspected = false;
        self.connection.has_import_inspection_select(hash, format as i64, inspection_version, |row| {
            inspected = row.get(0)?;
            Ok(())
        })?;
        Ok(inspected)
    }

    /// Committing this transaction is the import's success point. Callers
    /// resolve all fallible filesystem data before it.
    pub fn commit_import(&self, import: ImportCommit) -> Result<(), DatabaseError> {
        self.commit_import_with_contributor_matching(import, None)
    }

    /// Apply the caller's provisional contributor matching rule atomically with the import.
    pub fn commit_import_with_contributor_matching(&self, import: ImportCommit, matcher: Option<ContributorNameMatcher>) -> Result<(), DatabaseError> {
        self.commit_import_with_catalog_answer(import, matcher, None)
    }

    /// Save matching catalog metadata before the imported draft can be enriched.
    pub fn commit_import_with_catalog_answer(&self, import: ImportCommit, matcher: Option<ContributorNameMatcher>, catalog_record: Option<BookRecord>) -> Result<(), DatabaseError> {
        let content_hash = import.content_hash;
        {
            let imports: Vec<ImportCommit> = vec![import];

            if imports.is_empty() {
                return Ok(());
            }
            // Pure CPU work (normalization, TOC/chapter JSON, PDF validation)
            // runs before reserving the writer. Destination liveness is checked
            // inside the transaction: another connection may delete it meanwhile.
            let mut prepared_imports = Vec::with_capacity(imports.len());
            for import in imports {
                let prepared = import.inspection.as_ref().map(|inspection| prepare_inspection(inspection, &import.content_hash, import.format)).transpose()?;
                prepared_imports.push((import, prepared));
            }
            self.with_write_transaction(|tx| {
                tx.pragma_update(None, "defer_foreign_keys", "ON").map_err(DatabaseError::operation)?;
                let mut current: Option<String> = None;
                tx.shared_scanner_sequence(|row| {
                    current = row.get(0)?;
                    Ok(())
                })
                .map_err(DatabaseError::operation)?;
                let current = current.unwrap_or_default().parse::<i64>().map_err(|error| DatabaseError::message(format!("scan sequence is not readable: {error}")))?;
                let scan_id = current.checked_add(1).ok_or_else(|| DatabaseError::message("scan sequence overflow"))?;
                tx.scan_seq_update(scan_id).map_err(DatabaseError::operation)?;
                for (import, prepared) in prepared_imports {
                    if directory_relative_path(tx, &import.parent_id)?.is_none() {
                        return Err(DatabaseError::message(format!("library folder does not exist: {}", import.parent_id)));
                    }
                    commit_one(tx, import, prepared, matcher)?;
                }
                if let Some(record) = catalog_record {
                    crate::enrichment::facts::record_catalog_answer(tx, &content_hash, record)?;
                }
                Ok(crate::transactions::WriteOutcome::Commit(()))
            })
        }
    }

    pub fn commit_transfer_import(&self, import: ImportCommit, source: BookTransferState) -> Result<bool, DatabaseError> {
        self.write(|tx| commit_one_with_transfer(tx, import, None, None, Some(source)))
    }

    /// Placement rows for one book with their remembered materialized paths.
    /// The orchestrator filters these against the filesystem (without holding
    /// the writer) and hands the restore set back to the commit.
    pub fn import_restore_candidates(&self, hash: &BookId) -> Result<Vec<(library_model::RelativeBookPath, Option<String>)>, DatabaseError> {
        let mut rows = Vec::new();
        self.connection
            .restore_candidates_select(hash, |row| {
                rows.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?));
                Ok(())
            })
            .map_err(DatabaseError::operation)?;
        let mut folders = crate::placements::FolderPaths::new(&self.connection);
        let mut candidates = Vec::new();
        for (dir_id, file_name, projected) in rows {
            if let Some(path) = folders.placement(&dir_id, &file_name)? {
                candidates.push((path, projected));
            }
        }
        Ok(candidates)
    }
}

/// One book's identity resolution and writes, inside a transaction the
/// caller owns and commits. Acquiring the writer before this runs means a
/// directory can vanish between [`Database::commit_imports`]'s pre-check and
/// here, so the destination is re-validated.
pub(crate) fn commit_one(tx: &rusqlite::Transaction<'_>, import: ImportCommit, prepared: Option<PreparedInspection>, matcher: Option<ContributorNameMatcher>) -> Result<(), DatabaseError> {
    commit_one_with_transfer(tx, import, prepared, matcher, None).map(|_| ())
}

fn commit_one_with_transfer(tx: &rusqlite::Transaction<'_>, import: ImportCommit, prepared: Option<PreparedInspection>, matcher: Option<ContributorNameMatcher>, transfer: Option<BookTransferState>) -> Result<bool, DatabaseError> {
    let ImportCommit { parent_id, file_name: _file_name, format, content_hash, size_bytes, inspection_version, inspection: _inspection, published, request_thumbnail, restore_paths } = import;
    let hash = &content_hash;
    if directory_relative_path(tx, &parent_id)?.is_none() {
        return Err(DatabaseError::message(format!("library folder does not exist: {parent_id}")));
    }
    if prepared.is_none() && transfer.is_none() {
        let mut inspected: Option<bool> = None;
        tx.has_import_inspection_select(hash, format as i64, inspection_version, |row| {
            inspected = row.get(0)?;
            Ok(())
        })
        .map_err(DatabaseError::operation)?;
        let inspected = inspected.ok_or(rusqlite::Error::QueryReturnedNoRows).map_err(DatabaseError::operation)?;
        if !inspected {
            return Err(DatabaseError::message("book changed during import; retry the import"));
        }
    }
    let placement_created = tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id WHERE bd.dir_id=?1 AND b.content_hash=?2 AND bd.deleted_at IS NULL AND b.deleted_at IS NULL)",
        rusqlite::params![parent_id.to_string(), hash],
        |row| row.get::<_, bool>(0),
    )?;
    if let Some(source) = transfer {
        let BookTransferState { content_hash: source_hash, addition, reading, chapter_read, annotations, .. } = source;
        if source_hash != content_hash || addition.facts.format != format {
            return Err(DatabaseError::message("transferred book identity does not match its bytes"));
        }
        let has_facts: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM local_book_draft WHERE content_hash=?1) OR EXISTS(SELECT 1 FROM sync_book_record WHERE state_kind='book_add' AND content_hash=?1)", [hash], |row| row.get(0))?;
        tx.ingestion_ensure_book(hash)?;
        record_local(tx, LibraryStateEntry::BookLifecycle { content_hash, value: library_replica::BookLifecycleState::Present })?;
        if !has_facts {
            if addition.enrichment.is_some() {
                record_local(tx, LibraryStateEntry::BookAdd { content_hash, value: addition })?;
            } else {
                crate::sync::apply::records::stage(tx, hash, addition)?;
            }
        }
        crate::transfer::merge_transfer_progress(tx, content_hash, reading, chapter_read, annotations)?;
    }
    if let Some(prepared) = prepared {
        // The first inspection supplies the draft. Projection owns all
        // metadata writes; reimports retain the same immutable file facts.
        let PreparedInspection { metadata, navigation, recording, pdf } = prepared;
        // One immutable addition carries every file fact; lifecycle and
        // placement remain independently mutable.
        tx.ingestion_ensure_book(&hash)?;
        let added_at: i64 = tx.ingestion_added_at(&hash, &(unix_millis()?), |row| row.get(0))?;
        record_local(tx, LibraryStateEntry::BookLifecycle { content_hash, value: library_replica::BookLifecycleState::Present })?;
        // Re-imports may change placements and local observations, but cannot
        // rewrite the immutable addition for the same file hash.
        if canonical_body(tx, "book_add", &StateKey::Book(*hash), "")?.is_none() {
            let mut metadata = metadata;
            metadata.contributors = resolve_credits(tx, &metadata.contributors, matcher)?.into_iter().map(|(_, credit)| credit).collect();
            crate::sync::apply::records::stage(tx, hash, BookAddition { facts: BookFacts { added_at: u64::try_from(added_at).map_err(DatabaseError::operation)?, format }, record: metadata, navigation, pdf, recording, enrichment: None })?;
        }
        tx.record_import_inspection(hash, format as i64, inspection_version).map_err(DatabaseError::operation)?;
    }
    // The published file name is authoritative: connection-free book
    // may have uniquified it while this transaction was not held. Browser
    // imports have no placement filesystem, so they skip this recheck.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let conflict: bool = tx.shared_book_name_conflict(&parent_id.to_string(), &library_replica::portable_name_key(&published.name), hash, |row| row.get(0)).map_err(DatabaseError::operation)?;
        if conflict {
            return Err(DatabaseError::message("destination filename changed during import; retry the import"));
        }
    }
    crate::projection::place_book(tx, &parent_id, hash, &published.name)?;
    // File recording and later imports in this batch read the live placement.
    crate::projection::project_dirty(tx)?;
    let revision = if published.published {
        crate::native::observations::Revision::Local { size: i64::try_from(size_bytes).map_err(|_| DatabaseError::message("book size exceeds SQLite range"))? }
    } else {
        crate::native::observations::Revision::Retained
    };
    crate::native::observations::record_file(tx, &parent_id.to_string(), hash, &published.relative_path, published.fingerprint.as_deref(), revision)?;
    if request_thumbnail {
        tx.shared_request_thumbnail(hash).map_err(DatabaseError::operation)?;
    }
    for path in &restore_paths {
        tx.shared_placements_queue_file_work("materialize", hash, path).map_err(DatabaseError::operation)?;
    }
    Ok(placement_created)
}

include_sqlite_sql::include_sql!("src/import/sql/ingestion.sql");
