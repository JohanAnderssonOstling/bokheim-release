//! Account and library sync state.
//!
//! Universal in full: it holds account identity and aggregates *across* every
//! library, so it must outlive any one selected-library root. Reconciling the
//! library list is reported upward so the root can update the shared
//! [`Libraries`] rather than this page reaching into the navigation UI tree.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use ui_components::user_facing_error;
mod account;
#[cfg(target_os = "android")]
mod android_authentication;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod authentication;
mod authentication_request;
mod deletion;
mod storage;
mod view;
#[cfg(target_arch = "wasm32")]
mod web_authentication;

use account_contract::StorageUsageResponse;
use app::{AccountStatus, LocalLibraryStorageUsage, ServerLibraryStorageUsage};
use gpui::prelude::*;
use gpui::{div, px, Context, Div, Entity, IntoElement, Render, SharedString, Subscription, Task, Window};
use gpui_component::alert::Alert;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use gpui_component::input::{Input, OtpInput};
use gpui_component::IconName;
use gpui_component::{Sizable, Size};
use library_backend::{LibrarySyncState, LibrarySyncStatus, LibraryUpdate};
use ui_components as components;

pub use self::authentication_request::AuthenticationRequest;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use crate::model::AccountFlow;
use crate::navigation::Libraries;
use crate::services::AppServices;

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use self::authentication::AuthenticationInputs;
pub(crate) use self::view::LibrariesView;

/// Window-wide sync data. ApplicationRoot owns this entity and Settings only
/// reads it; the SyncPage keeps form and focus state separate.
pub struct SyncState {
    pub(crate) account_status: AccountStatus,
    pub(crate) storage_usage: Option<StorageUsageResponse>,
    pub(crate) sync_statuses: Vec<LibrarySyncStatus>,
    pub(crate) local_storage_usage: Vec<LocalLibraryStorageUsage>,
    pub(crate) server_storage_usage: Option<Vec<ServerLibraryStorageUsage>>,
}

impl SyncState {
    pub fn new(services: &AppServices) -> Self {
        Self { account_status: services.startup.account_status.clone(), storage_usage: None, sync_statuses: Vec::new(), local_storage_usage: Vec::new(), server_storage_usage: None }
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
    LibrarySyncChanged,
    AccountRefreshRequested,
    AccountSnapshot { generation: u64, result: Result<app::AccountSnapshot, app::BackendError> },
    LocalStorageUsageRequested,
    LocalStorageUsage { generation: u64, result: Result<Vec<app::LocalLibraryStorageUsage>, app::BackendError> },
    LogoutRequested,
    DeleteAccountRequested { email: String, password: String },
    AccountOperation { generation: u64, result: Result<app::AccountSnapshot, app::BackendError>, success: &'static str, reveal_password_reset_on_error: bool },
    AssetStorageRequested { library_id: sync_common::LibraryId, enabled: bool },
    AssetStorageChanged { library_id: sync_common::LibraryId, enabled: bool, result: Result<(), app::BackendError> },
    StorageCopyRequested { library_id: sync_common::LibraryId, bokheim: bool },
    StorageCopyFinished { library_id: sync_common::LibraryId, bokheim: bool, result: Result<app::LibraryEntry, app::BackendError> },
    Authenticate { request: AuthenticationRequest, reply: async_channel::Sender<Result<(), &'static str>> },
    WebAuthenticated,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountActivity {
    Idle,
    Refreshing,
    AccountRequest,
    DeletingAccount,
}

pub(crate) struct SyncPage {
    #[cfg(not(target_arch = "wasm32"))]
    services: AppServices,
    #[cfg(target_arch = "wasm32")]
    events: async_channel::Sender<SyncEvent>,
    state: Entity<SyncState>,
    active_library: Entity<Libraries>,
    identity: Entity<view::AccountIdentityView>,
    deletion: Option<deletion::DeleteAccountForm>,
    asset_storage_pending: HashSet<sync_common::LibraryId>,
    storage_copy_pending: HashSet<sync_common::LibraryId>,
    /// Where a new library is created. It belongs to the library list rather
    /// than to a storage group of its own: it is the setting that decides where
    /// the next row in that list will live. `None` until the backend answers,
    /// and on platforms that do not let the user choose.
    #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
    default_library_location: Option<String>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    flow: AccountFlow,
    /// Whether the account form is showing. It is opened by a header button and
    /// by nothing else: signed out, the group is the two ways in until one of
    /// them is pressed.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    access_open: bool,
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
    local_usage_libraries: HashSet<sync_common::LibraryId>,
    sync_update_tasks: HashMap<sync_common::LibraryId, Task<()>>,
    #[cfg(not(target_arch = "wasm32"))]
    local_usage_generation: u64,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    authentication: AuthenticationInputs,
    #[cfg(target_arch = "wasm32")]
    web_authentication: Option<web_authentication::WebAuthentication>,
    #[cfg(target_os = "android")]
    android_authentication: Option<android_authentication::AndroidAuthentication>,
    _active_library_subscription: Subscription,
    _state_subscription: Subscription,
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
        let backend = services.backend.clone();
        let subscription_backend = backend.clone();
        let active_library_subscription = cx.observe(&active_library, move |page, _, cx| {
            let library_ids = page.active_library.read(cx).entries().iter().map(|entry| *entry.library_id()).collect();
            if page.local_usage_libraries != library_ids {
                page.local_usage_libraries = library_ids;
                page.local_usage_due = true;
                page.observe_library_sync(&subscription_backend, cx);
            }
            page.schedule_refresh(cx);
            cx.notify();
        });
        let state_subscription = cx.observe(&state, |page, state, cx| {
            let account_status = state.read(cx).account_status.clone();
            if page.deletion.as_ref().is_some_and(|form| account_status.email() != Some(form.email.as_str())) {
                page.deletion = None;
            }
            page.identity.update(cx, |identity, cx| identity.set_account_status(account_status, cx));
            cx.notify();
        });
        let mut page = Self {
            #[cfg(not(target_arch = "wasm32"))]
            services,
            #[cfg(target_arch = "wasm32")]
            events: services.sync_events.clone().unwrap_or_else(|| {
                log::error!("Web sync event inbox is unavailable");
                // A closed sender lets the existing command error path report
                // unavailable sync without queuing requests that cannot finish.
                let (sender, receiver) = async_channel::unbounded();
                drop(receiver);
                sender
            }),
            state,
            active_library,
            identity,
            deletion: None,
            asset_storage_pending: HashSet::new(),
            storage_copy_pending: HashSet::new(),
            #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
            default_library_location: None,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            flow: AccountFlow::SignIn,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            access_open: false,
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
            sync_update_tasks: HashMap::new(),
            #[cfg(not(target_arch = "wasm32"))]
            local_usage_generation: 0,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            authentication,
            #[cfg(target_arch = "wasm32")]
            web_authentication: None,
            #[cfg(target_os = "android")]
            android_authentication: None,
            _active_library_subscription: active_library_subscription,
            _state_subscription: state_subscription,
        };
        page.observe_library_sync(&backend, cx);
        // Start snapshots from a normal window turn, never from an async
        // worker callback or a render borrow.
        cx.on_next_frame(window, |page, _, cx| page.refresh(cx));
        #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
        {
            let backend = page.services.backend.clone();
            cx.spawn(async move |page, cx| {
                let result = backend.default_library_save_location().await;
                let _ = page.update(cx, |page, cx| {
                    match result {
                        Ok(path) => page.default_library_location = path,
                        Err(error) => page.error = Some(user_facing_error(&error, "Could not load the library location. Please try again.").into()),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        page
    }

    /// Settings includes every library, even those without an open book view.
    fn observe_library_sync(&mut self, backend: &Rc<app::AppClient>, cx: &mut Context<Self>) {
        self.sync_update_tasks.retain(|id, _| self.local_usage_libraries.contains(id));
        for id in &self.local_usage_libraries {
            if self.sync_update_tasks.contains_key(id) {
                continue;
            }
            let library = backend.library(*id);
            #[cfg(target_arch = "wasm32")]
            let events = self.events.clone();
            let task = cx.spawn(async move |page, cx| {
                let (updates, _) = match library.updates().await {
                    Ok(updates) => updates,
                    Err(error) => {
                        log::warn!("Could not observe library sync status: {error}");
                        return;
                    }
                };
                while let Ok(update) = updates.recv().await {
                    if !matches!(update, LibraryUpdate::SyncStatusChanged) {
                        continue;
                    }
                    #[cfg(target_arch = "wasm32")]
                    {
                        if events.try_send(SyncEvent::LibrarySyncChanged).is_err() {
                            break;
                        }
                        let _ = (&page, &cx);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    cx.defer_update(page.clone(), |page, cx| page.schedule_refresh(cx));
                }
            });
            self.sync_update_tasks.insert(*id, task);
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl SyncPage {
    pub(crate) fn apply_event(&mut self, event: SyncEvent, cx: &mut Context<Self>) {
        match event {
            SyncEvent::LibrarySyncChanged => self.schedule_refresh(cx),
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
            SyncEvent::AssetStorageChanged { library_id, enabled, result } => {
                self.asset_storage_pending.remove(&library_id);
                match result {
                    Ok(()) => self.schedule_refresh(cx),
                    Err(error) => self.error = Some(user_facing_error(&error, "Could not change cloud storage for this library. Please try again.").into()),
                }
                let _ = enabled;
                cx.notify();
            }
            SyncEvent::WebAuthenticated => self.schedule_refresh(cx),
            SyncEvent::StorageCopyFinished { library_id, bokheim, result } => self.finish_storage_copy(library_id, bokheim, result, cx),
            SyncEvent::StorageCopyRequested { .. } => unreachable!("storage copies are handled by ApplicationRoot"),
            SyncEvent::AccountRefreshRequested
            | SyncEvent::LocalStorageUsageRequested
            | SyncEvent::LocalStorageUsage { .. }
            | SyncEvent::LogoutRequested
            | SyncEvent::DeleteAccountRequested { .. }
            | SyncEvent::AssetStorageRequested { .. }
            | SyncEvent::Authenticate { .. } => {
                unreachable!("sync commands are handled by ApplicationRoot")
            }
        }
    }
}
