//! Account, appearance and library presentation settings.
//!
//! Preference edits use the shared [`Preferences`] store.
//!
//! The account and its sync state are the first section rather than a
//! destination of their own: signing in is something you do once, and the
//! things it governs — quota, which libraries sync — are settings like any
//! other. [`SyncPage`] keeps owning that state and is embedded here.
//!
//! The page is reachable without a pointer. It is a tab group, so arriving here
//! puts focus on the first control rather than somewhere in the window, Tab and
//! the arrow keys walk the controls in the order they are drawn, and focus
//! stays inside the page instead of escaping into the navigation rail, which
//! has no way back. Moving focus scrolls the section it landed in into view.

use std::cell::Cell;
use std::rc::Rc;

use app_preferences::{BrowsingPreferencePatch, CoverText, LibraryView, UI_FONT_SIZE_MAX, UI_FONT_SIZE_MIN};
use gpui::prelude::*;
use gpui::{Bounds, Context, Entity, FocusHandle, Focusable, IntoElement, Pixels, Render, ScrollHandle, SharedString, Window, px};
use ui_components as components;

use crate::navigation::Libraries;
use crate::stores::Preferences;
use crate::universal::sync::SyncPage;
use crate::{FocusNextSetting, FocusPreviousSetting};

/// The key context the page's focus movement is bound in. A context of its own,
/// because Tab means "next control" only here.
pub(crate) const SETTINGS_KEY_CONTEXT: &str = "BrowserSettings";

/// Room left around a section brought into view, so a control never sits
/// against the edge of the surface it was scrolled into.
const REVEAL_MARGIN: f32 = 16.0;

/// An upper bound on the walk that wraps focus back to the last control. The
/// page has a few dozen controls at most; the bound only keeps a malformed tab
/// order from spinning.
const MAX_CONTROLS: usize = 128;

/// The blocks the page scrolls between, in the order it draws them. Focus moves
/// control by control, but the page scrolls section by section: a section is the
/// unit a reader recognises, and its heading is what makes the control below it
/// make sense.
///
/// The account leads because it is the one block that can be in a state you have
/// to act on. The libraries come last: that block grows with every library on
/// the device and with whatever is syncing, and the settings people open this
/// page to change should not sit underneath it.
const UPDATES_SECTION: usize = 0;
const ACCOUNT_SECTION: usize = 1;
#[cfg(not(feature = "kobo"))]
const APPEARANCE_SECTION: usize = 2;
const FONT_SIZE_SECTION: usize = 3;
#[cfg(target_os = "android")]
const LIBRARY_STORAGE_SECTION: usize = 4;
const LIBRARIES_SECTION: usize = 5;
const SECTION_COUNT: usize = 6;

/// Where a section can be found: which focus it contains, and where it was last
/// laid out.
struct SettingsSection {
    focus: FocusHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

pub(crate) struct SettingsPage {
    updates: Option<Entity<crate::services::UpdateControls>>,
    _update_changes: Option<gpui::Subscription>,
    _library_changes: gpui::Subscription,
    preferences: Entity<Preferences>,
    account: Entity<SyncPage>,
    libraries: Entity<crate::universal::sync::LibrariesView>,
    /// The last preference write that failed, and which setting it was. It is
    /// drawn inside the group that setting lives in: one alert at the foot of
    /// the page reported a theme that would not save from below everything
    /// else on it.
    error: Option<SharedString>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    sections: Vec<SettingsSection>,
}

impl SettingsPage {
    pub(crate) fn new(updates: Option<Entity<crate::services::UpdateControls>>, account: Entity<SyncPage>, preferences: Entity<Preferences>, active_library: Entity<Libraries>, cx: &mut Context<Self>) -> Self {
        let update_changes = updates.as_ref().map(|updates| cx.observe(updates, |_, _, cx| cx.notify()));
        let libraries = cx.new(|cx| crate::universal::sync::LibrariesView::new(account.clone(), cx));
        let mut selected = active_library.read(cx).selected_id();
        let library_changes = cx.observe(&active_library, move |_page, libraries, cx| {
            let next = libraries.read(cx).selected_id();
            if next != selected {
                selected = next;
            }
            cx.notify();
        });
        // Every section keeps its slot whether or not this build draws it, so a
        // section's identity does not depend on which features are compiled in.
        let sections = (0..SECTION_COUNT).map(|_| SettingsSection { focus: cx.focus_handle(), bounds: Rc::new(Cell::new(Bounds::default())) }).collect();
        Self { updates, _update_changes: update_changes, _library_changes: library_changes, preferences, account, libraries, error: None, focus: cx.focus_handle(), scroll: ScrollHandle::default(), sections }
    }

    /// One conditional row above the existing settings groups. The backend
    /// decides which state needs attention; recoverable errors are never dialogs.
    fn update_row(&self, cx: &Context<Self>) -> Option<gpui::Div> {
        use crate::services::{UpdateAction, UpdateView};
        let controls = self.updates.as_ref()?.read(cx);
        let (message, button) = match &controls.view {
            UpdateView::Hidden => return None,
            UpdateView::Available => ("Update available".to_owned(), Some(("Update", UpdateAction::Update, false))),
            UpdateView::Preparing => ("Preparing update…".into(), Some(("Update", UpdateAction::Update, true))),
            UpdateView::WaitingForConnection => ("Waiting for connection…".into(), None),
            UpdateView::Ready { action } => ("Update ready".into(), Some((if *action == UpdateAction::Install { "Install" } else { "Restart" }, action.clone(), false))),
            UpdateView::NeedsSpace { additional_bytes } => (format!("Free up {} to continue", crate::services::updates::needed_space(*additional_bytes)), None),
            UpdateView::NeedsPermission => ("Permission needed".into(), Some(("Continue", UpdateAction::GrantPermission, false))),
            UpdateView::Activating => ("Updating your library…".into(), None),
        };
        let message = if controls.worker_failed && button.is_some() { "Could not continue update. Try again.".to_owned() } else { message };
        let theme = components::browser_theme(cx);
        let mut row = gpui::div().w_full().px(px(components::SPACE_MD)).py(px(components::SPACE_SM)).min_h(px(56.0)).flex().flex_wrap().items_center().justify_between().gap(px(components::SPACE_SM)).child(message);
        if let Some((label, action, preparing)) = button {
            let handler = controls.action.clone();
            let loading = preparing && !controls.worker_failed;
            let label = if loading { "Updating…" } else if controls.worker_failed { "Retry" } else { label };
            row = row.child(
                components::browser_settings_header_button("apply-update", label, true, theme)
                    .when(loading, |button| button.icon(gpui_component::IconName::LoaderCircle))
                    .loading(loading)
                    .on_click(move |_, _, cx| handler(action.clone(), cx)),
            );
        }
        Some(self.section(UPDATES_SECTION, components::browser_settings_group(theme).child(row)))
    }

    /// Applies a preference, and on failure says which setting did not take and
    /// what is still on screen.
    ///
    /// "Could not save setting" is true of every control on the page, which
    /// makes it useless on the one that was just pressed — and the interface
    /// goes on showing the old value, which without being told reads as the
    /// press having done nothing at all.
    fn update_browsing(&mut self, patch: BrowsingPreferencePatch, cx: &mut Context<Self>) {
        let setting = Self::setting_name(&patch);
        let showing = self.preferences.read(cx).browsing();
        let current = match &patch {
            #[cfg(not(feature = "kobo"))]
            BrowsingPreferencePatch::Theme(_) => showing.theme().label().to_owned(),
            BrowsingPreferencePatch::Palette(_, _) => {
                let appearance = showing.appearance_override().unwrap_or_else(|| components::theme::resolved_appearance(cx));
                if appearance == app_preferences::ResolvedAppearance::Dark { "Dark".to_owned() } else { format!("{} light", showing.theme().label()) }
            }
            BrowsingPreferencePatch::LibraryView(_) => showing.library_view().label().to_owned(),
            BrowsingPreferencePatch::UiFontSizePx(_) => format!("{}px", showing.ui_font_size_px()),
            BrowsingPreferencePatch::CoverText(_) => showing.cover_text().label().to_owned(),
            _ => String::new(),
        };
        let result = self.preferences.update(cx, |preferences, cx| preferences.update_browsing(patch, cx));
        self.error = result.err().map(|error| {
            let showing = if current.is_empty() { String::new() } else { format!(" Still showing {current}.") };
            SharedString::from(format!("{setting} was not saved: {error}.{showing}"))
        });
        cx.notify();
    }

    /// What a patch is called where the user pressed it.
    fn setting_name(patch: &BrowsingPreferencePatch) -> &'static str {
        match patch {
            #[cfg(not(feature = "kobo"))]
            BrowsingPreferencePatch::Theme(_) => "Application theme",
            BrowsingPreferencePatch::Palette(_, _) => "Application theme",
            BrowsingPreferencePatch::LibraryView(_) => "Book card style",
            BrowsingPreferencePatch::UiFontSizePx(_) => "UI font size",
            BrowsingPreferencePatch::CoverText(_) => "Cover text",
            _ => "Setting",
        }
    }

    /// Moves focus one control forward, wrapping at the end of the page.
    fn focus_next(&mut self, _: &FocusNextSetting, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
        if !self.focus.contains_focused(window, cx) {
            self.focus.focus(window, cx);
            window.focus_next(cx);
        }
        self.reveal_focused_section(window, cx);
    }

    /// Moves focus one control back, wrapping at the start of the page.
    fn focus_previous(&mut self, _: &FocusPreviousSetting, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev(cx);
        if !self.focus.contains_focused(window, cx) {
            self.focus_last_control(window, cx);
        }
        self.reveal_focused_section(window, cx);
    }

    /// Walks forward from the page container to whatever the last control in it
    /// is. There is no "previous from the start" step, so the way back to the
    /// end of the page is to go round it.
    fn focus_last_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let mut last = None;
        for _ in 0..MAX_CONTROLS {
            window.focus_next(cx);
            if !self.focus.contains_focused(window, cx) {
                break;
            }
            last = window.focused(cx);
        }
        if let Some(handle) = last {
            handle.focus(window, cx);
        }
    }

    /// Scrolls whichever section now holds focus into view.
    fn reveal_focused_section(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(section) = self.sections.iter().find(|section| section.focus.contains_focused(window, cx)) else {
            return;
        };
        if components::scroll_to_reveal(&self.scroll, section.bounds.get(), px(REVEAL_MARGIN)) {
            cx.notify();
        }
    }

    /// Wraps `child` in the measured, focusable block that keyboard navigation
    /// scrolls to.
    fn section(&self, index: usize, child: impl IntoElement) -> gpui::Div {
        let section = &self.sections[index];
        components::browser_settings_section(&section.focus, section.bounds.clone(), child)
    }
}

impl Focusable for SettingsPage {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let preferences = self.preferences.read(cx).browsing();
        let entity = cx.entity();

        let current_font_size = preferences.ui_font_size_px();
        let [minimum_font_size, maximum_font_size] = [UI_FONT_SIZE_MIN, UI_FONT_SIZE_MAX];

        #[cfg(not(feature = "kobo"))]
        let application_themes = {
            let selected = cx.try_global::<components::theme::SelectedPalette>().map(|selection| (selection.0, selection.1));
            let mut application_themes = components::theme_picker_group();
            for (index, (value, appearance)) in components::THEME_CHOICES.into_iter().enumerate() {
                let entity = entity.clone();
                application_themes =
                    application_themes.child(components::theme_picker_swatch(format!("application-theme-{index}"), value, appearance, components::theme_choice_selected(selected, value, appearance)).on_click(move |_, _, cx| {
                        entity.update(cx, |page, cx| page.update_browsing(BrowsingPreferencePatch::Palette(value, appearance), cx));
                    }));
            }

            application_themes
        };

        let decrease_entity = entity.clone();
        let decrease_size = current_font_size.saturating_sub(1).max(minimum_font_size);
        let decrease = components::settings_stepper_button("decrease-ui-font-size", "−", current_font_size == minimum_font_size, theme).on_click(move |_, _, cx| {
            decrease_entity.update(cx, |page, cx| page.update_browsing(BrowsingPreferencePatch::UiFontSizePx(decrease_size), cx));
        });
        let increase_entity = entity.clone();
        let increase_size = current_font_size.saturating_add(1).min(maximum_font_size);
        let increase = components::settings_stepper_button("increase-ui-font-size", "+", current_font_size == maximum_font_size, theme).on_click(move |_, _, cx| {
            increase_entity.update(cx, |page, cx| page.update_browsing(BrowsingPreferencePatch::UiFontSizePx(increase_size), cx));
        });
        let font_size = components::settings_stepper(decrease, format!("{current_font_size}px"), increase, theme);

        let current_library_view = preferences.library_view();
        let library_views = {
            let mut choices = components::browser_settings_button_group(theme);
            for (index, value) in [LibraryView::Covers, LibraryView::Detailed].into_iter().enumerate() {
                let entity = entity.clone();
                choices = choices.child(components::browser_settings_choice_button(format!("library-view-{index}"), value.label(), value == current_library_view, theme).on_click(move |_, _, cx| {
                    entity.update(cx, |page, cx| page.update_browsing(BrowsingPreferencePatch::LibraryView(value), cx));
                }));
            }
            choices
        };

        let current_cover_text = preferences.cover_text();
        let cover_texts = {
            let mut choices = components::browser_settings_button_group(theme);
            for (index, value) in [CoverText::Always, CoverText::CoverOnly].into_iter().enumerate() {
                let entity = entity.clone();
                choices = choices.child(components::browser_settings_choice_button(format!("cover-text-{index}"), value.label(), value == current_cover_text, theme).on_click(move |_, _, cx| {
                    entity.update(cx, |page, cx| page.update_browsing(BrowsingPreferencePatch::CoverText(value), cx));
                }));
            }
            choices
        };

        let appearance_settings = components::browser_settings_fieldset("Appearance", theme);
        #[cfg(not(feature = "kobo"))]
        let appearance_settings = appearance_settings.child(self.section(APPEARANCE_SECTION, components::browser_settings_compact_row("Application theme", application_themes, theme)));
        let appearance_settings = appearance_settings.child(components::browser_settings_compact_row("Book card style", library_views, theme)).child(components::browser_settings_compact_row("Cover text", cover_texts, theme));
        let appearance_settings = appearance_settings.child(self.section(FONT_SIZE_SECTION, components::browser_settings_compact_row("UI font size", font_size, theme)));
        // Every preference on this page lives in this group, so this is where a
        // preference that would not save reports.
        let appearance_settings = appearance_settings.children(self.error.clone().map(|error| components::browser_settings_group_error(error, theme)));

        let mut sections = components::browser_settings_sections().child(appearance_settings);

        #[cfg(target_os = "android")]
        {
            let access = components::browser_settings_path_button("android-file-access", "Manage file access", theme).on_click(|_, _, _| gpui_android::request_all_files_access());
            sections = sections.child(self.section(LIBRARY_STORAGE_SECTION, components::browser_settings_fieldset("Library storage", theme).child(components::browser_settings_compact_row("Android permissions", access, theme))));
        }

        sections = sections.child(self.section(LIBRARIES_SECTION, self.libraries.clone()));

        // Keep account and library invalidations inside their own entities.
        // Transfer progress reaches the library rows several times a second and
        // must not rebuild the settings controls above them.
        components::browser_settings_page_with_scroll(&self.scroll)
            .key_context(SETTINGS_KEY_CONTEXT)
            .track_focus(&self.focus)
            .tab_group()
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .child(components::browser_settings_content().children(self.update_row(cx)).child(self.section(ACCOUNT_SECTION, self.account.clone())).child(sections))
    }
}
