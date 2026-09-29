//! Pure folder destination selection for folder-scoped browse actions.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Context, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, Render, Window, div, px};
use gpui_component::button::{Button, ButtonVariants};
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
    destinations: Rc<Vec<LibraryFolderDestination>>,
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

/// One column per level, from Library down to the children of the selection.
fn columns_of(destinations: &[LibraryFolderDestination], selected_id: DirId) -> Vec<Vec<LibraryFolderDestination>> {
    let selected_path = selected_path_of(destinations, selected_id);
    let mut columns = Vec::new();
    if let Some(root) = destinations.iter().find(|destination| destination.id == ROOT_DIR_ID) {
        columns.push(vec![root.clone()]);
    }
    let mut parent_id = ROOT_DIR_ID;
    let mut depth = 0;
    loop {
        let children = destinations.iter().filter(|destination| destination.parent_id == Some(parent_id)).cloned().collect::<Vec<_>>();
        if children.is_empty() {
            break;
        }
        columns.push(children);
        let Some(next) = selected_path.get(depth).copied() else { break };
        parent_id = next;
        depth += 1;
    }
    columns
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
        Self { destinations: Rc::new(destinations), selected_id, disabled_ids: HashSet::new(), focus: cx.focus_handle(), focus_taken: false }
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

    fn children_of(&self, parent_id: DirId) -> Vec<LibraryFolderDestination> {
        self.destinations.iter().filter(|destination| destination.parent_id == Some(parent_id)).cloned().collect()
    }

    /// The folders drawn beside the selected one, and where it sits among them.
    fn selected_row(&self) -> Option<(Vec<LibraryFolderDestination>, usize)> {
        let parent = self.destination(self.selected_id)?.parent_id?;
        let siblings = self.children_of(parent);
        let index = siblings.iter().position(|destination| destination.id == self.selected_id)?;
        Some((siblings, index))
    }

    fn step_within_column(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some((siblings, index)) = self.selected_row() else { return };
        let last = siblings.len().saturating_sub(1) as isize;
        let next = (index as isize + delta).clamp(0, last) as usize;
        self.select(siblings[next].id, cx);
    }

    fn step_to_child(&mut self, cx: &mut Context<Self>) {
        let Some(first) = self.children_of(self.selected_id).first().cloned() else { return };
        self.select(first.id, cx);
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

    fn selected_path(&self) -> Vec<DirId> {
        selected_path_of(&self.destinations, self.selected_id)
    }

    fn columns(&self) -> Vec<Vec<LibraryFolderDestination>> {
        columns_of(&self.destinations, self.selected_id)
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
        let picker = cx.entity();
        let selected_path = self.selected_path();
        let columns = self.columns();
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
                let select_picker = picker.clone();
                contents = contents.child(
                    components::selectable_button(components::base_button(format!("folder-picker-{destination_id}")).ghost().small().disabled(disabled).w_full().h(px(36.0)).justify_start().icon(IconName::Folder), on_path, theme)
                        .label(destination.label)
                        .on_click(move |_, _, cx| {
                            select_picker.update(cx, |picker, cx| {
                                picker.selected_id = destination_id;
                                cx.notify();
                            });
                        }),
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
        let cancel_picker = picker.clone();
        let confirm_picker = picker.clone();
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
                    .child(components::dialog_cancel_button("cancel-folder-picker", "Cancel").on_click(move |_, _, cx| cancel_picker.update(cx, |picker, cx| picker.cancel(cx))))
                    .child(components::base_button("confirm-folder-picker").primary().disabled(!can_select).label("Choose").on_click(move |_, _, cx| confirm_picker.update(cx, |picker, cx| picker.confirm(cx)))),
            )
            .child(column_strip);
        let dismiss_picker = picker;
        // Anchored to the window rather than left to flow: a picker opened
        // from folder editing lives inside one pane of a possibly-split
        // browse view, and `modal_overlay`'s `size_full` would only ever
        // cover that one narrow pane. Anchoring at the window's origin with
        // its own viewport-sized child escapes that nesting, so the picker
        // always gets the window's full width regardless of how many split
        // panes are open beside it.
        let viewport = window.viewport_size();
        gpui::deferred(
            gpui::anchored().position(gpui::point(px(0.0), px(0.0))).child(
                components::modal_overlay()
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .child(components::modal_scrim("folder-picker-scrim", theme).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        dismiss_picker.update(cx, |picker, cx| picker.cancel(cx));
                    }))
                    .child(panel),
            ),
        )
        .with_priority(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(id: u128, parent_id: DirId, label: &str) -> LibraryFolderDestination {
        LibraryFolderDestination { id: DirId::from_u128(id), parent_id: Some(parent_id), label: label.to_owned(), path: label.to_owned() }
    }

    fn tree() -> Vec<LibraryFolderDestination> {
        let shelf = DirId::from_u128(1);
        vec![
            LibraryFolderDestination { id: ROOT_DIR_ID, parent_id: None, label: "Library".to_owned(), path: "Library".to_owned() },
            destination(1, ROOT_DIR_ID, "Shelf / literal separator"),
            destination(2, shelf, "Child"),
            destination(3, ROOT_DIR_ID, "Other"),
        ]
    }

    #[test]
    fn columns_follow_ids_instead_of_parsing_labels() {
        let child = DirId::from_u128(2);
        let columns = columns_of(&tree(), child);
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0].iter().map(|destination| destination.id).collect::<Vec<_>>(), [ROOT_DIR_ID]);
        assert_eq!(columns[1].iter().map(|destination| destination.id).collect::<Vec<_>>(), [DirId::from_u128(1), DirId::from_u128(3)]);
        assert_eq!(columns[2].iter().map(|destination| destination.id).collect::<Vec<_>>(), [child]);
    }
}
