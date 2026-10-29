//! The application palette control shared by EPUB and PDF settings.

use gpui::prelude::*;
use gpui::{Context, Div, div, px, rems};
use ui_components as components;

pub(crate) fn reader_theme_picker<C: 'static>(cx: &mut Context<C>, theme: components::BrowserTheme) -> Div {
    let selected = cx.try_global::<components::theme::SelectedPalette>().map(|selection| (selection.0, selection.1));
    let mut picker = components::theme_picker_group();
    for (index, (tint, appearance)) in components::THEME_CHOICES.into_iter().enumerate() {
        picker = picker.child(components::theme_picker_swatch(format!("reader-theme-{index}"), tint, appearance, components::theme_choice_selected(selected, tint, appearance)).on_click(move |_, _, cx| {
            components::theme::select_palette(tint, appearance, cx);
        }));
    }
    div().w_full().flex().flex_wrap().items_center().justify_between().gap(px(components::SPACE_SM)).child(div().flex_none().text_size(rems(components::TEXT_SM)).text_color(theme.text).child("Theme")).child(picker)
}
