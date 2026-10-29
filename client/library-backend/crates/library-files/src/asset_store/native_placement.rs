//! Concrete native placement operations; inputs are selected by the caller.

use super::{AssetStore, AssetStoreError, imp};
use client_platform_native::filesystem::parent_directory;
use library_model::RelativeBookPath;
use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use sync_common::{BookId, DirId};

fn folder_unavailable() -> AssetStoreError {
    AssetStoreError::operation(client_runtime::BackendError::classified(client_runtime::Failure::LibraryFolderUnavailable, "placement folder is unavailable"))
}

pub struct PreparedPlacement(imp::PreparedPlacement);

impl PreparedPlacement {
    pub fn renames_source(&self) -> bool {
        matches!(self.0, imp::PreparedPlacement::Renamed { .. })
    }
    pub fn publish(self) -> Result<(), AssetStoreError> {
        self.0.publish()
    }

    /// Transactional publication must never fall back to a slow copy.
    pub fn publish_short(self) -> Result<(), AssetStoreError> {
        self.0.publish_short()
    }
}

pub struct PreparedDirectory(imp::projection::PreparedDirectory);

pub struct DirectoryPlacement {
    pub renamed_to: Option<String>,
    pub materialized: bool,
}

impl PreparedDirectory {
    /// Interactive commands have already chosen a database name. Never create
    /// a differently named folder and then reject its logical commit.
    pub fn publish_exact(self) -> Result<Option<DirectoryPlacement>, AssetStoreError> {
        if self.0.chosen.is_some() {
            return Err(AssetStoreError::operation("destination folder is occupied; please try again"));
        }
        self.publish()
    }

    /// None means an inspected filesystem entry changed; inspect again.
    pub fn publish(self) -> Result<Option<DirectoryPlacement>, AssetStoreError> {
        if !self.0.publish()? {
            return Ok(None);
        }
        Ok(Some(DirectoryPlacement { renamed_to: self.0.chosen, materialized: !self.0.missing_external }))
    }
}

impl AssetStore {
    /// Prepare a short, same-directory source rename. The owner records replay
    /// intent before publishing it, so an interrupted long move is recoverable.
    pub fn prepare_move_source_staging(&self, hash: &BookId, source: &RelativeBookPath) -> Result<Option<(PreparedPlacement, RelativeBookPath)>, AssetStoreError> {
        let path = imp::placement_path(&self.0, source);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let source_file = imp::VerifiedPath::new(path, hash)?;
        let staged = source.with_file_name(&format!(".bokheim-move-{}", uuid::Uuid::new_v4())).map_err(AssetStoreError::operation)?;
        let destination = imp::placement_path(&self.0, &staged);
        Ok(Some((PreparedPlacement(imp::PreparedPlacement::Renamed { source: source_file, destination }), staged)))
    }

    pub fn create_directory(&self, relative: &str, id: DirId) -> Result<String, AssetStoreError> {
        imp::create_directory(&self.0, relative, id)
    }

    pub fn root_available(&self) -> bool {
        imp::root_available(&self.0)
    }

    /// Notify once when access detects a missing or replaced storage root.
    /// A recovered store installs a new callback for its own storage identity.
    pub fn on_root_unavailable(&self, callback: impl FnOnce() + Send + 'static) {
        *self.0.root_failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(callback));
    }

    pub fn placement_conflicts(&self, hash: &BookId, relative: &RelativeBookPath) -> Result<bool, AssetStoreError> {
        imp::placement_conflicts(&self.0, hash, relative)
    }

    /// Verify that retiring one path will leave a live local copy available.
    pub fn verified_placement_exists(&self, hash: &BookId, paths: &[RelativeBookPath]) -> Result<bool, AssetStoreError> {
        for relative in paths {
            let path = imp::placement_path(&self.0, relative);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if !metadata.file_type().is_symlink() => {
                    if imp::content_matches(&path, hash)? {
                        return Ok(true);
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }

    pub fn prepare_removal(&self, hash: &BookId, relative: &RelativeBookPath) -> Result<PreparedPlacement, AssetStoreError> {
        imp::prepare_removal(&self.0, hash, relative).map(PreparedPlacement)
    }

    pub fn prepare_placement(&self, hash: &BookId, relative: &RelativeBookPath, source_paths: &[RelativeBookPath], movable_sources: &[RelativeBookPath]) -> Result<Option<PreparedPlacement>, AssetStoreError> {
        let destination = imp::placement_path(&self.0, relative);
        self.prepare_placement_at(hash, destination, source_paths, movable_sources)
    }

    fn prepare_placement_at(&self, hash: &BookId, destination: PathBuf, source_paths: &[RelativeBookPath], movable_sources: &[RelativeBookPath]) -> Result<Option<PreparedPlacement>, AssetStoreError> {
        use super::native::{VerifiedPath, content_matches, placement_path};
        let handle = &self.0;
        use std::io::Write as _;
        if destination.exists() {
            return Ok(Some(PreparedPlacement(imp::PreparedPlacement::Existing(VerifiedPath::new(destination, hash)?))));
        }
        let mut found = None;
        for relative in source_paths {
            let path = placement_path(handle, relative);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if !metadata.file_type().is_symlink() && (metadata.is_file() || metadata.is_dir()) => {
                    if content_matches(&path, hash)? {
                        found = Some((relative, path));
                        break;
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        // Only a source with a queued removal job for this path is safe to move.
        if let Some((source_relative, source)) = &found {
            if movable_sources.iter().any(|movable| movable == *source_relative) {
                if !parent_directory(&destination)?.is_dir() {
                    return Err(folder_unavailable());
                }
                return Ok(Some(PreparedPlacement(imp::PreparedPlacement::Renamed { source: VerifiedPath::new(source.clone(), hash)?, destination })));
            }
        }
        let Some((_, source)) = found else { return Ok(None) };
        if !parent_directory(&destination)?.is_dir() {
            return Err(folder_unavailable());
        }
        if source.is_dir() {
            let output = tempfile::Builder::new().prefix(".bokheim-copy-").tempdir_in(parent_directory(&destination)?)?;
            let copy_member = |path: &Path| -> Result<(), AssetStoreError> {
                let local = path.strip_prefix(&source).map_err(AssetStoreError::operation)?;
                let target = output.path().join(local);
                std::fs::create_dir_all(parent_directory(&target)?)?;
                std::fs::copy(path, target)?;
                Ok(())
            };
            for track in audiobook_folder::discover(&source)? {
                copy_member(&track.path)?;
            }
            for sidecar in [audiobook_folder::discover_nfo(&source)?, audiobook_folder::discover_cue(&source)?, audiobook_folder::discover_cover(&source)?].into_iter().flatten() {
                copy_member(&sidecar.path)?;
            }
            audiobook_folder::copy_path_mapping(&source, output.path())?;
            if !content_matches(output.path(), hash)? {
                return Err(AssetStoreError::operation("audiobook changed during copy"));
            }
            return Ok(Some(PreparedPlacement(imp::PreparedPlacement::CopiedFolder { output, destination })));
        }
        let mut output = tempfile::Builder::new().prefix(".bokheim-copy-").tempfile_in(parent_directory(&destination)?)?;
        let mut input = File::open(source)?;
        std::io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.as_file().sync_all()?;
        // Verify the staged bytes too: an external editor can change the source
        // between source discovery and copying it.
        if !content_matches(output.path(), hash)? {
            return Err(AssetStoreError::operation("book changed during copy"));
        }
        Ok(Some(PreparedPlacement(imp::PreparedPlacement::Copied { output, destination })))
    }

    /// Build a complete copied tree under one hidden root. Publication then
    /// needs only one rename, regardless of the number or size of its books.
    pub fn prepare_directory_copy(&self, root: &str, directories: &[(DirId, String)], books: &[(BookId, RelativeBookPath, Vec<RelativeBookPath>)]) -> Result<(PreparedPlacement, Vec<String>), AssetStoreError> {
        use std::io::Write as _;
        let destination = imp::library_path(&self.0, root);
        let output = tempfile::Builder::new().prefix(".bokheim-copy-").tempdir_in(parent_directory(&destination)?)?;
        for (id, relative) in directories {
            let suffix = Path::new(relative.trim_start_matches('/')).strip_prefix(root.trim_start_matches('/')).map_err(AssetStoreError::operation)?;
            let directory = output.path().join(suffix);
            std::fs::create_dir_all(&directory)?;
            let mut marker = File::create(directory.join(".biblos_uuid"))?;
            marker.write_all(id.to_string().as_bytes())?;
            marker.sync_all()?;
        }
        let mut materialized = Vec::new();
        for (hash, relative, sources) in books {
            let suffix = Path::new(relative.as_str().trim_start_matches('/')).strip_prefix(root.trim_start_matches('/')).map_err(AssetStoreError::operation)?;
            if let Some(prepared) = self.prepare_placement_at(hash, output.path().join(suffix), sources, &[])? {
                prepared.publish_short()?;
                materialized.push(relative.to_string());
            }
        }
        for (_, relative) in directories.iter().rev() {
            let suffix = Path::new(relative.trim_start_matches('/')).strip_prefix(root.trim_start_matches('/')).map_err(AssetStoreError::operation)?;
            client_platform_native::filesystem::sync_directory(&output.path().join(suffix))?;
        }
        Ok((PreparedPlacement(imp::PreparedPlacement::CopiedFolder { output, destination }), materialized))
    }

    pub fn inspect_directory(&self, id: DirId, relative: &str, projected: Option<&str>, name: &str, siblings: &[String]) -> Result<PreparedDirectory, AssetStoreError> {
        imp::projection::PreparedDirectory::inspect(&self.0, id, relative, projected, name, siblings).map(PreparedDirectory)
    }

    /// Move a managed directory aside, or recover an interrupted staging move.
    pub fn stage_directory(&self, id: DirId, old: &str, desired: Option<&str>) -> Result<Option<String>, AssetStoreError> {
        use super::native::projection::directory_entry_stamp;
        use super::native::{library_path, sync_parent};
        let handle = &self.0;
        let staged = format!("/.biblos-move-{id}");
        if old == staged {
            return Ok(None);
        }
        let source = library_path(handle, old);
        let destination = library_path(handle, &staged);
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
        } else if desired.is_none_or(|desired| old == desired) {
            return Ok(None);
        } else if source_exists {
            if !owned(&source)? {
                return Err(AssetStoreError::operation("folder identity changed before staging"));
            }
            std::fs::rename(&source, &destination)?;
            sync_parent(&source)?;
            sync_parent(&destination)?;
        } else {
            return Ok(None);
        }
        Ok(Some(staged))
    }
}
