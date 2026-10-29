//! The reading ruler settings UI shared by HTML and PDF readers.

use gpui::{Context, Div, div, prelude::*};
use ui_components as components;

use super::settings_navigation::{SettingsNavigation, SettingsRow};

/// Each renderer supplies its own style type and preference setters. Labels,
/// controls, limits, and keyboard row highlighting are defined here once.
pub(crate) struct ReadingRulerSettings<C: 'static, S> {
    pub current: S,
    pub off: S,
    /// Band, Focus, and Lines, in their displayed order.
    pub styles: [S; 3],
    pub lines: usize,
    pub set_enabled: fn(&C, bool, &mut Context<C>),
    pub set_style: fn(&C, S, &mut Context<C>),
    pub set_lines: fn(&C, usize, &mut Context<C>),
}

pub(crate) fn append_rows(rows: &mut Vec<SettingsRow>, enabled: bool) {
    rows.push(SettingsRow::ReadingRuler);
    if enabled {
        rows.extend([SettingsRow::ReadingRulerStyle, SettingsRow::ReadingRulerLines]);
    }
}

pub(crate) fn render<C: 'static, S: Copy + PartialEq + 'static>(settings: ReadingRulerSettings<C, S>, navigation: &SettingsNavigation, rows: &[SettingsRow], theme: components::BrowserTheme, cx: &mut Context<C>) -> Div {
    let ReadingRulerSettings { current, off, styles, lines, set_enabled, set_style, set_lines } = settings;
    let enabled = current != off;
    let checkbox = components::reader_setting_checkbox("reader-ruler-enabled", "Reading ruler", enabled, theme).on_click(cx.listener(move |reader, enabled, _, cx| set_enabled(reader, *enabled, cx)));
    let group = components::reader_settings_group(theme).child(navigation.row(SettingsRow::ReadingRuler, rows, div().w_full().child(checkbox), theme));
    if !enabled {
        return group;
    }
    let choices = styles
        .into_iter()
        .zip(["Band", "Focus", "Lines"])
        .enumerate()
        .map(|(index, (style, label))| components::settings_choice_button(("reader-ruler-style", index), label, current == style, theme).on_click(cx.listener(move |reader, _, _, cx| set_style(reader, style, cx))));
    let style = components::reader_setting_row_smallcaps("Ruler style", theme).child(components::reader_choice_button_group("reader-ruler-style", choices, theme));
    let lines = lines.clamp(1, 5);
    let count = components::reader_setting_row("Ruler lines").child(components::settings_stepper(
        components::settings_stepper_button("reader-ruler-fewer-lines", "−", lines == 1).on_click(cx.listener(move |reader, _, _, cx| set_lines(reader, lines.saturating_sub(1), cx))),
        lines.to_string(),
        components::settings_stepper_button("reader-ruler-more-lines", "+", lines == 5).on_click(cx.listener(move |reader, _, _, cx| set_lines(reader, lines + 1, cx))),
        theme,
    ));
    group.child(navigation.row(SettingsRow::ReadingRulerStyle, rows, style, theme)).child(navigation.row(SettingsRow::ReadingRulerLines, rows, count, theme))
}
