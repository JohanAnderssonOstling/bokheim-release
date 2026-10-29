use std::rc::Rc;

use gpui::{App, SharedString, Window};
use ui_components::Button;
use gpui_component::menu::PopupMenu;
use gpui_component::{Disableable, Icon};
use ui_components as components;

/// One command shared by the selection's context menu and footer.
#[derive(Clone)]
pub(crate) struct SelectionAction {
    pub(crate) id: &'static str,
    label: &'static str,
    icon: Icon,
    disabled: bool,
    run: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl SelectionAction {
    #[cfg(test)]
    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled
    }

    pub(crate) fn run(&self, window: &mut Window, cx: &mut App) {
        if !self.disabled {
            (self.run)(window, cx);
        }
    }

    pub(crate) fn new(id: &'static str, label: &'static str, icon: impl Into<Icon>, run: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { id, label, icon: icon.into(), disabled: false, run: Rc::new(run) }
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn button(&self, theme: components::BrowserTheme) -> Button {
        let run = self.run.clone();
        components::book_overlay_command(SharedString::from(format!("selection-{}", self.id)), self.label, self.icon.clone(), theme).disabled(self.disabled).on_click(move |_, window, cx| run(window, cx))
    }

    pub(crate) fn menu_item(&self, menu: PopupMenu) -> PopupMenu {
        let run = self.run.clone();
        menu.item(components::menu_item_with_icon(self.label, self.icon.clone(), move |_, window, cx| run(window, cx)).disabled(self.disabled))
    }
}
