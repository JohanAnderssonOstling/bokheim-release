//! Device-owned recovery starts when filesystem access detects a lost root.

use super::core::{AppBackend, AppState};
use crate::executor::{self, BackendExecutor, BackendTask};
use crate::BackendError;
use library_backend::library::handle::{LibraryActorCommand, LibraryControl};
use library_registry::LibraryRootRecovery;
use std::collections::HashSet;
use std::sync::Weak;
use std::time::Duration;
use sync_common::LibraryId;

pub(super) fn start(executor: &BackendExecutor, state: Weak<AppState>, requests: async_channel::Receiver<LibraryId>) -> BackendTask<()> {
    executor.spawn(async move {
        executor::library_maintenance_ready().await;
        let mut pending = HashSet::new();
        loop {
            if pending.is_empty() {
                let Ok(id) = requests.recv().await else { return };
                pending.insert(id);
            }
            while let Ok(id) = requests.try_recv() {
                pending.insert(id);
            }
            let Some(state) = state.upgrade() else { return };
            let backend = AppBackend { state };
            let mut retry = HashSet::new();
            for id in pending.drain() {
                if let Err(error) = backend.recover_library_root(id).await {
                    log::warn!("library folder recovery for {id} will retry: {error}");
                    retry.insert(id);
                }
            }
            drop(backend);
            pending = retry;
            if !pending.is_empty() {
                tokio::select! {
                    result = executor::sleep(Duration::from_secs(30)) => if result.is_err() { return; },
                    request = requests.recv() => match request {
                        Ok(id) => { pending.insert(id); },
                        Err(_) => return,
                    },
                }
            }
        }
    })
}

impl AppBackend {
    pub(super) async fn recover_library_root(&self, id: LibraryId) -> Result<(), BackendError> {
        let result = async {
            let prepared = self.with_storage(move |this| {
                // A queued failure may outlive removal of its library.
                if this.state.registry.entry(&id).map_err(BackendError::operation)?.is_none() || this.state.registry.is_detached(&id).map_err(BackendError::operation)? {
                    return Ok(None);
                }
                let recovered = this.state.registry.recover_library_root(&id).map_err(BackendError::operation)?;
                let handle = this.state.library_directory.try_get(&id)?;
                let entry = match recovered {
                    LibraryRootRecovery::Unchanged(entry) | LibraryRootRecovery::Recovered(entry) => entry,
                };
                // Access already reported a failed storage identity. Reopen
                // even if the original mount returned before discovery ran.
                let (spec, _) = this.library_runtime_spec(entry.storage_locator(), id)?;
                Ok(Some((handle, spec)))
            })?;
            let Some((handle, spec)) = prepared else {
                return Ok(());
            };
            match handle {
                Some(handle) => {
                    let (reply, result) = async_channel::bounded(1);
                    handle.commands.send(LibraryActorCommand::RecoverRoot { config: spec.config, reply }).await.map_err(|_| BackendError::message("library stopped before folder recovery"))?;
                    tokio::select! {
                        result = result.recv() => result.map_err(BackendError::operation)??,
                        () = handle.commands.closed() => return Err(BackendError::message("library stopped during folder recovery")),
                    }
                }
                None => {
                    self.state.library_directory.open_recovered(spec, &self.state.executor)?;
                }
            }
            self.state.registry.remember_library_root(&id).map_err(BackendError::operation)?;
            self.send_library_list_changed();
            Ok(())
        }
        .await;
        if result.is_err() {
            if let Some(handle) = self.state.library_directory.try_get(&id)? {
                let _ = handle.commands.try_send(LibraryActorCommand::Control(LibraryControl::RootUnavailable));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AppDataLocation, BackendContext};
    use library_backend::LibraryClient;
    use library_model::{BrowseLocation, DownloadState, LibraryBrowseQuery, LibraryUpdate};
    use std::{fs, path::Path};
    use sync_common::ROOT_DIR_ID;

    fn query() -> LibraryBrowseQuery {
        LibraryBrowseQuery {
            location: BrowseLocation::Books,
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: Default::default(),
            book_sort: Default::default(),
            hide_finished: false,
        }
    }

    #[tokio::test]
    async fn a_failed_file_lookup_triggers_recovery_without_a_polling_tick() {
        let directory = tempfile::tempdir().unwrap();
        let context = BackendContext::initialize(AppDataLocation::native_path(directory.path().join("app"))).unwrap();
        let backend = AppBackend::new(context).unwrap();
        let entry = backend.create_library_with_asset_storage("Books".into(), false).unwrap();
        let id = *entry.library_id();
        let client = LibraryClient::new(id, backend.state.library_directory.get(&id).unwrap());
        let hash = client.import_downloaded_book(ROOT_DIR_ID, "Book.epub".into(), include_bytes!("../../../library-backend/tests/fixtures/cloud-cover.epub").to_vec()).await.unwrap();
        let root = Path::new(entry.storage_locator());
        // Hidden metadata changes are deliberately ignored by the file watcher.
        fs::remove_dir_all(root.join(".bokheim")).unwrap();
        fs::remove_file(root.join("Book.epub")).unwrap();
        assert_eq!(client.download_state(hash).await.unwrap(), DownloadState::NotDownloaded);
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if root.join(".bokheim/library.json").exists() && client.browse(query()).await.unwrap().contents.books.is_empty() {
                    break;
                }
                executor::sleep(Duration::from_millis(20)).await.unwrap();
            }
        })
        .await
        .expect("file lookup failure must wake recovery immediately");
        assert_eq!(library_registry::read_library_manifest(root).unwrap().unwrap().0, id);
        assert_eq!(client.book_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn dismount_and_remount_preserve_clients_reading_state_and_subscriptions() {
        let directory = tempfile::tempdir().unwrap();
        let context = BackendContext::initialize(AppDataLocation::native_path(directory.path().join("app"))).unwrap();
        let backend = AppBackend::new(context).unwrap();
        let entry = backend.create_library_with_asset_storage("Books".into(), false).unwrap();
        let id = *entry.library_id();
        let handle = backend.state.library_directory.get(&id).unwrap();
        let client = LibraryClient::new(id, handle);
        let hash = client.import_downloaded_book(ROOT_DIR_ID, "Book.epub".into(), include_bytes!("../../../library-backend/tests/fixtures/cloud-cover.epub").to_vec()).await.unwrap();
        client.update_reading_position(hash, "epubcfi(/6/2)".into(), Some(0.5), None).await.unwrap();
        let (updates, _) = client.updates().await.unwrap();
        assert_eq!(client.browse(query()).await.unwrap().contents.books.len(), 1);

        let root = Path::new(entry.storage_locator());
        let parked = directory.path().join("disconnected");
        fs::rename(root, &parked).unwrap();
        fs::create_dir(root).unwrap(); // The empty directory underneath a mount.
        backend.recover_library_root(id).await.unwrap();

        assert_eq!(library_registry::read_library_manifest(root).unwrap().unwrap().0, id);
        assert!(!root.join("Book.epub").exists());
        assert_eq!(client.book_count().await.unwrap(), 1, "canonical membership survives");
        assert!(client.browse(query()).await.unwrap().contents.books.is_empty());
        assert_eq!(client.download_state(hash).await.unwrap(), DownloadState::NotDownloaded);
        assert_eq!(client.reading_position(hash).await.unwrap().as_deref(), Some("epubcfi(/6/2)"));
        let mut changed = false;
        while let Ok(update) = updates.try_recv() {
            changed |= matches!(update, LibraryUpdate::Download(status) if status.content_hash == hash && status.state == DownloadState::NotDownloaded);
        }
        assert!(changed, "existing cards must receive the new download state");

        fs::remove_dir_all(root).unwrap();
        fs::rename(&parked, root).unwrap();
        // Reading availability detects the returning storage identity and
        // wakes recovery even when the old filesystem watcher misses remount.
        assert_eq!(client.download_state(hash).await.unwrap(), DownloadState::NotDownloaded);
        tokio::time::timeout(Duration::from_secs(10), async {
            while client.download_state(hash).await.unwrap() != DownloadState::Downloaded {
                executor::sleep(Duration::from_millis(20)).await.unwrap();
            }
        })
        .await
        .expect("a returning mount must be rescanned");
        assert_eq!(client.browse(query()).await.unwrap().contents.books.len(), 1);
        assert_eq!(client.reading_position(hash).await.unwrap().as_deref(), Some("epubcfi(/6/2)"));
        assert!(!matches!(updates.try_recv(), Err(async_channel::TryRecvError::Closed)));
    }

    #[tokio::test]
    async fn a_relocated_root_keeps_the_existing_client_connected() {
        let directory = tempfile::tempdir().unwrap();
        let context = BackendContext::initialize(AppDataLocation::native_path(directory.path())).unwrap();
        let backend = AppBackend::new(context).unwrap();
        let entry = backend.create_library_with_asset_storage("Books".into(), false).unwrap();
        let id = *entry.library_id();
        let client = LibraryClient::new(id, backend.state.library_directory.get(&id).unwrap());
        let (updates, _) = client.updates().await.unwrap();
        let root = Path::new(entry.storage_locator());
        let moved = root.with_file_name("Moved");
        fs::rename(root, &moved).unwrap();
        backend.recover_library_root(id).await.unwrap();
        assert_eq!(Path::new(backend.library_entry(&id).unwrap().storage_locator()), moved);
        assert!(!root.exists());
        assert_eq!(client.book_count().await.unwrap(), 0);
        assert!(!matches!(updates.try_recv(), Err(async_channel::TryRecvError::Closed)));
    }
}
