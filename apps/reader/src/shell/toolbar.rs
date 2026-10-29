//! Sidebar chrome shared by every reader surface.
//!
//! On desktop the sidebar is an icon rail beside one panel. The rail leads to
//! the library and to each panel, starts and pauses reading aloud, and carries
//! the book's progress ring — the one place progress is shown. The rail stays
//! visible when the panel closes, so its buttons can open a panel again.
//!
//! On a phone the panels are sheets. The bar above the page opens search and
//! shows progress; devices without native Back also get a library button. The
//! bar below the page opens the other sheets and drives reading aloud.

use std::{cell::Cell, rc::Rc};

use super::search::{self, SearchBarTarget};
use super::state_panel::{SidebarTab, adjacent_available_tab, sidebar_tabs};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, FocusHandle, Window, div, px};
use gpui_component::IconName;
use ui_components::{self as components, ReaderDestination, ReaderRailMark};

/// Operations provided by the entity that owns a reader sidebar.
pub(crate) trait SidebarTarget: SearchBarTarget {
    fn available_tabs(&self) -> &'static [SidebarTab] {
        sidebar_tabs(self.contents_available())
    }
    #[cfg(feature = "kobo")]
    fn device_controls(&self) -> Option<gpui::AnyView>;
    fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>);
    fn hide_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>);
    fn sidebar_focus(&self) -> FocusHandle;
    fn rail_focus(&self) -> FocusHandle;
    fn keyboard_visibility(&self) -> Rc<Cell<bool>>;
    fn sidebar_tab(&self) -> SidebarTab;
    fn contents_available(&self) -> bool;
    fn select_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>);
    /// Mobile navigation closes the sheet when its current tab is tapped again.
    fn toggle_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>);
    fn focus_sidebar_body(&mut self, window: &mut Window, cx: &mut Context<Self>);
    fn settings_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool;
    /// The TTS button's state, or `None` where the reader cannot read aloud.
    fn tts_mark(&self) -> Option<ReaderRailMark> {
        None
    }
    /// Starts reading aloud, opening the dock, or pauses and resumes it.
    fn press_tts(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}

/// The format-specific content placed into the common sidebar structure.
pub(crate) struct SidebarContent {
    /// The open panel's body: a TOC tree, annotation rows, search results, or
    /// the settings — whichever `SidebarTab` is selected.
    body: AnyElement,
}

impl SidebarContent {
    pub(crate) fn new(body: AnyElement) -> Self {
        Self { body }
    }
}

fn panel_destination(tab: SidebarTab) -> ReaderDestination {
    match tab {
        SidebarTab::Contents => ReaderDestination::Contents,
        SidebarTab::Notes => ReaderDestination::Notes,
        SidebarTab::Search => ReaderDestination::Search,
        SidebarTab::Settings => ReaderDestination::Settings,
    }
}

/// `owner` is already leased by GPUI during rendering. Read state through it;
/// the entity handle is only for callbacks that run after rendering completes.
pub(crate) fn reader_sidebar<C: SidebarTarget>(owner: &C, target: &Entity<C>, content: SidebarContent, theme: components::BrowserTheme, window: &mut Window, cx: &mut Context<C>) -> AnyElement {
    let mobile = components::uses_mobile_reader_controls(window);
    if mobile {
        install_sidebar_keyboard(owner, target, window, cx);
    }
    let close_target = target.clone();
    let focus_target = target.clone();
    let sidebar_focus = owner.sidebar_focus();
    let active = owner.sidebar_tab();

    let header = components::reader_panel_header(panel_destination(active).label(), theme)
        .when(active == SidebarTab::Search, |header| header.child(search::header_controls(owner, target, theme)))
        .child(components::outlined_icon_button("reader-sidebar-close", components::shortcut_tooltip("Close sidebar", "escape"), IconName::Close).debug_selector(|| "reader-sidebar-close".into()).on_click(move |_, window, cx| {
            cx.stop_propagation();
            close_target.update(cx, |this, cx| this.hide_sidebar(window, cx));
        }));

    #[cfg(feature = "kobo")]
    let header = div()
        .flex_none()
        .w_full()
        .flex()
        .flex_col()
        .child(header)
        .when(active == SidebarTab::Settings, |header| {
            header.when_some(owner.device_controls(), |header, controls| header.child(div().key_context("ReaderDeviceControls").border_b_1().border_color(theme.rule).child(controls)))
        });

    let panel = div().flex_1().min_w_0().h_full().min_h_0().flex().flex_col().when(mobile, |panel| panel.child(components::reader_sheet_grab(theme))).child(header).child(
        div()
            .track_focus(&sidebar_focus)
            .tab_group()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                focus_target.update(cx, |this, cx| {
                    if this.sidebar_tab() == SidebarTab::Settings {
                        this.sidebar_focus().focus(window, cx);
                        cx.notify();
                    }
                });
            })
            .child(content.body),
    );

    div()
        .key_context("ReaderSidebar")
        .occlude()
        // Keep wheel events inside the sidebar, including at list boundaries.
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .on_action(window.listener_for(target, |this, _: &crate::CloseSearch, window, cx| {
            this.hide_sidebar(window, cx);
        }))
        .size_full()
        .min_h_0()
        .min_w_0()
        .flex()
        .bg(theme.page_bg)
        .child(panel)
        .into_any_element()
}

/// Intercept before GPUI matches bindings: inputs and trees own bindings that
/// would otherwise consume Tab or move the cursor before it is first revealed.
fn install_sidebar_keyboard<C: SidebarTarget>(owner: &C, target: &Entity<C>, window: &mut Window, cx: &mut Context<C>) {
    let target = target.downgrade();
    let window_handle = window.window_handle();
    let visible = owner.keyboard_visibility();
    window.use_keyed_state(("reader-sidebar-keyboard", cx.entity_id()), cx, move |_, cx| {
        cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != window_handle
                || !event.context_stack.iter().any(|context| context.contains("ReaderSidebar"))
                || event.context_stack.iter().any(|context| context.contains("ShortcutSettings") || context.contains("ReaderFontPicker") || context.contains("ReaderDeviceControls"))
            {
                return;
            }
            let Some(target) = target.upgrade() else { return };
            let key = &event.keystroke;
            // Reader panel shortcuts work from every tab, including inputs.
            if components::matches_shortcut("reader.search", key, cx) || components::matches_shortcut("reader.contents", key, cx) || components::matches_shortcut("reader.settings", key, cx) {
                return;
            }
            if components::matches_shortcut("reader.next_tab", key, cx) || components::matches_shortcut("reader.previous_tab", key, cx) {
                target.update(cx, |this, cx| {
                    visible.set(!components::uses_mobile_reader_controls(window));
                    let direction = if components::matches_shortcut("reader.previous_tab", key, cx) { -1 } else { 1 };
                    let tab = adjacent_available_tab(this.sidebar_tab(), this.available_tabs(), direction);
                    this.select_sidebar_tab(tab, window, cx);
                });
                cx.stop_propagation();
                return;
            }
            if components::matches_shortcut("reader.close_search", key, cx) {
                target.update(cx, |this, cx| this.hide_sidebar(window, cx));
                cx.stop_propagation();
                return;
            }
            target.update(cx, |this, cx| {
                let navigation = !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform && matches!(key.key.as_str(), "up" | "down" | "left" | "right" | "enter" | "space" | "home" | "end" | "pageup" | "pagedown");
                let mobile = components::uses_mobile_reader_controls(window);
                let first = !mobile && !visible.replace(true);
                if first {
                    cx.notify();
                }
                let focus = this.sidebar_focus();
                if first && navigation {
                    if !focus.contains_focused(window, cx) || focus.is_focused(window) {
                        this.focus_sidebar_body(window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                if navigation && this.sidebar_tab() == SidebarTab::Search && matches!(key.key.as_str(), "up" | "down") {
                    this.search_navigate(if key.key == "up" { -1 } else { 1 }, cx);
                    this.focus_sidebar_body(window, cx);
                    cx.stop_propagation();
                    return;
                }
                if navigation && this.sidebar_tab() == SidebarTab::Settings && this.settings_key(&key.key, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                if !focus.contains_focused(window, cx) || focus.is_focused(window) && this.sidebar_tab() != SidebarTab::Settings {
                    this.focus_sidebar_body(window, cx);
                    // Replay once after focus has moved, so the current key reaches
                    // the page's tree or input rather than the rail or its header.
                    if !focus.is_focused(window) && focus.contains_focused(window, cx) {
                        window.defer_keystroke(key.clone(), cx);
                    }
                    cx.stop_propagation();
                } else if navigation && this.sidebar_tab() == SidebarTab::Notes && matches!(key.key.as_str(), "up" | "down") {
                    let previous = window.focused(cx);
                    if key.key == "up" {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                    if !focus.contains_focused(window, cx) {
                        if let Some(previous) = previous {
                            previous.focus(window, cx);
                        }
                    }
                    cx.stop_propagation();
                }
            });
        })
    });
}

/// The desktop rail: back to the library, the panels, then settings, reading
/// aloud and the progress ring at the foot.
pub(crate) fn reader_rail<C: SidebarTarget>(
    owner: &C, target: &Entity<C>, active: Option<SidebarTab>, progress_fraction: f32, progress_label: String, progress_detail: String, theme: components::BrowserTheme, window: &mut Window, cx: &mut Context<C>,
) -> gpui::Div {
    if !components::uses_mobile_reader_controls(window) {
        install_sidebar_keyboard(owner, target, window, cx);
    }
    let item = |tab: SidebarTab| {
        let index = tab as usize;
        let target = target.clone();
        let mark = if Some(tab) == active { ReaderRailMark::Selected } else { ReaderRailMark::None };
        let destination = panel_destination(tab);
        components::reader_rail_item(("reader-rail", index), destination, destination.label(), mark, theme, cx)
            .debug_selector(move || format!("reader-rail-{index}"))
            .on_click(move |_, window, cx| target.update(cx, |this, cx| this.select_sidebar_tab(tab, window, cx)))
    };
    let rail_focus = owner.rail_focus();
    let library_target = target.clone();
    let mut rail = components::reader_rail(theme)
        .debug_selector(|| "reader-rail".into())
        .when(active.is_some(), |rail| rail.key_context("ReaderSidebar"))
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .track_focus(&rail_focus)
        .child(components::reader_rail_item("reader-rail-library", ReaderDestination::Library, "Return to library", ReaderRailMark::None, theme, cx).on_click(move |_, window, cx| {
            library_target.update(cx, |this, cx| this.close_reader(window, cx));
        }))
        .child(div().flex_none().h(px(components::SPACE_SM)));
    for &tab in owner.available_tabs() {
        // Settings sits apart at the rail's foot.
        if tab == SidebarTab::Settings {
            rail = rail.child(components::reader_rail_spacer());
        }
        rail = rail.child(item(tab));
    }
    if let Some(mark) = owner.tts_mark() {
        let tts_target = target.clone();
        rail = rail.child(components::reader_rail_item("reader-rail-tts", ReaderDestination::Tts, "Read aloud", mark, theme, cx).on_click(move |_, window, cx| tts_target.update(cx, |this, cx| this.press_tts(window, cx))));
    }
    let percent = progress_label.trim_end_matches('%').to_owned();
    rail.child(components::reader_progress_ring(progress_fraction, percent, progress_detail, theme))
}

/// The phone's bar above the page: the book with its chapter, reading progress
/// and search, plus a library button where native Back is unavailable.
pub(crate) fn reader_top_bar<C: SidebarTarget>(target: &Entity<C>, title: String, chapter: String, progress_label: String, show_search: bool, theme: components::BrowserTheme, cx: &gpui::App) -> AnyElement {
    let library_target = target.clone();
    let search_target = target.clone();
    components::ruled_topbar(theme)
        .when(cfg!(feature = "kobo") || !components::has_native_back_button(), |bar| {
            bar.child(components::outlined_icon_button("reader-top-library", ReaderDestination::Library.tooltip(ReaderDestination::Library.label(), cx), ReaderDestination::Library.icon()).on_click(move |_, window, cx| {
                library_target.update(cx, |this, cx| this.close_reader(window, cx));
            }))
        })
        .child(components::reader_top_bar_progress(progress_label, theme))
        .child(components::reader_top_bar_title(title, chapter, theme))
        .when(show_search, |bar| {
            bar.child(components::outlined_icon_button("reader-top-search", ReaderDestination::Search.tooltip(ReaderDestination::Search.label(), cx), ReaderDestination::Search.icon()).on_click(move |_, window, cx| {
                search_target.update(cx, |this, cx| this.toggle_sidebar_tab(SidebarTab::Search, window, cx));
            }))
        })
        .into_any_element()
}

/// The phone's bar below the page: Contents, Notes, Settings and reading
/// aloud. Tapping the open sheet's action closes it.
pub(crate) fn reader_bottom_bar<C: SidebarTarget>(id: &'static str, owner: &C, target: &Entity<C>, active: Option<SidebarTab>, theme: components::BrowserTheme, cx: &gpui::App) -> AnyElement {
    let mut bar = components::bottom_navigation(theme).id(id);
    // Search is in the top bar.
    for &tab in owner.available_tabs().iter().filter(|tab| **tab != SidebarTab::Search) {
        let target = target.clone();
        let mark = if active == Some(tab) { ReaderRailMark::Selected } else { ReaderRailMark::None };
        bar = bar.child(components::reader_bottom_bar_item((id, tab as usize), panel_destination(tab), mark, theme, cx).on_click(move |_, window, cx| {
            target.update(cx, |reader, cx| reader.toggle_sidebar_tab(tab, window, cx));
        }));
    }
    if let Some(mark) = owner.tts_mark() {
        let target = target.clone();
        bar = bar.child(components::reader_bottom_bar_item((id, 4usize), ReaderDestination::Tts, mark, theme, cx).on_click(move |_, window, cx| target.update(cx, |reader, cx| reader.press_tts(window, cx))));
    }
    bar.into_any_element()
}

#[cfg(all(test, not(feature = "mobile")))]
mod tests {
    use gpui::{App, Focusable, Render, ScrollDelta, ScrollWheelEvent, TestAppContext, VisualTestContext, div, point, px};
    use gpui_component::input::InputState;
    use gpui_component::tree::{TreeItem, TreeState};

    use super::*;
    use crate::shell::settings_navigation::{SettingsNavigation, SettingsRow};
    use crate::shell::state_panel;
    use crate::shell::{
        chrome::ChromeState,
        input::{CommandTarget, ReaderSidebarCommands},
        state_panel::SidebarState,
    };

    struct SettingsTest {
        tab: SidebarTab,
        tabs: &'static [SidebarTab],
        sidebar: SidebarState,
        chrome: ChromeState,
        sidebar_focus: FocusHandle,
        rail_focus: FocusHandle,
        document_focus: FocusHandle,
        navigation: SettingsNavigation,
        font_size: i32,
        theme_changes: i32,
        document_keys: usize,
        document_scrolls: usize,
        search_input: Entity<InputState>,
        search_open: bool,
        search_moves: Vec<i8>,
        sidebar_hidden: bool,
        toc_tree: Entity<TreeState>,
        navigated: Vec<String>,
    }

    impl Focusable for SettingsTest {
        fn focus_handle(&self, _: &App) -> FocusHandle {
            self.document_focus.clone()
        }
    }

    impl ReaderSidebarCommands for SettingsTest {
        fn sidebar_controls(&mut self) -> (&mut SidebarState, &mut ChromeState) {
            (&mut self.sidebar, &mut self.chrome)
        }
    }

    impl super::super::input::CommandTarget<crate::FocusReaderSearch> for SettingsTest {
        fn execute(&mut self, _: &crate::FocusReaderSearch, window: &mut Window, cx: &mut Context<Self>) {
            self.select_sidebar_tab(SidebarTab::Search, window, cx);
        }
    }

    impl super::super::input::CommandTarget<crate::ToggleToc> for SettingsTest {
        fn execute(&mut self, _: &crate::ToggleToc, window: &mut Window, cx: &mut Context<Self>) {
            self.select_sidebar_tab(SidebarTab::Contents, window, cx);
        }
    }

    impl SidebarTarget for SettingsTest {
        fn available_tabs(&self) -> &'static [SidebarTab] {
            self.tabs
        }
        fn close_reader(&mut self, _: &mut Window, _: &mut Context<Self>) {}
        fn hide_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            self.sidebar_hidden = true;
            self.hide_sidebar_controls(window, cx);
        }
        fn sidebar_focus(&self) -> FocusHandle {
            self.sidebar_focus.clone()
        }
        fn rail_focus(&self) -> FocusHandle {
            self.rail_focus.clone()
        }
        fn keyboard_visibility(&self) -> Rc<Cell<bool>> {
            self.navigation.keyboard_visibility()
        }
        fn sidebar_tab(&self) -> SidebarTab {
            self.tab
        }
        fn contents_available(&self) -> bool {
            true
        }
        fn select_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
            self.sidebar_hidden = false;
            self.tab = tab;
            self.sidebar.show(tab);
            self.chrome.show(cx);
            if tab == SidebarTab::Search {
                self.search_open = true;
                self.search_input.read(cx).focus_handle(cx).focus(window, cx);
            }
            self.focus_sidebar_body(window, cx);
            cx.notify();
        }
        fn toggle_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
            self.select_sidebar_tab(tab, window, cx);
        }
        fn focus_sidebar_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            match self.tab {
                SidebarTab::Contents => self.toc_tree.update(cx, |tree, cx| {
                    if tree.selected_index().is_none() {
                        tree.set_selected_index(Some(0), cx);
                    }
                    tree.focus(window, cx);
                }),
                SidebarTab::Search => self.search_input.read(cx).focus_handle(cx).focus(window, cx),
                SidebarTab::Settings => self.sidebar_focus.focus(window, cx),
                SidebarTab::Notes => {
                    self.sidebar_focus.focus(window, cx);
                    window.focus_next(cx);
                    if !self.sidebar_focus.contains_focused(window, cx) {
                        self.sidebar_focus.focus(window, cx);
                    }
                }
            }
        }
        fn settings_key(&mut self, key: &str, _: &mut Window, cx: &mut Context<Self>) -> bool {
            let rows = [SettingsRow::Theme, SettingsRow::FontSize];
            let direction = if matches!(key, "up" | "left") { -1 } else { 1 };
            if matches!(key, "up" | "down") {
                self.navigation.move_row(&rows, direction);
            } else if self.navigation.selected(&rows) == SettingsRow::FontSize {
                self.font_size += direction;
            } else {
                self.theme_changes += direction;
            }
            cx.notify();
            true
        }
    }

    impl super::super::search::SearchBarTarget for SettingsTest {
        fn search_navigate(&mut self, direction: i8, _: &mut Context<Self>) {
            self.search_moves.push(direction);
        }
    }

    impl Render for SettingsTest {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            super::super::input::install_reader_shortcuts(window, cx);
            let theme = components::browser_theme(cx);
            if components::uses_mobile_reader_controls(window) {
                self.navigation.keyboard_visibility().set(false);
            }
            let rows = [SettingsRow::Theme, SettingsRow::FontSize];
            let body = if self.tab == SidebarTab::Settings {
                components::reader_settings_tab_body(theme)
                    .child(components::reader_settings_group(theme).child(self.navigation.row(
                        SettingsRow::Theme,
                        &rows,
                        components::reader_setting_row_smallcaps("Theme", theme).debug_selector(|| "test-theme-row".into()).h(px(60.0)),
                        theme,
                    )))
                    .child(components::reader_settings_group(theme).child(self.navigation.row(
                        SettingsRow::FontSize,
                        &rows,
                        components::reader_setting_row_smallcaps("Font size", theme).debug_selector(|| "test-font-row".into()).child(components::reader_choice_button_group(
                            "test-font-group",
                            [
                                components::settings_choice_button("test-font-button", "Increase font", false, theme).debug_selector(|| "test-font-button".into()).on_click(cx.listener(|this, _, _, cx| {
                                    this.font_size += 1;
                                    cx.notify();
                                })),
                                components::settings_choice_button("test-font-other", "Other", false, theme).debug_selector(|| "test-font-other".into()),
                            ],
                            theme,
                        )),
                        theme,
                    )))
                    .child(div().flex_none().h(px(1200.0)))
                    .into_any_element()
            } else if self.tab == SidebarTab::Contents {
                state_panel::reader_toc_tree(&self.toc_tree, &cx.entity(), None, Default::default(), None, self.navigation.keyboard_visible(), theme, cx, |id, activation, window, this: &mut Self, cx| {
                    this.navigated.push(id.to_owned());
                    if activation == state_panel::TocActivation::Pointer {
                        this.document_focus.focus(window, cx);
                    }
                })
            } else if self.tab == SidebarTab::Search {
                let rows = (0usize..40)
                    .map(|index| {
                        search::result_row(("test-search-hit", index), "An excerpt from a matching passage.".into(), "matching", Default::default(), false, theme).debug_selector(move || format!("test-search-hit-{index}")).into_any_element()
                    })
                    .collect();
                search::panel(search::search_bar(&self.search_input, theme, window, cx), rows, theme)
            } else {
                div().children((0usize..3).map(|index| div().id(("test-note", index)).focusable().tab_stop(true).debug_selector(move || format!("test-note-{index}")).h(px(40.0)).child("A note"))).into_any_element()
            };
            let content = SidebarContent::new(body);
            let sidebar = reader_sidebar(self, &cx.entity(), content, theme, window, cx);
            let panel_visible = !self.sidebar_hidden && self.chrome.is_visible();
            let rail = reader_rail(self, &cx.entity(), panel_visible.then_some(self.tab), 0.0, "0%".into(), "Progress".into(), theme, window, cx);
            let document = div().size_full().debug_selector(|| "test-document".into()).track_focus(&self.document_focus).on_key_down(cx.listener(|this, _, _, _| this.document_keys += 1)).into_any_element();
            let content = super::super::chrome::desktop_content(rail.into_any_element(), panel_visible.then_some(sidebar), document, window, cx);
            div().size_full().key_context("Reader").on_scroll_wheel(cx.listener(|this, _, _, _| this.document_scrolls += 1)).child(content)
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<SettingsTest>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| cx.set_global(crate::settings::ReaderSettings { preferences: Default::default(), save: Rc::new(|_, _, _| {}) }));
        cx.update(super::super::input::bind_reader_keys);
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let document_focus = cx.focus_handle();
                document_focus.focus(window, cx);
                SettingsTest {
                    tab: SidebarTab::Contents,
                    tabs: sidebar_tabs(true),
                    sidebar: SidebarState::from_preferences(cx),
                    chrome: ChromeState::from_preferences(crate::invalidation::Component::PdfShell, cx),
                    sidebar_focus: cx.focus_handle(),
                    rail_focus: cx.focus_handle(),
                    document_focus,
                    navigation: Default::default(),
                    font_size: 20,
                    theme_changes: 0,
                    document_keys: 0,
                    document_scrolls: 0,
                    search_input: cx.new(|cx| InputState::new(window, cx)),
                    search_open: false,
                    search_moves: Vec::new(),
                    sidebar_hidden: false,
                    toc_tree: cx.new(|cx| TreeState::new(cx).items(vec![TreeItem::new("one", "Chapter one"), TreeItem::new("two", "Chapter two")])),
                    navigated: Vec::new(),
                }
            });
            gpui_component::Root::new(view, window, cx)
        });
        let view = cx.update(|_, cx| root.read(cx).view().clone().downcast::<SettingsTest>().ok().unwrap());
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui::test]
    fn shared_commands_cycle_only_supported_tabs_and_restore_focus_when_toggling(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.tabs = &[SidebarTab::Contents, SidebarTab::Settings];
                view.chrome.show(cx);
                view.select_sidebar_tab(SidebarTab::Contents, window, cx);
                view.execute(&crate::NextSidebarTab, window, cx);
                assert_eq!(view.tab, SidebarTab::Settings);
                view.execute(&crate::PreviousSidebarTab, window, cx);
                assert_eq!(view.tab, SidebarTab::Contents);
                view.execute(&crate::PreviousSidebarTab, window, cx);
                assert_eq!(view.tab, SidebarTab::Settings);

                view.chrome.hide(cx);
                view.execute(&crate::NextSidebarTab, window, cx);
                assert_eq!(view.tab, SidebarTab::Settings, "first press reveals the current panel");
                assert!(view.chrome.is_visible());
                view.execute(&crate::ToggleSidebar, window, cx);
                assert!(!view.chrome.is_visible());
                assert!(view.document_focus.is_focused(window));
                view.execute(&crate::ToggleSidebar, window, cx);
                assert!(view.chrome.is_visible() && view.sidebar.is_visible());
                assert!(view.sidebar_focus.is_focused(window));
            })
        });
    }

    #[gpui::test]
    fn remapped_panel_shortcuts_work_from_inputs_and_restore_defaults(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|_, cx| {
            components::apply_keyboard_shortcuts(
                &app_preferences::KeyboardShortcuts::from([("reader.search".into(), vec!["alt-f".into()]), ("reader.contents".into(), vec!["alt-t".into()]), ("reader.next_tab".into(), vec!["alt-n".into()])]),
                cx,
            );
        });
        cx.simulate_keystrokes("secondary-f");
        view.read_with(cx, |view, _| assert_eq!(view.tab, SidebarTab::Contents));
        cx.simulate_keystrokes("alt-f");
        cx.run_until_parked();
        cx.update(|window, cx| {
            let view = view.read(cx);
            assert_eq!(view.tab, SidebarTab::Search);
            assert!(view.search_input.read(cx).focus_handle(cx).is_focused(window));
        });
        cx.simulate_input("saved query");
        cx.simulate_keystrokes("alt-t");
        view.read_with(cx, |view, _| assert_eq!(view.tab, SidebarTab::Contents));
        cx.simulate_keystrokes("alt-n");
        view.read_with(cx, |view, _| assert_eq!(view.tab, SidebarTab::Notes));
        cx.update(|_, cx| components::apply_keyboard_shortcuts(&Default::default(), cx));
        cx.simulate_keystrokes("secondary-f");
        cx.run_until_parked();
        cx.update(|window, cx| {
            let view = view.read(cx);
            assert_eq!(view.tab, SidebarTab::Search);
            assert!(view.search_input.read(cx).focus_handle(cx).is_focused(window));
            assert_eq!(view.search_input.read(cx).value().as_str(), "saved query");
        });
    }

    #[gpui::test]
    fn find_opens_and_focuses_search_from_document_sidebar_and_input(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.simulate_keystrokes("secondary-f");
        cx.run_until_parked();
        cx.update(|window, cx| {
            let view = view.read(cx);
            assert_eq!(view.tab, SidebarTab::Search);
            assert!(view.search_input.read(cx).focus_handle(cx).is_focused(window));
        });
        cx.simulate_input("remember this query");
        cx.simulate_keystrokes("secondary-f");
        view.read_with(cx, |view, cx| assert_eq!(view.search_input.read(cx).value().as_str(), "remember this query"));

        cx.simulate_keystrokes("escape secondary-f");
        cx.run_until_parked();
        cx.update(|window, cx| {
            let view = view.read(cx);
            assert!(!view.sidebar_hidden);
            assert!(view.search_input.read(cx).focus_handle(cx).is_focused(window));
            assert_eq!(view.search_input.read(cx).value().as_str(), "remember this query");
        });

        let settings = cx.debug_bounds("reader-rail-3").unwrap();
        cx.simulate_click(settings.center(), Default::default());
        cx.simulate_keystrokes("secondary-f");
        cx.run_until_parked();
        cx.update(|window, cx| {
            let view = view.read(cx);
            assert_eq!(view.tab, SidebarTab::Search);
            assert!(view.search_input.read(cx).focus_handle(cx).is_focused(window));
            assert_eq!(view.theme_changes, 0);
            assert_eq!(view.document_keys, 0);
        });
    }

    #[gpui::test]
    fn contents_shortcut_opens_and_focuses_contents_from_search_and_hidden_sidebar(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.simulate_keystrokes("secondary-f");
        cx.simulate_input("remember this query");
        for keys in ["secondary-t", "escape secondary-t", "secondary-t"] {
            cx.simulate_keystrokes(keys);
            cx.run_until_parked();
            cx.update(|window, cx| {
                let view = view.read(cx);
                assert!(!view.sidebar_hidden);
                assert_eq!(view.tab, SidebarTab::Contents);
                assert!(view.sidebar_focus.contains_focused(window, cx));
                assert!(!view.sidebar_focus.is_focused(window));
                assert_eq!(view.toc_tree.read(cx).selected_index(), Some(0));
                assert_eq!(view.search_input.read(cx).value().as_str(), "remember this query");
            });
        }
    }

    #[gpui::test]
    fn search_autofocuses_when_mounted_without_stealing_focus_on_updates(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        for _ in 0..2 {
            view.update(cx, |view, cx| {
                view.tab = SidebarTab::Search;
                cx.notify();
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                assert!(view.read(cx).search_input.read(cx).focus_handle(cx).is_focused(window));
                let document_focus = view.read(cx).document_focus.clone();
                document_focus.focus(window, cx);
                view.update(cx, |_, cx| cx.notify());
            });
            cx.run_until_parked();
            cx.update(|window, cx| assert!(view.read(cx).document_focus.is_focused(window)));
            view.update(cx, |view, cx| {
                view.tab = SidebarTab::Contents;
                cx.notify();
            });
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn settings_clicks_and_arrows_keep_focus_and_edit_the_selected_row(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        let settings = cx.debug_bounds("reader-rail-3").unwrap();
        cx.simulate_click(settings.center(), Default::default());
        cx.simulate_keystrokes("down down right");
        view.update(cx, |view, _| {
            assert_eq!(view.tab, SidebarTab::Settings);
            assert_eq!(view.font_size, 21);
            assert_eq!(view.theme_changes, 0);
            assert_eq!(view.document_keys, 0);
        });
        cx.run_until_parked();
        let theme = cx.debug_bounds("test-theme-row").unwrap();
        cx.simulate_click(theme.center(), Default::default());
        cx.simulate_keystrokes("right");
        view.update(cx, |view, _| assert_eq!(view.theme_changes, 1));

        let button = cx.debug_bounds("test-font-button").unwrap();
        cx.simulate_click(button.center(), Default::default());
        cx.simulate_keystrokes("right");
        view.update(cx, |view, _| {
            assert_eq!(view.font_size, 23);
            assert_eq!(view.theme_changes, 1);
            assert_eq!(view.document_keys, 0);
        });
    }

    #[gpui::test]
    fn rail_arrows_navigate_the_active_page_and_tab_cycles_pages(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| view.read(cx).rail_focus.clone().focus(window, cx));
        cx.simulate_keystrokes("down");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tab, SidebarTab::Contents);
            assert_eq!(view.toc_tree.read(cx).selected_index(), Some(0));
        });
        cx.simulate_keystrokes("down");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tab, SidebarTab::Contents);
            assert_eq!(view.toc_tree.read(cx).selected_index(), Some(1));
        });
        cx.simulate_keystrokes("ctrl-tab ctrl-tab ctrl-tab");
        view.read_with(cx, |view, _| {
            assert_eq!(view.tab, SidebarTab::Settings);
            assert_eq!(view.theme_changes, 0);
            assert_eq!(view.font_size, 20);
            assert_eq!(view.document_keys, 0);
        });
        cx.simulate_keystrokes("down right");
        view.read_with(cx, |view, _| assert_eq!(view.font_size, 21));
        cx.simulate_keystrokes("ctrl-shift-tab");
        view.read_with(cx, |view, _| assert_eq!(view.tab, SidebarTab::Search));
        // The nested input must not indent, move focus, or consume tab shortcuts.
        cx.simulate_keystrokes("ctrl-tab ctrl-shift-tab ctrl-tab ctrl-shift-tab");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.tab, SidebarTab::Search);
            assert_eq!(view.search_input.read(cx).value().as_str(), "");
            assert_eq!(view.document_keys, 0);
        });
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.sidebar_hidden));
    }

    #[gpui::test]
    fn keyboard_marker_appears_on_first_key_without_moving_or_editing(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        let settings = cx.debug_bounds("reader-rail-3").unwrap();
        cx.simulate_click(settings.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-Theme").is_none());
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-Theme").is_some());
        view.read_with(cx, |view, _| {
            assert_eq!(view.navigation.selected(&[SettingsRow::Theme, SettingsRow::FontSize]), SettingsRow::Theme);
            assert_eq!(view.theme_changes, 0);
        });
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-FontSize").is_some());
        let font = cx.debug_bounds("test-font-row").unwrap();
        cx.simulate_click(font.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-FontSize").is_some());
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-Theme").is_some());
    }

    #[gpui::test]
    fn settings_row_and_keyboard_highlight_survive_pointer_page_switches(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        let settings = cx.debug_bounds("reader-rail-3").unwrap();
        cx.simulate_click(settings.center(), Default::default());
        cx.run_until_parked();
        let font = cx.debug_bounds("test-font-row").unwrap();
        cx.simulate_click(font.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-FontSize").is_none());
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-FontSize").is_some());

        let notes = cx.debug_bounds("reader-rail-1").unwrap();
        cx.simulate_click(notes.center(), Default::default());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.tab, SidebarTab::Notes);
            assert_eq!(view.navigation.selected(&[SettingsRow::Theme, SettingsRow::FontSize]), SettingsRow::FontSize);
            assert!(view.navigation.keyboard_visible());
        });
        let settings = cx.debug_bounds("reader-rail-3").unwrap();
        cx.simulate_click(settings.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-FontSize").is_some());
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-Theme").is_some());
    }

    #[gpui::test]
    fn notes_arrows_stay_in_the_page_at_both_ends(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| view.update(cx, |view, cx| view.select_sidebar_tab(SidebarTab::Notes, window, cx)));
        cx.simulate_keystrokes("down");
        let first = cx.update(|window, cx| window.focused(cx).unwrap());
        cx.simulate_keystrokes("down");
        let second = cx.update(|window, cx| window.focused(cx).unwrap());
        assert_ne!(first, second);
        cx.simulate_keystrokes("down down down down up up up up");
        cx.update(|window, cx| {
            let view = view.read(cx);
            assert_eq!(view.tab, SidebarTab::Notes);
            assert!(view.sidebar_focus.contains_focused(window, cx));
            assert_eq!(view.document_keys, 0);
        });
        cx.simulate_keystrokes("ctrl-tab");
        view.read_with(cx, |view, _| assert_eq!(view.tab, SidebarTab::Search));
    }

    #[gpui::test]
    fn search_arrows_navigate_matches_after_revealing_keyboard_mode(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| view.update(cx, |view, cx| view.select_sidebar_tab(SidebarTab::Search, window, cx)));
        cx.simulate_keystrokes("down");
        view.read_with(cx, |view, _| assert!(view.search_moves.is_empty()));
        cx.simulate_keystrokes("down up down");
        view.read_with(cx, |view, _| {
            assert_eq!(view.search_moves, [1, -1, 1]);
            assert_eq!(view.tab, SidebarTab::Search);
            assert_eq!(view.document_keys, 0);
        });
    }

    #[gpui::test]
    fn mobile_layout_keeps_keyboard_markers_hidden(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| view.update(cx, |view, cx| view.select_sidebar_tab(SidebarTab::Settings, window, cx)));
        cx.simulate_keystrokes("down down");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-FontSize").is_some());
        cx.simulate_resize(gpui::size(px(390.0), px(800.0)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-FontSize").is_none());
        cx.simulate_keystrokes("up down right tab shift-tab");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-settings-marker-Theme").is_none());
        assert!(cx.debug_bounds("reader-settings-marker-FontSize").is_none());
        view.read_with(cx, |view, _| {
            assert_eq!(view.tab, SidebarTab::Settings);
            assert_eq!(view.font_size, 21);
            assert!(!view.navigation.keyboard_visible());
        });
    }

    #[gpui::test]
    fn escape_closes_the_sidebar_and_returns_to_the_document(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| view.update(cx, |view, cx| view.select_sidebar_tab(SidebarTab::Settings, window, cx)));
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.sidebar_hidden));
        cx.update(|window, cx| assert!(view.read(cx).document_focus.is_focused(window)));
    }

    #[gpui::test]
    fn closing_panel_keeps_rail_visible_and_buttons_reopen_panels(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        let rail = cx.debug_bounds("reader-rail").unwrap();
        let expanded_document = cx.debug_bounds("test-document").unwrap();
        let close = cx.debug_bounds("reader-sidebar-close").unwrap();
        cx.simulate_click(close.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-sidebar-close").is_none());
        assert_eq!(cx.debug_bounds("reader-rail").unwrap(), rail);
        assert_eq!(cx.debug_bounds("test-document").unwrap().left(), rail.right());
        assert!(cx.debug_bounds("test-document").unwrap().size.width > expanded_document.size.width);
        view.read_with(cx, |view, _| assert!(!view.chrome.is_visible()));

        // Pointer movement over the former activation strip leaves it closed.
        cx.simulate_event(gpui::MouseMoveEvent { position: point(px(1.0), px(100.0)), ..Default::default() });
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-sidebar-close").is_none());

        for selector in ["reader-rail-3", "reader-rail-1", "reader-rail-2", "reader-rail-0"] {
            let button = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(button.center(), Default::default());
            cx.run_until_parked();
            assert!(cx.debug_bounds("reader-sidebar-close").is_some(), "{selector} must reopen the panel");
            assert_eq!(cx.debug_bounds("reader-rail").unwrap(), rail);
            view.read_with(cx, |view, _| assert!(view.chrome.is_visible()));
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            assert!(cx.debug_bounds("reader-sidebar-close").is_none());
            assert!(cx.debug_bounds("reader-rail").is_some());
        }
    }

    #[gpui::test]
    fn escape_hides_search_without_clearing_the_query(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.select_sidebar_tab(SidebarTab::Search, window, cx);
                view.search_input.update(cx, |input, cx| input.set_value("remember this query", window, cx));
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, cx| {
            assert!(view.sidebar_hidden && view.search_open);
            assert_eq!(view.search_input.read(cx).value().as_str(), "remember this query");
        });
        cx.update(|window, cx| assert!(view.read(cx).document_focus.is_focused(window)));
    }

    #[gpui::test]
    fn search_and_settings_scroll_without_reaching_reader_paging_even_at_list_edges(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| view.update(cx, |view, cx| view.select_sidebar_tab(SidebarTab::Search, window, cx)));
        cx.run_until_parked();
        let first = cx.debug_bounds("test-search-hit-0").unwrap();
        let position = first.center();
        for delta in [100_000.0, -24.0] {
            cx.simulate_event(ScrollWheelEvent { position, delta: ScrollDelta::Pixels(point(px(0.0), px(delta))), ..Default::default() });
            cx.run_until_parked();
        }
        let scrolled = cx.debug_bounds("test-search-hit-0").unwrap();
        assert!(scrolled.top() < first.top(), "the results must still scroll");
        for _ in 0..2 {
            cx.simulate_event(ScrollWheelEvent { position, delta: ScrollDelta::Pixels(point(px(0.0), px(-100_000.0))), ..Default::default() });
            cx.run_until_parked();
        }
        view.read_with(cx, |view, _| assert_eq!(view.document_scrolls, 0));
        cx.simulate_event(ScrollWheelEvent { position, delta: ScrollDelta::Pixels(point(px(0.0), px(100_000.0))), ..Default::default() });
        cx.run_until_parked();
        cx.update(|window, cx| view.update(cx, |view, cx| view.select_sidebar_tab(SidebarTab::Settings, window, cx)));
        cx.run_until_parked();
        let theme_row = cx.debug_bounds("test-theme-row").unwrap();
        let settings_position = theme_row.center();
        cx.simulate_event(ScrollWheelEvent { position: settings_position, delta: ScrollDelta::Pixels(point(px(0.0), px(-24.0))), ..Default::default() });
        cx.run_until_parked();
        assert!(cx.debug_bounds("test-theme-row").unwrap().top() < theme_row.top(), "settings must still scroll");
        for delta in [-100_000.0, -100_000.0, 100_000.0, 100_000.0] {
            cx.simulate_event(ScrollWheelEvent { position: settings_position, delta: ScrollDelta::Pixels(point(px(0.0), px(delta))), ..Default::default() });
            cx.run_until_parked();
        }
        view.read_with(cx, |view, _| assert_eq!(view.document_scrolls, 0));
        let document_position = cx.update(|window, _| point(window.viewport_size().width - px(20.0), px(100.0)));
        cx.simulate_event(ScrollWheelEvent { position: document_position, delta: ScrollDelta::Pixels(point(px(0.0), px(-24.0))), ..Default::default() });
        view.read_with(cx, |view, _| assert_eq!(view.document_scrolls, 1, "reader paging remains available over the document"));
    }

    #[gpui::test]
    fn keyboard_toc_navigation_keeps_focus_until_escape(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.sidebar_focus.focus(window, cx);
                view.focus_sidebar_body(window, cx);
            })
        });
        cx.simulate_keystrokes("enter enter down enter");
        view.read_with(cx, |view, _| {
            assert_eq!(view.navigated, ["one", "two"]);
            assert_eq!(view.document_keys, 0);
            assert!(!view.sidebar_hidden);
        });
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.sidebar_hidden));
        cx.update(|window, cx| assert!(view.read(cx).document_focus.is_focused(window)));
    }
}
