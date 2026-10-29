//! Per-window composition for the library and book reader.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;
mod audiobook_footer;
mod keyboard;
#[cfg(feature = "kobo")]
mod kobo_bar;
mod panes;
mod playback;
pub use panes::ApplicationRoot;

use std::rc::Rc;

use app::AppClient;
use browser_ui::{AppServices, BrowserRoot, DismissSettings, NavigationSession, NowPlayingBook, OpenBookRequested, ReaderSession, SaveNavigationSession, WindowSession};
use gpui::prelude::*;
use gpui::{AnyView, App, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, Modifiers, Render, SharedString, Subscription, Window};
#[cfg(all(target_os = "linux", not(feature = "mobile")))]
use gpui::{MouseMoveEvent, div, px};
use gpui_component::{Root, WindowExt, notification::Notification};
use gpui_image_eviction::trim_unused_images_after_transition;
use library_model::BookLocator;
use sync_common::LibraryId;

struct ReaderSurface {
    locator: BookLocator,
    title: String,
    view: Entity<reader_ui::ReaderView>,
}

#[cfg(all(target_os = "linux", not(feature = "mobile")))]
const NATIVE_TITLEBAR_HOVER_HEIGHT: gpui::Pixels = px(34.0);

struct AudiobookSurface {
    locator: BookLocator,
    session: Entity<reader_ui::PlaybackSession>,
    dock: Entity<reader_ui::AudiobookDock>,
}

fn active_reader_library_is_missing(active_library: Option<&LibraryId>, contains_library: impl FnOnce(&LibraryId) -> bool) -> bool {
    active_library.is_some_and(|library_id| !contains_library(library_id))
}

type NativeBackOverlay = Rc<dyn Fn(&mut Window, &mut App) -> bool>;

/// Owns one pane and switches between its browser and reader.
pub(crate) struct ApplicationPane {
    #[cfg(target_arch = "wasm32")]
    sync_state: Entity<browser_ui::SyncState>,
    #[cfg(target_arch = "wasm32")]
    _sync_events: gpui::Task<()>,
    #[cfg(target_arch = "wasm32")]
    sync_sender: async_channel::Sender<browser_ui::SyncEvent>,
    #[cfg(target_arch = "wasm32")]
    account_generation: u64,
    #[cfg(feature = "kobo")]
    device_bar: Entity<kobo_bar::KoboDeviceBar>,
    save_navigation_session: SaveNavigationSession,
    last_saved_navigation: Option<NavigationSession>,
    pending_reader: Option<ReaderSession>,
    native_back_overlay: Option<NativeBackOverlay>,
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
    audiobook: Option<AudiobookSurface>,
    audiobook_footer: Entity<audiobook_footer::AudiobookFooter>,
    playback: Entity<playback::PlaybackController>,
    pending_audiobook: Option<SharedString>,
    open_task: Option<gpui::Task<()>>,
    /// Set the instant a book is tapped, before the async format lookup that
    /// gates opening it resolves — without this, the screen looks unchanged
    /// for however long that lookup takes.
    opening: bool,
    opening_focus: FocusHandle,
    application_title: SharedString,
    _browser_subscription: Subscription,
    split_enabled: bool,
    notification_mode: Option<bool>,
    notification_task: Option<gpui::Task<()>>,
}

impl ApplicationPane {
    #[cfg(test)]
    fn new(
        services: AppServices, session: WindowSession, backend: AppClient, application_title: impl Into<SharedString>, playback_restore_path: Option<PathBuf>, #[cfg(feature = "kobo")] kobo_auto_rotate: bool, window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_playback(
            services,
            session,
            backend,
            application_title,
            playback_restore_path,
            None,
            #[cfg(feature = "kobo")]
            kobo_auto_rotate,
            window,
            cx,
        )
    }

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
            SyncEvent::DeleteAccountRequested { email, password } => {
                self.account_generation = self.account_generation.wrapping_add(1);
                let generation = self.account_generation;
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let result = backend.delete_account(email, password).await;
                    let _ = sender.try_send(SyncEvent::AccountOperation { generation, result, success: "Your account has been deleted. Books saved on this device have been kept.", reveal_password_reset_on_error: false });
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
            SyncEvent::StorageCopyRequested { library_id, bokheim } => {
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let result = backend.copy_library_storage(library_id, bokheim).await;
                    let _ = sender.try_send(SyncEvent::StorageCopyFinished { library_id, bokheim, result });
                })
                .detach();
            }
            SyncEvent::Authenticate { request, reply } => {
                let backend = self.backend.clone();
                let sender = self.sync_sender.clone();
                cx.spawn(async move |_, _| {
                    let signs_in = request.signs_in();
                    let result = request.submit(&backend).await;
                    let authenticated = result.is_ok() && signs_in;
                    let _ = reply.try_send(result);
                    if authenticated {
                        let _ = sender.try_send(SyncEvent::WebAuthenticated);
                    }
                })
                .detach();
            }
            SyncEvent::AccountSnapshot { generation, .. } | SyncEvent::AccountOperation { generation, .. } | SyncEvent::LocalStorageUsage { generation, .. } if generation != self.account_generation => {}
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
            browser_ui::NewWindowTarget::Book { locator, .. } => {
                self.browser.update(cx, |browser, cx| browser.open_book_detail(*locator.library_id(), locator.content_hash(), window, cx));
            }
            browser_ui::NewWindowTarget::Folder { library_id, directory_id, title } => {
                self.browser.update(cx, |browser, cx| browser.open_folder(library_id, directory_id, title, window, cx));
            }
            browser_ui::NewWindowTarget::Subject { library_id, location, label } => {
                self.browser.update(cx, |browser, cx| browser.open_subject(library_id, location, label, window, cx));
            }
        }
    }

    pub(crate) fn new_with_playback(
        services: AppServices, session: WindowSession, backend: AppClient, application_title: impl Into<SharedString>, playback_restore_path: Option<PathBuf>, playback_controller: Option<Entity<playback::PlaybackController>>,
        #[cfg(feature = "kobo")] kobo_auto_rotate: bool, window: &mut Window, cx: &mut Context<Self>,
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
                        if let Some(event) = pending.take() {
                            root.apply_sync_event(event, cx);
                        }
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
        let browser = cx.new(|cx| BrowserRoot::new(services.clone(), session.initial.clone(), sync_state.clone(), window, cx));
        let browser_subscription = cx.observe_in(&browser, window, |root, browser, window, cx| {
            let active_library = root.reader.as_ref().map(|reader| reader.locator.library_id()).or_else(|| root.pending_reader.as_ref().map(|reader| reader.locator.library_id()));
            let active_removed = active_reader_library_is_missing(active_library, |library_id| browser.read(cx).contains_library(library_id, cx));
            if active_removed {
                log::warn!("an open book's library was deleted; returning to the library browser");
                root.return_to_library(window, cx);
            }
            root.save_navigation(cx);
        });
        cx.subscribe_in(&browser, window, |root, _, request: &browser_ui::PaneRequest, window, cx| match request {
            browser_ui::PaneRequest::Browse => root.return_to_library(window, cx),
            request => cx.emit(request.clone()),
        })
        .detach();
        cx.subscribe_in(&browser, window, |root, _, request: &OpenBookRequested, window, cx| {
            root.open_book(request.locator.clone(), request.title.clone(), request.initial_target.clone(), window, cx);
        })
        .detach();
        cx.observe_window_activation(window, |root, window, cx| root.update_notification_interest(window, cx)).detach();
        let playback = playback_controller.unwrap_or_else(|| playback::PlaybackController::shared(backend.clone(), playback_restore_path, cx));
        cx.observe_in(&playback, window, |root, _, window, cx| root.sync_playback(window, cx)).detach();
        let audiobook_footer = cx.new(|_| audiobook_footer::AudiobookFooter::default());
        cx.subscribe(&audiobook_footer, |root, _, _: &audiobook_footer::CancelPending, cx| {
            root.pending_audiobook = None;
            root.open_task = None;
            root.opening = false;
            root.playback.update(cx, |controller, cx| controller.close(cx));
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
            #[cfg(feature = "kobo")]
            device_bar: cx.new(|cx| kobo_bar::KoboDeviceBar::new(backend.clone(), kobo_bar_services, kobo_auto_rotate, window, cx)),
            pending_reader: session.initial.reader.filter(|reader| browser.read(cx).contains_library(reader.locator.library_id(), cx)),
            save_navigation_session: session.save,
            last_saved_navigation: None,
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
            split_enabled: false,
            notification_mode: None,
            notification_task: None,
            native_back_overlay: None,
            audiobook: None,
            audiobook_footer,
            playback,
            pending_audiobook: None,
            open_task: None,
            opening: false,
            opening_focus: cx.focus_handle(),
        };
        root.update_notification_interest(window, cx);
        root.sync_playback(window, cx);
        root.save_navigation(cx);
        if let Some(reader) = root.pending_reader.clone() {
            cx.defer_in(window, move |root, window, cx| root.restore_reader(reader, window, cx));
        }
        root
    }

    /// Persistence is derived from the browser and reader owners. The saved
    /// value is only retained to suppress redundant writes.
    fn save_navigation(&mut self, cx: &mut App) {
        let mut snapshot = self.browser.read(cx).navigation_session(cx);
        snapshot.reader = self.reader.as_ref().map(|reader| ReaderSession { locator: reader.locator.clone(), title: reader.title.clone() }).or_else(|| self.pending_reader.clone());
        if self.last_saved_navigation.as_ref() != Some(&snapshot) {
            self.last_saved_navigation = Some(snapshot.clone());
            (self.save_navigation_session)(snapshot, cx);
        }
    }

    fn restore_reader(&mut self, reader: ReaderSession, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader.is_some() || self.opening || self.pending_reader.as_ref() != Some(&reader) {
            return;
        }
        let library = self.backend.library(*reader.locator.library_id());
        self.begin_opening(window, cx);
        self.open_task = Some(cx.spawn(async move |root, cx| {
            let format = library.book_format(reader.locator.content_hash()).await;
            let _ = root.update_in(cx, |root, window, cx| match format {
                Ok(Some(format)) if !matches!(format, book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder) => {
                    // No explicit target: the reader loads the latest durable EPUB/PDF position.
                    root.open_reader_surface(reader.locator, reader.title, None, window, cx);
                }
                Ok(_) => {
                    root.opening = false;
                    root.pending_reader = None;
                    root.save_navigation(cx);
                    log::warn!("the saved reader book is no longer available");
                    cx.notify();
                }
                Err(error) => {
                    root.book_open_failed(&reader.title, error, window, cx);
                }
            });
        }));
        cx.notify();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn import_and_open_path(&mut self, name: String, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| browser.import_and_open_path(name, path, window, cx));
    }

    pub fn incoming_import_failed(&mut self, error: app::BackendError, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| browser.incoming_import_failed(error, window, cx));
    }

    fn begin_opening(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.opening = true;
        self.opening_focus.focus(window, cx);
        cx.notify();
    }

    pub fn open_book(&mut self, locator: BookLocator, title: String, initial_target: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if browser_ui::library_is_being_removed(*locator.library_id(), cx) {
            return;
        }
        self.pending_reader = None;
        let audio_request = self.playback.update(cx, |controller, _| controller.reserve_open());
        if self.playback.update(cx, |controller, cx| controller.activate_current(audio_request, &locator, initial_target.as_deref(), cx)) {
            self.open_task.take();
            self.pending_audiobook = None;
            self.opening = false;
            self.sync_playback(window, cx);
            self.return_to_library(window, cx);
            return;
        }
        let library = self.backend.library(*locator.library_id());
        self.pending_audiobook = None;
        self.begin_opening(window, cx);
        self.open_task = Some(cx.spawn(async move |root, cx| {
            let audio = match library.book_format(locator.content_hash()).await {
                Ok(format) => matches!(format, Some(book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder)),
                Err(error) => {
                    let _ = root.update_in(cx, |root, window, cx| root.book_open_failed(&title, error, window, cx));
                    return;
                }
            };
            if !audio {
                let _ = root.update_in(cx, |root, window, cx| root.open_reader_surface(locator, title, initial_target, window, cx));
                return;
            }
            let Ok(Some(generation)) = root.update_in(cx, |root, window, cx| {
                if !root.playback.update(cx, |controller, cx| controller.prepare_open(audio_request, &locator, cx)) {
                    root.opening = false;
                    cx.notify();
                    return None;
                }
                root.opening = false;
                root.pending_audiobook = Some("Opening audiobook…".into());
                root.sync_playback(window, cx);
                cx.notify();
                Some(root.playback.read(cx).generation)
            }) else {
                return;
            };
            let result = library.resolve_book(locator.content_hash()).await;
            let _ = root.update_in(cx, |root, window, cx| {
                if root.playback.read(cx).generation != generation {
                    return;
                }
                match result {
                    Ok(resolved) => {
                        if !matches!(resolved.format(), book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder) {
                            root.pending_audiobook = None;
                            root.open_reader_surface(locator, title, initial_target, window, cx);
                            return;
                        }
                        let session = reader_ui::open_audiobook(locator.clone(), library, resolved, initial_target, cx);
                        root.playback.update(cx, |controller, cx| controller.replace(locator, title, session, cx));
                        root.pending_audiobook = None;
                        root.sync_playback(window, cx);
                        root.return_to_library(window, cx);
                    }
                    Err(error) => {
                        log::error!("cannot open audiobook: {error}");
                        root.pending_audiobook = None;
                        root.playback.update(cx, |controller, cx| controller.close(cx));
                        root.book_open_failed(&title, error, window, cx);
                    }
                }
            });
        }));
    }

    fn open_reader_surface(&mut self, locator: BookLocator, title: String, initial_target: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.opening = false;
        let audio_request = self.playback.update(cx, |controller, cx| {
            controller.close(cx);
            controller.reserve_open()
        });
        self.sync_playback(window, cx);
        let library = Rc::new(self.backend.library(*locator.library_id()));
        let audio_locator = locator.clone();
        let audio_title = title.clone();
        let saved_title = title.clone();
        let reader = cx.new(|cx| {
            let reader = reader_ui::ReaderView::new(locator.clone(), title, initial_target, self.backend.clone(), library, window, cx);
            #[cfg(feature = "kobo")]
            let reader = reader.with_device_controls(self.device_bar.clone());
            reader
        });
        cx.subscribe_in(&reader, window, |root, reader, _: &reader_ui::CloseRequested, window, cx| {
            if root.reader.as_ref().is_some_and(|surface| surface.view.entity_id() == reader.entity_id()) {
                root.return_to_library(window, cx);
            }
        })
        .detach();
        cx.subscribe_in(&reader, window, |root, reader, error: &reader_ui::BookOpenFailed, window, cx| {
            // A completion from a replaced or cancelled reader must not close
            // the book the user subsequently opened.
            if let Some(surface) = root.reader.as_ref().filter(|surface| surface.view.entity_id() == reader.entity_id()) {
                let title = surface.title.clone();
                root.book_open_failed(&title, app::BackendError::Message(error.0.clone()), window, cx);
            }
        })
        .detach();
        cx.observe(&reader, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&reader, window, move |root, _, event: &reader_ui::AudiobookOpened, window, cx| {
            let session = event.0.clone();
            if !root.playback.update(cx, |controller, cx| controller.prepare_open(audio_request, &audio_locator, cx)) {
                session.update(cx, |session, _| session.stop_playback());
                if root.reader.as_ref().is_some_and(|reader| reader.locator == audio_locator) {
                    root.return_to_library(window, cx);
                }
                return;
            }
            root.playback.update(cx, |controller, cx| controller.replace(audio_locator.clone(), audio_title.clone(), session, cx));
            root.sync_playback(window, cx);
            root.return_to_library(window, cx);
        })
        .detach();
        self.reader = Some(ReaderSurface { locator, title: saved_title, view: reader });
        self.pending_reader = None;
        browser_ui::evict_covers(window);
        let backend = self.backend.clone();
        cx.spawn(async move |_, _| {
            if let Err(error) = backend.evict_cover_caches().await {
                log::warn!("could not evict retained cover bytes: {error}");
            }
        })
        .detach();
        self.save_navigation(cx);
        self.update_notification_interest(window, cx);
        cx.set_volume_button_capture(true);
        cx.set_keep_screen_awake(true);
        cx.notify();
    }

    pub fn return_to_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_task = None;
        self.opening = false;
        self.reader = None;
        self.pending_reader = None;
        trim_unused_images_after_transition(window);
        self.save_navigation(cx);
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

    fn book_open_failed(&mut self, title: &str, error: app::BackendError, window: &mut Window, cx: &mut Context<Self>) {
        log::error!("could not open {title:?}: {error}");
        self.return_to_library(window, cx);
        let message = ui_components::user_facing_error(&error, "Try opening the book again. If it still won't open, download it again or try another edition.");
        window.push_notification(Notification::error(format!("Could not open “{title}”. {message}")).id1::<Self>("book-open-error").autohide(false).close_button_always_visible(true), cx);
    }

    pub fn audiobook_session(&self) -> Option<Entity<reader_ui::PlaybackSession>> {
        self.audiobook.as_ref().map(|audio| audio.session.clone())
    }

    pub fn audiobook_is_expanded(&self) -> bool {
        self.audiobook.as_ref().is_some_and(|audio| self.reader.as_ref().is_some_and(|reader| reader.locator == audio.locator))
    }

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

    pub fn close_audiobook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.playback.update(cx, |controller, cx| controller.close(cx));
        self.sync_playback(window, cx);
    }

    fn sync_playback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.playback.read(cx).current.as_ref().map(|active| (active.record.locator.clone(), active.session.clone()));
        if active.as_ref().is_some_and(|(_, session)| self.audiobook.as_ref().is_some_and(|audio| &audio.session == session)) {
            return;
        }
        if let Some(old) = self.audiobook.take()
            && self.reader.as_ref().is_some_and(|reader| reader.locator == old.locator)
        {
            self.return_to_library(window, cx);
        }
        let Some((locator, session)) = active else {
            self.browser.update(cx, |browser, cx| browser.set_now_playing(None, cx));
            cx.notify();
            return;
        };
        // The playing book's detail page holds the controls itself.
        let controls = cx.new(|cx| reader_ui::AudiobookDock::inline(session.clone(), cx));
        let position_session = session.clone();
        let position = std::rc::Rc::new(move |cx: &gpui::App| position_session.read(cx).listening_position());
        let status_session = session.clone();
        let chapter_read_status = std::rc::Rc::new(move |cx: &gpui::App| status_session.read(cx).chapter_read_status());
        let reset_session = session.clone();
        let reset_chapter_read_status = std::rc::Rc::new(move |target: Option<String>, cx: &mut gpui::App| {
            reset_session.update(cx, |session, cx| {
                if let Some(target) = target {
                    session.reset_chapter_read_status_at(target, cx);
                } else {
                    session.reset_chapter_read_status(cx);
                }
            })
        });
        self.browser.update(cx, |browser, cx| browser.set_now_playing(Some(NowPlayingBook { locator: locator.clone(), controls: controls.into(), position, chapter_read_status, reset_chapter_read_status }), cx));
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

    /// An in-flight open is cancelled before navigating away from the page.
    fn cancel_opening_on_back(&mut self, _: &DismissSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening || self.pending_reader.is_some() {
            self.return_to_library(window, cx);
            cx.stop_propagation();
        }
    }

    fn on_back(&mut self, _: &DismissSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader.is_some() || self.opening {
            self.return_to_library(window, cx);
        } else {
            cx.propagate();
        }
    }

    /// Registers a host-owned surface, such as Android's embedded webpage browser.
    #[cfg(test)]
    pub fn set_native_back_overlay(&mut self, handler: impl Fn(&mut Window, &mut App) -> bool + 'static) {
        self.native_back_overlay = Some(Rc::new(handler));
    }

    /// Native navigation enters here directly, without key bindings or a focus
    /// path. False permits the host to exit only after Home has nothing to close.
    pub fn native_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if Root::dismiss_active_overlay(window, cx) {
            return true;
        }
        if self.native_back_overlay.as_ref().is_some_and(|handler| handler(window, cx)) {
            return true;
        }
        if self.opening || self.pending_reader.is_some() {
            self.return_to_library(window, cx);
            return true;
        }
        if let Some(reader) = &self.reader {
            reader.view.update(cx, |reader, cx| reader.native_back(window, cx));
            return true;
        }
        self.browser.update(cx, |browser, cx| browser.navigate_back(window, cx))
    }

    fn cancel_opening_on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening && event.keystroke.modifiers == Modifiers::default() && matches!(event.keystroke.key.as_str(), "back" | "escape" | "backspace") {
            self.return_to_library(window, cx);
            cx.stop_propagation();
        }
    }
}

impl gpui::EventEmitter<browser_ui::PaneRequest> for ApplicationPane {}

impl Render for ApplicationPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.install_playback_keys(window, cx);
        window.set_page_scroll_on_swipe(cfg!(feature = "mobile") || cx.supports_touch_input());
        let footer = {
            // No dock while the book is open in the reader, or while its detail
            // page, which then holds the controls, is on screen.
            let shown_detail = self.browser.read(cx).shown_book_detail(cx);
            let dock = self.audiobook.as_ref().filter(|audio| self.reader.as_ref().is_none_or(|reader| reader.locator != audio.locator)).filter(|audio| shown_detail.as_ref() != Some(&audio.locator)).map(|audio| audio.dock.clone());
            let pending = self.pending_audiobook.clone();
            let visible = dock.is_some() || pending.is_some();
            self.audiobook_footer.update(cx, |footer, cx| footer.set(dock, pending, cx));
            visible.then(|| AnyView::from(self.audiobook_footer.clone()))
        };
        let embedded_reader = self.split_enabled && browser_ui::native_windows_available() && !ui_components::uses_mobile_navigation(window);
        self.browser.update(cx, |browser, cx| browser.set_content_footer(footer.clone().filter(|_| self.reader.is_none() || embedded_reader), cx));
        let reader_surface = self.reader.as_ref().filter(|_| embedded_reader).map(|reader| AnyView::from(reader.view.clone()));
        self.browser.update(cx, |browser, cx| browser.set_reader_surface(reader_surface, cx));
        let content = self.reader.as_ref().filter(|_| !embedded_reader).map(|reader| AnyView::from(reader.view.clone())).unwrap_or_else(|| AnyView::from(self.browser.clone()));
        // Covers the gap between tapping a book and the async format lookup
        // in `open_book` resolving, so the tap isn't indistinguishable from
        // one that did nothing.
        let opening_overlay = self.opening.then(|| {
            let theme = ui_components::browser_theme(cx);
            gpui::div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .gap(gpui::rems(1.0))
                .items_center()
                .justify_center()
                .bg(theme.page_bg)
                .text_color(theme.text)
                .child("Opening…")
                .child(ui_components::outlined_text_button("cancel-book-opening", "Cancel").on_click(cx.listener(|root, _, window, cx| root.return_to_library(window, cx))))
        });
        let root = gpui::div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .key_context("Application")
            .track_focus(&self.opening_focus)
            .capture_key_down(cx.listener(Self::cancel_opening_on_key))
            .capture_action(cx.listener(Self::cancel_opening_on_back))
            .on_action(cx.listener(|root, _: &ui_components::CloseBookPlayer, window, cx| root.close_audiobook(window, cx)))
            .on_action(cx.listener(Self::on_back));
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
        // No bottom nav bar below the dock on this path, so it needs its own
        // padding to clear the OS gesture/button bar.
        let root = root.children(footer.filter(|_| self.reader.is_some() && !embedded_reader).map(|footer| gpui::div().w_full().flex_none().pb(window.insets().safe_area.bottom).child(footer)));
        let root = root.children(opening_overlay);
        #[cfg(all(target_os = "linux", not(feature = "mobile")))]
        let root = root.when(matches!(window.window_decorations(), gpui::Decorations::Client { .. }), |root| {
            let theme = ui_components::browser_theme(cx);
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
                root.child(
                    div()
                        .id("window-titlebar-reveal")
                        .focusable()
                        .tab_index(0)
                        .focus(move |style| style.bg(theme.accent_wash))
                        .on_action(|_: &gpui_component::menu::OpenContextMenu, window, _| window.show_window_menu(gpui::point(gpui::px(12.), NATIVE_TITLEBAR_HOVER_HEIGHT)))
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(NATIVE_TITLEBAR_HOVER_HEIGHT)
                        .on_hover(cx.listener(|root, hovered: &bool, window, cx| {
                            if *hovered && root.titlebar_reveal_armed {
                                root.titlebar_reveal_armed = false;
                                root.native_pointer_left_client = false;
                                window.request_decorations(gpui::WindowDecorations::Server);
                                root.native_decorations_visible = matches!(window.window_decorations(), gpui::Decorations::Server);
                                root.window_titlebar_visible = !root.native_decorations_visible;
                                cx.notify();
                            }
                        })),
                )
            }
        });
        root.when(!self.split_enabled, |root| root.children(Root::render_sheet_layer(window, cx).map(|sheet| gpui::deferred(sheet).with_priority(2))))
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod navigation_tests {
    use super::ApplicationPane as ApplicationRoot;
    use super::*;
    use browser_ui::{AppStartup, BrowsePageSession, LibraryRoute, Route};
    use gpui::{AnyWindowHandle, TestAppContext, WindowHandle};
    use gpui_component::{Placement, Root};
    use library_model::BrowseLocation;
    use std::cell::Cell;
    use std::time::Duration;
    use sync_common::{BookId, ROOT_DIR_ID};

    fn press_native_back(window: AnyWindowHandle, cx: &mut TestAppContext) -> bool {
        cx.update_window(window, |view, window, cx| {
            let component = view.downcast::<Root>().ok().expect("component root");
            let root = component.read(cx).view().clone().downcast::<ApplicationRoot>().ok().expect("application root");
            root.update(cx, |root, cx| root.native_back(window, cx))
        })
        .unwrap()
    }

    #[gpui::test]
    async fn native_back_never_uses_keyboard_dispatch(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let (window, root) = back_window(services, backend, library_id, LibraryRoute::Authors, cx);
        let keys = Rc::new(Cell::new(0));
        let observed = keys.clone();
        let _subscription = cx.update(|cx| {
            cx.clear_key_bindings();
            cx.observe_keystrokes(move |_, _, _| observed.set(observed.get() + 1))
        });
        cx.update_window(window.into(), |_, window, _| window.blur()).unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        assert_eq!(keys.get(), 0);
        root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx).route, Route::Library(LibraryRoute::Home)));
        assert!(!press_native_back(window.into(), cx));
    }

    #[gpui::test]
    async fn native_back_prioritizes_a_dialog_then_the_host_surface_then_home(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let (window, root) = back_window(services, backend, library_id, LibraryRoute::Home, cx);
        let host_open = Rc::new(Cell::new(true));
        let host_calls = Rc::new(Cell::new(0));
        let open = host_open.clone();
        let calls = host_calls.clone();
        root.update(cx, |root, _| {
            root.set_native_back_overlay(move |_, _| {
                calls.set(calls.get() + 1);
                open.replace(false)
            })
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.open_dialog(cx, |dialog, _, _| dialog.title("Above the browser"));
            window.blur();
        })
        .unwrap();
        cx.run_until_parked();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        assert_eq!(host_calls.get(), 0);
        assert!(host_open.get());
        assert!(press_native_back(window.into(), cx));
        assert!(!host_open.get());
        assert!(!press_native_back(window.into(), cx));
        assert_eq!(host_calls.get(), 2);
    }

    #[gpui::test]
    async fn native_back_dismisses_a_menu_before_its_dialog_without_focus(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let (window, _) = back_window(services, backend, library_id, LibraryRoute::Home, cx);
        let menu_visible = Rc::new(Cell::new(true));
        let menu = cx.update_window(window.into(), |_, window, cx| gpui_component::menu::PopupMenu::build(window, cx, |menu, _, _| menu.item(ui_components::menu_item("Sample", |_, _, _| {})))).unwrap();
        let hidden = menu_visible.clone();
        let _subscription = cx
            .update_window(window.into(), |_, window, cx| {
                window.subscribe(&menu, cx, move |_, _: &gpui::DismissEvent, window, _| {
                    hidden.set(false);
                    window.refresh();
                })
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| window.open_dialog(cx, move |dialog, _, _| dialog.title("Menu owner").when(menu_visible.get(), |dialog| dialog.child(menu.clone())))).unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.blur();
        })
        .unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| assert!(window.has_active_dialog(cx))).unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| assert!(!window.has_active_dialog(cx))).unwrap();
    }

    async fn back_fixture(cx: &mut TestAppContext) -> (tempfile::TempDir, AppServices, AppClient, LibraryId) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = AppClient::start_native(launch).unwrap();
        let library = backend.create_library_with_asset_storage("Back edge cases".into(), true).await.unwrap();
        let services = AppServices::new(backend.clone(), AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status });
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::register_fonts(cx).unwrap();
            browser_ui::bind_keys(cx);
        });
        (directory, services, backend, *library.library_id())
    }

    fn back_window(services: AppServices, backend: AppClient, library_id: LibraryId, route: LibraryRoute, cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<ApplicationRoot>) {
        let session = WindowSession { initial: NavigationSession { library_id: Some(library_id), route: Route::Library(route), ..Default::default() }, ..Default::default() };
        let window = cx.add_window(|window, cx| {
            let root = cx.new(|cx| ApplicationRoot::new(services, session, backend, "Back test", None, window, cx));
            Root::new(root, window, cx)
        });
        let root = window.update(cx, |root, _, _| root.view().clone().downcast::<ApplicationRoot>().ok().unwrap()).unwrap();
        (window, root)
    }

    #[gpui::test]
    async fn playback_keys_seek_and_preserve_reader_and_dialog_navigation(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        cx.update(|cx| reader_ui::configure(reader_ui::ReaderPreferences::default(), Rc::new(|_, _, _| {}), cx));
        let (window, root) = back_window(services, backend.clone(), library_id, LibraryRoute::Home, cx);
        let locator = BookLocator::new(library_id, BookId::from_bytes([42; 32]));
        let metadata = library_model::PlaybackMetadata {
            format: book_model::BookFormat::M4b,
            title: "Keyboard playback".into(),
            author: String::new(),
            narrator: None,
            duration_ms: 120_000,
            chapters: Vec::new(),
            tracks: Vec::new(),
            cover: Vec::new(),
        };
        let record = reader_ui::ActiveAudiobook::new(locator.clone(), 45_000, 1.0, metadata);
        let session = cx.update(|cx| reader_ui::restore_audiobook(&record, backend.library(library_id), cx));
        window
            .update(cx, |_, window, cx| {
                root.update(cx, |root, cx| {
                    root.playback.update(cx, |controller, cx| controller.replace(locator, record.metadata.title, session.clone(), cx));
                    root.sync_playback(window, cx);
                    root.opening_focus.focus(window, cx);
                })
            })
            .unwrap();
        let position = |cx: &TestAppContext| session.read_with(cx, |session, _| session.listening_position().unwrap().0);
        cx.simulate_keystrokes(window.into(), "left");
        assert_eq!(position(cx), Duration::from_secs(30));
        cx.simulate_keystrokes(window.into(), "right");
        assert_eq!(position(cx), Duration::from_secs(60));
        cx.simulate_keystrokes(window.into(), "ctrl-left shift-right");
        assert_eq!(position(cx), Duration::from_secs(60));

        window
            .update(cx, |_, window, cx| {
                window.open_dialog(cx, |dialog, _, _| dialog.title("Keep keyboard focus"));
                // The overlay must win even if focus is stale underneath it.
                let focus = root.read(cx).opening_focus.clone();
                focus.focus(window, cx);
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "left right");
        assert_eq!(position(cx), Duration::from_secs(60));
        window
            .update(cx, |_, window, cx| {
                Root::dismiss_active_overlay(window, cx);
                root.update(cx, |root, cx| {
                    let locator = BookLocator::new(library_id, BookId::from_bytes([43; 32]));
                    let view = cx.new(|cx| reader_ui::ReaderView::new(locator.clone(), "Other reader".into(), None, backend.clone(), Rc::new(backend.library(library_id)), window, cx));
                    root.reader = Some(ReaderSurface { locator, title: "Other reader".into(), view });
                    cx.notify();
                });
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "left right");
        assert_eq!(position(cx), Duration::from_secs(60), "an open reader owns the arrow keys");
        window.update(cx, |_, window, cx| root.update(cx, |root, cx| root.return_to_library(window, cx))).unwrap();
        cx.simulate_keystrokes(window.into(), "left");
        assert_eq!(position(cx), Duration::from_secs(45));
    }

    #[gpui::test]
    async fn native_back_with_stale_modal_focus_respects_stacking_and_cancel_veto(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let (window, root) = back_window(services, backend, library_id, LibraryRoute::Home, cx);
        let lower_canceled = Rc::new(Cell::new(0));
        let upper_canceled = Rc::new(Cell::new(0));
        for counter in [&lower_canceled, &upper_canceled] {
            let counter = counter.clone();
            cx.update_window(window.into(), |_, window, cx| {
                window.open_dialog(cx, move |dialog, _, _| {
                    let counter = counter.clone();
                    dialog.title("Stacked dialog").on_cancel(move |_, _, _| {
                        counter.set(counter.get() + 1);
                        true
                    })
                });
            })
            .unwrap();
        }
        cx.run_until_parked();
        // Keep the handle alive but never render it, just as when a focused
        // control is removed without a replacement claiming focus.
        let stale_focus = cx.update(|cx| cx.focus_handle());
        cx.update_window(window.into(), |_, window, cx| stale_focus.focus(window, cx)).unwrap();
        assert!(press_native_back(window.into(), cx), "an open modal must prevent Android exit even with stale focus");
        cx.run_until_parked();
        assert_eq!(upper_canceled.get(), 1);
        assert_eq!(lower_canceled.get(), 0, "one Back must close only the top dialog");
        cx.update_window(window.into(), |_, window, cx| {
            assert!(window.has_active_dialog(cx));
            window.blur();
        })
        .unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        assert_eq!(lower_canceled.get(), 1);

        let vetoed = Rc::new(Cell::new(0));
        let veto_count = vetoed.clone();
        cx.update_window(window.into(), |_, window, cx| {
            window.open_dialog(cx, move |dialog, _, _| {
                let counter = veto_count.clone();
                dialog.title("Cannot dismiss yet").on_cancel(move |_, _, _| {
                    counter.set(counter.get() + 1);
                    false
                })
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, _| window.blur()).unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        assert_eq!(vetoed.get(), 1, "fallback must use the dialog's cancellation handler");
        cx.update_window(window.into(), |_, window, cx| assert!(window.has_active_dialog(cx))).unwrap();
        root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx).route, Route::Library(LibraryRoute::Home)));
    }

    #[gpui::test]
    async fn native_back_with_unfocused_sheet_consumes_only_the_sheet(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let (window, root) = back_window(services, backend, library_id, LibraryRoute::Authors, cx);
        let closed = Rc::new(Cell::new(0));
        let close_count = closed.clone();
        cx.update_window(window.into(), |_, window, cx| {
            window.open_sheet_at(Placement::Bottom, cx, move |sheet, _, _| {
                let counter = close_count.clone();
                sheet.on_close(move |_, _, _| counter.set(counter.get() + 1))
            });
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, _| window.blur()).unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        assert_eq!(closed.get(), 1);
        cx.update_window(window.into(), |_, window, cx| assert!(!window.has_active_sheet(cx))).unwrap();
        root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx).route, Route::Library(LibraryRoute::Authors)));
    }

    #[gpui::test]
    async fn native_back_with_focus_behind_a_dialog_does_not_navigate_the_folder(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let client = backend.library(library_id);
        client.create_directory(ROOT_DIR_ID, "Folder".into()).await.unwrap();
        let folder = client.folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Folder").unwrap().id;
        let (window, root) = back_window(services, backend, library_id, LibraryRoute::Folders, cx);
        cx.update_window(window.into(), |_, window, cx| root.update(cx, |root, cx| root.browser.update(cx, |browser, cx| browser.open_folder(library_id, folder, "Folder".into(), window, cx)))).unwrap();
        cx.run_until_parked();
        let before = root.read_with(cx, |root, cx| root.browser.read(cx).navigation_session(cx));
        assert_eq!(before.pages[0].path.len(), 1);
        let canceled = Rc::new(Cell::new(0));
        let cancel_count = canceled.clone();
        cx.update_window(window.into(), |_, window, cx| {
            window.open_dialog(cx, move |dialog, _, _| {
                let counter = cancel_count.clone();
                dialog.title("Stay in folder").on_cancel(move |_, _, _| {
                    counter.set(counter.get() + 1);
                    true
                })
            });
        })
        .unwrap();
        cx.run_until_parked();
        // Deliberately give the underlying page valid focus. Its normal Back
        // handler would consume the key before an unhandled-key fallback runs.
        cx.update_window(window.into(), |_, window, cx| root.update(cx, |root, cx| root.browser.update(cx, |browser, cx| browser.focus_active_page(window, cx)))).unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        assert_eq!(canceled.get(), 1, "the visible dialog must receive Back instead of the focused page underneath");
        root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx), before));
    }

    #[gpui::test]
    async fn native_back_with_missing_focus_is_window_local_at_every_browse_root(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let (home, _) = back_window(services.clone(), backend.clone(), library_id, LibraryRoute::Home, cx);
        let stale_focus = cx.update(|cx| cx.focus_handle());
        for route in [LibraryRoute::Books, LibraryRoute::Folders, LibraryRoute::Subjects, LibraryRoute::Authors, LibraryRoute::Catalog, LibraryRoute::Trash] {
            let (window, root) = back_window(services.clone(), backend.clone(), library_id, route, cx);
            cx.update_window(home.into(), |_, window, _| window.blur()).unwrap();
            assert!(!press_native_back(home.into(), cx), "another window must not intercept Home's exit");
            root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx).route, Route::Library(route)));
            cx.update_window(window.into(), |_, window, cx| stale_focus.focus(window, cx)).unwrap();
            assert!(press_native_back(window.into(), cx), "{route:?} must return Home even when the focused control is absent");
            cx.run_until_parked();
            root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx).route, Route::Library(LibraryRoute::Home)));
        }
    }

    #[gpui::test]
    async fn native_back_with_focus_behind_library_switcher_preserves_the_folder(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let client = backend.library(library_id);
        client.create_directory(ROOT_DIR_ID, "Folder".into()).await.unwrap();
        let folder = client.folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Folder").unwrap().id;
        let (window, root) = back_window(services, backend, library_id, LibraryRoute::Folders, cx);
        cx.update_window(window.into(), |_, window, cx| root.update(cx, |root, cx| root.browser.update(cx, |browser, cx| browser.open_folder(library_id, folder, "Folder".into(), window, cx)))).unwrap();
        cx.run_until_parked();
        let before = root.read_with(cx, |root, cx| root.browser.read(cx).navigation_session(cx));
        cx.dispatch_action(window.into(), browser_ui::OpenLibrarySwitcher);
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| root.update(cx, |root, cx| root.browser.update(cx, |browser, cx| browser.focus_active_page(window, cx)))).unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx), before, "Back must close the switcher before navigating underneath it"));
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        root.read_with(cx, |root, cx| assert!(root.browser.read(cx).navigation_session(cx).pages[0].path.is_empty()));
    }

    #[gpui::test]
    async fn native_back_with_modal_over_opening_cancels_only_one_layer(cx: &mut TestAppContext) {
        let (_directory, services, backend, library_id) = back_fixture(cx).await;
        let (window, root) = back_window(services, backend, library_id, LibraryRoute::Home, cx);
        cx.update_window(window.into(), |_, window, cx| {
            root.update(cx, |root, cx| {
                root.begin_opening(window, cx);
                root.open_task = Some(cx.spawn(async |_, _| std::future::pending::<()>().await));
            });
            window.open_dialog(cx, |dialog, _, _| dialog.title("Opening dialog"));
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            let focus = root.read(cx).opening_focus.clone();
            focus.focus(window, cx);
        })
        .unwrap();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| assert!(!window.has_active_dialog(cx))).unwrap();
        root.read_with(cx, |root, _| {
            assert!(root.opening, "the same Back must not reach the opening screen's raw key handler");
            assert!(root.open_task.is_some());
        });
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        root.read_with(cx, |root, _| {
            assert!(!root.opening);
            assert!(root.open_task.is_none());
        });
    }

    #[gpui::test]
    async fn native_back_recovers_unfocused_browse_navigation_and_only_exits_at_home(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = AppClient::start_native(launch).unwrap();
        let library = backend.create_library_with_asset_storage("Back navigation".into(), true).await.unwrap();
        let client = backend.library(*library.library_id());
        client.create_directory(ROOT_DIR_ID, "Parent".into()).await.unwrap();
        let parent = client.folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Parent").unwrap().id;
        client.create_directory(parent, "Child".into()).await.unwrap();
        let child = client.folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Child").unwrap().id;
        let services = AppServices::new(backend.clone(), AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status });
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::register_fonts(cx).unwrap();
            browser_ui::bind_keys(cx);
        });
        // Back must follow the nested trail even before its asynchronous
        // listing has replaced the initial root listing.
        let pages = vec![BrowsePageSession { path: vec![(BrowseLocation::Folder(parent), "Parent".into()), (BrowseLocation::Folder(child), "Child".into())], detail: None }];
        let session = WindowSession { initial: NavigationSession { library_id: Some(*library.library_id()), route: Route::Library(LibraryRoute::Folders), pages, reader: None, ..Default::default() }, ..Default::default() };
        let window = cx.add_window(|window, cx| {
            let root = cx.new(|cx| ApplicationRoot::new(services, session, backend, "Test", None, window, cx));
            gpui_component::Root::new(root, window, cx)
        });
        let root = window.update(cx, |root, _, _| root.view().clone().downcast::<ApplicationRoot>().ok().unwrap()).unwrap();
        for remaining in [1, 0] {
            window
                .update(cx, |_, window, cx| {
                    if remaining == 1 {
                        window.blur();
                    } else {
                        let focus = root.read(cx).opening_focus.clone();
                        focus.focus(window, cx);
                    }
                })
                .unwrap();
            let handled = press_native_back(window.into(), cx);
            assert!(handled, "Back inside a folder must not finish the Android activity");
            cx.run_until_parked();
            root.read_with(cx, |root, cx| {
                let saved = root.browser.read(cx).navigation_session(cx);
                assert_eq!(saved.route, Route::Library(LibraryRoute::Folders));
                assert_eq!(saved.pages[0].path.len(), remaining);
            });
        }
        let handled = press_native_back(window.into(), cx);
        assert!(handled, "Back at a browse root must return to Home first");
        cx.run_until_parked();
        root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx).route, Route::Library(LibraryRoute::Home)));
        window.update(cx, |_, window, _| window.blur()).unwrap();
        let handled = press_native_back(window.into(), cx);
        assert!(!handled, "Back at Home must retain Android's normal exit behavior");
    }

    #[gpui::test]
    async fn native_back_dismisses_a_dialog_before_leaving_the_browse_page(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = AppClient::start_native(launch).unwrap();
        let library = backend.create_library_with_asset_storage("Back dialog".into(), true).await.unwrap();
        let services = AppServices::new(backend.clone(), AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status });
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::register_fonts(cx).unwrap();
            browser_ui::bind_keys(cx);
        });
        let session = WindowSession { initial: NavigationSession { library_id: Some(*library.library_id()), route: Route::Library(LibraryRoute::Authors), ..Default::default() }, ..Default::default() };
        let window = cx.add_window(|window, cx| {
            let root = cx.new(|cx| ApplicationRoot::new(services, session, backend, "Test", None, window, cx));
            gpui_component::Root::new(root, window, cx)
        });
        let root = window.update(cx, |root, _, _| root.view().clone().downcast::<ApplicationRoot>().ok().unwrap()).unwrap();
        cx.update_window(window.into(), |_, window, cx| window.open_dialog(cx, |dialog, _, _| dialog.title("Confirm"))).unwrap();
        cx.run_until_parked();
        let handled = press_native_back(window.into(), cx);
        assert!(handled, "a dialog must consume native Back");
        cx.background_executor.advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| assert!(!window.has_active_dialog(cx))).unwrap();
        root.read_with(cx, |root, cx| assert_eq!(root.browser.read(cx).navigation_session(cx).route, Route::Library(LibraryRoute::Authors)));
    }

    #[gpui::test]
    async fn book_open_failure_restores_the_previous_route_and_clears_the_saved_reader(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = AppClient::start_native(launch).unwrap();
        let library = backend.create_library_with_asset_storage("Opening failure".into(), true).await.unwrap();
        let services = AppServices::new(backend.clone(), AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status });
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::register_fonts(cx).unwrap();
            browser_ui::bind_keys(cx);
            reader_ui::bind_keys(cx);
            reader_ui::configure(reader_ui::ReaderPreferences::default(), Rc::new(|_, _, _| {}), cx);
        });
        let initial = NavigationSession { library_id: Some(*library.library_id()), route: Route::Library(LibraryRoute::Authors), ..Default::default() };
        let (saved, snapshots) = async_channel::unbounded();
        let session = WindowSession { initial, save: Rc::new(move |snapshot, _| saved.try_send(snapshot).unwrap()) };
        let window = cx.add_window(|window, cx| {
            let root = cx.new(|cx| ApplicationRoot::new(services, session, backend, "Test", None, window, cx));
            gpui_component::Root::new(root, window, cx)
        });
        let root = window.update(cx, |root, _, _| root.view().clone().downcast::<ApplicationRoot>().ok().unwrap()).unwrap();
        snapshots.try_recv().unwrap();
        let locator = BookLocator::new(*library.library_id(), BookId::from_bytes([19; 32]));
        window.update(cx, |_, window, cx| root.update(cx, |root, cx| root.open_book(locator, "Unavailable book".into(), None, window, cx))).unwrap();
        while snapshots.recv().await.unwrap().reader.is_none() {}
        while snapshots.recv().await.unwrap().reader.is_some() {}
        cx.run_until_parked();
        root.read_with(cx, |root, _| {
            assert!(root.reader.is_none());
            assert!(!root.opening);
            assert!(root.pending_reader.is_none());
            let saved = root.last_saved_navigation.as_ref().unwrap();
            assert!(saved.reader.is_none());
            assert_eq!(saved.route, Route::Library(LibraryRoute::Authors));
        });
        window.update(cx, |root, _, cx| assert_eq!(root.notification.read(cx).notifications().len(), 1)).unwrap();

        // Back during the pre-reader lookup cancels the opening overlay; it
        // must not navigate the still-focused browser page underneath it.
        window
            .update(cx, |_, window, cx| {
                root.update(cx, |root, cx| {
                    root.begin_opening(window, cx);
                    root.open_task = Some(cx.spawn(async |_, _| std::future::pending::<()>().await));
                    cx.notify();
                })
            })
            .unwrap();
        cx.run_until_parked();
        assert!(press_native_back(window.into(), cx));
        cx.run_until_parked();
        root.read_with(cx, |root, _| {
            assert!(!root.opening);
            assert!(root.open_task.is_none());
            assert_eq!(root.last_saved_navigation.as_ref().unwrap().route, Route::Library(LibraryRoute::Authors));
        });
    }

    #[gpui::test]
    async fn persistence_combines_browser_state_and_pending_reader_and_is_window_local(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = AppClient::start_native(launch).unwrap();
        let library = backend.create_library_with_asset_storage("Session".into(), true).await.unwrap();
        let services = AppServices::new(backend.clone(), AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status });
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::init_page_scrolling(cx);
        });
        let reader = ReaderSession { locator: BookLocator::new(*library.library_id(), BookId::from_bytes([7; 32])), title: "Pending restoration".into() };
        let initial = NavigationSession { library_id: Some(*library.library_id()), route: Route::Library(LibraryRoute::Authors), reader: Some(reader.clone()), ..Default::default() };
        let (saved, snapshots) = async_channel::unbounded();
        let session = WindowSession { initial, save: Rc::new(move |snapshot, _| saved.try_send(snapshot).unwrap()) };
        let window = cx.add_window(|window, cx| ApplicationRoot::new(services.clone(), session, backend.clone(), "Test", None, window, cx));
        let first = snapshots.try_recv().unwrap();
        assert_eq!(first.route, Route::Library(LibraryRoute::Authors));
        assert_eq!(first.reader, Some(reader));
        // Even before restoration completes, a browser snapshot preserves the
        // pending reader instead of overwriting that part of the saved session.
        window
            .update(cx, |root, window, cx| {
                root.browser.update(cx, |browser, cx| browser.open_folder(*library.library_id(), ROOT_DIR_ID, "Folders".into(), window, cx));
                root.save_navigation(cx);
            })
            .unwrap();
        let changed = snapshots.try_recv().unwrap();
        assert_eq!(changed.route, Route::Library(LibraryRoute::Folders));
        assert_eq!(changed.reader, first.reader);
        // The backend runs on native threads. Await its persisted result;
        // draining the foreground executor alone does not await those threads.
        while snapshots.recv().await.unwrap().reader.is_some() {}
        cx.run_until_parked();
        // This book does not exist: a rejected restore must clear its snapshot.
        let current = window.update(cx, |root, _, _| root.last_saved_navigation.clone().unwrap()).unwrap();
        assert!(current.reader.is_none());
        while snapshots.try_recv().is_ok() {}
        let extra_services = services.for_new_window().await.unwrap();
        let extra = cx.add_window(|window, cx| ApplicationRoot::new(extra_services, WindowSession::default(), backend, "Extra", None, window, cx));
        extra.update(cx, |root, window, cx| root.browser.update(cx, |browser, cx| browser.open_folder(*library.library_id(), ROOT_DIR_ID, "Other window".into(), window, cx))).unwrap();
        cx.run_until_parked();
        assert!(snapshots.try_recv().is_err());
        assert_eq!(window.update(cx, |root, _, _| root.last_saved_navigation.clone().unwrap()).unwrap(), current);
    }
}
