//! EPUB-specific appearance controls in the shared sidebar.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Window};
use ui_components as components;

use crate::epub::ReaderView;
use crate::settings::ReaderSettings;
use crate::shell::toolbar::SidebarTarget;
use crate::{ReaderFontFamily, ReaderTextAlignment};

impl SidebarTarget for ReaderView {
    #[cfg(feature = "kobo")]
    fn brightness_controls(&self) -> gpui::Entity<crate::shell::brightness::BrightnessControls> {
        self.brightness.clone()
    }

    fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        (self.close_reader)(window, cx);
    }

    fn hide_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) {
        if components::uses_mobile_navigation(window) {
            if self.sidebar.close() {
                crate::invalidation::notify(cx, crate::invalidation::Component::ReaderShell, "sidebar_closed");
            }
        } else {
            self.chrome.hide(cx);
        }
    }

    fn search_is_open(&self) -> bool {
        self.search.query.is_some()
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::shell::input::CommandTarget::execute(self, &crate::ToggleSearch, window, cx);
    }
}

impl ReaderView {
    pub(super) fn render_settings_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let current = ReaderSettings::preferences(cx).clone();
        components::reader_settings_tab_body(theme)
            .when(!cfg!(feature = "kobo"), |body| body.child(crate::shell::theme_picker::reader_theme_picker(cx, theme)))
            .child(
                components::reader_settings_group(theme)
                    .child(
                        components::reader_setting_row_smallcaps("Typeface", theme).child(components::reader_choice_button_group(
                            "reader-font-group",
                            [
                                components::reader_choice_button("reader-font-publisher", "Book", current.font_family == ReaderFontFamily::Publisher, theme)
                                    .on_click(cx.listener(|this, _, _, cx| this.set_reader_font_family(ReaderFontFamily::Publisher, cx))),
                                components::reader_choice_button("reader-font-serif", "Serif", current.font_family == ReaderFontFamily::Serif, theme)
                                    .on_click(cx.listener(|this, _, _, cx| this.set_reader_font_family(ReaderFontFamily::Serif, cx))),
                            ],
                        )),
                    )
                    .child(
                        components::reader_setting_row_smallcaps("Alignment", theme).child(components::reader_choice_button_group(
                            "reader-alignment-group",
                            [
                                components::reader_choice_button("reader-alignment-publisher", "Book", current.text_alignment == ReaderTextAlignment::Publisher, theme)
                                    .on_click(cx.listener(|this, _, _, cx| this.set_reader_text_alignment(ReaderTextAlignment::Publisher, cx))),
                                components::reader_choice_button("reader-alignment-start", "Left", current.text_alignment == ReaderTextAlignment::Start, theme)
                                    .on_click(cx.listener(|this, _, _, cx| this.set_reader_text_alignment(ReaderTextAlignment::Start, cx))),
                                components::reader_choice_button("reader-alignment-justify", "Justify", current.text_alignment == ReaderTextAlignment::Justify, theme)
                                    .on_click(cx.listener(|this, _, _, cx| this.set_reader_text_alignment(ReaderTextAlignment::Justify, cx))),
                            ],
                        )),
                    ),
            )
            .child(
                components::reader_settings_group(theme)
                    .child(components::reader_setting_row("Font size").child(components::settings_stepper(
                        components::settings_stepper_button("font-size-smaller", "−", false, theme).tooltip("Decrease font size (Alt+-)").on_click(cx.listener(|this, _, _, cx| this.adjust_font_size(-1.0, cx))),
                        format!("{:.0}px", current.font_size),
                        components::settings_stepper_button("font-size-larger", "+", false, theme).tooltip("Increase font size (Alt++)").on_click(cx.listener(|this, _, _, cx| this.adjust_font_size(1.0, cx))),
                        theme,
                    )))
                    .child(components::reader_setting_row("Line height").child(components::settings_stepper(
                        components::settings_stepper_button("line-height-smaller", "−", false, theme).on_click(cx.listener(|this, _, _, cx| this.adjust_line_height(-0.1, cx))),
                        format!("{:.1}", current.line_height),
                        components::settings_stepper_button("line-height-larger", "+", false, theme).on_click(cx.listener(|this, _, _, cx| this.adjust_line_height(0.1, cx))),
                        theme,
                    ))),
            )
            .child(
                components::reader_settings_group(theme)
                    .child(components::reader_setting_row("Side margins").child(components::settings_stepper(
                        components::settings_stepper_button("side-margins-smaller", "−", false, theme).on_click(cx.listener(|this, _, _, cx| this.adjust_horizontal_margin(-8.0, cx))),
                        format!("{:.0}px", current.horizontal_margin_px),
                        components::settings_stepper_button("side-margins-larger", "+", false, theme).on_click(cx.listener(|this, _, _, cx| this.adjust_horizontal_margin(8.0, cx))),
                        theme,
                    )))
                    .child(components::reader_setting_row("Top/bottom margins").child(components::settings_stepper(
                        components::settings_stepper_button("vertical-margins-smaller", "−", false, theme).on_click(cx.listener(|this, _, _, cx| this.adjust_vertical_margin(-8.0, cx))),
                        format!("{:.0}px", current.vertical_margin_px),
                        components::settings_stepper_button("vertical-margins-larger", "+", false, theme).on_click(cx.listener(|this, _, _, cx| this.adjust_vertical_margin(8.0, cx))),
                        theme,
                    )))
                    .when(!cfg!(feature = "kobo"), |section| {
                        section.child(components::reader_setting_row("Column gap").child(components::settings_stepper(
                            components::settings_stepper_button("column-gap-smaller", "−", false, theme).on_click(cx.listener(|this, _, _, cx| this.adjust_min_column_gap(-8.0, cx))),
                            format!("{:.0}px", current.min_column_gap_px),
                            components::settings_stepper_button("column-gap-larger", "+", false, theme).on_click(cx.listener(|this, _, _, cx| this.adjust_min_column_gap(8.0, cx))),
                            theme,
                        )))
                    }),
            )
            .child(components::reader_settings_group(theme).when(!cfg!(feature = "kobo"), |section| {
                section
                    .child(components::reader_setting_row("Line width min").child(components::settings_stepper(
                        components::settings_stepper_button("line-width-min-smaller", "−", false, theme).on_click(cx.listener(|this, _, window, cx| this.adjust_min_line_width_ch(-5.0, window, cx))),
                        format!("{:.0}ch", current.min_line_width_ch),
                        components::settings_stepper_button("line-width-min-larger", "+", false, theme).on_click(cx.listener(|this, _, window, cx| this.adjust_min_line_width_ch(5.0, window, cx))),
                        theme,
                    )))
                    .child(components::reader_setting_row("Line width max").child(components::settings_stepper(
                        components::settings_stepper_button("line-width-max-smaller", "−", false, theme).on_click(cx.listener(|this, _, window, cx| this.adjust_max_line_width_ch(-5.0, window, cx))),
                        format!("{:.0}ch", current.max_line_width_ch),
                        components::settings_stepper_button("line-width-max-larger", "+", false, theme).on_click(cx.listener(|this, _, window, cx| this.adjust_max_line_width_ch(5.0, window, cx))),
                        theme,
                    )))
            }))
            .into_any_element()
    }
}
