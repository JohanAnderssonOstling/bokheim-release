//! Provider-owned inventory and offline downloads. This worker runs without a
//! Bokheim session and never sends file or thumbnail bytes to Bokheim storage.

use super::{events::LibraryEvent, LibrarySession};
use crate::BackendError;
use book_model::BookFormat;
use client_platform_runtime::pending::OnDrop;
use cloud_storage::{connections::ConnectedDrive, scanner::ScanCursor, CloudChange, CloudEntry, CloudError};
use library_model::{DownloadProgress, DownloadState, LibraryUpdate, TransferOrigin};
use std::collections::{HashMap, HashSet};
use std::ops::AsyncFnMut;
use sync_common::{
    storage::{CloudLibraryRoot, ProviderFile, ProviderId},
    BookId, DirId, ROOT_DIR_ID,
};

#[cfg(all(target_arch = "wasm32", feature = "browser-cloud-checks"))]
pub mod browser_checks;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(all(test, feature = "google-drive", not(target_arch = "wasm32")))]
pub(super) mod tests;
mod thumbnails;
#[cfg(target_arch = "wasm32")]
mod web;

fn entry_id(entry: &CloudEntry) -> &ProviderId {
    match entry {
        CloudEntry::Folder(folder) => &folder.id,
        CloudEntry::File(file) => &file.file.identity.file_id,
    }
}

impl LibrarySession {
    /// Copy revision-pinned chunks into the host's writer. A stale browser
    /// snapshot stops before publication; native publication rechecks its revision.
    async fn copy_cloud_download(
        &self, connection: &ConnectedDrive, file: &ProviderFile, mut write: impl AsyncFnMut(&[u8]) -> Result<(), BackendError>, mut current: impl FnMut() -> Result<bool, BackendError>,
    ) -> Result<Option<u64>, BackendError> {
        let mut completed = 0;
        while completed < file.size_bytes {
            if !current()? {
                return Ok(None);
            }
            let length = (file.size_bytes - completed).min(cloud_storage::MAX_RANGE_BYTES as u64) as usize;
            let bytes = connection.read_range(file, completed, length).await.map_err(BackendError::operation)?;
            if bytes.len() != length {
                return Err(BackendError::message("cloud download returned an incomplete range"));
            }
            write(&bytes).await?;
            completed += bytes.len() as u64;
            self.emit(LibraryEvent::DownloadStatusChanged { content_hash: file.identity.book_id(), state: DownloadState::Downloading(DownloadProgress::from_ratio(completed, file.size_bytes)) });
        }
        Ok(Some(completed))
    }

    pub(crate) async fn run_cloud_pass(&self) -> Result<(), BackendError> {
        let storage = self.db.storage_policy()?;
        let Some(root) = storage.external() else { return Ok(()) };
        let connection = self.cloud.google_drive(root.scope.clone()).map_err(BackendError::operation)?;
        let uploads = self.import.run_provider_uploads().await;
        let scan = self.scan_cloud_library(&connection, root).await;
        // A malformed or changing book must not block requested downloads of
        // other files whose provider references are already confirmed.
        let downloads = self.cloud_downloads(&connection).await;
        let covers = self.cloud_thumbnails(&connection, root).await;
        uploads?;
        scan?;
        downloads?;
        covers
    }

    async fn scan_cloud_library(&self, connection: &ConnectedDrive, root: &CloudLibraryRoot) -> Result<(), BackendError> {
        self.scanning.set(true);
        let _scanning = OnDrop::new(|| {
            self.scanning.set(false);
            // This scan already runs inside the asset worker. Announce its
            // result without scheduling another asset pass on its own completion.
            self.events.publish_update(LibraryUpdate::Scanning(self.scanner_running()));
            self.events.publish_update(LibraryUpdate::Contents);
            self.events.files.wake();
        });
        self.emit(LibraryEvent::ScanStarted);
        let inventory = self.cloud_inventory(connection, root).await?;
        self.admit_cloud_inventory(connection, root, inventory).await
    }

    async fn cloud_inventory(&self, connection: &ConnectedDrive, root: &CloudLibraryRoot) -> Result<Vec<CloudEntry>, BackendError> {
        let mut scan = self.db.provider_scan()?;
        if let Some(current) = scan.as_ref().filter(|scan| scan.completed) {
            let inventory = self.cloud_scan_entries(current.generation)?;
            let (changed, checkpoint) = match self.cloud_changes_affect_inventory(connection, root, &inventory, &current.checkpoint).await {
                Ok(result) => result,
                Err(CloudError::CursorExpired) => (true, connection.start_checkpoint().await.map_err(BackendError::operation)?),
                Err(error) => return Err(BackendError::operation(error)),
            };
            if !changed {
                self.db.advance_provider_checkpoint(current.generation, &checkpoint)?;
                return Ok(inventory);
            }
            scan = Some(self.db.begin_provider_scan(checkpoint, serde_json::to_string(&ScanCursor::new(root.folder_id.clone())).map_err(BackendError::operation)?)?);
        }
        let mut scan = match scan {
            Some(scan) => scan,
            None => self.db.begin_provider_scan(connection.start_checkpoint().await.map_err(BackendError::operation)?, serde_json::to_string(&ScanCursor::new(root.folder_id.clone())).map_err(BackendError::operation)?)?,
        };
        while let Some(cursor) = scan.cursor.as_deref() {
            let cursor: ScanCursor = serde_json::from_str(cursor).map_err(BackendError::operation)?;
            if cursor.root() != &root.folder_id {
                return Err(BackendError::message("cloud scan belongs to another library folder"));
            }
            let page = connection.scan_page(&cursor).await.map_err(BackendError::operation)?;
            let next = page.next.map(|next| serde_json::to_string(&next)).transpose().map_err(BackendError::operation)?;
            let entries = page.entries.into_iter().map(|entry| Ok((entry_id(&entry).as_str().to_owned(), serde_json::to_string(&entry).map_err(BackendError::operation)?))).collect::<Result<Vec<_>, BackendError>>()?;
            if !self.db.commit_provider_scan_page(&scan, next.clone(), entries)? {
                return Err(BackendError::message("cloud scan position changed"));
            }
            scan.cursor = next;
        }
        let entries = self.cloud_scan_entries(scan.generation)?;
        let (changed, checkpoint) = match self.cloud_changes_affect_inventory(connection, root, &entries, &scan.checkpoint).await {
            Ok(result) => result,
            Err(CloudError::CursorExpired) => (true, connection.start_checkpoint().await.map_err(BackendError::operation)?),
            Err(error) => return Err(BackendError::operation(error)),
        };
        if changed {
            self.db.begin_provider_scan(checkpoint, serde_json::to_string(&ScanCursor::new(root.folder_id.clone())).map_err(BackendError::operation)?)?;
            return Err(BackendError::message("cloud folder changed during enumeration; the new scan will resume"));
        }
        if !self.db.finish_provider_scan(scan.generation, &checkpoint)? {
            return Err(BackendError::message("cloud scan changed before completion"));
        }
        Ok(entries)
    }

    fn cloud_scan_entries(&self, generation: uuid::Uuid) -> Result<Vec<CloudEntry>, BackendError> {
        let mut after = String::new();
        let mut entries = Vec::new();
        loop {
            let page = self.db.provider_scan_entries(generation, &after, 1000)?;
            let Some((last, _)) = page.last() else { break };
            after = last.clone();
            for (_, entry) in page {
                entries.push(serde_json::from_str(&entry).map_err(BackendError::operation)?);
            }
        }
        Ok(entries)
    }

    async fn cloud_changes_affect_inventory(&self, connection: &ConnectedDrive, root: &CloudLibraryRoot, inventory: &[CloudEntry], checkpoint: &str) -> Result<(bool, String), CloudError> {
        let ids = inventory.iter().map(entry_id).collect::<HashSet<_>>();
        let mut folders = inventory
            .iter()
            .filter_map(|entry| match entry {
                CloudEntry::Folder(folder) => Some(&folder.id),
                _ => None,
            })
            .collect::<HashSet<_>>();
        folders.insert(&root.folder_id);
        let mut changed = false;
        let mut page = checkpoint.to_owned();
        loop {
            let changes = connection.changes(&page).await?;
            for change in changes.changes {
                match change {
                    CloudChange::Removed(id) => {
                        if id == root.folder_id {
                            return Err(CloudError::NotFound);
                        }
                        changed |= ids.contains(&id);
                    }
                    CloudChange::Updated(entry) => {
                        let parents = match &entry {
                            CloudEntry::Folder(folder) if folder.name == ".bokheim" => continue,
                            CloudEntry::Folder(folder) => &folder.parent_ids,
                            CloudEntry::File(file) => &file.parent_ids,
                        };
                        changed |= ids.contains(entry_id(&entry)) || parents.iter().any(|parent| folders.contains(parent));
                    }
                }
            }
            if let Some(next) = changes.next_page {
                page = next;
            } else {
                return Ok((changed, changes.checkpoint.ok_or_else(|| CloudError::InvalidResponse("cloud storage did not return a changes checkpoint".into()))?));
            }
        }
    }

    async fn admit_cloud_inventory(&self, connection: &ConnectedDrive, root: &CloudLibraryRoot, inventory: Vec<CloudEntry>) -> Result<(), BackendError> {
        let mut directories = HashMap::from([(root.folder_id.clone(), ROOT_DIR_ID)]);
        let mut folders = inventory
            .iter()
            .filter_map(|entry| match entry {
                CloudEntry::Folder(folder) => Some(folder.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        folders.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        let mut projected = HashSet::new();
        while !folders.is_empty() {
            let previous = folders.len();
            let mut pending = Vec::new();
            for folder in folders {
                let Some(parent) = folder.parent_ids.iter().find_map(|parent| directories.get(parent)).copied() else {
                    pending.push(folder);
                    continue;
                };
                let id = folder
                    .directory_hint
                    .as_ref()
                    .filter(|hint| hint.library_id == self.id && hint.directory_id != ROOT_DIR_ID && hint.directory_id != parent)
                    .map(|hint| hint.directory_id)
                    .unwrap_or_else(|| DirId::new_v5(&self.id, format!("provider-folder/{}", folder.id.as_str()).as_bytes()));
                let name = library_replica_folder_name(&folder.name);
                if projected.contains(&id) || self.db.upsert_provider_directory(id, parent, name)? {
                    projected.insert(id);
                    self.db.remember_provider_directory(id, &folder.id)?;
                    directories.insert(folder.id, id);
                }
            }
            if pending.len() == previous {
                break;
            }
            folders = pending;
        }
        // Folders are usable before potentially slow book inspection finishes.
        // Refresh the view now so its empty-library scan overlay can disappear.
        if !projected.is_empty() {
            self.events.publish_update(LibraryUpdate::Contents);
        }
        let mut present = HashSet::new();
        let mut failed = false;
        for entry in inventory {
            let CloudEntry::File(file) = entry else { continue };
            present.insert(file.file.identity.file_id.clone());
            let Some(parent) = file.parent_ids.iter().find_map(|parent| directories.get(parent)).copied() else { continue };
            let Some(format) = BookFormat::from_file_name(&file.name) else { continue };
            let id = file.file.identity.book_id();
            if self.db.provider_file(&id)?.is_some_and(|previous| previous.revision == file.file.revision) {
                self.db.confirm_provider_available(&id)?;
                self.db.place_provider_book(id, parent)?;
                continue;
            }
            let inspected = self.inspect_cloud_file(connection.clone(), file.file.clone(), file.name.clone(), format).await;
            match inspected {
                Ok(inspection) => {
                    self.db.admit_inspected_provider_book(parent, file.file, format, inspection)?;
                    self.db.place_provider_book(id, parent)?;
                    self.events.notify_contents_changed();
                }
                Err(error) => {
                    log::warn!("could not inspect cloud book {id}: {error}");
                    failed = true;
                }
            }
        }
        for (id, file) in self.db.provider_remote_books()? {
            if !present.contains(&file.identity.file_id) {
                self.db.mark_provider_unavailable(&id)?;
            }
        }
        self.events.sync.wake();
        if failed {
            Err(BackendError::message("some cloud books still require inspection"))
        } else {
            Ok(())
        }
    }

    async fn cloud_downloads(&self, connection: &ConnectedDrive) -> Result<(), BackendError> {
        let (mut after, mut after_id) = (-1, None);
        let mut failure = None;
        loop {
            let page = self.db.book_download_requests(after, after_id.as_ref(), 32)?;
            let Some((last, at)) = page.last() else { break };
            (after, after_id) = (*at, Some(*last));
            for (id, _) in page {
                let origin = self.db.book_download_origin(&id)?.unwrap_or(TransferOrigin::Background);
                if origin == TransferOrigin::Background && !client_platform_runtime::transfers::allowed() {
                    continue;
                }
                if let Err(error) = self.download_cloud_book(connection, id).await {
                    log::warn!("could not download cloud book {id}: {error}");
                    if origin == TransferOrigin::UserInitiated {
                        self.db.cancel_download(&id)?;
                        self.events.publish_update(library_model::LibraryUpdate::DownloadFailed { content_hash: id, failure: error.failure() });
                    }
                    failure.get_or_insert(error);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn library_replica_folder_name(name: &str) -> String {
    // Shared projection handles portable filenames and collision suffixes.
    let trimmed = name.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        "Folder".into()
    } else {
        trimmed.replace(['/', '\\'], "_")
    }
}
