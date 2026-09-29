//! Group-aware, intrinsically measured paginator.

mod element;
mod geometry;
mod placement;
mod policy;

use std::rc::Rc;
use std::time::Duration;

#[cfg(feature = "mobile")]
use gpui::TouchPhase;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, ElementId, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString, Task, Window, div, ease_in_out};
use web_time::Instant;

use element::PaginatorElement;
pub(crate) use policy::{PaginatorGapPolicy, PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaginatorAxis {
    Horizontal,
    Vertical,
}

type RenderChild = dyn Fn(&mut Window, &mut App) -> AnyElement;
type PrepareChild = dyn Fn(&mut Window, &mut App);
type ActivateChild = dyn Fn(&mut Window, &mut App);

const PAGE_TRANSITION_DURATION: Duration = Duration::from_millis(320);
#[cfg(not(feature = "mobile"))]
const WHEEL_SCROLL_END_DELAY: Duration = Duration::from_millis(150);
#[cfg(feature = "mobile")]
const SWIPE_AXIS_LOCK_DISTANCE: f32 = 12.0;
#[cfg(feature = "mobile")]
const SWIPE_AXIS_DOMINANCE: f32 = 1.25;
#[cfg(feature = "mobile")]
const SWIPE_VIEWPORT_FRACTION: f32 = 0.12;
#[cfg(feature = "mobile")]
const SWIPE_MINIMUM_DISTANCE: f32 = 48.0;
#[cfg(feature = "mobile")]
const SWIPE_MAXIMUM_DISTANCE: f32 = 120.0;

#[cfg(feature = "mobile")]
#[derive(Clone, Copy, Default)]
enum SwipeAxis {
    #[default]
    Pending,
    Horizontal,
    Vertical,
}

#[cfg(feature = "mobile")]
impl SwipeAxis {
    fn paginator_axis(self) -> Option<PaginatorAxis> {
        match self {
            Self::Pending => None,
            Self::Horizontal => Some(PaginatorAxis::Horizontal),
            Self::Vertical => Some(PaginatorAxis::Vertical),
        }
    }
}

#[cfg(feature = "mobile")]
#[derive(Default)]
struct PaginatorSwipe {
    x: f32,
    y: f32,
    axis: SwipeAxis,
}

#[cfg(feature = "mobile")]
impl PaginatorSwipe {
    fn add(&mut self, x: f32, y: f32) {
        self.x += x;
        self.y += y;
        if !matches!(self.axis, SwipeAxis::Pending) {
            return;
        }

        let x = self.x.abs();
        let y = self.y.abs();
        if x.max(y) < SWIPE_AXIS_LOCK_DISTANCE {
            return;
        }
        if x >= y * SWIPE_AXIS_DOMINANCE {
            self.axis = SwipeAxis::Horizontal;
        } else if y >= x * SWIPE_AXIS_DOMINANCE {
            self.axis = SwipeAxis::Vertical;
        }
    }
}

#[derive(Clone)]
pub(crate) struct PaginatorChild {
    pub(super) activate: Option<Rc<ActivateChild>>,
    pub(super) prepare: Option<Rc<PrepareChild>>,
    pub(super) render: Rc<RenderChild>,
}

impl PaginatorChild {
    pub(crate) fn new(render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self { activate: None, prepare: None, render: Rc::new(render) }
    }

    pub(crate) fn with_activate(mut self, activate: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.activate = Some(Rc::new(activate));
        self
    }

    /// Starts work needed to make this child ready before it becomes visible.
    /// The callback may be invoked repeatedly and must therefore be idempotent.
    pub(crate) fn with_prepare(mut self, prepare: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.prepare = Some(Rc::new(prepare));
        self
    }
}

pub(crate) struct PaginatorGroup {
    policy: PaginatorGroupPolicy,
    children: Vec<PaginatorChild>,
}

impl PaginatorGroup {
    pub(crate) fn new(policy: PaginatorGroupPolicy, children: impl IntoIterator<Item = PaginatorChild>) -> Self {
        Self { policy, children: children.into_iter().collect() }
    }
}

#[derive(Clone, Copy)]
pub(super) struct GroupSpan {
    pub(super) policy: PaginatorGroupPolicy,
    pub(super) start: usize,
    pub(super) end: usize,
}

impl GroupSpan {
    pub(super) fn len(self) -> usize {
        self.end - self.start
    }
}

pub(super) struct PaginatorData {
    pub(super) groups: Vec<GroupSpan>,
    pub(super) children: Vec<PaginatorChild>,
}

impl PaginatorData {
    fn new(groups: impl IntoIterator<Item = PaginatorGroup>) -> Self {
        let mut spans = Vec::new();
        let mut children = Vec::new();
        for group in groups {
            if group.children.is_empty() {
                continue;
            }
            let start = children.len();
            children.extend(group.children);
            spans.push(GroupSpan { policy: group.policy, start, end: children.len() });
        }
        Self { groups: spans, children }
    }

    pub(super) fn group_index_for_child(&self, child_index: usize) -> Option<usize> {
        self.groups.iter().position(|group| child_index >= group.start && child_index < group.end)
    }
}

pub(crate) struct Paginator {
    id: SharedString,
    data: Rc<PaginatorData>,
    stages: PaginatorStages,
    maximum_rows: Option<usize>,
    child_offset: usize,
    direction: AssignmentDirection,
    visible_page: Option<VisiblePage>,
    children_revision: usize,
    prefetched: Option<PrefetchKey>,
    geometry_selection: Option<GeometrySelection>,
    animate_page_transitions: bool,
    flow_axis: PaginatorAxis,
    allow_vertical_navigation: bool,
    transition_sequence: usize,
    transition: Option<PageTransition>,
    wheel_delta: f32,
    wheel_debounce: Option<Task<()>>,
    #[cfg(feature = "mobile")]
    swipe: Option<PaginatorSwipe>,
}

#[derive(Clone)]
struct PageTransition {
    sequence: usize,
    direction: AssignmentDirection,
    axis: PaginatorAxis,
    outgoing_page: VisiblePage,
    outgoing_selection: Option<GeometrySelection>,
    started_at: Option<Instant>,
}

/// Optional refinements applied in order after preferred geometry assignment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PaginatorStages {
    pub(crate) expand_widths: bool,
    pub(crate) repair_ranks: bool,
    pub(crate) fill_additional_rows: bool,
    pub(crate) distribute_gaps: bool,
}

impl PaginatorStages {
    pub(crate) const fn baseline() -> Self {
        Self { expand_widths: false, repair_ranks: false, fill_additional_rows: false, distribute_gaps: false }
    }

    /// Later phases imply every phase before them, even when a diagnostic
    /// caller enables an individual flag directly.
    pub(super) const fn effective(mut self) -> Self {
        if self.distribute_gaps {
            self.fill_additional_rows = true;
        }
        if self.fill_additional_rows {
            self.repair_ranks = true;
        }
        if self.repair_ranks {
            self.expand_widths = true;
        }
        self
    }
}

impl Default for PaginatorStages {
    fn default() -> Self {
        Self { expand_widths: true, repair_ranks: true, fill_additional_rows: true, distribute_gaps: true }
    }
}

impl Paginator {
    pub(crate) fn new(id: impl Into<SharedString>, groups: impl IntoIterator<Item = PaginatorGroup>) -> Self {
        let data = Rc::new(PaginatorData::new(groups));
        Self {
            id: id.into(),
            data,
            stages: PaginatorStages::default(),
            maximum_rows: None,
            child_offset: 0,
            direction: AssignmentDirection::Forward,
            visible_page: None,
            children_revision: 0,
            prefetched: None,
            geometry_selection: None,
            animate_page_transitions: true,
            flow_axis: PaginatorAxis::Vertical,
            allow_vertical_navigation: true,
            transition_sequence: 0,
            transition: None,
            wheel_delta: 0.0,
            wheel_debounce: None,
            #[cfg(feature = "mobile")]
            swipe: None,
        }
    }

    pub(crate) fn max_rows(mut self, maximum_rows: usize) -> Self {
        self.maximum_rows = Some(maximum_rows.max(1));
        self
    }

    pub(crate) fn flow_axis(mut self, axis: PaginatorAxis) -> Self {
        self.flow_axis = axis;
        self
    }

    pub(crate) fn horizontal_only(mut self) -> Self {
        self.allow_vertical_navigation = false;
        self
    }

    pub(crate) fn stages(&self) -> PaginatorStages {
        self.stages
    }

    pub(crate) fn set_stages(&mut self, stages: PaginatorStages, cx: &mut Context<Self>) {
        if self.stages == stages {
            return;
        }
        self.stages = stages;
        self.visible_page = None;
        self.prefetched = None;
        self.geometry_selection = None;
        self.cancel_page_transition();
        self.cancel_wheel_gesture();
        cx.notify();
    }
    pub(crate) fn set_groups(&mut self, groups: impl IntoIterator<Item = PaginatorGroup>, cx: &mut Context<Self>) {
        self.data = Rc::new(PaginatorData::new(groups));
        self.child_offset = 0;
        self.direction = AssignmentDirection::Forward;
        self.visible_page = None;
        self.children_revision = self.children_revision.wrapping_add(1);
        self.prefetched = None;
        self.geometry_selection = None;
        self.cancel_page_transition();
        self.cancel_wheel_gesture();
        #[cfg(feature = "mobile")]
        {
            self.swipe = None;
        }
        cx.notify();
    }

    pub(crate) fn previous(&mut self, cx: &mut Context<Self>) -> bool {
        self.previous_with_axis(PaginatorAxis::Horizontal, cx)
    }

    pub(crate) fn previous_with_axis(&mut self, axis: PaginatorAxis, cx: &mut Context<Self>) -> bool {
        self.cancel_wheel_gesture();
        if !self.allows_axis(axis) {
            return false;
        }
        if self.transition.is_some() {
            return false;
        }
        let Some(page) = self.visible_page else {
            return false;
        };
        if page.first_child == 0 {
            return false;
        }
        self.capture_page_transition(AssignmentDirection::Backward, page);
        self.child_offset = page.first_child - 1;
        self.direction = AssignmentDirection::Backward;
        self.visible_page = None;
        self.prefetched = None;
        self.geometry_selection = None;
        cx.notify();
        true
    }

    pub(crate) fn next(&mut self, cx: &mut Context<Self>) -> bool {
        self.next_with_axis(PaginatorAxis::Horizontal, cx)
    }

    pub(crate) fn next_with_axis(&mut self, axis: PaginatorAxis, cx: &mut Context<Self>) -> bool {
        self.cancel_wheel_gesture();
        if !self.allows_axis(axis) {
            return false;
        }
        if self.transition.is_some() {
            return false;
        }
        let Some(page) = self.visible_page else {
            return false;
        };
        let next_offset = page.last_child.saturating_add(1);
        if next_offset >= self.data.children.len() {
            return false;
        }
        self.capture_page_transition(AssignmentDirection::Forward, page);
        self.child_offset = next_offset;
        self.direction = AssignmentDirection::Forward;
        self.visible_page = None;
        self.prefetched = None;
        self.geometry_selection = None;
        cx.notify();
        true
    }

    /// Positions a newly installed bounded result window at its final page.
    /// This is used when crossing a remote window boundary backwards.
    pub(crate) fn show_end(&mut self, cx: &mut Context<Self>) {
        let Some(last_child) = self.data.children.len().checked_sub(1) else { return };
        self.child_offset = last_child;
        self.direction = AssignmentDirection::Backward;
        self.visible_page = None;
        self.prefetched = None;
        self.geometry_selection = None;
        self.cancel_page_transition();
        self.cancel_wheel_gesture();
        cx.notify();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.data.children.is_empty()
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.transition.is_none() && self.visible_page.is_some()
    }

    /// Whether a measured page exists before the page currently on screen.
    /// Unlike `can_previous`, this remains stable while a transition runs.
    pub(crate) fn has_previous(&self) -> bool {
        self.visible_page.or_else(|| self.transition.as_ref().map(|transition| transition.outgoing_page)).is_some_and(|page| page.first_child > 0)
    }

    /// Whether a measured page exists after the page currently on screen.
    /// Unlike `can_next`, this remains stable while a transition runs.
    pub(crate) fn has_next(&self) -> bool {
        self.visible_page.or_else(|| self.transition.as_ref().map(|transition| transition.outgoing_page)).is_some_and(|page| page.last_child.saturating_add(1) < self.data.children.len())
    }

    pub(crate) fn can_previous(&self) -> bool {
        self.transition.is_none() && self.has_previous()
    }

    pub(crate) fn can_next(&self) -> bool {
        self.transition.is_none() && self.has_next()
    }

    fn set_visible_page(&mut self, children_revision: usize, visible_page: Option<VisiblePage>, cx: &mut Context<Self>) {
        if !self.update_visible_page(children_revision, visible_page) {
            return;
        }
        self.maybe_start_page_transition(cx);
        cx.notify();
    }

    fn allows_axis(&self, axis: PaginatorAxis) -> bool {
        matches!(axis, PaginatorAxis::Horizontal) || self.allow_vertical_navigation
    }

    fn capture_page_transition(&mut self, direction: AssignmentDirection, page: VisiblePage) {
        if !self.animate_page_transitions {
            return;
        }
        let outgoing_selection = self.geometry_selection.as_ref().filter(|selection| selection.page == Some(page)).cloned();
        self.transition_sequence = self.transition_sequence.wrapping_add(1);
        self.transition = Some(PageTransition { sequence: self.transition_sequence, direction, axis: self.flow_axis, outgoing_page: page, outgoing_selection, started_at: None });
    }

    fn maybe_start_page_transition(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.visible_page else {
            return;
        };
        if self.geometry_selection.as_ref().is_none_or(|selection| selection.page != Some(page)) {
            return;
        }
        let Some(transition) = self.transition.as_mut() else {
            return;
        };
        if transition.started_at.is_some() {
            return;
        }
        if cx.reduce_motion() {
            self.cancel_page_transition();
            return;
        }

        transition.started_at = Some(Instant::now());
    }

    fn cancel_page_transition(&mut self) {
        self.transition = None;
    }

    fn cancel_wheel_gesture(&mut self) {
        self.wheel_delta = 0.0;
        self.wheel_debounce.take();
    }

    #[cfg(not(feature = "mobile"))]
    fn handle_mouse_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.flow_axis != PaginatorAxis::Vertical || !self.allows_axis(PaginatorAxis::Vertical) {
            return;
        }
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
            ScrollDelta::Lines(delta) => delta.y,
        };
        if !delta.is_finite() || delta == 0.0 {
            return;
        }

        // Treat a wheel or trackpad burst as one gesture. Every new event
        // postpones navigation; once input goes quiet, the net direction
        // advances exactly one page.
        self.wheel_delta += delta;
        self.wheel_debounce.take();
        let timer = cx.background_executor().timer(WHEEL_SCROLL_END_DELAY);
        self.wheel_debounce = Some(cx.spawn(async move |paginator, cx| {
            timer.await;
            let _ = paginator.update(cx, |paginator, cx| {
                paginator.wheel_debounce.take();
                let delta = std::mem::take(&mut paginator.wheel_delta);
                match wheel_direction(delta) {
                    Some(AssignmentDirection::Forward) => {
                        paginator.next_with_axis(PaginatorAxis::Vertical, cx);
                    }
                    Some(AssignmentDirection::Backward) => {
                        paginator.previous_with_axis(PaginatorAxis::Vertical, cx);
                    }
                    None => {}
                }
            });
        }));
        cx.stop_propagation();
    }

    fn update_visible_page(&mut self, children_revision: usize, visible_page: Option<VisiblePage>) -> bool {
        if self.children_revision != children_revision {
            return false;
        }
        let consumed_backward = matches!(self.direction, AssignmentDirection::Backward) && visible_page.is_some();
        if consumed_backward {
            self.child_offset = visible_page.expect("checked above").first_child;
            self.direction = AssignmentDirection::Forward;
        }
        if self.visible_page == visible_page && !consumed_backward {
            return false;
        }
        self.visible_page = visible_page;
        true
    }

    fn set_prefetched(&mut self, key: PrefetchKey, cx: &mut Context<Self>) {
        if !self.update_prefetched(key) {
            return;
        }
        cx.notify();
    }

    fn update_prefetched(&mut self, key: PrefetchKey) -> bool {
        if self.children_revision != key.children_revision || self.stages != key.stages || self.prefetched.as_ref() == Some(&key) {
            return false;
        }
        self.prefetched = Some(key);
        true
    }

    #[cfg(feature = "mobile")]
    fn handle_swipe(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ScrollDelta::Pixels(delta) = event.delta else {
            if matches!(event.touch_phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                self.swipe = None;
            }
            return;
        };
        let x = f32::from(delta.x);
        let y = f32::from(delta.y);

        match event.touch_phase {
            TouchPhase::Started => {
                let mut swipe = PaginatorSwipe::default();
                swipe.add(x, y);
                let consume = swipe.axis.paginator_axis().is_some_and(|axis| self.allows_axis(axis));
                self.swipe = Some(swipe);
                if consume {
                    cx.stop_propagation();
                }
            }
            TouchPhase::Moved => {
                let Some(swipe) = self.swipe.as_mut() else {
                    return;
                };
                swipe.add(x, y);
                if swipe.axis.paginator_axis().is_some_and(|axis| self.allows_axis(axis)) {
                    cx.stop_propagation();
                }
            }
            TouchPhase::Ended => {
                let Some(swipe) = self.swipe.take() else {
                    return;
                };
                let Some(axis) = swipe.axis.paginator_axis().filter(|axis| self.allows_axis(*axis)) else {
                    return;
                };

                let (distance, viewport_extent) = match axis {
                    PaginatorAxis::Horizontal => (swipe.x, f32::from(window.viewport_size().width)),
                    PaginatorAxis::Vertical => (swipe.y, f32::from(window.viewport_size().height)),
                };
                let threshold = (viewport_extent * SWIPE_VIEWPORT_FRACTION).clamp(SWIPE_MINIMUM_DISTANCE, SWIPE_MAXIMUM_DISTANCE);
                if distance.abs() >= threshold {
                    if distance < 0.0 {
                        self.next_with_axis(axis, cx);
                    } else {
                        self.previous_with_axis(axis, cx);
                    }
                }
                cx.stop_propagation();
            }
            TouchPhase::Cancelled => {
                if self.swipe.take().and_then(|swipe| swipe.axis.paginator_axis()).is_some_and(|axis| self.allows_axis(axis)) {
                    cx.stop_propagation();
                }
            }
        }
    }

    fn set_geometry_selection(&mut self, selection: GeometrySelection, cx: &mut Context<Self>) {
        if !self.update_geometry_selection(selection) {
            return;
        }
        let transition_was_pending = self.transition.as_ref().is_some_and(|transition| transition.started_at.is_none());
        self.maybe_start_page_transition(cx);
        if transition_was_pending && self.transition.as_ref().is_none_or(|transition| transition.started_at.is_some()) {
            cx.notify();
        }
    }

    fn update_geometry_selection(&mut self, selection: GeometrySelection) -> bool {
        let key = &selection.key;
        if self.children_revision != key.children_revision || self.child_offset != key.child_offset || self.direction != key.direction || self.stages != key.stages {
            return false;
        }
        self.geometry_selection = Some(selection);
        true
    }
}

impl Render for Paginator {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = ElementId::from(self.id.clone());
        let child_count = self.data.children.len();
        let child_offset = self.child_offset.min(child_count.saturating_sub(1));
        let rendered_transition = self.transition.clone().and_then(|transition| {
            let progress = transition.started_at.map(|started_at| started_at.elapsed().as_secs_f32() / PAGE_TRANSITION_DURATION.as_secs_f32());
            if progress.is_some_and(|progress| progress >= 1.0) {
                self.transition = None;
                cx.notify();
                None
            } else {
                if progress.is_some() {
                    window.request_animation_frame();
                }
                Some((transition, progress.map(|progress| ease_in_out(progress.clamp(0.0, 1.0)))))
            }
        });
        let incoming_offset = rendered_transition.as_ref().and_then(|(transition, progress)| progress.map(|progress| page_transition_vector(transition.axis, page_transition_offsets(transition.direction, progress).1))).unwrap_or((0.0, 0.0));
        let incoming = PaginatorElement::new(
            ElementId::named_usize(format!("{id}-layout"), self.children_revision),
            cx.entity().downgrade(),
            self.visible_page,
            self.children_revision,
            self.data.clone(),
            self.stages,
            self.maximum_rows,
            child_offset,
            self.direction,
            self.prefetched.clone(),
            self.geometry_selection.clone(),
            window.rem_size(),
            self.flow_axis,
        )
        .with_page_offset(incoming_offset.0, incoming_offset.1);

        let mut content = div().relative().w_full().min_h_0().min_w_0().overflow_hidden().when(self.maximum_rows.is_none(), |content| content.h_full().flex_1());
        if let Some((transition, progress)) = rendered_transition {
            let outgoing_selection = transition.outgoing_selection;
            let (outgoing_revision, outgoing_stages, outgoing_child_offset, outgoing_direction, outgoing_rem_size) =
                outgoing_selection.as_ref().map_or((self.children_revision, self.stages, transition.outgoing_page.first_child, AssignmentDirection::Forward, window.rem_size()), |selection| {
                    (selection.key.children_revision, selection.key.stages, selection.key.child_offset, selection.key.direction, selection.key.rem_size)
                });
            let outgoing_offset = progress.map(|progress| page_transition_vector(transition.axis, page_transition_offsets(transition.direction, progress).0)).unwrap_or((0.0, 0.0));
            let outgoing = PaginatorElement::new(
                ElementId::named_usize(format!("{id}-outgoing-layout"), transition.sequence),
                cx.entity().downgrade(),
                Some(transition.outgoing_page),
                outgoing_revision,
                self.data.clone(),
                outgoing_stages,
                self.maximum_rows,
                outgoing_child_offset,
                outgoing_direction,
                None,
                outgoing_selection,
                outgoing_rem_size,
                self.flow_axis,
            )
            .passive()
            .with_page_offset(outgoing_offset.0, outgoing_offset.1);

            let incoming_page = div().relative().w_full().min_h_0().min_w_0().when(self.maximum_rows.is_none(), |page| page.h_full().flex_1()).child(incoming);
            let outgoing_page = div().absolute().inset_0().size_full().min_h_0().min_w_0().child(outgoing);
            if progress.is_some() {
                content = content.child(outgoing_page).child(incoming_page);
            } else {
                // Keep laying out the incoming page so it can start cover
                // preparation, but do not reveal it before the transition.
                content = content.child(outgoing_page).child(incoming_page.invisible());
            }
        } else {
            content = content.child(incoming);
        }

        let paginator = div().id(id).w_full().min_h_0().min_w_0().flex().flex_col().when(self.maximum_rows.is_none(), |paginator| paginator.h_full().flex_1());
        #[cfg(not(feature = "mobile"))]
        let paginator = paginator.on_scroll_wheel(cx.listener(Self::handle_mouse_wheel));
        #[cfg(feature = "mobile")]
        let paginator = paginator.on_scroll_wheel(cx.listener(Self::handle_swipe));
        paginator.child(content)
    }
}

#[cfg(not(feature = "mobile"))]
fn wheel_direction(delta: f32) -> Option<AssignmentDirection> {
    if delta < 0.0 {
        Some(AssignmentDirection::Forward)
    } else if delta > 0.0 {
        Some(AssignmentDirection::Backward)
    } else {
        None
    }
}

fn page_transition_offsets(direction: AssignmentDirection, progress: f32) -> (f32, f32) {
    match direction {
        AssignmentDirection::Forward => (-progress, 1.0 - progress),
        AssignmentDirection::Backward => (progress, progress - 1.0),
    }
}

fn page_transition_vector(axis: PaginatorAxis, offset: f32) -> (f32, f32) {
    match axis {
        PaginatorAxis::Horizontal => (offset, 0.0),
        PaginatorAxis::Vertical => (0.0, offset),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AssignmentDirection {
    Forward,
    Backward,
}

#[derive(Clone, Debug, PartialEq)]
struct GeometrySelectionKey {
    children_revision: usize,
    child_offset: usize,
    direction: AssignmentDirection,
    stages: PaginatorStages,
    width: Pixels,
    height: Pixels,
    rem_size: Pixels,
}

#[derive(Clone, Debug)]
struct GeometrySelection {
    key: GeometrySelectionKey,
    geometries: Vec<geometry::GroupGeometry>,
    page: Option<VisiblePage>,
    row_heights: RowHeightMode,
    row_gaps: RowGapMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowHeightMode {
    Policy,
    Intrinsic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowGapMode {
    Preferred,
    Minimum,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VisiblePage {
    pub(super) first_child: usize,
    pub(super) last_child: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PrefetchKey {
    pub(super) children_revision: usize,
    pub(super) stages: PaginatorStages,
    pub(super) page: VisiblePage,
    pub(super) width: Pixels,
    pub(super) height: Pixels,
    pub(super) rem_size: Pixels,
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, rems};

    fn policy() -> PaginatorGroupPolicy {
        PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(200.0)), PaginatorGapPolicy::fixed(Pixels::ZERO), PaginatorGapPolicy::fixed(Pixels::ZERO))
    }

    fn children_with_heights(heights: impl IntoIterator<Item = f32>) -> PaginatorGroup {
        PaginatorGroup::new(policy(), heights.into_iter().map(|height| PaginatorChild::new(move |_, _| div().w_full().h(px(height)).into_any_element())))
    }

    #[test]
    fn backward_page_is_committed_as_a_one_shot_forward_anchor() {
        let mut paginator = Paginator::new("state-test", [children_with_heights([10.0; 12])]);
        paginator.child_offset = 7;
        paginator.direction = AssignmentDirection::Backward;
        let backward_page = VisiblePage { first_child: 2, last_child: 7 };

        assert!(paginator.update_visible_page(0, Some(backward_page)));
        assert_eq!(paginator.child_offset, 2);
        assert!(matches!(paginator.direction, AssignmentDirection::Forward));

        let reflowed = VisiblePage { first_child: 2, last_child: 9 };
        assert!(paginator.update_visible_page(0, Some(reflowed)));
        assert_eq!(paginator.child_offset, 2, "later forward layouts changed the stable start");
    }

    #[test]
    fn stale_visible_and_prefetch_updates_cannot_cross_a_children_revision() {
        let mut paginator = Paginator::new("revision-test", [children_with_heights([10.0; 4])]);
        paginator.children_revision = 3;
        paginator.child_offset = 2;
        paginator.direction = AssignmentDirection::Backward;
        let stale_page = VisiblePage { first_child: 0, last_child: 1 };
        let stale_prefetch = PrefetchKey { children_revision: 2, stages: PaginatorStages::default(), page: stale_page, width: px(100.0), height: px(100.0), rem_size: px(16.0) };
        let stale_geometry = GeometrySelection {
            key: GeometrySelectionKey { children_revision: 2, child_offset: 2, direction: AssignmentDirection::Backward, stages: PaginatorStages::default(), width: px(100.0), height: px(100.0), rem_size: px(16.0) },
            geometries: vec![geometry::GroupGeometry { group_index: 0, columns: 2, child_width: px(48.0), column_gap: px(4.0) }],
            page: Some(stale_page),
            row_heights: RowHeightMode::Policy,
            row_gaps: RowGapMode::Preferred,
        };

        assert!(!paginator.update_visible_page(2, Some(stale_page)));
        assert!(!paginator.update_prefetched(stale_prefetch));
        assert!(!paginator.update_geometry_selection(stale_geometry));
        assert_eq!(paginator.child_offset, 2);
        assert!(matches!(paginator.direction, AssignmentDirection::Backward));
        assert_eq!(paginator.visible_page, None);
        assert_eq!(paginator.prefetched, None);
        assert!(paginator.geometry_selection.is_none());
    }

    #[test]
    fn navigation_availability_remains_stable_during_a_transition() {
        let mut paginator = Paginator::new("position-test", [children_with_heights([10.0; 5])]);
        let page = VisiblePage { first_child: 1, last_child: 3 };
        paginator.visible_page = Some(page);

        assert!(paginator.has_previous());
        assert!(paginator.has_next());

        paginator.visible_page = None;
        paginator.transition = Some(PageTransition { sequence: 1, direction: AssignmentDirection::Forward, axis: PaginatorAxis::Horizontal, outgoing_page: page, outgoing_selection: None, started_at: None });
        assert!(paginator.has_previous());
        assert!(paginator.has_next());
        assert!(!paginator.can_previous());
        assert!(!paginator.can_next());
    }

    #[test]
    fn transition_offset_uses_the_requested_axis() {
        assert_eq!(page_transition_vector(PaginatorAxis::Horizontal, -0.25), (-0.25, 0.0));
        assert_eq!(page_transition_vector(PaginatorAxis::Vertical, -0.25), (0.0, -0.25));
    }

    #[test]
    fn configured_flow_axis_drives_page_transitions() {
        let mut paginator = Paginator::new("flow-transition", [children_with_heights([10.0; 5])]).flow_axis(PaginatorAxis::Horizontal);
        let page = VisiblePage { first_child: 0, last_child: 2 };

        paginator.capture_page_transition(AssignmentDirection::Forward, page);

        assert_eq!(paginator.transition.as_ref().map(|transition| transition.axis), Some(PaginatorAxis::Horizontal));
    }

    #[test]
    fn horizontal_only_paginators_reject_vertical_navigation() {
        let paginator = Paginator::new("horizontal-only", [children_with_heights([10.0; 5])]).horizontal_only();
        assert!(paginator.allows_axis(PaginatorAxis::Horizontal));
        assert!(!paginator.allows_axis(PaginatorAxis::Vertical));
    }

    #[cfg(not(feature = "mobile"))]
    #[test]
    fn wheel_gesture_direction_matches_vertical_page_navigation() {
        assert_eq!(wheel_direction(-1.0), Some(AssignmentDirection::Forward));
        assert_eq!(wheel_direction(1.0), Some(AssignmentDirection::Backward));
        assert_eq!(wheel_direction(0.0), None);
    }
}
