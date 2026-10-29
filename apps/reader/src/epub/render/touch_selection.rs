//! Selection actions for mouse and touch, plus touch selection handles.

use std::{cell::Cell, rc::Rc};

use gpui::prelude::*;
use gpui::{Anchor, AnyElement, Bounds, Context, IntoElement, LongPressEvent, MouseButton, Pixels, Point, TouchEvent, TouchId, TouchPhase, Window, anchored, canvas, deferred, div, point, px};
use ui_components as components;

use crate::epub::{LoadState, ReaderView, Selection};

#[derive(Default)]
pub(in crate::epub) struct TouchSelectionState {
    pub(in crate::epub) active: bool,
    drag: Option<SelectionDrag>,
    action_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

struct SelectionDrag {
    id: TouchId,
    offset: Point<Pixels>,
}

/// Keep both the knob and its touch target clear of the system gesture area.
fn handle_center(endpoint: Point<Pixels>, window: &Window) -> Point<Pixels> {
    let viewport = window.viewport_size();
    let bottom = (f32::from(viewport.height - window.insets().safe_area.bottom) - 22.0).max(22.0);
    point(px(f32::from(endpoint.x).clamp(22.0, f32::from(viewport.width).max(44.0) - 22.0)), px((f32::from(endpoint.y) + 12.0).clamp(22.0, bottom)))
}

impl ReaderView {
    pub(super) fn begin_touch_selection(&mut self, event: &LongPressEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let LoadState::Ready(renderer) = &self.load_state else { return false };
        let selected = renderer.update(cx, |view, cx| view.select_word_at(event.position, cx));
        if !selected {
            return false;
        }
        self.selection = None;
        self.touch_selection.active = true;
        self.touch_selection.drag = Some(SelectionDrag { id: event.id, offset: Point::default() });
        self.touch_selection.action_bounds.set(None);
        self.context_sheet = None;
        self.close_annotation_popup(cx);
        self.chrome.hide(cx);
        self.wheel_debounce.take();
        self.wheel_delta = 0.0;
        self.touch_swipe = None;
        self.focus_document(window, cx);
        window.prevent_default();
        cx.notify();
        true
    }

    pub(super) fn handle_selection_touch(&mut self, event: &TouchEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.touch_selection.active {
            return;
        }
        let LoadState::Ready(renderer) = &self.load_state else { return };
        let renderer = renderer.clone();
        if let Some(drag) = &self.touch_selection.drag {
            if event.id != drag.id {
                return;
            }
            let position = event.position - drag.offset;
            match event.phase {
                TouchPhase::Moved => renderer.update(cx, |view, cx| view.update_touch_selection(position, cx)),
                TouchPhase::Ended | TouchPhase::Cancelled => {
                    renderer.update(cx, |view, _| view.end_touch_selection());
                    self.touch_selection.drag = None;
                    cx.notify();
                }
                TouchPhase::Started => {}
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if event.phase != TouchPhase::Started {
            return;
        }
        if self.touch_selection.action_bounds.get().is_some_and(|bounds| bounds.contains(&event.position)) {
            return;
        }
        let (start, end) = renderer.read(cx).selection_endpoints();
        let handle = [(true, start), (false, end)]
            .into_iter()
            .filter_map(|(start, endpoint)| {
                let endpoint = endpoint?;
                let center = handle_center(endpoint, window);
                let distance = f32::from(event.position.x - center.x).hypot(f32::from(event.position.y - center.y));
                (distance <= 24.0).then_some((start, endpoint, distance))
            })
            .min_by(|left, right| left.2.total_cmp(&right.2));
        if let Some((start, endpoint, _)) = handle {
            if renderer.update(cx, |view, _| view.begin_selection_handle(start)) {
                // Touches land on the knob below the text. Preserve the grab
                // offset, and hit-test slightly above the line's lower edge.
                self.touch_selection.drag = Some(SelectionDrag { id: event.id, offset: event.position - endpoint + point(px(0.0), px(3.0)) });
                self.touch_selection.action_bounds.set(None);
                cx.notify();
            }
        } else if !renderer.read(cx).selection_contains(event.position) {
            self.dismiss_selection(cx);
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    pub(super) fn dismiss_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.touch_selection.active && self.selection.is_none() {
            return false;
        }
        self.touch_selection.active = false;
        self.touch_selection.drag = None;
        self.touch_selection.action_bounds.set(None);
        self.selection = None;
        if let LoadState::Ready(renderer) = &self.load_state {
            renderer.update(cx, |view, cx| view.clear_text_selection(cx));
        }
        cx.notify();
        true
    }

    fn annotate_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let LoadState::Ready(renderer) = &self.load_state else { return };
        let Some((cfi_range, text)) = renderer.read(cx).selected_passage() else { return };
        self.dismiss_selection(cx);
        self.selection = Some(Selection { cfi_range, text });
        self.create_annotation(window, cx);
        self.annotation_note_input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let LoadState::Ready(renderer) = &self.load_state {
            renderer.read(cx).copy_selection_as_markdown();
        }
        self.dismiss_selection(cx);
    }

    pub(super) fn render_selection_actions(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.touch_selection.active && self.selection.is_none() {
            return None;
        }
        let LoadState::Ready(renderer) = &self.load_state else { return None };
        let view = renderer.read(cx);
        let (start, end) = view.selection_endpoints();
        let bounds = view.selection_bounds()?;
        let theme = components::browser_theme(cx);
        let mut overlay = div().absolute().inset_0();
        for (name, endpoint) in [("start", start), ("end", end)].into_iter().filter(|_| self.touch_selection.active) {
            let Some(endpoint) = endpoint else { continue };
            let center = handle_center(endpoint, window);
            let knob = div()
                .id(format!("epub-selection-{name}-handle"))
                .debug_selector(move || format!("epub-selection-{name}-handle"))
                .size(px(44.0))
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .child(div().absolute().left(px(21.0)).top(px(6.0)).w(px(2.0)).h(px(16.0)).bg(theme.accent))
                .child(div().size(px(18.0)).rounded_full().bg(theme.accent));
            overlay = overlay.child(deferred(anchored().position(center - point(px(22.0), px(22.0))).child(knob)).with_priority(2));
        }
        if self.touch_selection.drag.is_none() {
            let measure = self.touch_selection.action_bounds.clone();
            let actions = div()
                .id("epub-selection-actions")
                .debug_selector(|| "epub-selection-actions".into())
                .relative()
                .flex()
                .flex_wrap()
                .max_w((window.viewport_size().width - px(16.0)).max(px(1.0)))
                .gap(px(4.0))
                .p(px(4.0))
                .bg(theme.page_bg)
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(canvas(move |bounds, _, _| measure.set(Some(bounds)), |_, _, _, _| {}).absolute().inset_0())
                .child(components::outlined_text_button("epub-selection-annotate", "Annotate").h(px(44.0)).on_click(cx.listener(|reader, _, window, cx| reader.annotate_selection(window, cx))))
                .child(
                    components::outlined_text_button("epub-selection-copy-markdown", components::shortcut_tooltip("Copy to Markdown", "secondary-shift-c"))
                        .h(px(44.0))
                        .debug_selector(|| "epub-selection-copy-markdown".into())
                        .on_click(cx.listener(|reader, _, _, cx| reader.copy_selection(cx))),
                );
            // Above the passage when there is room, otherwise below it.
            let above = f32::from(bounds.top()) > 64.0 + f32::from(window.insets().safe_area.top);
            let position = point(bounds.left(), if above { bounds.top() - px(8.0) } else { bounds.bottom() + px(40.0) });
            let actions = if cx.supports_touch_input() {
                let viewport = window.viewport_size();
                let safe = window.insets().effective();
                anchored()
                    .position(point(safe.left, viewport.height - safe.bottom))
                    .anchor(Anchor::BottomLeft)
                    .child(actions.w((viewport.width - safe.left - safe.right).max(px(1.0))).flex_wrap())
            } else {
                anchored().position(position).anchor(if above { Anchor::BottomLeft } else { Anchor::TopLeft }).snap_to_window_with_margin(px(8.0)).child(actions)
            };
            overlay = overlay.child(deferred(actions).with_priority(3));
        }
        Some(overlay.into_any_element())
    }
}
