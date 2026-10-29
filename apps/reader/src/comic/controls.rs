use super::{ComicReaderView, MAX_COLUMNS, Zoom};
use app_preferences::{READER_SPACING_MAX, READER_SPACING_MIN};
use crate::settings::ReaderSettings;
use crate::shell::settings_navigation::SettingsRow;

const SPACING_STEP_PX: f32 = 8.0;
use crate::shell::{
    chrome::ChromeState,
    input::{CommandTarget, ReaderSidebarCommands},
    search::SearchBarTarget,
    state_panel::{self, SidebarState, SidebarTab},
    toolbar::SidebarTarget,
};
use crate::{DecreaseScale, FocusReaderSearch, IncreaseScale, NextLine, NextPage, PreviousLine, PreviousPage, ToggleSearch, ToggleToc};
use gpui::{AnyElement, Context, FocusHandle, Window, prelude::*, px, uniform_list};
use std::{cell::Cell, rc::Rc};
use ui_components as components;

impl ComicReaderView {
    pub(super) fn adjust_spacing(&mut self, row: SettingsRow, delta: f32, cx: &mut Context<Self>) {
        if ReaderSettings::update(cx, |preferences| {
            let value = match row {
                SettingsRow::SideMargins => &mut preferences.comic_horizontal_margin_px,
                SettingsRow::VerticalMargins => &mut preferences.comic_vertical_margin_px,
                SettingsRow::ColumnGap => &mut preferences.comic_page_gap_px,
                _ => return,
            };
            *value = (*value + delta).clamp(READER_SPACING_MIN, READER_SPACING_MAX);
        }) {
            self.page_gap = ReaderSettings::preferences(cx).comic_page_gap_px;
            self.clamp_position();
            self.request_pages(cx);
            cx.notify();
        }
    }

    pub(super) fn render_panel(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        if tab == SidebarTab::Contents {
            return uniform_list(
                "comic-page-list",
                self.reader.pages.len(),
                cx.processor(|view, range: std::ops::Range<usize>, _, cx| {
                    let theme = components::browser_theme(cx);
                    range
                        .map(|index| {
                            let label = view.reader.pages[index].bookmark.as_ref().map(|name| format!("{} · {name}", index + 1)).unwrap_or_else(|| format!("Page {}", index + 1));
                            components::reader_state_card(("comic-page", index), index == view.current, theme).h(px(44.0)).flex_none().cursor_pointer().child(label).on_click(cx.listener(move |view, _, window, cx| {
                                view.go_to(index, cx);
                                if components::uses_mobile_reader_controls(window) {
                                    view.hide_sidebar(window, cx);
                                }
                            }))
                        })
                        .collect()
                }),
            )
            .size_full()
            .into_any_element();
        }
        let fit = components::reader_choice_button_group(
            "comic-fit",
            [
                components::settings_choice_button("comic-fit-page", "Whole page", self.zoom == Zoom::FitPage, theme).on_click(cx.listener(|view, _, _, cx| {
                    view.zoom = Zoom::FitPage;
                    view.offset = 0.0;
                    view.pan_x = 0.0;
                    view.clamp_position();
                    view.record_position();
                    view.request_pages(cx);
                    cx.notify();
                })),
                components::settings_choice_button("comic-fit-width", "Fit width", self.zoom == Zoom::FitWidth, theme).on_click(cx.listener(|view, _, _, cx| {
                    view.zoom = Zoom::FitWidth;
                    view.pan_x = 0.0;
                    view.clamp_position();
                    view.record_position();
                    view.request_pages(cx);
                    cx.notify();
                })),
            ],
            theme,
        );
        let columns = components::reader_setting_row("Pages").child(components::settings_stepper(
            components::settings_stepper_button("comic-pages-decrease", "−", self.columns == 1).debug_selector(|| "comic-pages-decrease".into()).on_click(cx.listener(|view, _, _, cx| view.adjust_columns(-1, cx))),
            self.columns.to_string(),
            components::settings_stepper_button("comic-pages-increase", "+", self.columns == MAX_COLUMNS).debug_selector(|| "comic-pages-increase".into()).on_click(cx.listener(|view, _, _, cx| view.adjust_columns(1, cx))),
            theme,
        ));
        let direction = components::reader_choice_button_group(
            "comic-direction",
            [
                components::settings_choice_button("comic-ltr", "Left to right", !self.right_to_left, theme).on_click(cx.listener(|view, _, _, cx| {
                    view.right_to_left = false;
                    cx.notify();
                })),
                components::settings_choice_button("comic-rtl", "Right to left", self.right_to_left, theme).on_click(cx.listener(|view, _, _, cx| {
                    view.right_to_left = true;
                    cx.notify();
                })),
            ],
            theme,
        );
        let zoom = components::reader_choice_button_group(
            "comic-zoom",
            [
                components::settings_choice_button("comic-zoom-out", "−", false, theme).on_click(cx.listener(|view, _, _, cx| view.adjust_zoom(1.0 / 1.2, cx))),
                components::settings_choice_button("comic-zoom-in", "+", false, theme).on_click(cx.listener(|view, _, _, cx| view.adjust_zoom(1.2, cx))),
            ],
            theme,
        );
        let preferences = ReaderSettings::preferences(cx);
        let spacing = [
            (SettingsRow::SideMargins, "Side margins", preferences.comic_horizontal_margin_px),
            (SettingsRow::VerticalMargins, "Vertical margins", preferences.comic_vertical_margin_px),
            (SettingsRow::ColumnGap, "Page gap", preferences.comic_page_gap_px),
        ];
        let spacing = spacing.into_iter().enumerate().map(|(index, (row, label, value))| {
            components::reader_setting_row(label).child(components::settings_stepper(
                components::settings_stepper_button(("comic-spacing-decrease", index), "−", value <= READER_SPACING_MIN)
                    .debug_selector(move || format!("comic-spacing-decrease-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.adjust_spacing(row, -SPACING_STEP_PX, cx))),
                format!("{value:.0}px"),
                components::settings_stepper_button(("comic-spacing-increase", index), "+", value >= READER_SPACING_MAX)
                    .debug_selector(move || format!("comic-spacing-increase-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.adjust_spacing(row, SPACING_STEP_PX, cx))),
                theme,
            ))
        });
        let fullscreen = if window.is_fullscreen() { "Exit full screen" } else { "Enter full screen" };
        components::reader_settings_tab_body(theme)
            .child(components::reader_settings_section("Page display", theme).child(fit))
            .child(components::reader_settings_group(theme).child(columns))
            .child(components::reader_settings_group(theme).children(spacing))
            .child(components::reader_settings_section("Reading direction", theme).child(direction))
            .child(components::reader_settings_section("Zoom", theme).child(zoom))
            .when(super::super::shell::settings_navigation::DESKTOP_FULLSCREEN, |panel| {
                panel.child(components::settings_choice_button("comic-fullscreen", fullscreen, window.is_fullscreen(), theme).on_click(|_, window, _| window.toggle_fullscreen()))
            })
            .into_any_element()
    }
}

impl ReaderSidebarCommands for ComicReaderView {
    fn sidebar_controls(&mut self) -> (&mut SidebarState, &mut ChromeState) {
        (&mut self.sidebar, &mut self.chrome)
    }
}

impl SearchBarTarget for ComicReaderView {
    fn search_navigate(&mut self, _: i8, _: &mut Context<Self>) {}
}

impl SidebarTarget for ComicReaderView {
    #[cfg(feature = "kobo")]
    fn device_controls(&self) -> Option<gpui::AnyView> {
        self.device_controls.clone()
    }
    fn available_tabs(&self) -> &'static [SidebarTab] {
        &[SidebarTab::Contents, SidebarTab::Settings]
    }
    fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record_position();
        (self.close_reader)(window, cx);
    }
    fn hide_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_sidebar_controls(window, cx);
    }
    fn sidebar_focus(&self) -> FocusHandle {
        self.sidebar_focus.clone()
    }
    fn rail_focus(&self) -> FocusHandle {
        self.rail_focus.clone()
    }
    fn keyboard_visibility(&self) -> Rc<Cell<bool>> {
        self.settings_navigation.keyboard_visibility()
    }
    fn sidebar_tab(&self) -> SidebarTab {
        match self.sidebar.effective_or_default(true) {
            SidebarTab::Settings => SidebarTab::Settings,
            _ => SidebarTab::Contents,
        }
    }
    fn contents_available(&self) -> bool {
        true
    }
    fn select_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.available_tabs().contains(&tab) {
            return;
        }
        state_panel::select_tab(&mut self.sidebar, tab, cx);
        self.chrome.show(cx);
        self.sidebar_focus.focus(window, cx);
        cx.notify();
    }
    fn toggle_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.available_tabs().contains(&tab) {
            return;
        }
        state_panel::toggle_tab(&mut self.sidebar, tab, true, cx);
        self.chrome.show(cx);
        if self.sidebar.is_visible() {
            self.sidebar_focus.focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }
    fn focus_sidebar_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_focus.focus(window, cx);
    }
    fn settings_key(&mut self, _: &str, _: &mut Window, _: &mut Context<Self>) -> bool {
        false
    }
}

macro_rules! command {
    ($action:ty, $view:ident, $window:ident, $cx:ident, $body:block) => {
        impl CommandTarget<$action> for ComicReaderView {
            fn execute(&mut self, _: &$action, $window: &mut Window, $cx: &mut Context<Self>) {
                let $view = self;
                $body
            }
        }
    };
}

command!(PreviousPage, view, window, cx, {
    view.turn(-1, cx);
    view.chrome.hide_for_motion(window, cx);
});
command!(NextPage, view, window, cx, {
    view.turn(1, cx);
    view.chrome.hide_for_motion(window, cx);
});
command!(PreviousLine, view, _window, cx, {
    view.scroll(0.0, 32.0, cx);
});
command!(NextLine, view, _window, cx, {
    view.scroll(0.0, -32.0, cx);
});
command!(IncreaseScale, view, _window, cx, {
    view.adjust_zoom(1.2, cx);
});
command!(DecreaseScale, view, _window, cx, {
    view.adjust_zoom(1.0 / 1.2, cx);
});
command!(ToggleToc, view, window, cx, {
    view.toggle_sidebar_tab(SidebarTab::Contents, window, cx);
});
command!(FocusReaderSearch, _view, _window, _cx, {});
command!(ToggleSearch, _view, _window, _cx, {});
