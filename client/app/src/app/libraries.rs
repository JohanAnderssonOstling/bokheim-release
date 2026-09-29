use super::core::*;
use crate::library::LibraryRuntimeSpec;
use std::sync::Arc;

impl AppBackend {
    pub(crate) fn libraries(&self) -> Result<Vec<LibraryEntry>, BackendError> {
        self.state.registry.entries().map_err(BackendError::operation)
    }

    /// Opens every registered library once during application startup, at the
    /// location the registry records. A failed open is isolated to that library
    /// so one damaged or inaccessible library cannot prevent the application
    /// from starting.
    pub(super) fn open_registered_libraries(&self) -> Result<(), BackendError> {
        for entry in self.state.registry.entries().map_err(BackendError::operation)? {
            if let Err(error) = self.open_registered_library(entry.library_id()) {
                log::warn!("Could not open library {} during startup: {error}", entry.library_id());
            }
        }
        // Work state lives in each library database. Once all sessions have
        // been opened, ask every available one to resume its durable queue.
        Ok(())
    }

    pub fn create_library(&self, name: String, locator: Option<String>) -> Result<LibraryEntry, BackendError> {
        let entry = self.register_library_with_asset_storage(&name, locator, true)?;
        self.finish_created_library(entry)
    }

    pub(crate) fn create_library_with_asset_storage(&self, name: String, asset_storage_enabled: bool) -> Result<LibraryEntry, BackendError> {
        let entry = self.register_library_with_asset_storage(&name, None, asset_storage_enabled)?;
        self.finish_created_library(entry)
    }

    fn register_library_with_asset_storage(&self, name: &str, locator: Option<String>, asset_storage_enabled: bool) -> Result<LibraryEntry, BackendError> {
        let _guard = self.state.library_directory.lock_lifecycle()?;
        let previous = self.state.registry.entries().map_err(BackendError::operation)?;
        let entry = self.state.registry.create_with_asset_storage(name, locator, asset_storage_enabled).map_err(BackendError::operation)?;
        if previous.iter().any(|old| old.library_id() == entry.library_id() && old.storage_locator() != entry.storage_locator()) {
            self.state.library_directory.retire(entry.library_id())?;
        }
        drop(_guard);
        Ok(entry)
    }

    /// Registers folders selected by a native frontend.
    ///
    /// Paths remain inside the backend after this call. Frontends identify the
    /// resulting libraries only by `LibraryId`.
    pub(crate) fn create_libraries_from_paths(&self, paths: Vec<PathBuf>) -> Result<(Vec<LibraryId>, Vec<String>), BackendError> {
        self.create_libraries_from_paths_with_asset_storage(paths, true)
    }

    pub(crate) fn create_libraries_from_paths_with_asset_storage(&self, paths: Vec<PathBuf>, asset_storage_enabled: bool) -> Result<(Vec<LibraryId>, Vec<String>), BackendError> {
        let mut known_paths = self.libraries()?.into_iter().map(|entry| PathBuf::from(entry.storage_locator())).collect::<HashSet<_>>();
        let mut created_ids = Vec::new();
        let mut errors = Vec::new();
        for path in paths {
            let display_path = path.display().to_string();
            if !path.is_dir() {
                errors.push(format!("{display_path}: not a folder"));
                continue;
            }
            let canonical_path = match path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    errors.push(format!("{display_path}: {error}"));
                    continue;
                }
            };
            if known_paths.contains(&canonical_path) {
                continue;
            }
            let opened = self.register_library_with_asset_storage(&library_name_for_folder(&path), Some(path.to_string_lossy().into_owned()), asset_storage_enabled).and_then(|entry| {
                // Opening starts any required library-owned work before the UI
                // receives the registry entry.
                self.open_registered_library(entry.library_id())?;
                self.finish_created_library(entry)
            });
            match opened {
                Ok(entry) => {
                    // Opening the library already queued its filesystem scan.
                    known_paths.insert(PathBuf::from(entry.storage_locator()));
                    created_ids.push(*entry.library_id());
                }
                Err(error) => errors.push(format!("{display_path}: {error}")),
            }
        }
        Ok((created_ids, errors))
    }

    /// Chooses or creates a destination for an explicit import.
    /// Startup and deletion must allow an empty library list.
    ///
    /// The check and creation share the library-list lock so concurrent imports
    /// cannot create two destinations from the same empty snapshot.
    pub fn ensure_import_library(&self) -> Result<LibraryEntry, BackendError> {
        let entry = {
            let _guard = self.state.library_directory.lock_lifecycle()?;
            for entry in self.state.registry.entries().map_err(BackendError::operation)? {
                if self.state.library_directory.try_get(entry.library_id())?.is_some() || self.open_registered_library(entry.library_id()).is_ok() {
                    return Ok(entry);
                }
            }
            self.state.registry.create("Library", None).map_err(BackendError::operation)?
        };
        self.finish_created_library(entry)
    }

    fn finish_created_library(&self, entry: LibraryEntry) -> Result<LibraryEntry, BackendError> {
        self.open_registered_library(entry.library_id())?;
        self.send_library_list_changed();
        Ok(entry)
    }

    pub(crate) async fn remove_library_from_device(&self, library_id: &LibraryId) -> Result<(), BackendError> {
        {
            let _guard = self.state.library_directory.lock_lifecycle()?;
            self.state.registry.begin_local_removal(library_id).map_err(BackendError::operation)?;
            self.state.library_directory.retire(library_id)?;
        }
        self.send_library_list_changed();
        self.finish_local_purge(*library_id).await?;
        Ok(())
    }

    async fn finish_local_purge(&self, library_id: LibraryId) -> Result<(), BackendError> {
        if self.state.library_directory.try_get(&library_id)?.is_some() {
            return Err(BackendError::message("library is still open during local cleanup"));
        }
        #[cfg(target_arch = "wasm32")]
        crate::platform::web_storage::purge_library(library_id).await.map_err(BackendError::message)?;
        self.state.registry.complete_local_purge(&library_id).map_err(BackendError::operation)
    }

    pub(super) fn start_pending_local_purges(&self) {
        let weak = Arc::downgrade(&self.state);
        self.state.executor.spawn_detached(async move {
            loop {
                let Some(state) = weak.upgrade() else { break };
                let pending = state.registry.pending_local_purges();
                drop(state);
                match pending {
                    Ok(pending) => {
                        for library_id in pending {
                            let Some(state) = weak.upgrade() else { return };
                            if let Err(error) = (AppBackend { state }).finish_local_purge(library_id).await {
                                log::warn!("local library purge for {library_id} will retry: {error}");
                            }
                        }
                    }
                    Err(error) => log::warn!("could not read pending local library purges: {error}"),
                }
                crate::executor::sleep(std::time::Duration::from_secs(30)).await;
            }
        });
    }

    pub(crate) fn rename_library(&self, library_id: &LibraryId, name: &str) -> Result<(), BackendError> {
        {
            let _guard = self.state.library_directory.lock_lifecycle()?;
            // Require an existing library; renaming must never recreate a deleted one.
            self.library_entry(library_id)?;
            // Updates both registry metadata and the native manifest, preserving storage.
            let entry = self.state.registry.attach_remote(library_id, name).map_err(BackendError::operation)?;
            self.state.registry.write_value(&format!("pending-library-rename/{library_id}"), entry.library_name().as_bytes()).map_err(BackendError::operation)?;
            if let Ok(handle) = self.state.library_directory.get(library_id) {
                handle.rename(entry.library_name().to_owned());
            }
        }
        self.send_library_list_changed();
        self.state.account_scheduler.wake();
        Ok(())
    }

    pub(crate) async fn delete_library(&self, library_id: &LibraryId) -> Result<(), BackendError> {
        // Local actions never depend on network/account availability. Removal
        // records a durable server-deletion intent before discarding local state.
        self.remove_library_from_device(library_id).await?;
        Ok(())
    }

    /// Explicit eager open.  This is called only during startup and creation;
    /// ordinary lookups never open a library as a side effect.
    pub(super) fn open_registered_library(&self, library_id: &LibraryId) -> Result<(), BackendError> {
        let entry = self.library_entry(library_id)?;
        let (spec, reset_token) = self.library_runtime_spec(entry.storage_locator(), *library_id)?;
        let opened = self.state.library_directory.ensure_open(spec, &self.state.executor)?;
        if opened {
            if let Some(token) = reset_token {
                let reset_key = format!("cloud-storage-reset:{library_id}");
                self.state.registry.clear_value_if_current(&reset_key, &token).map_err(BackendError::operation)?;
            }
        }
        Ok(())
    }

    pub(crate) fn set_library_asset_storage(&self, id: LibraryId, enabled: bool) -> Result<(), BackendError> {
        let library = self.state.library_directory.get(&id)?;
        self.state.registry.queue_cloud_storage(&id, enabled).map_err(BackendError::operation)?;
        // The registry owns the durable policy; the session actor owns its
        // local cache, sync object, and any reconciliation it triggers.
        library.set_asset_storage(enabled, true);
        self.state.account_scheduler.wake();
        Ok(())
    }

    pub(crate) fn library_asset_storage_enabled(&self, id: &LibraryId) -> Result<bool, BackendError> {
        self.state.registry.asset_storage_enabled(id).map_err(BackendError::operation)
    }
}

impl AppBackend {
    /// Converts app-owned registry/account policy into the input for a
    /// library-owned runtime open.  App supplies policy only; the library
    /// creates resources, installs observers, and starts its own workers.
    fn library_runtime_spec(&self, library_path: &str, library_id: LibraryId) -> Result<(LibraryRuntimeSpec, Option<Vec<u8>>), BackendError> {
        let reset_key = format!("cloud-storage-reset:{library_id}");
        let reset_token = self.state.registry.read_value(&reset_key).map_err(BackendError::operation)?;
        let cloud_enabled = self.library_asset_storage_enabled(&library_id)?;
        let display_name = self.library_entry(&library_id)?.library_name().to_owned();
        Ok((
            LibraryRuntimeSpec {
                config: crate::library::LibraryOpenConfig {
                    id: library_id,
                    display_name,
                    root: library_path.to_owned(),
                    database_locator: self.state.registry.library_database_locator(&library_id, library_path),
                    default_sync_server_url: crate::api_server_url(),
                    metadata_server_url: crate::METADATA_SERVER_ORIGIN.to_owned(),
                    asset_storage_enabled: cloud_enabled,
                    reset_cloud_presence: reset_token.is_some(),
                },
                account: self.state.account.library_account(),
                notification_interest: self.state.notification_interest.clone(),
                remote_changes: self.state.remote_changes.clone(),
            },
            reset_token,
        ))
    }
}
