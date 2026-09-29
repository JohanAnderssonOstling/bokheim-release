//! Per-window composition for the library and book reader.

#[cfg(feature = "audiobooks")]
mod audiobook_footer;
#[cfg(feature = "kobo")]
mod kobo_bar;
#[cfg(feature = "audiobooks")]
mod playback;

use std::rc::Rc;

use app::AppClient;
#[cfg(feature = "audiobooks")]
use browser_ui::BookDetailDismissed;
use browser_ui::{AppServices, BrowserRoot, DismissSettings, OpenBookRequested};
use gpui::prelude::*;
use gpui::{AnyView, Context, Entity, IntoElement, Render, SharedString, Subscription, Window};
#[cfg(all(target_os = "linux", not(feature = "mobile")))]
use gpui::{MouseMoveEvent, div, px};
use library_model::BookLocator;
use sync_common::LibraryId;

struct ReaderSurface {
    locator: BookLocator,
    view: AnyView,
}

#[cfg(all(target_os = "linux", not(feature = "mobile")))]
const NATIVE_TITLEBAR_HOVER_HEIGHT: gpui::Pixels = px(34.0);

#[cfg(feature = "audiobooks")]
struct AudiobookSurface {
    locator: BookLocator,
    session: Entity<reader_ui::PlaybackSession>,
    dock: Entity<reader_ui::AudiobookDock>,
}

fn active_reader_library_is_missing(active_library: Option<&LibraryId>, contains_library: impl FnOnce(&LibraryId) -> bool) -> bool {
    active_library.is_some_and(|library_id| !contains_library(library_id))
}

/// Owns one application window and switches between browsing and reading.
pub struct ApplicationRoot {
    #[cfg(target_arch = "wasm32")]
    sync_state: Entity<browser_ui::SyncState>,
    #[cfg(target_arch = "wasm32")]
    _sync_events: gpui::Task<()>,
    #[cfg(target_arch = "wasm32")]
    sync_sender: async_channel::Sender<browser_ui::SyncEvent>,
    #[cfg(target_arch = "wasm32")]
    account_generation: u64,
    #[cfg(target_arch = "wasm32")]
    transfer_generation: u64,
    #[cfg(feature = "kobo")]
    device_bar: Entity<kobo_bar::KoboDeviceBar>,
    browser: Entity<BrowserRoot>,
    backend: AppClient,
    reader: Option<ReaderSurface>,
    #[cfg(all(target_os = "linux", not(feature = "mobile")))]
    window_titlebar_visible: bool,
    #[cfg(all(target_os = "linux", not(feature = "mobile")))]
    native_decorations_visible: bool,
    #[cfg(all(target_os = "linux", not(feature = "mobile")))]
    titlebar_reveal_armed: bool,
    #[cfg(all(target_os = "linux", not(feature = "mobile")))]
    native_pointer_left_client: bool,
    #[cfg(feature = "audiobooks")]
    audiobook: Option<AudiobookSurface>,
    #[cfg(feature = "audiobooks")]
    audiobook_footer: Entity<audiobook_footer::AudiobookFooter>,
    #[cfg(feature = "audiobooks")]
    playback: Entity<playback::PlaybackController>,
    #[cfg(feature = "audiobooks")]
    pending_audiobook: Option<Entity<reader_ui::ReaderView>>,
    #[cfg(feature = "audiobooks")]
    open_task: Option<gpui::Task<()>>,
    /// Set the instant a book is tapped, before the async format lookup that
    /// gates opening it resolves — without this, the screen looks unchanged
    /// for however long that lookup takes.
    #[cfg(feature = "audiobooks")]
    opening: bool,
    application_title: SharedString,
    _browser_subscription: Subscription,
    notification_mode: Option<bool>,
    notification_task: Option<gpui::Task<()>>,
}

impl ApplicationRoot {
    #[cfg(target_arch = "wasm32")]
    fn apply_sync_event(&mut self, event: browser_ui::SyncEvent, cx: &mut Context<Self>) {
        use browser_ui::SyncEvent;
        match event {
            SyncEvent::AccountRefreshRequested => {
                self.account_generation = self.account_generation.wrapping_add(1);
                let generation = self.account_generation;
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let result = backend.account_snapshot().await;
                    let _ = sender.try_send(SyncEvent::AccountSnapshot { generation, result });
                })
                .detach();
            }
            SyncEvent::LocalStorageUsageRequested => {
                let generation = self.account_generation;
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let result = backend.local_storage_usage().await;
                    let _ = sender.try_send(SyncEvent::LocalStorageUsage { generation, result });
                })
                .detach();
            }
            SyncEvent::LogoutRequested => {
                self.account_generation = self.account_generation.wrapping_add(1);
                let generation = self.account_generation;
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let result = backend.logout().await;
                    let _ = sender.try_send(SyncEvent::AccountOperation { generation, result, success: "Signed out", reveal_password_reset_on_error: false });
                })
                .detach();
            }
            SyncEvent::TransferRefreshRequested => {
                self.transfer_generation = self.transfer_generation.wrapping_add(1);
                let generation = self.transfer_generation;
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let result = backend.transfers().await;
                    let _ = sender.try_send(SyncEvent::TransferSnapshot { generation, result });
                })
                .detach();
            }
            SyncEvent::AssetStorageRequested { library_id, enabled } => {
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let result = backend.set_library_asset_storage(library_id, enabled).await;
                    let _ = sender.try_send(SyncEvent::AssetStorageChanged { library_id, enabled, result });
                })
                .detach();
            }
            SyncEvent::Authenticate { action, email, secret, token, reply } => {
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let result = match action.as_str() {
                        "login" => backend.login(email, secret).await.map(|_| ()),
                        "register" => backend.request_public_registration(email, secret).await,
                        "verify" => backend.verify_email(email, secret).await.map(|_| ()),
                        "resend" => backend.resend_verification(email).await,
                        "request-reset" => backend.request_password_reset(email).await,
                        "reset" => backend.reset_password(token, secret).await,
                        _ => Err("Unknown authentication action".into()),
                    };
                    let authenticated = result.is_ok() && matches!(action.as_str(), "login" | "verify");
                    let _ = reply.try_send(result);
                    if authenticated {
                        let _ = sender.try_send(SyncEvent::WebAuthenticated);
                    }
                })
                .detach();
            }
            SyncEvent::AccountSnapshot { generation, .. } | SyncEvent::AccountOperation { generation, .. } | SyncEvent::LocalStorageUsage { generation, .. } if generation != self.account_generation => {}
            SyncEvent::TransferSnapshot { generation, .. } if generation != self.transfer_generation => {}
            SyncEvent::AccountSnapshot { generation, result } => {
                if let Ok(snapshot) = &result {
                    self.sync_state.update(cx, |state, cx| state.apply_account_snapshot(snapshot, cx));
                }
                self.browser.update(cx, |browser, cx| browser.apply_sync_event(SyncEvent::AccountSnapshot { generation, result }, cx));
            }
            SyncEvent::LocalStorageUsage { result, .. } => match result {
                Ok(usage) => self.sync_state.update(cx, |state, cx| state.apply_local_storage_usage(usage, cx)),
                Err(error) => log::warn!("could not read local storage usage: {error}"),
            },
            SyncEvent::AccountOperation { generation, result, success, reveal_password_reset_on_error } => {
                if let Ok(snapshot) = &result {
                    self.sync_state.update(cx, |state, cx| state.apply_account_snapshot(snapshot, cx));
                }
                self.browser.update(cx, |browser, cx| browser.apply_sync_event(SyncEvent::AccountOperation { generation, result, success, reveal_password_reset_on_error }, cx));
            }
            SyncEvent::TransferSnapshot { result, .. } => {
                let result = match result {
                    Ok(groups) => {
                        self.sync_state.update(cx, |state, cx| state.apply_transfer_snapshot(groups, cx));
                        Ok(())
                    }
                    Err(error) => {
                        self.sync_state.update(cx, |state, cx| state.apply_transfer_snapshot(Vec::new(), cx));
                        Err(error)
                    }
                };
                self.browser.update(cx, |browser, cx| browser.apply_sync_event(SyncEvent::TransferApplied { result }, cx));
            }
            SyncEvent::AssetStorageChanged { library_id, enabled, result } => {
                if result.is_ok() {
                    self.sync_state.update(cx, |state, cx| state.apply_asset_storage(library_id, enabled, cx));
                }
                self.browser.update(cx, |browser, cx| browser.apply_sync_event(SyncEvent::AssetStorageChanged { library_id, enabled, result }, cx));
            }
            event => self.browser.update(cx, |browser, cx| browser.apply_sync_event(event, cx)),
        }
    }

    pub fn open_window_target(&mut self, target: browser_ui::NewWindowTarget, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            browser_ui::NewWindowTarget::Book { locator, title } => self.open_book(locator, title, None, window, cx),
            browser_ui::NewWindowTarget::Folder { library_id, directory_id, title } => {
                self.browser.update(cx, |browser, cx| browser.open_folder(library_id, directory_id, title, window, cx));
            }
            browser_ui::NewWindowTarget::Subject { library_id, location, label } => {
                self.browser.update(cx, |browser, cx| browser.open_subject(library_id, location, label, window, cx));
            }
        }
    }

    pub fn new(
        services: AppServices, backend: AppClient, application_title: impl Into<SharedString>, playback_restore_path: Option<std::path::PathBuf>, #[cfg(feature = "kobo")] kobo_auto_rotate: bool, window: &mut Window, cx: &mut Context<Self>,
    ) -> Self {
        cx.set_volume_button_capture(true);
        let sync_state = cx.new(|_| browser_ui::SyncState::new(&services));
        #[cfg(target_arch = "wasm32")]
        let (sync_sender, sync_receiver) = async_channel::unbounded();
        #[cfg(target_arch = "wasm32")]
        let services = services.with_sync_events(sync_sender.clone());
        #[cfg(target_arch = "wasm32")]
        let retry_executor = cx.background_executor().clone();
        #[cfg(target_arch = "wasm32")]
        let sync_events = cx.spawn_in(window, async move |root, cx| {
            while let Ok(event) = sync_receiver.recv().await {
                let mut pending = Some(event);
                loop {
                    // WeakEntity::update_in uses AsyncApp::with_window, which
                    // tries the AppCell borrow and leaves the closure untouched
                    // if a browser callback is still updating the application.
                    let result = root.update_in(cx, |root, _, cx| {
                        root.apply_sync_event(pending.take().expect("pending sync event"), cx);
                    });
                    if result.is_ok() {
                        break;
                    }
                    if root.upgrade().is_none() {
                        return;
                    }
                    retry_executor.timer(std::time::Duration::from_millis(1)).await;
                }
            }
        });
        #[cfg(feature = "kobo")]
        let kobo_bar_services = services.clone();
        #[cfg(feature = "kobo")]
        let browser = cx.new(|cx| BrowserRoot::new(services, sync_state.clone(), window, cx));
        #[cfg(not(feature = "kobo"))]
        let browser = cx.new(|cx| BrowserRoot::new(services, sync_state.clone(), window, cx));
        let browser_subscription = cx.observe_in(&browser, window, |root, browser, window, cx| {
            let active_library = root.reader.as_ref().map(|reader| reader.locator.library_id());
            let active_removed = active_reader_library_is_missing(active_library, |library_id| browser.read(cx).contains_library(library_id, cx));
            if active_removed {
                log::warn!("an open book's library was deleted; returning to the library browser");
                root.return_to_library(window, cx);
            }
        });
        cx.subscribe_in(&browser, window, |root, _, request: &OpenBookRequested, window, cx| {
            root.open_book(request.locator.clone(), request.title.clone(), request.initial_target.clone(), window, cx);
        })
        .detach();
        #[cfg(feature = "audiobooks")]
        cx.subscribe(&browser, |root, _, _: &BookDetailDismissed, cx| {
            if let Some(audio) = &root.audiobook {
                audio.dock.update(cx, |dock, cx| dock.collapse_mobile(cx));
            }
        })
        .detach();
        cx.observe_window_activation(window, |root, window, cx| root.update_notification_interest(window, cx)).detach();
        #[cfg(feature = "audiobooks")]
        let playback = playback::PlaybackController::shared(backend.clone(), playback_restore_path, cx);
        #[cfg(feature = "audiobooks")]
        cx.observe_in(&playback, window, |root, _, window, cx| root.sync_playback(window, cx)).detach();
        #[cfg(feature = "audiobooks")]
        let audiobook_footer = cx.new(|_| audiobook_footer::AudiobookFooter::default());
        #[cfg(feature = "audiobooks")]
        cx.subscribe(&audiobook_footer, |root, _, _: &audiobook_footer::CancelPending, cx| {
            root.pending_audiobook = None;
            root.open_task = None;
            root.opening = false;
            cx.notify();
        })
        .detach();
        let mut root = Self {
            #[cfg(target_arch = "wasm32")]
            sync_state,
            #[cfg(target_arch = "wasm32")]
            _sync_events: sync_events,
            #[cfg(target_arch = "wasm32")]
            sync_sender,
            #[cfg(target_arch = "wasm32")]
            account_generation: 0,
            #[cfg(target_arch = "wasm32")]
            transfer_generation: 0,
            #[cfg(feature = "kobo")]
            device_bar: cx.new(|cx| kobo_bar::KoboDeviceBar::new(backend.clone(), kobo_bar_services, kobo_auto_rotate, window, cx)),
            browser,
            backend,
            reader: None,
            #[cfg(all(target_os = "linux", not(feature = "mobile")))]
            window_titlebar_visible: false,
            #[cfg(all(target_os = "linux", not(feature = "mobile")))]
            native_decorations_visible: false,
            #[cfg(all(target_os = "linux", not(feature = "mobile")))]
            titlebar_reveal_armed: true,
            #[cfg(all(target_os = "linux", not(feature = "mobile")))]
            native_pointer_left_client: false,
            application_title: application_title.into(),
            _browser_subscription: browser_subscription,
            notification_mode: None,
            notification_task: None,
            #[cfg(feature = "audiobooks")]
            audiobook: None,
            #[cfg(feature = "audiobooks")]
            audiobook_footer,
            #[cfg(feature = "audiobooks")]
            playback,
            #[cfg(feature = "audiobooks")]
            pending_audiobook: None,
            #[cfg(feature = "audiobooks")]
            open_task: None,
            #[cfg(feature = "audiobooks")]
            opening: false,
        };
        root.update_notification_interest(window, cx);
        #[cfg(feature = "audiobooks")]
        root.sync_playback(window, cx);
        root
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn import_and_open_path(&mut self, name: String, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| browser.import_and_open_path(name, path, window, cx));
    }

    pub fn incoming_import_failed(&mut self, error: String, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| browser.incoming_import_failed(error, window, cx));
    }

    pub fn open_book(&mut self, locator: BookLocator, title: String, initial_target: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if browser_ui::library_is_being_removed(*locator.library_id(), cx) {
            return;
        }
        #[cfg(feature = "audiobooks")]
        let audio_request = self.playback.update(cx, |controller, _| controller.reserve_open());
        #[cfg(feature = "audiobooks")]
        if self.playback.update(cx, |controller, cx| controller.activate_current(audio_request, &locator, initial_target.as_deref(), cx)) {
            self.open_task.take();
            self.pending_audiobook = None;
            self.opening = false;
            self.sync_playback(window, cx);
            self.return_to_library(window, cx);
            return;
        }
        #[cfg(feature = "audiobooks")]
        {
            let library = self.backend.library(*locator.library_id());
            self.pending_audiobook = None;
            self.opening = true;
            cx.notify();
            self.open_task = Some(cx.spawn(async move |root, cx| {
                let audio = matches!(library.book_format(locator.content_hash()).await, Ok(Some(book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder)));
                let _ = root.update_in(cx, |root, window, cx| {
                    if audio {
                        if !root.playback.update(cx, |controller, cx| controller.prepare_open(audio_request, &locator, cx)) {
                            root.opening = false;
                            cx.notify();
                            return;
                        }
                        root.sync_playback(window, cx);
                    }
                    root.open_reader_surface(locator, title, initial_target, audio, audio_request, window, cx);
                });
            }));
        }
        #[cfg(not(feature = "audiobooks"))]
        self.open_reader_surface(locator, title, initial_target, false, 0, window, cx);
    }

    fn open_reader_surface(&mut self, locator: BookLocator, title: String, initial_target: Option<String>, audio: bool, audio_request: u64, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "audiobooks")]
        {
            self.opening = false;
        }
        #[cfg(feature = "audiobooks")]
        if !audio {
            self.playback.update(cx, |controller, cx| controller.close(cx));
            self.sync_playback(window, cx);
            self.browser.update(cx, |browser, cx| browser.clear_cached_book_details(cx));
        }
        let reader_locator = locator.clone();
        let library = Rc::new(self.backend.library(*locator.library_id()));
        #[cfg(feature = "audiobooks")]
        let audio_title = title.clone();
        let reader = cx.new(|cx| reader_ui::ReaderView::new(reader_locator, title, initial_target, self.backend.clone(), library, window, cx));
        cx.subscribe_in(&reader, window, |root, owner, _: &reader_ui::CloseRequested, window, cx| {
            #[cfg(feature = "audiobooks")]
            if root.pending_audiobook.as_ref().is_some_and(|pending| pending == owner) {
                root.pending_audiobook = None;
            }
            root.return_to_library(window, cx);
        })
        .detach();
        cx.observe(&reader, |_, _, cx| cx.notify()).detach();
        #[cfg(feature = "audiobooks")]
        {
            let audio_locator = locator.clone();
            let generation = self.playback.read(cx).generation;
            cx.subscribe_in(&reader, window, move |root, owner, event: &reader_ui::AudiobookOpened, window, cx| {
                let session = event.0.clone();
                let accepted = if audio { root.playback.read(cx).generation == generation } else { root.playback.update(cx, |controller, cx| controller.prepare_open(audio_request, &audio_locator, cx)) };
                if !accepted {
                    session.update(cx, |session, _| session.stop_playback());
                    if root.reader.as_ref().is_some_and(|reader| reader.locator == audio_locator) {
                        root.return_to_library(window, cx);
                    }
                    if root.pending_audiobook.as_ref().is_some_and(|pending| pending == owner) {
                        root.pending_audiobook = None;
                        cx.notify();
                    }
                    return;
                }
                root.playback.update(cx, |controller, cx| controller.replace(audio_locator.clone(), audio_title.clone(), session, cx));
                root.sync_playback(window, cx);
                root.pending_audiobook = None;
                root.return_to_library(window, cx);
            })
            .detach();
        }
        #[cfg(feature = "audiobooks")]
        if audio {
            self.pending_audiobook = Some(reader);
            window.set_window_title(&self.application_title);
            cx.notify();
            return;
        }
        let view = reader.into();
        self.reader = Some(ReaderSurface { locator, view });
        self.update_notification_interest(window, cx);
        cx.set_volume_button_capture(true);
        cx.set_keep_screen_awake(true);
        cx.notify();
    }

    pub fn return_to_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reader = None;
        #[cfg(all(target_os = "linux", not(feature = "mobile")))]
        {
            if self.native_decorations_visible {
                window.request_decorations(gpui::WindowDecorations::Client);
            }
            self.window_titlebar_visible = false;
            self.native_decorations_visible = false;
            self.titlebar_reveal_armed = true;
            self.native_pointer_left_client = false;
        }
        self.update_notification_interest(window, cx);
        cx.set_volume_button_capture(true);
        cx.set_keep_screen_awake(false);
        cx.set_system_bars_visible(true);
        #[cfg(feature = "kobo")]
        if let Err(error) = gpui_kobo::request_experience_mode(gpui_kobo::KoboExperienceMode::LibraryFast) {
            log::error!("failed to return to Kobo library fast mode: {error}");
        }
        window.set_window_title(&self.application_title);
        self.browser.update(cx, |browser, cx| browser.focus_active_page(window, cx));
        cx.notify();
    }

    #[cfg(feature = "audiobooks")]
    pub fn audiobook_session(&self) -> Option<Entity<reader_ui::PlaybackSession>> {
        self.audiobook.as_ref().map(|audio| audio.session.clone())
    }

    #[cfg(feature = "audiobooks")]
    pub fn audiobook_is_expanded(&self) -> bool {
        self.audiobook.as_ref().is_some_and(|audio| self.reader.as_ref().is_some_and(|reader| reader.locator == audio.locator))
    }

    #[cfg(feature = "audiobooks")]
    pub fn expand_audiobook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.audiobook_is_expanded() {
            return;
        }
        if let Some(audio) = &self.audiobook {
            let locator = audio.locator.clone();
            self.browser.update(cx, |browser, cx| browser.open_book_detail(*locator.library_id(), locator.content_hash(), window, cx));
            cx.notify();
        }
    }

    #[cfg(feature = "audiobooks")]
    pub fn close_audiobook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.playback.update(cx, |controller, cx| controller.close(cx));
        self.sync_playback(window, cx);
    }

    #[cfg(feature = "audiobooks")]
    fn sync_playback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.playback.read(cx).current.as_ref().map(|active| (active.record.locator.clone(), active.session.clone()));
        if active.as_ref().is_some_and(|(_, session)| self.audiobook.as_ref().is_some_and(|audio| &audio.session == session)) {
            return;
        }
        if let Some(old) = self.audiobook.take() {
            if self.reader.as_ref().is_some_and(|reader| reader.locator == old.locator) {
                self.return_to_library(window, cx);
            }
        }
        let Some((locator, session)) = active else {
            cx.notify();
            return;
        };
        let dock = cx.new(|cx| reader_ui::AudiobookDock::new(session.clone(), cx));
        cx.subscribe_in(&dock, window, |root, _, action: &reader_ui::DockAction, window, cx| match action {
            reader_ui::DockAction::Expand => root.expand_audiobook(window, cx),
            reader_ui::DockAction::Close => root.close_audiobook(window, cx),
        })
        .detach();
        self.audiobook = Some(AudiobookSurface { locator, session, dock });
        cx.notify();
    }

    fn update_notification_interest(&mut self, window: &Window, cx: &mut Context<Self>) {
        let mode = window.is_window_active().then_some(self.reader.is_none());
        if self.notification_mode == mode {
            return;
        }
        self.notification_mode = mode;
        self.notification_task.take();
        if let Some(browsing) = mode {
            let backend = self.backend.clone();
            self.notification_task = Some(cx.spawn(async move |_, _| match backend.notification_interest(browsing).await {
                Ok(interest) => {
                    let _interest = interest;
                    std::future::pending::<()>().await;
                }
                Err(error) => log::warn!("could not register notification interest: {error}"),
            }));
        }
    }

    fn on_back(&mut self, _: &DismissSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader.is_some() {
            self.return_to_library(window, cx);
        } else {
            cx.propagate();
        }
    }
}

impl Render for ApplicationRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "audiobooks")]
        let footer = {
            let dock = self.audiobook.as_ref().filter(|audio| self.reader.as_ref().is_none_or(|reader| reader.locator != audio.locator)).map(|audio| audio.dock.clone());
            let pending = self.pending_audiobook.as_ref().map(|pending| pending.read(cx).loading_error().unwrap_or_else(|| "Opening audiobook…".into()));
            let visible = dock.is_some() || pending.is_some();
            self.audiobook_footer.update(cx, |footer, cx| footer.set(dock, pending, cx));
            visible.then(|| AnyView::from(self.audiobook_footer.clone()))
        };
        #[cfg(feature = "audiobooks")]
        self.browser.update(cx, |browser, cx| browser.set_content_footer(footer.clone().filter(|_| self.reader.is_none()), cx));
        let content = self.reader.as_ref().map(|reader| reader.view.clone()).unwrap_or_else(|| AnyView::from(self.browser.clone()));
        // Covers the gap between tapping a book and the async format lookup
        // in `open_book` resolving, so the tap isn't indistinguishable from
        // one that did nothing.
        #[cfg(feature = "audiobooks")]
        let opening_overlay = self.opening.then(|| gpui::div().absolute().inset_0().flex().items_center().justify_center().bg(gpui::rgba(0x000000a0)).text_color(gpui::rgb(0xffffff)).child("Opening…"));
        let root = gpui::div().size_full().relative().flex().flex_col().key_context("Application").on_action(cx.listener(Self::on_back));
        #[cfg(all(target_os = "linux", not(feature = "mobile")))]
        let root = root
            .on_mouse_exit(cx.listener(|root, _: &gpui::MouseExitEvent, _, _| {
                if root.native_decorations_visible {
                    root.native_pointer_left_client = true;
                }
            }))
            .on_mouse_move(cx.listener(|root, event: &MouseMoveEvent, window, cx| {
                if root.native_decorations_visible && root.native_pointer_left_client {
                    // Wayland sends MouseExit when the pointer leaves our
                    // surface for the compositor's title bar, then a move
                    // when it enters the client area again.
                    window.request_decorations(gpui::WindowDecorations::Client);
                    root.native_decorations_visible = false;
                    root.native_pointer_left_client = false;
                    root.titlebar_reveal_armed = false;
                    cx.notify();
                } else if !root.native_decorations_visible && !root.titlebar_reveal_armed && !root.window_titlebar_visible {
                    let trigger_bottom = gpui_component::window_paddings(window).top + NATIVE_TITLEBAR_HOVER_HEIGHT;
                    if event.position.y >= trigger_bottom {
                        root.titlebar_reveal_armed = true;
                    }
                }
            }));
        #[cfg(feature = "kobo")]
        let root = root.when(self.reader.is_none(), |root| root.child(self.device_bar.clone()));
        let root = root.child(gpui::div().flex_1().min_h_0().w_full().child(content));
        #[cfg(feature = "audiobooks")]
        // No bottom nav bar below the dock on this path, so it needs its own
        // padding to clear the OS gesture/button bar.
        let root = root.children(footer.filter(|_| self.reader.is_some()).map(|footer| gpui::div().w_full().flex_none().pb(window.insets().safe_area.bottom).child(footer)));
        #[cfg(feature = "audiobooks")]
        let root = root.children(opening_overlay);
        #[cfg(all(target_os = "linux", not(feature = "mobile")))]
        let root = root.when(matches!(window.window_decorations(), gpui::Decorations::Client { .. }), |root| {
            if self.window_titlebar_visible {
                root.child(
                    div()
                        .id("window-titlebar-hover")
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .on_hover(cx.listener(|root, hovered: &bool, _, cx| {
                            if !*hovered {
                                root.window_titlebar_visible = false;
                                root.titlebar_reveal_armed = true;
                                cx.notify();
                            }
                        }))
                        .child(gpui_component::TitleBar::new().child(self.application_title.clone())),
                )
            } else {
                root.child(div().id("window-titlebar-reveal").absolute().top_0().left_0().right_0().h(NATIVE_TITLEBAR_HOVER_HEIGHT).on_hover(cx.listener(|root, hovered: &bool, window, cx| {
                    if *hovered && root.titlebar_reveal_armed {
                        root.titlebar_reveal_armed = false;
                        root.native_pointer_left_client = false;
                        window.request_decorations(gpui::WindowDecorations::Server);
                        root.native_decorations_visible = matches!(window.window_decorations(), gpui::Decorations::Server);
                        root.window_titlebar_visible = !root.native_decorations_visible;
                        cx.notify();
                    }
                })))
            }
        });
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_reader_library_requires_returning_to_the_browser() {
        let library_id = LibraryId::from_u128(1);
        assert!(active_reader_library_is_missing(Some(&library_id), |_| false));
        assert!(!active_reader_library_is_missing(Some(&library_id), |_| true));
        assert!(!active_reader_library_is_missing(None, |_| false));
    }
}
