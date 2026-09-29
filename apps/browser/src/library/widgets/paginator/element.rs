//! GPUI element implementation for the paginator.
//!
//! Row assignment and geometry stay in `geometry`; this module is responsible
//! only for child preparation, placement, and painting.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, AvailableSpace, BorderStyle, Bounds, ContentMask, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Style, WeakEntity, Window, div, point, px, quad, relative, size,
};
use ui_components as components;

use super::geometry::{self, AssignedRow, GroupGeometry};
use super::{AssignmentDirection, Paginator, PaginatorAxis, PaginatorData, PaginatorState, PrefetchKey, VisiblePage};

const NEXT_PAGE_PEEK_PX: f32 = 8.0;
/// The band is drawn over the items it selects, so it has to be see-through.
const MARQUEE_FILL_OPACITY: f32 = 0.35;

pub(super) struct PaginatorElement {
    id: ElementId,
    paginator: WeakEntity<Paginator>,
    reported_visible_page: Option<VisiblePage>,
    children_revision: usize,
    data: Rc<PaginatorData>,
    maximum_rows: Option<usize>,
    child_offset: usize,
    direction: AssignmentDirection,
    prefetched: Option<PrefetchKey>,
    /// Set only for the outgoing page of a transition, which must keep the
    /// range it had rather than resolve a fresh one.
    forced_page: Option<VisiblePage>,
    rem_size: Pixels,
    flow_axis: PaginatorAxis,
    state: Rc<PaginatorState>,
    active: bool,
    horizontal_offset: f32,
    vertical_offset: f32,
    /// Transition offset in pixels. Pages are panned by the height of the page
    /// between them, which is a measurement, not a fraction of the container —
    /// using the container's height is what left a gap between pages whenever a
    /// page did not fill it.
    vertical_pixels: Pixels,
    reported_content_height: Option<Pixels>,
    /// Rubber-band selection, drawn and driven only where the page asked for it.
    selectable: bool,
    /// The band as of this frame, in window coordinates.
    marquee: Option<Bounds<Pixels>>,
}

impl PaginatorElement {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        id: ElementId, paginator: WeakEntity<Paginator>, reported_visible_page: Option<VisiblePage>, children_revision: usize, data: Rc<PaginatorData>, maximum_rows: Option<usize>, child_offset: usize, direction: AssignmentDirection,
        prefetched: Option<PrefetchKey>, forced_page: Option<VisiblePage>, rem_size: Pixels, flow_axis: PaginatorAxis, state: Rc<PaginatorState>,
    ) -> Self {
        Self {
            id,
            paginator,
            reported_visible_page,
            children_revision,
            data,
            maximum_rows,
            child_offset,
            direction,
            prefetched,
            forced_page,
            rem_size,
            flow_axis,
            state,
            active: true,
            horizontal_offset: 0.0,
            vertical_offset: 0.0,
            vertical_pixels: Pixels::ZERO,
            reported_content_height: None,
            selectable: false,
            marquee: None,
        }
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

    pub(super) fn with_page_offset(mut self, horizontal: f32, vertical: f32) -> Self {
        self.horizontal_offset = horizontal;
        self.vertical_offset = vertical;
        self
    }

    /// Mouse activation is declared by the child, not inferred from its bounds:
    /// a book card is a cover plus two lines of text, and only the cover should
    /// open the book. `activate` remains the keyboard's route in, so Enter and a
    /// click still run the same callback.
    fn activation_hitbox(&self, _child_index: usize, _item_bounds: Bounds<Pixels>, _clip_bounds: Bounds<Pixels>, _window: &mut Window) -> Option<ActivationHitbox> {
        None
    }

    /// Handles a secondary click left unanswered by a child. Registered before
    /// child paint, since GPUI bubbles mouse listeners in reverse order.
    fn paint_secondary_input(&self, rects: Vec<(usize, Bounds<Pixels>)>, hitbox: Hitbox, window: &mut Window) {
        let paginator = self.paginator.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Right || !hitbox.is_hovered(window) {
                return;
            }
            let pressed_child = rects.iter().find(|(_, bounds)| bounds.contains(&event.position)).map(|(index, _)| *index);
            let Some(paginator) = paginator.upgrade() else { return };
            let state = paginator.read(cx);
            let answers = pressed_child.is_none_or(|index| state.selection().contains(&index));
            if let Some(handler) = state.on_context_menu.clone().filter(|_| answers) {
                handler(pressed_child, event.position, window, cx);
            }
        });
    }

    /// Installs the rubber band's mouse handling.
    ///
    /// Down is gated on the paginator's own hitbox; move and up deliberately
    /// are not, so a band survives the pointer leaving it mid-drag.
    fn paint_selection_input(&self, container: Hitbox, child_rects: Vec<(usize, Bounds<Pixels>)>, window: &mut Window) {
        if !components::uses_mobile_navigation(window) {
            // Desktop keeps the immediate right-down menu behavior. Mobile
            // waits for release so the card can consume the long press first.
            let paginator = self.paginator.clone();
            let rects = child_rects.clone();
            let hitbox = container.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Right || !hitbox.is_hovered(window) {
                    return;
                }
                let pressed_child = rects.iter().find(|(_, bounds)| bounds.contains(&event.position)).map(|(index, _)| *index);
                let Some(paginator) = paginator.upgrade() else { return };
                let state = paginator.read(cx);
                let answers = pressed_child.is_none_or(|index| state.selection().contains(&index));
                if let Some(handler) = state.on_context_menu.clone().filter(|_| answers) {
                    handler(pressed_child, event.position, window, cx);
                }
            });
        }
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
                Some(_) => { paginator.clear_selection(cx); }
                None => { paginator.begin_marquee(event.position, cx); }
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
        for child in &self.data.children[page.first_child..=page.last_child] {
            if let Some(prepare) = &child.prepare {
                prepare(window, cx);
            }
        }
    }

    fn prepare_adjacent_pages(&self, page: VisiblePage, geometries: &[GroupGeometry], available_height: Pixels, window: &mut Window, cx: &mut App) {
        if page.first_child > 0 {
            let previous = self.measure_page_from(page.first_child - 1, AssignmentDirection::Backward, geometries, available_height, window, cx);
            if let Some(previous_page) = measured_visible_page(&previous) {
                self.prepare_page(previous_page, window, cx);
            }
        }

        let next_child = page.last_child.saturating_add(1);
        if next_child < self.data.children.len() {
            let next = self.measure_page_from(next_child, AssignmentDirection::Forward, geometries, available_height, window, cx);
            if let Some(next_page) = measured_visible_page(&next) {
                self.prepare_page(next_page, window, cx);
            }
        }
    }

    /// Builds a row at the height its policy declares. Children are laid out
    /// at a definite size rather than measured, so a short child no longer
    /// compacts the row it sits in and every row height is known in advance.
    fn measure_row(&self, assignment: AssignedRow, maximum_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredRow {
        let policy = self.data.groups[assignment.group_index].policy;
        let height = policy.sizing.height(assignment.child_width, self.rem_size).min(maximum_height.max(px(1.0))).max(px(1.0));
        let mut items = Vec::with_capacity(assignment.items.len());

        for item in &assignment.items {
            let child = &self.data.children[item.child_index];
            let mut element = div().w(item.width).h(height).overflow_hidden().child((child.render)(&self.state, window, cx)).into_any_element();
            element.layout_as_root(size(AvailableSpace::Definite(item.width), AvailableSpace::Definite(height)), window, cx);
            items.push(MeasuredItem { child_index: item.child_index, x: item.x, width: item.width, element });
        }

        MeasuredRow { assignment, items, height, gap_before: Pixels::ZERO, header_height: Pixels::ZERO }
    }

    fn measure_page(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let available_height = self.current_page_height(bounds.size.height);
        let geometries = self.geometries(bounds);
        let page = self.forced_page.or_else(|| self.page_range(&geometries, available_height));
        let mut measured = page.map_or_else(|| MeasuredPage { rows: Vec::new() }, |page| self.measure_range(page, &geometries, available_height, window, cx));
        set_vertical_gaps(&self.data, &mut measured.rows);
        measured
    }

    fn minimum_next_page_peek(&self, available_extent: Pixels) -> Pixels {
        // A small peek cannot show a header and a row together.
        if self.reported_visible_page.and_then(|page| self.data.group_index_for_child(page.last_child.saturating_add(1))).is_some_and(|group| self.data.groups[group].header_sizing.is_some()) {
            return Pixels::ZERO;
        }
        if (self.flow_axis == PaginatorAxis::Vertical && self.maximum_rows.is_some()) || self.reported_visible_page.is_none_or(|page| page.last_child.saturating_add(1) >= self.data.children.len()) {
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
        let assignment = geometry::next_forward_row(&self.data, geometries, &mut next_child)?;
        if self.data.groups[assignment.group_index].header_sizing.is_some() {
            return None;
        }
        Some(self.measure_row(assignment, available_height, window, cx))
    }

    /// Geometry is a pure function of the data, the usable width and the rem
    /// size, so it is recomputed where needed rather than carried around.
    fn geometries(&self, bounds: Bounds<Pixels>) -> Vec<GroupGeometry> {
        geometry::group_geometries(&self.data, self.current_page_width(bounds.size.width), self.rem_size)
    }

    /// Which children the page covers. Row heights are known from the policy,
    /// so this settles the range without building anything.
    fn page_range(&self, geometries: &[GroupGeometry], available_height: Pixels) -> Option<VisiblePage> {
        let rows = geometry::page_rows(&self.data, geometries, self.direction, self.child_offset, self.maximum_rows, available_height, self.rem_size);
        let first_child = rows.first()?.items.first()?.child_index;
        let last_child = rows.last()?.items.last()?.child_index;
        Some(VisiblePage { first_child, last_child })
    }

    /// Builds a whole page in one pass. Which rows belong to it is decided by
    /// arithmetic before anything is built, in either direction.
    fn measure_page_from(&self, child_offset: usize, direction: AssignmentDirection, geometries: &[GroupGeometry], available_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let assignments = geometry::page_rows(&self.data, geometries, direction, child_offset, self.maximum_rows, available_height, self.rem_size);
        self.measure_assignments(assignments, available_height, window, cx)
    }

    fn measure_assignments(&self, assignments: Vec<AssignedRow>, maximum_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let mut rows = Vec::new();
        let mut used = Pixels::ZERO;
        for assignment in assignments {
            let previous = rows.last().map(|row: &MeasuredRow| &row.assignment);
            let header_height = geometry::row_header_height(&self.data, previous, &assignment, self.rem_size);
            let gap = previous.map_or(Pixels::ZERO, |previous| geometry::gap_between(&self.data, previous, &assignment));
            let nominal_height = self.data.groups[assignment.group_index].policy.sizing.height(assignment.child_width, self.rem_size);
            // A forced outgoing range can outlive a viewport resize. Even then,
            // never paint a new heading unless its first row fits with it.
            if header_height > Pixels::ZERO && used + gap + header_height + nominal_height > maximum_height {
                break;
            }
            let mut row = self.measure_row(assignment, maximum_height - header_height, window, cx);
            row.header_height = header_height;
            used += gap + header_height + row.height;
            rows.push(row);
        }
        set_vertical_gaps(&self.data, &mut rows);
        MeasuredPage { rows }
    }

    fn measure_range(&self, page: VisiblePage, geometries: &[GroupGeometry], maximum_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let assignments = geometry::forward_rows_in_range(&self.data, geometries, page.first_child, page.last_child.saturating_add(1));
        self.measure_assignments(assignments, maximum_height, window, cx)
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
        for child in &self.data.children {
            if let Some(measure) = &child.intrinsic_width {
                child.measured_width.set(measure(window, cx));
            }
        }
        let mut style = Style::default();
        style.size.width = relative(1.0).into();

        if self.maximum_rows.is_none() {
            style.size.height = relative(1.0).into();
            return (window.request_layout(style, [], cx), ());
        }

        let data = self.data.clone();
        let maximum_rows = self.maximum_rows;
        let child_offset = self.child_offset;
        let direction = self.direction;
        let rem_size = self.rem_size;
        let layout_id = window.request_measured_layout(style, move |known_dimensions, available_space, _window, _cx| {
            let fallback_width = geometry::fallback_width(&data, rem_size);
            let width = known_dimensions.width.unwrap_or(match available_space.width {
                AvailableSpace::Definite(width) => width,
                AvailableSpace::MinContent | AvailableSpace::MaxContent => fallback_width,
            });
            let available_height = match available_space.height {
                AvailableSpace::Definite(height) => height,
                AvailableSpace::MinContent | AvailableSpace::MaxContent => px(f32::MAX / 4.0),
            };
            let geometries = geometry::group_geometries(&data, width, rem_size);
            let rows = geometry::page_rows(&data, &geometries, direction, child_offset, maximum_rows, available_height, rem_size);
            size(width, geometry::rows_height(&data, &rows, rem_size).min(available_height))
        });
        (layout_id, ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _layout: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        let mut page = self.measure_page(bounds, window, cx);
        let geometries = self.geometries(bounds);
        let visible_page = measured_visible_page(&page);

        if self.active {
            if let Some(visible_page) = visible_page {
                let prefetch_key =
                    PrefetchKey { children_revision: self.children_revision, page: visible_page, width: self.current_page_width(bounds.size.width), height: self.current_page_height(bounds.size.height), rem_size: self.rem_size };
                if self.prefetched.as_ref() != Some(&prefetch_key) {
                    self.prepare_adjacent_pages(visible_page, &geometries, bounds.size.height, window, cx);
                    let paginator = self.paginator.clone();
                    window.defer(cx, move |_window, cx| {
                        let _ = paginator.update(cx, |paginator, cx| paginator.set_prefetched(prefetch_key, cx));
                    });
                }
            }

            let content_height: Pixels = page.rows.iter().map(|row| row.height + row.gap_before + row.header_height).fold(Pixels::ZERO, |total, height| total + height);
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

        let row_left = bounds.left() + bounds.size.width * self.horizontal_offset;
        let mut row_top = bounds.top() + bounds.size.height * self.vertical_offset + self.vertical_pixels;
        let mut elements = Vec::new();
        let mut activations = Vec::new();
        // Only the page the reader is on: the peek row below it belongs to the
        // next page, and a band drawn over this one must not reach it.
        let mut child_rects = Vec::new();
        // A headed group reads as raised off the page: the panel is the
        // header's top to the last row's bottom, at the header's own inset —
        // tracked here because the paginator draws rows one at a time and this
        // is the one place that knows where a group starts and ends.
        let mut group_panels = Vec::new();
        let mut grid_rows = Vec::new();
        let mut open_panel: Option<(Pixels, Pixels, Pixels)> = None;
        let row_count = page.rows.len();
        for row_index in 0..row_count {
            let next_grid_contiguous = page.rows.get(row_index + 1).is_some_and(|next| {
                let policy = self.data.groups[next.assignment.group_index].policy;
                policy.grid_rules && next.gap_before == Pixels::ZERO && next.header_height == Pixels::ZERO
            });
            let row = &mut page.rows[row_index];
            row_top += row.gap_before;
            let group_index = row.assignment.group_index;
            if row.header_height > Pixels::ZERO {
                if let Some(header) = &self.data.headers[group_index] {
                    let inset = self.data.groups[group_index].policy.edge_inset;
                    let width = (self.current_page_width(bounds.size.width) - inset * 2.0).max(px(1.0));
                    let mut element = div().w(width).h(row.header_height).overflow_hidden().child((header.render)(window, cx)).into_any_element();
                    element.layout_as_root(size(AvailableSpace::Definite(width), AvailableSpace::Definite(row.header_height)), window, cx);
                    element.prepaint_at(point(row_left + inset, row_top), window, cx);
                    elements.push(element);
                    open_panel = Some((row_top, row_left + inset, width));
                }
                row_top += row.header_height;
            }
            if self.data.groups[group_index].policy.grid_rules {
                if let Some(grid_row) = GridRow::from_row(row, row_left, row_top, !next_grid_contiguous) {
                    grid_rows.push(grid_row);
                }
            }
            for item in row.items.drain(..) {
                let origin = point(row_left + item.x, row_top);
                let item_bounds = Bounds { origin, size: size(item.width, row.height) };
                if let Some(activation) = self.activation_hitbox(item.child_index, item_bounds, bounds, window) {
                    activations.push(activation);
                }
                if self.selectable && self.active && item_bounds.intersects(&bounds) {
                    child_rects.push((item.child_index, item_bounds));
                }
                let mut element = item.element;
                element.prepaint_at(origin, window, cx);
                elements.push(element);
            }
            row_top += row.height;
            let group_changes = page.rows.get(row_index + 1).is_none_or(|next| next.assignment.group_index != group_index);
            if group_changes && let Some((top, left, width)) = open_panel.take() {
                group_panels.push(Bounds { origin: point(left, top), size: size(width, row_top - top) });
            }
        }

        if let Some(visible_page) = visible_page
            && let Some(mut preview) = self.measure_next_preview(visible_page, &geometries, bounds.size.height, window, cx)
        {
            match self.flow_axis {
                PaginatorAxis::Horizontal => {
                    let occupied_width = measured_page_width(&page.rows);
                    let natural_left = row_left + occupied_width + preview.assignment.column_gap;
                    let reserved_left = bounds.left() + self.current_page_width(bounds.size.width) + bounds.size.width * self.horizontal_offset;
                    let preview_left = natural_left.min(reserved_left);
                    let preview_top = bounds.top() + bounds.size.height * self.vertical_offset;
                    if self.data.groups[preview.assignment.group_index].policy.grid_rules {
                        if let Some(grid_row) = GridRow::from_row(&preview, preview_left, preview_top, true) {
                            grid_rows.push(grid_row);
                        }
                    }
                    for item in preview.items.drain(..) {
                        let origin = point(preview_left + item.x, preview_top);
                        if let Some(activation) = self.activation_hitbox(item.child_index, Bounds { origin, size: size(item.width, preview.height) }, bounds, window) {
                            activations.push(activation);
                        }
                        let mut element = item.element;
                        element.prepaint_at(origin, window, cx);
                        elements.push(element);
                    }
                }
                PaginatorAxis::Vertical => {
                    let gap_before = page.rows.last().map_or(Pixels::ZERO, |row| geometry::gap_between(&self.data, &row.assignment, &preview.assignment));
                    let natural_top = row_top + gap_before;
                    let reserved_top = bounds.top() + self.current_page_height(bounds.size.height) + bounds.size.height * self.vertical_offset;
                    let preview_top = natural_top.min(reserved_top);
                    if self.data.groups[preview.assignment.group_index].policy.grid_rules {
                        if let Some(grid_row) = GridRow::from_row(&preview, row_left, preview_top, true) {
                            grid_rows.push(grid_row);
                        }
                    }
                    for item in preview.items.drain(..) {
                        let origin = point(row_left + item.x, preview_top);
                        if let Some(activation) = self.activation_hitbox(item.child_index, Bounds { origin, size: size(item.width, preview.height) }, bounds, window) {
                            activations.push(activation);
                        }
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
        PaginatorPrepaint { elements, activations, container, child_rects, group_panels, grid_rows }
    }

    fn paint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _layout: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        if components::uses_mobile_navigation(window) && let Some(container) = prepaint.container.clone() {
            self.paint_secondary_input(prepaint.child_rects.clone(), container, window);
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if !prepaint.group_panels.is_empty() {
                let theme = components::browser_theme(cx);
                for panel in &prepaint.group_panels {
                    window.paint_quad(quad(*panel, px(0.0), theme.raised_bg, px(1.0), theme.rule, BorderStyle::Solid));
                }
            }
            for element in &mut prepaint.elements {
                element.paint(window, cx);
            }
            if !prepaint.grid_rows.is_empty() {
                let rule = components::browser_theme(cx).rule;
                for row in &prepaint.grid_rows {
                    row.paint(rule, window);
                }
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

        for activation in &prepaint.activations {
            let hitbox = activation.hitbox.clone();
            let activate = activation.activate.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && hitbox.is_hovered(window) {
                    activate(window, cx);
                }
            });
        }
    }
}

struct ActivationHitbox {
    hitbox: gpui::Hitbox,
    activate: Rc<super::ActivateChild>,
}

#[derive(Default)]
pub(super) struct PaginatorPrepaint {
    elements: Vec<AnyElement>,
    activations: Vec<ActivationHitbox>,
    /// Present only while the paginator is selectable, and the only thing that
    /// distinguishes a press on empty space from one on a child.
    container: Option<Hitbox>,
    child_rects: Vec<(usize, Bounds<Pixels>)>,
    group_panels: Vec<Bounds<Pixels>>,
    grid_rows: Vec<GridRow>,
}

/// One row of edge-to-edge cards. Each seam is drawn once: the next card owns
/// its left line, the next row owns its top line, and only outer edges close.
struct GridRow {
    top: Pixels,
    bottom: Pixels,
    left: Pixels,
    right: Pixels,
    cell_lefts: Vec<Pixels>,
    close_bottom: bool,
}

impl GridRow {
    fn from_row(row: &MeasuredRow, origin_x: Pixels, top: Pixels, close_bottom: bool) -> Option<Self> {
        let first = row.items.first()?;
        let last = row.items.last()?;
        Some(Self {
            top,
            bottom: top + row.height,
            left: origin_x + first.x,
            right: origin_x + last.x + last.width,
            cell_lefts: row.items.iter().map(|item| origin_x + item.x).collect(),
            close_bottom,
        })
    }

    fn paint(&self, color: gpui::Hsla, window: &mut Window) {
        let line = |left: Pixels, top: Pixels, width: Pixels, height: Pixels, window: &mut Window| {
            window.paint_quad(quad(Bounds { origin: point(left, top), size: size(width, height) }, px(0.0), color, px(0.0), color, BorderStyle::Solid));
        };
        let width = (self.right - self.left).max(px(1.0));
        let height = (self.bottom - self.top).max(px(1.0));
        line(self.left, self.top, width, px(1.0), window);
        for &left in &self.cell_lefts {
            line(left, self.top, px(1.0), height, window);
        }
        line(self.right - px(1.0), self.top, px(1.0), height, window);
        if self.close_bottom {
            line(self.left, self.bottom - px(1.0), width, px(1.0), window);
        }
    }
}

struct MeasuredItem {
    child_index: usize,
    x: Pixels,
    width: Pixels,
    element: AnyElement,
}

struct MeasuredRow {
    assignment: AssignedRow,
    items: Vec<MeasuredItem>,
    height: Pixels,
    gap_before: Pixels,
    header_height: Pixels,
}

struct MeasuredPage {
    rows: Vec<MeasuredRow>,
}

fn measured_visible_page(page: &MeasuredPage) -> Option<VisiblePage> {
    let first_child = page.rows.first()?.assignment.items.first()?.child_index;
    let last_child = page.rows.last()?.assignment.items.last()?.child_index;
    Some(VisiblePage { first_child, last_child })
}

fn measured_page_width(rows: &[MeasuredRow]) -> Pixels {
    rows.iter().flat_map(|row| row.assignment.items.iter().map(move |item| item.x + item.width)).fold(Pixels::ZERO, Pixels::max)
}

fn set_vertical_gaps(data: &PaginatorData, rows: &mut [MeasuredRow]) {
    for index in 1..rows.len() {
        rows[index].gap_before = geometry::gap_between(data, &rows[index - 1].assignment, &rows[index].assignment);
    }
}
