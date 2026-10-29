//! Browser selected-folder imports: each file is opened from its source,
//! staged in browser storage, and finished through the shared import path.

use std::collections::HashMap;
use std::time::Duration;

use client_platform_runtime::executor::sleep;
use client_runtime::BackendError;
use sync_common::DirId;

use crate::{DirectoryImport, ImportFailure, LibraryImport};

impl LibraryImport {
    pub async fn import_selected_directory(&self, parent_id: DirId, directory: DirectoryImport, create_root: bool, cancelled: &dyn Fn() -> bool) -> Result<Vec<ImportFailure>, BackendError> {
        if cancelled() {
            return Err(BackendError::message("directory import cancelled"));
        }
        let DirectoryImport { name, directories, mut files } = directory;
        let prepared = self.prepare_directory_import(&parent_id, create_root.then_some(name), directories).await?;
        let parents = prepared.directories;
        let mut failures = prepared.failures;
        files.retain(|file| file.path.last().is_some_and(|name| book_model::BookFormat::from_file_name(name).is_some()));
        files.sort_by(|left, right| left.path.cmp(&right.path));
        for file in files {
            if cancelled() {
                return Err(BackendError::message("directory import cancelled"));
            }
            let result = async {
                let (name, parent) = file.path.split_last().ok_or("import file path is empty")?;
                let parent_id = parents.get(parent).ok_or("Parent folder could not be imported")?;
                // A book already at this name was imported before: continuing an
                // import skips it without reading the file.
                if self.database.has_book_named(parent_id, name).map_err(BackendError::operation)? {
                    return Ok(());
                }
                let import = self.validate_import(parent_id, name)?;
                // Browser storage is keyed by content, so the name needs no collision check.
                let staged = self.assets.stage_browser_file(&file.source.into_file()?).await.map_err(BackendError::operation)?;
                self.finish_import(import, staged).await.map(drop)
            }
            .await;
            if let Err(error) = result {
                failures.push(ImportFailure::file(&file.path, &error));
            }
        }
        Ok(failures)
    }

    pub(crate) async fn prepare_directory_import(&self, parent_id: &DirId, name: Option<String>, mut directories: Vec<Vec<String>>) -> Result<crate::PreparedDirectoryImport, BackendError> {
        crate::import::validate_import_destination(&self.database, parent_id)?;
        // A folder of the same name in the same place is reused, so importing a
        // folder again continues it instead of creating a second copy.
        let create = |parent: &DirId, name: &str| -> Result<DirId, BackendError> {
            if let Some(existing) = self.database.child_directory(parent, name.trim()).map_err(BackendError::operation)? {
                return Ok(existing);
            }
            Ok(self.placement.create_directory(parent, name)?.id)
        };
        // Let queued import status reach subscribers before doing SQLite work;
        // large directory trees must not monopolize the executor.
        sleep(Duration::ZERO).await.map_err(BackendError::operation)?;
        let root_id = match name {
            Some(name) => create(parent_id, &name)?,
            None => *parent_id,
        };
        let mut imported_directories = HashMap::from([(Vec::<String>::new(), root_id)]);
        let mut failures = Vec::new();

        // A parent path sorts before every path that extends it.
        directories.sort_unstable();
        directories.dedup();
        let mut slice_started = web_time::Instant::now();
        for path in directories {
            if slice_started.elapsed() >= Duration::from_millis(8) {
                sleep(Duration::ZERO).await.map_err(BackendError::operation)?;
                slice_started = web_time::Instant::now();
            }
            let Some((name, parent_path)) = path.split_last() else { continue };
            let Some(parent_id) = imported_directories.get(parent_path) else {
                failures.push(ImportFailure::folder(&path, "parent folder could not be imported"));
                continue;
            };
            match create(parent_id, name) {
                Ok(directory) => {
                    imported_directories.insert(path, directory);
                }
                Err(error) => failures.push(ImportFailure::folder(&path, &error)),
            }
        }

        Ok(crate::PreparedDirectoryImport { directories: imported_directories, failures })
    }
}
