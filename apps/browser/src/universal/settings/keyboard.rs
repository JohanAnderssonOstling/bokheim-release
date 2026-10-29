use super::*;
use gpui::{Div, Point};

impl SettingsPage {
    pub(crate) fn open_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select_settings_tab(false, cx);
        self.focus.focus(window, cx);
    }

    pub(super) fn settings_container(&self, content: impl IntoElement, desktop: bool, cx: &Context<Self>) -> components::Scrollable<Div> {
        let theme = components::browser_theme(cx);
        let tabs = components::browser_settings_button_group(theme)
            .child(components::settings_choice_button("settings-general-tab", "General", !self.keyboard_tab, theme).on_click(cx.listener(|page, _, _, cx| page.select_settings_tab(false, cx))))
            .child(components::settings_choice_button("settings-keyboard-tab", "Keyboard", self.keyboard_tab, theme).on_click(cx.listener(|page, _, _, cx| page.select_settings_tab(true, cx))));
        components::browser_settings_page_with_scroll(&self.scroll)
            .key_context(SETTINGS_KEY_CONTEXT)
            .track_focus(&self.focus)
            .tab_group()
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .when(desktop, |page| page.child(components::browser_settings_content().child(tabs)))
            .child(content)
    }

    fn select_settings_tab(&mut self, keyboard: bool, cx: &mut Context<Self>) {
        self.keyboard_tab = keyboard;
        self.keyboard.update(cx, |editor, cx| editor.cancel_capture(cx));
        self.scroll.set_offset(Point::default());
        cx.notify();
    }

    pub(super) fn keyboard_panel(&self) -> Div {
        components::browser_settings_content().child(self.keyboard.clone())
    }
}
