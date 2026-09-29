//! Native (on-device) filesystem state: work, directories, staged books.

use crate::shared_sql::SharedSql;
use rusqlite::OptionalExtension;
// ---- native ----
// Native staged-book snapshots and conditional projection commits.

use crate::BookPlacement;
use crate::{Database, DatabaseError};
use include_sqlite_sql::include_sql;
use sync_common::{ContentHash, DirId};

#[derive(Clone, Debug)]
pub struct NativeBookSnapshot {
    pub content_hash: ContentHash,
    pub directory: DirId,
    pub name: String,
    pub relative_path: String,
    pub occupied_names: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct NativeBookCompletion {
    pub snapshot: NativeBookSnapshot,
    pub name: String,
    pub relative_path: String,
    pub published: bool,
    pub fingerprint: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeBookAcknowledgement {
    Applied,
    Obsolete,
}

impl Database {
    pub fn native_book_snapshot(&self, placement: &BookPlacement, hash: &ContentHash) -> Result<Option<NativeBookSnapshot>, DatabaseError> {
        let connection = &self.connection;
        let Some((current_hash, directory, name)) = Self::native_book_entry(connection, placement.rel_path.as_str())? else {
            return Ok(None);
        };
        if &current_hash != hash {
            return Ok(None);
        }
        let relative_path = placement.rel_path.as_str().to_owned();
        if relative_path != placement.rel_path.as_str() {
            return Ok(None);
        }
        let occupied_names = Self::native_occupied_names(connection, &directory)?;
        Ok(Some(NativeBookSnapshot { content_hash: *hash, directory, name, relative_path, occupied_names }))
    }

    pub fn acknowledge_native_book(&self, completion: NativeBookCompletion) -> Result<NativeBookAcknowledgement, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let current = Self::native_book_entry(&transaction, &completion.snapshot.relative_path)?;
        let Some((hash, directory, name)) = current else { return Ok(NativeBookAcknowledgement::Obsolete) };
        if hash != completion.snapshot.content_hash || directory != completion.snapshot.directory || name != completion.snapshot.name {
            return Ok(NativeBookAcknowledgement::Obsolete);
        }
        let current_relative = completion.snapshot.relative_path.clone();
        if current_relative != completion.snapshot.relative_path {
            return Ok(NativeBookAcknowledgement::Obsolete);
        }
        let mut conflict = false;
        transaction.native_book_name_conflict(&directory.to_string(), &library_replica::portable_name_key(&completion.name), completion.snapshot.content_hash.as_str(), |row| {
            conflict = row.get(0)?;
            Ok(())
        })?;
        if conflict {
            return Ok(NativeBookAcknowledgement::Obsolete);
        }
        transaction.native_rename_book(&directory.to_string(), completion.snapshot.content_hash.as_str(), &completion.name)?;
        transaction.shared_scanner_publish_projection(completion.snapshot.content_hash.as_str(), &directory.to_string(), &completion.relative_path)?;
        if completion.published {
            let fingerprint = completion.fingerprint.ok_or_else(|| DatabaseError::message("published native file has no fingerprint"))?;
            transaction.native_set_book_fingerprint(&directory.to_string(), completion.snapshot.content_hash.as_str(), &fingerprint)?;
        }
        crate::sync::apply::commit(transaction)?;
        Ok(NativeBookAcknowledgement::Applied)
    }

    /// Finalize a downloaded book after the caller has published the
    /// verified bytes. This keeps all durable transfer state in one guarded
    /// transaction and never exposes SQLite to the transfer executor.
    pub fn complete_downloaded_book(&self, completion: NativeBookCompletion, thumbnail_set_exists: bool) -> Result<NativeBookAcknowledgement, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let acknowledgement = Self::complete_downloaded_book_in_transaction(&transaction, &completion, thumbnail_set_exists)?;
        crate::sync::apply::commit(transaction)?;
        Ok(acknowledgement)
    }

    pub(crate) fn complete_downloaded_book_in_transaction(connection: &rusqlite::Connection, completion: &NativeBookCompletion, thumbnail_set_exists: bool) -> Result<NativeBookAcknowledgement, DatabaseError> {
        let current = Self::native_book_entry(connection, &completion.snapshot.relative_path)?;
        let Some((hash, directory, name)) = current else { return Ok(NativeBookAcknowledgement::Obsolete) };
        if hash != completion.snapshot.content_hash || directory != completion.snapshot.directory || name != completion.snapshot.name {
            return Ok(NativeBookAcknowledgement::Obsolete);
        }
        let mut conflict = false;
        connection.native_book_name_conflict(&directory.to_string(), &library_replica::portable_name_key(&completion.name), completion.snapshot.content_hash.as_str(), |row| {
            conflict = row.get(0)?;
            Ok(())
        })?;
        if conflict {
            return Ok(NativeBookAcknowledgement::Obsolete);
        }
        connection.native_rename_book(&directory.to_string(), completion.snapshot.content_hash.as_str(), &completion.name)?;
        connection.shared_scanner_publish_projection(completion.snapshot.content_hash.as_str(), &directory.to_string(), &completion.relative_path)?;
        if completion.published {
            connection.native_set_book_fingerprint(&directory.to_string(), completion.snapshot.content_hash.as_str(), completion.fingerprint.as_deref().ok_or_else(|| DatabaseError::message("published native file has no fingerprint"))?)?;
        }
        connection.native_record_remote_book(completion.snapshot.content_hash.as_str())?;
        connection.native_mark_hash_downloaded(completion.snapshot.content_hash.as_str())?;
        connection.shared_transfer_complete_download(completion.snapshot.content_hash.as_str())?;
        connection.native_queue_book_work(completion.snapshot.content_hash.as_str())?;
        if !thumbnail_set_exists {
            connection.native_request_thumbnail(completion.snapshot.content_hash.as_str())?;
        }
        Ok(NativeBookAcknowledgement::Applied)
    }

    fn native_book_entry(connection: &rusqlite::Connection, relative_path: &str) -> Result<Option<(ContentHash, DirId, String)>, DatabaseError> {
        let mut entry = None;
        connection.native_book_entry(relative_path, |row| {
            entry = Some((ContentHash::new(&row.get::<_, String>(0)?), row.get::<_, String>(1)?, row.get::<_, String>(2)?));
            Ok(())
        })?;
        entry.map(|(hash, directory, name)| DirId::parse_str(&directory).map(|directory| (hash, directory, name)).map_err(DatabaseError::operation)).transpose()
    }
    fn native_occupied_names(connection: &rusqlite::Connection, directory: &DirId) -> Result<Vec<String>, DatabaseError> {
        let mut names = Vec::new();
        connection.native_occupied_file_names(&directory.to_string(), |row| {
            names.push(row.get(0)?);
            Ok(())
        })?;
        Ok(names)
    }
}

include_sql!("src/native/sql/schema.sql");
include_sql!("src/native/sql/native_file_work.sql");

// ---- work ----
// Native file-work queue: listing, snapshots, acknowledgements, purge.

#[derive(Clone, Debug)]
pub struct NativeFileWork {
    pub id: i64,
    pub operation: String,
    pub content_hash: ContentHash,
    pub original_path: String,
}

#[derive(Clone, Debug)]
pub struct NativeFileWorkSnapshot {
    pub work: NativeFileWork,
    pub resolved_path: String,
    pub live: bool,
    pub placement: Option<(String, String)>,
    pub occupied_names: Vec<String>,
    pub expected_checksum: Option<String>,
    pub book_paths: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct NativeFileWorkCompletion {
    pub snapshot: NativeFileWorkSnapshot,
    pub final_path: String,
    pub renamed: Option<(String, String)>,
    pub published: bool,
    pub thumbnail_set_exists: bool,
}

#[derive(Clone, Debug)]
pub struct PurgeBookSnapshot {
    work_id: i64,
    pub content_hash: ContentHash,
    pub remove_bytes: bool,
}

impl Database {
    pub fn native_file_work(&self) -> Result<Vec<NativeFileWork>, DatabaseError> {
        let mut rows = Vec::new();
        self.connection.native_file_work_rows(|row| {
            rows.push(NativeFileWork { id: row.get(0)?, operation: row.get(1)?, content_hash: ContentHash::new(&row.get::<_, String>(2)?), original_path: row.get(3)? });
            Ok(())
        })?;
        Ok(rows)
    }

    pub fn native_file_work_snapshot(&self, work: NativeFileWork) -> Result<Option<NativeFileWorkSnapshot>, DatabaseError> {
        if !self.connection.native_file_is_current(work.id, work.operation.as_str(), work.content_hash.as_str(), work.original_path.as_str(), |row| row.get(0))? {
            return Ok(None);
        }
        let mut live_places = Vec::new();
        self.connection.native_live_placement_paths(work.content_hash.as_str(), |row| {
            live_places.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?));
            Ok(())
        })?;
        let mut projections = Vec::new();
        self.connection.native_file_projection_paths(work.content_hash.as_str(), |row| {
            projections.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
            Ok(())
        })?;
        // A hash can have many placements. Resolve this job's path, never an
        // arbitrary sibling. A remembered projection can identify a moved one.
        let remembered = projections.iter().find(|(_, path)| path == &work.original_path);
        let target = live_places.iter().find(|(_, _, path)| path == &work.original_path).or_else(|| remembered.and_then(|(id, _)| live_places.iter().find(|(dir, _, _)| dir == id)));
        let live = target.is_some();
        let resolved_path = target.map(|(_, _, path)| path.clone()).unwrap_or_else(|| work.original_path.clone());
        let placement = target.map(|(dir, name, _)| (dir.clone(), name.clone()));
        let mut occupied_names = Vec::new();
        if let Some((dir_id, _)) = placement.as_ref() {
            self.connection.native_occupied_file_names(dir_id.as_str(), |row| {
                occupied_names.push(row.get::<_, String>(0)?);
                Ok(())
            })?;
        }
        let mut expected_checksum = None;
        self.connection.native_current_checksum(work.content_hash.as_str(), resolved_path.as_str(), |row| {
            expected_checksum = row.get::<_, Option<String>>(0)?;
            Ok(())
        })?;
        Ok(Some(NativeFileWorkSnapshot { work, resolved_path, live, placement, occupied_names, expected_checksum, book_paths: projections.into_iter().map(|(_, relative)| relative).collect() }))
    }

    pub fn acknowledge_native_file_work(&self, completion: NativeFileWorkCompletion) -> Result<(), DatabaseError> {
        self.with_write_transaction(|transaction| {
            let snapshot = &completion.snapshot;
            let work = &snapshot.work;
            let hash = work.content_hash.as_str();
            let Some(current) = self.native_file_work_snapshot(work.clone())? else { return Ok(crate::transactions::WriteOutcome::Rollback(())) };
            if current.resolved_path != snapshot.resolved_path || current.placement != snapshot.placement {
                return Err(DatabaseError::message("file placement changed during reconciliation; retry"));
            }
            if completion.published {
                if work.operation == "trash" {
                    if current.live {
                        return Err(DatabaseError::message("trashed placement became live; retry"));
                    }
                    transaction.native_delete_file_projection(hash, &completion.final_path)?;
                } else if work.operation == "restore" {
                    let (dir_id, _) = snapshot.placement.as_ref().ok_or_else(|| DatabaseError::message("restored placement no longer exists"))?;
                    if let Some((renamed_dir, name)) = completion.renamed.as_ref() {
                        if renamed_dir != dir_id {
                            return Err(DatabaseError::message("restore changed its destination directory"));
                        }
                        let mut conflict = false;
                        transaction.native_book_name_conflict(dir_id, &library_replica::portable_name_key(name), hash, |row| {
                            conflict = row.get(0)?;
                            Ok(())
                        })?;
                        if conflict {
                            return Err(DatabaseError::message("restored filename is occupied; retry"));
                        }
                        transaction.native_rename_book(dir_id, hash, name)?;
                    }
                    transaction.shared_scanner_publish_projection(hash, dir_id, &completion.final_path)?;
                    transaction.native_mark_placement_downloaded(hash, dir_id)?;
                    transaction.shared_transfer_complete_download(hash)?;
                    if !completion.thumbnail_set_exists {
                        transaction.native_request_thumbnail(hash)?;
                    }
                }
            }
            transaction.native_delete_file_work(work.id)?;
            Ok(crate::transactions::WriteOutcome::Commit(()))
        })
    }

    pub fn requested_purge_books(&self, after: &str) -> Result<Vec<ContentHash>, DatabaseError> {
        let mut hashes = Vec::new();
        self.connection.requested_purge_books(after, |row| {
            hashes.push(ContentHash::new(&row.get::<_, String>(0)?));
            Ok(())
        })?;
        Ok(hashes)
    }

    pub fn purge_book_snapshot(&self, hash: &ContentHash) -> Result<Option<PurgeBookSnapshot>, DatabaseError> {
        let work_id: Option<i64> = self.connection.query_row("SELECT id FROM local_purge_work WHERE content_hash=?1", [hash.as_str()], |r| r.get(0)).optional()?;
        let Some(work_id) = work_id else { return Ok(None) };
        let retired = self.connection.purge_book_is_retired(hash.as_str(), |row| row.get::<_, bool>(0)).map_err(DatabaseError::operation)?;
        // Failed or interrupted placement cleanup must complete before deleting
        // the trash container; otherwise a retry would strand bytes there.
        if retired && self.connection.query_row("SELECT EXISTS(SELECT 1 FROM local_file_work WHERE content_hash=?1)", [hash.as_str()], |r| r.get::<_, bool>(0))? {
            return Ok(None);
        }
        Ok(Some(PurgeBookSnapshot { work_id, content_hash: *hash, remove_bytes: retired }))
    }

    pub fn acknowledge_purge_book(&self, snapshot: PurgeBookSnapshot) -> Result<(), DatabaseError> {
        self.connection.acknowledge_purge_book(snapshot.content_hash.as_str(), snapshot.work_id)?;
        Ok(())
    }

    pub fn has_pending_purge_work(&self) -> Result<bool, DatabaseError> {
        let mut pending = false;
        self.connection.native_has_pending_purge_work(|row| {
            pending = row.get(0)?;
            Ok(())
        })?;
        Ok(pending)
    }

    pub fn native_directory_work_ids(&self) -> Result<Vec<DirId>, DatabaseError> {
        let mut ids = Vec::new();
        self.connection.native_directory_work_ids(|row| {
            ids.push(DirId::parse_str(&row.get::<_, String>(0)?).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))?);
            Ok(())
        })?;
        Ok(ids)
    }

    /// Queue IDs are generations: requeuing the same resource replaces its ID.
    pub fn native_directory_replay_work(&self) -> Result<Vec<(i64, DirId)>, DatabaseError> {
        let mut work = Vec::new();
        self.connection.native_directory_replay_work(|row| {
            let dir = DirId::parse_str(&row.get::<_, String>(1)?).map_err(|error| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error)))?;
            work.push((row.get(0)?, dir));
            Ok(())
        })?;
        Ok(work)
    }

    pub fn native_book_replay_work_ids(&self) -> Result<Vec<i64>, DatabaseError> {
        let mut ids = Vec::new();
        self.connection.native_book_replay_work_ids(|row| {
            ids.push(row.get(0)?);
            Ok(())
        })?;
        Ok(ids)
    }

    pub fn complete_native_directory_replay(&self, ids: &[i64]) -> Result<(), DatabaseError> {
        self.connection.native_complete_directory_replay(&serde_json::to_string(ids).map_err(DatabaseError::operation)?)?;
        Ok(())
    }

    /// A skipped/leased file or newer directory work still needs another pass.
    pub fn complete_native_book_replay(&self, ids: &[i64]) -> Result<(), DatabaseError> {
        self.connection.native_complete_book_replay(&serde_json::to_string(ids).map_err(DatabaseError::operation)?)?;
        Ok(())
    }

    /// Seeds this hash's queued follow-up file reconciliation, as a
    /// completed download would through [`Database::complete_downloaded_book`].
    pub fn queue_book_work(&self, hash: &ContentHash) -> Result<(), DatabaseError> {
        self.connection.native_queue_book_work(hash.as_str())?;
        Ok(())
    }

    /// Content hashes with queued follow-up file reconciliation.
    pub fn local_book_work_hashes(&self) -> Result<Vec<ContentHash>, DatabaseError> {
        let mut hashes = Vec::new();
        self.connection.local_book_work_hashes(|row| {
            hashes.push(ContentHash::new(&row.get::<_, String>(0)?));
            Ok(())
        })?;
        Ok(hashes)
    }

    /// Seeds a book's remembered physical projection path directly, without
    /// going through the snapshot/acknowledge flow.
    pub fn seed_file_projection(&self, hash: &ContentHash, dir_id: &DirId, relative_path: &str) {
        self.connection.shared_scanner_publish_projection(hash.as_str(), &dir_id.to_string(), relative_path).expect("seed file projection");
    }

    /// Drops every queued directory reconciliation without performing it.
    pub fn clear_local_directory_work(&self) {
        self.connection.clear_local_directory_work().expect("clear local directory work");
    }

    pub fn native_book_work_directory_ids(&self) -> Result<Vec<DirId>, DatabaseError> {
        let mut ids = Vec::new();
        self.connection.native_book_work_directory_ids(|row| {
            ids.push(DirId::parse_str(&row.get::<_, String>(0)?).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))?);
            Ok(())
        })?;
        Ok(ids)
    }

    /// The library-relative path a book named `name` would occupy in `parent`,
    /// or `None` when `parent` is not a live directory. Callers combine this
    /// with their own library root to reach a filesystem path; this crate
    /// never resolves or touches the filesystem itself.
    pub fn native_book_path(&self, parent: DirId, name: &str) -> Result<Option<String>, DatabaseError> {
        let Some(dir_path) = self.directory_relative_path_string(&parent)? else { return Ok(None) };
        Ok(Some(if dir_path.is_empty() { name.to_owned() } else { format!("{dir_path}/{name}") }))
    }
}

// ---- directory ----
// Native directory snapshots and conditional materialization commits.

#[derive(Clone, Debug)]
pub struct NativeDirectorySnapshot {
    pub id: DirId,
    pub relative_path: String,
    pub projected_path: Option<String>,
    pub name: String,
    pub sibling_names: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct NativeDirectoryCompletion {
    pub snapshot: NativeDirectorySnapshot,
    pub renamed_to: Option<String>,
    pub materialized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeDirectoryAcknowledgement {
    Applied,
    Obsolete,
    Retry,
}

impl Database {
    pub fn native_directory_snapshot(&self, id: &DirId) -> Result<Option<NativeDirectorySnapshot>, DatabaseError> {
        let snapshot = self.connection.is_autocommit().then(|| self.connection.unchecked_transaction()).transpose()?;
        let Some((relative_path, projected_path, parent_id, name)) = native_directory_record(&self.connection, id)? else { return Ok(None) };
        let mut sibling_names = Vec::new();
        self.connection.native_directory_sibling_names(parent_id.as_str(), |sibling| {
            sibling_names.push(sibling.get::<_, String>(0)?);
            Ok(())
        })?;
        if let Some(snapshot) = snapshot {
            snapshot.commit()?;
        }
        Ok(Some(NativeDirectorySnapshot { id: *id, relative_path, projected_path, name, sibling_names }))
    }

    /// Physical locations remain recoverable after a directory is hidden or trashed.
    pub fn native_directory_projected_path(&self, id: &DirId) -> Result<Option<String>, DatabaseError> {
        use rusqlite::OptionalExtension;
        Ok(self.connection.query_row("SELECT '/' || ltrim(relative_path,'/') FROM local_directory_projection WHERE dir_id=?1", [id.to_string()], |row| row.get(0)).optional()?)
    }

    /// Remember an intermediate physical location independently of logical lifecycle.
    pub fn acknowledge_native_directory_staging(&self, id: &DirId, previous: &str, staged_path: &str) -> Result<(), DatabaseError> {
        self.with_write_transaction(|transaction| {
            if self.native_directory_projected_path(id)?.as_deref() != Some(previous) {
                return Err(DatabaseError::message("directory physical location changed during staging"));
            }
            let prefix = format!("{}/", previous.trim_end_matches('/'));
            let replacement = format!("{staged_path}/");
            let upper = format!("{prefix}\u{10FFFF}");
            transaction.native_move_projection_paths(&prefix, &replacement, &upper)?;
            transaction.native_move_directory_projection_paths(&prefix, &replacement, &upper)?;
            transaction.native_move_file_work_paths(&prefix, &replacement, &upper)?;
            transaction.native_set_directory_projection(&id.to_string(), staged_path)?;
            Ok(crate::transactions::WriteOutcome::Commit(()))
        })
    }

    pub fn acknowledge_native_directory(&self, completion: NativeDirectoryCompletion) -> Result<NativeDirectoryAcknowledgement, DatabaseError> {
        self.with_write_transaction(|transaction| {
            let snapshot = &completion.snapshot;
            let Some((relative_path, projected_path, _, name)) = native_directory_record(transaction, &snapshot.id)? else {
                return Ok(crate::transactions::WriteOutcome::Rollback(NativeDirectoryAcknowledgement::Obsolete));
            };
            if relative_path != snapshot.relative_path || projected_path != snapshot.projected_path || name != snapshot.name {
                return Ok(crate::transactions::WriteOutcome::Rollback(NativeDirectoryAcknowledgement::Retry));
            }
            let final_name = completion.renamed_to.as_deref().unwrap_or(snapshot.name.as_str());
            if let Some(renamed_to) = completion.renamed_to.as_deref() {
                if transaction.native_directory_rename_conflict(snapshot.id.to_string().as_str(), renamed_to, |row| row.get(0))? {
                    return Ok(crate::transactions::WriteOutcome::Rollback(NativeDirectoryAcknowledgement::Retry));
                }
                transaction.native_directory_rename(snapshot.id.to_string().as_str(), renamed_to)?;
                transaction.native_directory_queue_work(snapshot.id.to_string().as_str())?;
            }
            let final_path = final_relative(snapshot.relative_path.as_str(), final_name);
            if completion.materialized {
                let previous = snapshot.projected_path.as_deref().unwrap_or(&snapshot.relative_path);
                if previous != final_path {
                    // Include the separator so /Shelf cannot match /Shelfish.
                    let prefix = format!("{}/", previous.trim_end_matches('/'));
                    let replacement = format!("{final_path}/");
                    let upper = format!("{prefix}\u{10FFFF}");
                    transaction.native_move_projection_paths(&prefix, &replacement, &upper)?;
                    transaction.native_move_directory_projection_paths(&prefix, &replacement, &upper)?;
                    transaction.native_move_file_work_paths(&prefix, &replacement, &upper)?;
                }
                transaction.native_set_directory_projection(snapshot.id.to_string().as_str(), final_path.as_str())?;
            }
            Ok(crate::transactions::WriteOutcome::Commit(NativeDirectoryAcknowledgement::Applied))
        })
    }
}

fn native_directory_record(connection: &rusqlite::Connection, id: &DirId) -> Result<Option<(String, Option<String>, String, String)>, DatabaseError> {
    let Some(relative_path) = crate::placements::directory_relative_path(connection, id)? else { return Ok(None) };
    let mut record = None;
    connection.native_directory_snapshot(id.to_string().as_str(), |row| {
        record = Some((format!("/{relative_path}"), row.get(0)?, row.get(1)?, row.get(2)?));
        Ok(())
    })?;
    Ok(record)
}

fn final_relative(relative_path: &str, name: &str) -> String {
    match relative_path.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => format!("{parent}/{name}"),
        _ => format!("/{name}"),
    }
}
