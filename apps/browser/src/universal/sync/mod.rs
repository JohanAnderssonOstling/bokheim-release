//! Account, library sync state and transfer queue.
//!
//! Universal in full: it holds account identity and aggregates *across* every
//! library, so it must outlive any one selected-library root. Reconciling the
//! library list is reported upward so the root can update the shared
//! [`Libraries`] rather than this page reaching into the navigation UI tree.

mod account;
#[cfg(target_os = "android")]
mod android_authentication;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod authentication;
mod transfers;
mod view;
#[cfg(target_arch = "wasm32")]
mod web_authentication;

use std::sync::Arc;

use app::{AccountStatus, AccountStorageUsage, LibrarySyncState, LibrarySyncStatus, LocalLibraryStorageUsage, ServerLibraryStorageUsage};
#[cfg(not(target_arch = "wasm32"))]
use gpui::Task;
use gpui::prelude::*;
use gpui::{Context, Div, Entity, IntoElement, Render, SharedString, Subscription, Window, div, px};
use gpui_component::IconName;
use gpui_component::alert::Alert;
use gpui_component::button::ButtonVariants;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use gpui_component::input::{Input, InputState, OtpInput};
use gpui_component::{Sizable, Size};
use ui_components as components;

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use crate::model::AccountFlow;
use crate::navigation::Libraries;
use crate::services::AppServices;

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use self::authentication::AuthenticationInputs;
use self::transfers::{ActivityState, LibraryActivity, LibraryActivitySummary};
pub(crate) use self::view::LibrariesView;

/// Window-wide sync data. ApplicationRoot owns this entity and Settings only
/// reads it; the SyncPage keeps form and focus state separate.
pub struct SyncState {
    pub(crate) account_status: AccountStatus,
    pub(crate) storage_usage: Option<AccountStorageUsage>,
    pub(crate) sync_statuses: Vec<LibrarySyncStatus>,
    pub(crate) local_storage_usage: Vec<LocalLibraryStorageUsage>,
    pub(crate) server_storage_usage: Option<Vec<ServerLibraryStorageUsage>>,
    /// Shared with the activity display cache without copying transfer rows.
    pub(crate) transfer_groups: Arc<Vec<app::LibraryTransfers>>,
}

impl SyncState {
    pub fn new(services: &AppServices) -> Self {
        Self { account_status: services.startup.account_status.clone(), storage_usage: None, sync_statuses: Vec::new(), local_storage_usage: Vec::new(), server_storage_usage: None, transfer_groups: Arc::new(Vec::new()) }
    }

    pub fn apply_account_snapshot(&mut self, snapshot: &app::AccountSnapshot, cx: &mut Context<Self>) {
        self.account_status = snapshot.account.clone();
        self.storage_usage = snapshot.storage_usage;
        self.sync_statuses = snapshot.sync_statuses.clone();
        self.server_storage_usage = snapshot.server_storage_usage.clone();
        cx.notify();
    }

    pub fn apply_local_storage_usage(&mut self, usage: Vec<LocalLibraryStorageUsage>, cx: &mut Context<Self>) {
        self.local_storage_usage = usage;
        cx.notify();
    }

    #[cfg(target_arch = "wasm32")]
    pub fn apply_transfer_snapshot(&mut self, groups: Vec<app::LibraryTransfers>, cx: &mut Context<Self>) {
        self.transfer_groups = Arc::new(groups);
        cx.notify();
    }

    #[cfg(target_arch = "wasm32")]
    pub fn apply_asset_storage(&mut self, library_id: sync_common::LibraryId, enabled: bool, cx: &mut Context<Self>) {
        for status in self.sync_statuses.iter_mut().filter(|status| status.library_id == library_id) {
            status.asset_storage_enabled = enabled;
        }
        cx.notify();
    }
}

/// Results enter the application root before any Settings entity is updated.
/// No variant carries a view handle, so a worker completion can only enqueue
/// data while another browser callback owns GPUI's AppCell.
#[cfg(target_arch = "wasm32")]
pub enum SyncEvent {
    AccountRefreshDue,
    AccountRefreshRequested,
    AccountSnapshot { generation: u64, result: Result<app::AccountSnapshot, String> },
    LocalStorageUsageRequested,
    LocalStorageUsage { generation: u64, result: Result<Vec<app::LocalLibraryStorageUsage>, String> },
    LogoutRequested,
    AccountOperation { generation: u64, result: Result<app::AccountSnapshot, String>, success: &'static str, reveal_password_reset_on_error: bool },
    TransferRefreshDue,
    TransferRefreshWake,
    TransferRefreshRequested,
    TransferDisplayTick,
    TransferSnapshot { generation: u64, result: Result<Vec<app::LibraryTransfers>, String> },
    TransferApplied { result: Result<(), String> },
    AssetStorageRequested { library_id: sync_common::LibraryId, enabled: bool },
    AssetStorageChanged { library_id: sync_common::LibraryId, enabled: bool, result: Result<(), String> },
    Authenticate { action: String, email: String, secret: String, token: String, reply: async_channel::Sender<Result<(), String>> },
    WebAuthenticated,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountActivity {
    Idle,
    Refreshing,
    AccountRequest,
}

pub(crate) struct SyncPage {
    #[cfg(not(target_arch = "wasm32"))]
    services: AppServices,
    #[cfg(target_arch = "wasm32")]
    events: async_channel::Sender<SyncEvent>,
    state: Entity<SyncState>,
    active_library: Entity<Libraries>,
    identity: Entity<view::AccountIdentityView>,
    asset_storage_pending: std::collections::HashSet<sync_common::LibraryId>,
    /// Where a new library is created. It belongs to the library list rather
    /// than to a storage group of its own: it is the setting that decides where
    /// the next row in that list will live. `None` until the backend answers,
    /// and on platforms that do not let the user choose.
    #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
    default_library_location: Option<String>,
    activity_jobs: Entity<LibraryActivity>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    flow: AccountFlow,
    /// Whether the account form is showing. It is opened by a header button and
    /// by nothing else: signed out, the group is the two ways in until one of
    /// them is pressed.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    access_open: bool,
    /// Whether the password field on the current flow is showing its text. One
    /// flag, because only one flow is on screen and each carries one password.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    password_revealed: bool,
    activity: AccountActivity,
    /// Password recovery is offered only after an actual sign-in request has
    /// failed, rather than as a permanent third authentication mode.
    login_failed: bool,
    message: Option<SharedString>,
    error: Option<SharedString>,
    #[cfg(not(target_arch = "wasm32"))]
    task: Option<Task<()>>,
    refresh_scheduled: bool,
    refresh_pending: bool,
    /// File sizes change independently of account notifications. Read them
    /// on entry, explicit refresh, and when the library set changes.
    local_usage_due: bool,
    local_usage_libraries: std::collections::HashSet<sync_common::LibraryId>,
    #[cfg(not(target_arch = "wasm32"))]
    local_usage_generation: u64,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    authentication: AuthenticationInputs,
    #[cfg(target_arch = "wasm32")]
    web_authentication: Option<web_authentication::WebAuthentication>,
    #[cfg(target_os = "android")]
    android_authentication: Option<android_authentication::AndroidAuthentication>,
    _active_library_subscription: Subscription,
    _activity_subscription: Subscription,
    _state_subscription: Subscription,
}

/// Native compatibility wrapper while other callback paths are migrated.
#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn after_browser_callback(cx: &gpui::AsyncApp) {
    cx.wait_for_update_slot().await;
}

impl SyncPage {
    fn is_busy(&self) -> bool {
        self.activity != AccountActivity::Idle
    }

    pub(crate) fn new(services: AppServices, state: Entity<SyncState>, active_library: Entity<Libraries>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let account_status = state.read(cx).account_status.clone();
        let local_usage_libraries = active_library.read(cx).entries().iter().map(|entry| *entry.library_id()).collect();
        let page_entity = cx.entity();
        let identity = cx.new(|_| view::AccountIdentityView::new(page_entity, account_status.clone()));
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        let authentication = AuthenticationInputs::new(window, cx);
        let activity_jobs = cx.new(|cx| {
            LibraryActivity::new(
                #[cfg(not(target_arch = "wasm32"))]
                services.backend.clone(),
                #[cfg(target_arch = "wasm32")]
                services.sync_events.clone(),
                cx,
            )
        });
        let activity_on_library_change = activity_jobs.clone();
        let active_library_subscription = cx.observe(&active_library, move |page, _, cx| {
            let library_ids = page.active_library.read(cx).entries().iter().map(|entry| *entry.library_id()).collect();
            if page.local_usage_libraries != library_ids {
                page.local_usage_libraries = library_ids;
                page.local_usage_due = true;
            }
            page.schedule_refresh(cx);
            activity_on_library_change.update(cx, |activity, cx| activity.refresh_now(cx));
            cx.notify();
        });
        // The queue lives in an entity of its own so its own refreshes stay
        // cheap, but the library rows read from it, so this page redraws with
        // it. Only the account section is rebuilt: the settings controls around
        // it belong to the page above and are untouched.
        let activity_subscription = cx.observe(&activity_jobs, |_, _, cx| cx.notify());
        let state_subscription = cx.observe(&state, |page, state, cx| {
            let account_status = state.read(cx).account_status.clone();
            page.identity.update(cx, |identity, cx| identity.set_account_status(account_status, cx));
            cx.notify();
        });
        let page = Self {
            #[cfg(not(target_arch = "wasm32"))]
            services,
            #[cfg(target_arch = "wasm32")]
            events: services.sync_events.clone().expect("web sync event inbox"),
            state,
            active_library,
            identity,
            asset_storage_pending: std::collections::HashSet::new(),
            #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
            default_library_location: None,
            activity_jobs,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            flow: AccountFlow::SignIn,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            access_open: false,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            password_revealed: false,
            activity: AccountActivity::Idle,
            login_failed: false,
            message: None,
            error: None,
            #[cfg(not(target_arch = "wasm32"))]
            task: None,
            refresh_scheduled: false,
            refresh_pending: false,
            local_usage_due: true,
            local_usage_libraries,
            #[cfg(not(target_arch = "wasm32"))]
            local_usage_generation: 0,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            authentication,
            #[cfg(target_arch = "wasm32")]
            web_authentication: None,
            #[cfg(target_os = "android")]
            android_authentication: None,
            _active_library_subscription: active_library_subscription,
            _activity_subscription: activity_subscription,
            _state_subscription: state_subscription,
        };
        // Start snapshots from a normal window turn, never from an async
        // worker callback or a render borrow.
        cx.on_next_frame(window, |page, _, cx| {
            page.refresh(cx);
            page.activity_jobs.update(cx, |activity, cx| activity.refresh_now(cx));
        });
        #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
        {
            let backend = page.services.backend.clone();
            cx.spawn(async move |page, cx| {
                let result = backend.default_library_save_location().await;
                let _ = page.update(cx, |page, cx| {
                    match result {
                        Ok(path) => page.default_library_location = path,
                        Err(error) => page.error = Some(error.into()),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        page
    }
}

#[cfg(target_arch = "wasm32")]
impl SyncPage {
    pub(crate) fn apply_event(&mut self, event: SyncEvent, cx: &mut Context<Self>) {
        match event {
            SyncEvent::AccountRefreshDue => {
                self.refresh_scheduled = false;
                self.refresh_with_feedback(false, cx);
            }
            SyncEvent::AccountSnapshot { result, .. } => match result {
                Ok(snapshot) => self.apply_snapshot(snapshot, None, cx),
                Err(error) => self.apply_error(error, cx),
            },
            SyncEvent::AccountOperation { result, success, reveal_password_reset_on_error, .. } => match result {
                Ok(snapshot) => {
                    self.login_failed = false;
                    self.apply_snapshot(snapshot, Some(success), cx);
                }
                Err(error) => {
                    self.login_failed = reveal_password_reset_on_error;
                    self.apply_error(error, cx);
                }
            },
            SyncEvent::TransferRefreshDue => {
                self.activity_jobs.update(cx, |activity, cx| activity.refresh_due(cx));
            }
            SyncEvent::TransferRefreshWake => {
                self.activity_jobs.update(cx, |activity, cx| activity.refresh_now(cx));
            }
            SyncEvent::TransferDisplayTick => {
                self.activity_jobs.update(cx, |activity, cx| activity.display_tick(cx));
            }
            SyncEvent::TransferApplied { result } => {
                let groups = self.state.read(cx).transfer_groups.clone();
                self.activity_jobs.update(cx, |activity, cx| activity.apply_result(result, groups, cx));
            }
            SyncEvent::AssetStorageChanged { library_id, enabled, result } => {
                self.asset_storage_pending.remove(&library_id);
                match result {
                    Ok(()) => self.schedule_refresh(cx),
                    Err(error) => self.error = Some(error.into()),
                }
                let _ = enabled;
                cx.notify();
            }
            SyncEvent::WebAuthenticated => self.schedule_refresh(cx),
            SyncEvent::AccountRefreshRequested | SyncEvent::LocalStorageUsageRequested | SyncEvent::LocalStorageUsage { .. } | SyncEvent::LogoutRequested | SyncEvent::TransferRefreshRequested | SyncEvent::TransferSnapshot { .. } | SyncEvent::AssetStorageRequested { .. } | SyncEvent::Authenticate { .. } => {
                unreachable!("sync commands are handled by ApplicationRoot")
            }
        }
    }
}
