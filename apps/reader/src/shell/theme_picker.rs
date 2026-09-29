//! The application palette control shared by EPUB and PDF settings.

use gpui::prelude::*;
use gpui::{Context, Div};
use ui_components as components;

pub(crate) fn reader_theme_picker<C: 'static>(cx: &mut Context<C>, theme: components::BrowserTheme) -> Div {
    let selected = cx.try_global::<components::theme::SelectedPalette>().map(|selection| (selection.0, selection.1));
    // Full width so the last swatch lands on the steppers' right edge below;
    // justify_between in the group spreads the fixed-size swatches across it.
    let mut picker = components::theme_picker_group().w_full();
    for (index, (tint, appearance)) in components::THEME_CHOICES.into_iter().enumerate() {
        picker = picker.child(components::theme_picker_swatch(format!("reader-theme-{index}"), tint, appearance, components::theme_choice_selected(selected, tint, appearance)).on_click(move |_, _, cx| {
            components::theme::select_palette(tint, appearance, cx);
        }));
    }
    components::reader_settings_group(theme).child(components::reader_setting_row_smallcaps("Theme", theme).child(picker))
}
