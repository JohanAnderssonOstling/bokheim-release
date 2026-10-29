use super::*;
use sync_common::LibraryId;

impl SyncPage {
    pub(super) fn copy_storage(&mut self, library_id: LibraryId, bokheim: bool, cx: &mut Context<Self>) {
        if !self.storage_copy_pending.insert(library_id) {
            return;
        }
        self.error = None;
        self.message = Some("Copying library… The original cloud files will be kept.".into());
        #[cfg(target_arch = "wasm32")]
        self.send_event(SyncEvent::StorageCopyRequested { library_id, bokheim }, cx);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let backend = self.services.backend.clone();
            cx.spawn(async move |page, cx| {
                let result = backend.copy_library_storage(library_id, bokheim).await;
                let _ = page.update(cx, |page, cx| page.finish_storage_copy(library_id, bokheim, result, cx));
            })
            .detach();
        }
        cx.notify();
    }

    pub(super) fn finish_storage_copy(&mut self, library_id: LibraryId, bokheim: bool, result: Result<app::LibraryEntry, app::BackendError>, cx: &mut Context<Self>) {
        self.storage_copy_pending.remove(&library_id);
        self.message = None;
        match result {
            Ok(entry) => {
                self.error = None;
                self.message = Some(
                    if bokheim {
                        format!("Created ‘{}’. Uploads to Bokheim will continue in the background. The original cloud files have been kept.", entry.library_name())
                    } else {
                        format!("Created ‘{}’ on this device. The original cloud files have been kept.", entry.library_name())
                    }
                    .into(),
                );
                self.local_usage_due = true;
                self.schedule_refresh(cx);
            }
            Err(error) => {
                let recovery = user_facing_error(&error, "The library could not be copied. Check your connection and storage, then retry.");
                self.error = Some(format!("{recovery} The original library and completed copies have been kept. Choose the same destination to resume.").into());
            }
        }
        cx.notify();
    }
}
