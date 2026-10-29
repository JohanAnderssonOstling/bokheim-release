//! Domain projections of canonical registers, shared by local commands and remote sync.
//! Only register admission publishes edits; projection writes disposable domain rows.

use crate::DatabaseError;
use crate::SqlParam;
use crate::annotations::AnnotationsSql;
use crate::books::audiobook::AudiobookSql;
use crate::books::toc::TocSql;
use crate::import::ImportCommitSql;
use crate::purge::remove_book;
use crate::query_methods::query_rows;
use crate::shared_sql::SharedSql;
use crate::stored_document;
use crate::sync::SyncApplySql;
use crate::sync::{nonnegative_u64, normalized_description, storage_i64, toc_entry_count};
use content_address::BookId;
use library_replica::{BookAddition, BookLifecycleState, DirectoryIntent, DirectoryLifecycleState, EnrichmentResult, LibraryStateEntry};
use rusqlite::Transaction;
use std::collections::{BTreeMap, HashMap, HashSet};
use sync_common::{DirId, ROOT_DIR_ID, StateKey, wire};

mod metadata;

/// Project canonical winners inside the same transaction as their admission.
pub(crate) fn project_dirty(tx: &Transaction<'_>) -> Result<(), DatabaseError> {
    let dirty = query_rows(|callback| tx.projection_dirty_addresses(callback), |r| Ok((r.get::<_, String>(0)?, r.get::<_, StateKey>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>, _>>()?;
    if dirty.is_empty() {
        return Ok(());
    }
    // Determine queue membership once, rather than scanning every dirty
    // address again for every projected book in a large sync response.
    let queued_books = dirty
        .iter()
        .filter_map(|(kind, key, _)| match key {
            StateKey::Book(hash) if matches!(kind.as_str(), "book_add" | "book_lifecycle" | "placement") => Some(*hash),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let directories = dirty.iter().any(|(kind, _, _)| kind.starts_with("directory_"));
    let mut books = BTreeMap::<BookId, HashSet<String>>::new();
    for row in query_rows(|callback| tx.projection_dirty_books(callback), |row| Ok((row.get::<_, BookId>(0)?, row.get::<_, String>(1)?)))? {
        let (hash, kind) = row?;
        books.entry(hash).or_default().insert(kind);
    }
    let purged = if directories || dirty.iter().any(|(kind, _, _)| matches!(kind.as_str(), "placement" | "book_projection")) {
        let mut purged = HashSet::new();
        let mut parents = HashMap::new();

        for bytes in query_rows(|callback| tx.projection_directory_lifecycles_and_parents(callback), |row| row.get::<_, Vec<u8>>(0))? {
            match wire::decode::<LibraryStateEntry>(&bytes?, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? {
                LibraryStateEntry::DirectoryParent { dir_id, parent_id } => {
                    parents.insert(dir_id, parent_id);
                }
                LibraryStateEntry::DirectoryLifecycle { dir_id, value: DirectoryLifecycleState::Purged } if dir_id != ROOT_DIR_ID => {
                    purged.insert(dir_id);
                }
                _ => {}
            }
        }
        loop {
            let descendants = parents.iter().filter(|(id, parent)| **id != ROOT_DIR_ID && !purged.contains(*id) && purged.contains(*parent)).map(|(id, _)| *id).collect::<Vec<_>>();
            if descendants.is_empty() {
                break;
            }
            purged.extend(descendants);
        }
        purged
    } else {
        HashSet::new()
    };
    if directories {
        // Directory deletion cascades placements away. Reproject memberships
        // when directory winners change, including a later explicit recreation.
        for hash in query_rows(|callback| tx.projection_missing_directory_books(callback), |row| row.get::<_, BookId>(0))? {
            books.entry(hash?).or_default().insert("placement".into());
        }
    }
    let mut pending = Vec::with_capacity(books.len());
    for (hash, changed) in books {
        let full = changed.contains("book_projection");
        let kinds = serde_json::to_string(&changed).map_err(DatabaseError::operation)?;
        let rows = query_rows(|callback| tx.projection_book_state(&(&hash), full, &kinds, callback), |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        let values = rows
            .into_iter()
            .map(|(bytes, time)| Ok((wire::decode::<LibraryStateEntry>(&bytes, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?, nonnegative_u64(time, 1, "register timestamp")?)))
            .collect::<Result<Vec<_>, DatabaseError>>()?;
        pending.push((hash, changed, values));
    }
    // Canonical winners have already been admitted under this writer. Build
    // complete metadata sets, then write them together before reader documents.
    metadata::project(tx, pending.iter().filter_map(|(hash, changed, values)| {
        let needs_metadata = changed.contains("book_projection") || changed.contains("book_add") || changed.contains("external_book");
        (needs_metadata && !book_is_purged(values)).then(|| (hash, book_addition(values)))
    }))?;
    for (hash, changed, values) in pending {
        project_book(tx, &hash, &values, &changed, &purged)?;
        if queued_books.contains(&hash) {
            tx.queue_book_work_insert(&hash)?;
        }
    }
    if directories {
        // Cache the canonical directory winners before deriving the display tree.
        for bytes in query_rows(|callback| tx.projection_directory_state(callback), |row| row.get::<_, Vec<u8>>(0))? {
            let body = wire::decode::<LibraryStateEntry>(&bytes?, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?;
            project_value(tx, &body, &purged)?;
        }
        project_directory_tree(tx, &purged)?;
    }
    tx.projection_clear_dirty()?;
    Ok(())
}

fn project_directory_tree(tx: &Transaction<'_>, purged: &HashSet<DirId>) -> Result<(), DatabaseError> {
    // Directory writes invalidate folder membership; the next browse refreshes
    // it once from the final tree, just as for local directory batches.
    let before = query_rows(|callback| tx.projection_directory_rows(callback), |r| Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?))))?.collect::<Result<HashMap<_, _>, _>>()?;
    // Read canonical winners; the directory columns cache those winners
    // alongside the repaired display tree.
    let mut intents = HashMap::new();
    let decode = |bytes: &[u8]| wire::decode::<LibraryStateEntry>(bytes, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation);
    for row in query_rows(|callback| tx.projection_directory_intents(callback), |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, Vec<u8>>(2)?)))? {
        let (name, parent, lifecycle) = row?;
        let (LibraryStateEntry::DirectoryName { dir_id: id, name }, LibraryStateEntry::DirectoryParent { parent_id, .. }, LibraryStateEntry::DirectoryLifecycle { value: lifecycle, .. }) =
            (decode(&name)?, decode(&parent)?, decode(&lifecycle)?)
        else {
            return Err(DatabaseError::message("invalid canonical directory registers"));
        };
        if id != ROOT_DIR_ID {
            intents.insert(id, DirectoryIntent { id, parent_id, name, lifecycle });
        }
    }
    let projection = library_replica::project_directories(&intents);
    let expected: BTreeMap<_, _> = intents
        .iter()
        .filter(|(id, _)| !purged.contains(id))
        .map(|(id, intent)| {
            let suppressed = projection.suppressed.contains(id);
            let parent = if suppressed { ROOT_DIR_ID } else { projection.parents[id] };
            let name = projection.names.get(id).unwrap_or(&intent.name);
            (*id, (parent.to_string(), name.clone(), suppressed))
        })
        .collect();
    // Local commands may already have installed the final display. In
    // particular, creating an empty folder must not invalidate every placement.
    let unchanged = before.iter().all(|(id, actual)| {
        if id == &ROOT_DIR_ID.to_string() {
            return true;
        }
        let Ok(id) = DirId::parse_str(id) else {
            return false;
        };
        if purged.contains(&id) {
            return false;
        }
        expected.get(&id).map_or(actual.2, |state| actual == state)
    });
    let unchanged = unchanged && expected.keys().all(|id| before.contains_key(&id.to_string()));
    if unchanged {
        return Ok(());
    }
    tx.hide_remote_directories().map_err(DatabaseError::operation)?;
    // Detach the old tree before installing new edges. Even hidden rows are
    // traversed by folder queries, so intermediate cycles are unsafe.
    tx.projection_detach_directories()?;
    for id in purged {
        if before.contains_key(&id.to_string()) {
            let id = id.to_string();
            tx.projection_queue_purged_directory_files(&id)?;
            tx.projection_remove_directory(&id)?;
            tx.shared_placements_queue_directory_work(&id)?;
        }
    }
    for (id, (parent, name, _)) in &expected {
        tx.ensure_remote_directory_placeholder(&id.to_string())?;
        tx.project_remote_directory(&id.to_string(), parent, name)?;
    }
    for id in projection.ordered_active {
        tx.activate_remote_directory(&id.to_string()).map_err(DatabaseError::operation)?;
    }
    #[cfg(not(target_arch = "wasm32"))]
    for (dir, state) in &expected {
        let id = dir.to_string();
        if before.get(&id) != Some(state) {
            // Lifecycle changes do not relocate directories. Only materialize
            // a live directory whose desired path differs from its stored path.
            let Some(desired) = crate::placements::directory_relative_path(tx, dir)? else { continue };
            let retained = tx.projection_directory_path_retained(&id, &desired, |row| row.get::<_, bool>(0))?;
            if retained {
                continue;
            }
            tx.shared_placements_queue_directory_work(&id)?;
        }
    }
    Ok(())
}

/// Project only changed state kinds. Synthetic book dirtiness requests a full rebuild.
fn book_is_purged(values: &[(LibraryStateEntry, u64)]) -> bool {
    values.iter().any(|(body, _)| matches!(body, LibraryStateEntry::BookLifecycle { value: BookLifecycleState::Purged, .. }))
}

fn book_addition(values: &[(LibraryStateEntry, u64)]) -> Option<&BookAddition> {
    values.iter().find_map(|(body, _)| match body {
        LibraryStateEntry::BookAdd { value, .. } => Some(value),
        LibraryStateEntry::ExternalBook { value, .. } => Some(&value.addition),
        _ => None,
    })
}

fn project_book(tx: &Transaction<'_>, hash: &BookId, values: &[(LibraryStateEntry, u64)], changed: &HashSet<String>, purged: &HashSet<DirId>) -> Result<(), DatabaseError> {
    if book_is_purged(values) {
        return remove_book(tx, hash);
    }
    if !tx.projection_book_exists(&hash, |row| row.get::<_, bool>(0))? {
        return Ok(());
    }
    let needs = |kind| changed.contains("book_projection") || changed.contains(kind);
    if needs("book_lifecycle") {
        let mut deleted_at = None;
        let mut origin = None;
        for (body, time) in values {
            if let LibraryStateEntry::BookLifecycle { value: BookLifecycleState::Deleted { origin_folder_id }, .. } = body {
                deleted_at = Some(storage_i64(*time, "deletion timestamp")?);
                origin = origin_folder_id.as_deref();
            }
        }
        let was_deleted: bool = tx.projection_book_deleted(&hash, |row| row.get(0))?;
        tx.projection_set_book_lifecycle(hash, deleted_at, origin)?;
        if deleted_at.is_some() {
            tx.projection_cancel_deleted_cover_work(&hash)?;
            tx.projection_clear_deleted_downloads(&hash)?;
        } else if was_deleted {
            tx.projection_restore_cover_work(&hash)?;
        }
    }
    if needs("reading_position") {
        let mut reading = None;
        let mut progress = 0.0;
        let mut read_at = None;
        for (body, time) in values {
            if let LibraryStateEntry::ReadingPosition { value, .. } = body {
                reading = Some(value.location.as_str());
                progress = value.progress;
                read_at = Some(storage_i64(*time, "reading timestamp")?);
            }
        }
        tx.projection_set_book_reading(hash, reading.as_deref(), progress, read_at)?;
    }
    if needs("book_add") || needs("external_book") {
        let addition = book_addition(values);
        if let Some(addition) = addition {
            project_addition(tx, hash, addition)?;
        } else {
            tx.projection_clear_description(&hash)?;
            tx.projection_clear_navigation(&hash)?;
            tx.projection_clear_pdf_metadata(&hash)?;
            tx.projection_clear_recording(&hash)?;
        }
        for (body, _) in values {
            if let LibraryStateEntry::ExternalBook { value, .. } = body {
                crate::provider::project_provider_file(tx, hash, &value.file, value.thumbnails.as_ref())?;
            }
        }
    }
    if needs("annotation") {
        tx.projection_trim_annotations(&hash)?;
    }
    if needs("placement") {
        tx.projection_trim_placements(&hash)?;
    }
    for (body, _) in values {
        if matches!(body, LibraryStateEntry::Annotation { .. } | LibraryStateEntry::Placement { .. }) {
            project_value(tx, body, purged)?;
        }
    }
    Ok(())
}

fn project_value(tx: &Transaction<'_>, body: &LibraryStateEntry, purged: &HashSet<DirId>) -> Result<(), DatabaseError> {
    if let LibraryStateEntry::DirectoryName { dir_id, .. } | LibraryStateEntry::DirectoryParent { dir_id, .. } | LibraryStateEntry::DirectoryLifecycle { dir_id, .. } | LibraryStateEntry::Placement { dir_id, .. } = body {
        if purged.contains(dir_id) {
            return Ok(());
        }
        if *dir_id != ROOT_DIR_ID {
            tx.ensure_remote_directory_placeholder(&dir_id.to_string())?;
        }
    }
    match body {
        LibraryStateEntry::DirectoryName { dir_id, name } => {
            tx.set_remote_directory_name(&dir_id.to_string(), name.as_str()).map_err(DatabaseError::operation)?;
        }
        LibraryStateEntry::DirectoryParent { dir_id, parent_id } => {
            if *parent_id != ROOT_DIR_ID && !purged.contains(parent_id) {
                tx.ensure_remote_directory_placeholder(&parent_id.to_string()).map_err(DatabaseError::operation)?;
            }
            tx.set_remote_directory_parent(&dir_id.to_string(), &parent_id.to_string()).map_err(DatabaseError::operation)?;
        }
        LibraryStateEntry::DirectoryLifecycle { dir_id, value } => {
            let value = match value {
                DirectoryLifecycleState::Present => 0,
                DirectoryLifecycleState::Deleted => 1,
                DirectoryLifecycleState::Purged => 2,
            };
            tx.set_remote_directory_lifecycle(&dir_id.to_string(), value).map_err(DatabaseError::operation)?;
        }
        LibraryStateEntry::BookLifecycle { .. } | LibraryStateEntry::ReadingPosition { .. } | LibraryStateEntry::ChapterReadStatus { .. } | LibraryStateEntry::BookAdd { .. } | LibraryStateEntry::ExternalBook { .. } => {
            unreachable!("book records and scalar registers are projected together")
        }
        LibraryStateEntry::Placement { dir_id, content_hash, present, origin_folder_id } => {
            if *present {
                // Filenames are a local projection. A tombstoned placement's
                // old name may have been reused since it was removed.
                let directory = dir_id.to_string();
                let mut statement = tx.prepare_cached(include_str!("projection/placement_name.sql"))?;
                let (requested, conflict): (String, bool) = statement.query_row(rusqlite::params![directory, content_hash], |row| Ok((row.get(0)?, row.get(1)?)))?;
                drop(statement);
                let chosen = if conflict {
                    let occupied = query_rows(|callback| tx.projection_other_placement_names(&directory, &(&content_hash), callback), |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                    // Use a short fallback when the old name conflicts or fills
                    // the filesystem's component byte limit.
                    library_replica::unique_file_name("Untitled", occupied.iter().map(String::as_str))
                } else {
                    requested
                };
                tx.apply_remote_book_dir_added(&directory, content_hash, &chosen).map_err(DatabaseError::operation)?;
            } else {
                tx.apply_remote_book_dir_removed(&dir_id.to_string(), content_hash, origin_folder_id.as_deref()).map_err(DatabaseError::operation)?;
            }
        }
        LibraryStateEntry::Annotation { value } => {
            let modified_at = storage_i64(value.modified_at, "annotation modification timestamp").map_err(DatabaseError::operation)?;
            let body = value.encode().map_err(DatabaseError::operation)?;
            tx.upsert_annotation(value.id.as_str(), &value.content_hash, value.toc_ordinal, value.progress, body.as_slice(), modified_at, value.deleted.then_some(modified_at)).map_err(DatabaseError::operation)?;
        }
    }
    Ok(())
}

/// Project the finalized record. Enrichment was already applied to it before
/// publication, or while decoding a historical two-record addition.
fn project_addition(tx: &Transaction<'_>, hash: &BookId, addition: &BookAddition) -> Result<(), DatabaseError> {
    let enriched_entries = match &addition.enrichment {
        Some(EnrichmentResult::Found { toc, .. }) => toc.as_deref(),
        _ => None,
    };
    tx.set_description(hash, &normalized_description(&addition.record.description)).map_err(DatabaseError::operation)?;

    if let Some(recording) = &addition.recording {
        let json = serde_json::to_string(recording).map_err(DatabaseError::operation)?;
        tx.audiobook_record_recording(hash, &json)?;
    } else {
        tx.projection_clear_recording(&hash)?;
    }
    if let Some(pdf) = &addition.pdf {
        let bytes = stored_document::encode(pdf)?;
        tx.pdf_reader_metadata_upsert(hash, &bytes).map_err(DatabaseError::operation)?;
    } else {
        tx.projection_clear_pdf_metadata(&hash)?;
    }
    if let Some(navigation) = &addition.navigation {
        // Provider entries apply only to audio navigation. Duration and track
        // offsets always come from the file, including when entries are empty.
        let entries = if navigation.duration_ms.is_some() { enriched_entries.unwrap_or(&navigation.entries) } else { &navigation.entries };
        let tracks = navigation.tracks.as_ref().map(stored_document::encode).transpose()?;
        let duration_ms = navigation.duration_ms.map(i64::try_from).transpose().map_err(DatabaseError::operation)?;
        let entry_count = i64::try_from(toc_entry_count(entries)).map_err(DatabaseError::operation)?;
        let toc = stored_document::encode(entries)?;
        tx.navigation_upsert(hash, entry_count, &toc, duration_ms, tracks.as_deref())?;
    } else {
        tx.projection_clear_navigation(&hash)?;
    }
    Ok(())
}

fn popularity_rating_count(addition: &BookAddition) -> Option<f64> {
    match &addition.enrichment {
        Some(EnrichmentResult::Found { popularity: Some(popularity), .. }) => Some(popularity.rating_count as f64),
        _ => None,
    }
}

/// Populate the disposable browse column once, without buffering every body
/// or changing canonical records, publication tokens, or the outbox.
pub(crate) fn backfill_popularity(tx: &Transaction<'_>) -> Result<(), DatabaseError> {
    let mut statement = tx.prepare(include_str!("sql/popularity_backfill.sql"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let hash: BookId = row.get(0)?;
        let body: Vec<u8> = row.get(1)?;
        let LibraryStateEntry::BookAdd { value, .. } = wire::decode::<LibraryStateEntry>(&body, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? else {
            return Err(DatabaseError::message("popularity requires an immutable book addition"));
        };
        tx.projection_set_book_popularity(&hash, popularity_rating_count(&value))?;
    }
    Ok(())
}

/// Local filenames are projection inputs, not synchronized
/// registers. Seed local values, then admit membership
/// through the same canonical path used by directory commands.
pub(crate) fn place_book(tx: &Transaction<'_>, directory: &DirId, hash: &BookId, name: &str) -> Result<(), DatabaseError> {
    tx.projection_upsert_placement(&(directory.to_string()), &(&hash), &name)?;
    crate::sync::apply::registers::record_local(tx, LibraryStateEntry::Placement { dir_id: *directory, content_hash: *hash, present: true, origin_folder_id: None })
}

include_sqlite_sql::include_sql!("src/sql/projection.sql");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use crate::sql_fixtures::FixturesSql;
    use crate::sync::apply::registers::record_local;
    use crate::taxonomy_update::TaxonomySql;
    use book_model::{BookFormat, BookRecord, BookSubject};
    use library_replica::BookFacts;

    #[test]
    fn cached_placement_lookup_retains_names_and_checks_only_other_live_books() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.create_scalar_function("portable_name_key", 1, rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC, |context| {
            Ok(library_replica::portable_name_key(&context.get::<String>(0)?))
        })
        .unwrap();
        conn.execute_batch(include_str!("sync/sql/schema/tables.sql")).unwrap();
        conn.execute_batch(include_str!("placements/sql/schema/tables.sql")).unwrap();
        let directory = ROOT_DIR_ID.to_string();
        conn.execute("INSERT INTO dir(id,parent_id,name) VALUES(?1,?1,'Root')", [&directory]).unwrap();
        let hash = BookId::new(&"a".repeat(64));
        let other = BookId::new(&"b".repeat(64));
        for hash in [hash, other] {
            conn.execute("INSERT INTO book(content_hash) VALUES(?1)", [hash]).unwrap();
        }
        let lookup = || {
            conn.prepare_cached(include_str!("projection/placement_name.sql")).unwrap().query_row(rusqlite::params![directory,hash], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))).unwrap()
        };
        assert_eq!(lookup(), ("Untitled".into(), false));
        conn.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash) SELECT ?1,row_id,'UNTITLED','' FROM book WHERE content_hash=?2", rusqlite::params![directory,other]).unwrap();
        assert_eq!(lookup(), ("Untitled".into(), true));
        conn.execute("UPDATE book_dir SET deleted_at=1", []).unwrap();
        assert_eq!(lookup(), ("Untitled".into(), false));
        conn.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,deleted_at) SELECT ?1,row_id,'Retained','',1 FROM book WHERE content_hash=?2", rusqlite::params![directory,hash]).unwrap();
        assert_eq!(lookup(), ("Retained".into(), false));
        conn.execute("UPDATE book_dir SET file_name='retained',deleted_at=NULL WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", [other]).unwrap();
        assert_eq!(lookup(), ("Retained".into(), true));
    }

    #[test]
    fn book_record_lookup_uses_the_existing_primary_key() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch(include_str!("sync/sql/schema/tables.sql")).unwrap();
        let target = BookId::new(&format!("{:064x}", 128));
        for number in 1..=256 {
            let hash = BookId::new(&format!("{number:064x}"));
            connection.execute("INSERT INTO book(content_hash) VALUES(?1)", [&hash]).unwrap();
            connection.execute("INSERT INTO sync_book_record(state_kind,content_hash,body,publication) VALUES('book_add',?1,?2,x'00')", rusqlite::params![hash, number.to_string().as_bytes()]).unwrap();
        }
        for (full, kinds, expected) in [(true, "[]", 1), (false, "[\"book_add\"]", 1), (false, "[\"reading_position\"]", 0)] {
            let mut bodies = Vec::new();
            connection
                .projection_book_state(&target, full, kinds, |row| {
                    bodies.push(row.get::<_, Vec<u8>>(0)?);
                    assert_eq!(crate::query_methods::active_query_fullscan_steps(&connection), 0);
                    Ok(())
                })
                .unwrap();
            assert_eq!(bodies.len(), expected);
            if expected != 0 {
                assert_eq!(bodies[0], b"128");
            }
        }
    }

    #[test]
    fn addition_and_enrichment_project_only_the_final_metadata() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let hash = BookId::new(&"e".repeat(64));
        db.connection.fixtures_insert_book(&hash).unwrap();
        db.connection.fixtures_create_transaction_probe().unwrap();
        db.connection.fixtures_track_subject_projection_writes().unwrap();
        let mut original = BookRecord { metadata: Default::default(), description: String::new() };
        original.title = "File title".into();
        original.book.subjects = vec![BookSubject::new(None, "Original", "file", None, None).unwrap()];
        let mut enriched = BookRecord { metadata: Default::default(), description: String::new() };
        enriched.title = "Provider title".into();
        enriched.book.subjects = vec![BookSubject::new(None, "Enriched", "provider", None, None).unwrap()];
        let mut addition = BookAddition {
            facts: BookFacts { added_at: 1, format: BookFormat::Epub },
            record: original,
            navigation: None,
            pdf: None,
            recording: None,
            enrichment: None,
        };
        addition.apply_enrichment(enriched, EnrichmentResult::Found { toc: None, cover_id: None, popularity: None });
        db.write(|tx| {
            record_local(
                tx,
                LibraryStateEntry::BookAdd {
                    content_hash: hash,
                    value: addition,
                },
            )?;
            place_book(tx, &ROOT_DIR_ID, &hash, "book.epub")
        })
        .unwrap();
        assert_eq!(db.connection.fixtures_read_transaction_probe(|row| row.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(db.browse_book_detail(&hash).unwrap().book.title, "File title");
        let book = db.connection.fixtures_read_book_row_id(&hash, |row| row.get::<_, i64>(0)).unwrap();
        let subjects = query_rows(|callback| db.connection.taxonomy_book_subjects(&book, callback), |row| row.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(subjects, ["Enriched"]);
        db.connection.fixtures_guard_non_reading_projection_writes().unwrap();
        db.update_reading_position(&hash, "epubcfi(/6/2)", Some(0.25), Some("Chapter")).unwrap();
        assert_eq!(db.reading_position(&hash).unwrap().as_deref(), Some("epubcfi(/6/2)"));
        assert_eq!(db.browse_book_detail(&hash).unwrap().book.title, "File title");
        assert_eq!(db.connection.fixtures_read_transaction_probe(|row| row.get::<_, i64>(0)).unwrap(), 1);

        db.connection.fixtures_drop_non_reading_projection_guards().unwrap();
        db.write(|tx| {
            tx.pull_mark_book_dirty(&hash)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(db.reading_position(&hash).unwrap().as_deref(), Some("epubcfi(/6/2)"));
        assert_eq!(db.connection.fixtures_read_transaction_probe(|row| row.get::<_, i64>(0)).unwrap(), 2);
    }
}
