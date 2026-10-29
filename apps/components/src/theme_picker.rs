//! Compact palette samples shared by Browse and Reader settings.

use app_preferences::{ApplicationTheme, ResolvedAppearance};
use gpui::prelude::*;
use gpui::{Div, ElementId, px, rems, rgb};
use crate::Button;
use gpui_component::{Icon, IconName};

use crate::theme::palette;

/// A swatch is a sample of text — "Aa" in the palette's ink — so the swatch and
/// everything drawn on it are in rems and grow with the UI font like the text
/// they sample.
const SWATCH_SIZE_REM: f32 = 3.0;
const MAX_GAP_REM: f32 = 1.0;

/// Each theme in each appearance: the two light palettes, then their dark
/// counterparts in the same order.
pub const THEME_CHOICES: [(ApplicationTheme, ResolvedAppearance); 4] =
    [(ApplicationTheme::Warm, ResolvedAppearance::Light), (ApplicationTheme::Cool, ResolvedAppearance::Light), (ApplicationTheme::Warm, ResolvedAppearance::Dark), (ApplicationTheme::Cool, ResolvedAppearance::Dark)];

/// Also the width of the settings choice groups, so the rows' controls end on
/// one line.
pub const THEME_PICKER_WIDTH_REM: f32 = THEME_CHOICES.len() as f32 * SWATCH_SIZE_REM + (THEME_CHOICES.len() - 1) as f32 * MAX_GAP_REM;

pub fn theme_choice_selected(selected: Option<(ApplicationTheme, ResolvedAppearance)>, theme: ApplicationTheme, appearance: ResolvedAppearance) -> bool {
    selected == Some((theme, appearance))
}

/// What a theme choice is called: its palette and its appearance.
pub fn theme_choice_label(theme: ApplicationTheme, appearance: ResolvedAppearance) -> String {
    format!("{} {}", theme.label(), if appearance == ResolvedAppearance::Dark { "dark" } else { "light" })
}

/// Fixed-size squares. The space between them expands with the panel,
/// stopping at `MAX_GAP_REM`; the samples themselves never stretch.
pub fn theme_picker_group() -> Div {
    gpui::div().w(rems(THEME_PICKER_WIDTH_REM)).max_w_full().flex().flex_row().items_center().justify_between()
}

pub fn theme_picker_swatch(id: impl Into<ElementId>, theme: ApplicationTheme, appearance: ResolvedAppearance, selected: bool) -> Button {
    let colors = palette(theme, appearance);
    let page = rgb(colors.surface_bg);
    let accent = rgb(colors.accent);
    let border = if selected { accent } else { rgb(colors.rule) };
    let label = theme_choice_label(theme, appearance);

    let letters = gpui::div().text_color(rgb(colors.ink_text)).text_size(rems(1.5)).child("Aa");

    let sample = gpui::div()
        .relative()
        .size_full()
        // Draw the palette sample's edge here: the ghost button's hover and
        // pressed styles replace its own border with a transparent one.
        .border_1()
        .border_color(border)
        .bg(page)
        .hover(move |style| style.border_color(if selected { accent } else { rgb(colors.border) }))
        .flex()
        .items_center()
        .justify_center()
        .child(letters)
        .child(gpui::div().absolute().bottom(rems(0.375)).left(rems(0.875)).w(rems(1.125)).h(px(3.0)).bg(accent))
        .when(selected, |sample| {
            sample.child(gpui::div().absolute().top(px(2.0)).right(px(2.0)).size(rems(0.875)).flex().items_center().justify_center().bg(accent).text_color(rgb(colors.accent_text)).child(Icon::new(IconName::Check).size(rems(0.625))))
        });

    crate::base_button(id).ghost().size(rems(SWATCH_SIZE_REM)).flex_none().p_0().rounded(px(0.0)).bg(page).tooltip(label).child(sample)
}
