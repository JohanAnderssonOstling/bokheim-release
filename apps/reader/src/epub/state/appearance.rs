//! EPUB appearance preferences and matching renderer commands.

use crate::invalidation::notify;
use app_preferences::{
    FIRST_LINE_INDENT_MAX_EM, FONT_SIZE_MAX, FONT_SIZE_MIN, LINE_HEIGHT_MAX, LINE_HEIGHT_MIN, LINE_WIDTH_MAX, LINE_WIDTH_MIN, PARAGRAPH_SPACING_MAX_EM, READER_SPACING_MAX, READER_SPACING_MIN, ReaderFirstLineIndent, ReaderFontFamily,
    ReaderHyphenation, ReaderTextAlignment, WORD_SPACING_MAX_PERCENT, WORD_SPACING_MIN_PERCENT,
};
use gpui::{Context, Window};
use skrift_column_paginator::RendererCommand;
use html_view_gpui::ReadingRulerStyle;

use crate::epub::ReaderView;
use crate::invalidation::Component;
use crate::settings::{CjkFontChoice, ReaderFontRole, ReaderPreferences, ReaderSettings, SidebarTab};
use crate::shell::chrome;

/// The layout engine's fallback `ch` metric is half an em. Keeping this
/// conversion here makes the preference typographic rather than tied to a
/// particular screen's pixels, while the layout itself continues to receive
/// its required pixel width.
const CH_ADVANCE_EM: f64 = 0.5;

/// Keeps glyph ink and logical-to-framebuffer rounding inside the Kobo panel.
/// Include this guard in both the column plan and the document's padding.
const KOBO_READER_EDGE_INSET: f32 = 2.0;

fn horizontal_insets(document_width: f32, requested_margin: f32, edge_inset: f32) -> (f32, f32) {
    let maximum_margin = ((document_width - 100.0) / 2.0 - edge_inset).max(0.0);
    let inset = requested_margin.min(maximum_margin) + edge_inset;
    (inset, (document_width - inset * 2.0).max(1.0))
}

fn line_width_range_px(preferences: &ReaderPreferences) -> (f64, f64) {
    let ch = f64::from(preferences.font_size) * CH_ADVANCE_EM;
    let minimum = (preferences.min_line_width_ch * ch).clamp(LINE_WIDTH_MIN, LINE_WIDTH_MAX);
    let maximum = (preferences.max_line_width_ch * ch).clamp(minimum, LINE_WIDTH_MAX);
    (minimum, maximum)
}

/// The renderer puts unused width between columns after applying the explicit
/// outer margin. A single column has no gap to constrain.
fn column_count_for_gap(viewport_width: f64, column_width: f64, minimum_gap: f64, desired: u8) -> u8 {
    let count = ((viewport_width + minimum_gap) / (column_width + minimum_gap)).floor();
    (count.max(1.0) as u8).min(desired.max(1))
}

/// Use the integer width the renderer will actually keep. Reserve one layout
/// pixel for split and framebuffer rounding so an exact fit cannot become one
/// column when the final GPUI document area is fractionally narrower.
fn fitted_columns_and_width(viewport_width: f64, minimum_gap: f64, desired: u8, minimum_width: f64, maximum_width: f64) -> (u8, f64) {
    let count = column_count_for_gap(viewport_width, minimum_width, minimum_gap, desired);
    let fit = ((viewport_width - f64::from(count.saturating_sub(1)) * minimum_gap) / f64::from(count) - 1.0).floor().max(1.0);
    // The preferred minimum cannot make a column wider than its actual slot.
    // Otherwise a narrow reader clips the ends of lines at the viewport edge.
    (count, fit.min(maximum_width))
}

impl ReaderView {
    pub(in crate::epub) fn set_reading_ruler(&self, style: ReadingRulerStyle, cx: &mut Context<Self>) {
        if ReaderSettings::update(cx, |preferences| {
            if style != ReadingRulerStyle::Off {
                preferences.reading_ruler_last_style = style;
            } else if preferences.reading_ruler != ReadingRulerStyle::Off {
                preferences.reading_ruler_last_style = preferences.reading_ruler;
            }
            preferences.reading_ruler = style;
        }) {
            cx.notify();
        }
    }

    pub(in crate::epub) fn set_reading_ruler_enabled(&self, enabled: bool, cx: &mut Context<Self>) {
        if (ReaderSettings::preferences(cx).reading_ruler != ReadingRulerStyle::Off) == enabled {
            return;
        }
        let style = if enabled {
            match ReaderSettings::preferences(cx).reading_ruler_last_style {
                ReadingRulerStyle::Off => ReadingRulerStyle::Band,
                style => style,
            }
        } else {
            ReadingRulerStyle::Off
        };
        self.set_reading_ruler(style, cx);
    }

    pub(in crate::epub) fn set_reading_ruler_lines(&self, lines: usize, cx: &mut Context<Self>) {
        if ReaderSettings::update(cx, |preferences| preferences.reading_ruler_lines = lines.clamp(1, 5)) {
            cx.notify();
        }
    }

    pub(in crate::epub) fn active_sidebar_tab(&self, window: &Window) -> Option<SidebarTab> {
        if chrome::sidebar_overlays_document(window) { self.sidebar.effective(true) } else { Some(self.sidebar.effective_or_default(true)) }
    }

    /// The same horizontal geometry is used when painting and when the user
    /// asks to fit columns, including the sidebar's actual clamped width.
    pub(in crate::epub) fn horizontal_layout(window: &Window, cx: &Context<Self>, sidebar_visible: bool) -> (f32, f32, f32) {
        let sidebar_width = chrome::sidebar_width(sidebar_visible, window, cx);
        // GPUI's client-side window frame reserves space on both sides of the
        // window before laying out the reader. viewport_size includes that
        // space, while the HTML view's final bounds do not.
        let frame = gpui_component::window_paddings(window);
        let document_width = (f32::from(window.viewport_size().width - frame.left - frame.right) - sidebar_width).max(1.0);
        let edge_inset = if cfg!(feature = "kobo") { KOBO_READER_EDGE_INSET } else { 0.0 };
        let (inset, available_width) = horizontal_insets(document_width, ReaderSettings::preferences(cx).horizontal_margin_px, edge_inset);
        (sidebar_width, inset, available_width)
    }

    pub(in crate::epub) fn fitted_column_width(&self, available_width: f64, cx: &Context<Self>) -> f64 {
        let preferences = ReaderSettings::preferences(cx);
        let scale = preferences.scale.max(0.1);
        let viewport_width = available_width.max(1.0) / scale;
        let gap = f64::from(preferences.min_column_gap_px) / scale;
        let (minimum, maximum) = line_width_range_px(preferences);
        fitted_columns_and_width(viewport_width, gap, self.desired_column_count, minimum, maximum).1
    }

    pub(in crate::epub) fn max_columns_for_gap(&self, available_width: f64, cx: &Context<Self>) -> u8 {
        let preferences = ReaderSettings::preferences(cx);
        let scale = preferences.scale.max(0.1);
        let (minimum, maximum) = line_width_range_px(preferences);
        fitted_columns_and_width(available_width / scale, f64::from(preferences.min_column_gap_px) / scale, self.desired_column_count, minimum, maximum).0
    }

    /// Persists a preference, applies its renderer command, and refreshes the
    /// reader only when the value actually changed.
    fn apply_appearance(&self, cx: &mut Context<Self>, update: impl FnOnce(&mut ReaderPreferences), command: impl FnOnce(&ReaderPreferences) -> RendererCommand) {
        if !ReaderSettings::update(cx, update) {
            return;
        }
        let command = command(ReaderSettings::preferences(cx));
        self.with_renderer(cx, |renderer, cx| renderer.queue_appearance(command, cx));
        notify(cx, Component::ReaderShell, "preferences_changed");
    }

    fn apply_style_override(&self, cx: &mut Context<Self>, update: impl FnOnce(&mut ReaderPreferences)) {
        // Read before the update: the overrides carry the theme's two lightness
        // ends, and those come from the application rather than from any reader
        // preference.
        let theme = crate::settings::library_theme_adaptation(cx);
        let link_foreground = crate::settings::library_link_foreground(cx);
        let cjk = self.cjk_font;
        self.apply_appearance(cx, update, move |preferences| RendererCommand::SetReaderStyleOverrides(preferences.style_overrides_for_book(cjk, Some(theme), link_foreground)));
    }

    pub(in crate::epub) fn adjust_font_size(&self, delta: f32, cx: &mut Context<Self>) {
        let font_size = (ReaderSettings::preferences(cx).font_size + delta).clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
        self.apply_appearance(cx, |preferences| preferences.font_size = font_size, |_| RendererCommand::SetFontSize(font_size));
    }

    pub(in crate::epub) fn set_reader_font_family(&self, font_family: ReaderFontFamily, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| {
            preferences.font_family = font_family;
            preferences.custom_fonts_enabled = Some(false);
        });
        self.font_picker.update(cx, |picker, cx| picker.hide(cx));
    }

    pub(in crate::epub) fn select_custom_fonts(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.cjk_font_picker.update(cx, |picker, cx| picker.hide(cx));
        self.apply_style_override(cx, |preferences| preferences.custom_fonts_enabled = Some(true));
        self.font_picker.update(cx, |picker, cx| picker.show(window, cx));
    }

    pub(in crate::epub) fn set_cjk_font_choice(&self, choice: CjkFontChoice, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.cjk_font_choice = choice);
        self.cjk_font_picker.update(cx, |picker, cx| picker.hide(cx));
    }

    pub(in crate::epub) fn select_cjk_custom_font(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.font_picker.update(cx, |picker, cx| picker.hide(cx));
        self.cjk_font_picker.update(cx, |picker, cx| picker.activate_cjk(window, cx));
    }

    pub(in crate::epub) fn open_cjk_font_picker(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.font_picker.update(cx, |picker, cx| picker.hide(cx));
        self.cjk_font_picker.update(cx, |picker, cx| picker.show_cjk(window, cx));
    }

    pub(in crate::epub) fn set_custom_font_family(&self, role: ReaderFontRole, family: String, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.set_custom_font_family(role, family));
    }

    pub(in crate::epub) fn set_reader_text_alignment(&self, text_alignment: ReaderTextAlignment, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.text_alignment = text_alignment);
    }

    pub(in crate::epub) fn set_line_height(&self, line_height: f32, cx: &mut Context<Self>) {
        let line_height = line_height.clamp(LINE_HEIGHT_MIN, LINE_HEIGHT_MAX);
        self.apply_style_override(cx, |preferences| {
            preferences.override_line_height = true;
            preferences.line_height = line_height;
        });
    }

    pub(in crate::epub) fn adjust_line_height(&self, delta: f32, cx: &mut Context<Self>) {
        let line_height = ((ReaderSettings::preferences(cx).line_height + delta) * 10.0).round() / 10.0;
        self.set_line_height(line_height, cx);
    }

    pub(in crate::epub) fn set_override_line_height(&self, enabled: bool, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.override_line_height = enabled);
    }

    pub(in crate::epub) fn set_override_paragraph_spacing(&self, enabled: bool, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.override_paragraph_spacing = enabled);
    }

    pub(in crate::epub) fn adjust_paragraph_spacing(&self, delta: f32, cx: &mut Context<Self>) {
        let value = ((ReaderSettings::preferences(cx).paragraph_spacing_em + delta) * 10.0).round() / 10.0;
        self.apply_style_override(cx, |preferences| {
            preferences.override_paragraph_spacing = true;
            preferences.paragraph_spacing_em = value.clamp(0.0, PARAGRAPH_SPACING_MAX_EM);
        });
    }

    pub(in crate::epub) fn set_first_line_indent(&self, mode: ReaderFirstLineIndent, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.first_line_indent = mode);
    }

    pub(in crate::epub) fn adjust_first_line_indent(&self, delta: f32, cx: &mut Context<Self>) {
        let value = ((ReaderSettings::preferences(cx).first_line_indent_em + delta) * 10.0).round() / 10.0;
        self.apply_style_override(cx, |preferences| {
            preferences.first_line_indent = ReaderFirstLineIndent::Custom;
            preferences.first_line_indent_em = value.clamp(0.0, FIRST_LINE_INDENT_MAX_EM);
        });
    }

    pub(in crate::epub) fn adjust_word_spacing(&self, delta_percent: f32, cx: &mut Context<Self>) {
        let value = (ReaderSettings::preferences(cx).word_spacing_percent + delta_percent).clamp(WORD_SPACING_MIN_PERCENT, WORD_SPACING_MAX_PERCENT);
        self.apply_style_override(cx, |preferences| preferences.word_spacing_percent = value);
    }

    pub(in crate::epub) fn set_hyphenation(&self, mode: ReaderHyphenation, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.hyphenation = mode);
    }

    pub(in crate::epub) fn set_font_weight(&self, weight: app_preferences::ReaderFontWeight, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.font_weight = weight);
    }

    pub(in crate::epub) fn adjust_column_width(&self, delta: f64, window: &Window, cx: &mut Context<Self>) {
        // Keep the existing keyboard shortcuts useful: they shift both ends
        // of the typographic range by five characters.
        self.apply_line_width_range(window, cx, |minimum, maximum| {
            *minimum = (*minimum + delta.signum() * 5.0).clamp(20.0, 100.0);
            *maximum = (*maximum + delta.signum() * 5.0).clamp(20.0, 100.0);
        });
    }

    pub(in crate::epub) fn set_line_width_range(&self, range: (f64, f64), window: &Window, cx: &mut Context<Self>) {
        self.apply_line_width_range(window, cx, |minimum, maximum| {
            *minimum = range.0.clamp(20.0, 100.0);
            *maximum = range.1.clamp(*minimum, 100.0);
        });
    }

    fn apply_line_width_range(&self, window: &Window, cx: &mut Context<Self>, change: impl FnOnce(&mut f64, &mut f64)) {
        let current = ReaderSettings::preferences(cx).clone();
        let mut minimum = current.min_line_width_ch;
        let mut maximum = current.max_line_width_ch;
        change(&mut minimum, &mut maximum);
        if !ReaderSettings::update(cx, |preferences| {
            preferences.min_line_width_ch = minimum;
            preferences.max_line_width_ch = maximum;
        }) {
            return;
        }
        let sidebar_visible = (self.chrome.is_visible() || self.search.query.is_some()) && self.active_sidebar_tab(window).is_some();
        let (_, _, available_width) = Self::horizontal_layout(window, cx, sidebar_visible);
        let width = self.fitted_column_width(f64::from(available_width), cx);
        self.with_renderer(cx, |renderer, cx| renderer.queue_appearance(RendererCommand::SetColumnWidth(width), cx));
        notify(cx, Component::ReaderShell, "line_width_range_changed");
    }

    pub(in crate::epub) fn set_horizontal_margin(&self, value: f32, cx: &mut Context<Self>) {
        let value = value.clamp(READER_SPACING_MIN, READER_SPACING_MAX);
        if ReaderSettings::update(cx, |preferences| preferences.horizontal_margin_px = value) {
            notify(cx, Component::ReaderShell, "horizontal_margin_changed");
        }
    }

    pub(in crate::epub) fn set_vertical_margin(&self, value: f32, cx: &mut Context<Self>) {
        let value = value.clamp(READER_SPACING_MIN, READER_SPACING_MAX);
        if ReaderSettings::update(cx, |preferences| preferences.vertical_margin_px = value) {
            notify(cx, Component::ReaderShell, "vertical_margin_changed");
        }
    }

    pub(in crate::epub) fn set_min_column_gap(&self, value: f32, cx: &mut Context<Self>) {
        let value = value.clamp(READER_SPACING_MIN, READER_SPACING_MAX);
        if ReaderSettings::update(cx, |preferences| preferences.min_column_gap_px = value) {
            notify(cx, Component::ReaderShell, "column_gap_changed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, IntoElement, Render, TestAppContext, prelude::*, px, size};
    use skrift_column_paginator::{RendererInitialConfig, ResourceProvider};
    use html_view_gpui::HtmlView;
    use std::{io, sync::Arc};
    use ui_components as components;

    struct TextBook;

    impl ResourceProvider for TextBook {
        fn read_bytes(&self, _: &str) -> io::Result<Vec<u8>> {
            Ok(format!("<html><body style='margin:0'><p style='margin:0;text-align:justify'>{}</p></body></html>", "A line of text should fit completely inside the visible reading area. ".repeat(100)).into_bytes())
        }
        fn exists(&self, _: &str) -> bool {
            true
        }
        fn resolve(&self, _: &str, href: &str) -> String {
            href.to_owned()
        }
        fn list_html_candidates(&self, _: &str) -> io::Result<Vec<String>> {
            Ok(vec!["chapter.html".into()])
        }
    }

    struct ReadingPage {
        document: Entity<HtmlView>,
        inset: f32,
    }

    #[test]
    fn line_length_and_gap_choose_columns_that_fit_kobo_orientations() {
        let preferences = ReaderPreferences { font_size: 20.0, min_line_width_ch: 35.0, max_line_width_ch: 50.0, ..Default::default() };
        let (minimum, maximum) = line_width_range_px(&preferences);
        let portrait = fitted_columns_and_width(628.0, 32.0, u8::MAX, minimum, maximum);
        let landscape = fitted_columns_and_width(836.0, 32.0, u8::MAX, minimum, maximum);
        let wider_gap = fitted_columns_and_width(836.0, 64.0, u8::MAX, minimum, maximum);
        assert_eq!(portrait, (1, 500.0));
        assert_eq!(landscape, (2, 401.0));
        assert_eq!(wider_gap, (2, 385.0));
        assert_eq!(fitted_columns_and_width(836.0, 160.0, u8::MAX, minimum, maximum), (1, 500.0));
    }

    impl Render for ReadingPage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            components::reader_document_body().child(components::reader_content().px(px(self.inset)).child(self.document.clone()))
        }
    }

    #[gpui::test]
    fn kobo_column_plan_fits_the_actual_padded_document(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        // Libra 2: 1264 physical pixels at scale 2, with a 24px saved margin.
        // Previously the plan used 584px while the guarded view was only 580px.
        for (margin, edge, expected_width) in [(24.0, KOBO_READER_EDGE_INSET, 580.0), (0.0, KOBO_READER_EDGE_INSET, 628.0), (24.0, 0.0, 584.0)] {
            let (inset, available_width) = horizontal_insets(632.0, margin, edge);
            let (columns, column_width) = fitted_columns_and_width(f64::from(available_width), 32.0, 1, 600.0, 1200.0);
            let (page, visual) = cx.add_window_view(|window, cx| {
                let config = RendererInitialConfig { column_width, max_column_count: Some(columns), ..Default::default() };
                let document = cx.new(|cx| HtmlView::from_provider(Arc::new(TextBook), vec!["chapter.html".into()], 0, None, config, window, cx));
                document.update(cx, |view, cx| view.apply(RendererCommand::SetColumnViewportPlan { available_width: f64::from(available_width), column_width, column_count: columns, column_gap: 32.0 }, cx));
                ReadingPage { document, inset }
            });
            visual.simulate_resize(size(px(632.0), px(840.0)));
            visual.run_until_parked();
            let document = page.read_with(visual, |page, _| page.document.clone());
            document.read_with(visual, |document, _| {
                let geometry = document.layout_geometry().expect("HTML viewport was prepared");
                assert_eq!(geometry.viewport_width, expected_width);
                assert_eq!(geometry.column_count, 1);
                assert!(geometry.column_width <= geometry.viewport_width, "text columns must fit the actual painted viewport: {geometry:?}");
            });
        }
    }
}
