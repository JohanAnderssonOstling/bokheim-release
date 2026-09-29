// ---- operations ----
// Durable import/sync workflow status, layered over one-snapshot progress
// accounting. Records are device-local; progress reflects durable work, not
// job lifetimes.

use super::OperationsSql;
use crate::{Database, DatabaseError};
use library_replica::{LibraryOperation, LibraryOperationKind as Kind, LibraryOperationState as State};

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    id: String,
    kind: Kind,
    scan_complete: bool,
    scan_failed: bool,
    discovered: u64,
    failures: u64,
    requires_sync: bool,
    completed: bool,
    #[serde(default)]
    sync_unfinished: bool,
}

impl Record {
    fn new(kind: Kind, requires_sync: bool) -> Self {
        Self { id: uuid::Uuid::new_v4().to_string(), kind, scan_complete: kind == Kind::Sync, scan_failed: false, discovered: 0, failures: 0, requires_sync, completed: false, sync_unfinished: false }
    }
}

fn flag(value: bool) -> i64 {
    i64::from(value)
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Import => "Import",
        Kind::Sync => "Sync",
    }
}

fn parse_kind(value: &str) -> rusqlite::Result<Kind> {
    match value {
        "Import" => Ok(Kind::Import),
        "Sync" => Ok(Kind::Sync),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, format!("unknown library operation {value}").into())),
    }
}

fn read(connection: &rusqlite::Connection) -> Result<Option<Record>, DatabaseError> {
    let mut record = None;
    connection.read_operation(|row| {
        let id: Option<String> = row.get(0)?;
        let Some(id) = id else { return Ok(()) };
        let kind: Option<String> = row.get(1)?;
        let get_flag = |index: usize| row.get::<_, Option<i64>>(index).map(|value| value.unwrap_or(0) != 0);
        record = Some(Record {
            id,
            kind: parse_kind(kind.as_deref().unwrap_or(""))?,
            scan_complete: get_flag(2)?,
            scan_failed: get_flag(3)?,
            discovered: row.get::<_, Option<i64>>(4)?.unwrap_or(0).max(0) as u64,
            failures: row.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as u64,
            requires_sync: get_flag(6)?,
            completed: get_flag(7)?,
            sync_unfinished: get_flag(8)?,
        });
        Ok(())
    })?;
    Ok(record)
}

fn write(connection: &rusqlite::Connection, record: &Record) -> Result<(), DatabaseError> {
    connection.write_operation(
        &record.id,
        kind_name(record.kind),
        flag(record.scan_complete),
        flag(record.scan_failed),
        record.discovered.min(i64::MAX as u64) as i64,
        record.failures.min(i64::MAX as u64) as i64,
        flag(record.requires_sync),
        flag(record.completed),
        flag(record.sync_unfinished),
    )?;
    Ok(())
}

#[derive(Default)]
pub struct OperationActivity<'a> {
    pub asset_uploads_disabled: bool,
    pub scanning: bool,
    pub discovered: u64,
    pub scan_failures: u64,
    pub local_busy: bool,
    pub local_waiting: bool,
    pub sync_busy: bool,
    pub signed_in: bool,
    pub file_error: Option<&'a str>,
    pub transfer_retrying: bool,
}

pub struct OperationStatus {
    pub operation: Option<LibraryOperation>,
    pub total_books: u64,
    pub uploaded_books: u64,
}

impl Database {
    /// The scan protocol owns this transition, even when Settings is closed.
    /// Use the snapshot transaction so an interrupted scan remains recoverable.
    #[cfg(feature = "scanner")]
    pub(crate) fn begin_filesystem_scan_operation(connection: &rusqlite::Connection) -> Result<(), DatabaseError> {
        let mut record = read(connection)?.filter(|record| !record.completed).unwrap_or_else(|| Record::new(Kind::Import, false));
        record.kind = Kind::Import;
        record.scan_complete = false;
        record.scan_failed = false;
        record.failures = 0;
        connection.clear_scan_failures()?;
        write(connection, &record)
    }

    #[cfg(feature = "scanner")]
    pub(crate) fn record_filesystem_scan_failures(connection: &rusqlite::Connection, failures: &[(String, String)]) -> Result<(), DatabaseError> {
        for (path, error) in failures {
            connection.record_scan_failure(path, error)?;
        }
        Ok(())
    }

    /// Commit completion with the final inventory batch, never with a partial
    /// or stale result. An unreadable directory walk still requires retry;
    /// individual inspection failures are stored separately and do not block completion.
    #[cfg(feature = "scanner")]
    pub(crate) fn finish_filesystem_scan_operation(connection: &rusqlite::Connection, readable: bool) -> Result<(), DatabaseError> {
        if let Some(mut record) = read(connection)? {
            record.scan_complete = readable;
            record.scan_failed = !readable;
            write(connection, &record)?;
        }
        Ok(())
    }

    /// Marks a sync workflow as needed, preserving an existing unfinished
    /// import as the owner of its metadata sync jobs.
    pub fn begin_sync_operation(&self) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let current = read(&transaction)?;
        if current.as_ref().is_some_and(|record| !record.completed && record.requires_sync && record.sync_unfinished == (record.kind == Kind::Sync)) {
            transaction.commit()?;
            return Ok(());
        }
        let mut record = current.filter(|record| !record.completed).unwrap_or_else(|| Record::new(Kind::Sync, true));
        record.requires_sync = true;
        // Import already has durable scan and upload/outbox dependencies. Do
        // not add another pair of disk commits for every metadata batch it produces.
        record.sync_unfinished = record.kind == Kind::Sync;
        write(&transaction, &record)?;
        transaction.commit()?;
        Ok(())
    }

    /// Commits a finished sync/exchange pass in one transaction: optionally
    /// records the successful-sync timestamp and clears the workflow's
    /// unfinished flag. The local library is authoritative, so completion
    /// never depends on which account is signed in.
    pub fn finish_state_sync(&self, completed_at: Option<u64>) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if let Some(completed_at) = completed_at {
            transaction.operations_set_last_successful_sync_at(&completed_at.to_string())?;
        }
        if let Some(mut record) = read(&transaction)?.filter(|record| !record.completed && record.sync_unfinished) {
            record.sync_unfinished = false;
            write(&transaction, &record)?;
        }
        transaction.commit()?;
        // A sync worker may be the last owner of an import's durable work.
        // Do not require a later Settings/status read to persist completion.
        self.reconcile_operation(OperationActivity::default())?;
        Ok(())
    }

    /// Reconcile a durable operation when one of its workers finishes.
    ///
    /// The operation snapshot remains the single definition of completion,
    /// but background workers—not the presentation layer—must invoke it after
    /// changing a durable queue. This makes completion survive an idle UI.
    pub fn reconcile_operation(&self, activity: OperationActivity<'_>) -> Result<(), DatabaseError> {
        let _ = self.operation_status(activity)?;
        Ok(())
    }

    /// Reconciles a workflow against a consistent queue snapshot. Only
    /// workflow creation/completion writes a record; ordinary progress reads
    /// do not commit unless the record already needs one of those transitions.
    pub fn operation_status(&self, activity: OperationActivity<'_>) -> Result<OperationStatus, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut progress = None;
        transaction.operation_progress(|row| {
            progress = Some((
                row.get::<_, i64>(0)?.max(0) as u64,
                row.get::<_, i64>(1)?.max(0) as u64,
                row.get::<_, i64>(2)?.max(0) as u64,
                row.get::<_, i64>(3)?.max(0) as u64,
                row.get::<_, i64>(4)?.max(0) as u64,
                row.get::<_, i64>(5)?.max(0) as u64,
                row.get::<_, i64>(6)?.max(0) as u64,
                row.get(7)?,
            ));
            Ok(())
        })?;
        let (books, uploaded_books, pending_uploads, pending_changes, pending_thumbnails, pending_covers, rejections, checkpoint): (u64, u64, u64, u64, u64, u64, u64, bool) =
            progress.ok_or_else(|| DatabaseError::message("operation progress query returned no row"))?;
        let (pending_uploads, pending_covers, rejections) = if activity.asset_uploads_disabled { (0, 0, 0) } else { (pending_uploads, pending_covers, rejections) };
        let mut scan_failures = Vec::new();
        transaction.read_scan_failures(|row| {
            scan_failures.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
            Ok(())
        })?;
        let mut stored = read(&transaction)?;
        let remote_pending = pending_uploads + pending_changes + pending_covers > 0;
        if stored.as_ref().is_none_or(|record| record.completed) {
            if activity.scanning {
                let record = Record::new(Kind::Import, activity.signed_in);
                write(&transaction, &record)?;
                stored = Some(record);
            } else if activity.signed_in && (remote_pending || activity.sync_busy) {
                let record = Record::new(Kind::Sync, true);
                write(&transaction, &record)?;
                stored = Some(record);
            } else if scan_failures.is_empty() || stored.is_none() {
                transaction.commit()?;
                return Ok(OperationStatus { operation: None, total_books: books, uploaded_books });
            }
        }
        let mut record = stored.expect("operation established above");
        if activity.signed_in && !record.requires_sync {
            record.requires_sync = true;
            write(&transaction, &record)?;
        }
        // Local imports retain syncable data for a future account, but those
        // rows are not pending uploads or failures of a local-only workflow.
        let (pending_uploads, pending_changes, rejections) = if record.requires_sync { (pending_uploads, pending_changes, rejections) } else { (0, 0, 0) };
        let local_pending = !record.scan_complete || activity.scanning || activity.local_busy || pending_thumbnails > 0;
        let sync_pending = record.requires_sync && (remote_pending || activity.sync_busy || record.sync_unfinished || !checkpoint);
        let waiting_reason = if record.scan_failed {
            Some("Some library files could not be scanned; retrying automatically".to_owned())
        } else if let Some(error) = activity.file_error {
            Some(error.to_owned())
        } else if sync_pending && !activity.signed_in {
            Some("Sign in to finish syncing".to_owned())
        } else if record.requires_sync && (rejections > 0 || activity.transfer_retrying) {
            Some("Waiting to retry uploads".to_owned())
        } else if !activity.scanning && activity.local_waiting && pending_thumbnails > 0 {
            Some("Waiting to retry thumbnails".to_owned())
        } else if !activity.scanning && !activity.local_busy && !activity.sync_busy && (local_pending || sync_pending) {
            Some("Waiting for background work".to_owned())
        } else {
            None
        };
        let complete = !local_pending && !sync_pending && waiting_reason.is_none();
        if complete && !record.completed {
            record.completed = true;
            write(&transaction, &record)?;
        }
        let result = LibraryOperation {
            id: record.id,
            kind: record.kind,
            state: if complete {
                State::Completed
            } else if waiting_reason.is_some() {
                State::Waiting
            } else {
                State::Running
            },
            scanning: activity.scanning,
            requires_sync: record.requires_sync,
            // Counts describe library state and never reset for an individual batch.
            books: books.max(record.discovered).max(activity.discovered),
            uploaded_books,
            pending_uploads,
            pending_covers,
            pending_changes,
            pending_thumbnails,
            failures: record.failures.max(activity.scan_failures).max(scan_failures.len() as u64) + rejections,
            scan_failures,
            needs_attention: record.scan_failed
                || activity.file_error.is_some()
                || (sync_pending && !activity.signed_in)
                || (record.requires_sync && (rejections > 0 || activity.transfer_retrying))
                || (!activity.scanning && activity.local_waiting && pending_thumbnails > 0),
            waiting_reason,
        };
        transaction.commit()?;
        Ok(OperationStatus { operation: Some(result), total_books: books, uploaded_books })
    }
}
