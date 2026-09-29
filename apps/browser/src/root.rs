//! Window-level browser shell.

use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Subscription, Window, px};
use gpui_component::Root;
use library_model::BookLocator;

use crate::navigation::{BrowserShell, GlobalRoute};
use crate::services::AppServices;
use crate::stores::Preferences;
use crate::{DecreaseUiFontSize, DismissSettings, IncreaseUiFontSize, ShowSettings};

pub struct OpenBookRequested {
    pub locator: BookLocator,
    pub title: String,
    pub initial_target: Option<String>,
}

/// The cached book detail page just closed — e.g. an audiobook dock expanded
/// to show it can collapse back down.
pub struct BookDetailDismissed;

/// Owns window policy and composes the browser shell.
pub struct BrowserRoot {
    #[cfg(target_arch = "wasm32")]
    _import_notifications: gpui::Task<()>,
    preferences: Entity<Preferences>,
    shell: Entity<BrowserShell>,
    focus: FocusHandle,
    _shell_subscription: Subscription,
    _preferences_subscription: Subscription,
}

impl BrowserRoot {
    #[cfg(target_arch = "wasm32")]
    pub fn apply_sync_event(&mut self, event: crate::SyncEvent, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.apply_sync_event(event, cx));
    }
    pub fn set_content_footer(&mut self, footer: Option<gpui::AnyView>, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.set_content_footer(footer, cx));
    }

    pub fn open_book_detail(&mut self, library_id: sync_common::LibraryId, content_hash: sync_common::ContentHash, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.open_book_detail(library_id, content_hash, window, cx));
    }

    pub fn clear_cached_book_details(&mut self, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.clear_cached_book_details(cx));
    }
    pub fn open_folder(&mut self, library_id: sync_common::LibraryId, directory_id: sync_common::DirId, title: String, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.open_folder(library_id, directory_id, title, window, cx));
    }

    pub fn open_subject(&mut self, library_id: sync_common::LibraryId, location: String, label: String, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.open_subject(library_id, location, label, window, cx));
    }

    /// Builds the window root and its browser shell.
    pub fn new(services: AppServices, sync_state: Entity<crate::SyncState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.set_system_bars_visible(true);
        #[cfg(target_arch = "wasm32")]
        let import_notifications = crate::services::import_progress::watch_browser_imports(services.backend.clone(), services.sync_events.clone().expect("web sync event inbox"), window, cx);
        let preferences = cx.new(|cx| Preferences::new(services.clone(), cx));
        let theme_preferences = preferences.clone();
        cx.set_global(ui_components::theme::PaletteSelectionHandler(std::rc::Rc::new(move |theme, appearance, cx| {
            theme_preferences.update(cx, |preferences, cx| {
                if let Err(error) = preferences.update_browsing(app_preferences::BrowsingPreferencePatch::Palette(theme, appearance), cx) {
                    log::error!("failed to select application palette: {error}");
                }
            });
        })));
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
        let open_book: crate::library::OpenBook = std::rc::Rc::new(move |locator, title, initial_target, _, cx| {
            let _ = browser.update(cx, |_, cx| cx.emit(OpenBookRequested { locator, title, initial_target }));
        });
        let browser = cx.entity().downgrade();
        let book_detail_closed: crate::library::BookDetailClosed = std::rc::Rc::new(move |cx| {
            let _ = browser.update(cx, |_, cx| cx.emit(BookDetailDismissed));
        });
        let shell = cx.new(|cx| BrowserShell::new(services.clone(), sync_state, open_book, book_detail_closed, preferences.clone(), window, cx));
        let shell_subscription = cx.observe(&shell, |_, _, cx| cx.notify());
        let shell_for_preferences = shell.clone();
        let preferences_subscription = cx.observe(&preferences, move |_, preferences, cx| {
            let view = preferences.read(cx).browsing().library_view();
            shell_for_preferences.update(cx, |shell, cx| shell.set_library_view(view, cx));
        });
        let initial_view = preferences.read(cx).browsing().library_view();
        shell.update(cx, |shell, cx| shell.set_library_view(initial_view, cx));

        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            #[cfg(target_arch = "wasm32")]
            _import_notifications: import_notifications,
            preferences,
            shell,
            focus,
            _shell_subscription: shell_subscription,
            _preferences_subscription: preferences_subscription,
        }
    }

    pub fn contains_library(&self, library_id: &sync_common::LibraryId, cx: &App) -> bool {
        self.shell.read(cx).contains_library(library_id, cx)
    }

    /// Restores keyboard focus to the visible page, for a caller returning to
    /// the browser from somewhere else.
    pub fn focus_active_page(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.focus_active_page(window, cx));
    }

    /// Imports a staged native file through the browser shell.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn import_and_open_path(&mut self, name: String, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.import_and_open_path(name, path, window, cx));
    }

    /// Reports a failure that occurred before an incoming file reached the shell.
    pub fn incoming_import_failed(&mut self, error: String, window: &mut Window, cx: &mut Context<Self>) {
        self.shell.update(cx, |shell, cx| shell.incoming_import_failed(error, window, cx));
    }

    /// Closes shell UI before allowing the action to propagate.
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
            crate::services::notify_error("browser-error", format!("Could not save interface font size: {error}"), window, cx);
        }
    }
}

impl EventEmitter<OpenBookRequested> for BrowserRoot {}
impl EventEmitter<BookDetailDismissed> for BrowserRoot {}

impl Focusable for BrowserRoot {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for BrowserRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ui_components::browser_theme(cx);
        let preferences = self.preferences.read(cx);
        let ui_font_size = preferences.ui_font_size_px();
        window.set_rem_size(px(ui_font_size as f32));

        ui_components::app_page(preferences.interface_font_name(), ui_font_size, theme)
            .key_context("Browser")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(|root, _: &DecreaseUiFontSize, window, cx| root.adjust_ui_font_size(-1, window, cx)))
            .on_action(cx.listener(|root, _: &IncreaseUiFontSize, window, cx| root.adjust_ui_font_size(1, window, cx)))
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
            .children(Root::render_dialog_layer(window, cx))
            .into_any_element()
    }
}
