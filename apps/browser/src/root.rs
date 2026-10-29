//! Window-level browser shell.

use ui_components::user_facing_error;

use app_preferences::{ApplicationTheme, BrowsingPreferencePatch, ResolvedAppearance};
use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Subscription, Window, px};
use gpui_component::{Root, WindowExt, dialog::DialogButtonProps};
use library_model::BookLocator;
use std::rc::Rc;
use ui_components::theme::{PaletteSelectionHandler, PaletteSelectionService};

use crate::navigation::{BrowserShell, GlobalRoute};
use crate::services::AppServices;
use crate::stores::Preferences;
use crate::{DecreaseUiFontSize, DismissSettings, IncreaseUiFontSize, NavigationSession, ShowSettings};

pub struct OpenBookRequested {
    pub locator: BookLocator,
    pub title: String,
    pub initial_target: Option<String>,
}

/// The audiobook that is playing, published by the application shell. That
/// book's detail page shows `controls` in place of Listen and its reading
/// position, and the shell hides the dock while the page is on screen.
#[derive(Default)]
pub struct NowPlaying {
    pub book: Option<NowPlayingBook>,
}

pub struct NowPlayingBook {
    pub locator: BookLocator,
    pub controls: gpui::AnyView,
    /// Where playback is now and the per cent of the book that is, read live
    /// from the player, so the page's chapter mark and cover bar keep up
    /// rather than waiting for the position to be saved and reloaded.
    pub position: Rc<dyn Fn(&App) -> Option<(std::time::Duration, f32)>>,
    pub chapter_read_status: Rc<dyn Fn(&App) -> book_model::ChapterReadStatus>,
    pub reset_chapter_read_status: Rc<dyn Fn(Option<String>, &mut App)>,
}

impl gpui::Global for NowPlaying {}

struct PalettePreferences {
    preferences: Entity<Preferences>,
}

impl PaletteSelectionService for PalettePreferences {
    fn select_palette(&self, theme: ApplicationTheme, appearance: ResolvedAppearance, cx: &mut App) {
        self.preferences.update(cx, |preferences, cx| {
            if let Err(error) = preferences.update_browsing(BrowsingPreferencePatch::Palette(theme, appearance), cx) {
                log::error!("failed to select application palette: {error}");
            }
        });
    }
}

/// Owns window policy and composes the browser shell.
pub struct BrowserRoot {
    preferences: Entity<Preferences>,
    shell: Entity<BrowserShell>,
    split_enabled: bool,
    focus: FocusHandle,
    _shell_subscription: Subscription,
    _preference_subscription: Subscription,
    updates: Option<Entity<crate::services::UpdateControls>>,
    _update_subscription: Option<Subscription>,
}

impl BrowserRoot {
    pub fn restore_navigation(&mut self, saved: NavigationSession, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.restore_navigation(saved, window, cx));
    }

    pub fn set_now_playing(&self, book: Option<NowPlayingBook>, cx: &mut App) {
        self.shell.update(cx, |shell, cx| shell.set_now_playing(book, cx));
    }

    pub fn set_reader_surface(&mut self, surface: Option<gpui::AnyView>, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.set_reader_surface(surface, cx));
    }

    pub fn set_split_close_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.split_enabled = enabled;
        self.shell.update(cx, |shell, cx| shell.set_split_close_enabled(enabled, cx));
    }
    #[cfg(target_arch = "wasm32")]
    pub fn apply_sync_event(&mut self, event: crate::SyncEvent, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.apply_sync_event(event, cx));
    }
    pub fn set_content_footer(&mut self, footer: Option<gpui::AnyView>, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.set_content_footer(footer, cx));
    }

    pub fn open_book_detail(&mut self, library_id: sync_common::LibraryId, content_hash: sync_common::BookId, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.open_book_detail(library_id, content_hash, window, cx));
    }

    /// The library book whose detail page is on screen, if one is.
    pub fn shown_book_detail(&self, cx: &App) -> Option<BookLocator> {
        self.shell.read(cx).shown_book_detail(cx)
    }

    pub fn open_folder(&mut self, library_id: sync_common::LibraryId, directory_id: sync_common::DirId, title: String, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.open_folder(library_id, directory_id, title, window, cx));
    }

    pub fn open_subject(&mut self, library_id: sync_common::LibraryId, location: library_model::BrowseLocation, label: String, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.open_subject(library_id, location, label, window, cx));
    }

    /// Builds the window root and its browser shell.
    pub fn new(services: AppServices, saved: NavigationSession, sync_state: Entity<crate::SyncState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.set_system_bars_visible(true);
        let preferences = Preferences::shared(&services, cx);
        let preference_subscription = cx.observe(&preferences, |_, _, cx| cx.notify());
        cx.set_global(PaletteSelectionHandler(Rc::new(PalettePreferences { preferences: preferences.clone() })));
        let appearance_preferences = preferences.clone();
        window
            .observe_window_appearance(move |_, cx| {
                let browsing = appearance_preferences.read(cx).browsing();
                // Re-resolve when following the system; an explicit palette
                // stays fixed and `apply_appearance` refreshes the windows.
                ui_components::theme::apply_appearance(&browsing, cx);
            })
            .detach();

        let browser = cx.entity().downgrade();
        let open_book: crate::library::OpenBook = Rc::new(move |locator, title, initial_target, _, cx| {
            let _ = browser.update(cx, |_, cx| cx.emit(OpenBookRequested { locator, title, initial_target }));
        });
        let shell = cx.new(|cx| BrowserShell::new(services.clone(), saved, sync_state, open_book, preferences.clone(), window, cx));
        cx.subscribe(&shell, |_, _, request: &crate::PaneRequest, cx| cx.emit(request.clone())).detach();
        let shell_subscription = cx.observe(&shell, |_, _, cx| cx.notify());

        let updates = services.updates.clone();
        let update_subscription = updates.as_ref().map(|updates| cx.observe_in(updates, window, |root, _, window, cx| root.prompt_for_update(window, cx)));
        // A persisted offer can already be available before the first window.
        // Wait until the enclosing component Root exists before opening a dialog.
        cx.defer_in(window, |root, window, cx| root.prompt_for_update(window, cx));
        cx.observe_window_activation(window, |root, window, cx| root.prompt_for_update(window, cx)).detach();

        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self { preferences, shell, split_enabled: false, focus, _shell_subscription: shell_subscription, _preference_subscription: preference_subscription, updates, _update_subscription: update_subscription }
    }

    fn prompt_for_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.is_window_active() {
            return;
        }
        let Some(updates) = &self.updates else { return };
        let Some(handler) = updates.update(cx, |updates, _| updates.take_available_prompt()) else { return };
        let shell = self.shell.downgrade();
        window.open_alert_dialog(cx, move |dialog, window, _| {
            let handler = handler.clone();
            let shell = shell.clone();
            ui_components::confirmation_dialog(dialog, window)
                .title("Update available")
                .description("A new Bokheim update is available. Would you like to update now?")
                .button_props(DialogButtonProps::default().ok_text("Update").cancel_text("Later").show_cancel(true))
                .on_ok(move |_, window, cx| {
                    // The existing Settings flow shows download progress and
                    // the platform's permission, install, or restart action.
                    let shell = shell.clone();
                    let handler = handler.clone();
                    window.defer(cx, move |window, cx| {
                        let _ = shell.update(cx, |shell, cx| shell.navigate(GlobalRoute::Settings.into(), window, cx));
                        handler(crate::services::UpdateAction::Update, cx);
                    });
                    true
                })
        });
    }

    pub fn navigation_session(&self, cx: &App) -> NavigationSession {
        self.shell.read(cx).navigation_session(cx)
    }

    pub fn contains_library(&self, library_id: &sync_common::LibraryId, cx: &App) -> bool {
        self.shell.read(cx).contains_library(library_id, cx)
    }

    /// Restores keyboard focus to the visible page, for a caller returning to
    /// the browser from somewhere else.
    pub fn focus_active_page(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.focus_active_page(window, cx));
    }

    pub fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.shell.update(cx, |shell, cx| shell.navigate_back(window, cx))
    }

    /// Imports a staged native file through the browser shell.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn import_and_open_path(&mut self, name: String, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.import_and_open_path(name, path, window, cx));
    }

    /// Reports a failure that occurred before an incoming file reached the shell.
    pub fn incoming_import_failed(&mut self, error: app::BackendError, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.incoming_import_failed(error, window, cx));
    }

    /// Closes shell UI before allowing the action to propagate.
    fn capture_dismiss(&mut self, _: &DismissSettings, window: &mut Window, cx: &mut Context<Self>) {
        if self.shell.update(cx, |shell, cx| shell.dismiss(window, cx)) {
            cx.stop_propagation();
        }
    }

    fn on_dismiss(&mut self, _: &DismissSettings, window: &mut Window, cx: &mut Context<Self>) {
        let handled = self.shell.update(cx, |shell, cx| shell.dismiss(window, cx));
        if !handled {
            cx.propagate();
        }
    }

    /// Adjusts and persists the window's interface font size.
    fn adjust_ui_font_size(&mut self, delta: i16, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.preferences.read(cx).ui_font_size_px();
        let next = (i16::from(current) + delta).clamp(i16::from(app_preferences::UI_FONT_SIZE_MIN), i16::from(app_preferences::UI_FONT_SIZE_MAX)) as u8;
        if next == current {
            return;
        }
        if let Err(error) = self.preferences.update(cx, |preferences, cx| preferences.update_browsing(app_preferences::BrowsingPreferencePatch::UiFontSizePx(next), cx)) {
            crate::services::notify_error("browser-error", format!("Could not save interface font size. {}", user_facing_error(&error, "Please try again.")), window, cx);
        }
    }
}

impl EventEmitter<OpenBookRequested> for BrowserRoot {}
impl EventEmitter<crate::PaneRequest> for BrowserRoot {}

impl Focusable for BrowserRoot {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for BrowserRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_page_scroll_on_swipe(cfg!(feature = "mobile") || cx.supports_touch_input());
        let theme = ui_components::browser_theme(cx);
        let preferences = self.preferences.read(cx);
        let ui_font_size = preferences.ui_font_size_px();
        window.set_rem_size(px(ui_font_size as f32));

        ui_components::app_page(preferences.interface_font_name(cx), ui_font_size, theme)
            .key_context("Browser")
            .track_focus(&self.focus)
            .capture_action(cx.listener(Self::capture_dismiss))
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(|root, _: &DecreaseUiFontSize, window, cx| root.adjust_ui_font_size(-1, window, cx)))
            .on_action(cx.listener(|root, _: &IncreaseUiFontSize, window, cx| root.adjust_ui_font_size(1, window, cx)))
            // Handled here, the ancestor of everything, because it is sent from
            // a popup menu rather than from the page that opened it.
            .on_action(cx.listener(|root, open: &crate::OpenLibraryView, window, cx| {
                root.shell.update(cx, |shell, cx| shell.navigate(crate::Route::Library(open.0), window, cx));
            }))
            .on_action(cx.listener(|root, _: &crate::PreviousDestination, window, cx| {
                root.shell.update(cx, |shell, cx| shell.step_destination(-1, window, cx));
            }))
            .on_action(cx.listener(|root, _: &crate::NextDestination, window, cx| {
                root.shell.update(cx, |shell, cx| shell.step_destination(1, window, cx));
            }))
            .on_action(cx.listener(|root, _: &crate::OpenLibrarySwitcher, window, cx| {
                root.shell.update(cx, |shell, cx| shell.open_library_switcher(window, cx));
            }))
            .on_action(cx.listener(|root, _: &ShowSettings, window, cx| {
                root.shell.update(cx, |shell, cx| shell.navigate(GlobalRoute::Settings.into(), window, cx));
            }))
            .child(self.shell.clone())
            .when(!self.split_enabled, |root| root.children(Root::render_dialog_layer(window, cx)))
            .into_any_element()
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod session_tests {
    use super::*;
    use crate::{AppStartup, LibraryRoute, NavigationSession, Route, SyncState};
    use gpui::TestAppContext;
    use sync_common::ROOT_DIR_ID;

    #[gpui::test]
    async fn selected_library_and_page_restore_and_snapshots_follow_navigation(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = app::AppClient::start_native(launch).unwrap();
        let first = backend.create_library_with_asset_storage("First".into(), true).await.unwrap();
        let second = backend.create_library_with_asset_storage("Second".into(), true).await.unwrap();
        let startup = AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status };
        // Leave this library out of the startup list so its first navigation
        // request waits for asynchronous discovery.
        let third = backend.create_library_with_asset_storage("Discovered later".into(), true).await.unwrap();
        let saved = NavigationSession { library_id: Some(*second.library_id()), route: Route::Library(LibraryRoute::Authors), ..Default::default() };
        let services = AppServices::new(backend, startup);
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::init_page_scrolling(cx);
        });
        let sync = cx.new(|_| SyncState::new(&services));
        let window = cx.add_window(|window, cx| BrowserRoot::new(services.clone(), saved.clone(), sync, window, cx));
        let snapshot = window.update(cx, |root, _, cx| root.navigation_session(cx)).unwrap();
        assert_eq!(snapshot.library_id, saved.library_id);
        assert_eq!(snapshot.route, saved.route);

        window
            .update(cx, |root, window, cx| {
                root.shell.update(cx, |shell, cx| {
                    shell.open_folder(*third.library_id(), ROOT_DIR_ID, "Stale folder request".into(), window, cx);
                    shell.open_subject(*first.library_id(), library_model::BrowseLocation::Subject(0), "Subjects".into(), window, cx);
                })
            })
            .unwrap();
        cx.run_until_parked();
        let snapshot = window.update(cx, |root, _, cx| root.navigation_session(cx)).unwrap();
        assert_eq!(snapshot.library_id, Some(*first.library_id()));
        assert_eq!(snapshot.route, Route::Library(LibraryRoute::Subjects));
        let restored = NavigationSession { library_id: Some(*third.library_id()), route: GlobalRoute::Settings.into(), ..Default::default() };
        window.update(cx, |root, window, cx| root.shell.update(cx, |shell, cx| shell.restore_navigation(restored.clone(), window, cx))).unwrap();
        let shell = window.update(cx, |root, _, _| root.shell.clone()).unwrap();
        cx.condition(&shell, |shell, cx| shell.navigation_session(cx).library_id == restored.library_id).await;
        let snapshot = window.update(cx, |root, _, cx| root.navigation_session(cx)).unwrap();
        assert_eq!(snapshot.library_id, restored.library_id);
        assert_eq!(snapshot.route, restored.route, "a superseded folder request must not reopen when its library is selected later");

        window.update(cx, |root, window, cx| root.open_folder(*first.library_id(), ROOT_DIR_ID, "Folders".into(), window, cx)).unwrap();
        cx.run_until_parked();
        let snapshot = window.update(cx, |root, _, cx| root.navigation_session(cx)).unwrap();
        assert_eq!(snapshot.library_id, Some(*first.library_id()));
        assert_eq!(snapshot.route, Route::Library(LibraryRoute::Folders));

        window.update(cx, |root, window, cx| root.shell.update(cx, |shell, cx| shell.navigate(GlobalRoute::Settings.into(), window, cx))).unwrap();
        cx.run_until_parked();
        let snapshot = window.update(cx, |root, _, cx| root.navigation_session(cx)).unwrap();
        assert_eq!(snapshot.route, Route::Global(GlobalRoute::Settings));
    }

    #[gpui::test]
    async fn windows_share_preferences_but_keep_independent_navigation(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = app::AppClient::start_native(launch).unwrap();
        let library = backend.create_library_with_asset_storage("Shared".into(), true).await.unwrap();
        let services = AppServices::new(backend.clone(), AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status });
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::init_page_scrolling(cx);
        });
        let sync = cx.new(|_| SyncState::new(&services));
        let first = cx.add_window(|window, cx| BrowserRoot::new(services.clone(), NavigationSession::default(), sync.clone(), window, cx));
        first.update(cx, |root, _, cx| root.preferences.update(cx, |preferences, cx| preferences.update_browsing(BrowsingPreferencePatch::UiFontSizePx(18), cx)).unwrap()).unwrap();
        // A new window receives startup data from before the edit. It must read
        // the live application owner instead of resetting it to that snapshot.
        let extra_services = services.for_new_window().await.unwrap();
        let second = cx.add_window(|window, cx| BrowserRoot::new(extra_services, NavigationSession::default(), sync, window, cx));
        let first_preferences = first.update(cx, |root, _, _| root.preferences.clone()).unwrap();
        let second_preferences = second.update(cx, |root, _, _| root.preferences.clone()).unwrap();
        assert_eq!(first_preferences.entity_id(), second_preferences.entity_id());
        let shortcuts = app_preferences::KeyboardShortcuts::from([("browse.undo".into(), vec!["alt-z".into()])]);
        let shortcut_save = first.update(cx, |root, _, cx| root.preferences.update(cx, |preferences, cx| preferences.save_keyboard_shortcuts(shortcuts.clone(), cx))).unwrap();
        shortcut_save.await.unwrap();
        second.update(cx, |root, _, cx| assert_eq!(root.preferences.read(cx).browsing().keyboard_shortcuts(), &shortcuts)).unwrap();
        assert_eq!(second.update(cx, |root, _, cx| root.preferences.read(cx).ui_font_size_px()).unwrap(), 18);
        let metered_save = first.update(cx, |root, _, cx| root.preferences.update(cx, |preferences, cx| preferences.save_metered_transfers(true, cx))).unwrap();
        second.update(cx, |root, _, cx| root.preferences.update(cx, |preferences, cx| preferences.update_browsing(BrowsingPreferencePatch::CatalogLanguages(Some(vec!["swe".into()])), cx)).unwrap()).unwrap();
        metered_save.await.unwrap();
        assert!(second.update(cx, |root, _, cx| root.preferences.read(cx).browsing().allow_metered_transfers()).unwrap());
        assert_eq!(first.update(cx, |root, _, cx| root.preferences.read(cx).browsing().catalog_languages().map(<[String]>::to_vec)).unwrap(), Some(vec!["swe".into()]));
        first.update(cx, |root, window, cx| root.open_folder(*library.library_id(), ROOT_DIR_ID, "Folders".into(), window, cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(first.update(cx, |root, _, cx| root.navigation_session(cx).route).unwrap(), Route::Library(LibraryRoute::Folders));
        assert_eq!(second.update(cx, |root, _, cx| root.navigation_session(cx).route).unwrap(), Route::Library(LibraryRoute::Home));
    }
}
