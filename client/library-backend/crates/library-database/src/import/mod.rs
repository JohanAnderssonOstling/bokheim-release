//! Local import commits: inspection projections, placements, observations.

use crate::shared_sql::SharedSql;
use crate::sync::{normalized_description, toc_entry_count, unix_millis};
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
use crate::books::audiobook::AudiobookSql;
use crate::books::audiobook::navigation_from_chapters;
use crate::sync::SyncApplySql;

include_sql!("src/import/sql/import_commit.sql");
use sync_common::{ContentHash, DirId};

/// Filesystem book outcome, recorded verbatim. Produced by the
/// backend's connection-free publish step from a database snapshot.
pub struct PublishedImport {
    pub name: String,
    pub relative_path: String,
    pub published: bool,
    pub fingerprint: Option<String>,
}

/// Snapshot in for one import commit. All filesystem truth (book
/// outcome, thumbnail need, restore paths) is resolved by the caller.
pub struct ImportCommit {
    pub parent_id: DirId,
    pub file_name: String,
    pub format: book_model::BookFormat,
    pub content_hash: ContentHash,
    pub checksum: ContentHash,
    pub size_bytes: u64,
    pub inspection_version: i64,
    pub inspection: Option<book_metadata::InspectedBook>,
    pub published: PublishedImport,
    pub request_thumbnail: bool,
    pub restore_paths: Vec<String>,
}

/// Chapter-derived table of contents, precomputed regardless of whether the
/// commit ends up preserving an existing database-only chapter plan instead.
pub(crate) struct PreparedAudiobookInsert {
    duration_ms: i64,
    tracks_json: Option<String>,
    request: metadata_contract::audible::LookupRequest,
    request_json: String,
    chapters_entry_count: i64,
    chapters_toc_json: String,
}

/// Everything [`Database::commit_import`] can derive from an [`book_metadata::InspectedBook`]
/// without touching storage: title/description normalization, subject projection,
/// encoded TOC and audiobook chapter JSON, and PDF checksum validation. Computed before
/// the transaction opens so the writer is held only for identity resolution
/// and the actual writes, not for CPU-bound parsing and serialization.
pub(crate) struct PreparedInspection {
    metadata: book_model::BookRecord,
    projection: crate::sync::apply::PreparedBook,
    description: String,
    toc: Option<(i64, String)>,
    audiobook: Option<PreparedAudiobookInsert>,
    pdf_bytes: Option<Vec<u8>>,
}

pub(crate) fn prepare_inspection(connection: &rusqlite::Connection, hash: &ContentHash, inspection: &book_metadata::InspectedBook, checksum: &ContentHash, format: book_model::BookFormat) -> Result<PreparedInspection, DatabaseError> {
    let mut timing = crate::timing::PhaseTimer::new("prepare_inspection");
    let mut metadata = inspection.metadata.clone();
    // A content hash identifies unchanged bytes. A new inspector version must
    // not erase identifiers or classifications learned after the prior scan.
    connection.shared_book_metadata(hash.as_str(), |row| {
        let raw: Vec<u8> = row.get(0)?;
        let previous = crate::sync::decode_book_metadata(&raw).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        for identifier in previous.identifiers {
            if !metadata.book.identifiers.contains(&identifier) {
                metadata.book.identifiers.push(identifier);
            }
        }
        for subject in previous.subjects {
            if (subject.source().starts_with("metadata:") || subject.source().starts_with("openlibrary:")) && !metadata.book.subjects.contains(&subject) {
                metadata.book.subjects.push(subject);
            }
        }
        Ok(())
    })?;
    // Recover classifications written by older versions only to projections.
    connection.shared_book_subjects(hash.as_str(), |row| {
        let subject = book_model::BookSubject::new(None, row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get(2)?, row.get(3)?).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        if !(subject.source().starts_with("metadata:") || subject.source().starts_with("openlibrary:")) {
            return Ok(());
        }
        if !metadata.book.subjects.iter().any(|existing| existing.name() == subject.name() && existing.source() == subject.source() && existing.authority() == subject.authority() && existing.code() == subject.code()) {
            metadata.book.subjects.push(subject);
        }
        Ok(())
    })?;
    book_enrichment::title_identity::normalize(&mut metadata).map_err(|error| DatabaseError::message(format!("book title is not readable: {error}")))?;
    let description = normalized_description(&metadata.description);
    timing.mark("normalize_metadata_and_description");
    // A document that yielded nothing is still recorded, so that nothing
    // parses it again hoping for a different answer.
    let toc = match format {
        book_model::BookFormat::Epub | book_model::BookFormat::Mobi | book_model::BookFormat::Pdf => {
            let entry_count = i64::try_from(toc_entry_count(&inspection.toc)).map_err(|_| DatabaseError::message("table of contents entry count exceeds SQLite integer range"))?;
            let toc_json = serde_json::to_string(&inspection.toc).map_err(DatabaseError::operation)?;
            Some((entry_count, toc_json))
        }
        book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder => None,
    };
    let audiobook = inspection
        .audiobook
        .as_ref()
        .map(|audiobook| -> Result<PreparedAudiobookInsert, DatabaseError> {
            let duration_ms = i64::try_from(audiobook.duration_ms).map_err(|_| DatabaseError::message("audiobook duration exceeds SQLite integer range"))?;
            let entries = navigation_from_chapters(&audiobook.chapters);
            let chapters_entry_count = i64::try_from(toc_entry_count(&entries)).map_err(|_| DatabaseError::message("table of contents entry count exceeds SQLite integer range"))?;
            let chapters_toc_json = serde_json::to_string(&entries).map_err(DatabaseError::operation)?;
            let tracks_json = (!audiobook.tracks.is_empty()).then(|| serde_json::to_string(&audiobook.tracks)).transpose().map_err(DatabaseError::operation)?;
            let request = audiobook.audible_lookup_request();
            let request_json = serde_json::to_string(&request).map_err(DatabaseError::operation)?;
            Ok(PreparedAudiobookInsert { duration_ms, tracks_json, request, request_json, chapters_entry_count, chapters_toc_json })
        })
        .transpose()?;
    let pdf_bytes = inspection
        .pdf
        .as_ref()
        .map(|pdf| -> Result<Vec<u8>, DatabaseError> {
            if !pdf.valid_for(checksum.as_str()) {
                return Err(DatabaseError::message("PDF ingestion checksum or metadata is invalid"));
            }
            serde_json::to_vec(pdf).map_err(DatabaseError::operation)
        })
        .transpose()?;
    timing.mark("encode_reader_metadata");
    let projection = crate::sync::apply::project_book(&metadata.book, Vec::new())?;
    timing.mark("project_book");
    Ok(PreparedInspection { metadata, projection, description, toc, audiobook, pdf_bytes })
}

impl Database {
    /// Whether a book already has an inspection projection at this exact
    /// format and inspection version, so the caller can skip re-inspecting it.
    pub fn has_import_inspection(&self, hash: &ContentHash, extension: &str, inspection_version: i64) -> Result<bool, DatabaseError> {
        let mut inspected = false;
        self.connection.has_import_inspection_select(hash.as_str(), extension, inspection_version, |row| {
            inspected = row.get(0)?;
            Ok(())
        })?;
        Ok(inspected)
    }

    /// Committing this transaction is the import's success point. Callers
    /// resolve all fallible filesystem data before it.
    pub fn commit_import(&self, import: ImportCommit) -> Result<(), DatabaseError> {
        self.commit_imports(vec![import])
    }

    /// Same as [`Database::commit_import`], for many books in one shared
    /// transaction (a scan pass discovering hundreds of files at once should
    /// not open hundreds of writer transactions). Every member shares one
    /// `scan_id`, matching the scanner's own one-generation-per-pass marker;
    /// a member's failure rolls back the whole batch, the same all-or-nothing
    /// semantics a filesystem scan apply already has.
    pub fn commit_imports(&self, imports: Vec<ImportCommit>) -> Result<(), DatabaseError> {
        if imports.is_empty() {
            return Ok(());
        }
        // Pure CPU work (normalization, TOC/chapter JSON, PDF validation)
        // runs before reserving the writer. Destination liveness is checked
        // inside the transaction: another connection may delete it meanwhile.
        let mut prepared_imports = Vec::with_capacity(imports.len());
        for import in imports {
            let prepared = import.inspection.as_ref().map(|inspection| prepare_inspection(&self.connection, &import.content_hash, inspection, &import.checksum, import.format)).transpose()?;
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
                let mut live = false;
                tx.live_destination_select(&import.parent_id.to_string(), |row| {
                    live = row.get(0)?;
                    Ok(())
                })
                .map_err(DatabaseError::operation)?;
                if !live {
                    return Err(DatabaseError::message(format!("library folder does not exist: {}", import.parent_id)));
                }
                commit_one(tx, &self.metadata_batch_flag, scan_id, import, prepared)?;
            }
            Ok(crate::transactions::WriteOutcome::Commit(()))
        })
    }

    /// Placement rows for one book with their remembered materialized paths.
    /// The orchestrator filters these against the filesystem (without holding
    /// the writer) and hands the restore set back to the commit.
    pub fn import_restore_candidates(&self, hash: &ContentHash) -> Result<Vec<(String, Option<String>)>, DatabaseError> {
        let mut candidates = Vec::new();
        self.connection
            .restore_candidates_select(hash.as_str(), |row| {
                candidates.push((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?));
                Ok(())
            })
            .map_err(DatabaseError::operation)?;
        Ok(candidates)
    }
}

/// One book's identity resolution and writes, inside a transaction the
/// caller owns and commits. Acquiring the writer before this runs means a
/// directory can vanish between [`Database::commit_imports`]'s pre-check and
/// here, so the destination is re-validated.
pub(crate) fn commit_one(tx: &rusqlite::Transaction<'_>, batch_flag: &std::sync::atomic::AtomicBool, scan_id: i64, import: ImportCommit, prepared: Option<PreparedInspection>) -> Result<(), DatabaseError> {
    let mut timing = crate::timing::PhaseTimer::new("import_book");
    let ImportCommit { parent_id, file_name: _file_name, format, content_hash, checksum, size_bytes, inspection_version, inspection: _inspection, published, request_thumbnail, restore_paths } = import;
    let hash = content_hash.as_str();
    let mut live: Option<bool> = None;
    tx.live_destination_select(&parent_id.to_string(), |row| {
        live = row.get(0)?;
        Ok(())
    })
    .map_err(DatabaseError::operation)?;
    let live = live.ok_or(rusqlite::Error::QueryReturnedNoRows).map_err(DatabaseError::operation)?;
    if !live {
        return Err(DatabaseError::message(format!("library folder does not exist: {parent_id}")));
    }
    if prepared.is_none() {
        let mut inspected: Option<bool> = None;
        tx.has_import_inspection_select(hash, format.canonical_extension(), inspection_version, |row| {
            inspected = row.get(0)?;
            Ok(())
        })
        .map_err(DatabaseError::operation)?;
        let inspected = inspected.ok_or(rusqlite::Error::QueryReturnedNoRows).map_err(DatabaseError::operation)?;
        if !inspected {
            return Err(DatabaseError::message("book changed during import; retry the import"));
        }
    }
    timing.mark("validate_destination");
    if let Some(prepared) = prepared {
        // Fresh inspection projects exactly like a rescan: normalize the
        // title identity, resolve contributor and publisher agents with
        // provisional-name reuse, then replace every projection row.
        let PreparedInspection { mut metadata, projection, description, toc, audiobook, pdf_bytes } = prepared;
        let stored_contributors = crate::contributors::resolve_import_credits(tx, hash, &metadata.contributors)?;
        let prepared_publishers = crate::contributors::resolve_publishers(tx, &metadata.book.publishers, crate::contributors::IdentityPolicy::ReuseProvisional)?;
        let mut prepared_book = metadata.book.clone();
        prepared_book.publishers = prepared_publishers;
        prepared_book.infer_embedded_subject_isbns().map_err(DatabaseError::operation)?;
        let encoded = sync_common::wire::encode(&prepared_book).map_err(DatabaseError::operation)?;
        let added_at = unix_millis()?;
        timing.mark("resolve_identities_and_encode");
        // The local relationship rows reference SQLite's integer key, so establish
        // the content-hash boundary row before projecting contributors into it.
        tx.prepare_cached(include_str!("sql/upsert_scanned_book.sql"))?.execute(rusqlite::named_params! {
            ":content_hash": hash, ":title": &metadata.title, ":subtitle": metadata.subtitle.as_deref(),
            ":book_metadata": &encoded, ":added_at": added_at, ":format": format.canonical_extension(),
        })?;
        timing.mark("write_book_row");
        tx.set_description(hash, &description).map_err(DatabaseError::operation)?;
        tx.shared_enrichment_mark_description_scanned(hash).map_err(DatabaseError::operation)?;
        timing.mark("write_description");
        crate::transactions::with_metadata_batch(tx, batch_flag, || crate::contributors::write_credits(tx, hash, &stored_contributors))?;
        timing.mark("contributors_and_metadata_payload");
        crate::sync::apply::write_book_projection(tx, hash, &projection)?;
        timing.mark("write_metadata_projection");
        if let Some(audiobook) = audiobook {
            tx.audiobook_upsert_metadata(hash, audiobook.duration_ms).map_err(DatabaseError::operation)?;
            if let Some(tracks_json) = &audiobook.tracks_json {
                tx.audiobook_upsert_tracks(hash, tracks_json).map_err(DatabaseError::operation)?;
            }
            // Preserve a database-only chapter plan while the imported evidence is unchanged.
            let mut applied_request: Option<String> = None;
            tx.audiobook_applied_request_select(hash, |row| {
                applied_request = row.get(0)?;
                Ok(())
            })
            .map_err(DatabaseError::operation)?;
            let preserve_chapters = applied_request.and_then(|json| serde_json::from_str::<metadata_contract::audible::LookupRequest>(&json).ok()).is_some_and(|applied| applied == audiobook.request);
            if !preserve_chapters {
                tx.shared_audiobook_upsert_toc(hash, audiobook.chapters_entry_count, &audiobook.chapters_toc_json).map_err(DatabaseError::operation)?;
            }
            let now = unix_millis()?.saturating_div(1000);
            tx.audible_discover_insert(hash, metadata_contract::audible::POLICY_VERSION, &audiobook.request_json, now).map_err(DatabaseError::operation)?;
        }
        if let Some((entry_count, toc_json)) = toc {
            tx.shared_audiobook_upsert_toc(hash, entry_count, &toc_json).map_err(DatabaseError::operation)?;
        }
        if let Some(pdf_bytes) = pdf_bytes {
            tx.pdf_reader_metadata_upsert(hash, checksum.as_str(), &pdf_bytes).map_err(DatabaseError::operation)?;
        }
        tx.record_import_inspection(hash, format.canonical_extension(), inspection_version).map_err(DatabaseError::operation)?;
        timing.mark("reader_metadata_and_inspection");
    }
    // The published file name is authoritative: connection-free book
    // may have uniquified it while this transaction was not held. Browser
    // imports have no placement filesystem, so they skip this recheck.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut conflict: Option<bool> = None;
        tx.publish_conflict_select(&parent_id.to_string(), &library_replica::portable_name_key(&published.name), hash, |row| {
            conflict = row.get(0)?;
            Ok(())
        })
        .map_err(DatabaseError::operation)?;
        let conflict = conflict.ok_or(rusqlite::Error::QueryReturnedNoRows).map_err(DatabaseError::operation)?;
        if conflict {
            return Err(DatabaseError::message("destination filename changed during import; retry the import"));
        }
    }
    timing.mark("check_placement_collision");
    tx.prepare_cached(include_str!("sql/insert_book_dir.sql"))?.execute(rusqlite::named_params! {
        ":dir_id": parent_id.to_string(), ":content_hash": hash, ":file_name": &published.name,
        ":local_hash": "", ":last_scan": scan_id, ":is_downloaded": 1,
    })?;
    timing.mark("write_placement");
    #[cfg(not(target_arch = "wasm32"))]
    tx.shared_scanner_publish_projection(hash, &parent_id.to_string(), &published.relative_path).map_err(DatabaseError::operation)?;
    if published.published {
        if let Some(fingerprint) = published.fingerprint.as_deref() {
            tx.publish_update_2(&parent_id.to_string(), hash, fingerprint).map_err(DatabaseError::operation)?;
        }
    }
    timing.mark("record_path_and_fingerprint");
    if request_thumbnail {
        tx.shared_request_thumbnail(hash).map_err(DatabaseError::operation)?;
    }
    timing.mark("queue_thumbnail");
    if published.published {
        let size = i64::try_from(size_bytes).map_err(|_| DatabaseError::message("book size exceeds SQLite range"))?;
        let mut previous: Option<String> = None;
        tx.shared_current_checksum_select(&parent_id.to_string(), hash, |row| {
            previous = row.get(0)?;
            Ok(())
        })
        .map_err(DatabaseError::operation)?;
        if previous.as_deref() != Some(checksum.as_str()) {
            let unseen = tx.shared_observe_local_book_insert(hash, checksum.as_str(), size, "local").map_err(DatabaseError::operation)? != 0;
            let changed = previous.is_some();
            if unseen || changed {
                tx.shared_scanner_enqueue_upload(hash, checksum.as_str(), size).map_err(DatabaseError::operation)?;
                tx.shared_rejected_book_cleanup(hash).map_err(DatabaseError::operation)?;
                tx.shared_scanner_enqueue_asset_work(hash).map_err(DatabaseError::operation)?;
            }
            let origin: String = if changed {
                "local".to_owned()
            } else {
                let mut origin: Option<String> = None;
                tx.shared_version_origin_select(hash, checksum.as_str(), |row| {
                    origin = row.get(0)?;
                    Ok(())
                })
                .map_err(DatabaseError::operation)?;
                origin.ok_or(rusqlite::Error::QueryReturnedNoRows).map_err(DatabaseError::operation)?
            };
            tx.shared_current_replace(&parent_id.to_string(), hash, checksum.as_str(), &origin).map_err(DatabaseError::operation)?;
        }
    }
    timing.mark("version_and_upload_work");
    for path in &restore_paths {
        tx.shared_placements_queue_file_work("restore", hash, path).map_err(DatabaseError::operation)?;
    }
    timing.mark("restore_work");
    Ok(())
}

#[cfg(test)]
mod commit_tests {
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
        let path = std::env::temp_dir().join(format!("library-database-import-commit-{}-{id}", std::process::id()));
        let database = Database::open(&path).expect("open test database");
        database.initialize_library().expect("initialize test library");
        TestDatabase { database, path }
    }

    fn inspected(title: &str, author: &str) -> book_metadata::InspectedBook {
        book_metadata::InspectedBook {
            pdf: None,
            metadata: book_model::BookRecord {
                title: title.to_owned(),
                subtitle: None,
                contributors: vec![book_model::Contributor::new(author, book_model::MarcRelatorCode(*b"aut")).unwrap()],
                description: "A synopsis.".to_owned(),
                book: book_model::BookMetadata::default(),
            },
            audiobook: None,
            toc: vec![book_model::BookTocEntry { title: "Chapter 1".to_owned(), target: "ch1".to_owned(), children: Vec::new() }],
        }
    }

    #[test]
    fn commit_import_projects_inspection_placement_and_book() {
        let holder = open_test_database();
        let database = &holder.database;
        let parent = database.create_directory(&sync_common::ROOT_DIR_ID, &"Fiction".to_owned()).expect("create folder");
        let hash = ContentHash::new(&"9".repeat(64));
        let checksum = ContentHash::new(&"a".repeat(64));
        database
            .commit_import(ImportCommit {
                parent_id: parent.id,
                file_name: "book.epub".to_owned(),
                format: book_model::BookFormat::Epub,
                content_hash: hash,
                checksum,
                size_bytes: 1024,
                inspection_version: 1,
                inspection: Some(inspected("Imported Title", "Import Author")),
                published: PublishedImport { name: "book.epub".to_owned(), relative_path: "Fiction/book.epub".to_owned(), published: true, fingerprint: Some("fp1".to_owned()) },
                request_thumbnail: true,
                restore_paths: vec!["Fiction/book.epub".to_owned()],
            })
            .expect("commit import");
        let detail = database.browse_book_detail(hash).expect("read detail");
        assert_eq!(detail.book.title, "Imported Title");
        assert_eq!(detail.authors.iter().map(|author| author.name.as_str()).collect::<Vec<_>>(), vec!["Import Author"]);
        assert!(database.has_book_placement(hash).expect("placement"));
        assert!(database.pending_thumbnail_job_count().expect("thumbnail jobs") >= 1);
        let candidates = database.import_restore_candidates(&hash).expect("restore candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].1.as_deref(), Some("Fiction/book.epub"));
        let _ = database.has_pending_asset_work().expect("pending asset work");
        // A cached inspection reuses the recorded proof and still commits.
        let other = database.create_directory(&sync_common::ROOT_DIR_ID, &"Nonfiction".to_owned()).expect("create folder");
        database
            .commit_import(ImportCommit {
                parent_id: other.id,
                file_name: "book.epub".to_owned(),
                format: book_model::BookFormat::Epub,
                content_hash: hash,
                checksum,
                size_bytes: 1024,
                inspection_version: 1,
                inspection: None,
                published: PublishedImport { name: "book.epub".to_owned(), relative_path: "Nonfiction/book.epub".to_owned(), published: true, fingerprint: Some("fp1".to_owned()) },
                request_thumbnail: false,
                restore_paths: Vec::new(),
            })
            .expect("commit cached import");
    }

    /// Regression test for the [`Database::commit_import`]/[`Database::commit_imports`]
    /// batching split: every member of one batch must land in the database
    /// (not just the first) and share one `scan_id`, since that's the
    /// generation marker a later scan pass uses to detect vanished placements.
    #[test]
    fn commit_imports_lands_every_member_with_one_shared_scan_id() {
        let holder = open_test_database();
        let database = &holder.database;
        let parent = database.create_directory(&sync_common::ROOT_DIR_ID, &"Shelf".to_owned()).expect("create folder");
        let first = ContentHash::new(&"1".repeat(64));
        let second = ContentHash::new(&"2".repeat(64));
        let checksum = ContentHash::new(&"a".repeat(64));
        database
            .commit_imports(vec![
                ImportCommit {
                    parent_id: parent.id,
                    file_name: "first.epub".to_owned(),
                    format: book_model::BookFormat::Epub,
                    content_hash: first,
                    checksum,
                    size_bytes: 10,
                    inspection_version: 1,
                    inspection: Some(inspected("First Book", "Author One")),
                    published: PublishedImport { name: "first.epub".to_owned(), relative_path: "Shelf/first.epub".to_owned(), published: true, fingerprint: None },
                    request_thumbnail: false,
                    restore_paths: Vec::new(),
                },
                ImportCommit {
                    parent_id: parent.id,
                    file_name: "second.epub".to_owned(),
                    format: book_model::BookFormat::Epub,
                    content_hash: second,
                    checksum,
                    size_bytes: 20,
                    inspection_version: 1,
                    inspection: Some(inspected("Second Book", "Author Two")),
                    published: PublishedImport { name: "second.epub".to_owned(), relative_path: "Shelf/second.epub".to_owned(), published: true, fingerprint: None },
                    request_thumbnail: false,
                    restore_paths: Vec::new(),
                },
            ])
            .expect("commit batch");
        let first_detail = database.browse_book_detail(first).expect("read first detail");
        assert_eq!(first_detail.book.title, "First Book");
        let second_detail = database.browse_book_detail(second).expect("read second detail");
        assert_eq!(second_detail.book.title, "Second Book");
        let scans: Vec<i64> =
            [first, second].iter().map(|hash| database.connection.query_row("SELECT last_scan FROM book_dir WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", [hash.as_str()], |row| row.get(0)).expect("last_scan")).collect();
        assert_eq!(scans[0], scans[1], "one batch must share one scan generation");
    }

    #[test]
    fn stale_import_destination_rejects_and_rolls_back_the_entire_batch() {
        let holder = open_test_database();
        let database = &holder.database;
        let parent = database.create_directory(&sync_common::ROOT_DIR_ID, &"Deleted while inspecting".to_owned()).unwrap();
        let make_import = |parent_id, n| ImportCommit {
            parent_id,
            file_name: format!("{n}.epub"),
            format: book_model::BookFormat::Epub,
            content_hash: ContentHash::new(&format!("{n:064x}")),
            checksum: ContentHash::new(&format!("{n:064x}")),
            size_bytes: 10,
            inspection_version: 1,
            inspection: Some(inspected("Pending", "Author")),
            published: PublishedImport { name: format!("{n}.epub"), relative_path: format!("{n}.epub"), published: true, fingerprint: None },
            request_thumbnail: false,
            restore_paths: vec![],
        };
        let pending = vec![make_import(sync_common::ROOT_DIR_ID, 71), make_import(parent.id, 72)];
        database.trash_directory(&parent.id).unwrap();
        let generation: i64 = database.connection.query_row("SELECT scan_seq FROM sync_metadata", [], |row| row.get(0)).unwrap();
        assert!(database.commit_imports(pending).is_err());
        assert!(!database.has_book_placement(ContentHash::new(&format!("{:064x}", 71))).unwrap());
        assert!(!database.has_book_placement(ContentHash::new(&format!("{:064x}", 72))).unwrap());
        assert!(!database.is_live_destination(&parent.id).unwrap());
        assert_eq!(database.connection.query_row("SELECT scan_seq FROM sync_metadata", [], |row| row.get::<_, i64>(0)).unwrap(), generation);
    }
}
