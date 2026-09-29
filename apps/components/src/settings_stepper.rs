//! One stepper shape for browsing and reading preferences.

use gpui::prelude::*;
use gpui::{Div, ElementId, SharedString, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Disableable, Sizable};

use crate::{BrowserTheme, RADIUS_SM, SETTINGS_CONTROL_SIZE_REM, SETTINGS_STEPPER_VALUE_WIDTH_REM, SPACE_XXS, TEXT_SM, base_button};

/// A borderless nudge button inside the stepper's single outer outline.
pub fn settings_stepper_button(id: impl Into<ElementId>, label: impl Into<SharedString>, disabled: bool, theme: BrowserTheme) -> Button {
    base_button(id)
        .label(label)
        .with_size(gpui_component::Size::Small)
        .ghost()
        .disabled(disabled)
        .w(gpui::rems(SETTINGS_CONTROL_SIZE_REM))
        .h(gpui::rems(SETTINGS_CONTROL_SIZE_REM))
        .px_0()
        .rounded(px(0.0))
        .text_size(gpui::rems(TEXT_SM))
        .text_color(theme.text)
}

/// A single square outline around decrement, value, and increment.
pub fn settings_stepper(decrement: Button, value: impl Into<SharedString>, increment: Button, theme: BrowserTheme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(gpui::rems(SETTINGS_CONTROL_SIZE_REM))
        .px(px(SPACE_XXS))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(theme.border)
        .bg(theme.page_bg)
        .child(decrement)
        .child(div().w(gpui::rems(SETTINGS_STEPPER_VALUE_WIDTH_REM)).flex().items_center().justify_center().text_align(gpui::TextAlign::Center).text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(value.into()))
        .child(increment)
}
