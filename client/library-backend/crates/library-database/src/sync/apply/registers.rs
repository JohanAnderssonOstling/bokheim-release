//! One acceptance rule for local and remote registers; projection never feeds it.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) fn book_key(body: &MutationBody) -> Option<&str> {
    match body {
        MutationBody::DirectoryName { .. } | MutationBody::DirectoryParent { .. } | MutationBody::DirectoryLifecycle { .. } => None,
        MutationBody::Annotation { value, .. } => Some(value.content_hash.as_str()),
        MutationBody::BookFacts { content_hash, .. }
        | MutationBody::BookLifecycle { content_hash, .. }
        | MutationBody::Placement { content_hash, .. }
        | MutationBody::ReadingPosition { content_hash, .. }
        | MutationBody::Metadata { content_hash, .. }
        | MutationBody::Description { content_hash, .. }
        | MutationBody::PdfReaderMetadata { content_hash, .. }
        | MutationBody::BookToc { content_hash, .. } => Some(content_hash.as_str()),
    }
}

/// Caller owns the transaction. Book fields exist only while their book exists.
pub(crate) fn merge(conn: &Connection, body: &MutationBody, version: &library_replica::VersionKey) -> Result<bool, DatabaseError> {
    merge_inner(conn, body, version, true)
}

fn merge_inner(conn: &Connection, body: &MutationBody, version: &library_replica::VersionKey, invalidate_publication: bool) -> Result<bool, DatabaseError> {
    validate_value(body)?;
    if !matches!(body, MutationBody::BookLifecycle { .. }) {
        if let Some(hash) = book_key(body) {
            if !conn.query_row("SELECT EXISTS(SELECT 1 FROM book WHERE content_hash=?1)", [hash], |r| r.get::<_, bool>(0))? {
                return Ok(false);
            }
        }
    }
    let (kind, key, subkey) = sync_state_identity(body);
    let current = read_version(conn, kind, &key, &subkey)?;
    if version.conflict_rank != body.conflict_rank() {
        return Err(DatabaseError::message("mutation conflict rank does not match its value"));
    }
    if let Some(stored) = &current {
        if version < stored || (version == stored && canonical_body(conn, kind, &key, &subkey)?.is_some()) {
            return Ok(false);
        }
    }
    let previous_book: Option<String> = conn.query_row("SELECT book_key FROM sync_state_version WHERE state_kind=?1 AND state_key=?2 AND state_subkey=?3", params![kind, key, subkey], |r| r.get(0)).optional()?.flatten();
    if previous_book.as_deref() != book_key(body) {
        if let Some(hash) = previous_book {
            conn.execute("INSERT OR IGNORE INTO sync_projection_dirty VALUES('book_projection',?1,'')", [hash])?;
        }
    }
    let encoded = sync_common::wire::encode(body).map_err(DatabaseError::operation)?;
    conn.execute(
        "INSERT INTO sync_state_version(state_kind,state_key,state_subkey,changed_at,conflict_rank,replica_id,replica_seq,mutation_id,body,book_key)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(state_kind,state_key,state_subkey) DO UPDATE SET changed_at=excluded.changed_at,conflict_rank=excluded.conflict_rank,
         replica_id=excluded.replica_id,replica_seq=excluded.replica_seq,mutation_id=excluded.mutation_id,body=excluded.body,book_key=excluded.book_key",
        params![
            kind,
            key,
            subkey,
            storage_i64(version.changed_at, "register timestamp")?,
            version.conflict_rank,
            version.replica_id.to_string(),
            storage_i64(version.replica_seq, "register sequence")?,
            version.mutation_id.to_string(),
            encoded,
            book_key(body)
        ],
    )?;
    conn.execute("INSERT OR IGNORE INTO sync_projection_dirty VALUES(?1,?2,?3)", params![kind, key, subkey])?;
    if invalidate_publication && conn.query_row("SELECT EXISTS(SELECT 1 FROM sync_outbox WHERE state_kind=?1 AND state_key=?2 AND state_subkey=?3)", params![kind, key, subkey], |r| r.get::<_, bool>(0))? {
        schedule(conn, kind, &key, &subkey, None)?;
    }
    Ok(true)
}

/// SQL producers submit values, never construct publication payloads or versions.
pub(crate) fn accept_local(ctx: &rusqlite::functions::Context<'_>) -> rusqlite::Result<bool> {
    let run = || -> Result<bool, DatabaseError> {
        // SAFETY: SQLite owns this connection for the duration of the scalar call.
        let conn = unsafe { ctx.get_connection()? };
        let encoded: Vec<u8> = ctx.get(0)?;
        let body: MutationBody = sync_common::wire::decode(&encoded, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?;
        accept_value(&conn, &body, nonnegative_u64(ctx.get(1)?, 1, "local timestamp")?, None)?;
        Ok(true)
    };
    run().map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
}

fn next_sequence(conn: &Connection) -> Result<u64, DatabaseError> {
    let sequence: i64 = conn.query_row("UPDATE sync_clock SET sequence=sequence+1 WHERE singleton=1 AND sequence<9223372036854775807 RETURNING sequence", [], |r| r.get(0))?;
    nonnegative_u64(sequence, 0, "replica sequence").map_err(Into::into)
}

fn local_replica(conn: &Connection) -> Result<uuid::Uuid, DatabaseError> {
    let raw: String = conn.query_row("SELECT replica_id FROM sync_metadata WHERE singleton=1", [], |r| r.get(0))?;
    Ok(parse_uuid_column(raw, 0)?)
}

pub(crate) fn accept_value(conn: &Connection, body: &MutationBody, time: u64, id: Option<MutationId>) -> Result<(), DatabaseError> {
    let (kind, key, subkey) = sync_state_identity(body);
    let current = read_version(conn, kind, &key, &subkey)?;
    let time = match current {
        Some(current) => time.max(current.changed_at.checked_add(1).ok_or_else(|| DatabaseError::message("register clock exhausted"))?),
        None => time,
    };
    let version = library_replica::VersionKey::new(time, body.conflict_rank(), local_replica(conn)?, next_sequence(conn)?, id.unwrap_or_else(|| MutationId::parse(&uuid::Uuid::new_v4().to_string()).expect("UUID")));
    if merge_inner(conn, body, &version, false)? {
        schedule(conn, kind, &key, &subkey, Some(&version))?;
    }
    Ok(())
}

/// A queue generation has no payload until preparation. Superseding it assigns
/// a new identity, so an acknowledgement of an in-flight older snapshot is safe.
fn schedule(conn: &Connection, kind: &str, key: &str, subkey: &str, local: Option<&library_replica::VersionKey>) -> Result<(), DatabaseError> {
    let (seq, id) = match local {
        Some(version) => (version.replica_seq, version.mutation_id.to_string()),
        None => (next_sequence(conn)?, uuid::Uuid::new_v4().to_string()),
    };
    conn.execute(
        "INSERT INTO sync_outbox(replica_seq,mutation_id,state_kind,state_key,state_subkey) VALUES(?1,?2,?3,?4,?5)
      ON CONFLICT(state_kind,state_key,state_subkey) DO UPDATE SET replica_seq=excluded.replica_seq,mutation_id=excluded.mutation_id,body=NULL,changed_at=NULL,origin=NULL",
        params![storage_i64(seq, "queue sequence")?, id, kind, key, subkey],
    )?;
    Ok(())
}

/// Recovery queues canonical identities only. Merge has already invalidated any
/// older queued snapshot. Unknown payloads stay unknown and cannot be published.
pub(crate) fn recover(conn: &Connection, cell: &sync_common::StateCell) -> Result<bool, DatabaseError> {
    if canonical_body(conn, &cell.kind, &cell.entity_key, &cell.entity_subkey)?.is_none() {
        return Ok(false);
    }
    let queued: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sync_outbox WHERE state_kind=?1 AND state_key=?2 AND state_subkey=?3)", params![cell.kind, cell.entity_key, cell.entity_subkey], |r| r.get(0))?;
    if queued {
        return Ok(false);
    }
    schedule(conn, &cell.kind, &cell.entity_key, &cell.entity_subkey, None)?;
    Ok(true)
}

/// Freeze a durable request snapshot under the writer reservation. Repeated
/// reads return exactly the same ID, version and payload until a new edit wins.
pub(crate) fn prepare_publications(conn: &Connection) -> Result<(), DatabaseError> {
    let pending = conn
        .prepare("SELECT state_kind,state_key,state_subkey,replica_seq,mutation_id FROM sync_outbox WHERE body IS NULL ORDER BY replica_seq")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let replica = local_replica(conn)?;
    for (kind, key, subkey, seq, id) in pending {
        let Some((bytes, _)) = canonical_body(conn, &kind, &key, &subkey)? else { continue };
        let current = read_version(conn, &kind, &key, &subkey)?.ok_or_else(|| DatabaseError::message("queued register has no version"))?;
        let id = MutationId::parse(&id).map_err(DatabaseError::operation)?;
        let origin = if current.replica_id == replica && current.mutation_id == id && current.replica_seq == nonnegative_u64(seq, 0, "queue sequence")? {
            None
        } else {
            Some(sync_common::MutationOrigin { replica_id: current.replica_id, replica_seq: sync_common::ReplicaSeq::new(current.replica_seq).map_err(DatabaseError::operation)?, mutation_id: current.mutation_id })
        };
        let origin = origin.as_ref().map(sync_common::wire::encode).transpose().map_err(DatabaseError::operation)?;
        // Recovery forwards a winner; it must never manufacture a newer edit.
        conn.execute("UPDATE sync_outbox SET body=?1,changed_at=?2,origin=?3 WHERE mutation_id=?4", params![bytes, storage_i64(current.changed_at, "publication timestamp")?, origin, id.to_string()])?;
    }
    Ok(())
}

pub(crate) fn canonical_body(conn: &Connection, kind: &str, key: &str, subkey: &str) -> Result<Option<(Vec<u8>, i64)>, DatabaseError> {
    Ok(conn.query_row("SELECT body,conflict_rank FROM sync_state_version WHERE state_kind=?1 AND state_key=?2 AND state_subkey=?3 AND body IS NOT NULL", params![kind, key, subkey], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

pub(crate) fn commit(tx: rusqlite::Transaction<'_>) -> Result<(), DatabaseError> {
    super::pull::project_dirty(&tx)?;
    tx.commit()?;
    Ok(())
}

// Validation must not depend on whether the book happens to be purged today.
// Otherwise an accepted value could poison a later reimport's projection.
fn validate_value(body: &MutationBody) -> Result<(), DatabaseError> {
    match body {
        MutationBody::ReadingPosition { value, .. } => {
            book_model::ReadProgress::new(value.progress).map_err(|error| DatabaseError::message(error.to_string()))?;
        }
        MutationBody::BookFacts { value: library_replica::BookFacts { added_at, .. }, .. } => {
            storage_i64(*added_at, "book addition timestamp")?;
        }
        MutationBody::Annotation { value, .. } => {
            storage_i64(value.modified_at, "annotation modification timestamp")?;
            if value.toc_ordinal.is_some_and(|ordinal| ordinal < 0) {
                return Err(DatabaseError::message("negative annotation TOC ordinal"));
            }
        }
        MutationBody::PdfReaderMetadata { value, .. } => {
            if !value.valid_for(&value.checksum) {
                return Err(DatabaseError::message("invalid PDF metadata"));
            }
        }
        MutationBody::BookToc { value, .. } => {
            if let Some(duration) = value.duration_ms {
                storage_i64(duration, "audiobook duration")?;
            }
            if let Some(tracks) = &value.tracks {
                let mut end = 0u64;
                let mut names = std::collections::HashSet::new();
                if tracks.is_empty()
                    || tracks.len() > 1024
                    || tracks.iter().any(|track| {
                        let invalid = !track.valid() || track.start_ms != end || !names.insert(&track.name);
                        end = track.end_ms;
                        invalid
                    })
                    || value.duration_ms != Some(end)
                {
                    return Err(DatabaseError::message("invalid audiobook track index"));
                }
            }
        }
        _ => {}
    }
    Ok(())
}
