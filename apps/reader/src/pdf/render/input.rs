//! PDF implementations of shared navigation commands.

use crate::shell::annotation_editor::AnnotationEditorTarget;

use gpui::{App, Context, Keystroke, Window};
use ui_components as components;

use crate::shell::input::CommandTarget;
use crate::shell::state_panel::SidebarTab;
use crate::shell::toolbar::SidebarTarget;
use crate::{DecreaseScale, FocusReaderSearch, IncreaseScale, NextLine, NextPage, PreviousLine, PreviousPage, ToggleSearch, ToggleToc};

use super::{PdfReaderView, ZOOM_STEP};

impl CommandTarget<PreviousPage> for PdfReaderView {
    fn execute(&mut self, _: &PreviousPage, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome.hide_for_motion(window, cx);
        if self.pdf.read(cx).can_previous_page() {
            self.pdf.update(cx, |pdf, cx| pdf.previous_page(cx));
        }
    }
}

impl CommandTarget<NextPage> for PdfReaderView {
    fn execute(&mut self, _: &NextPage, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome.hide_for_motion(window, cx);
        if self.pdf.read(cx).can_next_page() {
            self.pdf.update(cx, |pdf, cx| pdf.next_page(cx));
        }
    }
}

impl CommandTarget<PreviousLine> for PdfReaderView {
    fn execute(&mut self, _: &PreviousLine, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome.hide_for_motion(window, cx);
        self.pdf.update(cx, |pdf, cx| {
            if !pdf.step_reading_ruler(false, cx) { pdf.previous_item(cx); }
        });
    }
}

impl CommandTarget<NextLine> for PdfReaderView {
    fn execute(&mut self, _: &NextLine, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome.hide_for_motion(window, cx);
        self.pdf.update(cx, |pdf, cx| {
            if !pdf.step_reading_ruler(true, cx) { pdf.next_item(cx); }
        });
    }
}

impl CommandTarget<ToggleToc> for PdfReaderView {
    fn execute(&mut self, _: &ToggleToc, window: &mut Window, cx: &mut Context<Self>) {
        self.select_sidebar_tab(SidebarTab::Contents, window, cx);
    }
}

impl CommandTarget<FocusReaderSearch> for PdfReaderView {
    fn execute(&mut self, _: &FocusReaderSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.select_sidebar_tab(SidebarTab::Search, window, cx);
    }
}

impl CommandTarget<ToggleSearch> for PdfReaderView {
    fn execute(&mut self, _: &ToggleSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.select_sidebar_tab(SidebarTab::Search, window, cx);
    }
}

impl PdfReaderView {
    pub(super) fn turn_horizontal(&mut self, towards_right: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome.hide_for_motion(window, cx);
        self.pdf.update(cx, |pdf, cx| pdf.horizontal_page(towards_right, cx));
    }

    pub(super) fn install_directional_page_keys(window: &mut Window, cx: &mut Context<Self>) {
        let target = cx.entity().downgrade();
        let window_handle = window.window_handle();
        window.use_keyed_state(("pdf-directional-page-keys", cx.entity_id()), cx, move |_, cx| {
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != window_handle || !event.context_stack.iter().any(|context| context.contains("PdfReaderDocument")) {
                    return;
                }
                let Some(towards_right) = horizontal_page_key(&event.keystroke, cx) else { return };
                let Some(target) = target.upgrade() else { return };
                let handled = target.update(cx, |reader, cx| {
                    if !reader.pdf.read(cx).right_to_left() {
                        return false;
                    }
                    reader.turn_horizontal(towards_right, window, cx);
                    true
                });
                if handled {
                    cx.stop_propagation();
                }
            })
        });
    }

    pub(crate) fn native_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_sheet.take().is_some() {
            cx.notify();
        } else if self.marks.selected().is_some() {
            self.dismiss_annotation_editor(window, cx);
        } else if self.sidebar.is_visible() && self.chrome.is_visible() {
            self.hide_sidebar(window, cx);
        } else {
            self.close_reader(window, cx);
        }
    }
}

/// Only physical arrows assigned to page navigation change with direction.
/// Page Up/Down, Space and custom next/previous commands keep document order.
fn horizontal_page_key(key: &Keystroke, cx: &App) -> Option<bool> {
    let modifiers = key.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift || !matches!(key.key.as_str(), "left" | "right") {
        return None;
    }
    (components::matches_shortcut("reader.previous_page", key, cx) || components::matches_shortcut("reader.next_page", key, cx)).then_some(key.key == "right")
}

impl CommandTarget<IncreaseScale> for PdfReaderView {
    fn execute(&mut self, _: &IncreaseScale, _: &mut Window, cx: &mut Context<Self>) {
        self.adjust_zoom(ZOOM_STEP, cx);
    }
}

impl CommandTarget<DecreaseScale> for PdfReaderView {
    fn execute(&mut self, _: &DecreaseScale, _: &mut Window, cx: &mut Context<Self>) {
        self.adjust_zoom(-ZOOM_STEP, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn directional_keys_respect_shortcut_overrides_and_leave_semantic_commands_alone(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::shell::input::bind_document_keys(cx, "PdfReaderDocument");
            assert_eq!(horizontal_page_key(&Keystroke::parse("left").unwrap(), cx), Some(false));
            assert_eq!(horizontal_page_key(&Keystroke::parse("right").unwrap(), cx), Some(true));
            for key in ["pagedown", "pageup", "space", "ctrl-left", "shift-right"] {
                assert_eq!(horizontal_page_key(&Keystroke::parse(key).unwrap(), cx), None);
            }
            components::apply_keyboard_shortcuts(&app_preferences::KeyboardShortcuts::from([("reader.previous_page".into(), vec!["p".into()]), ("reader.next_page".into(), vec!["n".into()])]), cx);
            assert_eq!(horizontal_page_key(&Keystroke::parse("left").unwrap(), cx), None);
            assert_eq!(horizontal_page_key(&Keystroke::parse("right").unwrap(), cx), None);
        });
    }
}
