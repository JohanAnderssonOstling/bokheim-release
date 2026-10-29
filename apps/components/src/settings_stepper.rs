//! One stepper shape for browsing and reading preferences.

use gpui::prelude::*;
use gpui::{Div, ElementId, SharedString, div, px, rems};
use crate::Button;
use gpui_component::Disableable;

use crate::{BrowserTheme, RADIUS_SM, SETTINGS_STEPPER_VALUE_WIDTH_REM, SPACE_XXS, TEXT_SM, outlined_text_button, settings_control_size_rem};

/// A borderless nudge button inside the stepper's single outer outline.
pub fn settings_stepper_button(id: impl Into<ElementId>, label: impl Into<SharedString>, disabled: bool) -> Button {
    outlined_text_button(id, label)
        .ghost()
        .disabled(disabled)
        .size(rems(settings_control_size_rem()))
        .px_0()
        .rounded(px(0.0))
        .text_size(rems(TEXT_SM))
}

/// A single square outline around decrement, value, and increment.
pub fn settings_stepper(decrement: Button, value: impl Into<SharedString>, increment: Button, theme: BrowserTheme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(rems(settings_control_size_rem()))
        .px(px(SPACE_XXS))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(theme.border)
        .bg(theme.page_bg)
        .child(decrement)
        .child(div().w(rems(SETTINGS_STEPPER_VALUE_WIDTH_REM)).flex().items_center().justify_center().text_align(gpui::TextAlign::Center).text_size(rems(TEXT_SM)).text_color(theme.text_muted).child(value.into()))
        .child(increment)
}
