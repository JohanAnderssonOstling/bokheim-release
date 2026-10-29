//! Group-aware paginator with declared child sizes.

mod drag_preview;
mod element;
mod geometry;
mod identity;
mod layout_cache;
mod policy;
mod slots;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, ElementId, Font, PAGE_SCROLL_DURATION, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, SharedString, TouchPhase, Window, div, ease_in_out, point, touch_page_settle_duration,
    touch_page_settle_easing,
};
use ui_components::{HierarchyItemWidth, SwipePreview, TouchpadNavigation};
use web_time::Instant;

pub(crate) use drag_preview::PaginatorDragPreview;
use element::{PaginatorElement, PaginatorPage};
use identity::ItemPosition;
pub(crate) use slots::Slots as PaginatorSlots;
use slots::Slots;
pub(crate) type PaginatorChildren = Slots<PaginatorChild>;
pub(crate) use identity::PaginatorPosition;
pub(crate) use policy::{
    PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy, book_card_policy, browse_folder_card_policy, browse_folder_item_policy, browse_section_card_policy, browse_section_item_policy, browse_shelf_card_policy, graph_cover_policy,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaginatorAxis {
    Horizontal,
    Vertical,
}

/// One step of the keyboard cursor over the paginator's children.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CursorMove {
    Left,
    Right,
    Up,
    Down,
}

impl CursorMove {
    pub(crate) fn from_key(key: &str) -> Option<Self> {
        match key {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            _ => None,
        }
    }
}

type RenderChild = dyn Fn(&PaginatorState, &mut Window, &mut App) -> AnyElement;
type PrepareChild = dyn Fn(&mut Window, &mut App);
type ActivateChild = dyn Fn(&mut Window, &mut App);
type ContextMenuHandler = dyn Fn(Option<SharedString>, Point<Pixels>, &mut Window, &mut App);

/// How far a press has to travel before it is a rubber band rather than a
/// click. Without it, every click on empty space would flicker a band.
const MARQUEE_THRESHOLD: f32 = 4.0;
const SWIPE_AXIS_LOCK_DISTANCE: f32 = 12.0;
const SWIPE_AXIS_DOMINANCE: f32 = 1.25;
const SWIPE_VIEWPORT_FRACTION: f32 = 0.12;
const SWIPE_MINIMUM_DISTANCE: f32 = 48.0;
const SWIPE_MAXIMUM_DISTANCE: f32 = 120.0;
const SCROLL_REPEAT_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Default)]
struct PaginatorSwipe {
    x: f32,
    y: f32,
    axis: Option<PaginatorAxis>,
}

impl PaginatorSwipe {
    fn add(&mut self, x: f32, y: f32) {
        self.x += x;
        self.y += y;
        if self.axis.is_some() {
            return;
        }

        let x = self.x.abs();
        let y = self.y.abs();
        if x.max(y) < SWIPE_AXIS_LOCK_DISTANCE {
            return;
        }
        if x >= y * SWIPE_AXIS_DOMINANCE {
            self.axis = Some(PaginatorAxis::Horizontal);
        } else if y >= x * SWIPE_AXIS_DOMINANCE {
            self.axis = Some(PaginatorAxis::Vertical);
        }
    }
}

/// What the paginator knows about its children this frame, handed to each child
/// as it is drawn. The paginator owns the cursor, the selection and the drag
/// state; a child only has to look.
#[derive(Clone, Default)]
pub(crate) struct PaginatorState {
    pub(crate) cursor: Option<usize>,
    pub(crate) selection: BTreeSet<usize>,
    /// Whether a rubber band is sweeping; children suppress hover during it.
    pub(crate) marquee: bool,
    pub(crate) preview: bool,
    bounds: Rc<Vec<(usize, Bounds<Pixels>)>>,
    data: Rc<PaginatorData>,
}

impl PaginatorState {
    pub(crate) fn is_selected(&self, id: &SharedString) -> bool {
        self.data.indices.get(id).is_some_and(|index| self.selection.contains(index))
    }
    pub(crate) fn is_focused(&self, id: &SharedString) -> bool {
        self.cursor.and_then(|index| self.data.children.get(index)).and_then(|child| child.id.as_ref()) == Some(id)
    }
    pub(crate) fn target_ids(&self, id: &SharedString) -> Vec<SharedString> {
        self.data.target_ids(id, &self.selection)
    }
}

/// One immutable measurement snapshot, owned and replaced by the paginator.
struct MeasuredWidths {
    children_revision: usize,
    rem_size: Pixels,
    font: Font,
    widths: Rc<Slots<Pixels>>,
}

fn measure_widths(data: &PaginatorData, window: &mut Window, cx: &mut App) -> Rc<Slots<Pixels>> {
    Rc::new(data.children.map(|child| child.intrinsic_width.as_ref().map_or(Pixels::ZERO, |content| content.measure(window, cx))))
}

#[derive(Clone)]
pub(crate) struct PaginatorChild {
    id: Option<SharedString>,
    pub(super) activate: Option<Rc<ActivateChild>>,
    pub(super) prepare: Option<Rc<PrepareChild>>,
    pub(super) render: Rc<RenderChild>,
    pub(super) owns_cell: bool,
    intrinsic_width: Option<Rc<HierarchyItemWidth>>,
    pub(super) selection_start: bool,
    pub(super) hit_bounds: Option<Rc<Cell<Bounds<Pixels>>>>,
}

impl PaginatorChild {
    pub(crate) fn new(render: impl Fn(&PaginatorState, &mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self { id: None, activate: None, prepare: None, render: Rc::new(render), owns_cell: false, intrinsic_width: None, selection_start: true, hit_bounds: None }
    }

    /// The rendered root fills its definite cell and clips its own contents.
    /// Keep the paginator's outer wrapper only when the group needs a shadow.
    pub(crate) fn with_own_cell(mut self) -> Self {
        self.owns_cell = true;
        self
    }

    /// Hit-test and select the painted entry, leaving its cell padding empty.
    pub(crate) fn with_hit_bounds(mut self, bounds: Rc<Cell<Bounds<Pixels>>>) -> Self {
        self.hit_bounds = Some(bounds);
        self
    }

    /// The item can be selected by a band started elsewhere, but cannot start one.
    pub(crate) fn prevent_selection_start(mut self) -> Self {
        self.selection_start = false;
        self
    }

    /// Stable identity used to retain selection and focus when results reorder.
    pub(crate) fn with_id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Measures content without constructing off-page interactive children.
    pub(crate) fn with_intrinsic_width(mut self, content: HierarchyItemWidth) -> Self {
        self.intrinsic_width = Some(Rc::new(content));
        self
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
    children: PaginatorChildren,
}

impl PaginatorGroup {
    pub(crate) fn new(policy: PaginatorGroupPolicy, children: impl IntoIterator<Item = PaginatorChild>) -> Self {
        Self { policy, children: children.into_iter().collect() }
    }

    /// Flow entry rows in columns alongside subsequent cover groups.
    pub(crate) fn set_column_fraction(&mut self, fraction: f32) {
        self.policy.column_fraction = Some(fraction.clamp(0.01, 1.0));
    }

    pub(crate) fn from_slots(policy: PaginatorGroupPolicy, children: PaginatorChildren) -> Self {
        Self { policy, children }
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

#[derive(Default)]
pub(super) struct PaginatorData {
    indices: HashMap<SharedString, usize>,
    pub(super) groups: Vec<GroupSpan>,
    pub(super) children: PaginatorChildren,
}

impl PaginatorData {
    fn target_ids(&self, id: &SharedString, selection: &BTreeSet<usize>) -> Vec<SharedString> {
        if self.indices.get(id).is_some_and(|index| selection.contains(index)) { selection.iter().filter_map(|&index| self.children.get(index)?.id.clone()).collect() } else { vec![id.clone()] }
    }

    fn position(&self, index: usize) -> Option<ItemPosition> {
        Some(ItemPosition { id: self.children.get(index)?.id.clone(), index })
    }
    fn resolve(&self, position: &ItemPosition) -> Option<usize> {
        match &position.id {
            Some(id) => self.indices.get(id).copied(),
            None => (!self.children.is_empty()).then_some(position.index.min(self.children.len().saturating_sub(1))),
        }
    }

    fn new(groups: impl IntoIterator<Item = PaginatorGroup>) -> Self {
        let mut spans = Vec::new();
        let mut children = Slots::default();
        for group in groups {
            if group.children.is_empty() {
                continue;
            }
            let start = children.len();
            children.append(group.children);
            spans.push(GroupSpan { policy: group.policy, start, end: children.len() });
        }
        let indices = children.stored().filter_map(|(index, child)| child.id.clone().map(|id| (id, index))).collect();
        Self { indices, groups: spans, children }
    }

    pub(super) fn group_index_for_child(&self, child_index: usize) -> Option<usize> {
        self.groups.iter().position(|group| child_index >= group.start && child_index < group.end)
    }
}

/// A rubber band in flight. Both points are in window coordinates, which is
/// what the element reports its child bounds in.
#[derive(Clone, Copy)]
struct Marquee {
    anchor: Point<Pixels>,
    current: Point<Pixels>,
    /// A press only becomes a band once it has travelled far enough that it
    /// cannot be an ordinary click.
    active: bool,
}

impl Marquee {
    /// Corners are ordered here rather than by the caller: a band drawn up or
    /// to the left would otherwise have a negative size and intersect nothing.
    fn bounds(&self) -> Bounds<Pixels> {
        let top_left = point(self.anchor.x.min(self.current.x), self.anchor.y.min(self.current.y));
        let bottom_right = point(self.anchor.x.max(self.current.x), self.anchor.y.max(self.current.y));
        Bounds::from_corners(top_left, bottom_right)
    }
}

pub(crate) struct Paginator {
    id: SharedString,
    measurements: Option<MeasuredWidths>,
    layout_cache: Rc<RefCell<layout_cache::LayoutCache>>,
    warmed_page: Option<WarmedPage>,
    warm_request: Option<PageWarmup>,
    data: Rc<PaginatorData>,
    child_offset: usize,
    /// Index of the child holding the keyboard cursor, in flat child order.
    /// An index rather than a row and column, so it survives the window being
    /// resized under it — which changes the column count but not the order.
    cursor: Option<usize>,
    /// Height of the rows the visible page actually draws. Pages routinely come
    /// up short of the container — whole rows only — and the transition has to
    /// pan by this, not by the container, or the two pages separate.
    content_height: Option<Pixels>,
    direction: AssignmentDirection,
    visible_page: Option<VisiblePage>,
    children_revision: usize,
    prefetched: Option<PrefetchKey>,
    animate_page_transitions: bool,
    flow_axis: PaginatorAxis,
    allow_vertical_navigation: bool,
    show_next_page_preview: bool,
    reserve_preview_space: bool,
    transition_sequence: usize,
    transition: Option<PageTransition>,
    swipe_preview: SwipePreview,
    /// Opt-in: pages that have nothing to do with a selection should not grow
    /// mouse handlers for one.
    selectable: bool,
    /// Start a selection band on unselected children as well as empty space.
    select_on_drag: bool,
    /// Answers a secondary click on the selection, or on the space between
    /// children. The paginator knows where its children are; what they mean,
    /// and what a menu should offer, is the page's.
    on_context_menu: Option<Rc<ContextMenuHandler>>,
    marquee: Option<Marquee>,
    /// Children the band has picked up, in flat child order. Indices are valid
    /// only for the current children; every path that replaces them clears it.
    selection: BTreeSet<usize>,
    /// The starting cursor is included once per keyboard selection, so a
    /// subsequently deselected cursor is not re-added on the next move.
    keyboard_selection_active: bool,
    /// Where the last painted frame put each visible child, in window
    /// coordinates. The element is the only place this is known, so it reports
    /// it back for hit-testing between frames and for anchoring a menu on the
    /// keyboard cursor.
    child_bounds: Rc<Vec<(usize, Bounds<Pixels>)>>,
    swipe: Option<PaginatorSwipe>,
    touchpad: TouchpadNavigation,
    last_wheel_turn: Option<Instant>,
    /// Times a page turn until its frame is drawn, when browse profiling is on.
    profile_turn: Option<crate::library::pages::browse::profile::Stage>,
}

#[derive(Clone)]
struct PageTransition {
    sequence: usize,
    direction: AssignmentDirection,
    axis: PaginatorAxis,
    outgoing_page: VisiblePage,
    outgoing_content_height: Option<Pixels>,
    started_at: Option<Instant>,
    drag_offset: Point<Pixels>,
    touch_duration: Option<Duration>,
}

struct PageWarmup {
    source: PrefetchKey,
    direction: AssignmentDirection,
    ready: Rc<Cell<bool>>,
}

struct WarmedPage {
    source: PrefetchKey,
    widths: Rc<Slots<Pixels>>,
    direction: AssignmentDirection,
    page: VisiblePage,
    content_height: Pixels,
}

impl Paginator {
    pub(crate) fn new(id: impl Into<SharedString>, groups: impl IntoIterator<Item = PaginatorGroup>) -> Self {
        let data = Rc::new(PaginatorData::new(groups));
        Self {
            id: id.into(),
            measurements: None,
            layout_cache: Rc::default(),
            warmed_page: None,
            warm_request: None,
            data,
            child_offset: 0,
            cursor: None,
            content_height: None,
            direction: AssignmentDirection::Forward,
            visible_page: None,
            children_revision: 0,
            prefetched: None,
            animate_page_transitions: !cfg!(feature = "kobo"),
            flow_axis: PaginatorAxis::Vertical,
            allow_vertical_navigation: true,
            show_next_page_preview: true,
            reserve_preview_space: true,
            transition_sequence: 0,
            transition: None,
            swipe_preview: SwipePreview::default(),
            selectable: false,
            select_on_drag: false,
            on_context_menu: None,
            marquee: None,
            selection: BTreeSet::new(),
            keyboard_selection_active: false,
            child_bounds: Rc::new(Vec::new()),
            swipe: None,
            touchpad: TouchpadNavigation::repeating(SCROLL_REPEAT_INTERVAL),
            last_wheel_turn: None,
            profile_turn: None,
        }
    }

    /// Called at layout time, where inherited font and rem size are available.
    /// Elements and keyboard geometry only receive immutable width snapshots.
    pub(super) fn measure_widths(&mut self, children_revision: usize, data: &PaginatorData, window: &mut Window, cx: &mut App) -> Rc<Slots<Pixels>> {
        let rem_size = window.rem_size();
        let font = window.text_style().font();
        if let Some(measured) = &self.measurements
            && measured.children_revision == children_revision
            && measured.rem_size == rem_size
            && measured.font == font
        {
            return measured.widths.clone();
        }
        let widths = measure_widths(data, window, cx);
        if children_revision == self.children_revision {
            self.measurements = Some(MeasuredWidths { children_revision, rem_size, font, widths: widths.clone() });
        }
        widths
    }

    /// Enables rubber-band and keyboard selection over the children.
    pub(crate) fn selectable(mut self) -> Self {
        self.selectable = true;
        self
    }

    pub(crate) fn select_on_drag(mut self) -> Self {
        self.selectable = true;
        self.select_on_drag = true;
        self
    }

    /// Handles a secondary click on the selection, with the child clicked, or
    /// on the space between children, with `None`.
    pub(crate) fn on_context_menu(mut self, handler: impl Fn(Option<SharedString>, Point<Pixels>, &mut Window, &mut App) + 'static) -> Self {
        self.on_context_menu = Some(Rc::new(handler));
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

    pub(crate) fn next_page_preview(mut self, show: bool) -> Self {
        self.show_next_page_preview = show;
        self
    }

    /// Draw the next item only in leftover space, preserving the page's width.
    pub(crate) fn next_page_preview_in_remaining_space(mut self) -> Self {
        self.show_next_page_preview = true;
        self.reserve_preview_space = false;
        self
    }

    pub(crate) fn target_ids(&self, id: &SharedString) -> Vec<SharedString> {
        self.data.target_ids(id, &self.selection)
    }

    pub(crate) fn child_id(&self, index: usize) -> Option<&SharedString> {
        self.data.children.get(index)?.id.as_ref()
    }

    pub(crate) fn position(&self) -> PaginatorPosition {
        PaginatorPosition {
            cursor: self.cursor.and_then(|index| self.data.position(index)),
            anchor: self.data.position(self.child_anchor()),
            selection: self.selection.iter().filter_map(|&index| self.child_id(index).cloned()).collect(),
            keyboard_selection_active: self.keyboard_selection_active,
        }
    }

    pub(crate) fn restore_position(&mut self, position: &PaginatorPosition, cx: &mut Context<Self>) {
        self.cursor = position.cursor.as_ref().and_then(|cursor| self.data.resolve(cursor));
        self.child_offset = position.anchor.as_ref().and_then(|anchor| self.data.resolve(anchor)).unwrap_or_else(|| position.anchor.as_ref().map_or(0, |anchor| anchor.index.min(self.data.children.len().saturating_sub(1))));
        self.selection = position.selection.iter().filter_map(|id| self.data.indices.get(id).copied()).collect();
        self.keyboard_selection_active = position.keyboard_selection_active && self.cursor.is_some();
        cx.notify();
    }

    /// Installs a listing update, then applies any saved navigation position.
    pub(crate) fn install_groups(&mut self, groups: impl IntoIterator<Item = PaginatorGroup>, refresh: bool, position: Option<&PaginatorPosition>, cx: &mut Context<Self>) {
        if refresh {
            self.refresh_groups(groups, cx);
        } else {
            self.set_groups(groups, cx);
        }
        if let Some(position) = position {
            self.restore_position(position, cx);
        }
    }

    /// Replaces the children while staying where the reader was.
    ///
    /// A background refresh — a finished download, a scanner batch — is not a
    /// navigation, so it must not send the reader back to the first page.
    /// Identified children retain their cursor and selection across reordering.
    /// Other children retain the positional offset and cursor.
    pub(crate) fn refresh_groups(&mut self, groups: impl IntoIterator<Item = PaginatorGroup>, cx: &mut Context<Self>) {
        let mut position = self.position();
        // At the beginning, show newly prepended folders or books instead of
        // following the old first item past them. Later pages keep their anchor.
        if self.child_anchor() == 0 && self.direction == AssignmentDirection::Forward {
            position.anchor = None;
        }
        let direction = self.direction;
        // A page turn already in flight survives too, for the same reason. The
        // keystroke that walks the cursor onto the next page is followed by a
        // refresh — rebuilding the rows is what carries the move into their
        // headings — and cancelling here would leave that turn the only one
        // that never animates. The outgoing page's height rides along with it,
        // so a backward turn still pans by a measured page rather than a guess.
        let transition = self.transition.take();
        let content_height = self.content_height;
        self.set_groups(groups, cx);
        self.restore_position(&position, cx);
        self.direction = direction;
        self.transition = transition;
        self.content_height = content_height;
    }

    pub(crate) fn set_groups(&mut self, groups: impl IntoIterator<Item = PaginatorGroup>, cx: &mut Context<Self>) {
        self.data = Rc::new(PaginatorData::new(groups));
        self.measurements = None;
        self.child_offset = 0;
        // The children are different books now, so a retained index would point
        // the cursor at something the reader never put it on.
        self.cursor = None;
        self.selection.clear();
        self.keyboard_selection_active = false;
        self.marquee = None;
        self.child_bounds = Rc::new(Vec::new());
        self.content_height = None;
        self.direction = AssignmentDirection::Forward;
        self.visible_page = None;
        self.children_revision = self.children_revision.wrapping_add(1);
        self.prefetched = None;
        self.cancel_page_transition();
        self.swipe = None;
        cx.notify();
    }

    pub(crate) fn previous_with_axis(&mut self, axis: PaginatorAxis, cx: &mut Context<Self>) -> bool {
        self.turn_page(AssignmentDirection::Backward, axis, cx)
    }

    pub(crate) fn next_with_axis(&mut self, axis: PaginatorAxis, cx: &mut Context<Self>) -> bool {
        self.turn_page(AssignmentDirection::Forward, axis, cx)
    }

    fn turn_page(&mut self, direction: AssignmentDirection, axis: PaginatorAxis, cx: &mut Context<Self>) -> bool {
        if !self.allows_axis(axis) || self.transition.is_some() {
            return false;
        }
        let Some(page) = self.visible_page else { return false };
        let Some(offset) = page.adjacent_offset(direction, self.data.children.len()) else { return false };
        self.clear_selection(cx);
        self.capture_page_transition(direction, axis, page);
        let warmed = self
            .warmed_page
            .take()
            .filter(|warmed| warmed.direction == direction && self.prefetched.as_ref() == Some(&warmed.source) && self.measurements.as_ref().is_some_and(|measurements| Rc::ptr_eq(&measurements.widths, &warmed.widths)));
        self.child_offset = offset;
        self.direction = direction;
        self.visible_page = None;
        if let Some(warmed) = warmed {
            self.content_height = Some(warmed.content_height);
            self.update_visible_page(self.children_revision, Some(warmed.page));
        }
        self.prefetched = None;
        self.maybe_start_page_transition(cx);
        if let Some(previous) = self.profile_turn.replace(crate::library::pages::browse::profile::Stage::deferred("page turn to rendered frame")) {
            previous.cancel();
        }
        cx.notify();
        true
    }

    pub(crate) fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub(crate) fn selection(&self) -> &BTreeSet<usize> {
        &self.selection
    }

    /// A keyboard menu acts on the selection before the previous cursor.
    pub(crate) fn context_menu_index(&self) -> Option<usize> {
        self.selection.iter().next().copied().or_else(|| self.cursor())
    }

    /// Where the last painted frame put a child, for anchoring a menu opened
    /// from the keyboard rather than from a pointer.
    pub(crate) fn child_bounds(&self, child_index: usize) -> Option<Bounds<Pixels>> {
        self.child_bounds.iter().find(|(index, _)| *index == child_index).map(|(_, bounds)| *bounds)
    }

    /// Drops the selection, reporting whether there was one. Every path that
    /// invalidates the indices — new children, a page turn, the keyboard cursor
    /// moving — goes through here.
    pub(crate) fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        self.keyboard_selection_active = false;
        if self.selection.is_empty() && self.marquee.is_none() {
            return false;
        }
        self.selection.clear();
        self.marquee = None;
        cx.notify();
        true
    }

    pub(super) fn set_child_bounds(&mut self, bounds: Vec<(usize, Bounds<Pixels>)>) {
        // Reported every frame from prepaint, so this deliberately does not
        // notify: it is a record of the frame that was just drawn, not a change
        // to anything drawn from.
        self.child_bounds = Rc::new(bounds);
    }

    pub(super) fn begin_marquee(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.clear_selection(cx);
        self.marquee = Some(Marquee { anchor: position, current: position, active: false });
    }

    pub(super) fn drag_marquee(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(marquee) = self.marquee.as_mut() else { return };
        marquee.current = position;
        if !marquee.active {
            let travelled = f32::from(position.x - marquee.anchor.x).abs().max(f32::from(position.y - marquee.anchor.y).abs());
            if travelled < MARQUEE_THRESHOLD {
                return;
            }
            marquee.active = true;
        }
        let band = marquee.bounds();
        self.selection = self.child_bounds.iter().filter(|(_, bounds)| bounds.intersects(&band)).map(|(index, _)| *index).collect();
        cx.notify();
    }

    /// Ends the drag, keeping whatever it picked up.
    pub(super) fn end_marquee(&mut self, cx: &mut Context<Self>) {
        if self.marquee.take().is_some() {
            cx.notify();
        }
    }

    fn marquee_bounds(&self) -> Option<Bounds<Pixels>> {
        self.marquee.filter(|marquee| marquee.active).map(|marquee| marquee.bounds())
    }

    /// Whether a rubber-band drag is sweeping, suppressing child hover.
    pub(crate) fn marquee_active(&self) -> bool {
        self.marquee.is_some_and(|marquee| marquee.active)
    }

    fn child_anchor(&self) -> usize {
        self.visible_page.map_or(self.child_offset, |page| page.first_child)
    }

    /// Columns a group is laid out at, for the width the element last used.
    ///
    /// Geometry is a pure function of width and rem size, and the prefetch key
    /// the element already reports carries both, so cursor movement needs
    /// nothing published beyond what the element publishes today.
    fn columns_for_group(&self, group_index: usize) -> usize {
        let Some(key) = self.prefetched.as_ref() else {
            return 1;
        };
        geometry::group_geometries(&self.data, key.width, key.rem_size).get(group_index).map_or(1, |geometry| geometry.columns.max(1))
    }

    /// Moves the keyboard cursor, turning the page when it walks off the edge.
    /// Returns whether anything moved, so the caller knows to swallow the key.
    pub(crate) fn move_cursor(&mut self, movement: CursorMove, cx: &mut Context<Self>) -> bool {
        if self.data.children.is_empty() || self.transition.is_some() {
            return false;
        }
        // Arrow keys hand the page back to the cursor: one item, moved
        // deliberately, rather than whatever a band happened to sweep up.
        self.clear_selection(cx);
        let Some(current) = self.cursor else {
            // The first arrow key places the cursor rather than moving it, so
            // no keystroke is spent adopting a position the reader can see.
            return self.place_cursor(self.visible_page.map_or(0, |page| page.first_child), cx);
        };
        let Some(target) = self.cursor_target(current, movement) else {
            return false;
        };
        self.place_cursor(target, cx)
    }

    /// Includes the starting item once, then moves and toggles destinations.
    pub(crate) fn move_and_toggle_selection(&mut self, movement: CursorMove, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.toggle_cursor_selection(movement) else {
            return false;
        };
        // Cursor-driven page turns normally clear selection. Keep it while
        // revealing the destination so keyboard picks can span pages.
        let selection = std::mem::take(&mut self.selection);
        self.reveal_cursor(target, cx);
        self.selection = selection;
        self.keyboard_selection_active = true;
        cx.notify();
        true
    }

    fn toggle_cursor_selection(&mut self, movement: CursorMove) -> Option<usize> {
        if !self.selectable || self.data.children.is_empty() || self.transition.is_some() {
            return None;
        }
        let target = match self.cursor {
            Some(current) => self.cursor_target(current, movement)?,
            None => self.visible_page.map_or(0, |page| page.first_child),
        };
        if !self.keyboard_selection_active
            && let Some(current) = self.cursor
        {
            self.selection.insert(current);
        }
        self.keyboard_selection_active = true;
        self.cursor = Some(target);
        self.marquee = None;
        if !self.selection.remove(&target) {
            self.selection.insert(target);
        }
        Some(target)
    }

    fn cursor_target(&self, current: usize, movement: CursorMove) -> Option<usize> {
        let last = self.data.children.len().checked_sub(1)?;
        match movement {
            // Left and right walk the flat child order, so stepping right off
            // the last folder lands on the first book: reading order, and the
            // order the groups were declared in.
            CursorMove::Left => current.checked_sub(1),
            CursorMove::Right => (current < last).then_some(current + 1),
            CursorMove::Up | CursorMove::Down => self.vertical_cursor_target(current, movement, last),
        }
    }

    /// Vertical movement is by the column count of the cursor's own group.
    ///
    /// Groups carry their own widths — folder chips are wider than book cards —
    /// so their rows do not line up, and crossing a boundary cannot be a fixed
    /// stride. The column within the row is preserved as closely as the group
    /// being entered allows.
    fn vertical_cursor_target(&self, current: usize, movement: CursorMove, last: usize) -> Option<usize> {
        if self.data.groups.iter().any(|group| group.policy.intrinsic) {
            let key = self.prefetched.as_ref()?;
            let widths = &self.measurements.as_ref()?.widths;
            let geometries = geometry::group_geometries(&self.data, key.width, key.rem_size);
            // Match the visible page's packing, including a restored partial row.
            let start = self.visible_page.map_or(0, |page| page.first_child);
            let mut rows = if start == 0 { Vec::new() } else { geometry::page_rows(&self.data, widths, &geometries, AssignmentDirection::Backward, start - 1, key.height, key.rem_size) };
            rows.extend(geometry::forward_rows_in_range(&self.data, widths, &geometries, start, self.data.children.len()));
            let row_index = rows.iter().position(|row| row.items.iter().any(|item| item.child_index == current))?;
            let current_item = rows[row_index].items.iter().find(|item| item.child_index == current)?;
            let center = current_item.x + current_item.width / 2.0;
            let target_row = match movement {
                CursorMove::Up => row_index.checked_sub(1)?,
                CursorMove::Down => row_index + 1,
                _ => return None,
            };
            return rows
                .get(target_row)?
                .items
                .iter()
                .min_by(|a, b| {
                    let distance = |item: &geometry::PageItem| f32::from(item.x + item.width / 2.0 - center).abs();
                    distance(a).total_cmp(&distance(b))
                })
                .map(|item| item.child_index);
        }
        let group_index = self.data.group_index_for_child(current)?;
        let group = self.data.groups[group_index];
        let columns = self.columns_for_group(group_index);
        let column = (current - group.start) % columns;
        match movement {
            CursorMove::Up => {
                if current >= group.start + columns {
                    return Some(current - columns);
                }
                let previous_index = group_index.checked_sub(1)?;
                let previous = *self.data.groups.get(previous_index)?;
                let previous_columns = self.columns_for_group(previous_index);
                let last_row_start = previous.start + (previous.len().div_ceil(previous_columns) - 1) * previous_columns;
                Some((last_row_start + column.min(previous_columns - 1)).min(previous.end - 1))
            }
            CursorMove::Down => {
                let target = current + columns;
                if target < group.end {
                    return Some(target);
                }
                match self.data.groups.get(group_index + 1) {
                    Some(next) => {
                        let next_columns = self.columns_for_group(group_index + 1);
                        Some((next.start + column.min(next_columns - 1)).min(next.end - 1))
                    }
                    // Nothing below: settle on the final child rather than
                    // refusing, so a short last row is still reachable.
                    None => (current < last).then_some(last),
                }
            }
            CursorMove::Left | CursorMove::Right => None,
        }
    }

    /// Runs the cursor child's activation — the same callback a click runs, so
    /// Enter and a click cannot diverge.
    /// What Enter on the cursor would do, handed out rather than run.
    ///
    /// Activating a child navigates, and navigation can complete synchronously —
    /// a location that needs no query, such as a book's own page — which comes
    /// straight back through the page to the paginator to install new groups.
    /// Running the activation while the paginator is leased is a panic, so the
    /// caller takes this, lets go, and only then calls it.
    pub(crate) fn cursor_activation(&self) -> Option<Rc<ActivateChild>> {
        self.data.children.get(self.cursor?).and_then(|child| child.activate.clone())
    }

    fn place_cursor(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(last) = self.data.children.len().checked_sub(1) else {
            return false;
        };
        let index = index.min(last);
        if self.cursor == Some(index) {
            return false;
        }
        self.cursor = Some(index);
        self.reveal_cursor(index, cx);
        cx.notify();
        true
    }

    fn reveal_cursor(&mut self, index: usize, cx: &mut Context<Self>) {
        // Walking off the page is what turns it: one step past the last row
        // lands on the first row of the next page, so paging is a consequence
        // of moving rather than a second gesture.
        if let Some(page) = self.visible_page {
            if index < page.first_child {
                self.previous_with_axis(self.flow_axis, cx);
            } else if index > page.last_child {
                self.next_with_axis(self.flow_axis, cx);
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.data.children.is_empty()
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

    /// Whether the page on screen is measured and holds the last child.
    pub(crate) fn on_last_page(&self) -> bool {
        self.visible_page.is_some_and(|page| page.last_child.saturating_add(1) >= self.data.children.len())
    }

    /// Defer asynchronous page turns until layout and any animation settle.
    pub(crate) fn page_turn_ready(&self) -> bool {
        self.visible_page.is_some() && self.transition.is_none()
    }

    /// Measured page plus its next-item preview, for lazy section book reads.
    pub(crate) fn visible_child_range(&self) -> Option<Range<usize>> {
        let page = self.visible_page?;
        Some(page.first_child..page.last_child.saturating_add(2).min(self.data.children.len()))
    }

    pub(crate) fn adjacent_child_range(&self, next: bool) -> Option<Range<usize>> {
        let page = self.visible_page?;
        let columns = self.data.group_index_for_child(page.first_child).map_or(1, |group| self.columns_for_group(group));
        let count = page.last_child.saturating_sub(page.first_child).saturating_add(1).max(columns);
        if next {
            let start = page.last_child.checked_add(1)?;
            (start < self.data.children.len()).then(|| start..start.saturating_add(count).saturating_add(1).min(self.data.children.len()))
        } else {
            let end = page.first_child;
            (end > 0).then(|| end.saturating_sub(count)..end.saturating_add(1).min(self.data.children.len()))
        }
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

    fn capture_page_transition(&mut self, direction: AssignmentDirection, axis: PaginatorAxis, page: VisiblePage) {
        if !self.animate_page_transitions {
            return;
        }
        self.transition_sequence = self.transition_sequence.wrapping_add(1);
        self.transition =
            Some(PageTransition { sequence: self.transition_sequence, direction, axis, outgoing_page: page, outgoing_content_height: self.content_height, started_at: None, drag_offset: Point::default(), touch_duration: None });
    }

    fn maybe_start_page_transition(&mut self, cx: &mut Context<Self>) {
        // The element reports a visible page only once it has laid one out,
        // so this being set is the signal that the incoming page is ready.
        if self.visible_page.is_none() {
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
        self.warm_request = None;
        self.swipe_preview.clear();
    }

    #[cfg(not(feature = "mobile"))]
    fn handle_mouse_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(event.delta, ScrollDelta::Pixels(_)) || self.touchpad.is_active() || matches!(event.delta, ScrollDelta::Lines(delta) if delta.x.abs() > delta.y.abs()) {
            let vertical = match event.delta {
                ScrollDelta::Pixels(delta) => delta.y.abs() >= delta.x.abs(),
                ScrollDelta::Lines(delta) | ScrollDelta::Pages(delta) => delta.y.abs() >= delta.x.abs(),
            };
            if !self.allow_vertical_navigation && vertical {
                return;
            }
            if let Some(travel) = self.touchpad.update(event) {
                let (axis, delta) = if travel.x != 0.0 { (PaginatorAxis::Horizontal, travel.x) } else { (PaginatorAxis::Vertical, travel.y) };
                if self.allows_axis(axis) {
                    if delta < 0.0 {
                        self.next_with_axis(axis, cx);
                    } else {
                        self.previous_with_axis(axis, cx);
                    }
                }
            }
            cx.stop_propagation();
            return;
        }
        if self.flow_axis != PaginatorAxis::Vertical || !self.allows_axis(PaginatorAxis::Vertical) {
            return;
        }
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
            ScrollDelta::Lines(delta) | ScrollDelta::Pages(delta) => delta.y,
        };
        if !delta.is_finite() || delta == 0.0 {
            return;
        }

        // Only fresh input can turn a page. Nothing is queued during the
        // throttle interval or animation, so stopping input stops navigation.
        let now = Instant::now();
        if self.transition.is_some() || self.last_wheel_turn.is_some_and(|last| now.duration_since(last) < SCROLL_REPEAT_INTERVAL) {
            cx.stop_propagation();
            return;
        }
        let turned = match wheel_direction(delta) {
            Some(AssignmentDirection::Forward) => self.next_with_axis(PaginatorAxis::Vertical, cx),
            Some(AssignmentDirection::Backward) => self.previous_with_axis(PaginatorAxis::Vertical, cx),
            None => false,
        };
        if turned {
            self.last_wheel_turn = Some(now);
        }
        cx.stop_propagation();
    }

    fn update_visible_page(&mut self, children_revision: usize, visible_page: Option<VisiblePage>) -> bool {
        if self.children_revision != children_revision {
            return false;
        }
        let consumed_backward = if let (AssignmentDirection::Backward, Some(page)) = (self.direction, visible_page) {
            self.child_offset = page.first_child;
            self.direction = AssignmentDirection::Forward;
            true
        } else {
            false
        };
        if self.visible_page == visible_page && !consumed_backward {
            return false;
        }
        self.visible_page = visible_page;
        // A cursor the reader can no longer see is not a selection. Turning the
        // page with the wheel leaves it behind, so it is dropped here rather
        // than dragged along invisibly; walking the cursor off the edge turns
        // the page to follow it, so that cursor lands inside the new page and
        // is kept.
        if let (Some(cursor), Some(page)) = (self.cursor, visible_page)
            && (cursor < page.first_child || cursor > page.last_child)
        {
            self.cursor = None;
        }
        true
    }

    fn set_content_height(&mut self, children_revision: usize, content_height: Pixels, cx: &mut Context<Self>) {
        if self.children_revision != children_revision || self.content_height == Some(content_height) {
            return;
        }
        self.content_height = Some(content_height);
        // A settled page has already painted at this height. Only an active
        // vertical pan uses it to change what the next frame draws.
        if self.transition.is_some() {
            cx.notify();
        }
    }

    fn set_prefetched(&mut self, key: PrefetchKey) {
        // Records preparation and geometry from the frame just drawn. Neither
        // changes its appearance; notifying here redraws every prepared page.
        self.update_prefetched(key);
    }

    fn update_prefetched(&mut self, key: PrefetchKey) -> bool {
        if self.children_revision != key.children_revision || self.prefetched.as_ref() == Some(&key) {
            return false;
        }
        self.prefetched = Some(key);
        true
    }

    fn handle_swipe(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Kobo page buttons arrive as one completed page request, without a
        // finger-down event. They must not depend on an active swipe or focus.
        if let ScrollDelta::Pages(delta) = event.delta {
            let had_swipe = self.swipe.take().and_then(|swipe| swipe.axis).is_some_and(|axis| self.allows_axis(axis));
            let (axis, delta) = if delta.x.abs() > delta.y.abs() { (PaginatorAxis::Horizontal, delta.x) } else { (PaginatorAxis::Vertical, delta.y) };
            let turn = (self.allows_axis(axis) && delta.is_finite() && delta != 0.0).then_some(delta);
            if turn.is_some() || had_swipe {
                self.finish_swipe(axis, turn, had_swipe, window, cx);
            }
            return;
        }
        let ScrollDelta::Pixels(delta) = event.delta else {
            if matches!(event.touch_phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                self.swipe = None;
            }
            return;
        };
        let x = f32::from(delta.x);
        let y = f32::from(delta.y);

        match event.touch_phase {
            TouchPhase::Started | TouchPhase::Moved => {
                if event.touch_phase == TouchPhase::Started {
                    // A fresh drag supersedes any previous settle animation.
                    self.cancel_page_transition();
                    self.swipe = Some(PaginatorSwipe::default());
                }
                let Some(swipe) = self.swipe.as_mut() else {
                    return;
                };
                swipe.add(x, y);
                let axis = swipe.axis;
                let travel = match axis {
                    Some(PaginatorAxis::Horizontal) => point(swipe.x, 0.0),
                    _ => point(0.0, swipe.y),
                };
                if axis.is_some_and(|axis| self.allows_axis(axis)) {
                    if self.swipe_preview.drag(travel, window) {
                        cx.notify();
                    }
                    cx.stop_propagation();
                }
            }
            TouchPhase::Ended => {
                let Some(swipe) = self.swipe.take() else {
                    return;
                };
                let Some(axis) = swipe.axis.filter(|axis| self.allows_axis(*axis)) else {
                    return;
                };

                let (distance, viewport_extent) = match axis {
                    PaginatorAxis::Horizontal => (swipe.x, f32::from(window.viewport_size().width)),
                    PaginatorAxis::Vertical => (swipe.y, f32::from(window.viewport_size().height)),
                };
                let threshold = (viewport_extent * SWIPE_VIEWPORT_FRACTION).clamp(SWIPE_MINIMUM_DISTANCE, SWIPE_MAXIMUM_DISTANCE);
                self.finish_swipe(axis, (distance.abs() >= threshold).then_some(distance), true, window, cx);
            }
            TouchPhase::Cancelled => {
                self.swipe_preview.settle();
                cx.notify();
                if self.swipe.take().and_then(|swipe| swipe.axis).is_some_and(|axis| self.allows_axis(axis)) {
                    cx.stop_propagation();
                }
            }
        }
    }

    /// A qualifying release turns a page; every other consumed release settles
    /// back in place. Hardware requests use the normal page animation unless
    /// they finish a live drag.
    fn finish_swipe(&mut self, axis: PaginatorAxis, turn_delta: Option<f32>, dragged: bool, window: &mut Window, cx: &mut Context<Self>) {
        let offset = self.swipe_preview.offset(window, cx);
        let page_width = self.prefetched.as_ref().map_or(window.viewport_size().width, |key| key.width);
        let turned = turn_delta.is_some_and(|delta| if delta < 0.0 { self.next_with_axis(axis, cx) } else { self.previous_with_axis(axis, cx) });
        if turned {
            if let Some(transition) = self.transition.as_mut() {
                transition.drag_offset = offset;
                let extent = if axis == PaginatorAxis::Horizontal {
                    page_width
                } else if transition.direction == AssignmentDirection::Backward {
                    self.content_height.unwrap_or(window.viewport_size().height)
                } else {
                    transition.outgoing_content_height.unwrap_or(window.viewport_size().height)
                };
                let distance = if axis == PaginatorAxis::Horizontal { offset.x } else { offset.y };
                transition.touch_duration = dragged.then(|| touch_page_settle_duration(extent - distance.abs(), extent));
            }
            self.swipe_preview.clear();
        } else {
            self.swipe_preview.settle();
        }
        cx.notify();
        cx.stop_propagation();
    }
}

impl Render for Paginator {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(stage) = self.profile_turn.take() {
            window.on_next_frame(move |_, _| stage.finish("rendered"));
        }
        let id = ElementId::from(self.id.clone());
        let child_count = self.data.children.len();
        let child_offset = self.child_offset.min(child_count.saturating_sub(1));
        let rendered_transition = self.transition.clone().and_then(|transition| {
            let progress = transition.started_at.map(|started_at| started_at.elapsed().as_secs_f32() / transition.touch_duration.unwrap_or(PAGE_SCROLL_DURATION).as_secs_f32());
            if progress.is_some_and(|progress| progress >= 1.0) {
                self.transition = None;
                cx.notify();
                None
            } else {
                if progress.is_some() {
                    window.request_animation_frame();
                }
                let eased = progress.map(|progress| if transition.touch_duration.is_some() { touch_page_settle_easing(progress) } else { ease_in_out(progress.clamp(0.0, 1.0)) });
                Some((transition, eased))
            }
        });
        // Vertical transitions pan: both pages move by the height of the page
        // that lies between the two viewport positions, so they stay butted
        // together. Horizontal ones still slide by a fraction of the container,
        // where a page always fills the axis it moves along.
        let pan = rendered_transition.as_ref().and_then(|(transition, progress)| {
            let progress = (*progress)?;
            if transition.axis != PaginatorAxis::Vertical {
                return None;
            }
            let travel = match transition.direction {
                AssignmentDirection::Forward => transition.outgoing_content_height,
                AssignmentDirection::Backward => self.content_height,
            }?;
            let (outgoing, incoming) = page_transition_offsets(transition.direction, progress);
            Some((travel * outgoing, travel * incoming))
        });
        let incoming_offset = match pan {
            Some(_) => (0.0, 0.0),
            None => rendered_transition.as_ref().and_then(|(transition, progress)| progress.map(|progress| page_transition_vector(transition.axis, page_transition_offsets(transition.direction, progress).1))).unwrap_or((0.0, 0.0)),
        };
        let drag_offset = rendered_transition.as_ref().map_or_else(|| self.swipe_preview.offset(window, cx), |(transition, progress)| transition.drag_offset * (1.0 - progress.unwrap_or(0.0)));
        let state = Rc::new(PaginatorState { cursor: self.cursor, selection: self.selection.clone(), marquee: self.marquee_active(), preview: false, bounds: self.child_bounds.clone(), data: self.data.clone() });
        let warm_direction = self.swipe.as_ref().and_then(|swipe| {
            let axis = swipe.axis.filter(|axis| self.allows_axis(*axis))?;
            let travel = if axis == PaginatorAxis::Horizontal { swipe.x } else { swipe.y };
            (travel != 0.0).then_some(if travel < 0.0 { AssignmentDirection::Forward } else { AssignmentDirection::Backward })
        });
        let incoming = PaginatorElement::new(
            ElementId::named_usize(format!("{id}-layout"), self.children_revision),
            cx.entity().downgrade(),
            self,
            PaginatorPage { reported_visible_page: self.visible_page, child_offset, direction: self.direction, prefetched: self.prefetched.clone(), forced_page: None },
            window.rem_size(),
            state.clone(),
        )
        .with_warm_direction(warm_direction)
        .with_selection(self.selectable, self.marquee_bounds())
        .with_page_offset(incoming_offset.0, incoming_offset.1)
        .with_page_pixels(pan.map(|(_, incoming)| incoming).unwrap_or(Pixels::ZERO), self.content_height)
        .with_drag_offset(drag_offset);

        let mut content = div().relative().w_full().min_h_0().min_w_0().overflow_hidden().h_full().flex_1();
        if let Some((transition, progress)) = rendered_transition {
            let outgoing_offset = match pan {
                Some(_) => (0.0, 0.0),
                None => progress.map(|progress| page_transition_vector(transition.axis, page_transition_offsets(transition.direction, progress).0)).unwrap_or((0.0, 0.0)),
            };
            let outgoing = PaginatorElement::new(
                ElementId::named_usize(format!("{id}-outgoing-layout"), transition.sequence),
                cx.entity().downgrade(),
                self,
                PaginatorPage {
                    reported_visible_page: Some(transition.outgoing_page),
                    child_offset: transition.outgoing_page.first_child,
                    direction: AssignmentDirection::Forward,
                    prefetched: None,
                    forced_page: Some(transition.outgoing_page),
                },
                window.rem_size(),
                state,
            )
            .passive()
            .with_page_offset(outgoing_offset.0, outgoing_offset.1)
            .with_page_pixels(pan.map(|(outgoing, _)| outgoing).unwrap_or(Pixels::ZERO), transition.outgoing_content_height)
            .with_drag_offset(drag_offset);

            let incoming_page = div().relative().w_full().min_h_0().min_w_0().h_full().flex_1().child(incoming);
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

        let paginator = div().id(id).w_full().min_h_0().min_w_0().flex().flex_col().h_full().flex_1();
        let paginator = paginator.on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
            if cfg!(feature = "mobile") || window.is_page_scroll_swipe() || this.swipe.is_some() || matches!(event.delta, ScrollDelta::Pages(_)) {
                this.handle_swipe(event, window, cx);
            } else {
                #[cfg(not(feature = "mobile"))]
                this.handle_mouse_wheel(event, window, cx);
            }
        }));
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VisiblePage {
    pub(super) first_child: usize,
    pub(super) last_child: usize,
}

impl VisiblePage {
    fn adjacent_offset(self, direction: AssignmentDirection, child_count: usize) -> Option<usize> {
        match direction {
            AssignmentDirection::Backward => self.first_child.checked_sub(1),
            AssignmentDirection::Forward => {
                let offset = self.last_child.checked_add(1)?;
                (offset < child_count).then_some(offset)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PrefetchKey {
    pub(super) children_revision: usize,
    pub(super) page: VisiblePage,
    pub(super) width: Pixels,
    pub(super) height: Pixels,
    pub(super) rem_size: Pixels,
}

#[cfg(test)]
mod tests {
    use super::{AssignmentDirection, CursorMove, Paginator, PaginatorChild, PaginatorData, PaginatorGroup, PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy, PrefetchKey, VisiblePage};
    use gpui::{SharedString, px};
    use std::collections::{BTreeSet, HashMap};
    use std::rc::Rc;

    #[gpui::test]
    fn sparse_book_slots_page_to_distant_loaded_items_without_materializing_placeholders(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        use std::cell::Cell;

        cx.update(gpui_component::init);
        let renders = Rc::new(Cell::new(0));
        let rendered = renders.clone();
        let placeholder = PaginatorChild::new(move |_, _, _| {
            rendered.set(rendered.get() + 1);
            gpui::div().into_any_element()
        });
        let children = super::PaginatorChildren::sparse(1_000_000, placeholder.clone(), [(999_999, placeholder.clone().with_id("last-book"))].into_iter().collect());
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(40.0)), px(0.0));
        let groups = [PaginatorGroup::new(policy, [placeholder.with_id("prefix")]), PaginatorGroup::from_slots(policy, children)];
        let (paginator, visual) = cx.add_window_view(|_, _| Paginator::new("sparse-books", groups).next_page_preview(false));
        visual.simulate_resize(gpui::size(px(800.0), px(80.0)));
        renders.set(0);
        visual.run_until_parked();
        paginator.read_with(visual, |paginator, _| {
            assert_eq!(paginator.data.children.len(), 1_000_001);
            assert_eq!(paginator.data.children.stored().count(), 2);
            assert_eq!(paginator.data.indices.len(), 2);
            assert_eq!(paginator.measurements.as_ref().unwrap().widths.stored().count(), 2);
            assert_eq!(paginator.child_id(1_000_000).map(SharedString::as_str), Some("last-book"));
        });
        assert!(renders.get() < 100);
        renders.set(0);
        paginator.update(visual, |paginator, cx| {
            paginator.child_offset = 999_990;
            cx.notify();
        });
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        paginator.read_with(visual, |paginator, _| assert!(paginator.visible_child_range().unwrap().contains(&1_000_000)));
        assert!(renders.get() < 100, "a distant page must not create or measure the intervening placeholders");
    }

    #[cfg(not(feature = "mobile"))]
    #[gpui::test]
    fn touchpad_turns_during_movement_and_stops_on_release(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase, div, point};

        cx.update(gpui_component::init);
        let (paginator, visual) = cx.add_window_view(|_, _| Paginator::new("touchpad", []).flow_axis(super::PaginatorAxis::Horizontal));
        visual.update(|window, cx| {
            paginator.update(cx, |paginator, cx| {
                paginator.data = Rc::new(PaginatorData { indices: Default::default(), children: (0..12).map(|_| PaginatorChild::new(|_, _, _| div().into_any_element())).collect(), groups: Vec::new() });
                paginator.visible_page = Some(VisiblePage { first_child: 0, last_child: 3 });
                paginator.animate_page_transitions = false;
                let scroll = |x, touch_phase| ScrollWheelEvent { delta: ScrollDelta::Pixels(point(px(x), px(0.0))), touch_phase, ..Default::default() };
                paginator.handle_mouse_wheel(&scroll(-10.0, TouchPhase::Started), window, cx);
                assert_eq!(paginator.child_offset, 0);
                paginator.handle_mouse_wheel(&scroll(-60.0, TouchPhase::Moved), window, cx);
                assert_eq!(paginator.child_offset, 4, "continuous navigation starts during movement");
                paginator.handle_mouse_wheel(&scroll(0.0, TouchPhase::Ended), window, cx);
                assert_eq!(paginator.child_offset, 4);
                paginator.visible_page = Some(VisiblePage { first_child: 4, last_child: 7 });
                paginator.handle_mouse_wheel(&scroll(0.0, TouchPhase::Ended), window, cx);
                paginator.handle_mouse_wheel(&scroll(-160.0, TouchPhase::Moved), window, cx);
                assert_eq!(paginator.child_offset, 4, "momentum must not skip another page");
                paginator.flow_axis = super::PaginatorAxis::Vertical;
                let drift = ScrollWheelEvent { delta: ScrollDelta::Lines(point(0.0, -3.0)), ..Default::default() };
                paginator.handle_mouse_wheel(&drift, window, cx);
                assert_eq!(paginator.child_offset, 4, "cross-axis drift must not become a separate wheel turn");
                paginator.handle_mouse_wheel(&scroll(60.0, TouchPhase::Started), window, cx);
                paginator.handle_mouse_wheel(&scroll(0.0, TouchPhase::Ended), window, cx);
                assert_eq!(paginator.child_offset, 4, "a new gesture must also respect the repeat interval");
            });
        });
    }

    #[cfg(not(feature = "mobile"))]
    #[gpui::test]
    fn wheel_turns_require_fresh_input_after_the_repeat_interval(cx: &mut gpui::TestAppContext) {
        use super::SCROLL_REPEAT_INTERVAL;
        use gpui::{IntoElement, ScrollDelta, ScrollWheelEvent, div, point};
        use web_time::Instant;

        cx.update(gpui_component::init);
        let (paginator, visual) = cx.add_window_view(|_, _| Paginator::new("wheel-repeat", []));
        visual.update(|window, cx| {
            paginator.update(cx, |paginator, cx| {
                paginator.data = Rc::new(PaginatorData { indices: Default::default(), children: (0..24).map(|_| PaginatorChild::new(|_, _, _| div().into_any_element())).collect(), groups: Vec::new() });
                paginator.animate_page_transitions = false;
                let scroll = ScrollWheelEvent { delta: ScrollDelta::Lines(point(0.0, -1.0)), ..Default::default() };
                for page in 0..5 {
                    let first_child = page * 4;
                    paginator.visible_page = Some(VisiblePage { first_child, last_child: first_child + 3 });
                    if page > 0 {
                        paginator.handle_mouse_wheel(&scroll, window, cx);
                        assert_eq!(paginator.child_offset, first_child, "rapid events are discarded");
                        paginator.last_wheel_turn = Some(Instant::now() - SCROLL_REPEAT_INTERVAL);
                    }
                    paginator.handle_mouse_wheel(&scroll, window, cx);
                    assert_eq!(paginator.child_offset, first_child + 4);
                }
                paginator.visible_page = Some(VisiblePage { first_child: 20, last_child: 23 });
                paginator.last_wheel_turn = Some(Instant::now() - SCROLL_REPEAT_INTERVAL);
                let zero = ScrollWheelEvent { delta: ScrollDelta::Lines(point(0.0, 0.0)), ..Default::default() };
                paginator.handle_mouse_wheel(&zero, window, cx);
                assert_eq!(paginator.child_offset, 20, "elapsed time without movement cannot turn a page");
            });
        });
    }

    #[gpui::test]
    fn book_previews_only_join_selection_started_in_empty_space(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        use gpui::{MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, div, point};

        cx.update(gpui_component::init);
        let (paginator, visual) = cx.add_window_view(|_, _| {
            let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(150.0)), px(20.0))
                .with_gap(px(20.0))
                .with_edge_inset(px(20.0))
                .with_maximum_width(PaginatorWidthPolicy::pixels(px(100.0)))
                .pack_start();
            let children = (0..2).map(|index| PaginatorChild::new(|_, _, _| div().size_full().into_any_element()).with_id(index.to_string()).prevent_selection_start());
            Paginator::new("book-previews", [PaginatorGroup::new(policy, children)]).select_on_drag()
        });
        visual.run_until_parked();
        let (first, second) = visual.update(|_, cx| {
            let paginator = paginator.read(cx);
            (paginator.child_bounds(0).unwrap().center(), paginator.child_bounds(1).unwrap().center())
        });
        visual.simulate_event(MouseDownEvent { position: first, button: MouseButton::Left, ..Default::default() });
        visual.simulate_event(MouseMoveEvent { position: second, pressed_button: Some(MouseButton::Left), ..Default::default() });
        visual.run_until_parked();
        assert!(visual.update(|_, cx| paginator.read(cx).selection().is_empty()));
        assert!(!visual.update(|_, cx| paginator.read(cx).marquee_active()));
        visual.simulate_event(MouseUpEvent { position: second, button: MouseButton::Left, ..Default::default() });

        visual.simulate_event(MouseDownEvent { position: point(px(5.0), px(5.0)), button: MouseButton::Left, ..Default::default() });
        visual.simulate_event(MouseMoveEvent { position: second, pressed_button: Some(MouseButton::Left), ..Default::default() });
        visual.run_until_parked();
        visual.simulate_event(MouseUpEvent { position: second, button: MouseButton::Left, ..Default::default() });
        visual.run_until_parked();
        assert_eq!(visual.update(|_, cx| paginator.read(cx).selection().clone()), BTreeSet::from([0, 1]));

        visual.simulate_event(MouseDownEvent { position: first, button: MouseButton::Left, ..Default::default() });
        visual.simulate_event(MouseMoveEvent { position: first + point(px(10.0), px(0.0)), pressed_button: Some(MouseButton::Left), ..Default::default() });
        visual.run_until_parked();
        assert!(!visual.update(|_, cx| paginator.read(cx).marquee_active()));
        assert_eq!(visual.update(|_, cx| paginator.read(cx).selection().clone()), BTreeSet::from([0, 1]));
    }

    #[gpui::test]
    fn recording_settled_layout_and_prefetch_does_not_request_another_repaint(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext;
        use std::cell::Cell;

        let paginator = cx.new(|_| selection_grid(12));
        let notifications = Rc::new(Cell::new(0));
        let observed = notifications.clone();
        let _subscription = cx.update(|cx| cx.observe(&paginator, move |_, _| observed.set(observed.get() + 1)));
        paginator.update(cx, |paginator, cx| {
            paginator.set_content_height(paginator.children_revision, px(300.0), cx);
            paginator.set_prefetched(PrefetchKey { children_revision: paginator.children_revision, page: VisiblePage { first_child: 0, last_child: 7 }, width: px(400.0), height: px(300.0), rem_size: px(16.0) });
        });
        assert_eq!(notifications.get(), 0);
        cx.read_entity(&paginator, |paginator, _| {
            assert_eq!(paginator.content_height, Some(px(300.0)));
            assert_eq!(paginator.columns_for_group(0), 4, "keyboard geometry is still recorded");
        });
    }

    fn selection_grid(count: usize) -> Paginator {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(150.0)), px(0.0)).with_minimum_columns(4);
        let children = (0..count).map(|index| PaginatorChild::new(|_, _, _| unreachable!()).with_id(index.to_string()));
        let mut paginator = Paginator::new("selection-test", [PaginatorGroup::new(policy, children)]).selectable();
        let page = VisiblePage { first_child: 0, last_child: count.saturating_sub(1) };
        paginator.visible_page = Some(page);
        paginator.prefetched = Some(PrefetchKey { children_revision: 0, page, width: px(400.0), height: px(600.0), rem_size: px(16.0) });
        paginator
    }

    #[test]
    fn lazy_section_ranges_include_the_preview_and_a_full_previous_page_from_a_short_last_page() {
        let mut paginator = selection_grid(10);
        let first = VisiblePage { first_child: 0, last_child: 3 };
        paginator.visible_page = Some(first);
        paginator.prefetched = Some(PrefetchKey { children_revision: paginator.children_revision, page: first, width: px(400.0), height: px(150.0), rem_size: px(16.0) });
        assert_eq!(paginator.visible_child_range(), Some(0..5));
        assert_eq!(paginator.adjacent_child_range(false), None);
        assert_eq!(paginator.adjacent_child_range(true), Some(4..9));
        paginator.visible_page = Some(VisiblePage { first_child: 8, last_child: 9 });
        assert_eq!(paginator.adjacent_child_range(true), None);
        assert_eq!(paginator.adjacent_child_range(false), Some(4..9));
        assert_eq!(paginator.visible_child_range(), Some(8..10));
    }

    #[test]
    fn keyboard_selection_vertical_moves_include_current_and_toggle_destination() {
        let mut paginator = selection_grid(12);
        paginator.cursor = Some(1);
        paginator.selection = BTreeSet::from([1, 10]);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Down), Some(5));
        assert_eq!(paginator.cursor(), Some(5));
        assert_eq!(paginator.selection(), &BTreeSet::from([1, 5, 10]));
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Up), Some(1));
        assert_eq!(paginator.selection(), &BTreeSet::from([5, 10]));
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Down), Some(5));
        assert_eq!(paginator.selection(), &BTreeSet::from([10]));
    }

    #[test]
    fn keyboard_selection_horizontal_moves_toggle_existing_picks() {
        let mut paginator = selection_grid(12);
        paginator.cursor = Some(1);
        paginator.selection = BTreeSet::from([10]);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Right), Some(2));
        assert_eq!(paginator.selection(), &BTreeSet::from([1, 2, 10]));
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Left), Some(1));
        assert_eq!(paginator.selection(), &BTreeSet::from([2, 10]));
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Right), Some(2));
        assert_eq!(paginator.selection(), &BTreeSet::from([10]));
    }

    #[test]
    fn keyboard_selection_can_remove_every_pick_without_readding_departed_items() {
        let mut paginator = selection_grid(12);
        paginator.cursor = Some(1);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Right), Some(2));
        assert_eq!(paginator.selection(), &BTreeSet::from([1, 2]));
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Left), Some(1));
        assert_eq!(paginator.selection(), &BTreeSet::from([2]));
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Right), Some(2));
        assert!(paginator.selection().is_empty());
        assert_eq!(paginator.cursor(), Some(2));
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Left), Some(1));
        assert_eq!(paginator.selection(), &BTreeSet::from([1]));
    }

    #[test]
    fn keyboard_selection_starts_with_current_item_in_every_direction() {
        for (movement, target) in [(CursorMove::Left, 4), (CursorMove::Right, 6), (CursorMove::Up, 1), (CursorMove::Down, 9)] {
            let mut paginator = selection_grid(12);
            paginator.cursor = Some(5);
            assert_eq!(paginator.toggle_cursor_selection(movement), Some(target));
            assert_eq!(paginator.selection(), &BTreeSet::from([5, target]));
        }
    }

    #[test]
    fn keyboard_selection_boundaries_leave_selection_unchanged() {
        let mut paginator = selection_grid(12);
        paginator.cursor = Some(0);
        paginator.selection = BTreeSet::from([0, 8]);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Left), None);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Up), None);
        assert_eq!(paginator.cursor(), Some(0));
        assert_eq!(paginator.selection(), &BTreeSet::from([0, 8]));
        paginator.cursor = Some(11);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Right), None);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Down), None);
        assert_eq!(paginator.selection(), &BTreeSet::from([0, 8]));
    }

    #[test]
    fn keyboard_selection_starts_on_visible_page_and_handles_short_rows() {
        let mut paginator = selection_grid(10);
        paginator.visible_page = Some(VisiblePage { first_child: 4, last_child: 9 });
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Right), Some(4));
        assert_eq!(paginator.selection(), &BTreeSet::from([4]));
        paginator.cursor = Some(7);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Down), Some(9));
        assert_eq!(paginator.selection(), &BTreeSet::from([4, 9]));
    }

    #[test]
    fn keyboard_selection_requires_selectable_nonempty_grid() {
        let mut paginator = selection_grid(0);
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Right), None);
        let mut paginator = selection_grid(4);
        paginator.selectable = false;
        assert_eq!(paginator.toggle_cursor_selection(CursorMove::Right), None);
        assert_eq!(paginator.cursor(), None);
        assert!(paginator.selection().is_empty());
    }

    #[test]
    fn loading_placeholders_preserves_positions_and_loaded_books_follow_their_identity() {
        let placeholder = PaginatorChild::new(|_, _, _| unreachable!());
        let before = PaginatorData { indices: HashMap::new(), children: vec![placeholder.clone(); 3].into(), groups: Vec::new() };
        let waiting = before.position(2).unwrap();
        let ids = ["book-a", "book-b", "book-c"].map(SharedString::from);
        let loaded = PaginatorData { indices: ids.iter().cloned().enumerate().map(|(index, id)| (id, index)).collect(), children: ids.iter().cloned().map(|id| placeholder.clone().with_id(id)).collect(), groups: Vec::new() };
        assert_eq!(loaded.resolve(&waiting), Some(2));
        let known = loaded.position(0).unwrap();
        let reordered = PaginatorData { indices: [(ids[0].clone(), 2), (ids[1].clone(), 1)].into_iter().collect(), children: vec![placeholder.clone(); 3].into(), groups: Vec::new() };
        assert_eq!(reordered.resolve(&known), Some(2));
        assert_eq!(reordered.resolve(&loaded.position(2).unwrap()), None);
    }

    #[gpui::test]
    fn refresh_shows_prepended_entries_at_the_start_and_preserves_later_pages(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext;

        for offset in [0, 2] {
            let paginator = cx.new(|_| selection_grid(4));
            paginator.update(cx, |paginator, cx| {
                paginator.child_offset = offset;
                paginator.visible_page = Some(VisiblePage { first_child: offset, last_child: 3 });
                paginator.cursor = Some(offset);
                paginator.selection = BTreeSet::from([offset]);
                let policy = paginator.data.groups[0].policy.clone();
                let children = std::iter::once(PaginatorChild::new(|_, _, _| unreachable!()).with_id("new-folder")).chain(paginator.data.children.iter().cloned()).collect::<Vec<_>>();
                paginator.refresh_groups([PaginatorGroup::new(policy, children)], cx);
                assert_eq!(paginator.child_offset, if offset == 0 { 0 } else { offset + 1 });
                assert_eq!(paginator.cursor, Some(offset + 1));
                assert_eq!(paginator.selection, BTreeSet::from([offset + 1]));
            });
        }
    }

    #[test]
    fn keyboard_menu_targets_selection_instead_of_previous_cursor() {
        let mut paginator = Paginator::new("menu-test", []);
        let ids = ["a", "b", "c"].map(gpui::SharedString::from);
        paginator.data = Rc::new(PaginatorData {
            indices: ids.iter().cloned().enumerate().map(|(index, id)| (id, index)).collect(),
            children: ids.iter().cloned().map(|id| PaginatorChild::new(|_, _, _| unreachable!()).with_id(id)).collect(),
            groups: Vec::new(),
        });
        paginator.cursor = Some(2);
        paginator.selection = BTreeSet::from([0, 1]);
        let index = paginator.context_menu_index().unwrap();
        assert_eq!(paginator.target_ids(paginator.child_id(index).unwrap()), ids[..2]);
        paginator.selection.clear();
        assert_eq!(paginator.context_menu_index(), Some(2));
        paginator.cursor = None;
        assert_eq!(paginator.context_menu_index(), None);
    }

    #[test]
    fn adjacent_pages_stop_at_first_and_last_children() {
        let first = VisiblePage { first_child: 0, last_child: 2 };
        assert_eq!(first.adjacent_offset(AssignmentDirection::Backward, 8), None);
        assert_eq!(first.adjacent_offset(AssignmentDirection::Forward, 8), Some(3));
        let last = VisiblePage { first_child: 6, last_child: 7 };
        assert_eq!(last.adjacent_offset(AssignmentDirection::Backward, 8), Some(5));
        assert_eq!(last.adjacent_offset(AssignmentDirection::Forward, 8), None);
    }

    #[test]
    fn adjacent_page_offsets_follow_measured_page_boundaries() {
        let page = VisiblePage { first_child: 3, last_child: 5 };
        assert_eq!(page.adjacent_offset(AssignmentDirection::Backward, 8), Some(2));
        assert_eq!(page.adjacent_offset(AssignmentDirection::Forward, 8), Some(6));
    }

    #[test]
    fn overflowing_page_boundary_has_no_next_page() {
        let page = VisiblePage { first_child: 0, last_child: usize::MAX };
        assert_eq!(page.adjacent_offset(AssignmentDirection::Forward, usize::MAX), None);
    }

    #[gpui::test]
    fn touch_events_move_rendered_children_before_release_on_a_web_style_build(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase, div, point};

        cx.update(gpui_component::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(40.0)), px(0.0));
        let children = (0..100).map(|_| PaginatorChild::new(|_, _, _| div().into_any_element())).collect::<Vec<_>>();
        let (paginator, visual) = cx.add_window_view(|window, _| {
            window.set_page_scroll_on_swipe(true);
            Paginator::new("touch-web", [PaginatorGroup::new(policy, children)]).selectable().flow_axis(super::PaginatorAxis::Horizontal)
        });
        visual.simulate_resize(gpui::size(px(800.0), px(80.0)));
        visual.run_until_parked();
        let child_origin = |paginator: &Paginator| paginator.child_bounds.iter().find(|(index, _)| *index == 0).unwrap().1.origin;
        let original = visual.update(|_, cx| child_origin(paginator.read(cx)));
        let scroll = |dx, touch_phase| ScrollWheelEvent { position: point(px(50.0), px(20.0)), delta: ScrollDelta::Pixels(point(px(dx), px(0.0))), touch_phase, ..Default::default() };
        visual.simulate_event(scroll(-20.0, TouchPhase::Started));
        visual.simulate_event(scroll(-40.0, TouchPhase::Moved));
        visual.run_until_parked();
        assert_eq!(visual.update(|_, cx| child_origin(paginator.read(cx))), original + point(px(-60.0), px(0.0)));
        assert_eq!(visual.update(|_, cx| paginator.read(cx).child_offset), 0);
        visual.simulate_event(scroll(0.0, TouchPhase::Cancelled));
        visual.run_until_parked();
        assert_eq!(visual.update(|_, cx| child_origin(paginator.read(cx))), original);
    }

    #[gpui::test]
    fn drag_warms_one_neighbor_once_and_reuses_packing_until_release(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase, div, point};
        use std::cell::Cell;

        cx.update(gpui_component::init);
        let renders = Rc::new((0..100).map(|_| Cell::new(0usize)).collect::<Vec<_>>());
        let children = (0..100).map(|index| {
            let renders = renders.clone();
            PaginatorChild::new(move |_, _, _| {
                renders[index].set(renders[index].get() + 1);
                div().into_any_element()
            })
        });
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(40.0)), px(0.0));
        let (paginator, visual) = cx.add_window_view(|window, _| {
            window.set_page_scroll_on_swipe(true);
            Paginator::new("warm-swipe", [PaginatorGroup::new(policy, children)]).flow_axis(super::PaginatorAxis::Horizontal)
        });
        visual.simulate_resize(gpui::size(px(800.0), px(80.0)));
        visual.run_until_parked();
        let scroll = |dx, touch_phase| ScrollWheelEvent { position: point(px(50.0), px(20.0)), delta: ScrollDelta::Pixels(point(px(dx), px(0.0))), touch_phase, ..Default::default() };
        visual.simulate_event(scroll(-20.0, TouchPhase::Started));
        visual.run_until_parked();
        assert!(visual.update(|_, cx| paginator.read(cx).warmed_page.is_none()), "speculative layout must not delay the first visible drag frame");
        for _ in 0..4 {
            visual.simulate_scheduled_frame();
        }
        let (warmed, misses) = visual.update(|_, cx| {
            let paginator = paginator.read(cx);
            (paginator.warmed_page.as_ref().expect("neighbor layout must be ready during the drag").page, paginator.layout_cache.borrow().misses)
        });
        let before = renders[warmed.last_child].get();
        assert!(before > 0, "the neighbor's final row was actually laid out");
        for _ in 0..8 {
            visual.simulate_event(scroll(-5.0, TouchPhase::Moved));
            visual.run_until_parked();
        }
        assert_eq!(renders[warmed.last_child].get(), before, "warming must not rebuild offscreen rows every frame");
        visual.update(|_, cx| {
            let paginator = paginator.read(cx);
            assert_eq!(paginator.layout_cache.borrow().misses, misses, "translation must reuse page packing");
        });
        // Read synchronously on release, before another frame can perform
        // layout. The warmed page must already be known and ready to animate.
        visual.update(|window, cx| {
            paginator.update(cx, |paginator, cx| {
                let release = ScrollWheelEvent { delta: ScrollDelta::Pages(point(-1.0, 0.0)), touch_phase: TouchPhase::Ended, ..Default::default() };
                paginator.cursor = Some(0);
                paginator.handle_swipe(&release, window, cx);
                assert!(paginator.cursor.is_none(), "a prepared page turn still clears an off-page keyboard cursor");
                assert_eq!(paginator.visible_page, Some(warmed));
                assert!(paginator.transition.as_ref().unwrap().started_at.is_some());
                assert!(paginator.transition.as_ref().unwrap().touch_duration.unwrap() <= std::time::Duration::from_millis(120));
            });
        });
    }

    #[gpui::test]
    fn swipe_moves_before_release_and_a_new_drag_interrupts_the_settle(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase, div, point};

        cx.update(gpui_component::init);
        let (paginator, visual) = cx.add_window_view(|_, _| Paginator::new("live-swipe", []));
        visual.update(|window, cx| {
            paginator.update(cx, |paginator, cx| {
                paginator.data = Rc::new(PaginatorData { indices: Default::default(), children: (0..12).map(|_| PaginatorChild::new(|_, _, _| div().into_any_element())).collect(), groups: Vec::new() });
                paginator.visible_page = Some(VisiblePage { first_child: 0, last_child: 3 });
                let scroll = |dy, phase| ScrollWheelEvent { delta: ScrollDelta::Pixels(point(px(0.0), px(dy))), touch_phase: phase, ..Default::default() };
                paginator.handle_swipe(&scroll(-20.0, TouchPhase::Started), window, cx);
                paginator.handle_swipe(&scroll(-140.0, TouchPhase::Moved), window, cx);
                assert_eq!(paginator.child_offset, 0);
                assert_eq!(paginator.swipe_preview.offset(window, cx), point(px(0.0), px(-160.0)));
                let release = ScrollWheelEvent { delta: ScrollDelta::Pages(point(0.0, -1.0)), touch_phase: TouchPhase::Ended, ..Default::default() };
                paginator.handle_swipe(&release, window, cx);
                assert_eq!(paginator.child_offset, 4);
                assert_eq!(paginator.transition.as_ref().unwrap().drag_offset, point(px(0.0), px(-160.0)));
                // The incoming page has laid out, but the previous animation
                // has not finished. Another swipe should still be accepted.
                paginator.visible_page = Some(VisiblePage { first_child: 4, last_child: 7 });
                paginator.handle_swipe(&scroll(-80.0, TouchPhase::Started), window, cx);
                assert!(paginator.transition.is_none());
                paginator.handle_swipe(&release, window, cx);
                assert_eq!(paginator.child_offset, 8);
            });
        });
    }

    #[gpui::test]
    fn touch_releases_keep_thresholds_axis_locking_and_cancellation_separate_from_hardware(cx: &mut gpui::TestAppContext) {
        use super::PaginatorAxis;
        use gpui::{IntoElement, ScrollDelta, ScrollWheelEvent, TouchPhase, div, point};

        cx.update(gpui_component::init);
        let (paginator, visual) = cx.add_window_view(|_, _| Paginator::new("gesture-releases", []).horizontal_only());
        visual.update(|window, cx| {
            paginator.update(cx, |paginator, cx| {
                paginator.data = Rc::new(PaginatorData { indices: Default::default(), children: (0..12).map(|_| PaginatorChild::new(|_, _, _| div().into_any_element())).collect(), groups: Vec::new() });
                paginator.visible_page = Some(VisiblePage { first_child: 0, last_child: 3 });
                paginator.animate_page_transitions = true;
                let scroll = |x, y, touch_phase| ScrollWheelEvent { delta: ScrollDelta::Pixels(point(px(x), px(y))), touch_phase, ..Default::default() };
                paginator.handle_swipe(&scroll(-200.0, 0.0, TouchPhase::Moved), window, cx);
                assert!(paginator.swipe.is_none(), "movement without a press cannot start a drag");
                paginator.handle_swipe(&scroll(-20.0, -20.0, TouchPhase::Started), window, cx);
                assert!(paginator.swipe.as_ref().unwrap().axis.is_none(), "diagonal travel must wait for an axis");
                paginator.handle_swipe(&scroll(0.0, 0.0, TouchPhase::Ended), window, cx);
                assert_eq!(paginator.child_offset, 0);

                paginator.handle_swipe(&scroll(-20.0, 0.0, TouchPhase::Started), window, cx);
                paginator.handle_swipe(&scroll(-10.0, -150.0, TouchPhase::Moved), window, cx);
                assert_eq!(paginator.swipe.as_ref().unwrap().axis, Some(PaginatorAxis::Horizontal));
                paginator.handle_swipe(&scroll(0.0, 0.0, TouchPhase::Ended), window, cx);
                assert_eq!(paginator.child_offset, 0, "a short horizontal drag must not become a vertical page turn");
                assert!(paginator.transition.is_none());
                assert!(paginator.swipe.is_none());

                paginator.handle_swipe(&scroll(-200.0, 0.0, TouchPhase::Started), window, cx);
                paginator.handle_swipe(&scroll(0.0, 0.0, TouchPhase::Cancelled), window, cx);
                assert_eq!(paginator.child_offset, 0, "cancelling a long drag must not turn the page");
                assert!(paginator.swipe.is_none());

                paginator.handle_swipe(&scroll(-200.0, 0.0, TouchPhase::Started), window, cx);
                paginator.handle_swipe(&scroll(0.0, 0.0, TouchPhase::Ended), window, cx);
                assert_eq!(paginator.child_offset, 4);
                assert!(paginator.transition.as_ref().unwrap().touch_duration.is_some());

                paginator.child_offset = 0;
                paginator.transition = None;
                paginator.visible_page = Some(VisiblePage { first_child: 0, last_child: 3 });
                let hardware = ScrollWheelEvent { delta: ScrollDelta::Pages(point(-1.0, 0.0)), touch_phase: TouchPhase::Ended, ..Default::default() };
                paginator.handle_swipe(&hardware, window, cx);
                assert_eq!(paginator.child_offset, 4);
                assert!(paginator.transition.as_ref().unwrap().touch_duration.is_none(), "hardware requests keep the normal page animation");
            });
        });
    }

    #[gpui::test]
    fn hardware_page_request_does_not_require_a_touch_start(cx: &mut gpui::TestAppContext) {
        use gpui::{IntoElement, ScrollDelta, ScrollWheelEvent, TouchPhase, div, point};

        cx.update(gpui_component::init);
        let (paginator, visual) = cx.add_window_view(|_, _| Paginator::new("hardware-pages", []));
        visual.update(|window, cx| {
            paginator.update(cx, |paginator, cx| {
                paginator.data = Rc::new(PaginatorData { indices: Default::default(), children: (0..12).map(|_| PaginatorChild::new(|_, _, _| div().into_any_element())).collect(), groups: Vec::new() });
                for delta in [point(0.0, -1.0), point(-1.0, 0.0)] {
                    paginator.child_offset = 0;
                    paginator.transition = None;
                    paginator.visible_page = Some(VisiblePage { first_child: 0, last_child: 3 });
                    let event = ScrollWheelEvent { delta: ScrollDelta::Pages(delta), touch_phase: TouchPhase::Ended, ..Default::default() };
                    paginator.handle_swipe(&event, window, cx);
                    assert_eq!(paginator.child_offset, 4);
                    assert!(paginator.swipe.is_none());
                }
            });
        });
    }

    #[gpui::test]
    fn layout_replaces_measurement_snapshots_when_typography_changes(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        use gpui::{AvailableSpace, Point, size};
        use ui_components::{HierarchyItemWidth, NavigationIcon};

        cx.update(|cx| gpui_component::init(cx));
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(40.0)), px(0.0)).intrinsic();
        let child = PaginatorChild::new(|_, _, _| gpui::div().child("A folder").into_any_element()).with_intrinsic_width(HierarchyItemWidth { icon: NavigationIcon::Folder, label: "A folder".into(), count: Some(12), show_download: false });
        let (paginator, cx) = cx.add_window_view(|window, _| {
            window.set_rem_size(px(16.0));
            Paginator::new("measurement-owner", [PaginatorGroup::new(policy, [child])])
        });
        let space = size(AvailableSpace::Definite(px(500.0)), AvailableSpace::Definite(px(200.0)));
        cx.draw(Point::default(), space, |_, _| paginator.clone().into_element());
        let original = cx.update(|_, cx| paginator.read(cx).measurements.as_ref().unwrap().widths.clone());
        let original_width = original[0];
        cx.update(|window, _| window.set_rem_size(px(32.0)));
        cx.draw(Point::default(), space, |_, _| paginator.clone().into_element());
        let resized = cx.update(|_, cx| paginator.read(cx).measurements.as_ref().unwrap().widths.clone());
        assert!(resized[0] > original_width);
        assert_eq!(original[0], original_width);
        assert!(!Rc::ptr_eq(&original, &resized));
    }
}
