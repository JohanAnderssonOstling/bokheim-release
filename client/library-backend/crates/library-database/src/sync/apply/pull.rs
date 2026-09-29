//! Applying a pull page: preparation is pure; all database decisions share one transaction.
use super::*;
use crate::books::audiobook::AudiobookSql;
use crate::shared_sql::SharedSql;

fn read_cursor(conn: &rusqlite::Connection) -> Result<sync_common::SyncCursor, DatabaseError> {
    Ok(sync_common::SyncCursor { state_revision: parse_revision(read_cursor_revision(conn, PullCursorKey::State)?)?, reading_revision: parse_revision(read_cursor_revision(conn, PullCursorKey::Reading)?)? })
}

impl Database {
    /// Acknowledges sent mutations and applies a page atomically. Wire decoding,
    /// batch planning and input-only serialization run before reserving the writer.
    /// Returns whether the page carried remote changes, including stale changes.
    pub fn sync_commit_pull_response(&self, ids: &[MutationId], page: &PullStateResponse) -> Result<bool, DatabaseError> {
        if page.book_creations.is_empty() && page.mutations.is_empty() && ids.is_empty() && read_cursor(&self.connection)? == page.next_cursor {
            return Ok(false);
        }
        let mut creations = Vec::new();
        for declaration in &page.book_creations {
            let body = MutationBody::from_wire(&declaration.mutation).map_err(DatabaseError::operation)?;
            match body {
                Some(body @ MutationBody::BookLifecycle { value: library_replica::BookLifecycleState::Present | library_replica::BookLifecycleState::Deleted { .. }, .. }) => creations.push((declaration, body)),
                _ => return Err(DatabaseError::message("invalid explicit book creation")),
            }
        }
        let prepared = prepare_remote_changes(&page.mutations).map_err(DatabaseError::operation)?;
        let acknowledgements = if ids.is_empty() { None } else { Some(serde_json::to_string(&ids.iter().map(ToString::to_string).collect::<Vec<_>>()).map_err(DatabaseError::operation)?) };
        self.with_write_transaction(|tx| {
            tx.pragma_update(None, "defer_foreign_keys", "ON").map_err(DatabaseError::operation)?;
            let mut acknowledged_fields = Vec::new();
            if let Some(payload) = acknowledgements {
                // An acknowledgement confirms a publication even if its field
                // is on a later pull page. Capture only still-matching queue
                // generations: old acknowledgements cannot confirm new edits.
                let cells = tx
                    .prepare("SELECT state_kind,state_key,state_subkey FROM sync_outbox WHERE mutation_id IN (SELECT value FROM json_each(?1))")?
                    .query_map([&payload], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                for cell in cells {
                    if let Some(version) = read_version(tx, &cell.0, &cell.1, &cell.2)? {
                        acknowledged_fields.push((cell, version));
                    }
                }
                tx.outbox_delete_for_ids(&payload).map_err(DatabaseError::operation)?;
            }
            if creations.is_empty() && page.mutations.is_empty() && read_cursor(tx)? == page.next_cursor {
                return Ok(crate::transactions::WriteOutcome::Commit(false));
            }
            {
                // A still-existing row may have missed a complete purge/readd.
                // Remember its lifecycle before merging this page, not the
                // intermediate order of declarations and lifecycle mutations.
                let mut existing = std::collections::BTreeMap::new();
                for (_, body) in &creations {
                    let MutationBody::BookLifecycle { content_hash, .. } = body else { unreachable!() };
                    let hash = content_hash.as_str();
                    if tx.query_row("SELECT EXISTS(SELECT 1 FROM book WHERE content_hash=?1)", [hash], |r| r.get::<_, bool>(0))? {
                        existing.insert(hash.to_owned(), read_version(tx, "book_lifecycle", hash, "")?);
                    }
                }
                for (change, body) in &creations {
                    registers::merge(tx, body, &library_replica::VersionKey::from_wire(&change.mutation, change.replica_id))?;
                }
                for (change, body) in prepared.changes.iter().filter(|(_, body)| matches!(body, MutationBody::BookLifecycle { .. })) {
                    let version = library_replica::VersionKey::from_wire(&change.mutation, change.replica_id);
                    registers::merge(tx, body, &version)?;
                }
                // Apply final lifecycle winners before admitting dependent fields.
                project_dirty(tx)?;
                crate::transactions::with_remote_origin(tx, |origin| {
                    for (_, body) in &creations {
                        let MutationBody::BookLifecycle { content_hash, .. } = body else { unreachable!() };
                        let Some((bytes, _)) = registers::canonical_body(tx, "book_lifecycle", content_hash.as_str(), "")? else { continue };
                        let winner: MutationBody = sync_common::wire::decode(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?;
                        if matches!(winner, MutationBody::BookLifecycle { value: library_replica::BookLifecycleState::Purged, .. }) {
                            continue;
                        }
                        origin.mark(tx)?;
                        if tx.execute("INSERT OR IGNORE INTO book(content_hash) VALUES(?1)", [content_hash.as_str()])? != 0 {
                            tx.execute("INSERT OR IGNORE INTO sync_projection_dirty VALUES('book_projection',?1,'')", [content_hash.as_str()])?;
                        }
                    }
                    Ok(())
                })?;
                for (change, body) in prepared.changes.iter().filter(|(_, body)| !matches!(body, MutationBody::BookLifecycle { .. })) {
                    registers::merge(tx, body, &library_replica::VersionKey::from_wire(&change.mutation, change.replica_id))?;
                }
                project_dirty(tx)?;
                reconcile_surviving_fields(tx, existing, &prepared, acknowledged_fields)?;
                write_cursor_revision(tx, PullCursorKey::State, page.next_cursor.state_revision)?;
                write_cursor_revision(tx, PullCursorKey::Reading, page.next_cursor.reading_revision)?;
            }
            Ok(crate::transactions::WriteOutcome::Commit(!page.mutations.is_empty() || !creations.is_empty()))
        })
    }
}

/// A newer existence declaration does not reveal whether purge happened while
/// this replica was away. Reconcile surviving fields through normal publication,
/// preserving their original versions. Same-page confirmations avoid echoes.
fn reconcile_surviving_fields(
    tx: &rusqlite::Transaction<'_>, existing: std::collections::BTreeMap<String, Option<library_replica::VersionKey>>, prepared: &PreparedRemoteChanges<'_>, acknowledged: Vec<((String, String, String), library_replica::VersionKey)>,
) -> Result<(), DatabaseError> {
    let mut confirmed = acknowledged.into_iter().collect::<std::collections::HashMap<_, _>>();
    for (change, body) in &prepared.changes {
        let version = library_replica::VersionKey::from_wire(&change.mutation, change.replica_id);
        let (kind, key, subkey) = sync_state_identity(body);
        confirmed
            .entry((kind.to_owned(), key, subkey))
            .and_modify(|old: &mut library_replica::VersionKey| {
                if version > *old {
                    *old = version.clone();
                }
            })
            .or_insert(version);
    }
    for (hash, previous) in existing {
        if read_version(tx, "book_lifecycle", &hash, "")? == previous || !tx.query_row("SELECT EXISTS(SELECT 1 FROM book WHERE content_hash=?1)", [&hash], |r| r.get::<_, bool>(0))? {
            continue;
        }
        for cell in registers::book_fields(tx, &hash)? {
            let current = read_version(tx, &cell.kind, &cell.entity_key, &cell.entity_subkey)?;
            if confirmed.get(&(cell.kind.clone(), cell.entity_key.clone(), cell.entity_subkey.clone())).is_some_and(|seen| Some(seen) >= current.as_ref()) {
                continue;
            }
            registers::recover(tx, &cell)?;
        }
    }
    Ok(())
}

/// Project canonical winners inside the same transaction as their admission.
pub(crate) fn project_dirty(tx: &rusqlite::Transaction<'_>) -> Result<(), DatabaseError> {
    let dirty = tx.prepare("SELECT state_kind,state_key,state_subkey FROM sync_projection_dirty")?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?.collect::<Result<Vec<_>, _>>()?;
    if dirty.is_empty() {
        return Ok(());
    }
    let books = tx
        .prepare(
            "SELECT DISTINCT COALESCE(v.book_key,CASE WHEN d.state_kind='book_projection' THEN d.state_key END)
        FROM sync_projection_dirty d LEFT JOIN sync_state_version v USING(state_kind,state_key,state_subkey)
        WHERE v.book_key IS NOT NULL OR d.state_kind='book_projection' ORDER BY 1",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let directories = dirty.iter().any(|(kind, _, _)| kind.starts_with("directory_"));
    crate::transactions::with_remote_origin(tx, |origin| {
        origin.mark(tx)?;
        for hash in books {
            let rows = tx
                .prepare("SELECT body,changed_at FROM sync_state_version WHERE book_key=?1 AND body IS NOT NULL ORDER BY state_kind,state_key,state_subkey")?
                .query_map([&hash], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let values = rows
                .into_iter()
                .map(|(bytes, time)| Ok((sync_common::wire::decode::<MutationBody>(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?, nonnegative_u64(time, 1, "register timestamp")?)))
                .collect::<Result<Vec<_>, DatabaseError>>()?;
            project_book(tx, &hash, &values)?;
            if dirty.iter().any(|(kind, key, _)| key == &hash && matches!(kind.as_str(), "book_facts" | "book_lifecycle" | "placement" | "metadata")) {
                tx.queue_book_work_insert(&hash)?;
            }
        }
        if directories {
            // Refresh the command-input cache from the same canonical source.
            let rows = tx
                .prepare("SELECT body FROM sync_state_version WHERE state_kind IN ('directory_name','directory_parent','directory_lifecycle') AND body IS NOT NULL ORDER BY state_kind,state_key")?
                .query_map([], |r| r.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for bytes in rows {
                let body = sync_common::wire::decode::<MutationBody>(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?;
                project_value(tx, 0, &body)?;
            }
            rebuild_remote_directories(tx)?;
        }
        tx.execute("DELETE FROM sync_projection_dirty", [])?;
        Ok(())
    })
}

/// Desired synchronized scalars are derived before touching the display row.
/// Missing registers have explicit defaults, never the previous display value.
fn project_book(tx: &rusqlite::Transaction<'_>, hash: &str, values: &[(MutationBody, u64)]) -> Result<(), DatabaseError> {
    if values.iter().any(|(body, _)| matches!(body, MutationBody::BookLifecycle { value: library_replica::BookLifecycleState::Purged, .. })) {
        return crate::purge::remove_book(tx, hash);
    }
    // Only the explicit creation prelude (or a local import) inserts books.
    if !tx.query_row("SELECT EXISTS(SELECT 1 FROM book WHERE content_hash=?1)", [hash], |row| row.get::<_, bool>(0))? {
        return Ok(());
    }
    let mut format = "";
    let mut added_at = None;
    let mut deleted_at = None; // Deterministic placeholder until lifecycle arrives.

    let mut origin = None;
    let mut reading = None;
    let mut progress = 0f32;
    let mut kinds = std::collections::HashSet::new();
    for (body, time) in values {
        kinds.insert(body.kind());
        match body {
            MutationBody::BookFacts { value, .. } => {
                format = value.format.canonical_extension();
                added_at = Some(storage_i64(value.added_at, "book addition timestamp")?);
            }
            MutationBody::BookLifecycle { value, .. } => match value {
                library_replica::BookLifecycleState::Present => deleted_at = None,
                library_replica::BookLifecycleState::Deleted { origin_folder_id } => {
                    deleted_at = Some(storage_i64(*time, "deletion timestamp")?);
                    origin = origin_folder_id.as_deref();
                }
                library_replica::BookLifecycleState::Purged => {
                    return crate::purge::remove_book(tx, hash);
                }
            },
            MutationBody::ReadingPosition { value, .. } => {
                reading = Some(value.location.as_str());
                progress = value.progress;
            }
            _ => {}
        }
    }
    tx.execute(
        "UPDATE book SET format=?2,added_at=?3,deleted_at=?4,trash_origin_dir_id=?5,read_pos=?6,read_progress=?7
        WHERE content_hash=?1 AND (format,added_at,deleted_at,trash_origin_dir_id,read_pos,read_progress) IS NOT (?2,?3,?4,?5,?6,?7)",
        rusqlite::params![hash, format, added_at, deleted_at, origin, reading, progress],
    )?;
    if !kinds.contains("metadata") {
        let mut empty = library_replica::SyncBookMetadata::default();
        empty.title = String::new();
        project_remote_metadata(tx, &sync_common::ContentHash::new(hash), &empty)?;
        tx.execute("UPDATE book SET title=NULL,subtitle=NULL,book_metadata=X'' WHERE content_hash=?1", [hash])?;
    }
    if !kinds.contains("description") {
        tx.execute("DELETE FROM book_description WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", [hash])?;
        tx.execute("UPDATE book SET description_scanned=0 WHERE content_hash=?1", [hash])?;
    }
    if !kinds.contains("book_toc") {
        for table in ["book_toc", "audiobook_metadata", "audiobook_track_index"] {
            tx.execute(&format!("DELETE FROM {table} WHERE content_hash=?1"), [hash])?;
        }
    }
    // Values whose identities disappeared must not linger in projections.
    tx.execute(
        "DELETE FROM pdf_reader_metadata WHERE content_hash=?1 AND checksum NOT IN
       (SELECT state_subkey FROM sync_state_version WHERE state_kind='pdf_reader_metadata' AND state_key=?1 AND body IS NOT NULL)",
        [hash],
    )?;
    tx.execute(
        "DELETE FROM annotation WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1) AND id NOT IN
       (SELECT state_key FROM sync_state_version WHERE state_kind='annotation' AND book_key=?1 AND body IS NOT NULL)",
        [hash],
    )?;
    tx.execute(
        "DELETE FROM book_dir WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1) AND dir_id NOT IN
       (SELECT state_subkey FROM sync_state_version WHERE state_kind='placement' AND state_key=?1 AND body IS NOT NULL)",
        [hash],
    )?;
    for (body, time) in values {
        if !matches!(body, MutationBody::BookLifecycle { .. } | MutationBody::BookFacts { .. } | MutationBody::ReadingPosition { .. }) {
            project_value(tx, *time, body)?;
        }
    }
    Ok(())
}

fn project_value(tx: &rusqlite::Transaction<'_>, _changed_at: u64, body: &MutationBody) -> Result<(), DatabaseError> {
    match body {
        MutationBody::DirectoryName { dir_id, name } => {
            if *dir_id != sync_common::ROOT_DIR_ID {
                tx.ensure_remote_directory_placeholder(&dir_id.to_string()).map_err(DatabaseError::operation)?;
            }
            tx.set_remote_directory_name(&dir_id.to_string(), name.as_str()).map_err(DatabaseError::operation)?;
        }
        MutationBody::DirectoryParent { dir_id, parent_id } => {
            if *parent_id != sync_common::ROOT_DIR_ID {
                tx.ensure_remote_directory_placeholder(&parent_id.to_string()).map_err(DatabaseError::operation)?;
            }
            if *dir_id != sync_common::ROOT_DIR_ID {
                tx.ensure_remote_directory_placeholder(&dir_id.to_string()).map_err(DatabaseError::operation)?;
            }
            tx.set_remote_directory_parent(&dir_id.to_string(), &parent_id.to_string()).map_err(DatabaseError::operation)?;
        }
        MutationBody::DirectoryLifecycle { dir_id, value } => {
            if *dir_id != sync_common::ROOT_DIR_ID {
                tx.ensure_remote_directory_placeholder(&dir_id.to_string()).map_err(DatabaseError::operation)?;
            }
            let value = match value {
                DirectoryLifecycleState::Present => 0,
                DirectoryLifecycleState::Deleted => 1,
                DirectoryLifecycleState::Purged => 2,
            };
            tx.set_remote_directory_lifecycle(&dir_id.to_string(), value).map_err(DatabaseError::operation)?;
        }
        MutationBody::BookFacts { .. } | MutationBody::BookLifecycle { .. } | MutationBody::ReadingPosition { .. } => {
            unreachable!("book scalar registers are projected together")
        }
        MutationBody::Placement { dir_id, content_hash, present, origin_folder_id } => {
            if *dir_id != sync_common::ROOT_DIR_ID {
                tx.ensure_remote_directory_placeholder(&dir_id.to_string()).map_err(DatabaseError::operation)?;
            }
            if *present {
                // Filenames are a local projection. A tombstoned placement's
                // old name may have been reused since it was removed.
                let directory = dir_id.to_string();
                let requested: String =
                    tx.query_row("SELECT COALESCE((SELECT file_name FROM book_dir WHERE dir_id=?1 AND book_row_id=(SELECT row_id FROM book WHERE content_hash=?2)),?2)", rusqlite::params![directory, content_hash.as_str()], |row| {
                        row.get(0)
                    })?;
                let key = library_replica::portable_name_key(&requested);
                let conflict: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM book_dir WHERE dir_id=?1 AND deleted_at IS NULL AND portable_name_key(file_name)=?3 AND book_row_id!=(SELECT row_id FROM book WHERE content_hash=?2))",
                    rusqlite::params![directory, content_hash.as_str(), key],
                    |row| row.get(0),
                )?;
                let chosen = if conflict {
                    let occupied = tx
                        .prepare_cached("SELECT file_name FROM book_dir WHERE dir_id=?1 AND deleted_at IS NULL AND book_row_id!=(SELECT row_id FROM book WHERE content_hash=?2)")?
                        .query_map(rusqlite::params![directory, content_hash.as_str()], |row| row.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?;
                    // A hash is a bounded fallback even when the old filename
                    // already fills the filesystem's component byte limit.
                    library_replica::unique_file_name(content_hash.as_str(), occupied.iter().map(String::as_str))
                } else {
                    requested
                };
                tx.apply_remote_book_dir_added(&directory, content_hash.as_str(), &chosen).map_err(DatabaseError::operation)?;
            } else {
                tx.apply_remote_book_dir_removed(&dir_id.to_string(), content_hash.as_str(), origin_folder_id.as_deref()).map_err(DatabaseError::operation)?;
            }
        }
        MutationBody::Annotation { annotation_id, value } => {
            let modified_at = storage_i64(value.modified_at, "annotation modification timestamp").map_err(DatabaseError::operation)?;
            let detail = value.detail_json().map_err(DatabaseError::operation)?;
            tx.upsert_annotation(annotation_id.as_str(), value.content_hash.as_str(), value.toc_ordinal, value.progress, detail.as_slice(), modified_at, value.deleted.then_some(modified_at)).map_err(DatabaseError::operation)?;
        }
        MutationBody::PdfReaderMetadata { content_hash, value } => {
            let checksum = sync_common::ContentHash::new(&value.checksum);
            if !value.valid_for(checksum.as_str()) {
                return Err(DatabaseError::message("PDF ingestion checksum or metadata is invalid"));
            }
            let bytes = serde_json::to_vec(&value).map_err(DatabaseError::operation)?;
            tx.pdf_reader_metadata_upsert(content_hash.as_str(), checksum.as_str(), &bytes).map_err(DatabaseError::operation)?;
        }
        MutationBody::BookToc { content_hash, value } => {
            // This register is a complete document: absent optional fields
            // remove prior projections within the same pull transaction.
            if let Some(tracks) = &value.tracks {
                let json = serde_json::to_string(tracks).map_err(DatabaseError::operation)?;
                tx.audiobook_upsert_tracks(content_hash.as_str(), &json).map_err(DatabaseError::operation)?;
            } else {
                tx.execute("DELETE FROM audiobook_track_index WHERE content_hash=?1", [content_hash.as_str()])?;
            }
            // Duration first, mirroring the local write order: it is what
            // turns the last chapter's start into its end.
            if let Some(duration_ms) = value.duration_ms {
                let duration_ms = i64::try_from(duration_ms).map_err(|_| DatabaseError::message("audiobook duration exceeds SQLite integer range"))?;
                tx.audiobook_upsert_metadata(content_hash.as_str(), duration_ms).map_err(DatabaseError::operation)?;
            } else {
                tx.execute("DELETE FROM audiobook_metadata WHERE content_hash=?1", [content_hash.as_str()])?;
            }
            let entry_count = i64::try_from(toc_entry_count(&value.entries)).map_err(|_| DatabaseError::message("table of contents entry count exceeds SQLite integer range"))?;
            let toc_json = serde_json::to_string(&value.entries).map_err(DatabaseError::operation)?;
            tx.shared_audiobook_upsert_toc(content_hash.as_str(), entry_count, &toc_json).map_err(DatabaseError::operation)?;
        }
        MutationBody::Description { content_hash, value } => {
            let normalized = normalized_description(value);
            tx.set_description(content_hash.as_str(), &normalized).map_err(DatabaseError::operation)?;
            tx.shared_enrichment_mark_description_scanned(content_hash.as_str()).map_err(DatabaseError::operation)?;
        }
        MutationBody::Metadata { content_hash, value } => {
            project_remote_metadata(tx, content_hash, value)?;
        }
    }
    Ok(())
}

// Called only after lifecycle winners are projected.
fn project_remote_metadata(tx: &rusqlite::Transaction<'_>, meta_hash: &sync_common::ContentHash, meta: &library_replica::SyncBookMetadata) -> Result<(), DatabaseError> {
    // Every book always gets a full, unconditional re-resolve and
    // re-projection: idempotent regardless of whether this exact
    // metadata was already stored, so no prior-state read or diff
    // decides what to skip.
    let stored_contributors = crate::contributors::resolve_credits(tx, &meta.contributors)?;
    let prepared_publishers = crate::contributors::resolve_publishers(tx, &meta.book.publishers, crate::contributors::IdentityPolicy::StableOnly)?;
    let mut prepared_book = meta.book.clone();
    prepared_book.publishers = prepared_publishers;
    let encoded = sync_common::wire::encode(&prepared_book).map_err(DatabaseError::operation)?;
    tx.meta_update_book_metadata_state_preserving_ids_update_2(meta_hash.as_str(), &meta.title, meta.subtitle.as_deref(), &encoded).map_err(DatabaseError::operation)?;
    crate::contributors::write_credits(tx, meta_hash.as_str(), &stored_contributors)?;
    project_prepared_book(tx, meta_hash.as_str(), &prepared_book, encoded)?;
    Ok(())
}

pub(crate) fn rebuild_remote_directories(tx: &rusqlite::Transaction<'_>) -> Result<(), DatabaseError> {
    // Directory writes invalidate folder membership; the next browse refreshes
    // it once from the final tree, just as for local directory batches.
    let before = tx
        .prepare_cached("SELECT id,parent_id,name,deleted_at IS NOT NULL,purged_at IS NOT NULL FROM dir")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, bool>(3)?, r.get::<_, bool>(4)?))))?
        .collect::<Result<std::collections::HashMap<_, _>, _>>()?;
    let intents = read_directory_intents(tx)?;
    let projection = library_replica::project_directories(&intents);
    // Local commands may already have installed the final display. In
    // particular, creating an empty folder must not invalidate every placement.
    let unchanged = before.iter().all(|(id, actual)| {
        if id == &sync_common::ROOT_DIR_ID.to_string() {
            return true;
        }
        let Ok(id) = sync_common::DirId::parse_str(id) else {
            return false;
        };
        match intents.get(&id) {
            Some(intent) => {
                let suppressed = projection.suppressed.contains(&id);
                let parent = if suppressed { sync_common::ROOT_DIR_ID } else { projection.parents[&id] };
                let name = projection.names.get(&id).unwrap_or(&intent.name);
                actual == &(parent.to_string(), name.clone(), suppressed, intent.lifecycle == DirectoryLifecycleState::Purged)
            }
            None => actual.2,
        }
    });
    if unchanged {
        return Ok(());
    }
    tx.hide_remote_directories().map_err(DatabaseError::operation)?;
    let mut ids: Vec<_> = intents.keys().copied().collect();
    ids.sort();
    // Detach the old tree before installing new edges. Even hidden rows are
    // traversed by asset invalidation triggers, so intermediate cycles are unsafe.
    tx.execute("UPDATE dir SET parent_id='00000000-0000-0000-0000-000000000000' WHERE id!='00000000-0000-0000-0000-000000000000' AND parent_id!='00000000-0000-0000-0000-000000000000'", [])?;
    for id in &ids {
        let intent = &intents[id];
        let projected_parent = if projection.suppressed.contains(id) { sync_common::ROOT_DIR_ID } else { projection.parents[id] };
        let projected_name = projection.names.get(id).unwrap_or(&intent.name);
        tx.project_remote_directory(&intent.id.to_string(), &projected_parent.to_string(), projected_name.as_str()).map_err(DatabaseError::operation)?;
        if intent.lifecycle == DirectoryLifecycleState::Purged {
            tx.mark_remote_directory_purged(&intent.id.to_string()).map_err(DatabaseError::operation)?;
        }
    }
    for id in projection.ordered_active {
        tx.activate_remote_directory(&id.to_string()).map_err(DatabaseError::operation)?;
    }
    #[cfg(not(target_arch = "wasm32"))]
    let mut after: Vec<(String, (String, String, bool))> = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    tx.directory_projection_rows_select(|row| {
        after.push((row.get::<_, String>(0)?, (row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, bool>(3)?)));
        Ok(())
    })
    .map_err(DatabaseError::operation)?;
    #[cfg(not(target_arch = "wasm32"))]
    for (id, state) in after {
        if before.get(&id).map(|old| (&old.0, &old.1, old.2)) != Some((&state.0, &state.1, state.2)) {
            let dir = sync_common::DirId::parse_str(&id).map_err(DatabaseError::operation)?;
            tx.shared_placements_queue_directory_work(&dir.to_string()).map_err(DatabaseError::operation)?;
        }
    }
    Ok(())
}

fn read_directory_intents(tx: &rusqlite::Transaction<'_>) -> Result<std::collections::HashMap<sync_common::DirId, DirectoryIntent>, DatabaseError> {
    // Read only canonical winners. The intent_* columns are local command
    // inputs/cache, never a second source of truth for the projected tree.
    let rows = tx
        .prepare_cached(
            "SELECT n.body,p.body,l.body FROM sync_state_version n
        JOIN sync_state_version p ON p.state_kind='directory_parent' AND p.state_key=n.state_key AND p.state_subkey=''
        JOIN sync_state_version l ON l.state_kind='directory_lifecycle' AND l.state_key=n.state_key AND l.state_subkey=''
        WHERE n.state_kind='directory_name' AND n.state_subkey='' AND n.body IS NOT NULL AND p.body IS NOT NULL AND l.body IS NOT NULL",
        )?
        .query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, Vec<u8>>(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut intents = std::collections::HashMap::new();
    let decode = |bytes: &[u8]| sync_common::wire::decode::<MutationBody>(bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation);
    for (name, parent, lifecycle) in rows {
        let (MutationBody::DirectoryName { dir_id: id, name }, MutationBody::DirectoryParent { parent_id, .. }, MutationBody::DirectoryLifecycle { value: lifecycle, .. }) = (decode(&name)?, decode(&parent)?, decode(&lifecycle)?) else {
            return Err(DatabaseError::message("invalid canonical directory registers"));
        };
        if id != sync_common::ROOT_DIR_ID {
            intents.insert(id, DirectoryIntent { id, parent_id, name, lifecycle });
        }
    }
    Ok(intents)
}
