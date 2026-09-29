//! A library-owned snapshot for app-level transfer presentation.

use super::LibrarySession;
use crate::asset_workflow::AssetPlacementWorkflow;
use crate::BackendError;

impl LibrarySession {
    pub(crate) fn transfer_snapshot(&self) -> Result<crate::LibraryTransfers, BackendError> {
        let sync = self.sync().ok_or_else(|| BackendError::message("library sync is unavailable"))?;
        let library_name = self.db.sync_remote_library_name().map_err(BackendError::operation)?.unwrap_or_else(|| "Library".to_owned());
        let (file_work_pending, file_work_error) = self.assets.placement_work(&self.db).map_err(BackendError::operation)?;
        let file_work_error = if file_work_pending { self.file_work_error.borrow().clone().or(file_work_error) } else { None };
        let mut transfers = sync.transfer_statuses();
        let mut history = sync.transfer_history();
        let hashes: Vec<crate::ContentHash> =
            transfers.iter_mut().chain(history.iter_mut()).filter_map(|transfer| if transfer.state == crate::TransferState::Failed { transfer.failed_content_hash } else { Some(transfer.content_hash) }).collect();
        let file_names: std::collections::HashMap<crate::ContentHash, String> = self.db.transfer_first_file_names(&hashes).map_err(BackendError::operation)?.into_iter().collect();
        for transfer in transfers.iter_mut().chain(history.iter_mut()) {
            let hash = if transfer.state == crate::TransferState::Failed { transfer.failed_content_hash } else { Some(transfer.content_hash) };
            transfer.file_name = hash.and_then(|hash| file_names.get(&hash).cloned());
        }
        let scanning = self.operation_scanning();
        let scan_progress = self.directory_import_progress().or_else(|| scanning.then(|| self.scan_progress()).flatten());
        let signed_in = sync.account().map(|account| account.current().ok().flatten().is_some()).unwrap_or(false);
        let library_database::OperationStatus { operation, total_books, uploaded_books } = self
            .db
            .operation_status(library_database::OperationActivity {
                scanning,
                discovered: scan_progress.map_or(0, |progress| progress.total),
                scan_failures: scan_progress.map_or(0, |progress| progress.failed),
                local_busy: file_work_pending || self.thumbnail_work_running(),
                local_waiting: self.thumbnail_batch_waiting(),
                sync_busy: sync.metadata_sync_running()
                    || sync.preparing_transfers()
                    || transfers.iter().any(|transfer| transfer.origin == crate::TransferOrigin::Background && matches!(transfer.state, crate::TransferState::Running | crate::TransferState::Queued)),
                signed_in,
                asset_uploads_disabled: !sync.asset_storage_enabled(),
                file_error: file_work_pending.then_some(file_work_error.as_deref()).flatten(),
                transfer_retrying: transfers.iter().any(|transfer| transfer.origin == crate::TransferOrigin::Background && matches!(transfer.state, crate::TransferState::Retrying | crate::TransferState::Failed)),
            })
            .map_err(BackendError::operation)?;
        let storage_quota_blocked = sync.asset_storage_enabled()
            && (self.db.book_uploads_waiting_for_storage().map_err(BackendError::operation)?
                || transfers.iter().any(|transfer| {
                    transfer.kind == crate::TransferJobKind::UploadBook
                        && matches!(transfer.state, crate::TransferState::Retrying | crate::TransferState::Failed)
                        && matches!(transfer.last_error.as_deref(), Some("book storage quota exceeded" | "batch book upload HTTP 507"))
                }));
        Ok(crate::LibraryTransfers {
            storage_quota_blocked,
            operation,
            file_work_pending,
            file_work_error,
            library_id: self.id,
            library_name,
            scanner_running: scanning || self.directory_import_progress().is_some(),
            thumbnail_work_running: self.thumbnail_work_running(),
            thumbnail_progress: self.thumbnail_batch_progress(),
            thumbnail_waiting: self.thumbnail_batch_waiting(),
            thumbnail_download_coverage: self.db.thumbnail_download_coverage().map_err(BackendError::operation)?,
            metadata_sync_running: sync.metadata_sync_running(),
            preparing_transfers: sync.preparing_transfers(),
            total_books,
            uploaded_books,
            scan_progress,
            transfers,
            history,
        })
    }
}
