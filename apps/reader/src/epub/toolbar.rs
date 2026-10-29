//! EPUB-specific appearance controls in the shared sidebar.

use gpui::prelude::*;
use std::{cell::Cell, rc::Rc};

use gpui::{AnyElement, App, Context, Focusable, Window, div, font, px, rems};
use html_view_gpui::ReadingRulerStyle;
use ui_components as components;

use crate::epub::ReaderView;
use crate::invalidation::{Component, notify};
use crate::settings::{CjkFontChoice, ReaderFontRole, ReaderSettings, SidebarTab};
use crate::shell::reading_ruler::{self, ReadingRulerSettings};
use crate::shell::settings_navigation::{self, SettingsRow, adjacent_step, adjacent_value};
use crate::shell::state_panel;
use crate::shell::toolbar::SidebarTarget;
use crate::{ReaderFontFamily, ReaderTextAlignment};
use app_preferences::{ReaderFirstLineIndent, PARAGRAPH_SPACING_MAX_EM, FIRST_LINE_INDENT_MAX_EM, PARAGRAPH_STEP_EM, ReaderFontWeight, ReaderHyphenation, LINE_HEIGHT_MAX, LINE_HEIGHT_MIN, READER_SPACING_MAX, READER_SPACING_MIN, WORD_SPACING_MAX_PERCENT, WORD_SPACING_MIN_PERCENT, WORD_SPACING_STEP_PERCENT};

impl SidebarTarget for ReaderView {
    #[cfg(feature = "kobo")]
    fn device_controls(&self) -> Option<gpui::AnyView> {
        self.device_controls.clone()
    }

    fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
        self.tts.close();
        (self.close_reader)(window, cx);
    }

    fn hide_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if components::uses_mobile_reader_controls(window) {
            if self.sidebar.close(cx) {
                notify(cx, Component::ReaderShell, "sidebar_closed");
            }
        } else {
            self.chrome.hide(cx);
        }
        self.focus_document(window, cx);
    }

    fn sidebar_focus(&self) -> gpui::FocusHandle {
        self.sidebar_focus.clone()
    }

    fn rail_focus(&self) -> gpui::FocusHandle {
        self.rail_focus.clone()
    }

    fn keyboard_visibility(&self) -> Rc<Cell<bool>> {
        self.settings_navigation.keyboard_visibility()
    }

    fn sidebar_tab(&self) -> SidebarTab {
        self.sidebar.effective_or_default(self.contents_available())
    }

    fn contents_available(&self) -> bool {
        true
    }

    fn select_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        state_panel::select_tab(&mut self.sidebar, tab, cx);
        self.chrome.show(cx);
        if tab == SidebarTab::Search {
            self.open_search(cx);
            self.search_input.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.focus_sidebar_body(window, cx);
        cx.notify();
    }

    fn toggle_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        state_panel::toggle_tab(&mut self.sidebar, tab, true, cx);
        self.chrome.show(cx);
        if self.sidebar.is_visible() && tab == SidebarTab::Search {
            self.open_search(cx);
            self.search_input.read(cx).focus_handle(cx).focus(window, cx);
        } else if self.sidebar.is_visible() {
            self.focus_sidebar_body(window, cx);
        } else {
            self.focus_document(window, cx);
        }
        notify(cx, Component::ReaderShell, "mobile_sidebar_tab_changed");
    }

    fn focus_sidebar_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_tab() == SidebarTab::Contents {
            self.toc_tree.update(cx, |tree, cx| {
                if tree.selected_index().is_none() {
                    tree.set_selected_index(Some(0), cx);
                }
                tree.focus(window, cx);
            });
        } else if self.sidebar_tab() == SidebarTab::Search {
            self.search_input.read(cx).focus_handle(cx).focus(window, cx);
        } else {
            self.sidebar_focus.focus(window, cx);
            if self.sidebar_tab() == SidebarTab::Notes {
                window.focus_next(cx);
                if !self.sidebar_focus.contains_focused(window, cx) {
                    self.sidebar_focus.focus(window, cx);
                }
            }
        }
    }

    fn settings_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.handle_settings_key(key, window, cx)
    }

    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    fn tts_mark(&self) -> Option<components::ReaderRailMark> {
        self.tts.available().then(|| self.tts.button_mark())
    }

    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    fn press_tts(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        crate::shell::tts::TtsReader::press_tts(self, cx);
    }
}

/// The values a settings tile stores, its name, and its picture. The middle
/// tile of every row is the preference's default.
type Steps<T> = [(T, &'static str, &'static str); 3];

struct TileRow {
    id: &'static str,
    row: SettingsRow,
    label: &'static str,
}

const TYPEFACES: [(ReaderFontFamily, &str, Option<&str>); 3] = [(ReaderFontFamily::Publisher, "Book", None), (ReaderFontFamily::Serif, "Serif", Some("NV Garamond")), (ReaderFontFamily::SansSerif, "Sans serif", Some("Raleway"))];
const SYSTEM_FONT_PICKER: bool = !cfg!(any(target_arch = "wasm32", feature = "kobo"));
const SPACING_STEP_PX: f32 = 8.0;
const ALIGNMENTS: Steps<ReaderTextAlignment> = [
    (ReaderTextAlignment::Publisher, "Book", "icons/reader-settings/align-book.svg"),
    (ReaderTextAlignment::Start, "Left", "icons/reader-settings/align-left.svg"),
    (ReaderTextAlignment::Justify, "Justify", "icons/reader-settings/align-justify.svg"),
];
/// Minimum and maximum line length in characters.
const LINE_LENGTHS: Steps<(f64, f64)> =
    [((35.0, 50.0), "Short", "icons/reader-settings/line-length-short.svg"), ((45.0, 65.0), "Medium", "icons/reader-settings/line-length-medium.svg"), ((60.0, 85.0), "Long", "icons/reader-settings/line-length-long.svg")];

fn step_values<T: Copy>(steps: &Steps<T>) -> [T; 3] {
    steps.map(|(value, _, _)| value)
}

impl ReaderView {
    fn settings_rows(&self, cx: &App) -> Vec<SettingsRow> {
        let mut rows = vec![];
        if !cfg!(feature = "kobo") {
            rows.push(SettingsRow::Theme);
            if settings_navigation::DESKTOP_FULLSCREEN {
                rows.push(SettingsRow::Fullscreen);
            }
        }
        reading_ruler::append_rows(&mut rows, ReaderSettings::preferences(cx).reading_ruler != ReadingRulerStyle::Off);
        rows.push(SettingsRow::Typeface);
        if self.cjk_font.is_some() {
            rows.push(SettingsRow::CjkTypeface);
        }
        rows.push(SettingsRow::Alignment);
        rows.extend([SettingsRow::FontWeight, SettingsRow::Hyphenation,
            SettingsRow::FontSize, SettingsRow::WordSpacing]);
        rows.push(SettingsRow::LineLength);
        rows.extend([SettingsRow::LineHeightMode, SettingsRow::LineHeight,
            SettingsRow::ParagraphSpacingMode, SettingsRow::ParagraphSpacing,
            SettingsRow::FirstLineIndentMode, SettingsRow::FirstLineIndent]);
        rows.extend([SettingsRow::SideMargins, SettingsRow::VerticalMargins]);
        rows.push(SettingsRow::ColumnGap);
        rows
    }

    fn handle_settings_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let rows = self.settings_rows(cx);
        let direction = match key {
            "up" | "left" => -1,
            "down" | "right" => 1,
            _ => return false,
        };
        if matches!(key, "up" | "down") {
            self.settings_navigation.move_row(&rows, direction);
        } else {
            let current = ReaderSettings::preferences(cx).clone();
            match self.settings_navigation.selected(&rows) {
                SettingsRow::Theme => settings_navigation::adjust_theme(direction, cx),
                SettingsRow::Fullscreen => settings_navigation::set_fullscreen(direction > 0, window),
                SettingsRow::Typeface => {
                    if SYSTEM_FONT_PICKER && direction > 0 && (current.custom_fonts_active() || current.font_family == ReaderFontFamily::SansSerif) {
                        self.select_custom_fonts(window, cx);
                        return true;
                    }
                    let family = if current.custom_fonts_active() { ReaderFontFamily::SansSerif } else { adjacent_value(&TYPEFACES.map(|(family, _, _)| family), current.font_family, direction) };
                    self.set_reader_font_family(family, cx);
                }
                SettingsRow::CjkTypeface => {
                    if SYSTEM_FONT_PICKER && direction > 0 {
                        self.select_cjk_custom_font(window, cx);
                    } else {
                        self.set_cjk_font_choice(CjkFontChoice::Publisher, cx);
                    }
                }
                SettingsRow::Alignment => self.set_reader_text_alignment(adjacent_value(&step_values(&ALIGNMENTS), current.text_alignment, direction), cx),
                SettingsRow::FontSize => self.adjust_font_size(direction as f32, cx),
                SettingsRow::Hyphenation => self.set_hyphenation(adjacent_value(&ReaderHyphenation::ALL, current.hyphenation, direction), cx),
                SettingsRow::WordSpacing => self.adjust_word_spacing(direction as f32 * WORD_SPACING_STEP_PERCENT, cx),
                SettingsRow::FontWeight => self.set_font_weight(adjacent_value(&ReaderFontWeight::ALL, current.font_weight, direction), cx),
                SettingsRow::LineHeightMode => self.set_override_line_height(direction > 0, cx),
                SettingsRow::LineHeight => self.adjust_line_height(direction as f32 * 0.1, cx),
                SettingsRow::ParagraphSpacingMode => self.set_override_paragraph_spacing(direction > 0, cx),
                SettingsRow::ParagraphSpacing => self.adjust_paragraph_spacing(direction as f32 * PARAGRAPH_STEP_EM, cx),
                SettingsRow::FirstLineIndentMode => self.set_first_line_indent(adjacent_value(&ReaderFirstLineIndent::ALL, current.first_line_indent, direction), cx),
                SettingsRow::FirstLineIndent => self.adjust_first_line_indent(direction as f32 * PARAGRAPH_STEP_EM, cx),
                SettingsRow::ReadingRuler => self.set_reading_ruler_enabled(direction > 0, cx),
                SettingsRow::ReadingRulerStyle => self.set_reading_ruler(adjacent_value(&[ReadingRulerStyle::Band, ReadingRulerStyle::Focus, ReadingRulerStyle::Underline], current.reading_ruler, direction), cx),
                SettingsRow::ReadingRulerLines => self.set_reading_ruler_lines(adjacent_step(&[1, 2, 3, 4, 5], current.reading_ruler_lines, direction), cx),
                SettingsRow::LineLength => self.set_line_width_range(adjacent_step(&step_values(&LINE_LENGTHS), (current.min_line_width_ch, current.max_line_width_ch), direction), window, cx),
                SettingsRow::SideMargins => self.set_horizontal_margin(current.horizontal_margin_px + direction as f32 * SPACING_STEP_PX, cx),
                SettingsRow::VerticalMargins => self.set_vertical_margin(current.vertical_margin_px + direction as f32 * SPACING_STEP_PX, cx),
                SettingsRow::ColumnGap => self.set_min_column_gap(current.min_column_gap_px + direction as f32 * SPACING_STEP_PX, cx),
                _ => return false,
            }
        }
        self.sidebar_focus.focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn render_settings_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.settings_rows(cx);
        let theme = components::browser_theme(cx);
        let current = ReaderSettings::preferences(cx).clone();
        let general =
            components::reader_settings_tab_body(theme)
                .when(!cfg!(feature = "kobo"), |body| body.child(components::reader_settings_group(theme).child(self.settings_navigation.row(SettingsRow::Theme, &rows, crate::shell::theme_picker::reader_theme_picker(cx, theme), theme))))
                .children(self.settings_navigation.fullscreen_group(&rows, window, theme))
                .child(self.reading_ruler_settings(current.reading_ruler, current.reading_ruler_lines, theme, cx))
                .child(
                    components::reader_settings_group(theme).gap(px(components::SPACE_MD))
                        .child(
                            div().w_full().min_w_0().flex().flex_col().gap(px(components::SPACE_MD))
                                .child(self.typeface_row(current.font_family, theme, cx))
                                .children(self.cjk_typeface_row(theme, cx))
                                .child(self.tile_row(TileRow { id: "reader-alignment", row: SettingsRow::Alignment, label: "Alignment" }, &ALIGNMENTS, current.text_alignment, theme, cx, |this, alignment, _, cx| {
                                    this.set_reader_text_alignment(alignment, cx)
                                })),
                        )
                        .child(
                            div().w_full().min_w_0().flex().flex_col().gap(px(components::SPACE_MD))
                                .child(self.font_weight_settings(theme, cx))
                                .child(self.hyphenation_settings(theme, cx)),
                        )
                        .child(
                            div().w_full().min_w_0().flex().flex_col().gap(px(components::SPACE_XS))
                                .child(
                                    components::reader_setting_row("Font size").map(|row| self.settings_navigation.row(SettingsRow::FontSize, &rows, row, theme)).child(components::settings_stepper(
                                        components::settings_stepper_button("font-size-smaller", "−", false)
                                            .tooltip(components::configured_shortcut_tooltip("Decrease font size", "reader.font_decrease", cx))
                                            .on_click(cx.listener(|this, _, _, cx| this.adjust_font_size(-1.0, cx))),
                                        format!("{:.0}px", current.font_size),
                                        components::settings_stepper_button("font-size-larger", "+", false)
                                            .tooltip(components::configured_shortcut_tooltip("Increase font size", "reader.font_increase", cx))
                                            .on_click(cx.listener(|this, _, _, cx| this.adjust_font_size(1.0, cx))),
                                        theme,
                                    )),
                                )
                                .child(self.word_spacing_settings(theme, cx)),
                        ),
                )
                .child(
                    components::reader_settings_group(theme).gap(px(components::SPACE_MD))
                        .child(self.tile_row(
                            TileRow { id: "reader-line-length", row: SettingsRow::LineLength, label: "Line length" },
                            &LINE_LENGTHS,
                            (current.min_line_width_ch, current.max_line_width_ch),
                            theme,
                            cx,
                            |this, range, window, cx| this.set_line_width_range(range, window, cx),
                        ))
                        .child(self.line_height_settings(theme, cx)),
                )
                .child(
                    components::reader_settings_group(theme)
                        .child(self.paragraph_spacing_settings(theme, cx)),
                )
                .child(
                    components::reader_settings_group(theme)
                        .child(self.first_line_indent_settings(theme, cx)),
                )
                .child(
                    components::reader_settings_group(theme)
                        .child(
                            components::reader_setting_row("Side margins").map(|row| self.settings_navigation.row(SettingsRow::SideMargins, &rows, row, theme)).child(components::settings_stepper(
                                components::settings_stepper_button("reader-side-margins-decrease", "−", current.horizontal_margin_px <= READER_SPACING_MIN)
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_horizontal_margin(current.horizontal_margin_px - SPACING_STEP_PX, cx))),
                                format!("{:.0}px", current.horizontal_margin_px),
                                components::settings_stepper_button("reader-side-margins-increase", "+", current.horizontal_margin_px >= READER_SPACING_MAX)
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_horizontal_margin(current.horizontal_margin_px + SPACING_STEP_PX, cx))),
                                theme,
                            )),
                        )
                        .child(
                            components::reader_setting_row("Vertical margins").map(|row| self.settings_navigation.row(SettingsRow::VerticalMargins, &rows, row, theme)).child(components::settings_stepper(
                                components::settings_stepper_button("reader-vertical-margins-decrease", "−", current.vertical_margin_px <= READER_SPACING_MIN)
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_vertical_margin(current.vertical_margin_px - SPACING_STEP_PX, cx))),
                                format!("{:.0}px", current.vertical_margin_px),
                                components::settings_stepper_button("reader-vertical-margins-increase", "+", current.vertical_margin_px >= READER_SPACING_MAX)
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_vertical_margin(current.vertical_margin_px + SPACING_STEP_PX, cx))),
                                theme,
                            )),
                        )
                        .child(
                            components::reader_setting_row("Column gap").map(|row| self.settings_navigation.row(SettingsRow::ColumnGap, &rows, row, theme)).child(components::settings_stepper(
                                components::settings_stepper_button("reader-column-gap-decrease", "−", current.min_column_gap_px <= READER_SPACING_MIN)
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_min_column_gap(current.min_column_gap_px - SPACING_STEP_PX, cx))),
                                format!("{:.0}px", current.min_column_gap_px),
                                components::settings_stepper_button("reader-column-gap-increase", "+", current.min_column_gap_px >= READER_SPACING_MAX)
                                    .on_click(cx.listener(move |this, _, _, cx| this.set_min_column_gap(current.min_column_gap_px + SPACING_STEP_PX, cx))),
                                theme,
                            )),
                        ),
                )
                .into_any_element();
        crate::shell::keyboard_settings::with_keyboard_settings(general, window, cx)
    }

    fn hyphenation_settings(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let rows = self.settings_rows(cx);
        let selected = ReaderSettings::preferences(cx).hyphenation;
        let buttons = ReaderHyphenation::ALL.into_iter().enumerate().map(|(index, mode)| {
            components::settings_choice_button(("reader-hyphenation", index), mode.label(), mode == selected, theme)
                .on_click(cx.listener(move |reader, _, _, cx| reader.set_hyphenation(mode, cx)))
        });
        let group = components::reader_setting_row_smallcaps("Hyphenation", theme)
            .child(components::reader_choice_button_group("reader-hyphenation", buttons, theme));
        self.settings_navigation.row(SettingsRow::Hyphenation, &rows, group, theme)
    }

    fn word_spacing_settings(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let rows = self.settings_rows(cx);
        let percent = ReaderSettings::preferences(cx).word_spacing_percent;
        let row = components::reader_setting_row("Word spacing").child(components::settings_stepper(
            components::settings_stepper_button("reader-word-spacing-decrease", "−", percent <= WORD_SPACING_MIN_PERCENT)
                .on_click(cx.listener(|reader, _, _, cx| reader.adjust_word_spacing(-WORD_SPACING_STEP_PERCENT, cx))),
            format!("{percent:.0}%"),
            components::settings_stepper_button("reader-word-spacing-increase", "+", percent >= WORD_SPACING_MAX_PERCENT)
                .on_click(cx.listener(|reader, _, _, cx| reader.adjust_word_spacing(WORD_SPACING_STEP_PERCENT, cx))),
            theme,
        ));
        self.settings_navigation.row(SettingsRow::WordSpacing, &rows, row, theme)
    }

    fn font_weight_settings(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let rows = self.settings_rows(cx);
        let selected = ReaderSettings::preferences(cx).font_weight;
        let buttons = ReaderFontWeight::ALL.into_iter().enumerate().map(|(index, weight)| {
            components::settings_choice_button(("reader-font-weight", index), weight.label(), weight == selected, theme)
                .on_click(cx.listener(move |reader, _, _, cx| reader.set_font_weight(weight, cx)))
        });
        let group = components::reader_setting_row_smallcaps("Font weight", theme)
            .child(components::reader_choice_button_group("reader-font-weight", buttons, theme));
        self.settings_navigation.row(SettingsRow::FontWeight, &rows, group, theme)
    }

    fn line_height_settings(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let rows = self.settings_rows(cx);
        let current = ReaderSettings::preferences(cx).clone();
        let book = components::reader_picture_tile("reader-line-spacing-book", components::reader_tile_picture("icons/reader-settings/align-book.svg"), "Book", !current.override_line_height, theme)
            .on_click(cx.listener(|this, _, _, cx| this.set_override_line_height(false, cx)));
        let custom = components::reader_numeric_tile("reader-line-spacing-custom", format!("{:.1}×", current.line_height), current.override_line_height,
            components::settings_stepper_button("reader-line-height-decrease", "−", current.line_height <= LINE_HEIGHT_MIN)
                .tooltip("Decrease line spacing")
                .on_click(cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.adjust_line_height(-0.1, cx); })),
            components::settings_stepper_button("reader-line-height-increase", "+", current.line_height >= LINE_HEIGHT_MAX)
                .tooltip("Increase line spacing")
                .on_click(cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.adjust_line_height(0.1, cx); })), theme, cx.listener(|this, _, _, cx| this.set_override_line_height(true, cx)))
            .aria_label("Custom line spacing");
        let choices = components::reader_tile_group("reader-line-spacing-mode").flex_wrap()
            .child(self.settings_navigation.row(SettingsRow::LineHeightMode, &rows, div().flex().flex_1().min_w(rems(3.0)).child(book), theme))
            .child(self.settings_navigation.row(SettingsRow::LineHeight, &rows, div().flex().flex_1().min_w(rems(components::reader_numeric_tile_width_rem())).child(custom), theme));
        components::reader_setting_row_smallcaps("Line spacing", theme).child(choices)
    }

    fn paragraph_spacing_settings(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let rows = self.settings_rows(cx);
        let current = ReaderSettings::preferences(cx).clone();
        let book = components::reader_picture_tile("reader-paragraph-spacing-book", components::reader_tile_picture("icons/reader-settings/align-book.svg"), "Book", !current.override_paragraph_spacing, theme)
            .on_click(cx.listener(|this, _, _, cx| this.set_override_paragraph_spacing(false, cx)));
        let custom = components::reader_numeric_tile("reader-paragraph-spacing-custom", format!("{:.1} em", current.paragraph_spacing_em), current.override_paragraph_spacing,
            components::settings_stepper_button("reader-paragraph-spacing-decrease", "−", current.paragraph_spacing_em <= 0.0)
                .tooltip("Decrease paragraph spacing")
                .on_click(cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.adjust_paragraph_spacing(-PARAGRAPH_STEP_EM, cx); })),
            components::settings_stepper_button("reader-paragraph-spacing-increase", "+", current.paragraph_spacing_em >= PARAGRAPH_SPACING_MAX_EM)
                .tooltip("Increase paragraph spacing")
                .on_click(cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.adjust_paragraph_spacing(PARAGRAPH_STEP_EM, cx); })), theme, cx.listener(|this, _, _, cx| this.set_override_paragraph_spacing(true, cx)))
            .aria_label("Custom paragraph spacing");
        let choices = components::reader_tile_group("reader-paragraph-spacing-mode").flex_wrap()
            .child(self.settings_navigation.row(SettingsRow::ParagraphSpacingMode, &rows, div().flex().flex_1().min_w(rems(3.0)).child(book), theme))
            .child(self.settings_navigation.row(SettingsRow::ParagraphSpacing, &rows, div().flex().flex_1().min_w(rems(components::reader_numeric_tile_width_rem())).child(custom), theme));
        components::reader_setting_row_smallcaps("Paragraph spacing", theme).child(choices)
    }

    fn first_line_indent_settings(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let rows = self.settings_rows(cx);
        let current = ReaderSettings::preferences(cx).clone();
        let buttons = [ReaderFirstLineIndent::Book, ReaderFirstLineIndent::Off].into_iter().enumerate().map(|(index, mode)| {
            let picture = if mode == ReaderFirstLineIndent::Book { "icons/reader-settings/align-book.svg" } else { "icons/reader-settings/align-left.svg" };
            components::reader_picture_tile(("reader-first-line-indent-mode", index), components::reader_tile_picture(picture), mode.label(), current.first_line_indent == mode, theme)
                .on_click(cx.listener(move |this, _, _, cx| this.set_first_line_indent(mode, cx)))
        });
        let modes = components::reader_tile_group("reader-first-line-indent-presets").children(buttons);
        let custom = components::reader_numeric_tile("reader-first-line-indent-custom", format!("{:.1} em", current.first_line_indent_em), current.first_line_indent == ReaderFirstLineIndent::Custom,
            components::settings_stepper_button("reader-first-line-indent-decrease", "−", current.first_line_indent_em <= 0.0)
                .tooltip("Decrease first-line indentation")
                .on_click(cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.adjust_first_line_indent(-PARAGRAPH_STEP_EM, cx); })),
            components::settings_stepper_button("reader-first-line-indent-increase", "+", current.first_line_indent_em >= FIRST_LINE_INDENT_MAX_EM)
                .tooltip("Increase first-line indentation")
                .on_click(cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.adjust_first_line_indent(PARAGRAPH_STEP_EM, cx); })), theme, cx.listener(|this, _, _, cx| this.set_first_line_indent(ReaderFirstLineIndent::Custom, cx)))
            .aria_label("Custom first-line indentation");
        let choices = components::reader_tile_group("reader-first-line-indent-mode").flex_wrap()
            .child(self.settings_navigation.row(SettingsRow::FirstLineIndentMode, &rows, div().flex().flex_1().min_w(rems(6.5)).child(modes), theme))
            .child(self.settings_navigation.row(SettingsRow::FirstLineIndent, &rows, div().flex().flex_1().min_w(rems(components::reader_numeric_tile_width_rem())).child(custom), theme));
        components::reader_setting_row_smallcaps("First-line indentation", theme).child(choices)
    }

    fn reading_ruler_settings(&self, current: ReadingRulerStyle, lines: usize, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let rows = self.settings_rows(cx);
        reading_ruler::render(
            ReadingRulerSettings {
                current,
                off: ReadingRulerStyle::Off,
                styles: [ReadingRulerStyle::Band, ReadingRulerStyle::Focus, ReadingRulerStyle::Underline],
                lines,
                set_enabled: Self::set_reading_ruler_enabled,
                set_style: Self::set_reading_ruler,
                set_lines: Self::set_reading_ruler_lines,
            },
            &self.settings_navigation,
            &rows,
            theme,
            cx,
        )
    }

    /// A labelled row of three picture tiles for one setting. A stored value
    /// that matches no tile, such as one kept from an older version, simply
    /// selects none until a tile is chosen.
    fn tile_row<T: Copy + PartialEq + 'static>(&self, row: TileRow, steps: &Steps<T>, current: T, theme: components::BrowserTheme, cx: &mut Context<Self>, choose: fn(&mut Self, T, &mut Window, &mut Context<Self>)) -> gpui::Div {
        let rows = self.settings_rows(cx);
        let TileRow { id, row, label } = row;
        let tiles = steps.iter().enumerate().map(|(index, &(value, name, picture))| {
            components::reader_picture_tile((id, index), components::reader_tile_picture(picture), name, value == current, theme).on_click(cx.listener(move |this, _, window, cx| choose(this, value, window, cx)))
        });
        components::reader_setting_row_smallcaps(label, theme).map(|element| self.settings_navigation.row(row, &rows, element, theme)).child(components::reader_tile_group(id).children(tiles))
    }

    /// Main typeface choices retain their sample above the label.
    fn typeface_row(&self, current: ReaderFontFamily, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let rows = self.settings_rows(cx);
        let preferences = ReaderSettings::preferences(cx).clone();
        let custom = preferences.custom_fonts_active();
        let custom_face = ReaderFontRole::ALL.into_iter().find_map(|role| preferences.custom_font_family(role).map(str::to_owned));
        let tiles = TYPEFACES.iter().enumerate().map(|(index, &(family, name, face))| {
            components::reader_picture_tile(("reader-typeface", index), components::reader_typeface_sample(face), name, !custom && family == current, theme)
                .on_click(cx.listener(move |this, _, _, cx| this.set_reader_font_family(family, cx)))
        });
        components::reader_setting_row_smallcaps(if self.cjk_font.is_some() { "Latin typeface" } else { "Typeface" }, theme)
            .map(|element| self.settings_navigation.row(SettingsRow::Typeface, &rows, element, theme))
            .child(components::reader_tile_group("reader-typeface").children(tiles).when(SYSTEM_FONT_PICKER, |group| {
                let sample = components::reader_typeface_sample(None).when_some(custom_face, |sample, family| sample.font_family(family));
                group.child(components::reader_picture_tile("reader-custom-fonts", sample, "Custom", custom, theme).on_click(cx.listener(|this, _, window, cx| this.select_custom_fonts(window, cx))))
            }))
            .when(SYSTEM_FONT_PICKER, |row| row.child(self.font_picker.clone()))
    }
    fn cjk_typeface_row(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let rows = self.settings_rows(cx);
        let preferences = ReaderSettings::preferences(cx).clone();
        let cjk = self.cjk_font?;
        let sample = || div().h(rems(2.25)).flex().items_center().text_size(rems(1.5)).font_family(cjk.family()).child(cjk.sample());
        let system_family = preferences.saved_cjk_font_family().map(str::to_owned);
        let custom_sample = sample().when_some(system_family.clone().filter(|family| cx.text_system().font_id(&font(family.clone())).is_ok()), |sample, family| sample.font_family(family));
        let has_custom = system_family.is_some();
        let custom_label = system_family.unwrap_or_else(|| "Custom".into());
        Some(
            components::reader_setting_row_smallcaps("CJK typeface", theme)
                .map(|element| self.settings_navigation.row(SettingsRow::CjkTypeface, &rows, element, theme))
                .child(
                    components::reader_tile_group("reader-cjk-typeface")
                        .child(
                            components::reader_picture_tile("reader-cjk-publisher", sample(), "Book", preferences.cjk_font_choice == CjkFontChoice::Publisher, theme)
                                .on_click(cx.listener(|this, _, _, cx| this.set_cjk_font_choice(CjkFontChoice::Publisher, cx))),
                        )
                        .when(SYSTEM_FONT_PICKER, |group| {
                            group.child(
                                components::reader_picture_tile("reader-cjk-system", custom_sample, custom_label, preferences.cjk_font_choice == CjkFontChoice::Custom, theme)
                                    .track_focus(self.cjk_font_picker.read(cx).cjk_focus())
                                    .on_click(cx.listener(|this, _, window, cx| this.select_cjk_custom_font(window, cx)))
                                    .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                            this.select_cjk_custom_font(window, cx);
                                            cx.stop_propagation();
                                        }
                                    })),
                            )
                        }),
                )
                .when(SYSTEM_FONT_PICKER && has_custom && preferences.cjk_font_choice == CjkFontChoice::Custom, |row| {
                    row.child(components::outlined_text_button("reader-cjk-change-font", "Change font").on_click(cx.listener(|this, _, window, cx| this.open_cjk_font_picker(window, cx))))
                })
                .when(SYSTEM_FONT_PICKER, |row| row.child(self.cjk_font_picker.clone())),
        )
    }
}
