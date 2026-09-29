//! Compact palette samples shared by Browse and Reader settings.

use app_preferences::{ApplicationTheme, ResolvedAppearance};
use gpui::prelude::*;
use gpui::{Div, ElementId, px, rems, rgb};
use gpui_component::button::{Button, ButtonVariants};

use crate::theme::palette;

/// A swatch is a sample of text — "Aa" in the palette's ink — so the swatch and
/// everything drawn on it are in rems and grow with the UI font like the text
/// they sample.
const SWATCH_SIZE_REM: f32 = 3.0;
const MAX_GAP_REM: f32 = 1.5;
/// Also the width of the settings choice groups, so the rows' controls end on
/// one line.
pub const THEME_PICKER_WIDTH_REM: f32 = 3.0 * SWATCH_SIZE_REM + 2.0 * MAX_GAP_REM;

pub const THEME_CHOICES: [(ApplicationTheme, ResolvedAppearance); 3] = [(ApplicationTheme::Warm, ResolvedAppearance::Light), (ApplicationTheme::Cool, ResolvedAppearance::Light), (ApplicationTheme::Warm, ResolvedAppearance::Dark)];

pub fn theme_choice_selected(selected: Option<(ApplicationTheme, ResolvedAppearance)>, theme: ApplicationTheme, appearance: ResolvedAppearance) -> bool {
    match (selected, appearance) {
        (Some((_, ResolvedAppearance::Dark)), ResolvedAppearance::Dark) => true,
        (Some(choice), _) => choice == (theme, appearance),
        (None, _) => false,
    }
}

/// Three fixed-size squares. The space between them expands with the panel,
/// stopping at `MAX_GAP_REM`; the samples themselves never stretch.
pub fn theme_picker_group() -> Div {
    gpui::div().w(rems(THEME_PICKER_WIDTH_REM)).max_w_full().flex().flex_row().items_center().justify_between()
}

pub fn theme_picker_swatch(id: impl Into<ElementId>, theme: ApplicationTheme, appearance: ResolvedAppearance, selected: bool) -> Button {
    let colors = palette(theme, appearance);
    let page = rgb(colors.surface_bg);
    let accent = rgb(colors.accent);
    let label = if appearance == ResolvedAppearance::Dark { "Dark".to_owned() } else { format!("{} light", theme.label()) };

    let letters = gpui::div().font_family(app_preferences::INTERFACE_FONT_FAMILY).text_color(rgb(colors.text_accent)).text_size(rems(1.5)).child("Aa");

    let sample = gpui::div()
        .relative()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(letters)
        .child(gpui::div().absolute().bottom(rems(0.375)).left(rems(0.875)).w(rems(1.125)).h(px(3.0)).bg(accent))
        .when(selected, |sample| sample.child(gpui::div().absolute().top(px(2.0)).right(px(2.0)).size(rems(0.875)).flex().items_center().justify_center().bg(accent).text_color(page).text_size(rems(0.625)).child("✓")));

    crate::base_button(id).ghost().size(rems(SWATCH_SIZE_REM)).flex_none().p_0().rounded(px(0.0)).bg(page).tooltip(label).when(selected, |button| button.border_1().border_color(accent)).child(sample)
}
