//! Pure folder destination selection for folder-scoped browse actions.

use std::collections::HashSet;

use gpui::prelude::*;
use gpui::{Context, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, Render, Window, div, px};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Disableable, IconName, Sizable};
use library_model::LibraryFolderDestination;
use sync_common::{DirId, ROOT_DIR_ID};
use ui_components as components;
use ui_components::ScrollableElement as BokheimScrollableElement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderPickerEvent {
    Cancelled,
    Selected(DirId),
}

/// Browses the folder tree from a current directory and returns one directory
/// ID. It deliberately has no knowledge of the action using that destination.
pub(crate) struct FolderPicker {
    destinations: Vec<LibraryFolderDestination>,
    selected_id: DirId,
    disabled_ids: HashSet<DirId>,
    /// The picker holds the keyboard for as long as it is up: it is a modal,
    /// and the page behind it must not act on keys aimed at this.
    focus: FocusHandle,
    focus_taken: bool,
}

/// IDs from the first folder below Library through the selected folder.
fn selected_path_of(destinations: &[LibraryFolderDestination], selected_id: DirId) -> Vec<DirId> {
    let mut reversed = Vec::new();
    let mut cursor = selected_id;
    let mut visited = HashSet::new();
    while cursor != ROOT_DIR_ID && visited.insert(cursor) {
        let Some(destination) = destinations.iter().find(|destination| destination.id == cursor) else { break };
        reversed.push(cursor);
        let Some(parent_id) = destination.parent_id else { break };
        cursor = parent_id;
    }
    reversed.reverse();
    reversed
}

impl EventEmitter<FolderPickerEvent> for FolderPicker {}

impl Focusable for FolderPicker {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FolderPicker {
    pub(crate) fn new(current_dir: DirId, destinations: Vec<LibraryFolderDestination>, cx: &mut Context<Self>) -> Self {
        let selected_id = if destinations.iter().any(|destination| destination.id == current_dir) { current_dir } else { ROOT_DIR_ID };
        Self { destinations, selected_id, disabled_ids: HashSet::new(), focus: cx.focus_handle(), focus_taken: false }
    }

    /// Walks the tree with the arrows: up and down within a column, right into
    /// a folder's children, left back to its parent — the movements the columns
    /// already draw.
    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => self.step_within_column(-1, cx),
            "down" => self.step_within_column(1, cx),
            "left" => self.step_to_parent(cx),
            "right" => self.step_to_child(cx),
            "enter" => self.confirm(cx),
            "escape" => self.cancel(cx),
            _ => {}
        }
        // Every key, handled or not: a modal that lets keys through is a modal
        // the page behind it is still reading.
        cx.stop_propagation();
    }

    fn children_of(&self, parent_id: DirId) -> impl Iterator<Item = &LibraryFolderDestination> {
        self.destinations.iter().filter(move |destination| destination.parent_id == Some(parent_id))
    }

    fn step_within_column(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(parent) = self.destination(self.selected_id).and_then(|destination| destination.parent_id) else { return };
        let siblings = self.children_of(parent);
        let next = if delta < 0 {
            siblings.take_while(|destination| destination.id != self.selected_id).last()
        } else {
            siblings.skip_while(|destination| destination.id != self.selected_id).nth(1)
        }
        .map(|destination| destination.id);
        if let Some(next) = next {
            self.select(next, cx);
        }
    }

    fn step_to_child(&mut self, cx: &mut Context<Self>) {
        let first = self.children_of(self.selected_id).next().map(|destination| destination.id);
        if let Some(first) = first {
            self.select(first, cx);
        }
    }

    fn step_to_parent(&mut self, cx: &mut Context<Self>) {
        let Some(parent) = self.destination(self.selected_id).and_then(|destination| destination.parent_id) else { return };
        self.select(parent, cx);
    }

    fn select(&mut self, id: DirId, cx: &mut Context<Self>) {
        if self.selected_id == id {
            return;
        }
        self.selected_id = id;
        cx.notify();
    }

    pub(crate) fn disabling(mut self, id: DirId) -> Self {
        self.disabled_ids.insert(id);
        if self.selected_id == id {
            self.selected_id = self.destinations.iter().find(|destination| !self.disabled_ids.contains(&destination.id)).map(|destination| destination.id).unwrap_or(ROOT_DIR_ID);
        }
        self
    }

    fn destination(&self, id: DirId) -> Option<&LibraryFolderDestination> {
        self.destinations.iter().find(|destination| destination.id == id)
    }

    /// One column per level, from Library down to the children of the selection.
    fn columns(&self, selected_path: &[DirId]) -> Vec<Vec<&LibraryFolderDestination>> {
        let root = self.destination(ROOT_DIR_ID).map(|root| vec![root]);
        let children = std::iter::once(ROOT_DIR_ID).chain(selected_path.iter().copied()).map(|parent| self.children_of(parent).collect::<Vec<_>>()).take_while(|children| !children.is_empty());
        root.into_iter().chain(children).collect()
    }

    fn confirm(&self, cx: &mut Context<Self>) {
        let Some(destination) = self.destination(self.selected_id) else { return };
        if self.disabled_ids.contains(&destination.id) {
            return;
        }
        cx.emit(FolderPickerEvent::Selected(destination.id));
    }

    fn cancel(&self, cx: &mut Context<Self>) {
        cx.emit(FolderPickerEvent::Cancelled);
    }
}

impl Render for FolderPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        // Taken on the first frame rather than at construction: the picker is
        // built from an async destination load, which has no window.
        if !self.focus_taken {
            self.focus_taken = true;
            self.focus.focus(window, cx);
        }
        let selected_path = selected_path_of(&self.destinations, self.selected_id);
        let columns = self.columns(&selected_path);
        // Flex can size a `flex_1` child against a parent's own resolved
        // height, but not against one that is itself only a `max_h` with no
        // explicit height to grow into — that left the column strip at zero
        // height. Computed here instead, from the tallest column's own rows,
        // and clamped the same way the library switcher's list is.
        const ROW_HEIGHT: f32 = 36.0;
        let rows = columns.iter().map(|destinations| destinations.len()).max().unwrap_or(0);
        let column_content_height = rows as f32 * ROW_HEIGHT + components::SPACE_XS * 2.0;
        let panel_height = (58.0 + column_content_height).min(components::NAVIGATION_MODAL_MAX_HEIGHT_REM * f32::from(window.rem_size()));
        let mut column_strip = div().flex_1().min_h_0().min_w_0().flex().flex_row().overflow_x_scrollbar();
        for (depth, destinations) in columns.into_iter().enumerate() {
            let mut contents = div().w_full().flex().flex_col().p(px(components::SPACE_XS));
            for destination in destinations {
                let on_path = destination.id == self.selected_id || selected_path.contains(&destination.id);
                let disabled = self.disabled_ids.contains(&destination.id);
                let destination_id = destination.id;
                contents = contents.child(
                    components::selectable_button(components::base_button(format!("folder-picker-{destination_id}")).ghost().small().disabled(disabled).w_full().h(px(36.0)).justify_start().icon(IconName::Folder), on_path, theme)
                        .label(destination.label.clone())
                        .on_click(cx.listener(move |picker, _, _, cx| picker.select(destination_id, cx))),
                );
            }
            let column = div()
                .id(format!("folder-picker-column-{depth}"))
                .flex_1()
                .min_w(px(190.0))
                .min_h_0()
                .border_r_1()
                .border_color(theme.rule)
                .child(BokheimScrollableElement::overflow_y_scrollbar_with_id(contents.flex_1().min_h_0(), format!("folder-picker-column-scroll-{depth}")));
            column_strip = column_strip.child(column);
        }

        let can_select = self.destination(self.selected_id).is_some() && !self.disabled_ids.contains(&self.selected_id);
        // Sized to its content rather than stretched full-height: the picker
        // is a handful of folder columns, not a page, and a nearly-empty
        // library left it a mostly-blank pane. Capped the way the library
        // switcher's own growing list is, so a deep tree still scrolls
        // instead of pushing the dialog off the window.
        let panel = components::modal_surface(theme)
            .w_full()
            .h(px(panel_height))
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(58.0))
                    .flex_none()
                    .px(px(components::SPACE_MD))
                    .flex()
                    .items_center()
                    .gap(px(components::SPACE_SM))
                    .border_b_1()
                    .border_color(theme.rule)
                    .child(div().flex_1())
                    .child(components::dialog_cancel_button("cancel-folder-picker", "Cancel").on_click(cx.listener(|picker, _, _, cx| picker.cancel(cx))))
                    .child(
                        components::base_button("confirm-folder-picker")
                            .primary()
                            .disabled(!can_select)
                            .label("Choose")
                            .tooltip(components::shortcut_tooltip("Choose", "enter"))
                            .on_click(cx.listener(|picker, _, _, cx| picker.confirm(cx))),
                    ),
            )
            .child(column_strip);
        let scrim = components::modal_scrim("folder-picker-scrim", theme).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(cx.listener(|picker, _, _, cx| {
            cx.stop_propagation();
            picker.cancel(cx);
        }));
        gpui::deferred(components::centered_modal(scrim, panel, window)).with_priority(2)
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use std::{cell::RefCell, rc::Rc};

    #[gpui::test]
    fn keyboard_follows_columns_and_disabled_folders_cannot_be_confirmed(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let [first, second, child, disabled] = [1, 2, 3, 4].map(DirId::from_u128);
        let destinations = [(ROOT_DIR_ID, None), (first, Some(ROOT_DIR_ID)), (second, Some(ROOT_DIR_ID)), (child, Some(first)), (disabled, Some(first))]
            .into_iter()
            .map(|(id, parent_id)| LibraryFolderDestination { id, parent_id, label: id.to_string(), path: String::new() })
            .collect();
        let picker = cx.new(|cx| FolderPicker::new(child, destinations, cx).disabling(disabled));
        let received = Rc::new(RefCell::new(Vec::new()));
        let events = received.clone();
        let _subscription = cx.update(|cx| cx.subscribe(&picker, move |_, event: &FolderPickerEvent, _| events.borrow_mut().push(*event)));
        let window = cx.add_window(|window, cx| gpui_component::Root::new(picker.clone(), window, cx));
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        visual.read_entity(&picker, |picker, _| {
            let path = selected_path_of(&picker.destinations, picker.selected_id);
            assert_eq!(path, [first, child]);
            assert_eq!(picker.columns(&path).into_iter().map(|column| column.into_iter().map(|destination| destination.id).collect::<Vec<_>>()).collect::<Vec<_>>(), vec![vec![ROOT_DIR_ID], vec![first, second], vec![child, disabled]]);
        });
        visual.simulate_keystrokes("down down enter");
        visual.read_entity(&picker, |picker, _| assert_eq!(picker.selected_id, disabled));
        assert!(received.borrow().is_empty());
        visual.simulate_keystrokes("left down right");
        visual.read_entity(&picker, |picker, _| assert_eq!(picker.selected_id, second));
        visual.simulate_keystrokes("left left up down");
        visual.read_entity(&picker, |picker, _| assert_eq!(picker.selected_id, ROOT_DIR_ID));
        visual.simulate_keystrokes("right right up enter escape");
        assert_eq!(*received.borrow(), [FolderPickerEvent::Selected(child), FolderPickerEvent::Cancelled]);
    }
}
