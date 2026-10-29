//! Row selection and immediate arrow-key editing in reader settings.

use std::{cell::Cell, rc::Rc};

use gpui::{App, Context, Div, MouseButton, Subscription, Window, canvas, div, prelude::*, px};
use ui_components as components;

pub(crate) const DESKTOP_FULLSCREEN: bool = cfg!(all(not(feature = "mobile"), any(target_os = "linux", target_os = "macos", target_os = "windows")));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SettingsRow {
    #[default]
    Theme,
    Fullscreen,
    Typeface,
    CjkTypeface,
    Alignment,
    FontSize,
    FontWeight,
    WordSpacing,
    Hyphenation,
    LineHeightMode,
    LineHeight,
    ParagraphSpacingMode,
    ParagraphSpacing,
    FirstLineIndentMode,
    FirstLineIndent,
    ReadingRuler,
    ReadingRulerStyle,
    ReadingRulerLines,
    SideMargins,
    VerticalMargins,
    ColumnGap,
    LineLength,
    PdfMargins,
    PdfFit,
    PdfPages,
    PdfZoom,
}

#[derive(Default)]
pub(crate) struct SettingsNavigation {
    selected: Rc<Cell<SettingsRow>>,
    scroll_pending: Rc<Cell<bool>>,
    keyboard_visible: Rc<Cell<bool>>,
    _window_changes: Option<Subscription>,
}

impl SettingsNavigation {
    pub(crate) fn new<C: 'static>(window: &mut Window, cx: &mut Context<C>) -> Self {
        let window_changes = DESKTOP_FULLSCREEN.then(|| {
            let mut fullscreen = window.is_fullscreen();
            cx.observe_window_bounds(window, move |_, window, cx| {
                // Native fullscreen transitions finish asynchronously and can
                // also be triggered by the window manager.
                let current = window.is_fullscreen();
                if current != fullscreen {
                    fullscreen = current;
                    cx.notify();
                }
            })
        });
        Self { _window_changes: window_changes, ..Self::default() }
    }

    pub(crate) fn fullscreen_group(&self, rows: &[SettingsRow], window: &Window, theme: components::BrowserTheme) -> Option<Div> {
        DESKTOP_FULLSCREEN.then(|| {
            let checkbox = components::reader_setting_checkbox("reader-toggle-fullscreen", "Full screen", window.is_fullscreen(), theme).on_click(|enabled, window, _| set_fullscreen(*enabled, window));
            let row = div().w_full().child(checkbox);
            components::reader_settings_group(theme).child(self.row(SettingsRow::Fullscreen, rows, row, theme))
        })
    }

    pub(crate) fn keyboard_visibility(&self) -> Rc<Cell<bool>> {
        self.keyboard_visible.clone()
    }

    pub(crate) fn keyboard_visible(&self) -> bool {
        self.keyboard_visible.get()
    }

    pub(crate) fn selected(&self, rows: &[SettingsRow]) -> SettingsRow {
        if rows.contains(&self.selected.get()) { self.selected.get() } else { rows[0] }
    }

    pub(crate) fn move_row(&mut self, rows: &[SettingsRow], direction: i32) {
        let index = rows.iter().position(|row| *row == self.selected(rows)).unwrap_or(0);
        let next = (index as i32 + direction).clamp(0, rows.len() as i32 - 1) as usize;
        self.selected.set(rows[next]);
        self.scroll_pending.set(true);
    }

    /// Marks the selected row and scrolls it into view after keyboard movement.
    pub(crate) fn row(&self, row: SettingsRow, rows: &[SettingsRow], content: Div, theme: components::BrowserTheme) -> Div {
        let selected = self.keyboard_visible() && self.selected(rows) == row;
        let selection = self.selected.clone();
        let pending = self.scroll_pending.clone();
        content
            .relative()
            .on_mouse_down(MouseButton::Left, move |_, _, _| selection.set(row))
            .when(selected, |content| content.child(div().debug_selector(move || format!("reader-settings-marker-{row:?}")).absolute().left(px(-components::CONTENT_INSET)).top_0().bottom_0().w(px(2.0)).bg(theme.accent)))
            .child(
                canvas(
                    move |bounds, window, _| {
                        if selected && pending.replace(false) {
                            window.request_autoscroll(bounds);
                        }
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }
}

/// Left exits fullscreen and right enters it, stopping at either state.
pub(crate) fn set_fullscreen(fullscreen: bool, window: &Window) {
    if window.is_fullscreen() != fullscreen {
        window.toggle_fullscreen();
    }
}

/// Choice rows stop at their ends, like numeric settings stop at their bounds.
pub(crate) fn adjacent_value<T: Copy + PartialEq>(values: &[T], current: T, direction: i32) -> T {
    let index = values.iter().position(|value| *value == current).unwrap_or(0) as i32;
    values[(index + direction).clamp(0, values.len() as i32 - 1) as usize]
}

/// The tile one step from `current` on an ordered row of steps. A stored value
/// that falls between two steps moves to the nearer step in the direction
/// pressed, so the first press always lands on a tile.
pub(crate) fn adjacent_step<T: Copy + PartialOrd>(steps: &[T], current: T, direction: i32) -> T {
    if direction > 0 { steps.iter().copied().find(|step| *step > current).unwrap_or(steps[steps.len() - 1]) } else { steps.iter().rev().copied().find(|step| *step < current).unwrap_or(steps[0]) }
}

pub(crate) fn adjust_theme(direction: i32, cx: &mut App) {
    let choices = components::THEME_CHOICES;
    let current = cx.try_global::<components::theme::SelectedPalette>().map(|selection| (selection.0, selection.1)).unwrap_or(choices[0]);
    let (tint, appearance) = adjacent_value(&choices, current, direction);
    components::theme::select_palette(tint, appearance, cx);
}

#[cfg(test)]
mod tests {
    use super::{SettingsNavigation, SettingsRow, adjacent_value};

    #[test]
    fn row_selection_clamps_and_handles_conditional_rows() {
        let mut navigation = SettingsNavigation::default();
        let rows = [SettingsRow::PdfMargins, SettingsRow::PdfFit, SettingsRow::PdfPages];
        assert_eq!(navigation.selected(&rows), SettingsRow::PdfMargins);
        navigation.move_row(&rows, -1);
        assert_eq!(navigation.selected(&rows), SettingsRow::PdfMargins);
        navigation.move_row(&rows, 1);
        navigation.move_row(&rows, 1);
        navigation.move_row(&rows, 1);
        assert_eq!(navigation.selected(&rows), SettingsRow::PdfPages);
        let changed = [SettingsRow::PdfMargins, SettingsRow::PdfFit, SettingsRow::PdfZoom];
        assert_eq!(navigation.selected(&changed), SettingsRow::PdfMargins);
    }

    #[test]
    fn arrow_choices_apply_the_adjacent_value_and_stop_at_limits() {
        let values = [10, 20, 30];
        assert_eq!(adjacent_value(&values, 20, -1), 10);
        assert_eq!(adjacent_value(&values, 20, 1), 30);
        assert_eq!(adjacent_value(&values, 10, -1), 10);
        assert_eq!(adjacent_value(&values, 30, 1), 30);
    }
}
