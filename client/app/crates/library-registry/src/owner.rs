//! The storage worker owns connections; cloneable handles carry only IDs and
//! immutable locators. Every request returns values, never a SQLite borrow.
use super::{join_locator, LibraryEntry, LocalRegistry, RegistryResult, DATABASE_FILENAME};
use client_platform_runtime::storage_queue::{self, Priority};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use sync_common::LibraryId;
use uuid::Uuid;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

thread_local! {
    static REGISTRIES: RefCell<HashMap<Uuid, LocalRegistry>> = RefCell::new(HashMap::new());
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn cloned_clients_share_one_worker_owned_registry_until_last_drop() -> RegistryResult<()> {
        let root = tempfile::tempdir()?;
        let registry = LibraryRegistry::open(root.path().to_string_lossy())?;
        let id = registry.0.id;
        let owner = storage_queue::run_sync(Priority::Interactive, move || {
            assert!(REGISTRIES.with_borrow(|registries| registries.contains_key(&id)));
            thread::current().id()
        })?;
        assert_ne!(owner, thread::current().id());
        let clients = (0..6)
            .map(|index| {
                let registry = registry.clone();
                thread::spawn(move || -> RegistryResult<()> {
                    let key = format!("client-{index}");
                    registry.write_value(&key, &[index])?;
                    assert_eq!(registry.read_value(&key)?, Some(vec![index]));
                    registry.run(move |_| {
                        assert_eq!(thread::current().id(), owner);
                        Ok(())
                    })
                })
            })
            .collect::<Vec<_>>();
        for client in clients {
            client.join().unwrap()?;
        }
        assert!(storage_queue::run_sync(Priority::Background, move || REGISTRIES.with_borrow(|registries| registries.contains_key(&id)))?);
        drop(registry);
        // Background FIFO ensures the disposal has run before this check.
        assert!(!storage_queue::run_sync(Priority::Background, move || REGISTRIES.with_borrow(|registries| registries.contains_key(&id)))?);
        Ok(())
    }
}

struct Handle {
    id: Uuid,
    #[cfg(not(target_arch = "wasm32"))]
    app_data_locator: String,
    default_library_root: String,
}

impl Drop for Handle {
    fn drop(&mut self) {
        let id = self.id;
        let _ = storage_queue::dispatch_cleanup(move || {
            REGISTRIES.with_borrow_mut(|registries| registries.remove(&id));
        });
    }
}

#[derive(Clone)]
pub struct LibraryRegistry(Arc<Handle>);

impl std::fmt::Debug for LibraryRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("LibraryRegistry").field("default_library_root", &self.0.default_library_root).finish_non_exhaustive()
    }
}

impl LibraryRegistry {
    pub fn open(app_data_locator: impl Into<String>) -> RegistryResult<Self> {
        let app_data_locator = app_data_locator.into();
        let library_root = join_locator(&app_data_locator, "libraries");
        Self::open_with_library_root(app_data_locator, library_root)
    }

    pub fn open_with_library_root(app_data_locator: impl Into<String>, default_library_root: impl Into<String>) -> RegistryResult<Self> {
        let app_data_locator = app_data_locator.into();
        let default_library_root = default_library_root.into();
        storage_queue::run_sync(Priority::Interactive, move || {
            let registry = LocalRegistry::open_with_library_root(app_data_locator.clone(), default_library_root.clone())?;
            let id = Uuid::new_v4();
            REGISTRIES.with_borrow_mut(|registries| registries.insert(id, registry));
            Ok(Self(Arc::new(Handle {
                id,
                #[cfg(not(target_arch = "wasm32"))]
                app_data_locator,
                default_library_root,
            })))
        })?
    }

    /// Actual registry filename in the selected platform storage namespace.
    pub fn database_locator(app_data_locator: &str) -> String {
        join_locator(app_data_locator, DATABASE_FILENAME)
    }

    /// Native databases belong to the device, independently of the content folder.
    pub fn library_database_locator(&self, id: &LibraryId, content_locator: &str) -> String {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = content_locator;
            join_locator(&self.0.app_data_locator, &format!("libraries/{id}/library.db"))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = id;
            content_locator.to_owned()
        }
    }

    pub(super) fn run<T: Send + 'static>(&self, operation: impl FnOnce(&mut LocalRegistry) -> RegistryResult<T> + Send + 'static) -> RegistryResult<T> {
        let handle = self.0.clone();
        storage_queue::run_sync(Priority::Interactive, move || {
            REGISTRIES.with_borrow_mut(|registries| {
                let registry = registries.get_mut(&handle.id).ok_or("library registry is closed")?;
                operation(registry)
            })
        })?
    }

    pub fn entries(&self) -> RegistryResult<Vec<LibraryEntry>> {
        self.run(move |registry| registry.entries())
    }

    pub fn entry(&self, id: &LibraryId) -> RegistryResult<Option<LibraryEntry>> {
        let id = *id;
        self.run(move |registry| registry.entry(&id))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn recover_library_root(&self, id: &LibraryId) -> RegistryResult<super::LibraryRootRecovery> {
        let id = *id;
        self.run(move |registry| registry.recover_library_root(&id))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn remember_library_root(&self, id: &LibraryId) -> RegistryResult<()> {
        let id = *id;
        self.run(move |registry| registry.remember_library_root(&id))
    }

    pub fn asset_storage_enabled(&self, id: &LibraryId) -> RegistryResult<bool> {
        let id = *id;
        self.run(move |registry| registry.asset_storage_enabled(&id))
    }

    pub fn storage(&self, id: &LibraryId) -> RegistryResult<super::LibraryStorage> {
        let id = *id;
        self.run(move |registry| registry.storage(&id))
    }

    pub fn cache_remote_storage(&self, id: &LibraryId, storage: super::LibraryStorage) -> RegistryResult<()> {
        let id = *id;
        self.run(move |registry| registry.cache_remote_storage(&id, storage))
    }

    pub fn create_with_storage(&self, name: &str, locator: Option<String>, storage: super::LibraryStorage) -> RegistryResult<LibraryEntry> {
        let name = name.to_owned();
        self.run(move |registry| registry.create_with_storage(&name, locator, storage))
    }

    pub fn set_asset_storage_enabled(&self, id: &LibraryId, enabled: bool) -> RegistryResult<()> {
        let id = *id;
        self.run(move |registry| registry.set_asset_storage_enabled(&id, enabled))
    }

    pub fn cache_cloud_storage_with_reset(&self, id: &LibraryId, enabled: bool) -> RegistryResult<()> {
        let id = *id;
        self.run(move |registry| registry.cache_cloud_storage_with_reset(&id, enabled))
    }

    pub fn clear_value_if_current(&self, key: &str, value: &[u8]) -> RegistryResult<()> {
        let key = key.to_owned();
        let value = value.to_vec();
        self.run(move |registry| registry.clear_value_if_current(&key, &value))
    }

    /// Save the local cache and an idempotent account write together.
    pub fn queue_cloud_storage(&self, id: &LibraryId, enabled: bool) -> RegistryResult<()> {
        let id = *id;
        self.run(move |registry| registry.queue_cloud_storage(&id, enabled))
    }

    pub fn create(&self, name: &str, locator: Option<String>) -> RegistryResult<LibraryEntry> {
        let name = name.to_owned();
        self.run(move |registry| registry.create(&name, locator))
    }

    /// Persist the policy before the new library becomes visible to background work.
    pub fn create_with_asset_storage(&self, name: &str, locator: Option<String>, asset_storage_enabled: bool) -> RegistryResult<LibraryEntry> {
        let name = name.to_owned();
        self.run(move |registry| registry.create_with_asset_storage(&name, locator, asset_storage_enabled))
    }

    pub fn attach_remote(&self, id: &LibraryId, name: &str) -> RegistryResult<LibraryEntry> {
        let id = *id;
        let name = name.to_owned();
        self.run(move |registry| registry.attach_remote(&id, &name))
    }

    pub fn default_library_save_location(&self) -> RegistryResult<Option<String>> {
        self.run(move |registry| registry.default_library_save_location())
    }

    /// Changes the destination for future libraries. Registered roots never move.
    pub fn set_default_library_save_location(&self, path: &str) -> RegistryResult<String> {
        let path = path.to_owned();
        self.run(move |registry| registry.set_default_library_save_location(&path))
    }

    pub fn is_detached(&self, library_id: &LibraryId) -> RegistryResult<bool> {
        let library_id = *library_id;
        self.run(move |registry| registry.is_detached(&library_id))
    }

    pub fn pending_server_deletions(&self) -> RegistryResult<Vec<LibraryId>> {
        self.run(move |registry| registry.pending_server_deletions())
    }

    pub fn complete_server_deletion(&self, library_id: &LibraryId) -> RegistryResult<()> {
        let library_id = *library_id;
        self.run(move |registry| registry.complete_server_deletion(&library_id))
    }

    /// Hide the library and persist both server and local deletion intents
    /// before physical cleanup can be interrupted.
    pub fn begin_local_removal(&self, library_id: &LibraryId) -> RegistryResult<()> {
        let library_id = *library_id;
        self.run(move |registry| registry.begin_local_removal(&library_id))
    }

    pub fn pending_local_purges(&self) -> RegistryResult<Vec<LibraryId>> {
        self.run(move |registry| registry.pending_local_purges())
    }

    /// Physical cleanup is idempotent. Keep the purge record if any step fails.
    pub fn complete_local_purge(&self, library_id: &LibraryId) -> RegistryResult<()> {
        let library_id = *library_id;
        self.run(move |registry| registry.complete_local_purge(&library_id))
    }

    /// Discovers libraries exposed by the current platform and registers any
    /// new entries. Platforms without directory discovery return an explicit
    /// capability error from the adapter; registration behavior stays shared.
    pub fn discover(&self, parent: &Path) -> RegistryResult<()> {
        let parent = parent.to_owned();
        self.run(move |registry| registry.discover(&parent))
    }

    pub fn remove(&self, library_id: &LibraryId) -> RegistryResult<()> {
        let library_id = *library_id;
        self.run(move |registry| registry.remove(&library_id))
    }

    pub fn read_value(&self, key: &str) -> RegistryResult<Option<Vec<u8>>> {
        let key = key.to_owned();
        self.run(move |registry| registry.read_value(&key))
    }

    /// Compare and replace device credentials in one owner command. `None`
    /// preserves a newer login or a connection removed during token refresh.
    pub fn update_value(&self, key: &str, update: impl FnOnce(Option<&[u8]>) -> RegistryResult<Option<Vec<u8>>> + Send + 'static) -> RegistryResult<bool> {
        let key = key.to_owned();
        self.run(move |registry| {
            let tx = registry.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: Option<Vec<u8>> = tx.query_row("SELECT value FROM app_value WHERE key=?1", [&key], |row| row.get(0)).optional()?;
            let Some(value) = update(current.as_deref())? else { return Ok(false); };
            tx.execute(include_str!("sql/set_sync_disabled.sql"), params![key, value])?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn write_value(&self, key: &str, value: &[u8]) -> RegistryResult<()> {
        let key = key.to_owned();
        let value = value.to_vec();
        self.run(move |registry| registry.write_value(&key, &value))
    }

    pub fn remove_value(&self, key: &str) -> RegistryResult<()> {
        let key = key.to_owned();
        self.run(move |registry| registry.remove_value(&key))
    }
}
