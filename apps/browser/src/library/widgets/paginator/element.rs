//! GPUI element implementation for the paginator.
//!
//! Row assignment and geometry stay in `geometry`; this module is responsible
//! only for child preparation, placement, and painting.

use super::slots::Slots;
use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, AvailableSpace, BorderStyle, Bounds, ContentMask, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Style, WeakEntity, Window, div, point, px, quad, relative, size,
};
use ui_components as components;

use super::geometry::{self, AssignedRow, GroupGeometry};
use super::layout_cache::{LayoutCache, LayoutKey, PageLayout};
use super::{AssignmentDirection, Paginator, PaginatorAxis, PaginatorData, PaginatorState, PrefetchKey, VisiblePage};

const NEXT_PAGE_PEEK_PX: f32 = 8.0;
/// The band is drawn over the items it selects, so it has to be see-through.
const MARQUEE_FILL_OPACITY: f32 = 0.35;

pub(super) struct PaginatorPage {
    pub reported_visible_page: Option<VisiblePage>,
    pub child_offset: usize,
    pub direction: AssignmentDirection,
    pub prefetched: Option<PrefetchKey>,
    pub forced_page: Option<VisiblePage>,
}

pub(super) struct PaginatorElement {
    id: ElementId,
    paginator: WeakEntity<Paginator>,
    reported_visible_page: Option<VisiblePage>,
    children_revision: usize,
    data: Rc<PaginatorData>,
    widths: Rc<Slots<Pixels>>,
    layout_cache: Rc<RefCell<LayoutCache>>,
    warm_direction: Option<AssignmentDirection>,
    child_offset: usize,
    direction: AssignmentDirection,
    prefetched: Option<PrefetchKey>,
    /// Set only for the outgoing page of a transition, which must keep the
    /// range it had rather than resolve a fresh one.
    forced_page: Option<VisiblePage>,
    rem_size: Pixels,
    flow_axis: PaginatorAxis,
    show_next_page_preview: bool,
    reserve_preview_space: bool,
    state: Rc<PaginatorState>,
    active: bool,
    horizontal_offset: f32,
    vertical_offset: f32,
    /// Transition offset in pixels. Pages are panned by the height of the page
    /// between them, which is a measurement, not a fraction of the container —
    /// using the container's height is what left a gap between pages whenever a
    /// page did not fill it.
    vertical_pixels: Pixels,
    drag_offset: Point<Pixels>,
    reported_content_height: Option<Pixels>,
    /// Rubber-band selection, drawn and driven only where the page asked for it.
    selectable: bool,
    /// The band as of this frame, in window coordinates.
    marquee: Option<Bounds<Pixels>>,
}

impl PaginatorElement {
    pub(super) fn new(id: ElementId, paginator: WeakEntity<Paginator>, source: &Paginator, page: PaginatorPage, rem_size: Pixels, state: Rc<PaginatorState>) -> Self {
        let PaginatorPage { reported_visible_page, child_offset, direction, prefetched, forced_page } = page;
        Self {
            id,
            paginator,
            reported_visible_page,
            children_revision: source.children_revision,
            data: source.data.clone(),
            widths: Rc::default(),
            layout_cache: Rc::default(),
            warm_direction: None,
            child_offset,
            direction,
            prefetched,
            forced_page,
            rem_size,
            flow_axis: source.flow_axis,
            show_next_page_preview: source.show_next_page_preview,
            reserve_preview_space: source.reserve_preview_space,
            state,
            active: true,
            horizontal_offset: 0.0,
            vertical_offset: 0.0,
            vertical_pixels: Pixels::ZERO,
            drag_offset: Point::default(),
            reported_content_height: None,
            selectable: false,
            marquee: None,
        }
    }

    pub(super) fn with_warm_direction(mut self, direction: Option<AssignmentDirection>) -> Self {
        self.warm_direction = direction;
        self
    }

    pub(super) fn passive(mut self) -> Self {
        self.active = false;
        self
    }

    pub(super) fn with_selection(mut self, selectable: bool, marquee: Option<Bounds<Pixels>>) -> Self {
        self.selectable = selectable;
        self.marquee = marquee;
        self
    }

    pub(super) fn with_page_pixels(mut self, vertical: Pixels, reported_content_height: Option<Pixels>) -> Self {
        self.vertical_pixels = vertical;
        self.reported_content_height = reported_content_height;
        self
    }

    pub(super) fn with_drag_offset(mut self, offset: Point<Pixels>) -> Self {
        self.drag_offset = offset;
        self
    }

    pub(super) fn with_page_offset(mut self, horizontal: f32, vertical: f32) -> Self {
        self.horizontal_offset = horizontal;
        self.vertical_offset = vertical;
        self
    }

    /// Handles a secondary click left unanswered by a child. Registered before
    /// child paint, since GPUI bubbles mouse listeners in reverse order.
    fn paint_secondary_input(&self, rects: Vec<(usize, Bounds<Pixels>)>, hitbox: Hitbox, window: &mut Window) {
        let paginator = self.paginator.clone();
        let secondary_input = move |position: Point<Pixels>, window: &mut Window, cx: &mut App| {
            let pressed_child = rects.iter().find(|(_, bounds)| bounds.contains(&position)).map(|(index, _)| *index);
            let Some(paginator) = paginator.upgrade() else { return };
            let state = paginator.read(cx);
            let answers = pressed_child.is_none_or(|index| state.selection().contains(&index));
            if let Some(handler) = state.on_context_menu.clone().filter(|_| answers) {
                let id = pressed_child.and_then(|index| state.child_id(index).cloned());
                handler(id, position, window, cx);
            }
        };
        if components::uses_mobile_navigation(window) {
            // A long press gets the first chance to open the item's menu.
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Right && hitbox.is_hovered(window) {
                    secondary_input(event.position, window, cx);
                }
            });
        } else {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Right && hitbox.is_hovered(window) {
                    secondary_input(event.position, window, cx);
                }
            });
        }
    }

    /// Installs the rubber band's mouse handling.
    ///
    /// Down is gated on the paginator's own hitbox; move and up deliberately
    /// are not, so a band survives the pointer leaving it mid-drag.
    fn paint_selection_input(&self, container: Hitbox, child_rects: Vec<(usize, Bounds<Pixels>)>, window: &mut Window) {
        let paginator = self.paginator.clone();
        let rects = child_rects.clone();
        let hitbox = container.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                return;
            }
            let pressed_child = rects.iter().find(|(_, bounds)| bounds.contains(&event.position)).map(|(index, _)| *index);
            let _ = paginator.update(cx, |paginator, cx| match pressed_child {
                // Pressing something already selected begins a drag of the
                // whole selection, so the selection has to survive the press.
                Some(index) if paginator.selection().contains(&index) => {}
                Some(index) if paginator.select_on_drag && paginator.data.children[index].selection_start => {
                    paginator.begin_marquee(event.position, cx);
                }
                Some(_) => {
                    paginator.clear_selection(cx);
                }
                None => {
                    paginator.begin_marquee(event.position, cx);
                }
            });
        });

        let paginator = self.paginator.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble || !event.dragging() {
                return;
            }
            // A press that carried an item is a drag, which the drop targets
            // answer for themselves; a press that carried nothing is a rubber
            // band. The two cannot be in flight at once.
            if cx.has_active_drag() {
                return;
            }
            let _ = paginator.update(cx, |paginator, cx| paginator.drag_marquee(event.position, cx));
        });

        let paginator = self.paginator.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let _ = paginator.update(cx, |paginator, cx| paginator.end_marquee(cx));
        });
    }

    fn prepare_page(&self, page: VisiblePage, window: &mut Window, cx: &mut App) {
        for child in self.data.children.range(page.first_child..page.last_child + 1) {
            if let Some(prepare) = &child.prepare {
                prepare(window, cx);
            }
        }
    }

    fn page_layout(&self, bounds: Bounds<Pixels>, child_offset: usize, direction: AssignmentDirection, forced_page: Option<VisiblePage>) -> Rc<PageLayout> {
        self.layout_cache.borrow_mut().resolve(LayoutKey {
            data: self.data.clone(),
            widths: self.widths.clone(),
            width: self.current_page_width(bounds.size.width),
            height: self.current_page_height(bounds.size.height),
            rem_size: self.rem_size,
            child_offset,
            direction,
            forced_page,
        })
    }

    fn adjacent_layout(&self, page: VisiblePage, direction: AssignmentDirection, bounds: Bounds<Pixels>) -> Option<Rc<PageLayout>> {
        let child_offset = page.adjacent_offset(direction, self.data.children.len())?;
        Some(self.page_layout(bounds, child_offset, direction, None))
    }

    fn prepare_adjacent_pages(&self, page: VisiblePage, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        for direction in [AssignmentDirection::Backward, AssignmentDirection::Forward] {
            if let Some(layout) = self.adjacent_layout(page, direction, bounds) {
                if let Some(page) = layout.page {
                    self.prepare_page(page, window, cx);
                }
            }
        }
    }

    fn warm_adjacent_page(&self, page: VisiblePage, bounds: Bounds<Pixels>, source: &PrefetchKey, window: &mut Window, cx: &mut App) {
        let Some(direction) = self.warm_direction else { return };
        let needed = self.paginator.update(cx, |paginator, _| paginator.warmed_page.as_ref().is_none_or(|warmed| warmed.direction != direction || warmed.source != *source || !Rc::ptr_eq(&warmed.widths, &self.widths))).unwrap_or(false);
        if !needed {
            return;
        }
        let request = self
            .paginator
            .update(cx, |paginator, _| {
                if let Some(request) = &paginator.warm_request {
                    if request.source == *source && request.direction == direction {
                        return (request.ready.clone(), false);
                    }
                }
                let ready = Rc::new(std::cell::Cell::new(false));
                paginator.warm_request = Some(super::PageWarmup { source: source.clone(), direction, ready: ready.clone() });
                (ready, true)
            })
            .ok();
        let Some((ready, schedule)) = request else { return };
        if !ready.get() {
            if schedule {
                // Present the first drag frame before doing speculative work.
                let paginator = self.paginator.clone();
                window.on_next_frame(move |_, cx| {
                    let _ = paginator.update(cx, |paginator, cx| {
                        if paginator.swipe.is_some() && paginator.warm_request.as_ref().is_some_and(|request| Rc::ptr_eq(&request.ready, &ready)) {
                            ready.set(true);
                            cx.notify();
                        }
                    });
                });
            }
            return;
        }
        let Some(layout) = self.adjacent_layout(page, direction, bounds) else { return };
        let Some(page) = layout.page else { return };
        // Warm only the requested page's layout. No prepaint or paint means
        // no offscreen hitboxes, selection handlers or recursive prefetch.
        let measured = self.measure_assignments(&layout.rows, bounds.size.height, window, cx);
        let content_height = measured_content_height(&measured);
        let source = source.clone();
        let widths = self.widths.clone();
        let _ = self.paginator.update(cx, |paginator, _| {
            paginator.warmed_page = Some(super::WarmedPage { source, widths, direction, page, content_height });
        });
    }

    /// Builds a row at the height its policy declares. Children are laid out
    /// at a definite size rather than measured, so a short child no longer
    /// compacts the row it sits in and every row height is known in advance.
    fn measure_row(&self, assignment: &AssignedRow, maximum_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredRow {
        let policy = self.data.groups[assignment.group_index].policy;
        let height = policy.sizing.height(assignment.child_width, self.rem_size).min(maximum_height.max(px(1.0))).max(px(1.0));
        let mut items = Vec::with_capacity(assignment.items.len());

        for item in &assignment.items {
            let child = &self.data.children[item.child_index];
            let content = (child.render)(&self.state, window, cx);
            let mut element = if child.owns_cell && !policy.item_shadow {
                content
            } else {
                div()
                    .w(item.width)
                    .h(height)
                    .overflow_hidden()
                    .when(policy.item_shadow, |cell| {
                        let theme = components::browser_theme(cx);
                        cell.bg(theme.page_bg).shadow(components::section_card_shadow(theme))
                    })
                    .child(content)
                    .into_any_element()
            };
            element.layout_as_root(size(AvailableSpace::Definite(item.width), AvailableSpace::Definite(height)), window, cx);
            items.push(MeasuredItem { child_index: item.child_index, x: item.x, width: item.width, element });
        }

        MeasuredRow {
            group_index: assignment.group_index,
            column_gap: assignment.column_gap,
            width: assignment.items.iter().map(|item| item.x + item.width).fold(Pixels::ZERO, Pixels::max),
            items,
            height,
            gap_before: Pixels::ZERO,
            top: assignment.top,
        }
    }

    fn minimum_next_page_peek(&self, available_extent: Pixels) -> Pixels {
        if !self.show_next_page_preview || !self.reserve_preview_space || self.reported_visible_page.is_none_or(|page| page.last_child.saturating_add(1) >= self.data.children.len()) {
            return Pixels::ZERO;
        }
        px(NEXT_PAGE_PEEK_PX.min(f32::from(available_extent)))
    }

    fn current_page_width(&self, available_width: Pixels) -> Pixels {
        if self.flow_axis == PaginatorAxis::Horizontal { (available_width - self.minimum_next_page_peek(available_width)).max(px(1.0)) } else { available_width }
    }

    fn current_page_height(&self, available_height: Pixels) -> Pixels {
        if self.flow_axis == PaginatorAxis::Vertical { (available_height - self.minimum_next_page_peek(available_height)).max(px(1.0)) } else { available_height }
    }

    fn measure_next_preview(&self, page: VisiblePage, geometries: &[GroupGeometry], available_height: Pixels, window: &mut Window, cx: &mut App) -> Option<MeasuredRow> {
        let mut next_child = page.last_child.saturating_add(1);
        let assignment = geometry::next_forward_row(&self.data, &self.widths, geometries, &mut next_child)?;
        Some(self.measure_row(&assignment, available_height, window, cx))
    }

    fn measure_assignments(&self, assignments: &[AssignedRow], maximum_height: Pixels, window: &mut Window, cx: &mut App) -> Vec<MeasuredRow> {
        let mut rows = assignments.iter().map(|assignment| self.measure_row(assignment, maximum_height, window, cx)).collect::<Vec<_>>();
        set_vertical_gaps(&self.data, &mut rows);
        rows
    }
}

impl IntoElement for PaginatorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PaginatorElement {
    type RequestLayoutState = ();
    type PrepaintState = PaginatorPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        (self.widths, self.layout_cache) = self
            .paginator
            .update(cx, |paginator, cx| (paginator.measure_widths(self.children_revision, &self.data, window, cx), paginator.layout_cache.clone()))
            .unwrap_or_else(|_| (super::measure_widths(&self.data, window, cx), Rc::default()));
        let mut style = Style::default();
        style.size.width = relative(1.0).into();

        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _layout: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        let layout = self.page_layout(bounds, self.child_offset, self.direction, self.forced_page);
        let mut page = self.measure_assignments(&layout.rows, bounds.size.height, window, cx);
        let geometries = &layout.geometries;
        let visible_page = measured_visible_page(&page);

        if self.active {
            if let Some(visible_page) = visible_page {
                let prefetch_key =
                    PrefetchKey { children_revision: self.children_revision, page: visible_page, width: self.current_page_width(bounds.size.width), height: self.current_page_height(bounds.size.height), rem_size: self.rem_size };
                self.warm_adjacent_page(visible_page, bounds, &prefetch_key, window, cx);
                if self.prefetched.as_ref() != Some(&prefetch_key) {
                    self.prepare_adjacent_pages(visible_page, bounds, window, cx);
                    let paginator = self.paginator.clone();
                    window.defer(cx, move |_window, cx| {
                        let _ = paginator.update(cx, |paginator, _| paginator.set_prefetched(prefetch_key));
                    });
                }
            }

            let content_height: Pixels = measured_content_height(&page);
            if self.reported_content_height != Some(content_height) {
                let paginator = self.paginator.clone();
                let children_revision = self.children_revision;
                window.defer(cx, move |_window, cx| {
                    let _ = paginator.update(cx, |paginator, cx| paginator.set_content_height(children_revision, content_height, cx));
                });
            }

            if self.reported_visible_page != visible_page {
                let paginator = self.paginator.clone();
                let children_revision = self.children_revision;
                window.defer(cx, move |_window, cx| {
                    let _ = paginator.update(cx, |paginator, cx| {
                        paginator.set_visible_page(children_revision, visible_page, cx);
                    });
                });
            }
        }

        let row_left = bounds.left() + bounds.size.width * self.horizontal_offset + self.drag_offset.x;
        let mut row_top = bounds.top() + bounds.size.height * self.vertical_offset + self.drag_offset.y + self.vertical_pixels;
        let mut elements = Vec::new();
        // Only the page the reader is on: the peek row below it belongs to the
        // next page, and a band drawn over this one must not reach it.
        let mut child_rects = Vec::new();
        for row in &mut page {
            row_top = row.top.map_or(row_top + row.gap_before, |top| bounds.top() + bounds.size.height * self.vertical_offset + self.drag_offset.y + self.vertical_pixels + top);
            for item in row.items.drain(..) {
                let origin = point(row_left + item.x, row_top);
                let item_bounds = Bounds { origin, size: size(item.width, row.height) };
                let mut element = item.element;
                element.prepaint_at(origin, window, cx);
                let hit_bounds = self.data.children[item.child_index].hit_bounds.as_ref().map_or(item_bounds, |bounds| bounds.get());
                if self.selectable && self.active && hit_bounds.intersects(&bounds) {
                    child_rects.push((item.child_index, hit_bounds));
                }
                elements.push(element);
            }
            row_top += row.height;
        }

        if self.show_next_page_preview
            && let Some(visible_page) = visible_page
            && let Some(mut preview) = self.measure_next_preview(visible_page, &geometries, bounds.size.height, window, cx)
        {
            match self.flow_axis {
                PaginatorAxis::Horizontal => {
                    let occupied_width = measured_page_width(&page);
                    let natural_left = row_left + occupied_width + preview.column_gap;
                    let reserved_left = bounds.left() + self.current_page_width(bounds.size.width) + bounds.size.width * self.horizontal_offset + self.drag_offset.x;
                    let preview_left = natural_left.min(reserved_left);
                    let preview_top = bounds.top() + bounds.size.height * self.vertical_offset + self.drag_offset.y;
                    for item in preview.items.drain(..) {
                        let origin = point(preview_left + item.x, preview_top);
                        let mut element = item.element;
                        element.prepaint_at(origin, window, cx);
                        elements.push(element);
                    }
                }
                PaginatorAxis::Vertical => {
                    let gap_before = page.last().map_or(Pixels::ZERO, |row| geometry::gap_between(&self.data, row.group_index, preview.group_index));
                    let natural_top = row_top + gap_before;
                    let reserved_top = bounds.top() + self.current_page_height(bounds.size.height) + bounds.size.height * self.vertical_offset + self.drag_offset.y + self.vertical_pixels;
                    let preview_top = natural_top.min(reserved_top);
                    for item in preview.items.drain(..) {
                        let origin = point(row_left + item.x, preview_top);
                        let mut element = item.element;
                        element.prepaint_at(origin, window, cx);
                        elements.push(element);
                    }
                }
            }
        }
        let container = (self.selectable && self.active).then(|| {
            let paginator = self.paginator.clone();
            let reported = child_rects.clone();
            // Reported from the frame that drew them, so a mouse event arriving
            // before the next frame hit-tests against what is on screen.
            window.defer(cx, move |_window, cx| {
                let _ = paginator.update(cx, |paginator, _| paginator.set_child_bounds(reported));
            });
            window.insert_hitbox(bounds, HitboxBehavior::Normal)
        });
        PaginatorPrepaint { elements, container, child_rects }
    }

    fn paint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _layout: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        if let Some(container) = prepaint.container.clone() {
            self.paint_secondary_input(prepaint.child_rects.clone(), container, window);
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for element in &mut prepaint.elements {
                element.paint(window, cx);
            }
            // The band is a wash over the items it is picking up, not a panel:
            // it draws after them so the selection reads through it.
            if let Some(marquee) = self.marquee {
                let theme = components::browser_theme(cx);
                // Translucent, because the band lies over the items it is
                // picking up and they are drawn in the accent it is a wash of:
                // an opaque fill would hide the very thing it is selecting.
                window.paint_quad(quad(marquee, px(0.0), theme.accent_wash.opacity(MARQUEE_FILL_OPACITY), px(1.0), theme.accent, BorderStyle::Solid));
            }
        });

        if let Some(container) = prepaint.container.clone() {
            self.paint_selection_input(container, prepaint.child_rects.clone(), window);
        }
    }
}

#[derive(Default)]
pub(super) struct PaginatorPrepaint {
    elements: Vec<AnyElement>,
    /// Present only while the paginator is selectable, and the only thing that
    /// distinguishes a press on empty space from one on a child.
    container: Option<Hitbox>,
    child_rects: Vec<(usize, Bounds<Pixels>)>,
}

struct MeasuredItem {
    child_index: usize,
    x: Pixels,
    width: Pixels,
    element: AnyElement,
}

struct MeasuredRow {
    group_index: usize,
    column_gap: Pixels,
    width: Pixels,
    items: Vec<MeasuredItem>,
    height: Pixels,
    gap_before: Pixels,
    top: Option<Pixels>,
}

fn measured_content_height(rows: &[MeasuredRow]) -> Pixels {
    let mut bottom = Pixels::ZERO;
    let mut extent = Pixels::ZERO;
    for row in rows {
        bottom = row.top.unwrap_or(bottom + row.gap_before) + row.height;
        extent = extent.max(bottom);
    }
    extent
}

fn measured_visible_page(page: &[MeasuredRow]) -> Option<VisiblePage> {
    let first_child = page.first()?.items.first()?.child_index;
    let last_child = page.last()?.items.last()?.child_index;
    Some(VisiblePage { first_child, last_child })
}

fn measured_page_width(rows: &[MeasuredRow]) -> Pixels {
    rows.iter().map(|row| row.width).fold(Pixels::ZERO, Pixels::max)
}

fn set_vertical_gaps(data: &PaginatorData, rows: &mut [MeasuredRow]) {
    for index in 1..rows.len() {
        rows[index].gap_before = geometry::gap_between(data, rows[index - 1].group_index, rows[index].group_index);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PaginatorChild, PaginatorGroup, PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy};
    use super::*;
    use gpui::{AppContext, Modifiers, TestAppContext, VisualTestContext, rgb};
    use std::cell::{Cell, RefCell};

    #[gpui::test]
    fn owned_cells_preserve_size_and_clip_paint_and_input(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for owns_cell in [false, true] {
            for item_shadow in [false, true] {
                for width in [90.0, 200.0] {
                    let presses = Rc::new(Cell::new(0));
                    let pressed = presses.clone();
                    let child = PaginatorChild::new(move |_, _, _| {
                        let pressed = pressed.clone();
                        div()
                            .id("cell")
                            .debug_selector(|| "cell".into())
                            .size_full()
                            .relative()
                            .overflow_hidden()
                            .child(div().id("overflow").absolute().left(px(60.0)).top(px(0.0)).w(px(80.0)).h(px(40.0)).bg(rgb(0xfe0102)).on_mouse_down(MouseButton::Left, move |_, _, _| pressed.set(pressed.get() + 1)))
                            .into_any_element()
                    });
                    let child = if owns_cell { child.with_own_cell() } else { child };
                    let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(80.0)), px(0.0)).with_gap(px(0.0)).with_maximum_width(PaginatorWidthPolicy::pixels(px(100.0))).pack_start();
                    let policy = if item_shadow { policy.with_item_shadow() } else { policy };
                    let window = cx.open_window(size(px(width), px(150.0)), |_, _| Paginator::new("cell-test", [PaginatorGroup::new(policy, [child])]).next_page_preview(false));
                    let visual = &mut VisualTestContext::from_window(window.into(), cx);
                    visual.run_until_parked();
                    let bounds = visual.debug_bounds("cell").expect("cell bounds");
                    assert_eq!(bounds.size, size(px(width.min(100.0)), px(80.0)));
                    visual.update(|window, cx| {
                        let quads = window.painted_quads();
                        let overflow = quads.iter().find(|quad| quad.background == rgb(0xfe0102).into()).expect("overflow paint");
                        assert_eq!(overflow.content_mask.bounds, bounds.scale(window.scale_factor()), "paint must stay inside the cell");
                        let background = components::browser_theme(cx).page_bg;
                        assert_eq!(quads.iter().any(|quad| quad.background == background.into() && quad.bounds == bounds.scale(window.scale_factor())), item_shadow, "shadow cells retain their background");
                    });
                    visual.simulate_mouse_down(bounds.origin + point(px(70.0), px(20.0)), MouseButton::Left, Modifiers::default());
                    visual.simulate_mouse_up(bounds.origin + point(px(70.0), px(20.0)), MouseButton::Left, Modifiers::default());
                    assert_eq!(presses.get(), 1);
                    let outside = point(bounds.right() + px(10.0), bounds.top() + px(20.0));
                    visual.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
                    visual.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
                    assert_eq!(presses.get(), 1, "clipped content must not receive input");
                }
            }
        }
    }

    #[gpui::test]
    fn remaining_space_previews_clip_the_next_cover_without_changing_page_membership(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for entries in [0, 2] {
            for width in [200.0, 250.0, 300.0, 450.0] {
                let window = cx.open_window(size(px(width), px(120.0)), |_, _| {
                    let mut entry_policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(300.0)), PaginatorSizing::pixels(px(24.0)), px(0.0)).with_gap(px(0.0));
                    entry_policy.column_fraction = Some(0.5);
                    let cover_policy =
                        PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(120.0)), px(0.0)).with_gap(px(0.0)).with_maximum_width(PaginatorWidthPolicy::pixels(px(100.0))).pack_start();
                    let groups = [
                        PaginatorGroup::new(entry_policy, (0..entries).map(|_| PaginatorChild::new(|_, _, _| div().size_full().into_any_element()))),
                        PaginatorGroup::new(cover_policy, (0..8).map(|index| PaginatorChild::new(move |_, _, _| div().size_full().bg(rgb(0x10a000 + index)).into_any_element()))),
                    ];
                    let mut flow = Paginator::new("remaining-space", groups).flow_axis(PaginatorAxis::Horizontal).next_page_preview_in_remaining_space().selectable();
                    flow.animate_page_transitions = false;
                    flow
                });
                let flow = window.root(cx).unwrap();
                let visual = &mut VisualTestContext::from_window(window.into(), cx);
                visual.run_until_parked();
                let entry_width = if entries == 0 { 0.0 } else { width * 0.5 };
                let covers = ((width - entry_width) / 100.0_f32).floor() as usize;
                let next = entries + covers;
                let occupied = entry_width + covers as f32 * 100.0;
                visual.update(|window, cx| {
                    let flow = flow.read(cx);
                    assert_eq!(flow.visible_page.unwrap().last_child + 1, next, "only whole covers belong to the page");
                    assert!(flow.child_bounds(next).is_none(), "the clipped preview must not join marquee hit bounds");
                    let preview_color = rgb(0x10a000 + covers as u32).into();
                    let preview = window.painted_quads().into_iter().find(|quad| quad.background == preview_color && quad.bounds.intersects(&quad.content_mask.bounds));
                    if occupied < width {
                        let preview = preview.expect("the next cover must fill the leftover width");
                        let painted = preview.bounds.intersect(&preview.content_mask.bounds);
                        let scale = window.scale_factor();
                        assert_eq!(painted.left().0, occupied * scale);
                        assert_eq!(painted.right().0, width * scale);
                    } else {
                        assert!(preview.is_none(), "an exactly full page must keep all its covers and reserve no peek");
                    }
                });
                visual.update(|_, cx| flow.update(cx, |flow, cx| flow.next_with_axis(PaginatorAxis::Horizontal, cx)));
                visual.run_until_parked();
                visual.update(|_, cx| assert_eq!(flow.read(cx).visible_page.unwrap().first_child, next, "the preview must be the first cover on the next page"));
            }
        }
    }

    #[gpui::test]
    fn adjacent_prefetch_prepares_only_the_neighbor_ranges_without_rendering(cx: &mut TestAppContext) {
        let prepared = Rc::new(RefCell::new(Vec::new()));
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(100.0)), px(0.0)).with_gap(px(0.0)).flush_edges();
        let children = (0..12).map(|index| {
            let prepared = prepared.clone();
            PaginatorChild::new(|_, _, _| panic!("prefetch must not render offscreen children")).with_prepare(move |_, _| prepared.borrow_mut().push(index))
        });
        let paginator = cx.new(|_| Paginator::new("prefetch-test", [PaginatorGroup::new(policy, children)]));
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            let page = PaginatorPage { reported_visible_page: None, child_offset: 4, direction: AssignmentDirection::Forward, prefetched: None, forced_page: None };
            let mut element = PaginatorElement::new("prefetch-test".into(), paginator.downgrade(), paginator.read(cx), page, px(16.0), Rc::default());
            element.widths = Rc::new(vec![Pixels::ZERO; element.data.children.len()].into());
            element.prepare_adjacent_pages(VisiblePage { first_child: 4, last_child: 7 }, Bounds::new(point(px(0.0), px(0.0)), size(px(200.0), px(200.0))), window, cx);
        });
        assert_eq!(*prepared.borrow(), [0, 1, 2, 3, 8, 9, 10, 11]);
    }
}
