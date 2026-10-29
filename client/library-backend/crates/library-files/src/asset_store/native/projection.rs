pub(crate) use super::*;
use client_platform_native::filesystem::parent_directory;
use std::fs::OpenOptions;
use std::io::ErrorKind;
pub(crate) use sync_common::DirId;
// Native asset-store projections. The shared model owns paths;
// these snapshots remember which physical paths this device has materialized.
pub(crate) fn readable_path(handle: &Handle, name: String) -> Result<Option<PathBuf>, AssetStoreError> {
    if !handle.enabled {
        return Ok(None);
    }
    if name.starts_with("book/") {
        return Err(AssetStoreError::operation("native books require explicit paths"));
    }
    let path = handle.assets_root.join(name);
    if path.is_file() {
        return Ok(Some(path));
    }
    root_available(handle);
    Ok(None)
}

pub(crate) fn evict_book_files(handle: &Handle, relative_paths: &[RelativeBookPath], hash: &BookId) -> Result<(), AssetStoreError> {
    for relative in relative_paths {
        let path = placement_path(handle, relative);
        if (path.is_file() || path.is_dir()) && content_matches(&path, hash)? {
            if path.is_dir() {
                let sidecars = [audiobook_folder::discover_nfo(&path)?, audiobook_folder::discover_cue(&path)?, audiobook_folder::discover_cover(&path)?].into_iter().flatten().collect::<Vec<_>>();
                for track in audiobook_folder::discover(&path)? {
                    std::fs::remove_file(&track.path)?;
                    if let Some(parent) = track.path.parent().filter(|parent| *parent != path.as_path()) {
                        let _ = std::fs::remove_dir(parent);
                    }
                }
                for sidecar in sidecars {
                    std::fs::remove_file(sidecar.path)?;
                }
                let mapping = path.join(audiobook_folder::PATH_MAPPING_FILE);
                if mapping.try_exists()? {
                    std::fs::remove_file(mapping)?;
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
pub(crate) struct DirectoryEntryStamp {
    pub(crate) directory: bool,
    pub(crate) symlink: bool,
    pub(crate) length: u64,
    pub(crate) modified: std::time::SystemTime,
}

pub(crate) fn directory_entry_stamp(path: &Path) -> Result<Option<DirectoryEntryStamp>, AssetStoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(DirectoryEntryStamp { directory: metadata.is_dir(), symlink: metadata.file_type().is_symlink(), length: metadata.len(), modified: metadata.modified()? })),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) struct PreparedDirectory {
    pub(crate) id: DirId,
    pub(crate) chosen: Option<String>,
    pub(crate) destination: PathBuf,
    pub(crate) source: Option<PathBuf>,
    pub(crate) inspected: Vec<(PathBuf, Option<DirectoryEntryStamp>)>,
    pub(crate) exists: bool,
    pub(crate) missing_external: bool,
}

impl PreparedDirectory {
    pub(crate) fn inspect(handle: &Handle, id: DirId, relative: &str, projected: Option<&str>, name: &str, sibling_names: &[String]) -> Result<Self, AssetStoreError> {
        let relative = relative.to_owned();
        let old = projected.map(str::to_owned);
        let mut destination = library_path(handle, &relative);
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
        let owned = marker_exists && std::fs::read_to_string(&marker)?.trim() == id.to_string();
        let chosen = if exists && !owned {
            let mut occupied = std::fs::read_dir(parent_directory(&destination)?)?.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>()?;
            occupied.extend(sibling_names.iter().cloned());
            let chosen = library_replica::unique_folder_name(name, occupied.iter().map(String::as_str));
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
                let source = library_path(handle, old);
                let source_exists = record(&source)?;
                if source_exists && source.is_dir() {
                    let marker = source.join(".biblos_uuid");
                    record(&marker)?;
                    if std::fs::read_to_string(&marker)?.trim() != id.to_string() {
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
        Ok(Self { id, chosen, destination, source, inspected, exists, missing_external })
    }
}

impl PreparedDirectory {
    pub(crate) fn publish(&self) -> Result<bool, AssetStoreError> {
        for (path, stamp) in &self.inspected {
            if directory_entry_stamp(path)? != *stamp {
                return Ok(false);
            }
        }
        if self.missing_external {
            return Ok(true);
        }
        if let Some(source) = &self.source {
            crate::filesystem::rename_entry(source, &self.destination)?;
            sync_parent(source)?;
            sync_parent(&self.destination)?;
        } else if !self.exists {
            std::fs::create_dir(&self.destination)?;
            sync_parent(&self.destination)?;
            use std::io::Write as _;
            let mut file = OpenOptions::new().write(true).create_new(true).open(self.destination.join(".biblos_uuid"))?;
            file.write_all(self.id.to_string().as_bytes())?;
            file.sync_all()?;
        }
        Ok(true)
    }
}

/// Create a new folder before its database record. Never replace an existing
/// path; even an empty unregistered folder belongs to the user.
pub(crate) fn create_directory(handle: &Handle, relative: &str, id: DirId) -> Result<String, AssetStoreError> {
    use std::io::Write;
    let requested = library_path(handle, relative);
    let parent = requested.parent().ok_or_else(|| AssetStoreError::operation("folder has no parent"))?;
    let name = requested.file_name().and_then(|name| name.to_str()).ok_or_else(|| AssetStoreError::operation("invalid folder name"))?;
    let occupied = std::fs::read_dir(parent)?.map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>()?;
    for chosen in library_replica::folder_name_candidates(name, occupied.iter().map(String::as_str)) {
        let destination = parent.join(&chosen);
        match std::fs::create_dir(&destination) {
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                continue;
            }
            result => result?,
        }
        // If marker creation fails, leave the directory for discovery rather
        // than risk removing contents created concurrently by another process.
        let mut marker = OpenOptions::new().write(true).create_new(true).open(destination.join(".biblos_uuid"))?;
        marker.write_all(id.to_string().as_bytes())?;
        marker.sync_all()?;
        sync_parent(&destination.join(".biblos_uuid"))?;
        sync_parent(&destination)?;
        return Ok(chosen);
    }
    unreachable!("numeric name suffix space is unbounded")
}
