//! Context actions shared by desktop popup menus and mobile bottom sheets.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Window, div, px};
use gpui_component::menu::PopupMenu;
use ui_components as components;

#[derive(Clone)]
pub(crate) struct ContextAction {
    label: &'static str,
    run: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl ContextAction {
    pub(crate) fn new(label: &'static str, run: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { label, run: Rc::new(run) }
    }
}

#[derive(Default)]
pub(crate) struct ContextActions {
    sections: Vec<Vec<ContextAction>>,
}

impl ContextActions {
    pub(crate) fn add_section(&mut self, actions: Vec<ContextAction>) {
        if !actions.is_empty() {
            self.sections.push(actions);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    pub(crate) fn popup(&self, mut menu: PopupMenu) -> PopupMenu {
        for (index, section) in self.sections.iter().enumerate() {
            if index > 0 {
                menu = menu.separator();
            }
            for action in section {
                let run = action.run.clone();
                menu = menu.item(components::menu_item(action.label, move |_, window, cx| run(window, cx)));
            }
        }
        menu
    }

    pub(crate) fn sheet(&self, theme: components::BrowserTheme, safe_bottom: gpui::Pixels, on_dismiss: impl Fn(&mut Window, &mut App) + Clone + 'static) -> AnyElement {
        let mut rows = div().id("reader-context-sheet-rows").flex_1().min_h_0().overflow_y_scroll();
        for (section_index, section) in self.sections.iter().enumerate() {
            if section_index > 0 {
                rows = rows.child(div().h(px(1.0)).my(px(components::SPACE_XXS)).bg(theme.rule));
            }
            for (action_index, action) in section.iter().enumerate() {
                let run = action.run.clone();
                let dismiss = on_dismiss.clone();
                rows = rows.child(
                    div()
                        .id(("reader-context-action", section_index * 10 + action_index))
                        .cursor_pointer()
                        .min_h(px(48.0))
                        .flex()
                        .items_center()
                        .px(px(components::SPACE_MD))
                        .py(px(components::SPACE_SM))
                        .text_size(gpui::rems(components::TEXT_MD))
                        .hover(move |style| style.bg(theme.hover))
                        .child(action.label)
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            dismiss(window, cx);
                            run(window, cx);
                        }),
                );
            }
        }
        let dismiss_scrim = on_dismiss.clone();
        components::bottom_sheet_overlay()
            .child(components::modal_scrim("reader-context-sheet-scrim", theme).on_click(move |_, window, cx| dismiss_scrim(window, cx)))
            .child(components::bottom_sheet_surface(theme).id("reader-context-sheet").child(components::bottom_sheet_header("Actions", theme)).child(rows).pb(safe_bottom))
            .into_any_element()
    }
}
