//! The same shortcut controls embedded in the settings of their owning page.

use std::{cell::Cell, rc::Rc};

use app_preferences::KeyboardShortcuts;
use gpui::prelude::*;
use gpui::{App, Bounds, Context, FocusHandle, Focusable, IntoElement, Keystroke, Pixels, Render, ScrollHandle, SharedString, Subscription, Window, px, rems};
use gpui_component::{Disableable, kbd::Kbd};

use crate::keyboard_settings::{keyboard_shortcut_overrides, save_keyboard_shortcuts};
use crate::{self as components, ShortcutPage, configured_shortcuts, shortcut_conflict, shortcut_settings};

const SHORTCUT_BUTTON_WIDTH_REM: f32 = 11.0;

struct ShortcutRow {
    focus: FocusHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

pub struct KeyboardShortcutEditor {
    page: ShortcutPage,
    capturing: Option<&'static str>,
    saving: Option<KeyboardShortcuts>,
    warning: Option<SharedString>,
    focus: FocusHandle,
    rows: Vec<ShortcutRow>,
    _capture: Subscription,
}

impl KeyboardShortcutEditor {
    pub fn new(page: ShortcutPage, cx: &mut Context<Self>) -> Self {
        let target = cx.weak_entity();
        let capture = cx.intercept_keystrokes(move |event, window, cx| {
            let Some(target) = target.upgrade() else { return };
            let editor = target.read(cx);
            if editor.capturing.is_none() || !editor.focus.contains_focused(window, cx) {
                return;
            }
            target.update(cx, |editor, cx| editor.capture_shortcut(&event.keystroke, cx));
            window.prevent_default();
            cx.stop_propagation();
        });
        Self { page, capturing: None, saving: None, warning: None, focus: cx.focus_handle(), rows: Vec::new(), _capture: capture }
    }

    pub fn cancel_capture(&mut self, cx: &mut Context<Self>) {
        self.capturing = None;
        self.warning = None;
        cx.notify();
    }

    pub fn reveal_focused_row(&self, scroll: &ScrollHandle, window: &Window, cx: &App) -> bool {
        self.rows.iter().find(|row| row.focus.contains_focused(window, cx)).is_some_and(|row| components::scroll_to_reveal(scroll, row.bounds.get(), px(16.0)))
    }

    fn save(&mut self, shortcuts: KeyboardShortcuts, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        self.capturing = None;
        self.warning = None;
        self.saving = Some(shortcuts.clone());
        let saved = save_keyboard_shortcuts(shortcuts, cx);
        cx.spawn(async move |editor, cx| {
            let result = saved.await;
            let _ = editor.update(cx, |editor, cx| {
                editor.saving = None;
                editor.warning = result.err().map(|error| format!("Keyboard shortcuts were not saved. {}", components::user_facing_error(&*error, "Please try again.")).into());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn capture_shortcut(&mut self, key: &Keystroke, cx: &mut Context<Self>) {
        let Some(id) = self.capturing else { return };
        if key.key == "escape" && key.modifiers == gpui::Modifiers::default() {
            self.cancel_capture(cx);
            return;
        }
        if matches!(key.key.as_str(), "control" | "ctrl" | "alt" | "shift" | "super" | "cmd" | "command" | "meta" | "platform" | "fn" | "function") {
            return;
        }
        let shortcut = key.unparse();
        #[cfg(target_arch = "wasm32")]
        let shortcut = if key.modifiers.platform { format!("cmd-{shortcut}") } else { shortcut };
        let settings = shortcut_settings(cx);
        let mut overrides = keyboard_shortcut_overrides(cx);
        overrides.insert(id.to_owned(), Vec::new());
        if let Err(warning) = shortcut_conflict(&settings, &overrides, id, None, &shortcut) {
            self.warning = Some(warning.into());
            cx.notify();
            return;
        }
        overrides.insert(id.to_owned(), vec![shortcut]);
        self.save(overrides, cx);
    }
}

impl Focusable for KeyboardShortcutEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for KeyboardShortcutEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let settings: Vec<_> = shortcut_settings(cx).into_iter().filter(|setting| setting.page == self.page).collect();
        // Preview the submitted keys while saving, without dimming or moving
        // the whole form. A failed save falls back to the persisted bindings.
        let overrides = self.saving.clone().unwrap_or_else(|| keyboard_shortcut_overrides(cx));
        while self.rows.len() < settings.len() {
            self.rows.push(ShortcutRow { focus: cx.focus_handle(), bounds: Rc::new(Cell::new(Bounds::default())) });
        }
        let restore = components::browser_settings_header_button(format!("restore-{}-shortcuts", self.page.label()), "Restore defaults", false, theme)
            .w(rems(SHORTCUT_BUTTON_WIDTH_REM))
            .max_w_full()
            .disabled(!settings.iter().any(|setting| overrides.contains_key(setting.id)))
            .on_click(cx.listener(|editor, _, _, cx| {
                if editor.saving.is_some() {
                    return;
                }
                let mut overrides = keyboard_shortcut_overrides(cx);
                for setting in shortcut_settings(cx).iter().filter(|setting| setting.page == editor.page) {
                    overrides.remove(setting.id);
                }
                editor.save(overrides, cx);
            }));
        let mut group = components::browser_settings_group();
        for (row, setting) in settings.iter().enumerate() {
            let id = setting.id;
            let keys = configured_shortcuts(setting, &overrides);
            let recording = self.capturing == Some(id);
            let label = if recording { "Press shortcut…".to_owned() } else { keys.first().map(|key| Keystroke::parse(key).map(|key| Kbd::format(&key)).unwrap_or_else(|_| key.clone())).unwrap_or_else(|| "Not assigned".into()) };
            let capture_focus = self.rows[row].focus.clone();
            let control = components::browser_settings_header_button(format!("{id}-shortcut"), label, recording, theme)
                .w(rems(SHORTCUT_BUTTON_WIDTH_REM))
                .max_w_full()
                .tooltip(if self.saving.is_some() {
                    "Saving keyboard shortcuts…"
                } else if recording {
                    "Press a shortcut, or click to cancel"
                } else {
                    "Click to change shortcut"
                })
                .on_click(cx.listener(move |editor, _, window, cx| {
                    if editor.saving.is_some() {
                        return;
                    }
                    editor.capturing = if editor.capturing == Some(id) { None } else { Some(id) };
                    editor.warning = None;
                    capture_focus.focus(window, cx);
                    cx.notify();
                }));
            let mut content = components::browser_settings_compact_row(setting.label, control, theme).when(row == 0, |content| content.border_t_0());
            if recording {
                content = content.children(self.warning.clone().map(|warning| components::browser_settings_group_error(warning, theme)));
            }
            group = group.child(components::browser_settings_section(&self.rows[row].focus, self.rows[row].bounds.clone(), content));
        }
        gpui::div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(components::SPACE_MD))
            .key_context("ShortcutSettings")
            .track_focus(&self.focus)
            .when(self.capturing.is_none(), |content| content.children(self.warning.clone().map(|warning| components::browser_settings_group_error(warning, theme))))
            .child(group)
            .child(gpui::div().w_full().flex().justify_end().child(restore))
    }
}
