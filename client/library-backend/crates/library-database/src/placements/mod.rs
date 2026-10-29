//! The stored collection: placements, discovery, physical paths.

use crate::SqlParam;
use crate::native::observations::forget_file;
use crate::projection::{place_book, project_dirty};
use crate::query_methods::query_rows;
pub(crate) use crate::shared_sql::SharedSql;
use crate::sync::apply::registers::record_local;
use crate::transactions::WriteOutcome::{Commit, Rollback};
use client_platform_runtime::startup_profile::StartupProfile;
use rusqlite::Transaction;
use std::collections::HashMap;
use std::path::Path;
// ---- placements ----
// Directory and physical book-placement persistence: moves, copies, and the
// Trash lifecycle (trash/restore/purge) for both books and folders.

pub(crate) use crate::{Database, DatabaseError};
pub(crate) use include_sqlite_sql::include_sql;
pub(crate) use library_model::TransferOrigin;
pub(crate) use library_model::{BookPlacement, RelativeBookPath};
pub(crate) use library_model::{Dir, LibraryFolderDestination, LibraryTrashView, TrashedBook, TrashedFolder};
pub(crate) use library_replica::{DirectoryLifecycleState, LibraryStateEntry};
pub(crate) use rusqlite::OptionalExtension;
pub(crate) use std::collections::HashSet;
pub(crate) use sync_common::{BookId, DirId, FileName};

pub(crate) fn unix_millis() -> Result<i64, DatabaseError> {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|duration| duration.as_millis() as i64).map_err(DatabaseError::operation)
}

pub(crate) fn parse_dir_id(raw: String) -> Result<DirId, DatabaseError> {
    DirId::parse_str(&raw).map_err(DatabaseError::operation)
}

/// Read command preconditions from canonical registers, never placeholder defaults.
#[derive(Default)]
pub(crate) struct CanonicalDirectory {
    pub(crate) name: Option<FileName>,
    pub(crate) parent: Option<DirId>,
    pub(crate) lifecycle: Option<DirectoryLifecycleState>,
}

pub(crate) fn canonical_directory(connection: &rusqlite::Connection, id: &DirId) -> Result<CanonicalDirectory, DatabaseError> {
    let mut state = CanonicalDirectory::default();

    for row in query_rows(|callback| connection.directory_read_sync_state_version_body(&(id.to_string()), callback), |row| row.get::<_, Vec<u8>>(0))? {
        match sync_common::wire::decode::<LibraryStateEntry>(&row?, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? {
            LibraryStateEntry::DirectoryName { name, .. } => state.name = Some(name),
            LibraryStateEntry::DirectoryParent { parent_id, .. } => state.parent = Some(parent_id),
            LibraryStateEntry::DirectoryLifecycle { value, .. } => state.lifecycle = Some(value),
            _ => return Err(DatabaseError::message("invalid canonical directory state")),
        }
    }
    Ok(state)
}

pub(crate) fn book_is_trashed(connection: &rusqlite::Connection, content_hash: &BookId) -> Result<bool, DatabaseError> {
    let mut trashed = false;
    connection.placements_book_is_trashed(content_hash, |row| {
        trashed = row.get(0)?;
        Ok(())
    })?;
    Ok(trashed)
}

pub(crate) fn restore_placement_candidates(connection: &rusqlite::Connection, content_hash: &BookId) -> Result<Vec<(String, String)>, DatabaseError> {
    let mut placements = Vec::new();
    connection.restore_book_record_with_parent_select_2(content_hash, |row| {
        placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
        Ok(())
    })?;
    if placements.is_empty() {
        // A restore with no remaining membership explicitly creates a root
        // placement. Planning is read-only; commit revalidates these candidates.
        connection.restore_book_fallback_name(content_hash, |row| {
            placements.push((sync_common::ROOT_DIR_ID.to_string(), row.get::<_, String>(0)?));
            Ok(())
        })?;
    }
    Ok(placements)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirPathComponent {
    pub id: DirId,
    pub name: FileName,
}

/// One step of a book restore: a live flag snapshot for
/// every directory the walk would visit, starting at the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreAncestor {
    pub id: DirId,
    pub live: bool,
}

/// Database ancestry used to choose a live folder when restoring a book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlacement {
    /// Retained folder, or ROOT_DIR_ID as an explicit creation candidate when
    /// the book has no remaining placements. Planning does not create it.
    pub original: DirId,
    pub file_name: String,
    pub ancestry: Vec<RestoreAncestor>,
}

pub(crate) fn dir_path_components(connection: &rusqlite::Connection, dir_id: &str) -> Result<Vec<DirPathComponent>, DatabaseError> {
    // Reuse an existing transaction, or hold a read snapshot for the whole walk.
    let snapshot = connection.is_autocommit().then(|| connection.unchecked_transaction()).transpose()?;

    let mut current = dir_id.to_owned();
    let mut seen = HashSet::new();
    let mut components = Vec::new();
    let root = sync_common::ROOT_DIR_ID.to_string();
    loop {
        // A loop never reaches the root, which is the same as not being live.
        if !seen.insert(current.clone()) {
            components.clear();
            break;
        }
        let row = connection.directory_live_parent_and_name(&(&current), |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).optional()?;
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

/// Folder and placement paths for many rows in one operation. Rows share few
/// folders, so each folder's ancestry is walked once and remembered.
pub(crate) struct FolderPaths<'c> {
    pub(crate) connection: &'c rusqlite::Connection,
    pub(crate) folders: HashMap<String, Option<String>>,
}

impl<'c> FolderPaths<'c> {
    pub(crate) fn new(connection: &'c rusqlite::Connection) -> Self {
        Self { connection, folders: HashMap::new() }
    }

    /// "" for the root, "a/b" below it, or None when the folder is not live.
    pub(crate) fn folder(&mut self, dir_id: &str) -> Result<Option<String>, DatabaseError> {
        if !self.folders.contains_key(dir_id) {
            let folder = directory_relative_path(self.connection, &parse_dir_id(dir_id.to_owned())?)?;
            self.folders.insert(dir_id.to_owned(), folder);
        }
        Ok(self.folders[dir_id].clone())
    }

    /// "/a/b/book.epub", or None when the folder is not live.
    pub(crate) fn placement(&mut self, dir_id: &str, file_name: &str) -> Result<Option<RelativeBookPath>, DatabaseError> {
        self.folder(dir_id)?.map(|folder| stored_book_path(&format!("{folder}/{file_name}"))).transpose()
    }
}

/// Stored folder and file names must still form a path inside the library.
pub(crate) fn stored_book_path(value: &str) -> Result<RelativeBookPath, DatabaseError> {
    RelativeBookPath::parse(value).map_err(|error| DatabaseError::message(format!("stored book path {value:?} is not readable: {error}")))
}

/// Folders reached from `roots` one level per query, roots included.
/// Child queries take a JSON list of parents and return their children.
/// Each folder is visited once, so an ancestry loop cannot walk forever.
pub(crate) fn walk_down(connection: &rusqlite::Connection, roots: Vec<String>, _trashed_only: bool) -> Result<Vec<String>, DatabaseError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut level: Vec<String> = roots.into_iter().filter(|root| seen.insert(root.clone())).collect();
    let mut folders = level.clone();
    while !level.is_empty() {
        let parents = serde_json::to_string(&level).map_err(DatabaseError::operation)?;
        let children = query_rows(|callback| connection.directory_read_dir_parent_id_name_2(&parents, callback), |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        level = children.into_iter().filter(|child| seen.insert(child.clone())).collect();
        folders.extend(level.iter().cloned());
    }
    Ok(folders)
}

/// Folders with queued physical work, and every folder below them: a book in
/// any of them may be about to move on disk.
pub(crate) fn pending_work_directories(connection: &rusqlite::Connection) -> Result<Vec<String>, DatabaseError> {
    let roots = query_rows(|callback| connection.directory_work_roots(callback), |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    walk_down(connection, roots, false)
}

/// Trashed folders below `roots`, following each folder's remembered original
/// parent so folders hidden from the display tree still come along.
pub(crate) fn trashed_subtree(connection: &rusqlite::Connection, roots: Vec<String>) -> Result<Vec<String>, DatabaseError> {
    let roots = serde_json::to_string(&roots).map_err(DatabaseError::operation)?;
    let roots = query_rows(|callback| connection.directory_trashed_roots(&roots, callback), |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    walk_down(connection, roots, true)
}

/// Materialized paths first, then the book's live placements.
pub(crate) fn book_paths_in(connection: &rusqlite::Connection, hash: &BookId) -> Result<Vec<RelativeBookPath>, DatabaseError> {
    let mut profile = StartupProfile::new("book_lookup_paths");
    let mut projected = Vec::new();
    connection.book_projection_paths(hash, |row| {
        projected.push(row.get::<_, String>(0)?);
        Ok(())
    })?;
    profile.mark("projection_sql");
    let mut paths = projected.iter().map(|path| stored_book_path(path)).collect::<Result<Vec<_>, _>>()?;
    let mut placements = Vec::new();
    connection.shared_live_book_placements(hash, |row| {
        placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
        Ok(())
    })?;
    profile.mark("placements_sql");
    let mut resolver = FolderPaths::new(connection);
    for (dir_id, file_name) in placements {
        paths.extend(resolver.placement(&dir_id, &file_name)?);
    }
    profile.mark("folder_paths");
    Ok(paths)
}

/// A placement path's live folder and file name, walking folder names down
/// from the root. None when a folder on the way is missing or deleted.
pub(crate) fn resolve_placement_path(connection: &rusqlite::Connection, relative_path: &str) -> Result<Option<(String, String)>, DatabaseError> {
    let root = sync_common::ROOT_DIR_ID.to_string();

    if connection.directory_live_parent_and_name(&(&root), |_| Ok(())).optional()?.is_none() {
        return Ok(None);
    }
    let Some((folders, file_name)) = relative_path.strip_prefix('/').and_then(|path| path.rsplit_once('/').or(Some(("", path)))) else {
        return Ok(None);
    };

    let mut current = root;
    for name in folders.split('/').filter(|name| !name.is_empty()) {
        let Some(child) = connection.directory_named_child(&(&current), &name, |row| row.get::<_, String>(0)).optional()? else {
            return Ok(None);
        };
        current = child;
    }
    Ok(Some((current, file_name.to_owned())))
}

/// The live book at a placement path, with the folder and file name the path
/// resolved to.
pub(crate) fn book_at_path(connection: &rusqlite::Connection, relative_path: &str) -> Result<Option<(BookId, DirId, String)>, DatabaseError> {
    let Some((dir_id, file_name)) = resolve_placement_path(connection, relative_path)? else {
        return Ok(None);
    };
    let mut hash = None;
    connection.book_at_placement(&dir_id, &file_name, |row| {
        hash = Some(row.get::<_, BookId>(0)?);
        Ok(())
    })?;
    let Some(hash) = hash else { return Ok(None) };
    Ok(Some((hash, parse_dir_id(dir_id)?, file_name)))
}

/// Whether `directory` is `folder` or one of its ancestors, following current
/// parents through deleted folders too. A move that answers yes closes a loop.
pub(crate) fn is_ancestor_or_self(connection: &rusqlite::Connection, directory: &str, folder: &str) -> Result<bool, DatabaseError> {
    let root = sync_common::ROOT_DIR_ID.to_string();
    let mut current = folder.to_owned();
    let mut seen = HashSet::new();
    while seen.insert(current.clone()) {
        if current == directory {
            return Ok(true);
        }
        if current == root {
            break;
        }
        let Some(parent) = connection.directory_any_parent(&(&current), |row| row.get::<_, String>(0)).optional()? else { break };
        current = parent;
    }
    Ok(false)
}

pub(crate) fn directory_exists_in(connection: &rusqlite::Connection, directory_id: &str) -> Result<bool, DatabaseError> {
    let mut exists = false;
    connection.placements_live_directory_exists(directory_id, |row| {
        exists = row.get(0)?;
        Ok(())
    })?;
    Ok(exists)
}

pub(crate) fn validate_library_directory_name(name: &str) -> Result<(), DatabaseError> {
    if name.starts_with('.') {
        return Err(DatabaseError::message("dot-prefixed library directories are unsupported"));
    }
    Ok(())
}

/// Live database names reserved in a destination. Physical collisions are
/// checked separately by the file store.
pub(crate) fn occupied_file_names_in(connection: &rusqlite::Connection, destination: &str) -> Result<Vec<String>, DatabaseError> {
    let mut names = Vec::new();
    connection.shared_occupied_file_names(destination, |row| {
        names.push(row.get::<_, String>(0)?);
        Ok(())
    })?;
    Ok(names)
}

/// The filename of a live placement, or `None` when it is absent.
pub(crate) fn live_file_name(connection: &rusqlite::Connection, directory: &DirId, hash: &BookId) -> Result<Option<String>, DatabaseError> {
    let mut name = None;
    connection.placements_live_file_name(&directory.to_string(), hash, |row| {
        name = Some(row.get(0)?);
        Ok(())
    })?;
    Ok(name)
}

pub(crate) fn choose_move_folder_name(connection: &rusqlite::Connection, parent: &str, excluded: &str, requested: &str) -> Result<String, DatabaseError> {
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

/// Records this directory into `dir`, validating it is neither the root nor
/// its own ancestor.
pub(crate) fn upsert_dir(transaction: &rusqlite::Transaction<'_>, dir_id: &DirId, parent_id: &DirId, name: &str) -> Result<(), DatabaseError> {
    if *dir_id == sync_common::ROOT_DIR_ID {
        return Err(DatabaseError::message("the root directory is reserved"));
    }
    if dir_id == parent_id {
        return Err(DatabaseError::message("a directory cannot be its own parent"));
    }
    let closes_cycle = is_ancestor_or_self(transaction, &dir_id.to_string(), &parent_id.to_string())?;
    if closes_cycle {
        return Err(DatabaseError::message("directory move would create a cycle"));
    }
    record_local(transaction, LibraryStateEntry::DirectoryName { dir_id: *dir_id, name: name.to_owned() })?;
    record_local(transaction, LibraryStateEntry::DirectoryParent { dir_id: *dir_id, parent_id: *parent_id })?;
    record_local(transaction, LibraryStateEntry::DirectoryLifecycle { dir_id: *dir_id, value: DirectoryLifecycleState::Present })?;
    project_dirty(transaction)?;
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
pub(crate) fn queue_directory_work(transaction: &rusqlite::Transaction<'_>, dir: &DirId) -> Result<(), DatabaseError> {
    #[cfg(target_arch = "wasm32")]
    let _ = (transaction, dir);
    #[cfg(not(target_arch = "wasm32"))]
    transaction.shared_placements_queue_directory_work(&dir.to_string())?;
    Ok(())
}

/// Publishes a book's queued local file operation. A no-op on wasm, which has
/// no separate physical file store to reconcile.
pub(crate) fn queue_file_work(transaction: &rusqlite::Transaction<'_>, operation: &str, hash: &BookId, path: &RelativeBookPath) -> Result<(), DatabaseError> {
    #[cfg(target_arch = "wasm32")]
    let _ = (transaction, operation, hash, path);
    #[cfg(not(target_arch = "wasm32"))]
    transaction.shared_placements_queue_file_work(operation, hash, path.as_str())?;
    Ok(())
}

/// The library-relative path of `name` in `dir`: "/book.epub" at the root,
/// "/a/b/book.epub" below it. Fails when the folder is not live.
pub(crate) fn placement_path(connection: &rusqlite::Connection, dir: &DirId, name: &str) -> Result<RelativeBookPath, DatabaseError> {
    FolderPaths::new(connection).placement(&dir.to_string(), name)?.ok_or_else(|| DatabaseError::message("placement directory has no path"))
}

/// Preserves upload provenance across a metadata-only copy/move. No upload work.
pub(crate) fn copy_book_baseline(transaction: &rusqlite::Transaction<'_>, source: &DirId, destination: &DirId, hash: &BookId) -> Result<(), DatabaseError> {
    transaction.placements_copy_book_baseline(&source.to_string(), &destination.to_string(), hash)?;
    Ok(())
}

impl Database {
    pub fn dir_path_components(&self, directory_id: &DirId) -> Result<Vec<DirPathComponent>, DatabaseError> {
        dir_path_components(&self.connection, &directory_id.to_string())
    }

    /// A destination is usable only when its complete ancestry is live and rooted.
    pub fn is_live_destination(&self, directory: &DirId) -> Result<bool, DatabaseError> {
        Ok(directory_relative_path(&self.connection, directory)?.is_some())
    }

    /// Whether a placement still addresses these bytes through live
    /// directories. A move, rename, or deletion that landed mid-transfer
    /// makes the finished bytes belong to a stale path.
    pub fn book_placement_expects_hash(&self, placement: &BookPlacement, content_hash: &BookId) -> Result<bool, DatabaseError> {
        let Some((stored_hash, dir_id, file_name)) = book_at_path(&self.connection, placement.rel_path.as_str())? else {
            return Ok(false);
        };
        if stored_hash != placement.content_hash || stored_hash != *content_hash {
            return Ok(false);
        }
        let mut exists = false;
        self.connection
            .placement_expects_hash(&dir_id.to_string(), content_hash, &file_name, |row| {
                exists = row.get(0)?;
                Ok(())
            })
            .map_err(DatabaseError::operation)?;
        Ok(exists)
    }

    /// The live folder in `parent_id` that a folder named `name` would
    /// collide with, if any.
    pub fn child_directory(&self, parent_id: &DirId, name: &str) -> Result<Option<DirId>, DatabaseError> {
        let mut child = None;
        self.connection.placements_child_directory(&parent_id.to_string(), name, |row| {
            child = Some(row.get::<_, String>(0)?);
            Ok(())
        })?;
        child.map(parse_dir_id).transpose()
    }

    /// Whether the folder already holds a live book under this file name,
    /// compared as file name collisions are.
    pub fn has_book_named(&self, dir_id: &DirId, file_name: &str) -> Result<bool, DatabaseError> {
        Ok(self.connection.placements_has_book_named(&dir_id.to_string(), file_name, |row| row.get(0))?)
    }

    /// Creates and validates a synchronized directory with a fresh id in one
    /// transaction.
    pub fn create_directory(&self, parent_id: &DirId, name: &FileName) -> Result<Dir, DatabaseError> {
        self.create_directory_with_id(&DirId::new_v4(), parent_id, name)
    }

    /// Creates and validates a synchronized directory in one transaction.
    /// An import job may retry the same directory id after losing its worker.
    pub fn create_directory_with_id(&self, dir_id: &DirId, parent_id: &DirId, name: &FileName) -> Result<Dir, DatabaseError> {
        validate_library_directory_name(name)?;
        self.write(|transaction| {
            if *dir_id == sync_common::ROOT_DIR_ID {
                return Err(DatabaseError::message("the library root cannot be created"));
            }
            let state = canonical_directory(transaction, dir_id)?;
            if state.name.as_ref().is_some_and(|existing| existing != name) || state.parent.is_some_and(|existing| existing != *parent_id) || state.lifecycle.is_some_and(|existing| existing != DirectoryLifecycleState::Present) {
                return Err(DatabaseError::message("import directory was changed or deleted"));
            }
            if state.name.is_none() || state.parent.is_none() || state.lifecycle.is_none() {
                if !directory_exists_in(transaction, &parent_id.to_string())? {
                    return Err(DatabaseError::message(format!("parent directory does not exist: {parent_id}")));
                }
                let missing = [
                    state.name.is_none().then(|| LibraryStateEntry::DirectoryName { dir_id: *dir_id, name: name.clone() }),
                    state.parent.is_none().then_some(LibraryStateEntry::DirectoryParent { dir_id: *dir_id, parent_id: *parent_id }),
                    state.lifecycle.is_none().then_some(LibraryStateEntry::DirectoryLifecycle { dir_id: *dir_id, value: DirectoryLifecycleState::Present }),
                ];
                for body in missing.into_iter().flatten() {
                    record_local(transaction, body)?;
                }
            }
            project_dirty(transaction)?;
            directory_relative_path(transaction, dir_id)?.ok_or_else(|| DatabaseError::message("created directory has no path"))?;
            let projected_name = transaction.directory_projected_name(&(dir_id.to_string()), |row| row.get::<_, String>(0))?;
            Ok(Dir::new(*dir_id, projected_name, 0, 0))
        })
    }

    /// Plan a new local folder without writing an intent to SQLite.
    pub fn directory_creation_path(&self, parent_id: &DirId, name: &str) -> Result<(String, String), DatabaseError> {
        validate_library_directory_name(name)?;
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
        self.write(|transaction| {
            if !directory_exists_in(transaction, &parent_id.to_string())? {
                return Err(DatabaseError::message("parent directory does not exist"));
            }
            let parent = directory_relative_path(transaction, parent_id)?.ok_or_else(|| DatabaseError::message("parent directory has no path"))?;
            let expected = if parent.is_empty() { name.to_owned() } else { format!("{parent}/{name}") };
            if path != expected || choose_move_folder_name(transaction, &parent_id.to_string(), &id.to_string(), name)? != name {
                return Err(DatabaseError::message("folder destination changed during creation"));
            }
            upsert_dir(transaction, id, parent_id, name)?;
            transaction.native_set_directory_projection(&id.to_string(), path)?;
            // The caller has already materialized this exact projected directory.
            transaction.directory_clear_work(&(id.to_string()))?;
            Ok(Dir::new(*id, name.to_owned(), 0, 0))
        })
    }

    pub fn folder_destinations(&self) -> Result<Vec<LibraryFolderDestination>, DatabaseError> {
        // Children by parent; the tree is walked from the root, so folders under
        // a deleted one are left out along with it.
        let mut children: HashMap<String, Vec<(String, String)>> = HashMap::new();
        self.connection.placements_live_folders(|row| {
            children.entry(row.get(1)?).or_default().push((row.get(0)?, row.get(2)?));
            Ok(())
        })?;
        let root = sync_common::ROOT_DIR_ID;
        let mut destinations = vec![LibraryFolderDestination { id: root, parent_id: None, label: "Library".to_owned(), path: "Library".to_owned() }];
        let mut pending = vec![(root.to_string(), String::new())];
        let mut seen = HashSet::from([root.to_string()]);
        while let Some((parent, parent_path)) = pending.pop() {
            for (id, name) in children.remove(&parent).unwrap_or_default() {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let path = if parent_path.is_empty() { name.clone() } else { format!("{parent_path} / {name}") };
                destinations.push(LibraryFolderDestination { id: parse_dir_id(id.clone())?, parent_id: Some(parse_dir_id(parent.clone())?), label: name, path: path.clone() });
                pending.push((id, path));
            }
        }
        destinations[1..].sort_by(|left, right| library_replica::portable_name_key(&left.path).cmp(&library_replica::portable_name_key(&right.path)).then_with(|| left.id.cmp(&right.id)));
        Ok(destinations)
    }

    pub fn move_directory(&self, directory_id: &DirId, requested_parent: Option<&DirId>, requested_name: Option<&str>) -> Result<String, DatabaseError> {
        self.write(|transaction| move_directory_in(transaction, directory_id, requested_parent, requested_name))
    }

    /// Duplicates a folder and everything under it beneath `destination_parent_id`,
    /// returning the copy's own directory id — what an undo needs to find what
    /// this call created.
    ///
    /// Books are content-addressed, so a copied book is a second placement of
    /// the same asset rather than a second copy of its bytes.
    pub fn copy_directory(&self, directory_id: &DirId, destination_parent_id: &DirId) -> Result<DirId, DatabaseError> {
        self.write(|transaction| copy_directory_in(transaction, directory_id, destination_parent_id, &mut DirId::new_v4))
    }

    /// Puts a book back in a folder it was removed from, reviving the book
    /// itself if that removal was what sent it to Trash. The tombstoned
    /// placement is reused where it still exists, so the book returns under
    /// the file name it had rather than a fresh unique one.
    pub fn restore_book_placement(&self, content_hash: &BookId, directory_id: &DirId) -> Result<(), DatabaseError> {
        self.write(|transaction| restore_book_placement_in(transaction, content_hash, directory_id, false))
    }

    /// Copies from an explicit folder, or any live placement for a flat listing.
    /// Returns whether a new destination placement was created, for undo.
    pub fn copy_book_to_directory(&self, content_hash: &BookId, source_id: Option<&DirId>, destination_id: &DirId) -> Result<bool, DatabaseError> {
        self.write(|transaction| copy_book_to_directory_in(transaction, content_hash, source_id, destination_id))
    }

    pub fn transfer_book_placement(&self, content_hash: &BookId, source_id: &DirId, destination_id: &DirId, remove_source: bool) -> Result<String, DatabaseError> {
        self.write(|transaction| transfer_book_placement_in(transaction, content_hash, source_id, destination_id, remove_source))
    }

    /// Atomically move a placement and report whether its destination was new.
    pub fn move_book_placement(&self, book: &BookId, source: &DirId, destination: &DirId) -> Result<bool, DatabaseError> {
        self.write(|tx| {
            let created = live_file_name(tx, destination, book)?.is_none();
            transfer_book_placement_in(tx, book, source, destination, true)?;
            Ok(created)
        })
    }

    pub fn remove_book_placement(&self, content_hash: &BookId, source_id: &DirId) -> Result<bool, DatabaseError> {
        self.write(|transaction| remove_book_placement_in(transaction, content_hash, source_id))
    }

    pub fn library_trash(&self) -> Result<LibraryTrashView, DatabaseError> {
        let mut books = Vec::new();
        self.connection.library_trash_select(|row| {
            books.push(TrashedBook {
                content_hash: row.get::<_, BookId>(0)?,
                title: row.get(1)?,
                author: row.get(2)?,
                format: row.get::<_, Option<i64>>(3)?.and_then(BookFormat::from_code).map(|format| format.canonical_extension().to_owned()).unwrap_or_default(),
                deleted_at: row.get(4)?,
            });
            Ok(())
        })?;
        let mut raw = Vec::new();
        self.connection.library_trash_select_2(|row| {
            raw.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, Option<String>>(3)?));
            Ok(())
        })?;
        let mut folders = Vec::new();
        for (id, name, deleted_at, parent_name) in raw {
            folders.push(TrashedFolder { id: parse_dir_id(id)?, name, parent_name, deleted_at });
        }
        Ok(LibraryTrashView { books, folders })
    }

    pub fn trash_book(&self, content_hash: &BookId) -> Result<(), DatabaseError> {
        self.write(|transaction| {
            let live: bool = transaction.directory_live_book_exists(&(&content_hash), |row| row.get(0))?;
            if !live {
                return Err(DatabaseError::message("book does not exist or is already in Trash"));
            }
            record_book_lifecycle(transaction, content_hash, library_replica::BookLifecycleState::Deleted { origin_folder_id: None })?;
            Ok(())
        })
    }

    /// Restore a trashed book using only the database's directory liveness.
    pub fn restore_book(&self, content_hash: &BookId) -> Result<(), DatabaseError> {
        let choices = self
            .restore_book_plan(content_hash)?
            .into_iter()
            .map(|candidate| {
                let target = candidate.ancestry.iter().find(|ancestor| ancestor.live).map(|ancestor| ancestor.id).unwrap_or(sync_common::ROOT_DIR_ID);
                (candidate.original, target)
            })
            .collect::<Vec<_>>();
        self.restore_book_commit(content_hash, &choices)
    }

    pub(crate) fn restore_book_plan(&self, content_hash: &BookId) -> Result<Vec<RestorePlacement>, DatabaseError> {
        if !book_is_trashed(&self.connection, content_hash)? {
            return Err(DatabaseError::message("book is not in Trash"));
        }
        let recoverable = restore_placement_candidates(&self.connection, content_hash)?;
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
                ancestry.push(RestoreAncestor { id: target, live });
                target = parse_dir_id(parent)?;
            }
            plan.push(RestorePlacement { original: parse_dir_id(original)?, file_name: name, ancestry });
        }
        Ok(plan)
    }

    pub(crate) fn restore_book_commit(&self, content_hash: &BookId, choices: &[(DirId, DirId)]) -> Result<(), DatabaseError> {
        self.write(|transaction| {
            if !book_is_trashed(transaction, content_hash)? {
                return Err(DatabaseError::message("book is not in Trash"));
            }
            let recoverable: HashMap<DirId, String> = restore_placement_candidates(transaction, content_hash)?.into_iter().map(|(original, name)| parse_dir_id(original).map(|original| (original, name))).collect::<Result<_, _>>()?;
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
                    remove_placement(transaction, original, content_hash, None)?;
                }
            }
            record_book_lifecycle(transaction, content_hash, library_replica::BookLifecycleState::Present)?;
            project_dirty(transaction)?;
            for (original, target) in choices {
                if *target != sync_common::ROOT_DIR_ID
                    && !{
                        let connection: &rusqlite::Connection = transaction;
                        let dir_id: &DirId = target;
                        let mut directory: Option<(String, bool)> = None;
                        connection.restore_book_record_with_parent_select_4(&dir_id.to_string(), |row| {
                            directory = Some((row.get(0)?, row.get(1)?));
                            Ok(())
                        })?;
                        directory.is_some_and(|(_, live)| live)
                    }
                {
                    return Err(DatabaseError::message("restore target changed; retry the restore"));
                }
                let name = &recoverable[original];
                let existing = live_file_name(transaction, target, content_hash)?;
                if existing.is_none() {
                    let occupied = occupied_file_names_in(transaction, &target.to_string())?;
                    let chosen = library_replica::unique_file_name(name, occupied.iter().map(String::as_str));
                    place_book(transaction, target, content_hash, &chosen)?;
                    project_dirty(transaction)?;
                }
            }
            Ok(())
        })
    }

    /// Permanently removes a book in Trash. Sync intent and retryable file
    /// cleanup survive independently of the deleted database row.
    pub fn purge_book(&self, content_hash: &BookId) -> Result<(), DatabaseError> {
        self.write(|transaction| purge_book_in(transaction, content_hash))
    }

    pub fn trash_directory(&self, directory_id: &DirId) -> Result<(), DatabaseError> {
        self.write(|transaction| trash_directory_in(transaction, directory_id))
    }

    pub fn restore_directory(&self, directory_id: &DirId, requested_parent: Option<&DirId>) -> Result<String, DatabaseError> {
        self.write(|transaction| {
            let state = canonical_directory(transaction, directory_id)?;
            if state.lifecycle != Some(DirectoryLifecycleState::Deleted) {
                return Err(DatabaseError::message("folder is not in Trash"));
            }
            let (Some(stored_parent), Some(name)) = (state.parent, state.name) else {
                return Err(DatabaseError::message("folder state is incomplete; retry after sync"));
            };
            let requested_parent = requested_parent.copied().unwrap_or(stored_parent);
            let parent = if directory_exists_in(transaction, &requested_parent.to_string())? { requested_parent } else { sync_common::ROOT_DIR_ID };
            let mut occupied = Vec::new();
            transaction.placements_sibling_folder_names(&parent.to_string(), &directory_id.to_string(), |row| {
                occupied.push(row.get::<_, String>(0)?);
                Ok(())
            })?;
            let chosen = library_replica::unique_folder_name(&name, occupied.iter().map(String::as_str));
            upsert_dir(transaction, directory_id, &parent, &chosen)?;
            let placements = query_rows(|callback| transaction.directory_placements_trashed_from(&(directory_id.to_string()), callback), |row| Ok((row.get::<_, String>(0)?, row.get::<_, BookId>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            for (directory, hash) in placements {
                record_local(transaction, LibraryStateEntry::Placement { dir_id: parse_dir_id(directory)?, content_hash: hash, present: true, origin_folder_id: None })?;
            }
            let books = query_rows(|callback| transaction.directory_books_trashed_from(&(directory_id.to_string()), callback), |row| row.get::<_, BookId>(0))?.collect::<Result<Vec<_>, _>>()?;
            for hash in &books {
                record_book_lifecycle(transaction, hash, library_replica::BookLifecycleState::Present)?;
            }
            // Restore the complete tree and book projections in the database.
            project_dirty(transaction)?;
            for hash in books {
                let paths = query_rows(|callback| transaction.directory_unobserved_placements(&(&hash), callback), |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                for (dir, name) in paths {
                    if directory_relative_path(transaction, &parse_dir_id(dir.clone())?)?.is_some() {
                        queue_file_work(transaction, "materialize", &hash, &placement_path(transaction, &parse_dir_id(dir)?, &name)?)?;
                    }
                }
            }
            directory_relative_path(transaction, directory_id)?.ok_or_else(|| DatabaseError::message("restored directory has no path"))?;
            let projected_name = transaction.directory_projected_name(&(directory_id.to_string()), |row| row.get::<_, String>(0))?;
            Ok(projected_name)
        })
    }

    /// Permanently deletes the directory subtree and its placements. Canonical
    /// lifecycle tombstones survive independently for synchronization.
    pub fn purge_directory(&self, directory_id: &DirId) -> Result<(), DatabaseError> {
        self.write(|transaction| {
            if canonical_directory(transaction, directory_id)?.lifecycle != Some(DirectoryLifecycleState::Deleted) {
                return Err(DatabaseError::message("folder is not in Trash"));
            }
            // Hidden descendants are detached in the display tree, so the walk
            // follows retained original parents through suppressed folders.
            let directories = trashed_subtree(transaction, vec![directory_id.to_string()])?;
            if directories.is_empty() {
                return Err(DatabaseError::message("folder is not in Trash"));
            }
            for id in directories {
                record_local(transaction, LibraryStateEntry::DirectoryLifecycle { dir_id: parse_dir_id(id)?, value: DirectoryLifecycleState::Purged })?;
            }
            Ok(())
        })
    }

    /// Permanently purges every current Trash entry in one transaction.
    /// Folder purging deliberately does not imply book purging, so both
    /// collections are processed explicitly.
    pub fn empty_trash(&self) -> Result<(), DatabaseError> {
        self.write(|transaction| {
            let mut roots = Vec::new();
            transaction.empty_trash_roots(|row| {
                roots.push(row.get::<_, String>(0)?);
                Ok(())
            })?;
            let directories = trashed_subtree(transaction, roots)?;
            for id in directories {
                record_local(transaction, LibraryStateEntry::DirectoryLifecycle { dir_id: parse_dir_id(id)?, value: DirectoryLifecycleState::Purged })?;
            }
            let mut books = Vec::new();
            transaction.empty_trash_records_select(|row| {
                books.push(row.get::<_, BookId>(0)?);
                Ok(())
            })?;
            for hash in books {
                purge_book_in(transaction, &hash)?;
            }
            Ok(())
        })
    }
}

pub(crate) fn restore_book_placement_in(transaction: &Transaction<'_>, content_hash: &BookId, directory_id: &DirId, materialize_retained: bool) -> Result<(), DatabaseError> {
    if !directory_exists_in(transaction, &directory_id.to_string())? {
        return Err(DatabaseError::message("folder does not exist or is in Trash"));
    }
    let mut previous: Option<String> = None;
    transaction.restore_book_placement_record_select(&directory_id.to_string(), content_hash, |row| {
        previous = row.get(0)?;
        Ok(())
    })?;
    let mut title: Option<String> = None;
    transaction.restore_book_placement_record_select_2(content_hash, |row| {
        title = row.get(0)?;
        Ok(())
    })?;
    let adding = previous.is_none();
    let requested = previous.or(title).ok_or_else(|| DatabaseError::message("book does not exist"))?;
    let mut occupied = Vec::new();
    transaction.restore_book_placement_record_select_3(&directory_id.to_string(), content_hash, |row| {
        occupied.push(row.get::<_, String>(0)?);
        Ok(())
    })?;
    let chosen = library_replica::unique_file_name(&requested, occupied.iter().map(String::as_str));
    record_book_lifecycle(transaction, content_hash, library_replica::BookLifecycleState::Present)?;
    project_dirty(transaction)?;
    place_book(transaction, directory_id, content_hash, &chosen)?;
    project_dirty(transaction)?;
    #[cfg(target_arch = "wasm32")]
    let _ = materialize_retained;
    #[cfg(not(target_arch = "wasm32"))]
    if adding || materialize_retained {
        queue_file_work(transaction, "materialize", content_hash, &placement_path(transaction, directory_id, &chosen)?)?;
    }
    if adding {
        {
            let transaction: &rusqlite::Transaction<'_> = transaction;
            let hash: &BookId = content_hash;
            transaction.shared_request_book_download(hash, TransferOrigin::UserInitiated.storage(), unix_millis()?)?;
        };
    }
    Ok(())
}

pub(crate) fn move_directory_in(transaction: &Transaction<'_>, directory_id: &DirId, requested_parent: Option<&DirId>, requested_name: Option<&str>) -> Result<String, DatabaseError> {
    if let Some(name) = requested_name {
        validate_library_directory_name(name)?;
    }
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
    if !directory_exists_in(transaction, &parent_id.to_string())? {
        return Err(DatabaseError::message("destination folder does not exist"));
    }
    let requested = requested_name.unwrap_or(&current_name);
    let chosen = choose_move_folder_name(transaction, &parent_id.to_string(), &directory_id.to_string(), requested)?;
    let (canonical_parent, canonical_name): (String, String) = transaction.directory_canonical_location(&(directory_id.to_string()), |row| Ok((row.get(0)?, row.get(1)?)))?;
    let parent_changed = requested_parent.is_some() && canonical_parent != parent_id.to_string();
    let name_changed = (requested_name.is_some() || chosen != current_name) && canonical_name != chosen;
    if parent_changed || name_changed || parent_id != current_parent || chosen != current_name {
        let closes_cycle = is_ancestor_or_self(transaction, &directory_id.to_string(), &parent_id.to_string())?;
        if closes_cycle {
            return Err(DatabaseError::message("directory move would create a cycle"));
        }
        // A rename must not adopt a repaired parent; a move must not adopt
        // a numbered display name unless it needs a new name at its destination.
        if parent_changed {
            record_local(transaction, LibraryStateEntry::DirectoryParent { dir_id: *directory_id, parent_id })?;
        }
        if name_changed {
            record_local(transaction, LibraryStateEntry::DirectoryName { dir_id: *directory_id, name: chosen.clone() })?;
        }
        queue_directory_work(transaction, directory_id)?;
    }
    project_dirty(transaction)?;
    // Moving can remove a collision suffix while retaining the name intent.
    let projected_name = transaction.directory_projected_name(&(directory_id.to_string()), |row| row.get::<_, String>(0))?;
    Ok(projected_name)
}

pub(crate) fn copy_directory_in(transaction: &Transaction<'_>, directory_id: &DirId, destination_parent_id: &DirId, next_id: &mut impl FnMut() -> DirId) -> Result<DirId, DatabaseError> {
    if *directory_id == sync_common::ROOT_DIR_ID {
        return Err(DatabaseError::message("the library root cannot be copied"));
    }
    let mut name: Option<String> = None;
    transaction.copy_directory_record_select(&directory_id.to_string(), |row| {
        name = row.get(0)?;
        Ok(())
    })?;
    let name = name.ok_or_else(|| DatabaseError::message("folder does not exist or is in Trash"))?;
    if !directory_exists_in(transaction, &destination_parent_id.to_string())? {
        return Err(DatabaseError::message("destination folder does not exist"));
    }
    // The subtree is read as the copy is written, so a destination inside
    // it would be copying a folder into itself; the same query that
    // rejects a cycle-forming move answers this.
    if is_ancestor_or_self(transaction, &directory_id.to_string(), &destination_parent_id.to_string())? {
        return Err(DatabaseError::message("a folder cannot be copied into itself"));
    }
    let mut occupied = Vec::new();
    transaction.copy_directory_record_select_2(&destination_parent_id.to_string(), |row| {
        occupied.push(row.get::<_, String>(0)?);
        Ok(())
    })?;
    let chosen = library_replica::unique_folder_name(&name, occupied.iter().map(String::as_str));

    let copy_id = next_id();
    let mut pending = vec![(*directory_id, copy_id, chosen, *destination_parent_id)];
    let mut visited = HashSet::new();
    while let Some((source_id, copy_id, copy_name, copy_parent)) = pending.pop() {
        if !visited.insert(source_id) {
            continue;
        }
        upsert_dir(transaction, &copy_id, &copy_parent, &copy_name)?;
        queue_directory_work(transaction, &copy_id)?;
        let mut placements = Vec::new();
        transaction.copy_directory_record_select_3(&source_id.to_string(), |row| {
            placements.push((row.get::<_, BookId>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?));
            Ok(())
        })?;
        for (hash, file_name, downloaded) in placements {
            // Names are already unique within the source folder and the
            // copy is empty, so each placement keeps the name it had.
            place_book(transaction, &copy_id, &hash, &file_name)?;
            project_dirty(transaction)?;
            #[cfg(target_arch = "wasm32")]
            if downloaded {
                transaction.shared_set_book_hash_downloaded(&hash, 1)?;
            }
            #[cfg(not(target_arch = "wasm32"))]
            let _ = downloaded;
            copy_book_baseline(transaction, &source_id, &copy_id, &hash)?;
            #[cfg(not(target_arch = "wasm32"))]
            queue_file_work(transaction, "materialize", &hash, &placement_path(transaction, &copy_id, &file_name)?)?;
        }
        let mut children = Vec::new();
        transaction.copy_directory_record_select_4(&source_id.to_string(), |row| {
            children.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
            Ok(())
        })?;
        for (child_id, child_name) in children {
            pending.push((parse_dir_id(child_id)?, next_id(), child_name, copy_id));
        }
    }
    Ok(copy_id)
}

pub(crate) fn copy_book_to_directory_in(transaction: &Transaction<'_>, content_hash: &BookId, source_id: Option<&DirId>, destination_id: &DirId) -> Result<bool, DatabaseError> {
    if !directory_exists_in(transaction, &destination_id.to_string())? {
        return Err(DatabaseError::message("destination folder does not exist"));
    }
    if live_file_name(transaction, destination_id, content_hash)?.is_some() {
        return Ok(false);
    }
    let source_id = match source_id {
        Some(source) => *source,
        None => {
            let mut source = None;
            transaction.placements_copy_source(content_hash, |row| {
                source = Some(row.get::<_, String>(0)?);
                Ok(())
            })?;
            parse_dir_id(source.ok_or_else(|| DatabaseError::message("book has no live folder placement"))?)?
        }
    };
    transfer_book_placement_in(transaction, content_hash, &source_id, destination_id, false)?;
    Ok(true)
}

pub(crate) fn transfer_book_placement_in(transaction: &Transaction<'_>, content_hash: &BookId, source_id: &DirId, destination_id: &DirId, remove_source: bool) -> Result<String, DatabaseError> {
    if source_id == destination_id {
        return Err(DatabaseError::message("source and destination folders are the same"));
    }
    // Native stores must materialize the requested filesystem placement;
    // a hash-addressed browser store has no separate physical placement.
    #[cfg(not(target_arch = "wasm32"))]
    let old_path = {
        let name = live_file_name(transaction, source_id, content_hash)?.ok_or_else(|| DatabaseError::message("book is not present in the source folder"))?;
        placement_path(transaction, source_id, &name)?
    };
    let mut source: Option<(String, bool)> = None;
    transaction.transfer_book_placement_record_select(&source_id.to_string(), content_hash, |row| {
        source = Some((row.get(0)?, row.get(1)?));
        Ok(())
    })?;
    let (source_name, downloaded): (String, bool) = source.ok_or_else(|| DatabaseError::message("book is not present in the source folder"))?;
    if !directory_exists_in(transaction, &destination_id.to_string())? {
        return Err(DatabaseError::message("destination folder does not exist"));
    }
    let existing = live_file_name(transaction, destination_id, content_hash)?;
    let destination_name = if let Some(existing) = existing {
        existing
    } else {
        let chosen = {
            let connection: &rusqlite::Connection = transaction;
            let destination: &str = &destination_id.to_string();
            let requested: &str = &source_name;
            {
                let mut chosen = None;
                for number in 1..=8 {
                    let candidate = if number == 1 { requested.to_owned() } else { library_replica::numbered_file_name(requested, number) };
                    let mut occupied = false;
                    connection.placements_file_name_occupied(destination, &library_replica::portable_name_key(&candidate), |row| {
                        occupied = row.get(0)?;
                        Ok(())
                    })?;
                    if !occupied {
                        chosen = Some(candidate);
                        break;
                    }
                }
                match chosen {
                    Some(name) => name,
                    None => {
                        let names = occupied_file_names_in(connection, destination)?;
                        library_replica::unique_file_name(requested, names.iter().map(String::as_str))
                    }
                }
            }
        };
        place_book(transaction, destination_id, content_hash, &chosen)?;
        project_dirty(transaction)?;
        #[cfg(target_arch = "wasm32")]
        if downloaded {
            transaction.shared_set_book_hash_downloaded(&content_hash, 1)?;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = downloaded;
        copy_book_baseline(transaction, source_id, destination_id, content_hash)?;
        chosen
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Publish the destination before retiring the source. Only this
        // book's paths need work; no whole-library projection is needed.
        queue_file_work(transaction, "materialize", content_hash, &placement_path(transaction, destination_id, &destination_name)?)?;
        if remove_source && source_id != destination_id {
            queue_file_work(transaction, "remove", content_hash, &old_path)?;
        }
    }
    if remove_source {
        remove_placement(transaction, source_id, content_hash, None)?;
    }
    Ok(destination_name)
}

pub(crate) fn purge_book_in(transaction: &rusqlite::Transaction<'_>, content_hash: &BookId) -> Result<(), DatabaseError> {
    let mut book_row_id: Option<i64> = None;
    transaction.purge_book_record_select(content_hash, |row| {
        book_row_id = row.get(0)?;
        Ok(())
    })?;
    let book_row_id = book_row_id.ok_or_else(|| DatabaseError::message("book is not in Trash"))?;

    // Publish placement tombstones before removing their disposable local
    // rows. Other replicas use these before the Purged lifecycle arrives.
    let directories = query_rows(|callback| transaction.directory_live_book_folders(&book_row_id, callback), |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    for directory in directories {
        remove_placement(transaction, &parse_dir_id(directory)?, content_hash, None)?;
    }
    // Publish the delete intent directly. Projection performs the same physical
    // row deletion for local commands and received lifecycle winners.
    record_book_lifecycle(transaction, content_hash, library_replica::BookLifecycleState::Purged)?;
    Ok(())
}

include_sql!("src/placements/sql/schema/tables.sql");
include_sql!("src/placements/sql/schema/index.sql");
include_sql!("src/placements/sql/schema/views.sql");
include_sql!("src/placements/sql/placements.sql");

// ---- scanner ----
// Database-owned contracts for filesystem discovery.
//
// A scanner receives one immutable inventory snapshot and returns observations.
// It never owns a connection, emits application events, or decides when work
// is retried. The library session owns that orchestration.

pub(crate) use book_model::BookFormat;

#[derive(Clone, Debug)]
pub struct ScanSnapshot {
    pub work_revision: i64,
    pub scan_id: i64,
    pub excluded_paths: Vec<String>,
    pub directories: Vec<ScanKnownDirectory>,
    pub books: Vec<ScanKnownBook>,
    pub placements: Vec<ScanPlacement>,
    pub tombstoned_files: Vec<(DirId, String)>,
}

/// Local placement state captured before filesystem I/O, including rows whose
/// fingerprint is empty (for example, a freshly downloaded book).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanPlacement {
    pub content_hash: BookId,
    pub directory: DirId,
    pub name: String,
    pub path: String,
    pub fingerprint: String,
    pub downloaded: bool,
    pub projected_path: Option<String>,
}

#[cfg(feature = "scanner")]
pub(crate) fn scan_placements(connection: &rusqlite::Connection) -> Result<Vec<ScanPlacement>, DatabaseError> {
    let mut rows = Vec::new();
    connection.scanner_placements(|row| {
        rows.push((row.get::<_, BookId>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get(3)?, row.get(4)?, row.get(5)?));
        Ok(())
    })?;
    // A placement in a folder that is not live has no path on disk to scan.
    let mut folders = FolderPaths::new(connection);
    let mut placements = Vec::new();
    for (content_hash, directory, name, fingerprint, downloaded, projected_path) in rows {
        let Some(folder) = folders.folder(&directory)? else { continue };
        placements.push(ScanPlacement { content_hash, directory: parse_dir_id(directory)?, path: if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") }, name, fingerprint, downloaded, projected_path });
    }
    Ok(placements)
}

#[cfg(feature = "scanner")]
pub(crate) fn scan_path_excluded(snapshot: &ScanSnapshot, path: &str) -> bool {
    let path = Path::new(path.trim_matches('/'));
    snapshot.excluded_paths.iter().any(|excluded| {
        let excluded = Path::new(excluded.trim_matches('/'));
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
    pub content_hash: BookId,
    pub directory: DirId,
    pub name: String,
    pub fingerprint: u64,
    pub inspection_version: Option<i64>,
    /// Complete playback facts for these verified bytes, including facts
    /// received through sync. This does not claim a local inspection occurred.
    pub cached_audiobook_format: Option<BookFormat>,
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
    pub books: Vec<ScannedBook>,
    pub readable: bool,
}

#[derive(Clone, Debug)]
pub struct ScannedBook {
    pub directory: DirId,
    pub name: String,
    pub path: String,
    pub fingerprint: u64,
    pub content_hash: BookId,
    pub size_bytes: u64,
    pub extension: String,
    /// The book's own metadata (title, authors, TOC, ...), extracted the
    /// same way an explicit import extracts it. The scanner omits a book
    /// from a pass entirely rather than including one with no inspection:
    /// a bare `content_hash`/no metadata here would be indistinguishable
    /// from "reuse the cached inspection," which does not exist yet for a
    /// book discovered for the first time.
    pub inspection: book_inspection::InspectedBook,
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
            let mut folders = FolderPaths::new(connection);
            let mut excluded_paths = Vec::new();
            let mut pending_directories = Vec::new();
            let pending = serde_json::to_string(&pending_work_directories(connection)?).map_err(DatabaseError::operation)?;
            connection.scanner_pending_directories(&pending, |row| {
                pending_directories.push((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?));
                Ok(())
            })?;
            for (id, remembered) in pending_directories {
                let live = folders.folder(&id)?;
                excluded_paths.extend(live.into_iter().chain(remembered).map(|path| path.trim_matches('/').to_owned()));
            }
            let mut pending_file_paths = Vec::new();
            connection.scanner_pending_files(|row| {
                pending_file_paths.push(row.get::<_, String>(0)?);
                Ok(())
            })?;
            let mut pending_placements = Vec::new();
            connection.scanner_pending_placements(|row| {
                pending_placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
                Ok(())
            })?;
            for (dir_id, file_name) in pending_placements {
                pending_file_paths.extend(folders.placement(&dir_id, &file_name)?.map(|path| path.to_string()));
            }
            excluded_paths.extend(pending_file_paths.into_iter().map(|path| path.trim_matches('/').to_owned()));
            let mut books = Vec::new();
            connection.scanner_known_books(|row| {
                let Ok(fingerprint) = row.get::<_, String>(0)?.parse::<u64>() else { return Ok(()) };
                let content_hash = row.get::<_, BookId>(1)?;
                let inspection_version = row.get::<_, Option<i64>>(4)?;
                let format = BookFormat::from_code(row.get(5)?);
                let cached_audiobook_format = if inspection_version.is_none() && matches!(format, Some(BookFormat::M4b | BookFormat::Mp3Folder)) {
                    crate::books::audiobook::reusable_playback_format(content_hash, format, row.get(6)?, row.get::<_, Option<Vec<u8>>>(7)?.as_deref(), row.get::<_, Option<Vec<u8>>>(8)?.as_deref())
                } else {
                    None
                };
                books.push((fingerprint, content_hash, row.get::<_, String>(2)?, row.get::<_, String>(3)?, inspection_version, cached_audiobook_format));
                Ok(())
            })?;
            let books = books
                .into_iter()
                .map(|(fingerprint, content_hash, directory, name, inspection_version, cached_audiobook_format)| {
                    Ok(ScanKnownBook { content_hash, directory: DirId::parse_str(&directory).map_err(DatabaseError::operation)?, name, fingerprint, inspection_version, cached_audiobook_format })
                })
                .collect::<Result<Vec<_>, DatabaseError>>()?;
            let mut directories = Vec::new();
            connection.scanner_known_directories(|row| {
                directories.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, bool>(3)?));
                Ok(())
            })?;
            let directories = directories
                .into_iter()
                .map(|(id, parent_id, name, live)| {
                    Ok(ScanKnownDirectory {
                        path: if live { folders.folder(&id)? } else { None },
                        id: DirId::parse_str(&id).map_err(DatabaseError::operation)?,
                        parent_id: DirId::parse_str(&parent_id).map_err(DatabaseError::operation)?,
                        name,
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
            let placements = scan_placements(connection)?;
            ({
                use crate::assets::ActivitySql;

                connection.activity_begin_scan()?;
            });
            Ok(Commit(ScanSnapshot { work_revision, scan_id, excluded_paths, directories, books, placements, tombstoned_files }))
        })
    }

    /// Publish inspected books incrementally. Only the final, readable batch
    /// may reconcile missing files. Refresh the guard inside the transaction
    /// so our writes are accepted but concurrent edits still invalidate it.
    #[cfg(feature = "scanner")]
    pub fn apply_filesystem_scan_batch(
        &self, snapshot: &mut ScanSnapshot, discovery: &FilesystemScan, inspected: &InspectedFilesystemScan, complete: bool, matcher: Option<crate::ContributorNameMatcher>,
    ) -> Result<ScanApplyResult, DatabaseError> {
        // Normalize metadata and serialize inspection data before acquiring
        // the writer. Snapshot validation and identity resolution remain in
        // the transaction so changes during preparation still invalidate it.
        let prepared_books = inspected
            .books
            .iter()
            .map(|book| {
                let format = BookFormat::from_file_name(&format!("book.{}", book.extension)).ok_or_else(|| DatabaseError::message(format!("unsupported scanned extension: {}", book.extension)))?;
                let prepared = crate::import::prepare_inspection(&book.inspection, &book.content_hash, format)?;
                let catalog_record = crate::catalog::import_record(book.content_hash, format)?;
                Ok((format, prepared, catalog_record))
            })
            .collect::<Result<Vec<_>, DatabaseError>>()?;
        let mut next_guard = None;
        let result = self.with_write_transaction(|transaction| {
            let current_revision: i64 = transaction.scanner_file_work_revision(|row| row.get(0))?;
            let mut current_sequence = String::new();
            transaction.shared_scanner_sequence(|row| {
                current_sequence = row.get(0)?;
                Ok(())
            })?;
            if current_revision != snapshot.work_revision || current_sequence != snapshot.scan_id.to_string() || scan_placements(transaction)? != snapshot.placements {
                return Ok(Rollback(ScanApplyResult::Stale));
            }
            for directory in &discovery.directories {
                // Folder paths are derived from dir, so upserting dir is all a
                // new path needs.
                upsert_dir(transaction, &directory.id, &directory.parent_id, &directory.name)?;
            }
            for (book, (format, prepared, catalog_record)) in inspected.books.iter().zip(prepared_books) {
                // Scanned books commit through the same identity-resolution and
                // placement logic explicit import uses, so a scan-discovered book
                // gets its title, authors, and subjects just like an imported one.
                let import = crate::import::ImportCommit {
                    parent_id: book.directory,
                    file_name: book.name.clone(),
                    format,
                    content_hash: book.content_hash,
                    size_bytes: book.size_bytes,
                    inspection_version: book.inspection_version,
                    inspection: None,
                    published: library_model::PublishedStagedBook { name: book.name.clone(), relative_path: book.path.clone(), published: true, fingerprint: Some(book.fingerprint.to_string()) },
                    request_thumbnail: true,
                    restore_paths: Vec::new(),
                };
                // Only scanning may replace the old identity at a path already
                // observed on disk. Ordinary imports retain their collision check.
                for old in snapshot.placements.iter().filter(|old| old.directory == book.directory && old.name == book.name && old.content_hash != book.content_hash) {
                    if scan_path_excluded(snapshot, &old.path) {
                        return Ok(Rollback(ScanApplyResult::Stale));
                    }
                    remove_placement(transaction, &old.directory, &old.content_hash, None)?;
                    transaction.directory_clear_placement_hash(&(old.directory.to_string()), &(&old.content_hash))?;
                    project_dirty(transaction)?;
                    forget_file(transaction, &old.directory.to_string(), &old.content_hash)?;
                }
                crate::import::commit_one(transaction, import, Some(prepared), matcher)?;
                if let Some(record) = catalog_record {
                    crate::enrichment::facts::record_catalog_answer(transaction, &book.content_hash, record)?;
                }
            }
            if complete {
                let known: HashMap<_, _> = snapshot.placements.iter().map(|old| ((old.directory, old.name.as_str()), old)).collect();
                for file in &discovery.files {
                    if let Some(old) = known.get(&(file.directory, file.name.as_str())).filter(|old| old.fingerprint == file.fingerprint.to_string())
                        && (!old.downloaded || old.projected_path.as_deref().map(|path| path.trim_matches('/')) != Some(file.path.trim_matches('/')))
                    {
                        crate::native::observations::record_file(transaction, &file.directory.to_string(), &old.content_hash, &file.path, Some(&old.fingerprint), crate::native::observations::Revision::Retained)?;
                    }
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
                        forget_file(transaction, &old.directory.to_string(), &old.content_hash)?;
                    }
                }
            }
            if complete {
                ({
                    use crate::assets::ActivitySql;
                    let connection: &rusqlite::Connection = transaction;
                    let readable: bool = discovery.readable;

                    connection.activity_finish_scan(if readable { "idle" } else { "failed" })?;
                });
            }
            // Commit-time projection can change placements and their work revision.
            // Include those changes in our next batch guard while still holding the writer.
            project_dirty(transaction)?;
            #[cfg(not(target_arch = "wasm32"))]
            for directory in &discovery.directories {
                if directory_relative_path(transaction, &directory.id)?.as_deref() == Some(directory.path.trim_matches('/')) {
                    use crate::native::NativeFileWorkSql;
                    transaction.native_set_directory_projection(&directory.id.to_string(), &directory.path)?;
                    transaction.directory_clear_work(&(directory.id.to_string()))?;
                }
            }
            next_guard = Some((transaction.scanner_file_work_revision(|row| row.get(0))?, scan_placements(transaction)?));
            Ok(Commit(ScanApplyResult::Applied))
        })?;
        if let Some((revision, placements)) = next_guard {
            snapshot.work_revision = revision;
            snapshot.placements = placements;
        }
        Ok(result)
    }
}

// Only filesystem scanning reads and writes through these queries.
#[cfg(feature = "scanner")]
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
    pub path: Option<RelativeBookPath>,
}

impl Database {
    pub fn pdf_metadata(&self, hash: &BookId) -> Result<Option<pdf_view_common::PdfReaderMetadata>, DatabaseError> {
        let connection = &self.connection;
        let mut bytes = None;
        connection.stored_pdf_metadata(hash, |row| {
            bytes = Some(row.get::<_, Vec<u8>>(0)?);
            Ok(())
        })?;
        Ok(bytes.and_then(|bytes| crate::stored_document::decode::<pdf_view_common::PdfReaderMetadata>(&bytes).ok()).filter(|metadata| metadata.valid_for(&hash.to_hex())))
    }

    pub fn local_book_info(&self, hash: &BookId) -> Result<LocalBookInfo, DatabaseError> {
        let mut profile = StartupProfile::new("book_lookup_info");
        let connection = &self.connection;
        let mut format = None;
        connection.shared_book_format(hash, |row| {
            format = Some(row.get::<_, Option<i64>>(0)?);
            Ok(())
        })?;
        let format = format.flatten().and_then(BookFormat::from_code).ok_or_else(|| DatabaseError::message("book format missing"))?;
        profile.mark("format_sql");
        let mut placements = Vec::new();
        connection.readable_placements(hash, |row| {
            placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?));
            Ok(())
        })?;
        profile.mark("placements_sql");
        // Prefer a placement with local bytes, so the reader opens bytes that
        // are present instead of failing on an arbitrarily chosen empty one.
        let mut folders = FolderPaths::new(connection);
        let mut candidates = Vec::new();
        for (dir_id, file_name, downloaded) in placements {
            if let Some(path) = folders.placement(&dir_id, &file_name)? {
                candidates.push((downloaded, path));
            }
        }
        let path = candidates.into_iter().min_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1))).map(|(_, path)| path);
        profile.mark("folder_paths_and_selection");
        Ok(LocalBookInfo { format, path })
    }

    /// Every remembered native path for a book, including a source
    /// retained while a move is pending.
    pub fn book_paths(&self, hash: &BookId) -> Result<Vec<RelativeBookPath>, DatabaseError> {
        book_paths_in(&self.connection, hash)
    }

    /// Paths that contribute to local book storage usage.
    pub fn local_book_paths(&self) -> Result<Vec<RelativeBookPath>, DatabaseError> {
        let connection = &self.connection;
        let mut projected = Vec::new();
        connection.local_book_projection_paths(|row| {
            projected.push(row.get::<_, String>(0)?);
            Ok(())
        })?;
        let mut paths = projected.iter().map(|path| stored_book_path(path)).collect::<Result<Vec<_>, _>>()?;
        let mut placements = Vec::new();
        connection.local_book_placements(|row| {
            placements.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
            Ok(())
        })?;
        let mut resolver = FolderPaths::new(connection);
        for (dir_id, file_name) in placements {
            paths.extend(resolver.placement(&dir_id, &file_name)?);
        }
        let mut seen = HashSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        Ok(paths)
    }

    /// Load cached PDF metadata for the selected placement without checking the file.
    pub fn local_pdf_metadata(&self, hash: &BookId, relative_path: &str) -> Result<Option<pdf_view_common::PdfReaderMetadata>, DatabaseError> {
        let matches = book_at_path(&self.connection, relative_path)?.is_some_and(|(current, _, _)| current == *hash);
        if !matches {
            return Ok(None);
        }
        self.pdf_metadata(hash)
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

include_sql!("src/placements/sql/book_paths.sql");

fn record_book_lifecycle(connection: &rusqlite::Connection, hash: &BookId, value: library_replica::BookLifecycleState) -> Result<(), DatabaseError> {
    record_local(connection, LibraryStateEntry::BookLifecycle { content_hash: *hash, value })
}

fn remove_placement(connection: &rusqlite::Connection, directory: &DirId, hash: &BookId, origin_folder_id: Option<String>) -> Result<(), DatabaseError> {
    connection.directory_clear_placement_download(&(directory.to_string()), &(&hash))?;
    record_local(connection, LibraryStateEntry::Placement { dir_id: *directory, content_hash: *hash, present: false, origin_folder_id })
}

include_sqlite_sql::include_sql!("src/placements/sql/directory_operations.sql");

/// Transaction-scoped source removal shared with cross-library moves.
pub(crate) fn remove_book_placement_in(transaction: &rusqlite::Transaction<'_>, content_hash: &BookId, source_id: &DirId) -> Result<bool, DatabaseError> {
    let mut exists = false;
    transaction.remove_book_placement_record_select(&source_id.to_string(), content_hash, |row| {
        exists = row.get(0)?;
        Ok(())
    })?;
    if !exists {
        return Err(DatabaseError::message("book is not present in this folder"));
    }
    let mut orphaned = false;
    transaction.remove_book_placement_record_select_2(content_hash, &source_id.to_string(), |row| {
        orphaned = row.get(0)?;
        Ok(())
    })?;
    if orphaned {
        // Removing the last copy trashes the book, retaining its placement
        // so restore does not have to reconstruct membership from clocks.
        record_book_lifecycle(transaction, content_hash, library_replica::BookLifecycleState::Deleted { origin_folder_id: Some(source_id.to_string()) })?;
    } else {
        remove_placement(transaction, source_id, content_hash, None)?;
    }
    Ok(orphaned)
}

/// Transaction-scoped source removal shared with cross-library moves.
pub(crate) fn trash_directory_in(transaction: &rusqlite::Transaction<'_>, directory_id: &DirId) -> Result<(), DatabaseError> {
    if *directory_id == sync_common::ROOT_DIR_ID {
        return Err(DatabaseError::message("the library root cannot be moved to Trash"));
    }
    if !directory_exists_in(transaction, &directory_id.to_string())? {
        return Err(DatabaseError::message("folder does not exist or is already in Trash"));
    }
    let subtree = serde_json::to_string(
        &({
            let connection: &rusqlite::Connection = transaction;
            let dir_id: &DirId = directory_id;
            walk_down(connection, vec![dir_id.to_string()], false)?
        }),
    )
    .map_err(DatabaseError::operation)?;
    let mut hashes = Vec::new();
    transaction.trash_subtree_only_books(&subtree, |row| {
        hashes.push(row.get::<_, BookId>(0)?);
        Ok(())
    })?;
    for hash in hashes {
        record_book_lifecycle(transaction, &hash, library_replica::BookLifecycleState::Deleted { origin_folder_id: Some(directory_id.to_string()) })?;
    }
    // Only books still present elsewhere need independent placement removals.
    project_dirty(transaction)?;
    let removed = query_rows(|callback| transaction.directory_live_subtree_placements(&(&subtree), callback), |row| Ok((row.get::<_, String>(0)?, row.get::<_, BookId>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
    for (directory, hash) in removed {
        remove_placement(transaction, &parse_dir_id(directory)?, &hash, Some(directory_id.to_string()))?;
    }
    record_local(transaction, LibraryStateEntry::DirectoryLifecycle { dir_id: *directory_id, value: DirectoryLifecycleState::Deleted })?;
    Ok(())
}
