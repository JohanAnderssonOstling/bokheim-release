//! Choice controls shared by application and reader settings.

use gpui::prelude::*;
use gpui::{Div, ElementId, SharedString, div, px, rems};
use crate::Button;

use crate::{BrowserTheme, RADIUS_SM, SETTINGS_CONTROL_GAP, SPACE_SM, TEXT_SM, base_button, settings_control_size_rem};

/// One quiet outline with single-pixel separators between equal-width choices.
/// Callers choose the width to suit their settings surface.
pub fn settings_choice_button_group(theme: BrowserTheme) -> Div {
    div().min_w_0().flex().flex_row().flex_wrap().gap(px(SETTINGS_CONTROL_GAP)).overflow_hidden().rounded(px(RADIUS_SM)).border_1().border_color(theme.rule).bg(theme.rule)
}

/// The same outline as [`settings_choice_button_group`] laid out as a grid,
/// for a list of independent on/off choices too long for one row.
pub fn settings_choice_button_grid(columns: u16, theme: BrowserTheme) -> Div {
    div().w_full().grid().grid_cols(columns).gap(px(SETTINGS_CONTROL_GAP)).overflow_hidden().rounded(px(RADIUS_SM)).border_1().border_color(theme.rule).bg(theme.rule)
}

/// Chosen values share the heavy accent used by selected buttons.
/// Kobo retains its underline so changing a setting avoids a solid e-ink fill.
pub fn settings_choice_button(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    let label = label.into();
    let button = base_button(id)
        .child(div().min_w_0().max_w_full().whitespace_normal().text_align(gpui::TextAlign::Center).child(label))
        .ghost()
        .flex_1()
        .min_w_0()
        .h_auto()
        .min_h(rems(settings_control_size_rem()))
        .px(px(SPACE_SM))
        .rounded(px(0.0))
        .border_0()
        .bg(theme.page_bg)
        .text_size(rems(TEXT_SM));
    crate::selectable_button(button, selected, theme).when(selected && cfg!(feature = "kobo"), |button| button.border_0().border_b(px(2.0)))
}
