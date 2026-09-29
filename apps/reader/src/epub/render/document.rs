//! EPUB document surface and its selection/table/image context menu.

use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, Entity, IntoElement, Point, Pixels, px};
use gpui_component::menu::ContextMenuExt;
use ui_components as components;

use crate::epub::{LoadState, ReaderView};
use crate::shell::context_sheet::{ContextAction, ContextActions};

/// Keeps glyph ink and logical-to-framebuffer rounding inside the Kobo panel.
/// This is a renderer guard band, not an authored EPUB margin.
const KOBO_READER_EDGE_INSET: f32 = 2.0;

impl ReaderView {
    pub(in crate::epub) fn toggle_mobile_controls(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if !components::uses_mobile_navigation(window) || self.sidebar.is_visible() && self.chrome.is_visible() {
            return;
        }
        if self.chrome.is_visible() {
            self.chrome.hide(cx);
            self.focus_document(window, cx);
        } else {
            self.sidebar.close();
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
        let document = components::reader_content()
            .id("epub-reader-document")
            .px(px(horizontal_margin + if cfg!(feature = "kobo") { KOBO_READER_EDGE_INSET } else { 0.0 }))
            .py(px(vertical_margin))
            .child(renderer.clone())
            .on_click(cx.listener(|reader, event, window, cx| {
                if crate::shell::chrome::is_page_tap(event) {
                    reader.toggle_mobile_controls(window, cx);
                }
            }))
            .on_aux_click(cx.listener(|reader, event: &ClickEvent, window, cx| {
                if event.is_secondary() && components::uses_mobile_navigation(window) {
                    reader.open_context_sheet(event.position(), cx);
                }
            }));
        if mobile {
            document.into_any_element()
        } else {
            document.context_menu(move |menu, window, cx| {
                let position = window.mouse_position();
                context_target.read(cx).context_actions_at(position, &context_target, cx).popup(menu)
            }).into_any_element()
        }
    }
}

impl ReaderView {
    fn context_actions_at(&self, position: Point<Pixels>, entity: &Entity<Self>, cx: &gpui::App) -> ContextActions {
        let mut actions = ContextActions::default();
        let LoadState::Ready(renderer) = &self.load_state else { return actions };
        if self.selection.is_some() && renderer.read(cx).selection_contains(position) {
            let highlight = entity.clone();
            let cite = entity.clone();
            actions.add_section(vec![
                ContextAction::new("Highlight", move |window, cx| highlight.update(cx, |reader, cx| reader.highlight_selection(window, cx))),
                ContextAction::new("Cite", move |_, cx| cite.update(cx, |reader, cx| reader.copy_selection_citation(cx))),
            ]);
        }
        if renderer.read(cx).table_at(position) {
            let markdown = renderer.clone();
            let plain = renderer.clone();
            let styled = renderer.clone();
            actions.add_section(vec![
                ContextAction::new("Copy Table as Markdown", move |_, cx| { markdown.update(cx, |view, _| view.copy_table_at(position)); }),
                ContextAction::new("Copy Table as Unstyled HTML", move |_, cx| { plain.update(cx, |view, _| view.copy_table_unstyled_html_at(position)); }),
                ContextAction::new("Copy Table as Styled HTML", move |_, cx| { styled.update(cx, |view, _| view.copy_table_styled_html_at(position)); }),
            ]);
        }
        if renderer.read(cx).image_at(position) {
            let image = renderer.clone();
            actions.add_section(vec![ContextAction::new("Copy Image", move |_, cx| { image.update(cx, |view, _| view.copy_image_at(position)); })]);
        }
        actions
    }

    fn open_context_sheet(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let actions = self.context_actions_at(position, &cx.entity(), cx);
        if !actions.is_empty() {
            self.context_sheet = Some(actions);
            self.chrome.hide(cx);
            cx.notify();
        }
    }
}
