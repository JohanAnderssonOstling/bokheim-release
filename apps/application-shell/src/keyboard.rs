//! Playback keys take precedence over document and library navigation.

use gpui::{Context, Modifiers, Window};
use gpui_component::WindowExt;

use super::ApplicationPane;

impl ApplicationPane {
    pub(super) fn install_playback_keys(&self, window: &mut Window, cx: &mut Context<Self>) {
        let target = cx.entity().downgrade();
        let handle = window.window_handle();
        window.use_keyed_state(("playback-keyboard", cx.entity_id()), cx, move |_, cx| {
            cx.intercept_keystrokes(move |event, window, cx| {
                let key = &event.keystroke;
                if window.window_handle() != handle
                    || key.modifiers != Modifiers::default()
                    || !matches!(key.key.as_str(), "space" | "left" | "right")
                    || window.is_text_input_active()
                    || window.has_active_dialog(cx)
                    || window.has_active_sheet(cx)
                    || event.context_stack.iter().any(|context| context.contains("Input") || context.contains("NumberInput") || context.contains("ShortcutSettings") || context.contains("PopupMenu") || context.contains("AppMenuBar"))
                {
                    return;
                }
                let Some(target) = target.upgrade() else { return };
                let handled = target.update(cx, |pane, cx| {
                    if pane.opening || !pane.opening_focus.contains_focused(window, cx) {
                        return false;
                    }
                    if key.key == "space" {
                        if let Some(reader) = &pane.reader
                            && reader.view.update(cx, |reader, cx| reader.toggle_narration(cx))
                        {
                            return true;
                        }
                    } else if pane.reader.is_some() {
                        return false;
                    }
                    let Some(audio) = &pane.audiobook else { return false };
                    audio.session.update(cx, |session, cx| match key.key.as_str() {
                        "space" => session.request_toggle(cx),
                        "left" => session.skip_backward(cx),
                        "right" => session.skip_forward(cx),
                        _ => unreachable!(),
                    });
                    true
                });
                if handled {
                    cx.stop_propagation();
                }
            })
        });
    }
}
