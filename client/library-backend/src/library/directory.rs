//! Library-owned lifecycle directory for already-open library actors.
//!
//! This is deliberately separate from the durable application registry: the
//! registry decides which libraries exist, while this directory owns the
//! running sessions for the current process.

use crate::{BackendError, LibraryId};
use futures_util::{stream, StreamExt, TryStreamExt};
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
};

use super::{LibraryHandle, LibraryRuntimeSpec};

#[derive(Default)]
pub struct LibraryDirectory {
    handles: Mutex<HashMap<LibraryId, LibraryHandle>>,
    lifecycle: Mutex<()>,
}

impl LibraryDirectory {
    /// Serializes open/retire transitions with their registry-policy update.
    pub fn lock_lifecycle(&self) -> Result<std::sync::MutexGuard<'_, ()>, BackendError> {
        self.lifecycle.lock().map_err(|_| BackendError::message("library lifecycle lock is poisoned"))
    }

    pub fn get(&self, id: &LibraryId) -> Result<LibraryHandle, BackendError> {
        self.try_get(id)?.ok_or_else(|| BackendError::message(format!("library {id} is not open")))
    }

    /// Resolves an optional runtime without treating a registered but
    /// unavailable library as a directory failure.
    pub fn try_get(&self, id: &LibraryId) -> Result<Option<LibraryHandle>, BackendError> {
        Ok(self.lock_handles()?.get(id).cloned())
    }

    /// Opens exactly one session for this process. Holding the directory lock
    /// through construction is intentional: a duplicate open would start a
    /// second scanner/sync actor before either handle could be discarded.
    /// Ensures a session exists, returning whether this call created it.
    ///
    /// The directory retains the handle: application lifecycle only needs to
    /// establish the session, while users of a running library obtain a handle
    /// explicitly through `get`.
    pub fn ensure_open(&self, spec: LibraryRuntimeSpec, executor: &crate::executor::BackendExecutor) -> Result<bool, BackendError> {
        let id = spec.config.id;
        let mut handles = self.lock_handles()?;
        if handles.contains_key(&id) {
            return Ok(false);
        }
        handles.insert(id, LibraryHandle::open(spec, executor)?);
        Ok(true)
    }

    /// Removes a session from the process directory and tells its actor to
    /// stop. Registry changes remain app policy; retirement is session
    /// lifecycle and belongs here.
    pub fn retire(&self, id: &LibraryId) -> Result<(), BackendError> {
        let handle = self.lock_handles()?.remove(id);
        if let Some(handle) = handle {
            handle.shutdown();
        }
        Ok(())
    }

    pub fn all(&self) -> Result<Vec<LibraryHandle>, BackendError> {
        Ok(self.lock_handles()?.values().cloned().collect())
    }

    /// Collects presentation snapshots from the open library actors. The
    /// application may display this aggregate, but does not inspect sessions.
    pub async fn transfer_snapshots(&self) -> Result<Vec<crate::LibraryTransfers>, BackendError> {
        let handles = self.all()?;
        let mut snapshots: Vec<_> = stream::iter(handles.into_iter().map(|handle| async move { handle.transfer_snapshot().await }))
            .buffered(4)
            .try_collect()
            .await?;
        snapshots.sort_by(|left, right| left.library_name.cmp(&right.library_name));
        Ok(snapshots)
    }

    fn lock_handles(&self) -> Result<MutexGuard<'_, HashMap<LibraryId, LibraryHandle>>, BackendError> {
        self.handles.lock().map_err(|_| BackendError::message("library directory lock is poisoned"))
    }

    pub fn shutdown_all(&self) {
        for (_, handle) in self.handles.lock().unwrap_or_else(|error| error.into_inner()).drain() {
            handle.shutdown();
        }
    }
}
