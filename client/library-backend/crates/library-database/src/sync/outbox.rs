//! Sync outbox snapshots and acknowledgements.

use crate::sync::SyncOutboxSql;
use crate::{Database, DatabaseError};
use library_replica::{MutationBody, StateMutation};
use sync_common::MutationId;
use sync_common::ReplicaSeq;

// Sync outbox snapshots and acknowledgements.
//
// The orchestrator reads a publishable snapshot, performs its network work
// with no database reference held, then commits acknowledgements back in
// caller-sized batches. One method call is one transaction.

#[derive(Clone, Debug)]
struct OutboxRow {
    mutation_id: String,
    state_kind: String,
    state_key: String,
    body: Vec<u8>,
    changed_at: i64,
    replica_seq: i64,
    state_subkey: String,
    origin: Option<Vec<u8>>,
}

fn decode_outbox_row(row: OutboxRow) -> Result<StateMutation, DatabaseError> {
    let corrupt = |reason: &'static str| DatabaseError::message(format!("corrupt local sync mutation at replica sequence {}: {reason}", row.replica_seq));
    let mutation_id = MutationId::parse(&row.mutation_id).map_err(|_| corrupt("invalid mutation id"))?;
    let body: MutationBody = sync_common::wire::decode(&row.body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(|_| corrupt("body is not a canonical mutation"))?;
    let (kind, key, subkey) = crate::sync::apply::sync_state_identity(&body);
    if row.state_kind != kind || row.state_key != key || row.state_subkey != subkey {
        return Err(corrupt("stored coalescing identity does not match the mutation body"));
    }
    let changed_at = u64::try_from(row.changed_at).map_err(|_| corrupt("negative timestamp"))?;
    let replica_seq = ReplicaSeq::new(u64::try_from(row.replica_seq).map_err(|_| corrupt("negative replica sequence"))?).map_err(|_| corrupt("replica sequence must be positive"))?;
    Ok(StateMutation { origin: row.origin.as_deref().map(|bytes| sync_common::wire::decode(bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES)).transpose().map_err(DatabaseError::operation)?, mutation_id, body, changed_at, replica_seq })
}

/// Pending state stays durable while its exact local book version is
/// uploading (including rejected/retryable jobs). Only completion releases it.
fn book_ready(mutation: &StateMutation, pending: &std::collections::HashSet<String>) -> bool {
    mutation.body.upload_dependency().is_none_or(|hash| !pending.contains(hash.as_str()))
}

/// Content hashes whose exact local book version is still uploading. Pending
/// state stays durable until completion releases it.
fn outbox_pending_uploads(connection: &rusqlite::Connection) -> Result<std::collections::HashSet<String>, DatabaseError> {
    let mut cloud_storage_enabled: Option<i64> = None;
    connection.sync_cloud_storage_enabled(|row| {
        cloud_storage_enabled = row.get(0)?;
        Ok(())
    })?;
    if cloud_storage_enabled == Some(0) {
        return Ok(std::collections::HashSet::new());
    }
    let mut pending = std::collections::HashSet::new();
    connection.sync_pending_uploads(|row| {
        pending.insert(row.get::<_, String>(0)?);
        Ok(())
    })?;
    Ok(pending)
}

impl Database {
    /// Freeze and select a publication snapshot under one writer reservation.
    fn with_publications<T>(&self, read: impl FnOnce(&rusqlite::Connection) -> Result<T, DatabaseError>) -> Result<T, DatabaseError> {
        self.with_write_transaction(|tx| {
            crate::sync::apply::registers::prepare_publications(tx)?;
            Ok(crate::transactions::WriteOutcome::Commit(read(tx)?))
        })
    }

    /// Reads every outbox mutation whose book dependencies are
    /// satisfied. The returned snapshot is plain data: the caller performs
    /// network work with no database reference held, then commits
    /// acknowledgements with [`Database::sync_acknowledge_mutations`].
    pub fn sync_publishable_mutations(&self) -> Result<Vec<StateMutation>, DatabaseError> {
        self.with_publications(|connection| {
            let pending = outbox_pending_uploads(connection)?;
            let mut rows = Vec::new();
            connection.sync_outbox_rows(|row| {
                rows.push(OutboxRow { mutation_id: row.get(0)?, state_kind: row.get(1)?, state_key: row.get(2)?, body: row.get(3)?, changed_at: row.get(4)?, replica_seq: row.get(5)?, state_subkey: row.get(6)?, origin: row.get(7)? });
                Ok(())
            })?;
            let mut mutations = Vec::with_capacity(rows.len());
            for row in rows {
                let mutation = decode_outbox_row(row)?;
                if book_ready(&mutation, &pending) {
                    mutations.push(mutation);
                }
            }
            Ok(mutations)
        })
    }

    /// Fixes the end of an upload scan so continuous local edits cannot keep
    /// it running. The coordinator pages up to this sequence number.
    pub fn sync_outbox_watermark(&self) -> Result<i64, DatabaseError> {
        let mut watermark = 0i64;
        self.connection.sync_outbox_watermark(|row| {
            watermark = row.get(0)?;
            Ok(())
        })?;
        Ok(watermark)
    }

    /// Reads one bridge-sized page of publishable outbox mutations. Keyset
    /// paging stays valid when acknowledgments remove rows between pages.
    /// At least one row is returned even if its body exceeds the byte
    /// target, so oversized mutations cannot starve later rows.
    pub fn sync_pending_mutations_page(&self, after: i64, through: i64) -> Result<Vec<StateMutation>, DatabaseError> {
        self.with_publications(|connection| {
            const BYTE_TARGET: usize = 256 * 1024;
            let pending = outbox_pending_uploads(connection)?;
            let mut rows = Vec::new();
            connection.sync_outbox_rows_page(after, through, |row| {
                rows.push(OutboxRow { mutation_id: row.get(0)?, state_kind: row.get(1)?, state_key: row.get(2)?, body: row.get(3)?, changed_at: row.get(4)?, replica_seq: row.get(5)?, state_subkey: row.get(6)?, origin: row.get(7)? });
                Ok(())
            })?;
            let mut mutations = Vec::new();
            let mut bytes = 0usize;
            for stored in rows {
                let body_bytes = stored.body.len();
                let mutation = decode_outbox_row(stored)?;
                if !book_ready(&mutation, &pending) {
                    continue;
                }
                if !mutations.is_empty() && bytes + body_bytes > BYTE_TARGET {
                    break;
                }
                bytes += body_bytes;
                mutations.push(mutation);
                if bytes >= BYTE_TARGET || mutations.len() >= 256 {
                    break;
                }
            }
            Ok(mutations)
        })
    }

    /// Enqueues one reading-position change directly into the durable outbox.
    pub fn enqueue_outbox_reading_change(&self, mutation_id: &str, state_key: &str, body: &[u8], changed_at: i64) -> Result<(), DatabaseError> {
        self.with_write_transaction(|tx| {
            let body: MutationBody = sync_common::wire::decode(body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?;
            if !matches!(&body,MutationBody::ReadingPosition { content_hash,.. } if content_hash.as_str()==state_key) {
                return Err(DatabaseError::message("reading identity mismatch"));
            }
            crate::sync::apply::registers::accept_value(tx, &body, u64::try_from(changed_at).map_err(DatabaseError::operation)?, Some(MutationId::parse(mutation_id).map_err(DatabaseError::operation)?))?;
            Ok(crate::transactions::WriteOutcome::Commit(()))
        })
    }

    /// Drops every durable outbox entry.
    pub fn clear_sync_outbox(&self) -> Result<(), DatabaseError> {
        self.connection.clear_sync_outbox()?;
        Ok(())
    }

    /// Seeds one outbox entry with an explicit replica sequence, e.g. a
    /// poison mutation the server will permanently reject.
    pub fn seed_poison_outbox_mutation(&self, replica_seq: i64, mutation_id: &str, body: &[u8], changed_at: i64) -> Result<(), DatabaseError> {
        self.connection.seed_poison_outbox_mutation(replica_seq, mutation_id, body, changed_at)?;
        Ok(())
    }

    /// Acknowledges exactly these mutation ids in one transaction. Callers
    /// batch large id sets into repeated calls; batching policy belongs to
    /// the orchestrator, not to storage.
    pub fn sync_acknowledge_mutations(&self, ids: &[MutationId]) -> Result<(), DatabaseError> {
        if ids.is_empty() {
            return Ok(());
        }
        let payload = serde_json::to_string(&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>()).map_err(DatabaseError::operation)?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.sync_acknowledge_mutations(&payload)?;
        transaction.commit()?;
        Ok(())
    }
}
