use super::*;
use sync_common::{DirId, ROOT_DIR_ID};
// Native asset-store projections. The shared model owns paths;
// these snapshots remember which physical paths this device has materialized.
pub(super) fn projection_error(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> AssetStoreError {
    AssetStoreError::operation(error)
}

pub(in super::super) fn readable_book_paths(handle: &Handle, hash: &ContentHash) -> Result<Vec<PathBuf>, AssetStoreError> {
    let Some(lookup) = handle.3.as_ref() else {
        return Ok(Vec::new());
    };
    // Include remembered source paths while a move is pending. The caller's
    // database adapter supplies the snapshot; this module only resolves files.
    let mut paths = lookup.paths(hash)?.into_iter().map(|relative| placement_path(handle, &relative)).collect::<Result<Vec<_>, _>>()?;
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    Ok(paths)
}

fn stored_book_path(handle: &Handle, hash: &ContentHash) -> Result<Option<PathBuf>, AssetStoreError> {
    Ok(readable_book_paths(handle, hash)?.into_iter().find(|path| path.is_file()))
}

pub(super) fn readable_path(handle: &Handle, name: String) -> Result<Option<PathBuf>, AssetStoreError> {
    if !enabled(handle) {
        return Ok(None);
    }
    if let Some(hash) = name.strip_prefix("book/") {
        let hash = hash.parse::<ContentHash>().map_err(projection_error)?;
        return stored_book_path(handle, &hash);
    }
    let path = path(handle, name);
    Ok(path.is_file().then_some(path))
}

pub(in super::super) fn evict_book_files(handle: &Handle, relative_paths: &[String], hash: &ContentHash) -> Result<(), AssetStoreError> {
    for relative in relative_paths {
        let path = placement_path(handle, relative)?;
        if (path.is_file() || path.is_dir()) && content_matches(&path, hash)? {
            if path.is_dir() {
                for track in audiobook_folder::discover(&path)? {
                    std::fs::remove_file(&track.path)?;
                    if let Some(parent) = track.path.parent().filter(|parent| *parent != path.as_path()) {
                        let _ = std::fs::remove_dir(parent);
                    }
                }
                let _ = std::fs::remove_dir(&path);
            } else {
                std::fs::remove_file(&path)?;
            }
            sync_parent(&path)?;
        }
    }
    Ok(())
}

// Inspect slow storage before acquiring SQLite's writer. Final book only
// checks the inspected entries' metadata, then performs the required writes.
#[derive(PartialEq)]
struct DirectoryEntryStamp {
    directory: bool,
    symlink: bool,
    length: u64,
    modified: std::time::SystemTime,
}

fn directory_entry_stamp(path: &Path) -> Result<Option<DirectoryEntryStamp>, AssetStoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(DirectoryEntryStamp { directory: metadata.is_dir(), symlink: metadata.file_type().is_symlink(), length: metadata.len(), modified: metadata.modified()? })),
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

struct PreparedDirectory {
    id: DirId,
    relative: String,
    old: Option<String>,
    chosen: Option<String>,
    destination: PathBuf,
    source: Option<PathBuf>,
    inspected: Vec<(PathBuf, Option<DirectoryEntryStamp>)>,
    exists: bool,
    missing_external: bool,
}

impl PreparedDirectory {
    fn inspect_snapshot(handle: &Handle, snapshot: &library_database::NativeDirectorySnapshot) -> Result<Option<Self>, AssetStoreError> {
        let relative = snapshot.relative_path.clone();
        let old = snapshot.projected_path.clone();
        let mut destination = placement_path(handle, &relative)?;
        let mut inspected = Vec::new();
        let mut record = |path: &Path| -> Result<bool, AssetStoreError> {
            let stamp = directory_entry_stamp(path)?;
            let exists = stamp.is_some();
            inspected.push((path.to_owned(), stamp));
            Ok(exists)
        };
        let mut exists = record(&destination)?;
        let marker = destination.join(".biblos_uuid");
        let marker_exists = record(&marker)?;
        let owned = marker_exists && std::fs::read_to_string(&marker)?.trim() == snapshot.id.to_string();
        let chosen = if exists && !owned {
            let mut occupied = std::fs::read_dir(destination.parent().unwrap())?.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>()?;
            occupied.extend(snapshot.sibling_names.iter().cloned());
            let chosen = library_replica::unique_folder_name(&snapshot.name, occupied.iter().map(String::as_str));
            destination.set_file_name(&chosen);
            exists = record(&destination)?;
            if exists {
                return Err(AssetStoreError::operation("folder destination changed during inspection"));
            }
            Some(chosen)
        } else {
            None
        };
        let missing_external = old.as_deref() == Some(relative.as_str()) && !exists && chosen.is_none();
        let source = if !exists && !missing_external {
            if let Some(old) = old.as_ref().filter(|old| *old != &relative || chosen.is_some()) {
                let source = placement_path(handle, &old)?;
                let source_exists = record(&source)?;
                if source_exists && source.is_dir() {
                    let marker = source.join(".biblos_uuid");
                    record(&marker)?;
                    if std::fs::read_to_string(&marker)?.trim() != snapshot.id.to_string() {
                        return Err(AssetStoreError::operation("folder identity changed before materialization"));
                    }
                    Some(source)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        Ok(Some(Self { id: snapshot.id, relative, old, chosen, destination, source, inspected, exists, missing_external }))
    }

    /// Performs only physical mutations after rechecking filesystem stamps.
    fn publish_files(&self) -> Result<bool, AssetStoreError> {
        for (path, stamp) in &self.inspected {
            if directory_entry_stamp(path)? != *stamp {
                return Ok(false);
            }
        }
        if self.missing_external {
            return Ok(true);
        }
        if let Some(source) = &self.source {
            std::fs::rename(source, &self.destination)?;
            sync_parent(source)?;
            sync_parent(&self.destination)?;
        } else if !self.exists {
            std::fs::create_dir(&self.destination)?;
            sync_parent(&self.destination)?;
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(self.destination.join(".biblos_uuid"))?;
            file.write_all(self.id.to_string().as_bytes())?;
            file.sync_all()?;
        }
        Ok(true)
    }
}

/// Vacate managed sources before publishing a batch of resolved destinations.
/// The stable hidden path also lets replay recover a rename interrupted before
/// its database acknowledgement. Never overwrite an unrelated physical entry.
pub(in super::super) fn stage_directory_with_database(handle: &Handle, database: &library_database::Database, id: DirId) -> Result<(), AssetStoreError> {
    let snapshot = database.native_directory_snapshot(&id).map_err(projection_error)?;
    let Some(old) = database.native_directory_projected_path(&id).map_err(projection_error)? else { return Ok(()) };
    let staged = format!("/.biblos-move-{id}");
    if old == staged {
        return Ok(());
    }
    let source = placement_path(handle, &old)?;
    let destination = placement_path(handle, &staged)?;
    let source_exists = directory_entry_stamp(&source)?.is_some();
    let destination_exists = directory_entry_stamp(&destination)?.is_some();
    let owned = |path: &Path| -> Result<bool, AssetStoreError> {
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(metadata.is_dir() && !metadata.file_type().is_symlink() && std::fs::read_to_string(path.join(".biblos_uuid"))?.trim() == id.to_string())
    };
    if destination_exists {
        if source_exists || !owned(&destination)? {
            return Err(AssetStoreError::operation("directory staging destination is occupied"));
        }
    } else if snapshot.as_ref().is_none_or(|snapshot| old == snapshot.relative_path) {
        return Ok(());
    } else if source_exists {
        if !owned(&source)? {
            return Err(AssetStoreError::operation("folder identity changed before staging"));
        }
        std::fs::rename(&source, &destination)?;
        sync_parent(&source)?;
        sync_parent(&destination)?;
    } else {
        return Ok(());
    }
    database.acknowledge_native_directory_staging(&id, &old, &staged).map_err(projection_error)
}

/// Prepares one directory with connection-free filesystem work.  Queue
/// processing composes this primitive for every affected directory.
pub(in super::super) async fn prepare_directory_with_database(handle: &Handle, database: &library_database::Database, id: DirId) -> Result<(), AssetStoreError> {
    if id == ROOT_DIR_ID {
        return Ok(());
    }
    if !root_available(handle) {
        return Err(AssetStoreError::operation("library storage is unavailable"));
    }
    loop {
        let Some(snapshot) = database.native_directory_snapshot(&id).map_err(projection_error)? else { return Ok(()) };
        let Some(prepared) = PreparedDirectory::inspect_snapshot(handle, &snapshot)? else { return Ok(()) };
        if !prepared.publish_files()? {
            continue;
        }
        let completion = library_database::NativeDirectoryCompletion { snapshot, renamed_to: prepared.chosen.clone(), materialized: !prepared.missing_external };
        match database.acknowledge_native_directory(completion).map_err(projection_error)? {
            library_database::NativeDirectoryAcknowledgement::Applied | library_database::NativeDirectoryAcknowledgement::Obsolete => return Ok(()),
            library_database::NativeDirectoryAcknowledgement::Retry => continue,
        }
    }
}

#[cfg(test)]
mod prepared_directory_tests {
    use super::*;

    #[tokio::test]
    async fn database_snapshot_prepares_a_renamed_directory_without_holding_its_connection() {
        let root = tempfile::tempdir().unwrap();
        let database = library_database::Database::open(root.path().join("library.db")).unwrap();
        database.initialize_library().unwrap();
        let handle = open(root.path().to_str().unwrap()).unwrap();
        let id = database.create_directory(&ROOT_DIR_ID, &"Source".to_owned()).unwrap().id;
        prepare_directory_with_database(&handle, &database, id).await.unwrap();
        std::fs::write(root.path().join("Source/notes.txt"), b"preserved").unwrap();
        database.move_directory(&id, None, Some("Destination")).unwrap();
        prepare_directory_with_database(&handle, &database, id).await.unwrap();
        assert_eq!(std::fs::read(root.path().join("Destination/notes.txt")).unwrap(), b"preserved");
        assert_eq!(database.directory_relative_path_string(&id).unwrap(), Some("Destination".to_owned()));
    }
}

/// Create a new folder before its database record. Never replace an existing
/// path; even an empty unregistered folder belongs to the user.
pub(in super::super) fn create_directory(handle: &Handle, relative: &str, id: DirId) -> Result<String, AssetStoreError> {
    use std::io::Write;
    let requested = placement_path(handle, relative)?;
    let parent = requested.parent().ok_or_else(|| AssetStoreError::operation("folder has no parent"))?;
    let name = requested.file_name().and_then(|name| name.to_str()).ok_or_else(|| AssetStoreError::operation("invalid folder name"))?;
    let mut occupied = std::fs::read_dir(parent)?.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>()?;
    loop {
        let chosen = library_replica::unique_folder_name(name, occupied.iter().map(String::as_str));
        let destination = parent.join(&chosen);
        match std::fs::create_dir(&destination) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                occupied.push(chosen);
                continue;
            }
            result => result?,
        }
        // If marker creation fails, leave the directory for discovery rather
        // than risk removing contents created concurrently by another process.
        let mut marker = std::fs::OpenOptions::new().write(true).create_new(true).open(destination.join(".biblos_uuid"))?;
        marker.write_all(id.to_string().as_bytes())?;
        marker.sync_all()?;
        sync_parent(&destination.join(".biblos_uuid"))?;
        sync_parent(&destination)?;
        return Ok(chosen);
    }
}
