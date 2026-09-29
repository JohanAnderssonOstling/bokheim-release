//! EPUB appearance preferences and matching renderer commands.

use app_preferences::{COLUMN_COUNT_MAX, COLUMN_COUNT_MIN, FONT_SIZE_MAX, FONT_SIZE_MIN, LINE_HEIGHT_MAX, LINE_HEIGHT_MIN, LINE_WIDTH_MAX, LINE_WIDTH_MIN, READER_SPACING_MAX, READER_SPACING_MIN, ReaderFontFamily, ReaderTextAlignment};
use gpui::{Context, Window};
use html_view_core::RendererCommand;

use crate::epub::ReaderView;
use crate::invalidation::Component;
use crate::settings::{ReaderPreferences, ReaderSettings, SidebarTab};
use crate::shell::chrome;

/// The layout engine's fallback `ch` metric is half an em. Keeping this
/// conversion here makes the preference typographic rather than tied to a
/// particular screen's pixels, while the layout itself continues to receive
/// its required pixel width.
const CH_ADVANCE_EM: f64 = 0.5;

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
        let margin = ReaderSettings::preferences(cx).horizontal_margin_px.min(((document_width - 100.0) / 2.0).max(0.0));
        (sidebar_width, margin, (document_width - margin * 2.0).max(1.0))
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

    /// Mobile owns the viewport width, so its user-facing layout preference
    /// is a column count. Recalculate the underlying width whenever that
    /// viewport changes; do not persist a meaningless phone pixel width.
    pub(in crate::epub) fn sync_mobile_column_width(&self, available_width: f64, cx: &mut Context<Self>) {
        if !cfg!(target_os = "android") {
            return;
        }
        let width = self.fitted_column_width(available_width, cx);
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetColumnWidth(width), cx));
    }

    /// Persists a preference, applies its renderer command, and refreshes the
    /// reader only when the value actually changed.
    fn apply_appearance(&self, cx: &mut Context<Self>, update: impl FnOnce(&mut ReaderPreferences), command: impl FnOnce(&ReaderPreferences) -> RendererCommand) {
        if !ReaderSettings::update(cx, update) {
            return;
        }
        let command = command(ReaderSettings::preferences(cx));
        self.with_renderer(cx, |renderer, cx| renderer.apply(command, cx));
        crate::invalidation::notify(cx, Component::ReaderShell, "preferences_changed");
    }

    fn apply_style_override(&self, cx: &mut Context<Self>, update: impl FnOnce(&mut ReaderPreferences)) {
        // Read before the update: the overrides carry the theme's two lightness
        // ends, and those come from the application rather than from any reader
        // preference.
        let theme = crate::settings::library_theme_adaptation(cx);
        self.apply_appearance(cx, update, move |preferences| RendererCommand::SetReaderStyleOverrides(preferences.style_overrides(Some(theme))));
    }

    pub(in crate::epub) fn adjust_font_size(&self, delta: f32, cx: &mut Context<Self>) {
        let font_size = (ReaderSettings::preferences(cx).font_size + delta).clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
        self.apply_appearance(cx, |preferences| preferences.font_size = font_size, |_| RendererCommand::SetFontSize(font_size));
    }

    pub(in crate::epub) fn set_reader_font_family(&self, font_family: ReaderFontFamily, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.font_family = font_family);
    }

    pub(in crate::epub) fn set_reader_text_alignment(&self, text_alignment: ReaderTextAlignment, cx: &mut Context<Self>) {
        self.apply_style_override(cx, |preferences| preferences.text_alignment = text_alignment);
    }

    pub(in crate::epub) fn adjust_line_height(&self, delta: f32, cx: &mut Context<Self>) {
        let line_height = (ReaderSettings::preferences(cx).line_height + delta).clamp(LINE_HEIGHT_MIN, LINE_HEIGHT_MAX);
        self.apply_style_override(cx, |preferences| preferences.line_height = line_height);
    }

    pub(in crate::epub) fn adjust_desired_column_count(&mut self, delta: i8, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(feature = "kobo") {
            return;
        }
        let count = (i16::from(self.desired_column_count) + i16::from(delta)).clamp(i16::from(COLUMN_COUNT_MIN), i16::from(COLUMN_COUNT_MAX)) as u8;
        // Clicking at a count limit also refits the current count after a
        // window or sidebar resize.
        self.desired_column_count = count;
        let sidebar_visible = (self.chrome.is_visible() || self.search.query.is_some()) && self.active_sidebar_tab(window).is_some();
        let (_, _, available_width) = Self::horizontal_layout(window, cx, sidebar_visible);
        if cfg!(target_os = "android") {
            ReaderSettings::update(cx, |preferences| preferences.max_column_count = count);
            self.sync_mobile_column_width(f64::from(available_width), cx);
            crate::invalidation::notify(cx, Component::ReaderShell, "mobile_column_count_changed");
            return;
        }
        let width = self.fitted_column_width(f64::from(available_width), cx);
        ReaderSettings::update(cx, |preferences| {
            preferences.max_column_count = count;
            preferences.column_width = width;
        });
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetColumnWidth(width), cx));
        crate::invalidation::notify(cx, Component::ReaderShell, "column_count_changed");
    }

    pub(in crate::epub) fn adjust_column_width(&self, delta: f64, window: &Window, cx: &mut Context<Self>) {
        // Keep the existing keyboard shortcuts useful: they shift both ends
        // of the typographic range by five characters.
        self.apply_line_width_range(window, cx, |minimum, maximum| {
            *minimum = (*minimum + delta.signum() * 5.0).clamp(20.0, 100.0);
            *maximum = (*maximum + delta.signum() * 5.0).clamp(20.0, 100.0);
        });
    }

    pub(in crate::epub) fn adjust_min_line_width_ch(&self, delta: f64, window: &Window, cx: &mut Context<Self>) {
        self.apply_line_width_range(window, cx, |minimum, maximum| {
            *minimum = (*minimum + delta).clamp(20.0, 100.0);
            if *minimum > *maximum {
                *maximum = *minimum;
            }
        });
    }

    pub(in crate::epub) fn adjust_max_line_width_ch(&self, delta: f64, window: &Window, cx: &mut Context<Self>) {
        self.apply_line_width_range(window, cx, |minimum, maximum| {
            *maximum = (*maximum + delta).clamp(20.0, 100.0);
            if *maximum < *minimum {
                *minimum = *maximum;
            }
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
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetColumnWidth(width), cx));
        crate::invalidation::notify(cx, Component::ReaderShell, "line_width_range_changed");
    }

    pub(in crate::epub) fn adjust_horizontal_margin(&self, delta: f32, cx: &mut Context<Self>) {
        let value = (ReaderSettings::preferences(cx).horizontal_margin_px + delta).clamp(READER_SPACING_MIN, READER_SPACING_MAX);
        if ReaderSettings::update(cx, |preferences| preferences.horizontal_margin_px = value) {
            crate::invalidation::notify(cx, Component::ReaderShell, "horizontal_margin_changed");
        }
    }

    pub(in crate::epub) fn adjust_vertical_margin(&self, delta: f32, cx: &mut Context<Self>) {
        let value = (ReaderSettings::preferences(cx).vertical_margin_px + delta).clamp(READER_SPACING_MIN, READER_SPACING_MAX);
        if ReaderSettings::update(cx, |preferences| preferences.vertical_margin_px = value) {
            crate::invalidation::notify(cx, Component::ReaderShell, "vertical_margin_changed");
        }
    }

    pub(in crate::epub) fn adjust_min_column_gap(&self, delta: f32, cx: &mut Context<Self>) {
        let value = (ReaderSettings::preferences(cx).min_column_gap_px + delta).clamp(READER_SPACING_MIN, READER_SPACING_MAX);
        if ReaderSettings::update(cx, |preferences| preferences.min_column_gap_px = value) {
            crate::invalidation::notify(cx, Component::ReaderShell, "column_gap_changed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{column_count_for_gap, fitted_columns_and_width};

    #[test]
    fn minimum_gap_reduces_columns_when_the_viewport_cannot_fit_them() {
        assert_eq!(column_count_for_gap(1200.0, 600.0, 0.0, 2), 2);
        assert_eq!(column_count_for_gap(1200.0, 600.0, 20.0, 2), 1);
        assert_eq!(column_count_for_gap(1240.0, 600.0, 0.0, 2), 2);
        assert_eq!(column_count_for_gap(1240.0, 600.0, 20.0, 2), 2);
        assert_eq!(column_count_for_gap(1500.0, 600.0, 100.0, 4), 2);
    }

    #[test]
    fn fit_width_survives_renderer_rounding_and_a_fractionally_smaller_document() {
        let (count, fitted) = fitted_columns_and_width(1940.0, 0.0, 2, 450.0, 650.0);
        assert_eq!((count, fitted), (2, 650.0));

        let (count, fitted) = fitted_columns_and_width(1940.0, 80.0, 2, 450.0, 650.0);
        assert_eq!((count, fitted), (2, 650.0));
    }

    #[test]
    fn narrow_viewport_fits_even_when_below_preferred_line_width() {
        let (count, fitted) = fitted_columns_and_width(390.0, 0.0, 2, 450.0, 650.0);
        assert_eq!((count, fitted), (1, 389.0));
    }
}
