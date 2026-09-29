//! Loads account and library state; local sizes and transfers arrive separately.

use gpui::Context;

use super::SyncPage;
#[cfg(not(target_arch = "wasm32"))]
use crate::services::UiFuture;
#[cfg(not(target_arch = "wasm32"))]
use app::AppClient;

pub(super) type CloudSnapshot = app::AccountSnapshot;

#[cfg(not(target_arch = "wasm32"))]
fn snapshot(backend: &AppClient) -> UiFuture<CloudSnapshot> {
    let backend = backend.clone();
    Box::pin(async move { backend.account_snapshot().await })
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn login(backend: &AppClient, email: String, password: String) -> UiFuture<CloudSnapshot> {
    let backend = backend.clone();
    Box::pin(async move { backend.login(email, password).await })
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn verify_email(backend: &AppClient, email: String, pin: String) -> UiFuture<CloudSnapshot> {
    let backend = backend.clone();
    Box::pin(async move { backend.verify_email(email, pin).await })
}

impl SyncPage {
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_with_feedback(true, cx);
    }

    /// Coalesces bursts of account and synchronization notifications.
    pub(crate) fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_scheduled {
            return;
        }
        self.refresh_scheduled = true;
        let timer = cx.background_executor().timer(std::time::Duration::from_millis(200));
        #[cfg(target_arch = "wasm32")]
        let events = self.events.clone();
        cx.spawn(async move |page, cx| {
            timer.await;
            #[cfg(target_arch = "wasm32")]
            {
                let _ = events.try_send(super::SyncEvent::AccountRefreshDue);
                let _ = (page, cx);
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                super::after_browser_callback(cx).await;
                let _ = page.update(cx, |page, cx| {
                    page.refresh_scheduled = false;
                    page.refresh_with_feedback(false, cx);
                });
            }
        })
        .detach();
    }

    pub(super) fn refresh_with_feedback(&mut self, show_feedback: bool, cx: &mut Context<Self>) {
        if show_feedback {
            self.local_usage_due = true;
        }
        if self.is_busy() {
            if !show_feedback {
                self.refresh_pending = true;
            }
            return;
        }
        self.activity = super::AccountActivity::Refreshing;
        self.error = None;
        // Signed out, this reloads the local library list, not anything
        // account-related; showing "Refreshing…" would imply
        // account/network activity that isn't happening.
        if show_feedback && self.state.read(cx).account_status.signed_in() {
            self.message = Some("Refreshing…".into());
        }
        cx.notify();
        #[cfg(target_arch = "wasm32")]
        {
            let _ = self.events.try_send(super::SyncEvent::AccountRefreshRequested);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let operation = snapshot(&self.services.backend);
            self.task = Some(cx.spawn(async move |page, cx| {
                let result = operation.await;
                super::after_browser_callback(cx).await;
                let _ = page.update(cx, |page, cx| match result {
                    Ok(snapshot) => page.apply_snapshot(snapshot, None, cx),
                    Err(error) => page.apply_error(error, cx),
                });
            }));
        }
    }

    pub(super) fn apply_snapshot(&mut self, snapshot: CloudSnapshot, message: Option<&str>, cx: &mut Context<Self>) {
        #[cfg(not(target_arch = "wasm32"))]
        self.state.update(cx, |state, cx| state.apply_account_snapshot(&snapshot, cx));
        #[cfg(target_arch = "wasm32")]
        let _ = snapshot;
        let account_status = self.state.read(cx).account_status.clone();
        #[cfg(target_arch = "wasm32")]
        if account_status.signed_in() {
            self.web_authentication = None;
        }
        self.identity.update(cx, |identity, cx| identity.set_account_status(account_status.clone(), cx));
        // The form was for reaching an account; close it after sign-in.
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        if account_status.signed_in() {
            self.access_open = false;
        }
        self.activity = super::AccountActivity::Idle;
        self.error = None;
        self.message = message.map(Into::into);
        let refresh_again = self.refresh_pending;
        self.refresh_pending = false;
        cx.notify();
        if std::mem::take(&mut self.local_usage_due) {
            #[cfg(target_arch = "wasm32")]
            let _ = self.events.try_send(super::SyncEvent::LocalStorageUsageRequested);
            #[cfg(not(target_arch = "wasm32"))]
            self.request_local_storage_usage(cx);
        }
        if refresh_again {
            self.schedule_refresh(cx);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn request_local_storage_usage(&mut self, cx: &mut Context<Self>) {
        self.local_usage_generation = self.local_usage_generation.wrapping_add(1);
        let generation = self.local_usage_generation;
        let backend = self.services.backend.clone();
        cx.spawn(async move |page, cx| {
            let result = backend.local_storage_usage().await;
            super::after_browser_callback(cx).await;
            let _ = page.update(cx, |page, cx| {
                if generation != page.local_usage_generation {
                    return;
                }
                match result {
                    Ok(usage) => page.state.update(cx, |state, cx| state.apply_local_storage_usage(usage, cx)),
                    Err(error) => log::warn!("could not read local storage usage: {error}"),
                }
            });
        })
        .detach();
    }

    pub(super) fn apply_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.activity = super::AccountActivity::Idle;
        self.message = None;
        self.error = Some(error.into());
        cx.notify();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn start_snapshot_operation(&mut self, progress: &'static str, success: &'static str, reveal_password_reset_on_error: bool, operation: UiFuture<CloudSnapshot>, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        self.activity = super::AccountActivity::AccountRequest;
        if reveal_password_reset_on_error {
            self.login_failed = false;
        }
        self.error = None;
        self.message = Some(progress.into());
        cx.notify();
        self.task = Some(cx.spawn(async move |page, cx| {
            let result = operation.await;
            super::after_browser_callback(cx).await;
            let _ = page.update(cx, |page, cx| match result {
                Ok(snapshot) => {
                    page.login_failed = false;
                    page.apply_snapshot(snapshot, Some(success), cx);
                }
                Err(error) => {
                    page.login_failed = reveal_password_reset_on_error;
                    page.apply_error(error, cx);
                }
            });
        }));
    }

    pub(super) fn start_logout(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_arch = "wasm32")]
        {
            if self.is_busy() {
                return;
            }
            self.activity = super::AccountActivity::AccountRequest;
            self.error = None;
            self.message = Some("Signing out…".into());
            cx.notify();
            let _ = self.events.try_send(super::SyncEvent::LogoutRequested);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let backend = self.services.backend.clone();
            let operation = Box::pin(async move { backend.logout().await });
            self.start_snapshot_operation("Signing out…", "Signed out", false, operation, cx);
        }
    }
}

impl SyncPage {
    pub(super) fn change_asset_storage(&mut self, library_id: sync_common::LibraryId, enabled: bool, cx: &mut Context<Self>) {
        if !self.asset_storage_pending.insert(library_id) {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = self.events.try_send(super::SyncEvent::AssetStorageRequested { library_id, enabled });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let backend = self.services.backend.clone();
            cx.spawn(async move |page, cx| {
                let result = backend.set_library_asset_storage(library_id, enabled).await;
                super::after_browser_callback(cx).await;
                let _ = page.update(cx, |page, cx| {
                    page.asset_storage_pending.remove(&library_id);
                    match result {
                        Ok(()) => {
                            page.state.update(cx, |state, cx| {
                                for status in state.sync_statuses.iter_mut().filter(|status| status.library_id == library_id) {
                                    status.asset_storage_enabled = enabled;
                                }
                                cx.notify();
                            });
                            page.schedule_refresh(cx);
                        }
                        Err(error) => page.error = Some(error.into()),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }
}
