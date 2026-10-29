//! EPUB document surface, direct annotation creation, and table/image context menu.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, IntoElement, LongPressEvent, MouseButton, MouseDownEvent, Pixels, Point, Window, px};
use gpui_component::menu::{ContextMenuExt, OpenContextMenu};
use ui_components as components;

use crate::epub::{EpubContextCommand, LoadState, ReaderView, TableCopyFormat};
use crate::shell::context_sheet::{ContextAction, ContextActionTarget, ContextActions};

impl ReaderView {
    pub(in crate::epub) fn toggle_mobile_controls(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if !components::uses_mobile_reader_controls(window) || self.sidebar.is_visible() && self.chrome.is_visible() {
            return;
        }
        if self.chrome.is_visible() {
            self.chrome.hide(cx);
            self.focus_document(window, cx);
        } else {
            self.sidebar.close(cx);
            self.chrome.show_from_touch(cx);
        }
    }

    pub(super) fn render_document(&self, entity: Entity<Self>, horizontal_margin: f32, vertical_margin: f32, mobile: bool, cx: &mut Context<Self>) -> AnyElement {
        if let LoadState::Error(error) = &self.load_state {
            return components::reader_content().child(gpui::div().size_full().flex().flex_col().items_center().justify_center().gap(px(16.0)).p(px(24.0)).child("Could not display this book").child(error.clone())).into_any_element();
        }
        let LoadState::Ready(renderer) = &self.load_state else {
            return components::reader_content().child(gpui::div().size_full()).into_any_element();
        };

        let context_target = entity.clone();
        let keyboard_target = entity.clone();
        renderer.update(cx, |view, cx| view.set_reading_ruler_margins(horizontal_margin, vertical_margin, cx));
        let document = components::reader_content()
            .id("epub-reader-document")
            .px(px(horizontal_margin))
            .py(px(vertical_margin))
            .child(renderer.clone())
            .on_click(cx.listener(|reader, event, window, cx| {
                if crate::shell::chrome::is_page_tap(event) {
                    reader.toggle_mobile_controls(window, cx);
                }
            }))
            // Handle the selection before the context-menu wrapper sees the
            // event, so it cannot open a menu over the annotation editor.
            .capture_any_mouse_down(cx.listener(|reader, event: &MouseDownEvent, window, cx| {
                if event.button == MouseButton::Left {
                    reader.dismiss_selection(cx);
                    return;
                }
                if event.button != MouseButton::Right {
                    return;
                }
                if matches!(&reader.load_state, LoadState::Ready(renderer) if renderer.read(cx).equation_at(event.position)) {
                    return;
                }
                if reader.open_passage_context(event.position, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_long_press(cx.listener(|reader, event: &LongPressEvent, window, cx| {
                if reader.begin_touch_selection(event, window, cx) || reader.open_passage_context(event.position, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|reader, _: &OpenContextMenu, window, cx| {
                if reader.selection.is_some() {
                    reader.create_annotation(window, cx);
                    cx.stop_propagation();
                }
            }));
        if mobile {
            document.on_action(cx.listener(|reader, _: &OpenContextMenu, window, cx| reader.open_keyboard_context_sheet(window.mouse_position(), cx))).into_any_element()
        } else {
            document
                .context_menu(move |menu, window, cx| {
                    let position = window.mouse_position();
                    context_target.read(cx).context_actions_at(position, cx).popup(menu, &context_target)
                })
                .keyboard_menu(move |menu, window, cx| keyboard_target.read(cx).keyboard_context_actions(window.mouse_position(), cx).popup(menu, &keyboard_target))
                .into_any_element()
        }
    }
}

impl ReaderView {
    fn open_passage_context(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.selection.is_some() && matches!(&self.load_state, LoadState::Ready(renderer) if renderer.read(cx).selection_contains(position)) {
            self.annotation_popup_anchor = Some(position);
            self.create_annotation(window, cx);
            true
        } else if components::uses_mobile_reader_controls(window) {
            self.open_context_sheet(position, cx);
            true
        } else {
            false
        }
    }

    fn context_actions_at(&self, position: Point<Pixels>, cx: &App) -> ContextActions<EpubContextCommand> {
        self.context_actions(position, cx)
    }

    fn keyboard_context_actions(&self, pointer: Point<Pixels>, cx: &App) -> ContextActions<EpubContextCommand> {
        let position = match &self.load_state {
            LoadState::Ready(renderer) => renderer.read(cx).selection_bounds().map(|bounds| bounds.center()).unwrap_or(pointer),
            _ => pointer,
        };
        self.context_actions(position, cx)
    }

    fn context_actions(&self, position: Point<Pixels>, cx: &App) -> ContextActions<EpubContextCommand> {
        let mut actions = ContextActions::default();
        let LoadState::Ready(renderer) = &self.load_state else { return actions };
        if renderer.read(cx).table_at(position) {
            actions.add_section(vec![
                ContextAction { label: "Copy Table as Markdown", command: EpubContextCommand::CopyTable { view: renderer.clone(), position, format: TableCopyFormat::Markdown } },
                ContextAction { label: "Copy Table as Unstyled HTML", command: EpubContextCommand::CopyTable { view: renderer.clone(), position, format: TableCopyFormat::UnstyledHtml } },
                ContextAction { label: "Copy Table as Styled HTML", command: EpubContextCommand::CopyTable { view: renderer.clone(), position, format: TableCopyFormat::StyledHtml } },
            ]);
        }
        if renderer.read(cx).image_at(position) {
            actions.add_section(vec![ContextAction { label: "Copy Image", command: EpubContextCommand::CopyImage { view: renderer.clone(), position } }]);
        }
        if renderer.read(cx).equation_at(position) {
            actions.add_section(vec![ContextAction { label: "Copy as LaTeX", command: EpubContextCommand::CopyEquationLatex { view: renderer.clone(), position } }]);
        }
        actions
    }

    fn open_keyboard_context_sheet(&mut self, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let actions = self.keyboard_context_actions(pointer, cx);
        self.show_context_sheet(actions, cx);
    }

    fn open_context_sheet(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let actions = self.context_actions_at(position, cx);
        self.show_context_sheet(actions, cx);
    }

    fn show_context_sheet(&mut self, actions: ContextActions<EpubContextCommand>, cx: &mut Context<Self>) {
        if !actions.is_empty() {
            self.context_sheet = Some(actions);
            self.chrome.hide(cx);
            cx.notify();
        }
    }
}

impl ContextActionTarget for ReaderView {
    type Command = EpubContextCommand;

    fn run_context_action(&mut self, command: Self::Command, _window: &mut Window, cx: &mut Context<Self>) {
        match command {
            EpubContextCommand::CopyTable { view, position, format } => {
                view.update(cx, |view, _| match format {
                    TableCopyFormat::Markdown => view.copy_table_at(position),
                    TableCopyFormat::UnstyledHtml => view.copy_table_unstyled_html_at(position),
                    TableCopyFormat::StyledHtml => view.copy_table_styled_html_at(position),
                });
            }
            EpubContextCommand::CopyImage { view, position } => {
                view.update(cx, |view, _| view.copy_image_at(position));
            }
            EpubContextCommand::CopyEquationLatex { view, position } => {
                view.update(cx, |view, _| view.copy_equation_latex_at(position));
            }
        }
    }

    fn dismiss_context_actions(&mut self, cx: &mut Context<Self>) {
        self.context_sheet = None;
        cx.notify();
    }
}
