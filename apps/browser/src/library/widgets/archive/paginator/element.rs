//! GPUI element implementation for the paginator.
//!
//! Row assignment and geometry stay in `geometry`; this module is responsible
//! only for intrinsic child measurement, preparation, placement, and painting.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, DispatchPhase, Element, ElementId, GlobalElementId, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, Pixels, Style, WeakEntity, Window, div,
    point, px, relative, size,
};

use super::geometry::{self, AssignedRow, GroupGeometry};
use super::placement::{self, Placement, ProfileRow};
use super::{AssignmentDirection, GeometrySelection, GeometrySelectionKey, Paginator, PaginatorAxis, PaginatorData, PaginatorStages, PrefetchKey, RowGapMode, RowHeightMode, VisiblePage};

const NEXT_PAGE_PEEK_PX: f32 = 8.0;

pub(super) struct PaginatorElement {
    id: ElementId,
    paginator: WeakEntity<Paginator>,
    reported_visible_page: Option<VisiblePage>,
    children_revision: usize,
    data: Rc<PaginatorData>,
    stages: PaginatorStages,
    maximum_rows: Option<usize>,
    child_offset: usize,
    direction: AssignmentDirection,
    prefetched: Option<PrefetchKey>,
    geometry_selection: Option<GeometrySelection>,
    rem_size: Pixels,
    flow_axis: PaginatorAxis,
    active: bool,
    horizontal_offset: f32,
    vertical_offset: f32,
}

impl PaginatorElement {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        id: ElementId, paginator: WeakEntity<Paginator>, reported_visible_page: Option<VisiblePage>, children_revision: usize, data: Rc<PaginatorData>, stages: PaginatorStages, maximum_rows: Option<usize>, child_offset: usize,
        direction: AssignmentDirection, prefetched: Option<PrefetchKey>, geometry_selection: Option<GeometrySelection>, rem_size: Pixels, flow_axis: PaginatorAxis,
    ) -> Self {
        Self { id, paginator, reported_visible_page, children_revision, data, stages, maximum_rows, child_offset, direction, prefetched, geometry_selection, rem_size, flow_axis, active: true, horizontal_offset: 0.0, vertical_offset: 0.0 }
    }

    pub(super) fn passive(mut self) -> Self {
        self.active = false;
        self
    }

    pub(super) fn with_page_offset(mut self, horizontal: f32, vertical: f32) -> Self {
        self.horizontal_offset = horizontal;
        self.vertical_offset = vertical;
        self
    }

    fn activation_hitbox(&self, child_index: usize, item_bounds: Bounds<Pixels>, clip_bounds: Bounds<Pixels>, window: &mut Window) -> Option<ActivationHitbox> {
        if !self.active {
            return None;
        }
        let Some(activate) = self.data.children[child_index].activate.clone() else {
            return None;
        };
        let visible_bounds = item_bounds.intersect(&clip_bounds);
        if visible_bounds.size.width <= Pixels::ZERO || visible_bounds.size.height <= Pixels::ZERO {
            return None;
        }
        let hitbox = window.insert_hitbox(visible_bounds, HitboxBehavior::Normal);
        Some(ActivationHitbox { hitbox, activate })
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
            let previous = self.measure_backward_from(page.first_child - 1, geometries, available_height, window, cx);
            if let Some(previous_page) = measured_visible_page(&previous) {
                self.prepare_page(previous_page, window, cx);
            }
        }

        let next_child = page.last_child.saturating_add(1);
        if next_child < self.data.children.len() {
            let next = self.measure_forward_from(next_child, geometries, available_height, window, cx);
            if let Some(next_page) = measured_visible_page(&next) {
                self.prepare_page(next_page, window, cx);
            }
        }
    }

    fn measure_row(&self, assignment: AssignedRow, maximum_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredRow {
        let policy = self.data.groups[assignment.group_index].policy;
        let policy_height = policy.sizing.height(assignment.child_width, self.rem_size);
        let maximum_height = maximum_height.max(px(1.0));
        let constrained_height = policy_height > maximum_height;
        let sizing_height = policy_height.min(maximum_height);
        let mut items = Vec::with_capacity(assignment.items.len());
        let mut height = Pixels::ZERO;

        for item in &assignment.items {
            let child = &self.data.children[item.child_index];
            let mut element =
                div().w(assignment.child_width).when(constrained_height, |item| item.h(sizing_height)).when(!constrained_height, |item| item.max_h(sizing_height)).overflow_hidden().child((child.render)(window, cx)).into_any_element();
            let measured = element.layout_as_root(size(AvailableSpace::Definite(assignment.child_width), AvailableSpace::MinContent), window, cx);
            height = height.max(measured.height.min(sizing_height));
            items.push(MeasuredItem { child_index: item.child_index, column: item.column, element });
        }

        MeasuredRow { assignment, items, height: height.max(px(1.0)), gap_before: Pixels::ZERO }
    }

    fn measure_page(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let stages = self.stages.effective();
        let available_width = self.current_page_width(bounds.size.width);
        let available_height = self.current_page_height(bounds.size.height);
        let selection_key = self.geometry_selection_key(bounds);
        if let Some(selection) = &self.geometry_selection
            && selection.key == selection_key
            && selection.geometries.len() == self.data.groups.len()
        {
            let mut measured =
                selection.page.map_or_else(|| MeasuredPage { rows: Vec::new(), geometries: selection.geometries.clone() }, |page| self.measure_range(page, &selection.geometries, selection.row_heights, available_height, window, cx));
            apply_vertical_gaps(&self.data, &mut measured.rows, available_height, selection.row_gaps, stages.distribute_gaps);
            return measured;
        }

        let starting_geometries = if stages.expand_widths { geometry::minimum_gap_preferred_geometries(&self.data, available_width, self.rem_size) } else { geometry::preferred_geometries(&self.data, available_width, self.rem_size) };
        let mut intrinsic_heights = HashMap::new();
        let candidates = geometry::expansion_geometries(&self.data, available_width, self.rem_size);
        let mut selected = self.estimated_placement(&starting_geometries, available_height, stages.expand_widths);
        let mut row_heights = RowHeightMode::Policy;
        let row_gaps = if stages.expand_widths { RowGapMode::Minimum } else { RowGapMode::Preferred };

        if let Some(page) = selected.visible_page() {
            let policy_placement = self.profile_exact_policy_range(page, &selected.geometries, available_height);
            selected = self.refine_locked_policy_range(page, &candidates, policy_placement, available_height);

            if stages.fill_additional_rows {
                row_heights = RowHeightMode::Intrinsic;
                let intrinsic = self.profile_exact_range(page, &selected.geometries, available_height, &mut intrinsic_heights, window, cx);
                selected = self.fill_additional_rows(intrinsic, &candidates, available_height, &mut intrinsic_heights, window, cx);
            }
        }

        if stages.distribute_gaps {
            geometry::expand_horizontal_gaps(&self.data, &mut selected.geometries, available_width);
        }
        let selection = GeometrySelection { key: selection_key, geometries: selected.geometries.clone(), page: selected.visible_page(), row_heights, row_gaps };
        let paginator = self.paginator.clone();
        window.defer(cx, move |_window, cx| {
            let _ = paginator.update(cx, |paginator, cx| paginator.set_geometry_selection(selection, cx));
        });
        let mut measured = selected.visible_page().map_or_else(|| MeasuredPage { rows: Vec::new(), geometries: selected.geometries.clone() }, |page| self.measure_range(page, &selected.geometries, row_heights, available_height, window, cx));
        apply_vertical_gaps(&self.data, &mut measured.rows, available_height, row_gaps, stages.distribute_gaps);
        measured
    }

    fn minimum_next_page_peek(&self, available_extent: Pixels) -> Pixels {
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
        Some(self.measure_row(assignment, available_height, window, cx))
    }

    fn geometry_selection_key(&self, bounds: Bounds<Pixels>) -> GeometrySelectionKey {
        GeometrySelectionKey {
            children_revision: self.children_revision,
            child_offset: self.child_offset,
            direction: self.direction,
            stages: self.stages,
            width: self.current_page_width(bounds.size.width),
            height: self.current_page_height(bounds.size.height),
            rem_size: self.rem_size,
        }
    }

    fn estimated_placement(&self, geometries: &[GroupGeometry], available_height: Pixels, minimum_gaps: bool) -> Placement {
        let rows = if minimum_gaps {
            geometry::estimated_page_rows_with_minimum_gaps(&self.data, geometries, self.direction, self.child_offset, self.maximum_rows, available_height, self.rem_size)
        } else {
            geometry::estimated_page_rows(&self.data, geometries, self.direction, self.child_offset, self.maximum_rows, available_height, self.rem_size)
        }
        .into_iter()
        .map(|assignment| {
            let height = self.data.groups[assignment.group_index].policy.sizing.height(assignment.child_width, self.rem_size);
            ProfileRow { assignment, height }
        })
        .collect();
        Placement { rows, geometries: geometries.to_vec() }.constrain_single_row_height(available_height)
    }

    fn fill_additional_rows(&self, mut current: Placement, candidates: &[Vec<GroupGeometry>], available_height: Pixels, intrinsic_heights: &mut HashMap<IntrinsicMeasurementKey, Pixels>, window: &mut Window, cx: &mut App) -> Placement {
        loop {
            let Some(page) = current.visible_page() else {
                break;
            };
            let Some(mut extension) = placement::extend_by_source_row(&self.data, &current.geometries, self.direction, page) else {
                break;
            };

            let mut probe_geometries = current.geometries.clone();
            if !current.group_is_visible(extension.group_index) {
                let Some(probe) = geometry::minimum_probe_geometry(&self.data, extension.group_index, &candidates[extension.group_index], self.rem_size) else {
                    break;
                };
                probe_geometries[extension.group_index] = probe;
                let Some(probed_extension) = placement::extend_by_source_row(&self.data, &probe_geometries, self.direction, page) else {
                    break;
                };
                extension = probed_extension;
            }

            // Only the group receiving the new row may reflow. Previously
            // placed groups remain fixed, while earlier rows from this same
            // group participate in rank repair with the new children.
            let active_candidates = candidates.iter().enumerate().map(|(group_index, candidates)| if group_index == extension.group_index { candidates.clone() } else { Vec::new() }).collect::<Vec<_>>();

            let policy_placement = self.profile_exact_policy_range(extension.page, &probe_geometries, available_height);
            let refined = self.refine_locked_policy_range(extension.page, &active_candidates, policy_placement, available_height);
            let intrinsic = self.profile_exact_range(extension.page, &refined.geometries, available_height, intrinsic_heights, window, cx);
            if !intrinsic.fits(&self.data, available_height, self.maximum_rows) {
                break;
            }
            current = intrinsic;
        }

        current
    }

    fn refine_locked_policy_range(&self, page: VisiblePage, candidates: &[Vec<GroupGeometry>], current: Placement, available_height: Pixels) -> Placement {
        let stages = self.stages.effective();
        let expanded = if stages.expand_widths {
            placement::expand_locked_range(&self.data, self.rem_size, available_height, self.maximum_rows, candidates, current, |geometries| self.profile_exact_policy_range(page, geometries, available_height))
        } else {
            current
        };

        if stages.repair_ranks { placement::repair_ranks(&self.data, candidates, expanded, |geometries| self.profile_exact_policy_range(page, geometries, available_height)) } else { expanded }
    }

    fn profile_exact_policy_range(&self, page: VisiblePage, geometries: &[GroupGeometry], available_height: Pixels) -> Placement {
        let rows = geometry::forward_rows_in_range(&self.data, geometries, page.first_child, page.last_child.saturating_add(1))
            .into_iter()
            .map(|assignment| {
                let height = self.data.groups[assignment.group_index].policy.sizing.height(assignment.child_width, self.rem_size);
                ProfileRow { assignment, height }
            })
            .collect();
        Placement { rows, geometries: geometries.to_vec() }.constrain_single_row_height(available_height)
    }

    fn profile_exact_range(&self, page: VisiblePage, geometries: &[GroupGeometry], available_height: Pixels, intrinsic_heights: &mut HashMap<IntrinsicMeasurementKey, Pixels>, window: &mut Window, cx: &mut App) -> Placement {
        let rows = geometry::forward_rows_in_range(&self.data, geometries, page.first_child, page.last_child.saturating_add(1))
            .into_iter()
            .map(|assignment| {
                let height = self.measure_intrinsic_row_height(&assignment, intrinsic_heights, window, cx);
                ProfileRow { assignment, height }
            })
            .collect();
        Placement { rows, geometries: geometries.to_vec() }.constrain_single_row_height(available_height)
    }

    fn measure_intrinsic_row_height(&self, assignment: &AssignedRow, intrinsic_heights: &mut HashMap<IntrinsicMeasurementKey, Pixels>, window: &mut Window, cx: &mut App) -> Pixels {
        let policy = self.data.groups[assignment.group_index].policy;
        let sizing_height = policy.sizing.height(assignment.child_width, self.rem_size);
        assignment
            .items
            .iter()
            .map(|item| {
                let key = IntrinsicMeasurementKey::new(item.child_index, assignment.child_width, self.rem_size, policy.sizing.intrinsic_height_depends_on_width());
                *intrinsic_heights.entry(key).or_insert_with(|| {
                    let child = &self.data.children[item.child_index];
                    let mut element = div().w(assignment.child_width).max_h(sizing_height).overflow_hidden().child((child.render)(window, cx)).into_any_element();
                    let measured = element.layout_as_root(size(AvailableSpace::Definite(assignment.child_width), AvailableSpace::MinContent), window, cx);
                    measured.height.min(sizing_height).max(px(1.0))
                })
            })
            .fold(px(1.0), |row_height, child_height| row_height.max(child_height))
    }

    fn measure_forward_from(&self, child_offset: usize, geometries: &[GroupGeometry], available_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let mut next_child = child_offset.min(self.data.children.len());
        let mut rows = Vec::new();
        let mut rejected = None;

        while self.maximum_rows.is_none_or(|maximum_rows| rows.len() < maximum_rows) {
            let Some(assignment) = geometry::next_forward_row(&self.data, geometries, &mut next_child) else {
                break;
            };
            let row = self.measure_row(assignment, available_height, window, cx);
            if !rows.is_empty() && !rows_fit_with_minimum_gaps(&self.data, &rows, &row, available_height, false) {
                rejected = Some(row);
                break;
            }
            rows.push(row);
        }

        // Keep the rejected element alive until the accepted page has been
        // decided. GPUI can otherwise reuse element state while measuring the
        // following candidate in the same frame.
        drop(rejected);
        MeasuredPage { rows, geometries: geometries.to_vec() }
    }

    fn measure_backward_from(&self, child_offset: usize, geometries: &[GroupGeometry], available_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let page_end = child_offset.saturating_add(1).min(self.data.children.len());
        let accepted = grow_backward_page(
            &self.data,
            geometries,
            page_end,
            self.maximum_rows,
            |assignments| self.measure_assignments(assignments, geometries, available_height, window, cx),
            |candidate| page_fits_with_minimum_gaps(&self.data, &candidate.rows, available_height),
        );
        accepted.unwrap_or_else(|| MeasuredPage { rows: Vec::new(), geometries: geometries.to_vec() })
    }

    fn measure_assignments(&self, assignments: Vec<AssignedRow>, geometries: &[GroupGeometry], maximum_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let rows = assignments.into_iter().map(|assignment| self.measure_row(assignment, maximum_height, window, cx)).collect();
        MeasuredPage { rows, geometries: geometries.to_vec() }
    }

    fn measure_range(&self, page: VisiblePage, geometries: &[GroupGeometry], row_heights: RowHeightMode, maximum_height: Pixels, window: &mut Window, cx: &mut App) -> MeasuredPage {
        let assignments = geometry::forward_rows_in_range(&self.data, geometries, page.first_child, page.last_child.saturating_add(1));
        let mut measured = self.measure_assignments(assignments, geometries, maximum_height, window, cx);
        if row_heights == RowHeightMode::Policy {
            for row in &mut measured.rows {
                row.height = self.data.groups[row.assignment.group_index].policy.sizing.height(row.assignment.child_width, self.rem_size).min(maximum_height.max(px(1.0)));
            }
        }
        measured
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
            let fallback_width = geometry::preferred_fallback_width(&data, rem_size);
            let width = known_dimensions.width.unwrap_or(match available_space.width {
                AvailableSpace::Definite(width) => width,
                AvailableSpace::MinContent | AvailableSpace::MaxContent => fallback_width,
            });
            let available_height = match available_space.height {
                AvailableSpace::Definite(height) => height,
                AvailableSpace::MinContent | AvailableSpace::MaxContent => px(f32::MAX / 4.0),
            };
            let geometries = geometry::preferred_geometries(&data, width, rem_size);
            let rows = geometry::estimated_page_rows(&data, &geometries, direction, child_offset, maximum_rows, available_height, rem_size);
            size(width, geometry::estimated_height(&data, &rows, rem_size).min(available_height))
        });
        (layout_id, ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _layout: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        let mut page = self.measure_page(bounds, window, cx);
        let visible_page = measured_visible_page(&page);

        if self.active {
            if let Some(visible_page) = visible_page {
                self.prepare_page(visible_page, window, cx);
                let prefetch_key = PrefetchKey { children_revision: self.children_revision, stages: self.stages, page: visible_page, width: bounds.size.width, height: bounds.size.height, rem_size: self.rem_size };
                if self.prefetched.as_ref() != Some(&prefetch_key) {
                    self.prepare_adjacent_pages(visible_page, &page.geometries, bounds.size.height, window, cx);
                    let paginator = self.paginator.clone();
                    window.defer(cx, move |_window, cx| {
                        let _ = paginator.update(cx, |paginator, cx| paginator.set_prefetched(prefetch_key, cx));
                    });
                }
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
        let mut row_top = bounds.top() + bounds.size.height * self.vertical_offset;
        let mut elements = Vec::new();
        let mut activations = Vec::new();
        for row in &mut page.rows {
            row_top += row.gap_before;
            for item in row.items.drain(..) {
                let origin = point(row_left + (row.assignment.child_width + row.assignment.column_gap) * item.column as f32, row_top);
                if let Some(activation) = self.activation_hitbox(item.child_index, Bounds { origin, size: size(row.assignment.child_width, row.height) }, bounds, window) {
                    activations.push(activation);
                }
                let mut element = item.element;
                element.prepaint_at(origin, window, cx);
                elements.push(element);
            }
            row_top += row.height;
        }

        if let Some(visible_page) = visible_page
            && let Some(mut preview) = self.measure_next_preview(visible_page, &page.geometries, bounds.size.height, window, cx)
        {
            match self.flow_axis {
                PaginatorAxis::Horizontal => {
                    let occupied_width = measured_page_width(&page.rows);
                    let natural_left = row_left + occupied_width + preview.assignment.column_gap;
                    let reserved_left = bounds.left() + self.current_page_width(bounds.size.width) + bounds.size.width * self.horizontal_offset;
                    let preview_left = natural_left.min(reserved_left);
                    let preview_top = bounds.top() + bounds.size.height * self.vertical_offset;
                    for item in preview.items.drain(..) {
                        let origin = point(preview_left + (preview.assignment.child_width + preview.assignment.column_gap) * item.column as f32, preview_top);
                        if let Some(activation) = self.activation_hitbox(item.child_index, Bounds { origin, size: size(preview.assignment.child_width, preview.height) }, bounds, window) {
                            activations.push(activation);
                        }
                        let mut element = item.element;
                        element.prepaint_at(origin, window, cx);
                        elements.push(element);
                    }
                }
                PaginatorAxis::Vertical => {
                    let gap_before = page.rows.last().map_or(Pixels::ZERO, |row| geometry::minimum_gap_between(&self.data, &row.assignment, &preview.assignment));
                    let natural_top = row_top + gap_before;
                    let reserved_top = bounds.top() + self.current_page_height(bounds.size.height) + bounds.size.height * self.vertical_offset;
                    let preview_top = natural_top.min(reserved_top);
                    for item in preview.items.drain(..) {
                        let origin = point(row_left + (preview.assignment.child_width + preview.assignment.column_gap) * item.column as f32, preview_top);
                        if let Some(activation) = self.activation_hitbox(item.child_index, Bounds { origin, size: size(preview.assignment.child_width, preview.height) }, bounds, window) {
                            activations.push(activation);
                        }
                        let mut element = item.element;
                        element.prepaint_at(origin, window, cx);
                        elements.push(element);
                    }
                }
            }
        }
        PaginatorPrepaint { elements, activations }
    }

    fn paint(&mut self, _id: Option<&GlobalElementId>, _inspector_id: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _layout: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for element in &mut prepaint.elements {
                element.paint(window, cx);
            }
        });

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
}

struct MeasuredItem {
    child_index: usize,
    column: usize,
    element: AnyElement,
}

struct MeasuredRow {
    assignment: AssignedRow,
    items: Vec<MeasuredItem>,
    height: Pixels,
    gap_before: Pixels,
}

struct MeasuredPage {
    rows: Vec<MeasuredRow>,
    geometries: Vec<GroupGeometry>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct IntrinsicMeasurementKey {
    child_index: usize,
    width: Option<u32>,
    rem_size: u32,
}

impl IntrinsicMeasurementKey {
    fn new(child_index: usize, width: Pixels, rem_size: Pixels, width_dependent: bool) -> Self {
        Self { child_index, width: width_dependent.then(|| f32::from(width).to_bits()), rem_size: f32::from(rem_size).to_bits() }
    }
}

fn measured_visible_page(page: &MeasuredPage) -> Option<VisiblePage> {
    let first_child = page.rows.first()?.assignment.items.first()?.child_index;
    let last_child = page.rows.last()?.assignment.items.last()?.child_index;
    Some(VisiblePage { first_child, last_child })
}

fn measured_page_width(rows: &[MeasuredRow]) -> Pixels {
    rows.iter().flat_map(|row| row.assignment.items.iter().map(move |item| row.assignment.child_width + (row.assignment.child_width + row.assignment.column_gap) * item.column as f32)).fold(Pixels::ZERO, Pixels::max)
}

fn rows_fit_with_minimum_gaps(data: &PaginatorData, rows: &[MeasuredRow], candidate: &MeasuredRow, available_height: Pixels, prepend: bool) -> bool {
    let row_height = rows.iter().map(|row| f32::from(row.height)).sum::<f32>() + f32::from(candidate.height);
    let internal_gaps = rows.windows(2).map(|pair| f32::from(geometry::minimum_gap_between(data, &pair[0].assignment, &pair[1].assignment))).sum::<f32>();
    let edge_gap = if prepend { geometry::minimum_gap_between(data, &candidate.assignment, &rows[0].assignment) } else { geometry::minimum_gap_between(data, &rows[rows.len() - 1].assignment, &candidate.assignment) };
    row_height + internal_gaps + f32::from(edge_gap) <= f32::from(available_height).max(1.0)
}

fn page_fits_with_minimum_gaps(data: &PaginatorData, rows: &[MeasuredRow], available_height: Pixels) -> bool {
    let heights = rows.iter().map(|row| f32::from(row.height)).sum::<f32>();
    let gaps = rows.windows(2).map(|pair| f32::from(geometry::minimum_gap_between(data, &pair[0].assignment, &pair[1].assignment))).sum::<f32>();
    heights + gaps <= f32::from(available_height).max(1.0)
}

fn grow_backward_page<T>(data: &PaginatorData, geometries: &[GroupGeometry], page_end: usize, maximum_rows: Option<usize>, mut build: impl FnMut(Vec<AssignedRow>) -> T, mut fits: impl FnMut(&T) -> bool) -> Option<T> {
    let mut previous_child = page_end.min(data.children.len());
    let mut accepted = None;
    while geometry::previous_row(data, geometries, &mut previous_child).is_some() {
        let assignments = geometry::forward_rows_in_range(data, geometries, previous_child, page_end);
        if maximum_rows.is_some_and(|maximum_rows| assignments.len() > maximum_rows) {
            break;
        }
        let candidate = build(assignments);
        if accepted.is_some() && !fits(&candidate) {
            break;
        }
        accepted = Some(candidate);
    }
    accepted
}

fn apply_vertical_gaps(data: &PaginatorData, rows: &mut [MeasuredRow], available_height: Pixels, mode: RowGapMode, distribute: bool) {
    match (mode, distribute) {
        (RowGapMode::Preferred, _) => set_vertical_gaps(data, rows, geometry::preferred_gap_between),
        (RowGapMode::Minimum, false) => set_vertical_gaps(data, rows, geometry::minimum_gap_between),
        (RowGapMode::Minimum, true) => resolve_vertical_gaps(data, rows, available_height),
    }
}

fn set_vertical_gaps(data: &PaginatorData, rows: &mut [MeasuredRow], gap_between: fn(&PaginatorData, &AssignedRow, &AssignedRow) -> Pixels) {
    for index in 1..rows.len() {
        rows[index].gap_before = gap_between(data, &rows[index - 1].assignment, &rows[index].assignment);
    }
}

fn resolve_vertical_gaps(data: &PaginatorData, rows: &mut [MeasuredRow], available_height: Pixels) {
    if rows.len() < 2 {
        return;
    }

    let heights = rows.iter().map(|row| f32::from(row.height)).sum::<f32>();
    let gap_ranges = rows
        .windows(2)
        .map(|pair| {
            let minimum = f32::from(geometry::minimum_gap_between(data, &pair[0].assignment, &pair[1].assignment));
            let maximum = f32::from(geometry::maximum_gap_between(data, &pair[0].assignment, &pair[1].assignment));
            (minimum, maximum)
        })
        .collect::<Vec<_>>();
    let minimum_total = gap_ranges.iter().map(|(minimum, _)| minimum).sum::<f32>();
    let capacity = gap_ranges.iter().map(|(minimum, maximum)| maximum - minimum).sum::<f32>();
    let extra = (f32::from(available_height) - heights - minimum_total).max(0.0).min(capacity);
    let expansion = if capacity > 0.0 { extra / capacity } else { 0.0 };

    for (row, (minimum, maximum)) in rows.iter_mut().skip(1).zip(gap_ranges) {
        row.gap_before = px(minimum + (maximum - minimum) * expansion);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PaginatorChild, PaginatorGapPolicy, PaginatorGroup, PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy};
    use super::*;
    use gpui::{div, rems};

    fn fixture() -> (PaginatorData, Vec<GroupGeometry>) {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(200.0)), PaginatorGapPolicy::fixed(Pixels::ZERO), PaginatorGapPolicy::fixed(Pixels::ZERO));
        let children = (0..7).map(|_| PaginatorChild::new(|_, _| div().into_any_element()));
        let data = PaginatorData::new([PaginatorGroup::new(policy, children)]);
        let geometries = vec![GroupGeometry { group_index: 0, columns: 2, child_width: px(80.0), column_gap: Pixels::ZERO }];
        (data, geometries)
    }

    fn numeric_page(assignments: Vec<AssignedRow>, child_heights: &[f32]) -> (Vec<AssignedRow>, f32) {
        let height = assignments.iter().map(|row| row.items.iter().map(|item| child_heights[item.child_index]).fold(0.0, f32::max)).sum();
        (assignments, height)
    }

    fn indices(rows: &[AssignedRow]) -> Vec<usize> {
        rows.iter().flat_map(|row| row.items.iter().map(|item| item.child_index)).collect()
    }

    fn measured_rows(assignments: Vec<AssignedRow>, height: Pixels) -> Vec<MeasuredRow> {
        assignments.into_iter().map(|assignment| MeasuredRow { assignment, items: Vec::new(), height, gap_before: Pixels::ZERO }).collect()
    }

    #[test]
    fn backward_growth_rechecks_heights_after_normalization() {
        let (data, geometries) = fixture();
        let heights = [1.0, 100.0, 1.0, 100.0, 1.0, 100.0, 100.0];

        let accepted = grow_backward_page(&data, &geometries, 7, None, |assignments| numeric_page(assignments, &heights), |(_, height)| *height <= 310.0).expect("preceding page");

        assert_eq!(indices(&accepted.0), (1..7).collect::<Vec<_>>());
        assert_eq!(accepted.1, 300.0);
    }

    #[test]
    fn backward_normalization_can_admit_an_earlier_partial_source_row() {
        let (data, geometries) = fixture();
        let heights = [100.0, 100.0, 1.0, 100.0, 1.0, 100.0, 1.0];

        let accepted = grow_backward_page(&data, &geometries, 7, None, |assignments| numeric_page(assignments, &heights), |(_, height)| *height <= 310.0).expect("preceding page");

        assert_eq!(indices(&accepted.0), (0..7).collect::<Vec<_>>());
        assert_eq!(accepted.1, 301.0);
    }

    #[test]
    fn intrinsic_measurement_keys_only_include_width_for_aspect_sizing() {
        let fixed = PaginatorSizing::rems(rems(4.0));
        let aspect = PaginatorSizing::aspect_ratio(0.7).with_pixels(px(20.0));

        let fixed_narrow = IntrinsicMeasurementKey::new(3, px(100.0), px(16.0), fixed.intrinsic_height_depends_on_width());
        let fixed_wide = IntrinsicMeasurementKey::new(3, px(180.0), px(16.0), fixed.intrinsic_height_depends_on_width());
        let aspect_narrow = IntrinsicMeasurementKey::new(3, px(100.0), px(16.0), aspect.intrinsic_height_depends_on_width());
        let aspect_wide = IntrinsicMeasurementKey::new(3, px(180.0), px(16.0), aspect.intrinsic_height_depends_on_width());

        assert_eq!(fixed_narrow, fixed_wide);
        assert_ne!(aspect_narrow, aspect_wide);
    }

    #[test]
    fn distributed_vertical_gaps_fill_only_their_declared_capacity() {
        let row_gap = PaginatorGapPolicy::new(px(2.0), px(4.0)).with_maximum(px(10.0));
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(20.0)), PaginatorGapPolicy::fixed(Pixels::ZERO), row_gap);
        let data = PaginatorData::new([PaginatorGroup::new(policy, (0..4).map(|_| PaginatorChild::new(|_, _| div().into_any_element())))]);
        let geometries = [GroupGeometry { group_index: 0, columns: 1, child_width: px(80.0), column_gap: Pixels::ZERO }];
        let assignments = geometry::forward_rows_in_range(&data, &geometries, 0, 4);
        let mut rows = measured_rows(assignments, px(20.0));

        resolve_vertical_gaps(&data, &mut rows, px(200.0));

        assert_eq!(rows.iter().skip(1).map(|row| row.gap_before).collect::<Vec<_>>(), vec![px(10.0); 3]);
        let occupied = rows.iter().map(|row| f32::from(row.height + row.gap_before)).sum::<f32>();
        assert_eq!(occupied, 110.0, "space beyond maximum gaps must remain free");
    }

    #[test]
    fn exact_minimum_gap_height_boundary_is_accepted_but_subpixel_overflow_is_not() {
        let row_gap = PaginatorGapPolicy::new(px(2.5), px(8.0));
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(20.25)), PaginatorGapPolicy::fixed(Pixels::ZERO), row_gap);
        let data = PaginatorData::new([PaginatorGroup::new(policy, (0..3).map(|_| PaginatorChild::new(|_, _| div().into_any_element())))]);
        let geometries = [GroupGeometry { group_index: 0, columns: 1, child_width: px(80.0), column_gap: Pixels::ZERO }];
        let assignments = geometry::forward_rows_in_range(&data, &geometries, 0, 3);
        let rows = measured_rows(assignments, px(20.25));

        assert!(page_fits_with_minimum_gaps(&data, &rows, px(65.75)));
        assert!(!page_fits_with_minimum_gaps(&data, &rows, px(65.74)));
    }
}
