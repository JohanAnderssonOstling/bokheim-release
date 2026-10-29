use crate::{Icon, IconName, Theme};
use gpui::{div, prelude::FluentBuilder, px, Div, Hsla, InteractiveElement, Styled};

/// Semantic colors shared by popup choices and application-owned sheet rows.
/// Containers own spacing and interaction; chosen values use a solid accent
/// with contrasting text, while hover and keyboard focus use a soft highlight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChoiceRowColors {
    pub foreground: Hsla,
    pub disabled_foreground: Hsla,
    pub selected_background: Hsla,
    pub selected_foreground: Hsla,
    pub hover_background: Hsla,
}

impl From<&Theme> for ChoiceRowColors {
    fn from(theme: &Theme) -> Self {
        Self {
            foreground: theme.foreground,
            disabled_foreground: theme.muted_foreground,
            selected_background: *theme.tokens.primary,
            selected_foreground: theme.primary_foreground,
            hover_background: *theme.tokens.accent,
        }
    }
}

/// Shared selection, hover and keyboard treatment. Hovering a chosen value
/// preserves its highlight; focusing another row never moves its checkmark.
pub fn choice_row(checked: bool, active: bool, disabled: bool, colors: ChoiceRowColors) -> Div {
    div()
        .flex()
        .items_center()
        .text_color(if checked {
            colors.selected_foreground
        } else if disabled {
            colors.disabled_foreground
        } else {
            colors.foreground
        })
        .when(checked, |row| row.bg(colors.selected_background))
        .when(active && !checked && !disabled, |row| {
            row.bg(colors.hover_background)
        })
        .when(disabled && checked, |row| row.opacity(0.5))
        .when(!disabled, |row| {
            row.hover(move |style| {
                style.bg(if checked {
                    colors.selected_background
                } else {
                    colors.hover_background
                })
            })
        })
}

/// The same selection indicator in popups and sheets. Place it after the
/// choice's label and any descriptive content, before separate row actions.
pub fn choice_row_checkmark(colors: ChoiceRowColors) -> Icon {
    Icon::new(IconName::Check)
        .size(px(15.0))
        .text_color(colors.selected_foreground)
}
