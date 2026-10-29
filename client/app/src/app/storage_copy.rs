//! Copy into a separate library; provider files and the source stay untouched.
use super::{core::AppBackend, LibraryEntry};
use crate::{BackendError, Failure};
use library_backend::library::StorageCopyProgress;
use serde::{Deserialize, Serialize};
use sync_common::{storage::LibraryStorage, LibraryId};

#[derive(Clone, Serialize, Deserialize)]
struct CopyJournal {
    destination: LibraryId,
    progress: StorageCopyProgress,
}

impl AppBackend {
    pub(crate) async fn copy_library_storage(&self, source: LibraryId, bokheim: bool) -> Result<LibraryEntry, BackendError> {
        // Requests from different windows must resume the same pending copy.
        let _copy = self.state.storage_copy_lock.lock().await;
        if bokheim && !self.account_status()?.signed_in() {
            return Err(BackendError::classified(Failure::SignInRequired, "sign in before copying a library to Bokheim storage"));
        }
        let key = format!("storage-copy:v1:{source}:{}", u8::from(bokheim));
        let journal_key = key.clone();
        let (entry, mut journal) = self.with_storage(move |this| {
            let original = this.library_entry(&source)?;
            let journal: Option<CopyJournal> = this.state.registry.read_value(&journal_key).map_err(BackendError::operation)?.map(|bytes| serde_json::from_slice(&bytes).map_err(BackendError::operation)).transpose()?;
            if let Some(journal) = journal {
                if let Some(entry) = this.state.registry.entry(&journal.destination).map_err(BackendError::operation)? {
                    let expected = if bokheim { LibraryStorage::Bokheim } else { LibraryStorage::Local };
                    if this.state.registry.storage(&journal.destination).map_err(BackendError::operation)? != expected {
                        return Err(BackendError::message("the copy's storage has changed; restore its storage setting before retrying"));
                    }
                    return Ok((entry, journal));
                }
            }
            let name = format!("{} ({})", original.library_name(), if bokheim { "Bokheim copy" } else { "local copy" });
            let entry = this.state.registry.create_with_asset_storage(&name, None, bokheim).map_err(BackendError::operation)?;
            let journal = CopyJournal { destination: *entry.library_id(), progress: Default::default() };
            this.state.registry.write_value(&journal_key, &serde_json::to_vec(&journal).map_err(BackendError::operation)?).map_err(BackendError::operation)?;
            Ok((entry, journal))
        })?;
        // Opening failures also leave the durable destination available to retry.
        self.open_registered_library(entry.library_id())?;
        self.send_library_list_changed();
        let original = self.state.library_directory.get(&source)?;
        let destination = self.state.library_directory.get(entry.library_id())?;
        let registry = self.state.registry.clone();
        let checkpoint_key = key.clone();
        destination
            .copy_storage_from(&original, journal.progress.clone(), move |progress| {
                journal.progress = progress.clone();
                registry.write_value(&checkpoint_key, &serde_json::to_vec(&journal).map_err(BackendError::operation)?).map_err(BackendError::operation)
            })
            .await?;
        self.state.registry.remove_value(&key).map_err(BackendError::operation)?;
        self.state.account_wake.send_replace(());
        self.send_library_list_changed();
        Ok(entry)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::app::data::{AppDataLocation, BackendContext};
    use library_backend::{LibraryCommand, PendingCommand};
    use sync_common::ROOT_DIR_ID;

    #[tokio::test]
    async fn bokheim_copy_requires_sign_in_before_creating_a_destination() {
        let root = tempfile::tempdir().unwrap();
        let backend = AppBackend::new(BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap()).unwrap();
        let source = backend.create_library_with_asset_storage("Source".into(), false).unwrap();
        let error = backend.copy_library_storage(*source.library_id(), true).await.unwrap_err();
        assert_eq!(error.failure(), Some(Failure::SignInRequired));
        assert_eq!(backend.libraries().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn local_copy_resumes_a_durable_destination_without_retiring_the_source() {
        let root = tempfile::tempdir().unwrap();
        let context = BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap();
        let backend = AppBackend::new(context).unwrap();
        let source = backend.create_library_with_asset_storage("Source".into(), false).unwrap();
        let source_id = *source.library_id();
        let handle = backend.state.library_directory.get(&source_id).unwrap();
        let bytes = include_bytes!("../../../library-backend/tests/fixtures/cloud-cover.epub");
        handle.dispatch(LibraryCommand::Pending(PendingCommand::ImportDownloadedBook { parent_id: ROOT_DIR_ID, file_name: "Book.epub".into(), bytes: bytes.to_vec().into() })).await.unwrap();
        let destination = backend.create_library_with_asset_storage("Pending copy".into(), false).unwrap();
        let key = format!("storage-copy:v1:{source_id}:0");
        backend.state.registry.write_value(&key, &serde_json::to_vec(&CopyJournal { destination: *destination.library_id(), progress: Default::default() }).unwrap()).unwrap();
        let copied = backend.copy_library_storage(source_id, false).await.unwrap();
        assert_eq!(copied.library_id(), destination.library_id());
        assert_eq!(backend.libraries().unwrap().len(), 2);
        assert!(backend.state.registry.read_value(&key).unwrap().is_none());
        assert!(backend.state.library_directory.get(&source_id).is_ok());
        let copied_bytes = std::fs::read(std::path::Path::new(copied.storage_locator()).join("Book.epub")).unwrap();
        assert_eq!(copied_bytes, bytes);
        assert!(std::path::Path::new(source.storage_locator()).join("Book.epub").exists());
    }
}
