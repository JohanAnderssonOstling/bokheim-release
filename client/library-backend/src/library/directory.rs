//! Library-owned directory for already-open library actors.
//!
//! The storage worker owns the map alongside durable registry policy. Callers
//! carry only a directory ID and receive cloned actor handles, never map borrows.

use super::{LibraryHandle, LibraryRuntimeSpec};
use crate::executor::{BackendExecutor, BackendOutput, BackendSend};
use crate::{BackendError, LibraryId};
use client_platform_runtime::storage_queue::{self, Priority};
use std::cell::RefCell;
use std::collections::{hash_map::Entry, HashMap};
use uuid::Uuid;

thread_local! {
    static DIRECTORIES: RefCell<HashMap<Uuid, LocalDirectory>> = RefCell::new(HashMap::new());
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::library::handle::LibraryActorCommand;
    use std::sync::Arc;
    use std::thread;

    fn handle() -> (LibraryHandle, async_channel::Receiver<LibraryActorCommand>) {
        let (commands, inbox) = async_channel::unbounded();
        (LibraryHandle { id: LibraryId::new_v4(), commands }, inbox)
    }

    #[test]
    fn concurrent_callers_use_one_owner_and_retirement_closes_old_handles() {
        let directory = Arc::new(LibraryDirectory::default());
        let owner = directory.run(|_| Ok(thread::current().id())).unwrap();
        assert_ne!(owner, thread::current().id());
        let clients = (0..6)
            .map(|_| {
                let directory = directory.clone();
                thread::spawn(move || {
                    let (library, _inbox) = handle();
                    let id = library.id();
                    directory.insert(library.clone()).unwrap();
                    let old = directory.get(&id).unwrap();
                    assert!(!old.commands.is_closed());
                    directory
                        .run(move |_| {
                            assert_eq!(thread::current().id(), owner);
                            Ok(())
                        })
                        .unwrap();
                    directory.retire(&id).unwrap();
                    assert!(old.commands.is_closed());
                    assert!(library.commands.is_closed());
                    assert!(directory.try_get(&id).unwrap().is_none());
                })
            })
            .collect::<Vec<_>>();
        for client in clients {
            client.join().unwrap();
        }
        // Registry-policy commands already on the owner can use the directory
        // directly without enqueueing and waiting on that same worker.
        storage_queue::run_sync(Priority::Interactive, move || assert!(directory.all().unwrap().is_empty())).unwrap();
    }

    #[test]
    fn shutdown_closes_actors_while_directory_clients_remain() {
        let directory = Arc::new(LibraryDirectory::default());
        let client = directory.clone();
        let (library, _inbox) = handle();
        directory.insert(library.clone()).unwrap();
        assert!(!library.commands.is_closed());
        directory.shutdown();
        assert!(library.commands.is_closed());
        assert!(client.all().unwrap().is_empty());
    }

    #[test]
    fn last_drop_disposes_only_its_own_directory() {
        let directory = LibraryDirectory::default();
        let other = LibraryDirectory::default();
        let (library, _inbox) = handle();
        let (other_library, _other_inbox) = handle();
        directory.insert(library.clone()).unwrap();
        other.insert(other_library.clone()).unwrap();
        assert!(other.try_get(&library.id()).unwrap().is_none());
        let id = directory.id;
        drop(directory);
        // Cleanup and this check share background FIFO ordering.
        storage_queue::run_sync(Priority::Background, move || {
            assert!(!DIRECTORIES.with_borrow(|directories| directories.contains_key(&id)));
        })
        .unwrap();
        assert!(library.commands.is_closed());
        assert!(!other_library.commands.is_closed());
        assert!(other.try_get(&other_library.id()).unwrap().is_some());
    }
}

#[derive(Default)]
struct LocalDirectory {
    handles: HashMap<LibraryId, LibraryHandle>,
}

impl LocalDirectory {
    fn shutdown(&mut self) {
        for (_, handle) in self.handles.drain() {
            handle.shutdown();
        }
    }
}

impl Drop for LocalDirectory {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct LibraryDirectory {
    id: Uuid,
}

impl Default for LibraryDirectory {
    fn default() -> Self {
        Self { id: Uuid::new_v4() }
    }
}

impl Drop for LibraryDirectory {
    fn drop(&mut self) {
        let id = self.id;
        if let Err(error) = storage_queue::dispatch_cleanup(move || {
            DIRECTORIES.with_borrow_mut(|directories| directories.remove(&id));
        }) {
            log::error!("could not dispose library directory: {error}");
        }
    }
}

impl LibraryDirectory {
    fn run<T: BackendOutput>(&self, operation: impl FnOnce(&mut LocalDirectory) -> Result<T, BackendError> + BackendSend + 'static) -> Result<T, BackendError> {
        let id = self.id;
        storage_queue::run_sync(Priority::Interactive, move || DIRECTORIES.with_borrow_mut(|directories| operation(directories.entry(id).or_default()))).map_err(BackendError::message)?
    }

    /// Opens at most one actor for this ID. Registry policy and this operation
    /// can share one storage command; nested owner calls execute directly.
    pub fn open(&self, spec: LibraryRuntimeSpec, executor: &BackendExecutor) -> Result<bool, BackendError> {
        self.open_with(spec, executor, LibraryHandle::open)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_recovered(&self, spec: LibraryRuntimeSpec, executor: &BackendExecutor) -> Result<bool, BackendError> {
        self.open_with(spec, executor, LibraryHandle::open_recovered)
    }

    fn open_with(&self, spec: LibraryRuntimeSpec, executor: &BackendExecutor, open: fn(LibraryRuntimeSpec, &BackendExecutor) -> Result<LibraryHandle, BackendError>) -> Result<bool, BackendError> {
        let executor = executor.clone();
        self.run(move |directory| match directory.handles.entry(spec.config.id) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(entry) => {
                entry.insert(open(spec, &executor)?);
                Ok(true)
            }
        })
    }

    pub fn get(&self, id: &LibraryId) -> Result<LibraryHandle, BackendError> {
        self.try_get(id)?.ok_or_else(|| BackendError::message(format!("library {id} is not open")))
    }

    /// Resolves an optional runtime without treating a registered but
    /// unavailable library as a directory failure.
    pub fn try_get(&self, id: &LibraryId) -> Result<Option<LibraryHandle>, BackendError> {
        let id = *id;
        self.run(move |directory| Ok(directory.handles.get(&id).cloned()))
    }

    /// Removes a session and closes its command channel before replying.
    /// Previously cloned handles immediately reject new commands as well.
    pub fn retire(&self, id: &LibraryId) -> Result<(), BackendError> {
        let id = *id;
        self.run(move |directory| {
            if let Some(handle) = directory.handles.remove(&id) {
                handle.shutdown();
            }
            Ok(())
        })
    }

    pub fn all(&self) -> Result<Vec<LibraryHandle>, BackendError> {
        self.run(|directory| Ok(directory.handles.values().cloned().collect()))
    }

    /// Application shutdown stops actors even if transports retain the
    /// directory. Cleanup remains admitted when the request queue is full.
    pub fn shutdown(&self) {
        if self
            .run(|directory| {
                directory.shutdown();
                Ok(())
            })
            .is_err()
        {
            let id = self.id;
            if let Err(error) = storage_queue::dispatch_cleanup(move || {
                DIRECTORIES.with_borrow_mut(|directories| {
                    if let Some(directory) = directories.get_mut(&id) {
                        directory.shutdown();
                    }
                });
            }) {
                log::error!("could not shut down library directory: {error}");
            }
        }
    }

    /// Notifications for an unopened library require no actor work. Opening
    /// it later schedules its initial remote sync pass.
    pub fn request_remote_refresh(&self, id: &LibraryId) -> Result<(), BackendError> {
        let id = *id;
        self.run(move |directory| {
            if let Some(library) = directory.handles.get(&id) {
                library.request_remote_refresh();
            }
            Ok(())
        })
    }

    /// Reconcile every open library after reconnecting or resuming.
    pub fn request_remote_refresh_all(&self) -> Result<(), BackendError> {
        self.run(|directory| {
            for library in directory.handles.values() {
                library.request_remote_refresh();
            }
            Ok(())
        })
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(super) fn insert(&self, handle: LibraryHandle) -> Result<(), BackendError> {
        self.run(move |directory| {
            directory.handles.insert(handle.id(), handle);
            Ok(())
        })
    }
}
