//! The stored collection: placements, discovery, physical paths.

use crate::shared_sql::SharedSql;
// ---- placements ----
// Directory and physical book-placement persistence: moves, copies, and the
// Trash lifecycle (trash/restore/purge) for both books and folders.

use crate::{Database, DatabaseError};
use include_sqlite_sql::include_sql;
use library_model::{Dir, LibraryFolderDestination, LibraryTrashView, TrashedBook, TrashedFolder};
use library_replica::{BlobKind, BookPlacement, DirectoryLifecycleState, MutationBody, TransferOrigin};
use rusqlite::OptionalExtension;
use std::collections::HashSet;
use sync_common::{ContentHash, DirId, FileName};

fn unix_millis() -> Result<i64, DatabaseError> {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|duration| duration.as_millis() as i64).map_err(DatabaseError::operation)
}

fn parse_dir_id(raw: String) -> Result<DirId, DatabaseError> {
    DirId::parse_str(&raw).map_err(DatabaseError::operation)
}

/// Read command preconditions from canonical registers, never placeholder defaults.
#[derive(Default)]
struct CanonicalDirectory {
    name: Option<FileName>,
    parent: Option<DirId>,
    lifecycle: Option<DirectoryLifecycleState>,
}

fn canonical_directory(connection: &rusqlite::Connection, id: &DirId) -> Result<CanonicalDirectory, DatabaseError> {
    let mut state = CanonicalDirectory::default();
    let mut statement = connection.prepare("SELECT body FROM sync_state_version WHERE state_key=?1 AND state_subkey='' AND state_kind IN ('directory_name','directory_parent','directory_lifecycle') AND body IS NOT NULL")?;
    for row in statement.query_map([id.to_string()], |row| row.get::<_, Vec<u8>>(0))? {
        match sync_common::wire::decode::<MutationBody>(&row?, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? {
            MutationBody::DirectoryName { name, .. } => state.name = Some(name),
            MutationBody::DirectoryParent { parent_id, .. } => state.parent = Some(parent_id),
            MutationBody::DirectoryLifecycle { value, .. } => state.lifecycle = Some(value),
            _ => return Err(DatabaseError::message("invalid canonical directory state")),
        }
    }
    Ok(state)
}

fn book_is_trashed(connection: &rusqlite::Connection, content_hash: &ContentHash) -> Result<bool, DatabaseError> {
    let mut trashed = false;
    connection.placements_book_is_trashed(content_hash.as_str(), |row| {
        trashed = row.get(0)?;
        Ok(())
    })?;
    Ok(trashed)
}

fn recoverable_placements(connection: &rusqlite::Connection, content_hash: &ContentHash) -> Result<Vec<(String, String)>, DatabaseError> {
    let mut placements = Vec::new();
    connection.restore_book_record_with_parent_select_2(content_hash.as_str(), |row| {
        placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
        Ok(())
    })?;
    Ok(placements)
}

fn directory_is_live(connection: &rusqlite::Connection, dir_id: &DirId) -> Result<bool, DatabaseError> {
    let mut directory: Option<(String, bool)> = None;
    connection.restore_book_record_with_parent_select_4(&dir_id.to_string(), |row| {
        directory = Some((row.get(0)?, row.get(1)?));
        Ok(())
    })?;
    Ok(directory.is_some_and(|(_, live)| live))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirPathComponent {
    pub id: DirId,
    pub name: FileName,
}

/// One step of a book restore: a live flag and relative path snapshot for
/// every directory the walk would visit, starting at the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreAncestor {
    pub id: DirId,
    pub path: String,
    pub live: bool,
}

/// Snapshot in for a book restore. The caller walks each ancestry with
/// its own filesystem knowledge and commits the chosen targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlacement {
    pub original: DirId,
    pub file_name: String,
    pub ancestry: Vec<RestoreAncestor>,
}

fn dir_path_components(connection: &rusqlite::Connection, dir_id: &str) -> Result<Vec<DirPathComponent>, DatabaseError> {
    // Reuse an existing transaction, or hold a read snapshot for the whole walk.
    let snapshot = connection.is_autocommit().then(|| connection.unchecked_transaction()).transpose()?;
    let mut current = dir_id.to_owned();
    let mut seen = HashSet::new();
    let mut components = Vec::new();
    let root = sync_common::ROOT_DIR_ID.to_string();
    loop {
        if !seen.insert(current.clone()) {
            return Err(DatabaseError::message("cycle in directory ancestry"));
        }
        let mut row: Option<(String, String)> = None;
        connection.live_directory_location(&current, |found| {
            row = Some((found.get(0)?, found.get(1)?));
            Ok(())
        })?;
        let Some((parent, name)) = row else {
            components.clear();
            break;
        };
        if current == root {
            break;
        }
        components.push(DirPathComponent { id: parse_dir_id(current)?, name });
        current = parent;
    }
    components.reverse();
    if let Some(snapshot) = snapshot {
        snapshot.commit()?;
    }
    Ok(components)
}

fn directory_exists_in(connection: &rusqlite::Connection, directory_id: &str) -> Result<bool, DatabaseError> {
    let mut exists = false;
    connection.placements_live_directory_exists(directory_id, |row| {
        exists = row.get(0)?;
        Ok(())
    })?;
    Ok(exists)
}

/// Live database names reserved in a destination. Physical collisions are
/// checked separately by the file store.
fn occupied_file_names_in(connection: &rusqlite::Connection, destination: &str) -> Result<Vec<String>, DatabaseError> {
    let mut names = Vec::new();
    connection.shared_occupied_file_names(destination, |row| {
        names.push(row.get::<_, String>(0)?);
        Ok(())
    })?;
    Ok(names)
}

/// The filename of a live placement, or `None` when it is absent.
fn live_file_name(connection: &rusqlite::Connection, directory: &DirId, hash: &ContentHash) -> Result<Option<String>, DatabaseError> {
    let mut name = None;
    connection.placements_live_file_name(&directory.to_string(), hash.as_str(), |row| {
        name = Some(row.get(0)?);
        Ok(())
    })?;
    Ok(name)
}

fn choose_move_folder_name(connection: &rusqlite::Connection, parent: &str, excluded: &str, requested: &str) -> Result<String, DatabaseError> {
    for number in 1..=8 {
        let candidate = if number == 1 { library_replica::project_folder_name(requested) } else { library_replica::numbered_folder_name(requested, number) };
        let mut occupied = false;
        connection.placements_folder_name_occupied(parent, &library_replica::portable_name_key(&candidate), excluded, |row| {
            occupied = row.get(0)?;
            Ok(())
        })?;
        if !occupied {
            return Ok(candidate);
        }
    }
    let mut names = Vec::new();
    connection.placements_sibling_folder_names(parent, excluded, |row| {
        names.push(row.get::<_, String>(0)?);
        Ok(())
    })?;
    Ok(library_replica::unique_folder_name(requested, names.iter().map(String::as_str)))
}

fn choose_move_file_name(connection: &rusqlite::Connection, destination: &str, requested: &str) -> Result<String, DatabaseError> {
    for number in 1..=8 {
        let candidate = if number == 1 { requested.to_owned() } else { library_replica::numbered_file_name(requested, number) };
        let mut occupied = false;
        connection.placements_file_name_occupied(destination, &library_replica::portable_name_key(&candidate), |row| {
            occupied = row.get(0)?;
            Ok(())
        })?;
        if !occupied {
            return Ok(candidate);
        }
    }
    let names = occupied_file_names_in(connection, destination)?;
    Ok(library_replica::unique_file_name(requested, names.iter().map(String::as_str)))
}

/// Records this directory into `dir`, validating it is neither the root nor
/// its own ancestor.
fn upsert_dir(transaction: &rusqlite::Transaction<'_>, dir_id: &DirId, parent_id: &DirId, name: &str) -> Result<(), DatabaseError> {
    if *dir_id == sync_common::ROOT_DIR_ID {
        return Err(DatabaseError::message("the root directory is reserved"));
    }
    if dir_id == parent_id {
        return Err(DatabaseError::message("a directory cannot be its own parent"));
    }
    let mut closes_cycle = false;
    transaction.placements_directory_move_closes_cycle(&parent_id.to_string(), &dir_id.to_string(), |row| {
        closes_cycle = row.get(0)?;
        Ok(())
    })?;
    if closes_cycle {
        return Err(DatabaseError::message("directory move would create a cycle"));
    }
    transaction.placements_upsert_dir(&dir_id.to_string(), &parent_id.to_string(), name)?;
    Ok(())
}

/// The relative path a live directory would materialize at, or `None` when
/// it is not reachable from the library root.
pub(crate) fn directory_relative_path(connection: &rusqlite::Connection, dir_id: &DirId) -> Result<Option<String>, DatabaseError> {
    if *dir_id == sync_common::ROOT_DIR_ID {
        return Ok(Some(String::new()));
    }
    let components = dir_path_components(connection, &dir_id.to_string())?;
    if components.is_empty() {
        return Ok(None);
    }
    Ok(Some(components.iter().map(|component| component.name.as_str()).collect::<Vec<_>>().join("/")))
}

/// Publishes a directory's queued physical projection to `local_directory_work`.
fn queue_directory_work(transaction: &rusqlite::Transaction<'_>, dir: &DirId) -> Result<(), DatabaseError> {
    #[cfg(target_arch = "wasm32")]
    let _ = (transaction, dir);
    #[cfg(not(target_arch = "wasm32"))]
    transaction.shared_placements_queue_directory_work(&dir.to_string())?;
    Ok(())
}

/// Publishes a book's queued local file operation. A no-op on wasm, which has
/// no separate physical file store to reconcile.
fn queue_file_work(transaction: &rusqlite::Transaction<'_>, operation: &str, hash: &ContentHash, path: &str) -> Result<(), DatabaseError> {
    #[cfg(target_arch = "wasm32")]
    let _ = (transaction, operation, hash, path);
    #[cfg(not(target_arch = "wasm32"))]
    transaction.shared_placements_queue_file_work(operation, hash.as_str(), path)?;
    Ok(())
}

fn placement_path(connection: &rusqlite::Connection, dir: &DirId, name: &str) -> Result<String, DatabaseError> {
    let parent = directory_relative_path(connection, dir)?.ok_or_else(|| DatabaseError::message("placement directory has no path"))?;
    Ok(if parent.is_empty() { format!("/{name}") } else { format!("/{parent}/{name}") })
}

/// Preserves upload provenance across a metadata-only copy/move. No upload work.
fn copy_book_baseline(transaction: &rusqlite::Transaction<'_>, source: &DirId, destination: &DirId, hash: &ContentHash) -> Result<(), DatabaseError> {
    transaction.placements_copy_book_baseline(&source.to_string(), &destination.to_string(), hash.as_str())?;
    Ok(())
}

/// Records a user-initiated download request for a restored book.
fn add_download_request(transaction: &rusqlite::Transaction<'_>, hash: &ContentHash) -> Result<(), DatabaseError> {
    transaction.placements_add_download_request(BlobKind::Book.storage(), hash.as_str(), TransferOrigin::UserInitiated.storage(), unix_millis()?)?;
    Ok(())
}

impl Database {
    pub fn directory_exists(&self, directory_id: &DirId) -> Result<bool, DatabaseError> {
        directory_exists_in(&self.connection, &directory_id.to_string())
    }

    pub fn dir_path_components(&self, directory_id: &DirId) -> Result<Vec<DirPathComponent>, DatabaseError> {
        dir_path_components(&self.connection, &directory_id.to_string())
    }

    /// A destination is usable only when its complete ancestry is live and rooted.
    pub fn is_live_destination(&self, directory: &DirId) -> Result<bool, DatabaseError> {
        let mut live = false;
        self.connection.placements_is_live_destination(&directory.to_string(), |row| {
            live = row.get(0)?;
            Ok(())
        })?;
        Ok(live)
    }

    pub fn shared_set_book_hash_downloaded(&self, content_hash: &ContentHash, downloaded: bool) -> Result<(), DatabaseError> {
        self.connection.shared_set_book_hash_downloaded(content_hash.as_str(), i32::from(downloaded))?;
        Ok(())
    }

    /// Relative path of one directory, if it is still rooted.
    pub fn directory_relative_path_string(&self, dir_id: &DirId) -> Result<Option<String>, DatabaseError> {
        directory_relative_path(&self.connection, dir_id)
    }

    /// Whether a placement still addresses these bytes through live
    /// directories. A move, rename, or deletion that landed mid-transfer
    /// makes the finished bytes belong to a stale path.
    pub fn book_placement_expects_hash(&self, placement: &BookPlacement, content_hash: &ContentHash) -> Result<bool, DatabaseError> {
        let Some((stored_hash, dir_id, file_name)) = self.book_dir_entry_by_path(placement.rel_path.as_str())? else {
            return Ok(false);
        };
        if stored_hash != placement.content_hash || stored_hash != *content_hash {
            return Ok(false);
        }
        let mut exists = false;
        self.connection
            .placement_expects_hash(&dir_id, content_hash.as_str(), &file_name, |row| {
                exists = row.get(0)?;
                Ok(())
            })
            .map_err(DatabaseError::operation)?;
        Ok(exists)
    }

    fn book_dir_entry_by_path(&self, rel_path: &str) -> Result<Option<(ContentHash, String, String)>, DatabaseError> {
        let mut found = None;
        self.connection
            .dir_entry_by_path(rel_path, |row| {
                found = Some((ContentHash::new(&row.get::<_, String>(0)?), row.get::<_, String>(1)?, row.get::<_, String>(2)?));
                Ok(())
            })
            .map_err(DatabaseError::operation)?;
        Ok(found)
    }

    /// Seeds one book placement with explicit physical state.
    pub fn add_book_placement(&self, dir: &DirId, hash: &ContentHash, file_name: &str, local_hash: &str, downloaded: bool) {
        self.connection.add_book_placement(&dir.to_string(), hash.as_str(), file_name, local_hash, downloaded).expect("seed book placement");
    }

    /// Seeds one book row with explicit identity columns.
    pub fn seed_book(&self, hash: &ContentHash, title: Option<&str>, added_at: i64, format: &str) {
        self.connection.seed_book(hash.as_str(), title, added_at, format).expect("seed book");
    }

    /// Seeds one directory row with an explicit parent and name.
    pub fn seed_dir(&self, dir_id: &DirId, parent_id: &DirId, name: &str) {
        self.connection.seed_dir(&dir_id.to_string(), &parent_id.to_string(), name).expect("seed directory");
    }

    /// Seeds one queued local file operation directly, as a mutation that
    /// changes a book's live placement would through [`queue_file_work`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn seed_file_work(&self, operation: &str, hash: &ContentHash, path: &str) {
        self.connection.shared_placements_queue_file_work(operation, hash.as_str(), path).expect("seed file work");
    }

    /// Seeds a directory's queued physical projection, as a real remote
    /// folder mutation would through the sync reducer. Fixtures that insert
    /// a directory directly (bypassing the reducer) call this to reproduce
    /// the same durable work queue state.
    pub fn queue_directory_work(&self, dir: &DirId) -> Result<(), DatabaseError> {
        self.connection.shared_placements_queue_directory_work(&dir.to_string())?;
        Ok(())
    }

    /// Seeds one shelf directory with its synchronized register intent.
    pub fn seed_shelf_dir(&self, dir_id: &DirId) {
        self.connection.seed_shelf_dir(&dir_id.to_string()).expect("seed shelf directory");
    }

    /// Marks every placement deleted.
    pub fn mark_all_placements_deleted(&self) {
        self.connection.mark_all_placements_deleted().expect("mark placements deleted");
    }

    /// Renames one directory.
    pub fn rename_dir(&self, id: &DirId, name: &str) {
        self.connection.rename_dir(name, &id.to_string()).expect("rename directory");
    }

    /// Retouches every placement fingerprint, as a scan during upload would.
    pub fn touch_all_placement_hashes(&self) {
        self.connection.touch_all_placement_hashes().expect("touch placement hashes");
    }

    /// Stales every placement: deleted and no longer downloaded.
    pub fn stale_all_placements(&self) {
        self.connection.stale_all_placements().expect("stale placements");
    }

    /// Relative path of one live directory.
    pub fn dir_path_string(&self, id: &DirId) -> String {
        self.directory_relative_path_string(id).expect("read directory path").expect("directory has a live path")
    }

    /// Display title of one book.
    pub fn book_title(&self, hash: &ContentHash) -> String {
        {
            let mut title = None;
            self.connection
                .book_title(hash.as_str(), |row| {
                    title = Some(row.get::<_, String>(0)?);
                    Ok(())
                })
                .expect("read book title");
            title.expect("read book title")
        }
    }

    /// Number of book rows with one content hash.
    pub fn book_count(&self, hash: &ContentHash) -> i64 {
        {
            let mut count = None;
            self.connection
                .book_count(hash.as_str(), |row| {
                    count = Some(row.get::<_, i64>(0)?);
                    Ok(())
                })
                .expect("count books");
            count.expect("count books")
        }
    }

    /// Number of book rows with one title.
    pub fn book_count_with_title(&self, title: &str) -> i64 {
        {
            let mut count = None;
            self.connection
                .book_count_with_title(title, |row| {
                    count = Some(row.get::<_, i64>(0)?);
                    Ok(())
                })
                .expect("count books by title");
            count.expect("count books by title")
        }
    }

    /// Number of book rows matching any of these hashes.
    pub fn count_books(&self, hashes: &[ContentHash]) -> i64 {
        if hashes.is_empty() {
            return 0;
        }
        let placeholders = hashes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT COUNT(*) FROM book WHERE content_hash IN ({placeholders})");
        let params: Vec<&str> = hashes.iter().map(ContentHash::as_str).collect();
        self.connection.query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0)).expect("count books by hashes")
    }

    /// Total downloaded placement flags; empty libraries read as zero.
    pub fn downloaded_placement_total(&self) -> i64 {
        {
            let mut total = None;
            self.connection
                .downloaded_placement_total(|row| {
                    total = row.get::<_, Option<i64>>(0)?;
                    Ok(())
                })
                .expect("sum downloaded placements");
            total.unwrap_or(0)
        }
    }

    /// Name of one directory.
    pub fn dir_name(&self, id: &DirId) -> String {
        {
            let mut name = None;
            self.connection
                .dir_name(&id.to_string(), |row| {
                    name = Some(row.get::<_, String>(0)?);
                    Ok(())
                })
                .expect("read directory name");
            name.expect("read directory name")
        }
    }

    /// Creates and validates a synchronized directory with a fresh id in one
    /// transaction.
    pub fn create_directory(&self, parent_id: &DirId, name: &FileName) -> Result<Dir, DatabaseError> {
        self.create_directory_with_id(&DirId::new_v4(), parent_id, name)
    }

    /// Creates and validates a synchronized directory in one transaction.
    /// An import job may retry the same directory id after losing its worker.
    pub fn create_directory_with_id(&self, dir_id: &DirId, parent_id: &DirId, name: &FileName) -> Result<Dir, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if *dir_id == sync_common::ROOT_DIR_ID {
            return Err(DatabaseError::message("the library root cannot be created"));
        }
        let state = canonical_directory(&transaction, dir_id)?;
        if state.name.as_ref().is_some_and(|existing| existing != name) || state.parent.is_some_and(|existing| existing != *parent_id) || state.lifecycle.is_some_and(|existing| existing != DirectoryLifecycleState::Present) {
            return Err(DatabaseError::message("import directory was changed or deleted"));
        }
        if state.name.is_none() || state.parent.is_none() || state.lifecycle.is_none() {
            if !directory_exists_in(&transaction, &parent_id.to_string())? {
                return Err(DatabaseError::message(format!("parent directory does not exist: {parent_id}")));
            }
            let exists: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM dir WHERE id=?1)", [dir_id.to_string()], |row| row.get(0))?;
            if !exists {
                // Stage a free display row so an ordinary empty-folder creation
                // need not invalidate unrelated cached memberships. Projection
                // still assigns the final name from canonical intent.
                let provisional_name = choose_move_folder_name(&transaction, &parent_id.to_string(), &dir_id.to_string(), name.as_str())?;
                transaction.create_directory_intent(&dir_id.to_string(), &parent_id.to_string(), name.as_str(), &provisional_name)?;
                queue_directory_work(&transaction, dir_id)?;
            } else {
                let time = unix_millis()? as u64;
                let missing = [
                    state.name.is_none().then(|| MutationBody::DirectoryName { dir_id: *dir_id, name: name.clone() }),
                    state.parent.is_none().then_some(MutationBody::DirectoryParent { dir_id: *dir_id, parent_id: *parent_id }),
                    state.lifecycle.is_none().then_some(MutationBody::DirectoryLifecycle { dir_id: *dir_id, value: DirectoryLifecycleState::Present }),
                ];
                for body in missing.into_iter().flatten() {
                    crate::sync::apply::registers::accept_value(&transaction, &body, time, None)?;
                }
            }
        }
        crate::sync::apply::project_dirty(&transaction)?;
        directory_relative_path(&transaction, dir_id)?.ok_or_else(|| DatabaseError::message("created directory has no path"))?;
        let projected_name = transaction.query_row("SELECT name FROM dir WHERE id=?1", [dir_id.to_string()], |row| row.get::<_, String>(0))?;
        transaction.commit()?;
        Ok(Dir::new(*dir_id, projected_name, 0, 0))
    }

    /// Plan a new local folder without writing an intent to SQLite.
    pub fn directory_creation_path(&self, parent_id: &DirId, name: &str) -> Result<(String, String), DatabaseError> {
        if !directory_exists_in(&self.connection, &parent_id.to_string())? {
            return Err(DatabaseError::message("parent directory does not exist"));
        }
        let parent = directory_relative_path(&self.connection, parent_id)?.ok_or_else(|| DatabaseError::message("parent directory has no path"))?;
        let chosen = choose_move_folder_name(&self.connection, &parent_id.to_string(), "", name)?;
        let path = if parent.is_empty() { chosen.clone() } else { format!("{parent}/{chosen}") };
        Ok((chosen, path))
    }

    /// Register a folder already created on disk, including its projection.
    /// A crash before this commit is recovered by filesystem discovery.
    pub fn register_created_directory(&self, id: &DirId, parent_id: &DirId, name: &str, path: &str) -> Result<Dir, DatabaseError> {
        use crate::native::NativeFileWorkSql;
        let transaction = self.connection.unchecked_transaction()?;
        if !directory_exists_in(&transaction, &parent_id.to_string())? {
            return Err(DatabaseError::message("parent directory does not exist"));
        }
        let parent = directory_relative_path(&transaction, parent_id)?.ok_or_else(|| DatabaseError::message("parent directory has no path"))?;
        let expected = if parent.is_empty() { name.to_owned() } else { format!("{parent}/{name}") };
        if path != expected || choose_move_folder_name(&transaction, &parent_id.to_string(), &id.to_string(), name)? != name {
            return Err(DatabaseError::message("folder destination changed during creation"));
        }
        upsert_dir(&transaction, id, parent_id, name)?;
        transaction.native_set_directory_projection(&id.to_string(), path)?;
        crate::sync::apply::commit(transaction)?;
        Ok(Dir::new(*id, name.to_owned(), 0, 0))
    }

    pub fn folder_destinations(&self) -> Result<Vec<LibraryFolderDestination>, DatabaseError> {
        let mut raw = Vec::new();
        self.connection.placements_folder_destinations(|row| {
            raw.push((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?));
            Ok(())
        })?;
        raw.into_iter()
            .map(|(id, parent_id, label, path)| Ok(LibraryFolderDestination { id: parse_dir_id(id)?, parent_id: parent_id.map(parse_dir_id).transpose()?, label, path: if path.is_empty() { "Library".to_owned() } else { path } }))
            .collect()
    }

    pub fn move_directory(&self, directory_id: &DirId, requested_parent: Option<&DirId>, requested_name: Option<&str>) -> Result<String, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if *directory_id == sync_common::ROOT_DIR_ID {
            return Err(DatabaseError::message("the library root cannot be renamed or moved"));
        }
        let mut existing: Option<(String, String)> = None;
        transaction.live_directory_location(&directory_id.to_string(), |row| {
            existing = Some((row.get(0)?, row.get(1)?));
            Ok(())
        })?;
        let (current_parent, current_name) = existing.ok_or_else(|| DatabaseError::message("folder does not exist or is in Trash"))?;
        let current_parent = parse_dir_id(current_parent)?;
        let parent_id = requested_parent.copied().unwrap_or(current_parent);
        if !directory_exists_in(&transaction, &parent_id.to_string())? {
            return Err(DatabaseError::message("destination folder does not exist"));
        }
        let requested = requested_name.unwrap_or(&current_name);
        let chosen = choose_move_folder_name(&transaction, &parent_id.to_string(), &directory_id.to_string(), requested)?;
        let (intent_parent, intent_name): (String, String) =
            transaction.query_row("SELECT COALESCE(intent_parent_id,parent_id),COALESCE(intent_name,name) FROM dir WHERE id=?1", [directory_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let parent_changed = requested_parent.is_some() && intent_parent != parent_id.to_string();
        let name_changed = (requested_name.is_some() || chosen != current_name) && intent_name != chosen;
        if parent_changed || name_changed || parent_id != current_parent || chosen != current_name {
            let mut closes_cycle = false;
            transaction.placements_directory_move_closes_cycle(&parent_id.to_string(), &directory_id.to_string(), |row| {
                closes_cycle = row.get(0)?;
                Ok(())
            })?;
            if closes_cycle {
                return Err(DatabaseError::message("directory move would create a cycle"));
            }
            // A rename must not adopt a repaired parent; a move must not adopt
            // a numbered display name unless it needs a new name at its destination.
            transaction.execute(
                "UPDATE dir SET parent_id=?2,name=?3,intent_parent_id=?4,intent_name=?5 WHERE id=?1",
                rusqlite::params![
                    directory_id.to_string(),
                    parent_id.to_string(),
                    chosen,
                    if requested_parent.is_some() { parent_id.to_string() } else { intent_parent },
                    if requested_name.is_some() || chosen != current_name { chosen.clone() } else { intent_name }
                ],
            )?;
            crate::transactions::with_remote_origin(&transaction, |origin| {
                origin.mark(&transaction)?;
                crate::sync::apply::rebuild_remote_directories(&transaction)
            })?;
            queue_directory_work(&transaction, directory_id)?;
        }
        crate::sync::apply::project_dirty(&transaction)?;
        // Moving can remove a collision suffix while retaining the name intent.
        let projected_name = transaction.query_row("SELECT name FROM dir WHERE id=?1", [directory_id.to_string()], |row| row.get::<_, String>(0))?;
        transaction.commit()?;
        Ok(projected_name)
    }

    /// Duplicates a folder and everything under it beneath `destination_parent_id`,
    /// returning the copy's own directory id — what an undo needs to find what
    /// this call created.
    ///
    /// Books are content-addressed, so a copied book is a second placement of
    /// the same asset rather than a second copy of its bytes.
    pub fn copy_directory(&self, directory_id: &DirId, destination_parent_id: &DirId) -> Result<DirId, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if *directory_id == sync_common::ROOT_DIR_ID {
            return Err(DatabaseError::message("the library root cannot be copied"));
        }
        let mut name: Option<String> = None;
        transaction.copy_directory_record_select(&directory_id.to_string(), |row| {
            name = row.get(0)?;
            Ok(())
        })?;
        let name = name.ok_or_else(|| DatabaseError::message("folder does not exist or is in Trash"))?;
        if !directory_exists_in(&transaction, &destination_parent_id.to_string())? {
            return Err(DatabaseError::message("destination folder does not exist"));
        }
        // The subtree is read as the copy is written, so a destination inside
        // it would be copying a folder into itself; the same query that
        // rejects a cycle-forming move answers this.
        let mut inside_source = false;
        transaction.placements_directory_move_closes_cycle(&destination_parent_id.to_string(), &directory_id.to_string(), |row| {
            inside_source = row.get(0)?;
            Ok(())
        })?;
        if inside_source {
            return Err(DatabaseError::message("a folder cannot be copied into itself"));
        }
        let mut occupied = Vec::new();
        transaction.copy_directory_record_select_2(&destination_parent_id.to_string(), |row| {
            occupied.push(row.get::<_, String>(0)?);
            Ok(())
        })?;
        let chosen = library_replica::unique_folder_name(&name, occupied.iter().map(String::as_str));

        let copy_id = DirId::new_v4();
        let mut pending = vec![(*directory_id, copy_id, chosen, *destination_parent_id)];
        let mut visited = HashSet::new();
        while let Some((source_id, copy_id, copy_name, copy_parent)) = pending.pop() {
            if !visited.insert(source_id) {
                continue;
            }
            upsert_dir(&transaction, &copy_id, &copy_parent, &copy_name)?;
            queue_directory_work(&transaction, &copy_id)?;
            let mut placements = Vec::new();
            transaction.copy_directory_record_select_3(&source_id.to_string(), |row| {
                placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?));
                Ok(())
            })?;
            for (hash, file_name, downloaded) in placements {
                // Names are already unique within the source folder and the
                // copy is empty, so each placement keeps the name it had.
                transaction.copy_directory_record_insert(&copy_id.to_string(), hash.as_str(), &file_name, i32::from(downloaded))?;
                copy_book_baseline(&transaction, &source_id, &copy_id, &ContentHash::new(&hash))?;
                #[cfg(not(target_arch = "wasm32"))]
                queue_file_work(&transaction, "restore", &ContentHash::new(&hash), &placement_path(&transaction, &copy_id, &file_name)?)?;
            }
            let mut children = Vec::new();
            transaction.copy_directory_record_select_4(&source_id.to_string(), |row| {
                children.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
                Ok(())
            })?;
            for (child_id, child_name) in children {
                pending.push((parse_dir_id(child_id)?, DirId::new_v4(), child_name, copy_id));
            }
        }
        crate::sync::apply::commit(transaction)?;
        Ok(copy_id)
    }

    /// Puts a book back in a folder it was removed from, reviving the book
    /// itself if that removal was what sent it to Trash. The tombstoned
    /// placement is reused where it still exists, so the book returns under
    /// the file name it had rather than a fresh unique one.
    pub fn restore_book_placement(&self, content_hash: &ContentHash, directory_id: &DirId) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if !directory_exists_in(&transaction, &directory_id.to_string())? {
            return Err(DatabaseError::message("folder does not exist or is in Trash"));
        }
        let mut previous: Option<String> = None;
        transaction.restore_book_placement_record_select(&directory_id.to_string(), content_hash.as_str(), |row| {
            previous = row.get(0)?;
            Ok(())
        })?;
        let mut title: Option<String> = None;
        transaction.restore_book_placement_record_select_2(content_hash.as_str(), |row| {
            title = row.get(0)?;
            Ok(())
        })?;
        let requested = previous.or(title).ok_or_else(|| DatabaseError::message("book does not exist"))?;
        let mut occupied = Vec::new();
        transaction.restore_book_placement_record_select_3(&directory_id.to_string(), content_hash.as_str(), |row| {
            occupied.push(row.get::<_, String>(0)?);
            Ok(())
        })?;
        let chosen = library_replica::unique_file_name(&requested, occupied.iter().map(String::as_str));
        transaction.restore_book_record_update(content_hash.as_str())?;
        transaction.restore_book_placement_record_insert(&directory_id.to_string(), content_hash.as_str(), &chosen)?;
        #[cfg(not(target_arch = "wasm32"))]
        queue_file_work(&transaction, "restore", content_hash, &placement_path(&transaction, directory_id, &chosen)?)?;
        add_download_request(&transaction, content_hash)?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub fn transfer_book_placement(&self, content_hash: &ContentHash, source_id: &DirId, destination_id: &DirId, remove_source: bool) -> Result<String, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if source_id == destination_id {
            return Err(DatabaseError::message("source and destination folders are the same"));
        }
        // Native stores must materialize the requested filesystem placement;
        // a hash-addressed browser store has no separate physical placement.
        #[cfg(not(target_arch = "wasm32"))]
        let old_path = {
            let name = live_file_name(&transaction, source_id, content_hash)?.ok_or_else(|| DatabaseError::message("book is not present in the source folder"))?;
            placement_path(&transaction, source_id, &name)?
        };
        let mut source: Option<(String, bool)> = None;
        transaction.transfer_book_placement_record_select(&source_id.to_string(), content_hash.as_str(), |row| {
            source = Some((row.get(0)?, row.get(1)?));
            Ok(())
        })?;
        let (source_name, downloaded): (String, bool) = source.ok_or_else(|| DatabaseError::message("book is not present in the source folder"))?;
        if !directory_exists_in(&transaction, &destination_id.to_string())? {
            return Err(DatabaseError::message("destination folder does not exist"));
        }
        let existing = live_file_name(&transaction, destination_id, content_hash)?;
        let destination_name = if let Some(existing) = existing {
            existing
        } else {
            let chosen = choose_move_file_name(&transaction, &destination_id.to_string(), &source_name)?;
            transaction.transfer_book_placement_record_insert(&destination_id.to_string(), content_hash.as_str(), &chosen, i32::from(downloaded))?;
            copy_book_baseline(&transaction, source_id, destination_id, content_hash)?;
            chosen
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Publish the destination before retiring the source. Only this
            // book's paths need work; no whole-library projection is needed.
            queue_file_work(&transaction, "restore", content_hash, &placement_path(&transaction, destination_id, &destination_name)?)?;
            if remove_source && source_id != destination_id {
                queue_file_work(&transaction, "trash", content_hash, &old_path)?;
            }
        }
        if remove_source {
            transaction.remove_book_placement_record_update(&source_id.to_string(), content_hash.as_str(), unix_millis()?)?;
        }
        crate::sync::apply::commit(transaction)?;
        Ok(destination_name)
    }

    pub fn remove_book_placement(&self, content_hash: &ContentHash, source_id: &DirId) -> Result<bool, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(name) = live_file_name(&transaction, source_id, content_hash)? {
            queue_file_work(&transaction, "trash", content_hash, &placement_path(&transaction, source_id, &name)?)?;
        }
        let mut exists = false;
        transaction.remove_book_placement_record_select(&source_id.to_string(), content_hash.as_str(), |row| {
            exists = row.get(0)?;
            Ok(())
        })?;
        if !exists {
            return Err(DatabaseError::message("book is not present in this folder"));
        }
        let mut orphaned = false;
        transaction.remove_book_placement_record_select_2(content_hash.as_str(), &source_id.to_string(), |row| {
            orphaned = row.get(0)?;
            Ok(())
        })?;
        let now = unix_millis()?;
        if orphaned {
            // Removing the last copy trashes the book, retaining its placement
            // so restore does not have to reconstruct membership from clocks.
            transaction.trash_directory_record_update(content_hash.as_str(), now, &source_id.to_string())?;
        } else {
            transaction.remove_book_placement_record_update(&source_id.to_string(), content_hash.as_str(), now)?;
        }
        crate::sync::apply::commit(transaction)?;
        Ok(orphaned)
    }

    pub fn library_trash(&self) -> Result<LibraryTrashView, DatabaseError> {
        let mut books = Vec::new();
        self.connection.library_trash_select(|row| {
            books.push(TrashedBook { content_hash: ContentHash::new(&row.get::<_, String>(0)?), title: row.get(1)?, format: row.get(2)?, deleted_at: row.get(3)? });
            Ok(())
        })?;
        let mut raw = Vec::new();
        self.connection.library_trash_select_2(|row| {
            raw.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?));
            Ok(())
        })?;
        let mut folders = Vec::new();
        for (id, name, deleted_at) in raw {
            folders.push(TrashedFolder { id: parse_dir_id(id)?, name, deleted_at });
        }
        Ok(LibraryTrashView { books, folders })
    }

    pub fn trash_book(&self, content_hash: &ContentHash) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut placements = Vec::new();
            transaction.trash_book_record_select(content_hash.as_str(), |row| {
                placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
                Ok(())
            })?;
            for (dir, name) in &placements {
                queue_file_work(&transaction, "trash", content_hash, &placement_path(&transaction, &parse_dir_id(dir.clone())?, name)?)?;
            }
        }
        let now = unix_millis()?;
        let changed = transaction.trash_book_record_update(content_hash.as_str(), now)?;
        if changed == 0 {
            return Err(DatabaseError::message("book does not exist or is already in Trash"));
        }
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub fn restore_book_plan(&self, content_hash: &ContentHash) -> Result<Vec<RestorePlacement>, DatabaseError> {
        if !book_is_trashed(&self.connection, content_hash)? {
            return Err(DatabaseError::message("book is not in Trash"));
        }
        let recoverable = recoverable_placements(&self.connection, content_hash)?;
        if recoverable.is_empty() {
            return Err(DatabaseError::message("book has no recoverable placement"));
        }
        let mut plan = Vec::with_capacity(recoverable.len());
        for (original, name) in recoverable {
            let mut ancestry = Vec::new();
            let mut target = parse_dir_id(original.clone())?;
            let mut visited = HashSet::new();
            while target != sync_common::ROOT_DIR_ID {
                if !visited.insert(target) {
                    break;
                }
                let mut directory: Option<(String, bool)> = None;
                self.connection.restore_book_record_with_parent_select_4(&target.to_string(), |row| {
                    directory = Some((row.get(0)?, row.get(1)?));
                    Ok(())
                })?;
                let Some((parent, live)) = directory else {
                    break;
                };
                let path = directory_relative_path(&self.connection, &target)?.unwrap_or_default();
                ancestry.push(RestoreAncestor { id: target, path, live });
                target = parse_dir_id(parent)?;
            }
            plan.push(RestorePlacement { original: parse_dir_id(original)?, file_name: name, ancestry });
        }
        Ok(plan)
    }

    /// Commit out for a book restore. Every recoverable placement needs
    /// exactly one target; a target that stopped being live fails the whole
    /// commit so the caller can re-plan.
    pub fn restore_book_commit(&self, content_hash: &ContentHash, choices: &[(DirId, DirId)]) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if !book_is_trashed(&transaction, content_hash)? {
            return Err(DatabaseError::message("book is not in Trash"));
        }
        let recoverable: std::collections::HashMap<DirId, String> =
            recoverable_placements(&transaction, content_hash)?.into_iter().map(|(original, name)| parse_dir_id(original).map(|original| (original, name))).collect::<Result<_, _>>()?;
        if recoverable.is_empty() {
            return Err(DatabaseError::message("book has no recoverable placement"));
        }
        let mut originals = HashSet::new();
        if choices.len() != recoverable.len() || choices.iter().any(|(original, _)| !recoverable.contains_key(original) || !originals.insert(*original)) {
            return Err(DatabaseError::message("restore targets do not cover the recoverable placements"));
        }
        // Choosing another folder is an explicit move, not a side effect of
        // trashing. Retain originals that are also selected destinations.
        let targets = choices.iter().map(|(_, target)| *target).collect::<HashSet<_>>();
        for (original, _) in choices {
            if !targets.contains(original) {
                transaction.remove_book_placement_record_update(&original.to_string(), content_hash.as_str(), unix_millis()?)?;
            }
        }
        transaction.restore_book_record_update(content_hash.as_str())?;
        for (original, target) in choices {
            if *target != sync_common::ROOT_DIR_ID && !directory_is_live(&transaction, target)? {
                return Err(DatabaseError::message("restore target changed; retry the restore"));
            }
            let name = &recoverable[original];
            let existing = live_file_name(&transaction, target, content_hash)?;
            let chosen = if let Some(existing) = existing {
                existing
            } else {
                let occupied = occupied_file_names_in(&transaction, &target.to_string())?;
                let chosen = library_replica::unique_file_name(name, occupied.iter().map(String::as_str));
                transaction.restore_book_record_with_parent_insert(&target.to_string(), content_hash.as_str(), &chosen)?;
                chosen
            };
            queue_file_work(&transaction, "restore", content_hash, &placement_path(&transaction, target, &chosen)?)?;
        }
        add_download_request(&transaction, content_hash)?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    /// Permanently removes a book in Trash. Sync intent and retryable file
    /// cleanup survive independently of the deleted database row.
    pub fn purge_book(&self, content_hash: &ContentHash) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        purge_book_in(&transaction, content_hash)?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    /// Seeds this hash's queued purge work directly, without running the
    /// rest of [`Database::purge_book`]'s bibliographic cleanup.
    pub fn seed_purge_work(&self, content_hash: &ContentHash) {
        self.connection.shared_purge_book_record_insert(content_hash.as_str()).expect("seed purge work");
    }

    pub fn trash_directory(&self, directory_id: &DirId) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if *directory_id == sync_common::ROOT_DIR_ID {
            return Err(DatabaseError::message("the library root cannot be moved to Trash"));
        }
        if !directory_exists_in(&transaction, &directory_id.to_string())? {
            return Err(DatabaseError::message("folder does not exist or is already in Trash"));
        }
        let now = unix_millis()?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut files = Vec::new();
            transaction.trash_directory_record_with(&directory_id.to_string(), |row| {
                files.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?));
                Ok(())
            })?;
            for (hash, dir, name) in files {
                // A pending folder move may not have reached its logical path.
                let remembered: Option<String> = transaction.query_row("SELECT relative_path FROM local_file_projection WHERE content_hash=?1 AND dir_id=?2", rusqlite::params![hash, dir], |row| row.get(0)).optional()?;
                let path = match remembered {
                    Some(path) => path,
                    None => placement_path(&transaction, &parse_dir_id(dir)?, &name)?,
                };
                queue_file_work(&transaction, "trash", &ContentHash::new(&hash), &path)?;
            }
        }
        let mut hashes = Vec::new();
        transaction.trash_directory_record_with_2(&directory_id.to_string(), |row| {
            hashes.push(row.get::<_, String>(0)?);
            Ok(())
        })?;
        for hash in hashes {
            transaction.trash_directory_record_update(&hash, now, &directory_id.to_string())?;
        }
        // Only books still present elsewhere need independent placement removals.
        transaction.trash_directory_record_with_3(&directory_id.to_string(), now)?;
        transaction.trash_directory_record_update_2(&directory_id.to_string(), now)?;
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    pub fn restore_directory(&self, directory_id: &DirId, requested_parent: Option<&DirId>) -> Result<String, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let state = canonical_directory(&transaction, directory_id)?;
        if state.lifecycle != Some(DirectoryLifecycleState::Deleted) {
            return Err(DatabaseError::message("folder is not in Trash"));
        }
        let (Some(stored_parent), Some(name)) = (state.parent, state.name) else {
            return Err(DatabaseError::message("folder state is incomplete; retry after sync"));
        };
        let requested_parent = requested_parent.copied().unwrap_or(stored_parent);
        let parent = if directory_exists_in(&transaction, &requested_parent.to_string())? { requested_parent } else { sync_common::ROOT_DIR_ID };
        let mut occupied = Vec::new();
        transaction.placements_sibling_folder_names(&parent.to_string(), &directory_id.to_string(), |row| {
            occupied.push(row.get::<_, String>(0)?);
            Ok(())
        })?;
        let chosen = library_replica::unique_folder_name(&name, occupied.iter().map(String::as_str));
        upsert_dir(&transaction, directory_id, &parent, &chosen)?;
        queue_directory_work(&transaction, directory_id)?;
        let mut restored_files = Vec::new();
        transaction.restore_directory_record_select_3(&directory_id.to_string(), |row| {
            restored_files.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?));
            Ok(())
        })?;
        transaction.restore_directory_record_update(&directory_id.to_string())?;
        transaction.restore_directory_record_update_2(&directory_id.to_string())?;
        // Restore the complete tree and book projections before deriving work
        // paths. Suppressed descendants still have no physical destination.
        crate::sync::apply::project_dirty(&transaction)?;
        for (hash, dir, _) in restored_files {
            let hash = ContentHash::new(&hash);
            let dir = parse_dir_id(dir)?;
            if directory_relative_path(&transaction, &dir)?.is_none() {
                continue;
            }
            let Some(name) = live_file_name(&transaction, &dir, &hash)? else { continue };
            queue_file_work(&transaction, "restore", &hash, &placement_path(&transaction, &dir, &name)?)?;
            add_download_request(&transaction, &hash)?;
        }
        directory_relative_path(&transaction, directory_id)?.ok_or_else(|| DatabaseError::message("restored directory has no path"))?;
        let projected_name = transaction.query_row("SELECT name FROM dir WHERE id=?1", [directory_id.to_string()], |row| row.get::<_, String>(0))?;
        transaction.commit()?;
        Ok(projected_name)
    }

    pub fn purge_directory(&self, directory_id: &DirId) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        if canonical_directory(&transaction, directory_id)?.lifecycle != Some(DirectoryLifecycleState::Deleted) {
            return Err(DatabaseError::message("folder is not in Trash"));
        }
        let changed = transaction.purge_directory_record_with(&directory_id.to_string(), unix_millis()?)?;
        if changed == 0 {
            return Err(DatabaseError::message("folder is not in Trash"));
        }
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }

    /// Permanently purges every current Trash entry in one transaction.
    /// Folder purging deliberately does not imply book purging, so both
    /// collections are processed explicitly.
    pub fn empty_trash(&self) -> Result<(), DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.empty_trash_records_with(unix_millis()?)?;
        let mut books = Vec::new();
        transaction.empty_trash_records_select(|row| {
            books.push(row.get::<_, String>(0)?);
            Ok(())
        })?;
        for hash in books {
            purge_book_in(&transaction, &ContentHash::new(&hash))?;
        }
        crate::sync::apply::commit(transaction)?;
        Ok(())
    }
}

fn purge_book_in(transaction: &rusqlite::Transaction<'_>, content_hash: &ContentHash) -> Result<(), DatabaseError> {
    let mut book_row_id: Option<i64> = None;
    transaction.purge_book_record_select(content_hash.as_str(), |row| {
        book_row_id = row.get(0)?;
        Ok(())
    })?;
    let book_row_id = book_row_id.ok_or_else(|| DatabaseError::message("book is not in Trash"))?;
    let now = unix_millis()?;

    // Publish placement tombstones before removing their disposable local
    // rows. Other replicas use these before the Purged lifecycle arrives.
    transaction.purge_book_record_update(book_row_id, now)?;
    // Publish the delete intent directly. Projection performs the same physical
    // row deletion for local commands and received lifecycle winners.
    crate::sync::apply::registers::accept_value(
        transaction,
        &library_replica::MutationBody::BookLifecycle { content_hash: *content_hash, value: library_replica::BookLifecycleState::Purged },
        now as u64,
        None,
    )?;
    Ok(())
}

include_sql!("src/placements/sql/schema.sql");
include_sql!("src/placements/sql/placements.sql");

// ---- scanner ----
// Database-owned contracts for filesystem discovery.
//
// A scanner receives one immutable inventory snapshot and returns observations.
// It never owns a connection, emits application events, or decides when work
// is retried. The library session owns that orchestration.

use book_model::BookFormat;

#[derive(Clone, Debug)]
pub struct ScanSnapshot {
    pub work_revision: i64,
    pub scan_id: i64,
    pub excluded_paths: Vec<String>,
    pub directories: Vec<ScanKnownDirectory>,
    pub books: Vec<ScanKnownBook>,
    pub placements: Vec<ScanPlacement>,
    pub tombstoned_files: Vec<(DirId, String)>,
    pub audible_current: Vec<ContentHash>,
}

/// Local placement state captured before filesystem I/O, including rows whose
/// fingerprint is empty (for example, a freshly downloaded book).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanPlacement {
    pub content_hash: ContentHash,
    pub directory: DirId,
    pub name: String,
    pub path: String,
    pub fingerprint: String,
    pub downloaded: bool,
    pub last_scan: i64,
    pub projected_path: Option<String>,
}

#[cfg(feature = "scanner")]
fn scan_placements(connection: &rusqlite::Connection) -> Result<Vec<ScanPlacement>, DatabaseError> {
    let mut placements = Vec::new();
    connection.scanner_placements(|row| {
        placements.push(ScanPlacement {
            content_hash: ContentHash::new(&row.get::<_, String>(0)?),
            directory: row.get::<_, String>(1)?.parse().map_err(|error| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error)))?,
            name: row.get(2)?,
            path: row.get(3)?,
            fingerprint: row.get(4)?,
            downloaded: row.get(5)?,
            last_scan: row.get(6)?,
            projected_path: row.get(7)?,
        });
        Ok(())
    })?;
    Ok(placements)
}

#[cfg(feature = "scanner")]
fn scan_path_excluded(snapshot: &ScanSnapshot, path: &str) -> bool {
    let path = std::path::Path::new(path.trim_matches('/'));
    snapshot.excluded_paths.iter().any(|excluded| {
        let excluded = std::path::Path::new(excluded.trim_matches('/'));
        path.starts_with(excluded) || excluded.starts_with(path)
    })
}

#[derive(Clone, Debug)]
pub struct ScanKnownDirectory {
    pub id: DirId,
    pub parent_id: DirId,
    pub name: String,
    pub path: Option<String>,
    pub deleted: bool,
}

#[derive(Clone, Debug)]
pub struct ScanKnownBook {
    pub content_hash: ContentHash,
    pub directory: DirId,
    pub name: String,
    pub fingerprint: u64,
    pub description_missing: bool,
    pub inspection_version: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct FilesystemScan {
    pub directories: Vec<ScanDirectoryObservation>,
    pub files: Vec<ScanFileObservation>,
    pub readable: bool,
}

#[derive(Clone, Debug)]
pub struct ScanDirectoryObservation {
    pub id: DirId,
    pub parent_id: DirId,
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct ScanFileObservation {
    pub directory: DirId,
    pub name: String,
    pub path: String,
    pub fingerprint: u64,
}

#[derive(Clone, Debug, Default)]
pub struct InspectedFilesystemScan {
    pub failures: Vec<(String, String)>,
    pub books: Vec<ScannedBook>,
    pub readable: bool,
}

#[derive(Clone, Debug)]
pub struct ScannedBook {
    pub directory: DirId,
    pub name: String,
    pub path: String,
    pub fingerprint: u64,
    pub content_hash: ContentHash,
    pub checksum: ContentHash,
    pub size_bytes: u64,
    pub extension: String,
    /// The book's own metadata (title, authors, TOC, ...), extracted the
    /// same way an explicit import extracts it. The scanner omits a book
    /// from a pass entirely rather than including one with no inspection:
    /// a bare `content_hash`/no metadata here would be indistinguishable
    /// from "reuse the cached inspection," which does not exist yet for a
    /// book discovered for the first time.
    pub inspection: book_metadata::InspectedBook,
    pub inspection_version: i64,
}

impl Database {
    /// Takes the immutable inventory view required by filesystem discovery. The
    /// connection is released before the caller walks or hashes any path.
    #[cfg(feature = "scanner")]
    pub fn scan_snapshot(&self) -> Result<ScanSnapshot, DatabaseError> {
        self.with_write_transaction(|transaction| {
            let connection = &*transaction;
            let work_revision = connection.scanner_file_work_revision(|row| row.get(0))?;
            let mut sequence = None;
            connection.shared_scanner_sequence(|row| {
                sequence = Some(row.get::<_, String>(0)?);
                Ok(())
            })?;
            let current = sequence.map(|value| value.parse::<i64>().map_err(DatabaseError::operation)).transpose()?.unwrap_or(0);
            let scan_id = current.checked_add(1).ok_or_else(|| DatabaseError::message("scan sequence overflow"))?;
            connection.scanner_set_sequence(&scan_id.to_string())?;
            let mut excluded_paths = Vec::new();
            let mut pending_directory_paths = Vec::new();
            connection.scanner_pending_directories(&sync_common::ROOT_DIR_ID.to_string(), |row| {
                pending_directory_paths.push((row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?));
                Ok(())
            })?;
            for (live, remembered) in pending_directory_paths {
                excluded_paths.extend(live.into_iter().chain(remembered).map(|path| path.trim_matches('/').to_owned()));
            }
            let mut pending_file_paths = Vec::new();
            connection.scanner_pending_files(|row| {
                pending_file_paths.push(row.get::<_, String>(0)?);
                Ok(())
            })?;
            excluded_paths.extend(pending_file_paths.into_iter().map(|path| path.trim_matches('/').to_owned()));
            let mut books = Vec::new();
            connection.scanner_known_books(|row| {
                let Ok(fingerprint) = row.get::<_, String>(0)?.parse::<u64>() else { return Ok(()) };
                books.push((fingerprint, ContentHash::new(&row.get::<_, String>(1)?), row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get(4)?, row.get(5)?));
                Ok(())
            })?;
            let books = books
                .into_iter()
                .map(|(fingerprint, content_hash, directory, name, description_missing, inspection_version)| {
                    Ok(ScanKnownBook { content_hash, directory: DirId::parse_str(&directory).map_err(DatabaseError::operation)?, name, fingerprint, description_missing, inspection_version })
                })
                .collect::<Result<Vec<_>, DatabaseError>>()?;
            let mut directories = Vec::new();
            connection.scanner_known_directories(|row| {
                directories.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, bool>(3)?, row.get::<_, Option<String>>(4)?));
                Ok(())
            })?;
            let directories = directories
                .into_iter()
                .map(|(id, parent_id, name, live, path)| {
                    Ok(ScanKnownDirectory {
                        id: DirId::parse_str(&id).map_err(DatabaseError::operation)?,
                        parent_id: DirId::parse_str(&parent_id).map_err(DatabaseError::operation)?,
                        name,
                        path: if live { path } else { None },
                        deleted: !live,
                    })
                })
                .collect::<Result<Vec<_>, DatabaseError>>()?;
            let mut tombstoned_files = Vec::new();
            connection.scanner_tombstoned_files(|row| {
                tombstoned_files.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
                Ok(())
            })?;
            let tombstoned_files = tombstoned_files.into_iter().map(|(directory, name)| DirId::parse_str(&directory).map(|directory| (directory, name)).map_err(DatabaseError::operation)).collect::<Result<Vec<_>, DatabaseError>>()?;
            let mut audible_current = Vec::new();
            connection.scanner_audible_current(metadata_contract::audible::POLICY_VERSION, |row| {
                audible_current.push(ContentHash::new(&row.get::<_, String>(0)?));
                Ok(())
            })?;
            let placements = scan_placements(connection)?;
            Self::begin_filesystem_scan_operation(connection)?;
            Ok(crate::transactions::WriteOutcome::Commit(ScanSnapshot { work_revision, scan_id, excluded_paths, directories, books, placements, tombstoned_files, audible_current }))
        })
    }

    /// Applies one filesystem result only when the file-work revision that
    /// produced its snapshot is still current. Filesystem discovery never
    /// holds this transaction.
    #[cfg(feature = "scanner")]
    pub fn apply_filesystem_scan(&self, snapshot: &ScanSnapshot, discovery: &FilesystemScan, inspected: &InspectedFilesystemScan) -> Result<ScanApplyResult, DatabaseError> {
        self.apply_filesystem_scan_batch(&mut snapshot.clone(), discovery, inspected, true)
    }

    /// Publish inspected books incrementally. Only the final, readable batch
    /// may reconcile missing files. Refresh the guard inside the transaction
    /// so our writes are accepted but concurrent edits still invalidate it.
    #[cfg(feature = "scanner")]
    pub fn apply_filesystem_scan_batch(&self, snapshot: &mut ScanSnapshot, discovery: &FilesystemScan, inspected: &InspectedFilesystemScan, complete: bool) -> Result<ScanApplyResult, DatabaseError> {
        let mut preparation_timing = crate::timing::PhaseTimer::new("scan_batch");
        // Normalize metadata and serialize inspection data before acquiring
        // the writer. Snapshot validation and identity resolution remain in
        // the transaction so changes during preparation still invalidate it.
        let prepared_books = inspected
            .books
            .iter()
            .map(|book| {
                let format = BookFormat::from_extension(&book.extension).ok_or_else(|| DatabaseError::message(format!("unsupported scanned extension: {}", book.extension)))?;
                let prepared = crate::import::prepare_inspection(&self.connection, &book.content_hash, &book.inspection, &book.checksum, format)?;
                Ok((format, prepared))
            })
            .collect::<Result<Vec<_>, DatabaseError>>()?;
        preparation_timing.mark("prepare_before_transaction");
        let mut next_guard = None;
        let result = self.with_write_transaction(|transaction| {
            let mut timing = crate::timing::PhaseTimer::new("scan_transaction");
            let current_revision: i64 = transaction.scanner_file_work_revision(|row| row.get(0))?;
            let mut current_sequence = String::new();
            transaction.shared_scanner_sequence(|row| {
                current_sequence = row.get(0)?;
                Ok(())
            })?;
            if current_revision != snapshot.work_revision || current_sequence != snapshot.scan_id.to_string() || scan_placements(transaction)? != snapshot.placements {
                return Ok(crate::transactions::WriteOutcome::Rollback(ScanApplyResult::Stale));
            }
            timing.mark("validate_snapshot");
            for directory in &discovery.directories {
                // dir_paths is a view computed from dir; upserting dir here is
                // sufficient for the view to reflect the new path.
                transaction.scanner_upsert_directory(&directory.id.to_string(), &directory.parent_id.to_string(), &directory.name)?;
            }
            timing.mark("directories");
            for (book, (format, prepared)) in inspected.books.iter().zip(prepared_books) {
                // Scanned books commit through the same identity-resolution and
                // placement logic explicit import uses, so a scan-discovered book
                // gets its title, authors, and subjects just like an imported one.
                let import = crate::import::ImportCommit {
                    parent_id: book.directory,
                    file_name: book.name.clone(),
                    format,
                    content_hash: book.content_hash.clone(),
                    checksum: book.checksum.clone(),
                    size_bytes: book.size_bytes,
                    inspection_version: book.inspection_version,
                    inspection: None,
                    published: crate::import::PublishedImport { name: book.name.clone(), relative_path: book.path.clone(), published: true, fingerprint: Some(book.fingerprint.to_string()) },
                    request_thumbnail: true,
                    restore_paths: Vec::new(),
                };
                // Only scanning may replace the old identity at a path already
                // observed on disk. Ordinary imports retain their collision check.
                for old in snapshot.placements.iter().filter(|old| old.directory == book.directory && old.name == book.name && old.content_hash != book.content_hash) {
                    if scan_path_excluded(snapshot, &old.path) {
                        return Ok(crate::transactions::WriteOutcome::Rollback(ScanApplyResult::Stale));
                    }
                    transaction.scanner_retire_replaced_placement(&old.directory.to_string(), old.content_hash.as_str(), unix_millis()?)?;
                    transaction.scanner_forget_projection(&old.directory.to_string(), old.content_hash.as_str())?;
                    transaction.scanner_forget_current_version(&old.directory.to_string(), old.content_hash.as_str())?;
                }
                crate::import::commit_one(transaction, &self.metadata_batch_flag, snapshot.scan_id, import, Some(prepared))?;
                transaction.scanner_clear_renamed_placement(&book.directory.to_string(), book.content_hash.as_str(), &book.name)?;
            }
            timing.mark("books");
            if complete {
                let seen = discovery.files.iter().map(|file| (file.directory, file.name.clone(), file.fingerprint)).collect::<Vec<_>>();
                for chunk in seen.chunks(250) {
                    let values = serde_json::to_string(&chunk.iter().map(|(dir, name, fingerprint)| (dir.to_string(), name, fingerprint.to_string())).collect::<Vec<_>>()).map_err(DatabaseError::operation)?;
                    transaction.scanner_mark_seen(snapshot.scan_id, &values)?;
                }
                if discovery.readable && inspected.readable {
                    let observed = discovery.files.iter().map(|file| (file.directory, file.name.as_str())).collect::<HashSet<_>>();
                    let observed_paths = discovery.files.iter().map(|file| file.path.trim_matches('/')).collect::<HashSet<_>>();
                    let refreshed = inspected.books.iter().map(|book| (book.directory, book.content_hash)).collect::<HashSet<_>>();
                    for old in &snapshot.placements {
                        if !old.downloaded && old.projected_path.is_none() && old.fingerprint.is_empty() {
                            continue;
                        }
                        if observed.contains(&(old.directory, old.name.as_str()))
                            || refreshed.contains(&(old.directory, old.content_hash))
                            || scan_path_excluded(snapshot, &old.path)
                            || old.projected_path.as_deref().is_some_and(|path| scan_path_excluded(snapshot, path) || observed_paths.contains(path.trim_matches('/')))
                        {
                            continue;
                        }
                        // Absence is local availability, not a synchronized deletion.
                        transaction.scanner_clear_local_availability(&old.directory.to_string(), old.content_hash.as_str())?;
                        transaction.scanner_forget_projection(&old.directory.to_string(), old.content_hash.as_str())?;
                        transaction.scanner_forget_current_version(&old.directory.to_string(), old.content_hash.as_str())?;
                    }
                }
            }
            Self::record_filesystem_scan_failures(transaction, &inspected.failures)?;
            if complete {
                Self::finish_filesystem_scan_operation(transaction, discovery.readable)?;
            }
            timing.mark("finalize_inventory");
            // Commit-time projection can change placements and their work revision.
            // Include those changes in our next batch guard while still holding the writer.
            crate::sync::apply::project_dirty(transaction)?;
            next_guard = Some((transaction.scanner_file_work_revision(|row| row.get(0))?, scan_placements(transaction)?));
            timing.mark("refresh_snapshot");
            Ok(crate::transactions::WriteOutcome::Commit(ScanApplyResult::Applied))
        })?;
        if let Some((revision, placements)) = next_guard {
            snapshot.work_revision = revision;
            snapshot.placements = placements;
        }
        Ok(result)
    }
}

include_sql!("src/placements/sql/scanner.sql");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanApplyResult {
    Applied,
    Stale,
}

// ---- paths ----
// Read-only book metadata used to resolve physical storage.
//
// The filesystem layer receives the resulting values; it never opens the
// library database to discover them.

/// Database facts required to open a locally materialized book.
/// Filesystem code receives only `path`; it never consults SQLite itself.
pub struct LocalBookInfo {
    pub format: book_model::BookFormat,
    pub path: Option<String>,
}

impl Database {
    pub fn pdf_metadata(&self, hash: &ContentHash, checksum: &ContentHash) -> Result<Option<pdf_view_common::PdfReaderMetadata>, DatabaseError> {
        let connection = &self.connection;
        let mut bytes = None;
        connection.stored_pdf_metadata(hash.as_str(), checksum.as_str(), |row| {
            bytes = Some(row.get::<_, Vec<u8>>(0)?);
            Ok(())
        })?;
        Ok(bytes.and_then(|bytes| serde_json::from_slice::<pdf_view_common::PdfReaderMetadata>(&bytes).ok()).filter(|metadata| metadata.valid_for(checksum.as_str())))
    }

    pub fn local_book_info(&self, hash: &ContentHash) -> Result<LocalBookInfo, DatabaseError> {
        let connection = &self.connection;
        let mut info = None;
        connection.local_book_info(hash.as_str(), |row| {
            info = Some((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?));
            Ok(())
        })?;
        let (format, path) = info.ok_or_else(|| DatabaseError::message("book format missing"))?;
        let format = book_model::BookFormat::from_extension(&format).ok_or_else(|| DatabaseError::message("book format missing"))?;
        Ok(LocalBookInfo { format, path })
    }

    /// Every remembered native path for a book, including a source
    /// retained while a move is pending.
    pub fn book_paths(&self, hash: &ContentHash) -> Result<Vec<String>, DatabaseError> {
        let connection = &self.connection;
        let mut paths = Vec::new();
        connection.book_paths(hash.as_str(), |row| {
            paths.push(row.get(0)?);
            Ok(())
        })?;
        Ok(paths)
    }

    /// Resolve native book placements without opening a session or changing
    /// its schema. Used by physical readers that need a fresh path snapshot.
    pub fn book_paths_at(path: &std::path::Path, hash: &ContentHash) -> Result<Vec<String>, DatabaseError> {
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let connection = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut paths = Vec::new();
        connection.book_paths(hash.as_str(), |row| {
            paths.push(row.get(0)?);
            Ok(())
        })?;
        Ok(paths)
    }

    /// Paths that contribute to local book storage usage.
    pub fn local_book_paths(&self) -> Result<Vec<String>, DatabaseError> {
        let connection = &self.connection;
        let mut paths = Vec::new();
        connection.local_book_paths(|row| {
            paths.push(row.get(0)?);
            Ok(())
        })?;
        Ok(paths)
    }

    /// PDF metadata only applies when the metadata fingerprint still matches
    /// the exact local placement selected for reading.
    pub fn local_pdf_metadata(&self, hash: &ContentHash, fingerprint: &str, relative_path: &str) -> Result<Option<pdf_view_common::PdfReaderMetadata>, DatabaseError> {
        let connection = &self.connection;
        let mut raw_checksum = None;
        use rusqlite::OptionalExtension as _;
        connection
            .local_pdf_checksum(hash.as_str(), fingerprint, relative_path, |row| {
                raw_checksum = Some(row.get::<_, String>(0)?);
                Ok(())
            })
            .optional()?;
        let Some(checksum) = raw_checksum.and_then(|value| value.parse::<ContentHash>().ok()) else {
            return Ok(None);
        };
        self.pdf_metadata(hash, &checksum)
    }

    /// Live file names already occupied in a destination folder. Physical
    /// name collisions are checked separately by the file store.
    pub fn shared_occupied_file_names(&self, destination: &sync_common::DirId) -> Result<Vec<String>, DatabaseError> {
        let mut names = Vec::new();
        self.connection.shared_occupied_file_names(&destination.to_string(), |row| {
            names.push(row.get::<_, String>(0)?);
            Ok(())
        })?;
        Ok(names)
    }
}

#[cfg(all(test, feature = "scanner"))]
mod scan_tests {
    use super::{scan_path_excluded, FilesystemScan, InspectedFilesystemScan, ScanApplyResult, ScanDirectoryObservation, ScanFileObservation, ScanSnapshot, ScannedBook};
    use crate::Database;
    use sync_common::ContentHash;

    #[test]
    fn an_excluded_child_preserves_its_existing_book_placement() {
        let snapshot = ScanSnapshot { work_revision: 0, scan_id: 0, excluded_paths: vec!["Story/02.mp3".into()], directories: vec![], books: vec![], placements: vec![], tombstoned_files: vec![], audible_current: vec![] };
        assert!(scan_path_excluded(&snapshot, "Story"));
        assert!(scan_path_excluded(&snapshot, "Story/02.mp3"));
        assert!(!scan_path_excluded(&snapshot, "Other Story"));
    }

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
        let path = std::env::temp_dir().join(format!("library-database-scan-{}-{id}", std::process::id()));
        let database = Database::open(&path).expect("open test database");
        database.initialize_library().expect("initialize test library");
        TestDatabase { database, path }
    }

    /// Regression test: `scanner_known_books` referenced a `description_missing`
    /// column that does not exist on `book` (the real column is
    /// `description_scanned`), so every scan of a new library that already had
    /// books on disk failed before discovering anything.
    #[test]
    fn scan_snapshot_reports_a_locally_placed_book() {
        let test_db = open_test_database();
        let db = &test_db.database;
        let directory = db.create_directory(&sync_common::ROOT_DIR_ID, &"Folder".to_owned()).expect("create directory");
        let content_hash = ContentHash::new(&"a".repeat(64));
        db.connection.execute("INSERT INTO book(content_hash,title,format,added_at) VALUES(?1,?2,'epub',0)", rusqlite::params![content_hash.as_str(), "Title"]).expect("insert book");
        let book_row_id: i64 = db.connection.query_row("SELECT row_id FROM book WHERE content_hash=?1", [content_hash.as_str()], |row| row.get(0)).expect("book row id");
        db.connection
            .execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) VALUES(?1,?2,?3,'12345',1)", rusqlite::params![directory.id.to_string(), book_row_id, "book.epub"])
            .expect("insert book_dir placement");

        let snapshot = db.scan_snapshot().expect("scan snapshot succeeds");

        assert_eq!(snapshot.books.len(), 1);
        let book = &snapshot.books[0];
        assert_eq!(book.content_hash, content_hash);
        assert_eq!(book.name, "book.epub");
        assert_eq!(book.directory, directory.id);
        assert_eq!(book.fingerprint, 12345);
        // description_scanned defaults to 0, so a freshly discovered book
        // still needs its description fetched.
        assert!(book.description_missing);
        assert_eq!(book.inspection_version, None);
        db.connection.execute("INSERT INTO local_import_inspection(content_hash,format,version) VALUES(?1,'epub',7)", [content_hash.as_str()]).unwrap();
        assert_eq!(db.scan_snapshot().unwrap().books[0].inspection_version, Some(7));
        db.connection.execute("UPDATE local_import_inspection SET format='pdf' WHERE content_hash=?1", [content_hash.as_str()]).unwrap();
        assert_eq!(db.scan_snapshot().unwrap().books[0].inspection_version, None, "another format's inspection is not reusable");
    }

    /// Regression test: `scanner_insert_book` wrote to a `created_at` column
    /// that does not exist on `book` (the real column is `added_at`), so
    /// applying a scan that discovered any new, previously-unknown file
    /// failed before registering it — exactly what happens when a new
    /// library is pointed at a folder that already has books in it.
    #[test]
    fn apply_filesystem_scan_registers_a_newly_discovered_book() {
        let test_db = open_test_database();
        let db = &test_db.database;
        let snapshot = db.scan_snapshot().expect("scan snapshot succeeds");
        let content_hash = ContentHash::new(&"b".repeat(64));
        let checksum = ContentHash::new(&"c".repeat(64));
        let subdirectory_id: sync_common::DirId = "12345678-1234-1234-1234-000000000001".parse().unwrap();
        let discovery = FilesystemScan {
            // Also exercises the directory side of apply: `scanner_set_directory_path`
            // used to try writing into `dir_paths`, a read-only view over `dir`.
            directories: vec![ScanDirectoryObservation { id: subdirectory_id, parent_id: sync_common::ROOT_DIR_ID, name: "Folder".to_owned(), path: "Folder".to_owned() }],
            files: vec![ScanFileObservation { directory: sync_common::ROOT_DIR_ID, name: "book.epub".to_owned(), path: "book.epub".to_owned(), fingerprint: 999 }],
            readable: true,
        };
        let inspected = InspectedFilesystemScan {
            failures: vec![],
            books: vec![ScannedBook {
                directory: sync_common::ROOT_DIR_ID,
                name: "book.epub".to_owned(),
                path: "book.epub".to_owned(),
                fingerprint: 999,
                content_hash: content_hash.clone(),
                checksum,
                size_bytes: 1234,
                extension: "epub".to_owned(),
                inspection: book_metadata::InspectedBook {
                    pdf: None,
                    metadata: book_model::BookRecord {
                        title: "Scanned Title".to_owned(),
                        subtitle: None,
                        contributors: vec![book_model::Contributor::new("Scanned Author", book_model::MarcRelatorCode(*b"aut")).unwrap()],
                        description: "A synopsis.".to_owned(),
                        book: book_model::BookMetadata::default(),
                    },
                    audiobook: None,
                    toc: Vec::new(),
                },
                inspection_version: 1,
            }],
            readable: true,
        };

        let result = db.apply_filesystem_scan(&snapshot, &discovery, &inspected).expect("apply filesystem scan succeeds");

        assert_eq!(result, ScanApplyResult::Applied);
        let exists: bool = db.connection.query_row("SELECT EXISTS(SELECT 1 FROM book WHERE content_hash=?1)", [content_hash.as_str()], |row| row.get(0)).expect("book existence check");
        assert!(exists, "scanned book was not registered in the book table");
        // A scanned book commits through the same identity-resolution path
        // as an explicit import, so it must not land title-less.
        let title: String = db.connection.query_row("SELECT title FROM book WHERE content_hash=?1", [content_hash.as_str()], |row| row.get(0)).expect("book title");
        assert_eq!(title, "Scanned Title");
        let path: String = db.connection.query_row("SELECT path FROM dir_paths WHERE id=?1", [subdirectory_id.to_string()], |row| row.get(0)).expect("discovered directory is in dir_paths");
        assert_eq!(path, "Folder");
    }
}

include_sql!("src/placements/sql/book_paths.sql");
