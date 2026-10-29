mod session;
mod ruler;
pub use ruler::ReadingRulerStyle;

use fixed_page::{DISPLAY_CACHE_BUDGET, PageCache, PageImage, PageSwipe, PageZoom};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Axis, Bounds, ClipboardItem, ContentMask, Context, Corners, Element, ElementId, EventEmitter, FocusHandle, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, PinchEvent, Pixels, Point,
    Render, ScrollDelta, ScrollHandle, ScrollWheelEvent, Style, Task, TouchPhase, Window, div, fill, point, px, rgba, size,
};
use gpui_component::ElementExt;
use gpui_image_eviction::track_image;
use image::RgbaImage;
use pdf_reader_core::{
    DISPLAY_TILE_HEIGHT, FitWidthAnchor, FitWidthAnchorPager, FitWidthColumn, FitWidthPage, FitWidthPlan, FitWidthViewport, NormalizedRect, PageRenderKey, PdfDocumentInfo, PdfDocumentSession, PdfPageGeometry, PdfPageTextLayout, PdfResult,
    PdfSearchHit, PdfTextSelection, PixelCrop, ReaderInteractionStyle, SelectionGranularity, SelectionOptions,
};
use smallvec::SmallVec;
use std::cell::Cell;
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use ui_components::{self as components, PageTurn};
use web_time::Instant;

// PDF raster cost grows with area. A 64px bucket keeps resize cache reuse
// without routinely producing far more pixels than the viewport can display.
const RENDER_WIDTH_BUCKET: f32 = 64.0;
const SEARCH_PAGE_CHUNK: usize = 1;
const LINE_SCROLL_HEIGHT: f32 = 16.0;
const TEXT_HIT_TOLERANCE_PX: f32 = 8.0;
const MIN_ZOOM: f32 = 0.05;
/// US Letter, used only until the real page size is known.
const DEFAULT_PAGE_WIDTH_POINTS: f32 = 612.0;
const DEFAULT_PAGE_HEIGHT_POINTS: f32 = 792.0;
const FIT_WIDTH_SURFACE_PADDING: f32 = 0.0;
const WHEEL_SCROLL_END_DELAY: Duration = Duration::from_millis(150);
const SMOOTH_SCROLL_DURATION: Duration = Duration::from_millis(120);
const DEFAULT_AVAILABLE_WIDTH: f32 = 948.0;
const DEFAULT_AVAILABLE_HEIGHT: f32 = 800.0;
pub const MIN_VISIBLE_PAGE_COUNT: usize = 1;
pub const MAX_VISIBLE_PAGE_COUNT: usize = 6;

/// How large a page is drawn.
///
/// All three modes settle the same quantity — `zoom`, in pixels per PDF point —
/// and differ only in which end is held fixed. Because margin trimming and
/// mixed page sizes change a page's effective width, the page count these
/// derive is a property of the page being read, not of the document.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PdfZoomMode {
    /// You choose how many pages sit side by side; the zoom follows.
    FitWidth { columns: usize },
    /// You choose the zoom; the number of pages that fit follows.
    Fixed { zoom: f32 },
    /// A whole page's height fills the viewport; the zoom, and then the number
    /// of pages that fit, follow.
    FitHeight,
}

impl Default for PdfZoomMode {
    fn default() -> Self {
        Self::FitWidth { columns: 1 }
    }
}

/// A [`PdfZoomMode`] resolved against the viewport and the page being read.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ResolvedLayout {
    columns: usize,
    column_width: f32,
    zoom: f32,
}

/// A reading position in the original, untrimmed PDF. Display-column
/// coordinates are converted explicitly before updating it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfSourcePosition {
    page_index: usize,
    page_y: f32,
}

impl PdfSourcePosition {
    fn new(page_index: usize, page_y: f32) -> Self {
        Self { page_index, page_y: page_y.clamp(0.0, 1.0) }
    }

    pub fn page_index(self) -> usize {
        self.page_index
    }

    pub fn page_y(self) -> f32 {
        self.page_y
    }
}

#[derive(Clone, Debug)]
pub enum PdfViewEvent {
    /// An exact document position, including movement inside one PDF page.
    PositionChanged {
        position: PdfSourcePosition,
        total: usize,
    },
    SearchChanged {
        current: usize,
        total: usize,
    },
    AnnotationActivated {
        id: String,
        position: Point<Pixels>,
    },
    PageTapped,
    LoadFailed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfAnnotationStyle {
    Highlight,
    Underline,
    Squiggly,
    Strikethrough,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfAnnotationOverlay {
    pub id: String,
    pub page_index: usize,
    /// Untrimmed, normalized PDF page coordinates.
    pub rects: Vec<NormalizedRect>,
    pub style: PdfAnnotationStyle,
    /// Packed as `0xRRGGBBAA`.
    pub color: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfSelectionSnapshot {
    pub page_index: usize,
    pub text: String,
    /// Untrimmed, normalized PDF page coordinates.
    pub rects: Vec<NormalizedRect>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PageGeometryKey {
    page_index: usize,
    trim_margins: bool,
}

struct NavigationOffsets {
    column_height: f32,
    maximum_offset: f32,
    offsets: Arc<[f32]>,
}

struct PdfScrollAnimation {
    distance: f32,
    applied: f32,
    started_at: Instant,
    /// Line navigation settles at its original source anchor, preserving the
    /// same text boundary even when its distance reaches a viewport edge.
    target_anchor: Option<PdfSourcePosition>,
}

impl PdfScrollAnimation {
    fn remaining(&self) -> f32 {
        self.distance - self.applied
    }

    fn step(&mut self, finish: bool) -> (f32, bool) {
        let progress = if finish { 1.0 } else { (self.started_at.elapsed().as_secs_f32() / SMOOTH_SCROLL_DURATION.as_secs_f32()).clamp(0.0, 1.0) };
        let applied = self.distance * gpui::touch_page_settle_easing(progress);
        let delta = applied - self.applied;
        self.applied = applied;
        (delta, progress >= 1.0)
    }
}

pub struct PdfView {
    session: Option<session::Session>,
    info: Option<PdfDocumentInfo>,
    pages: Vec<(usize, Arc<DisplayPage>)>,
    display_cache: DisplayPageCache,
    paper_color: [u8; 3],
    ink_color: [u8; 3],
    /// The only durable reading position: untrimmed PDF page coordinates.
    source_position: PdfSourcePosition,
    page_geometry: HashMap<PageGeometryKey, PdfPageGeometry>,
    geometry_loading: bool,
    prefetched_pages: Vec<usize>,
    prefetch_task: Option<Task<()>>,
    fit_width_plan: FitWidthPlan,
    /// Viewport-local fit-width pagination. The plan above is always the
    /// pager's current window, whose display index zero is `start_anchor`.
    viewport_pager: Option<FitWidthAnchorPager>,
    plan_key: Option<(f32, f32, usize, bool, bool, u64)>,
    geometry_revision: u64,
    navigation_offsets: Option<NavigationOffsets>,
    /// Filled by the page elements as they paint; see [`Self::passage_bounds`].
    passage_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    navigation_revision: u64,
    /// A direct jump can reveal a page excluded from ordinary reading flow.
    revealed_page: Option<usize>,
    zoom_mode: PdfZoomMode,
    right_to_left: bool,
    /// Device pixels per logical pixel, sampled from the window each frame so
    /// pages are rastered at the resolution they are actually painted at.
    device_pixel_ratio: f32,
    available_width: f32,
    available_height: f32,
    page_gap: f32,
    trim_margins: bool,
    load_generation: u64,
    render_generation: u64,
    pending_render: Option<(u64, Vec<PageRenderKey>)>,
    search_generation: u64,
    search_in_flight: bool,
    pending_search: Option<(u64, String)>,
    rendering: bool,
    error: Option<String>,
    selection_anchor: Option<(f32, f32)>,
    selection_anchor_source_index: Option<usize>,
    selection_active: Option<(f32, f32)>,
    tap_start: Option<(Point<Pixels>, Instant)>,
    selection_page: Option<usize>,
    selection_page_column: Option<u32>,
    selection_granularity: SelectionGranularity,
    selection: Option<Arc<PdfTextSelection>>,
    annotations: Arc<[PdfAnnotationOverlay]>,
    reading_highlight: Option<(usize, Arc<[NormalizedRect]>)>,
    ruler: ruler::ReadingRuler,
    active_annotation_id: Option<Arc<str>>,
    search_hits: Vec<PdfSearchHit>,
    search_page_hits: HashMap<usize, PageSearchHits>,
    active_search_hit: Option<usize>,
    pending_search_scroll: Option<(usize, NormalizedRect)>,
    interaction_style: ReaderInteractionStyle,
    /// Derived whole-page presentation offset in fit-height mode. Anchored
    /// fit-width layouts always begin at zero and retain source_position.
    flow_offset: f32,
    wheel_delta: f32,
    wheel_debounce: Option<Task<()>>,
    touchpad: components::TouchpadNavigation,
    /// How far the finger has travelled on both axes, while a touch swipe is
    /// in progress.
    touch_swipe: PageSwipe,
    swipe_preview: components::SwipePreview,
    page_turn: Option<PageTurn<PdfFlowSnapshot>>,
    scroll_animation: Option<PdfScrollAnimation>,
    scroll_handle: ScrollHandle,
    focus: FocusHandle,
}

struct DisplayPage {
    raster: PageImage,
    text_layout: Arc<PdfPageTextLayout>,
    ruler_lines: Arc<[ruler::TextLine]>,
    raster_crop: PixelCrop,
    page_crop: NormalizedRect,
}

type DisplayPageCache = PageCache<PageRenderKey, DisplayPage>;

#[derive(Clone)]
struct PageSearchHits {
    first_index: usize,
    hits: Arc<[PdfSearchHit]>,
}

#[derive(Clone)]
struct PdfPageElement {
    page_index: usize,
    column: FitWidthColumn,
    page: Arc<DisplayPage>,
    tiles: Vec<Arc<DisplayPage>>,
    selection: Option<Arc<PdfTextSelection>>,
    annotations: Arc<[PdfAnnotationOverlay]>,
    reading_highlight: Option<Arc<[NormalizedRect]>>,
    active_annotation_id: Option<Arc<str>>,
    search_hits: Option<PageSearchHits>,
    active_search_hit: Option<usize>,
    interaction_style: ReaderInteractionStyle,
    instance_index: usize,
    /// Where the passage the reader is annotating ended up on screen, unioned
    /// across every page column that drew part of it. Page rects only become
    /// window rects during paint, so this is recorded there and read by the
    /// note panel, which has to be placed clear of the passage rather than over
    /// it. See [`PdfView::passage_bounds`].
    passage_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

struct PdfFlowSnapshot {
    ruler: Vec<(Bounds<Pixels>, Hsla)>,
    page_gap: f32,
    column_height: f32,
    remainder: f32,
    fit_height: bool,
    right_to_left: bool,
    slots: Vec<PdfFlowSlot>,
}

struct PdfFlowSlot {
    width: f32,
    streams: Vec<Vec<PdfFlowFragment>>,
}

struct PdfFlowFragment {
    column: FitWidthColumn,
    page: Option<PdfPageElement>,
}

impl PdfFlowSnapshot {
    fn is_ready(&self) -> bool {
        let mut fragments = self.slots.iter().flat_map(|slot| &slot.streams).flatten().peekable();
        fragments.peek().is_some() && fragments.all(|fragment| fragment.page.as_ref().is_some_and(PdfPageElement::raster_is_ready))
    }

    fn element(&self, offset: Point<Pixels>, paper_color: Hsla) -> gpui::Div {
        let mut columns = SmallVec::<[AnyElement; MAX_VISIBLE_PAGE_COUNT]>::new();
        columns.push(div().flex_1().h_full().into_any_element());
        for (slot_index, slot) in self.slots.iter().enumerate() {
            let streams = slot.streams.iter().enumerate().map(|(stream_index, stream)| {
                let mut fragments = SmallVec::<[AnyElement; 4]>::new();
                for fragment in stream {
                    if fragment.column.leading_page_gap() > 0.0 {
                        fragments.push(div().h(px(fragment.column.leading_page_gap())).flex_shrink_0().bg(paper_color).into_any_element());
                    }
                    fragments
                        .push(fragment.page.as_ref().map(|page| page.clone().into_any_element()).unwrap_or_else(|| div().w(px(fragment.column.display_width())).h(px(fragment.column.display_height())).flex_shrink_0().into_any_element()));
                }
                div()
                    .id(("pdf-logical-column", slot_index * 2 + stream_index))
                    .w(px(slot.width))
                    .h(px(self.column_height))
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .when(self.fit_height, |column| column.items_center().justify_center())
                    .children(fragments)
            });
            columns.push(
                div().id(("pdf-flow-slot", slot_index)).w(px(slot.width)).h(px(self.column_height)).overflow_hidden().flex_shrink_0().child(div().w_full().flex().flex_col().mt(px(-self.remainder)).children(streams)).into_any_element(),
            );
            if slot_index + 1 < self.slots.len() {
                columns.push(div().w(px(self.page_gap)).h_full().flex_shrink_0().bg(paper_color).into_any_element());
            }
        }
        columns.push(div().flex_1().h_full().into_any_element());
        div().relative().w_full().h(px(self.column_height)).flex().flex_row().when(self.right_to_left, |row| row.flex_row_reverse()).items_start().flex_shrink_0().transform(gpui::ElementTransform::default().translate(offset)).children(columns).child(ruler::overlay(&self.ruler))
    }
}

struct PdfPagePrepaint {
    column_bounds: Bounds<Pixels>,
    image_bounds: Vec<Bounds<Pixels>>,
}

#[derive(Clone, Copy)]
struct PdfInteractionTarget {
    page_index: usize,
    column: FitWidthColumn,
    page_point: (f32, f32),
    page_display_size: (f32, f32),
}

impl EventEmitter<PdfViewEvent> for PdfView {}

impl PdfView {
    pub fn from_bytes(source_name: String, bytes: Vec<u8>, initial_page: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::empty(initial_page, window, cx);
        view.start_load(move || PdfDocumentSession::from_bytes(source_name, bytes), cx);
        view
    }

    pub fn from_reader<R>(source_name: String, reader: R, initial_page: usize, window: &mut Window, cx: &mut Context<Self>) -> Self
    where
        R: Read + Seek + Send + 'static,
    {
        let mut view = Self::empty(initial_page, window, cx);
        view.start_load(move || PdfDocumentSession::from_reader(source_name, reader), cx);
        view
    }

    /// Metadata has already been matched to the opened remote revision by the backend.
    pub fn from_reader_with_metadata<R>(source_name: String, reader: R, metadata: Option<pdf_reader_core::PdfReaderMetadata>, initial_page: usize, window: &mut Window, cx: &mut Context<Self>) -> Self
    where
        R: Read + Seek + Send + 'static,
    {
        Self::from_reader_with_page_source(source_name, reader, metadata, None, initial_page, window, cx)
    }

    pub fn from_reader_with_page_source<R>(
        source_name: String, reader: R, metadata: Option<pdf_reader_core::PdfReaderMetadata>, page_source: Option<std::sync::Arc<dyn pdf_reader_core::PdfPageSource>>, initial_page: usize, window: &mut Window, cx: &mut Context<Self>,
    ) -> Self
    where
        R: Read + Seek + Send + 'static,
    {
        let mut view = Self::empty(initial_page, window, cx);
        view.start_load(
            move || {
                let content_hash = metadata.as_ref().map(|m| m.content_hash.clone()).unwrap_or_default();
                PdfDocumentSession::from_reader_with_opening_page(source_name, reader, &content_hash, metadata, page_source, Some(initial_page))
            },
            cx,
        );
        view
    }

    fn empty(initial_page: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            session: None,
            info: None,
            pages: Vec::new(),
            display_cache: DisplayPageCache::new(DISPLAY_CACHE_BUDGET),
            paper_color: [255; 3],
            ink_color: [0; 3],
            source_position: PdfSourcePosition::new(initial_page, 0.0),
            page_geometry: HashMap::new(),
            geometry_loading: false,
            prefetched_pages: Vec::new(),
            prefetch_task: None,
            fit_width_plan: FitWidthPlan::default(),
            viewport_pager: None,
            plan_key: None,
            geometry_revision: 0,
            navigation_offsets: None,
            passage_bounds: Rc::new(Cell::new(None)),
            navigation_revision: 0,
            revealed_page: None,
            zoom_mode: PdfZoomMode::default(),
            right_to_left: false,
            device_pixel_ratio: 1.0,
            available_width: DEFAULT_AVAILABLE_WIDTH,
            available_height: DEFAULT_AVAILABLE_HEIGHT,
            page_gap: 0.0,
            trim_margins: true,
            load_generation: 1,
            render_generation: 0,
            pending_render: None,
            search_generation: 0,
            search_in_flight: false,
            pending_search: None,
            rendering: false,
            error: None,
            selection_anchor: None,
            selection_anchor_source_index: None,
            selection_active: None,
            tap_start: None,
            selection_page: None,
            selection_page_column: None,
            selection_granularity: SelectionGranularity::Character,
            selection: None,
            annotations: Arc::from([]),
            reading_highlight: None,
            ruler: ruler::ReadingRuler::default(),
            active_annotation_id: None,
            search_hits: Vec::new(),
            search_page_hits: HashMap::new(),
            active_search_hit: None,
            pending_search_scroll: None,
            interaction_style: ReaderInteractionStyle::default(),
            flow_offset: 0.0,
            wheel_delta: 0.0,
            wheel_debounce: None,
            touchpad: components::TouchpadNavigation::default(),
            touch_swipe: PageSwipe::default(),
            swipe_preview: components::SwipePreview::default(),
            page_turn: None,
            scroll_animation: None,
            scroll_handle: ScrollHandle::new(),
            focus,
        }
    }

    pub fn info(&self) -> Option<&PdfDocumentInfo> {
        self.info.as_ref()
    }

    pub fn current_page(&self) -> usize {
        self.source_position.page_index()
    }
    pub fn navigation_revision(&self) -> u64 {
        self.navigation_revision
    }

    pub fn page_count(&self) -> usize {
        self.info.as_ref().map(PdfDocumentInfo::page_count).unwrap_or(0)
    }

    /// How many pages currently sit side by side. Chosen directly in
    /// [`PdfZoomMode::FitWidth`] and derived from the zoom otherwise.
    pub fn visible_page_count(&self) -> usize {
        self.resolve_layout().columns
    }

    pub fn set_visible_page_count(&mut self, count: usize, cx: &mut Context<Self>) {
        self.set_zoom_mode(PdfZoomMode::FitWidth { columns: count.clamp(MIN_VISIBLE_PAGE_COUNT, MAX_VISIBLE_PAGE_COUNT) }, cx);
    }

    pub fn set_available_width(&mut self, available_width: f32, cx: &mut Context<Self>) {
        self.set_available_size(available_width, self.available_height, cx);
    }

    fn set_available_size(&mut self, available_width: f32, available_height: f32, cx: &mut Context<Self>) {
        let width_changed = available_width.is_finite() && available_width > 0.0 && (self.available_width - available_width).abs() >= 1.0;
        let height_changed = available_height.is_finite() && available_height > 0.0 && (self.available_height - available_height).abs() >= 1.0;
        if !width_changed && !height_changed {
            return;
        }
        self.scroll_animation = None;
        self.page_turn = None;
        self.swipe_preview.clear();
        let page_position = self.current_page_position();
        if width_changed {
            self.available_width = available_width;
        }
        if height_changed {
            self.available_height = available_height;
        }
        self.rebuild_fit_width_plan();
        self.restore_page_position(page_position);
        self.preserve_reading_ruler(cx);
        self.request_current_pages(cx);
        cx.notify();
    }

    /// Space between side-by-side display columns, in logical pixels.
    pub fn set_page_gap(&mut self, page_gap: f32, cx: &mut Context<Self>) {
        if !page_gap.is_finite() || self.page_gap == page_gap.max(0.0) {
            return;
        }
        self.page_gap = page_gap.max(0.0);
        self.scroll_animation = None;
        self.page_turn = None;
        self.swipe_preview.clear();
        let position = self.current_page_position();
        self.plan_key = None;
        self.rebuild_fit_width_plan();
        self.restore_page_position(position);
        self.preserve_reading_ruler(cx);
        self.request_current_pages(cx);
        cx.notify();
    }

    pub fn trim_margins(&self) -> bool {
        self.trim_margins
    }

    pub fn set_trim_margins(&mut self, trim_margins: bool, cx: &mut Context<Self>) {
        self.scroll_animation = None;
        self.page_turn = None;
        self.swipe_preview.clear();
        if self.trim_margins == trim_margins {
            return;
        }
        self.trim_margins = trim_margins;
        self.rebuild_fit_width_plan();
        self.preserve_reading_ruler(cx);
        self.clear_selection();
        self.request_current_pages(cx);
        cx.notify();
    }

    pub fn can_previous_page(&self) -> bool {
        self.viewport_pager.as_ref().map_or(self.flow_offset > 0.5, FitWidthAnchorPager::can_backward)
    }

    pub fn can_next_page(&self) -> bool {
        self.viewport_pager.as_ref().map_or(self.flow_offset + 0.5 < self.max_flow_offset(), FitWidthAnchorPager::can_forward)
    }

    pub fn visible_page_range(&self) -> Option<(usize, usize)> {
        let columns = self.visible_columns();
        Some((columns.first()?.page_index(), columns.last()?.page_index()))
    }

    pub fn full_page_position(&self) -> f32 {
        self.current_page_position()
    }

    pub fn set_full_page_position(&mut self, full_page_position: f32, cx: &mut Context<Self>) {
        self.pending_search_scroll = None;
        self.scroll_animation = None;
        self.page_turn = None;
        self.swipe_preview.clear();
        if let Some(pager) = &mut self.viewport_pager {
            pager.clear_history();
        }
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        self.restore_page_position(full_page_position);
        self.request_current_pages(cx);
        cx.notify();
    }

    pub fn next_page(&mut self, cx: &mut Context<Self>) {
        self.turn_page(true, cx);
    }

    pub fn previous_page(&mut self, cx: &mut Context<Self>) {
        self.turn_page(false, cx);
    }

    pub fn right_to_left(&self) -> bool {
        self.right_to_left
    }

    /// `towards_right` describes a physical control, such as the right arrow.
    /// Next/previous-page commands themselves retain document order.
    pub fn horizontal_page(&mut self, towards_right: bool, cx: &mut Context<Self>) {
        self.turn_page(towards_right != self.right_to_left, cx);
    }

    fn turn_page(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.pending_search_scroll = None;
        self.finish_scroll_animation(cx);
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        let before = (self.source_position, self.flow_offset);
        let outgoing = PageTurn::<PdfFlowSnapshot>::enabled(cx).then(|| self.flow_snapshot(true)).filter(PdfFlowSnapshot::is_ready);
        if self.viewport_pager.is_some() {
            self.move_viewport_anchor(forward, cx);
        } else {
            let target = self.keyboard_page_target(forward);
            self.clear_selection();
            self.set_flow_offset(target, cx);
        }
        if before != (self.source_position, self.flow_offset) {
            let drag = self.swipe_preview.take_offset();
            let viewport = size(px(self.available_width), px(self.flow_column_height()));
            let axis = if self.zoom_mode == PdfZoomMode::FitHeight && drag.x.abs() >= drag.y.abs() { Axis::Horizontal } else { Axis::Vertical };
            let physical_forward = if axis == Axis::Horizontal && self.right_to_left { !forward } else { forward };
            self.page_turn = outgoing.and_then(|outgoing| PageTurn::new_on_axis(outgoing, physical_forward, drag, viewport, axis, cx));
        } else {
            self.swipe_preview.settle();
        }
        self.ruler_page_turned(forward, before != (self.source_position, self.flow_offset));
        cx.notify();
    }

    fn keyboard_page_target(&mut self, forward: bool) -> f32 {
        let stride = self.flow_column_height();
        let unsnapped = if forward { self.flow_offset + stride } else { self.flow_offset - stride };
        if self.zoom_mode == PdfZoomMode::FitHeight {
            return unsnapped;
        }
        let offsets = self.atomic_flow_offsets();
        if forward { forward_page_snap_offset(self.flow_offset, unsnapped, &offsets).unwrap_or(unsnapped) } else { backward_page_snap_offset(self.flow_offset, unsnapped, &offsets).unwrap_or(unsnapped) }
    }

    pub fn next_item(&mut self, cx: &mut Context<Self>) {
        self.pending_search_scroll = None;
        if self.zoom_mode == PdfZoomMode::FitHeight {
            self.next_page(cx);
            return;
        }
        self.page_turn = None;
        self.swipe_preview.clear();
        self.finish_scroll_animation(cx);
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        if let Some(pager) = &self.viewport_pager {
            if let Some(target) = pager.adjacent_boundary(true) {
                self.animate_anchor_scroll(target, cx);
            } else {
                self.animate_flow_scroll(-LINE_SCROLL_HEIGHT, cx);
            }
            self.clear_selection();
            return;
        }
        let offsets = self.atomic_flow_offsets();
        let Some(target) = next_snap_offset(self.flow_offset, &offsets) else {
            return;
        };
        self.clear_selection();
        self.animate_item_scroll(target, cx);
    }

    pub fn previous_item(&mut self, cx: &mut Context<Self>) {
        self.pending_search_scroll = None;
        if self.zoom_mode == PdfZoomMode::FitHeight {
            self.previous_page(cx);
            return;
        }
        self.page_turn = None;
        self.swipe_preview.clear();
        self.finish_scroll_animation(cx);
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        if let Some(pager) = &self.viewport_pager {
            if let Some(target) = pager.adjacent_boundary(false) {
                self.animate_anchor_scroll(target, cx);
            } else {
                self.animate_flow_scroll(LINE_SCROLL_HEIGHT, cx);
            }
            self.clear_selection();
            return;
        }
        let offsets = self.atomic_flow_offsets();
        let Some(target) = previous_snap_offset(self.flow_offset, &offsets) else {
            return;
        };
        self.clear_selection();
        self.animate_item_scroll(target, cx);
    }

    pub fn set_page(&mut self, page_index: usize, cx: &mut Context<Self>) {
        self.pending_search_scroll = None;
        self.scroll_animation = None;
        self.page_turn = None;
        self.swipe_preview.clear();
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        if page_index >= self.page_count() || (page_index == self.current_page() && self.current_page_position() <= self.geometry_crop(page_index).top() + f32::EPSILON) {
            return;
        }
        self.reveal_skipped_page(page_index);
        if self.seek_viewport_anchor(FitWidthAnchor::new(page_index, 0.0), false, cx) {
            self.clear_selection();
            return;
        }
        self.source_position = PdfSourcePosition::new(page_index, 0.0);
        self.reset_page_scroll();
        self.clear_selection();
        self.request_current_pages(cx);
    }

    /// The width every page is rastered at, in device pixels.
    ///
    /// A column is painted `page_display_width()` wide, so rastering at a fixed
    /// width both blurs wide columns and wastes most of the buffer on narrow
    /// ones — six columns of a spread are a fraction of the width of one, but
    /// used to cost the same memory and render time per page. The result is
    /// rounded up to a bucket so that dragging a window edge or stepping the
    /// zoom keeps hitting the same cached pages.
    fn render_target_width(&self, page_index: usize) -> u16 {
        let crop_width = self.geometry_crop(page_index).width();
        let device_width = self.page_display_width() * self.device_pixel_ratio.max(1.0) / crop_width;
        let bucketed = (device_width / RENDER_WIDTH_BUCKET).ceil() * RENDER_WIDTH_BUCKET;
        let columns = (self.visible_page_count() + 1) as f32;
        let device_height = self.page_display_height() * self.device_pixel_ratio.max(1.0);
        let maximum_width = DISPLAY_CACHE_BUDGET as f32 / 4.0 / columns / (device_height + 2.0 * DISPLAY_TILE_HEIGHT as f32) / crop_width;
        PageRenderKey::new(0, bucketed.min(maximum_width).clamp(0.0, u16::MAX as f32) as u16).target_width()
    }

    pub fn zoom_mode(&self) -> PdfZoomMode {
        self.zoom_mode
    }

    /// The zoom every mode resolves to, in pixels per PDF point.
    pub fn zoom(&self) -> f32 {
        self.resolve_layout().zoom
    }

    pub fn set_zoom_mode(&mut self, zoom_mode: PdfZoomMode, cx: &mut Context<Self>) {
        self.scroll_animation = None;
        self.page_turn = None;
        self.swipe_preview.clear();
        if self.zoom_mode == zoom_mode {
            return;
        }
        let page_position = self.current_page_position();
        self.wheel_debounce.take();
        self.wheel_delta = 0.0;
        self.touch_swipe.cancel();
        self.zoom_mode = zoom_mode;
        self.rebuild_fit_width_plan();
        self.restore_page_position(page_position);
        self.preserve_reading_ruler(cx);
        self.clear_selection();
        self.request_current_pages(cx);
        cx.notify();
    }

    /// Switches to a fixed zoom, letting the page count fall out of it.
    pub fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(MIN_ZOOM, self.width_fit_zoom(MIN_VISIBLE_PAGE_COUNT));
        self.set_zoom_mode(PdfZoomMode::Fixed { zoom }, cx);
    }

    pub fn change_zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        if self.zoom_mode == PdfZoomMode::FitHeight {
            return;
        }
        self.set_zoom(self.zoom() + delta, cx);
    }

    fn handle_pinch(&mut self, event: &PinchEvent, cx: &mut Context<Self>) {
        self.wheel_debounce.take();
        self.wheel_delta = 0.0;
        self.touch_swipe.cancel();
        self.touchpad.consume();
        self.swipe_preview.settle();
        let factor = 1.0 + event.delta;
        if event.phase != TouchPhase::Cancelled && factor.is_finite() && factor > 0.0 && factor != 1.0 {
            self.set_zoom(self.zoom() * factor, cx);
        }
    }

    fn handle_touchpad_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        self.wheel_debounce.take();
        self.wheel_delta = 0.0;
        if let Some(travel) = self.touchpad.update(event) {
            let forward = if travel.x != 0.0 { (travel.x < 0.0) != self.right_to_left } else { travel.y < 0.0 };
            if forward {
                self.next_page(cx);
            } else {
                self.previous_page(cx);
            }
        }
    }

    pub fn set_interaction_style(&mut self, style: ReaderInteractionStyle, cx: &mut Context<Self>) {
        if self.interaction_style == style {
            return;
        }
        self.interaction_style = style;
        cx.notify();
    }

    pub fn search(&mut self, query: String, cx: &mut Context<Self>) {
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        if query.trim().is_empty() {
            self.pending_search = None;
            self.search_hits.clear();
            self.search_page_hits.clear();
            self.active_search_hit = None;
            self.pending_search_scroll = None;
            cx.emit(PdfViewEvent::SearchChanged { current: 0, total: 0 });
            cx.notify();
            return;
        }
        if self.search_in_flight || self.session.is_none() {
            self.pending_search = Some((generation, query));
            return;
        }
        self.start_search(generation, query, cx);
    }

    fn start_search(&mut self, generation: u64, query: String, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.pending_search = Some((generation, query));
            return;
        };
        self.search_in_flight = true;
        self.search_hits.clear();
        self.search_page_hits.clear();
        self.active_search_hit = None;
        self.pending_search_scroll = None;
        let page_count = self.page_count();
        cx.spawn(async move |this, cx| {
            let mut page_index = 0;
            while page_index < page_count {
                // Visible pages get the document before another background search
                // page. Recheck cancellation while yielding during navigation.
                let state = this.update(cx, |this, _| (this.search_generation == generation, this.rendering || this.geometry_loading));
                match state {
                    Ok((true, false)) => {}
                    Ok((true, true)) => {
                        cx.background_executor().timer(std::time::Duration::from_millis(8)).await;
                        continue;
                    }
                    _ => break,
                }
                let last = (page_index + SEARCH_PAGE_CHUNK).min(page_count);
                let session = session.clone();
                let query = query.clone();
                let chunk = session.search(page_index..last, query).await;
                if !this.update(cx, |this, cx| this.apply_search_chunk(generation, chunk, cx)).unwrap_or(false) {
                    break;
                }
                page_index = last;
            }
            let _ = this.update(cx, |this, cx| this.finish_search(generation, cx));
        })
        .detach();
    }

    /// Folds one chunk of hits in, reporting whether the search that produced
    /// them is still the one the reader is waiting for.
    fn apply_search_chunk(&mut self, generation: u64, chunk: PdfResult<Vec<PdfSearchHit>>, cx: &mut Context<Self>) -> bool {
        if self.search_generation != generation {
            return false;
        }
        let hits = match chunk {
            Ok(hits) => hits,
            Err(error) => {
                self.error = Some(components::user_facing_error(&error.to_string(), "Could not display this PDF page. Try reopening the book.").to_owned());
                cx.notify();
                return false;
            }
        };
        if hits.is_empty() {
            return true;
        }
        let first_chunk_with_hits = self.search_hits.is_empty();
        // Search visits pages in order. Publish only the new page slices; older
        // pages keep their Arc even while later results arrive.
        let mut start = 0;
        while start < hits.len() {
            let page_index = hits[start].page_index();
            let end = start + hits[start..].partition_point(|hit| hit.page_index() == page_index);
            let entry = self.search_page_hits.entry(page_index).or_insert_with(|| PageSearchHits { first_index: self.search_hits.len() + start, hits: Arc::from([]) });
            entry.hits = if entry.hits.is_empty() {
                Arc::from(&hits[start..end])
            } else {
                // Also support a page split across successive result batches.
                let mut combined = Vec::with_capacity(entry.hits.len() + end - start);
                combined.extend_from_slice(&entry.hits);
                combined.extend_from_slice(&hits[start..end]);
                combined.into()
            };
            start = end;
        }
        self.search_hits.extend(hits);
        // Jump to the first match as soon as it is known, rather than after the
        // whole document has been scanned.
        if first_chunk_with_hits && let Some(hit) = self.search_hits.first().cloned() {
            self.active_search_hit = Some(0);
            self.reveal_skipped_page(hit.page_index());
            self.seek_viewport_anchor(FitWidthAnchor::new(hit.page_index(), 0.0), false, cx);
            self.pending_search_scroll = Some((hit.page_index(), hit.bounds()));
            self.source_position = PdfSourcePosition::new(hit.page_index(), 0.0);
            self.reset_page_scroll();
            self.clear_selection();
            self.request_current_pages(cx);
        }
        cx.emit(PdfViewEvent::SearchChanged { current: self.active_search_hit.map_or(0, |index| index + 1), total: self.search_hits.len() });
        cx.notify();
        true
    }

    fn finish_search(&mut self, generation: u64, cx: &mut Context<Self>) {
        self.search_in_flight = false;
        if self.search_generation == generation {
            cx.emit(PdfViewEvent::SearchChanged { current: self.active_search_hit.map_or(0, |index| index + 1), total: self.search_hits.len() });
        }
        if let Some((next_generation, next_query)) = self.pending_search.take().filter(|(next_generation, _)| *next_generation == self.search_generation) {
            self.start_search(next_generation, next_query, cx);
        }
        cx.notify();
    }

    pub fn navigate_search(&mut self, step: i8, cx: &mut Context<Self>) {
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        if self.search_hits.is_empty() {
            return;
        }
        let current = self.active_search_hit.unwrap_or(0);
        let next = if step < 0 { (current + self.search_hits.len() - 1) % self.search_hits.len() } else { (current + 1) % self.search_hits.len() };
        self.go_to_search_hit(next, cx);
    }

    /// The search hits found so far, in document order.
    pub fn search_hits(&self) -> &[PdfSearchHit] {
        &self.search_hits
    }

    /// Shows one hit from the results list and makes it the active match.
    pub fn go_to_search_hit(&mut self, index: usize, cx: &mut Context<Self>) {
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        let Some(hit) = self.search_hits.get(index).cloned() else { return };
        let next = index;
        self.active_search_hit = Some(next);
        self.reveal_skipped_page(hit.page_index());
        self.seek_viewport_anchor(FitWidthAnchor::new(hit.page_index(), 0.0), false, cx);
        self.pending_search_scroll = Some((hit.page_index(), hit.bounds()));
        self.source_position = PdfSourcePosition::new(hit.page_index(), 0.0);
        self.reset_page_scroll();
        self.clear_selection();
        self.request_current_pages(cx);
        cx.emit(PdfViewEvent::SearchChanged { current: next + 1, total: self.search_hits.len() });
    }

    pub fn copy_selection(&self, cx: &mut Context<Self>) -> bool {
        let Some(selection) = &self.selection else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(selection.text().to_owned()));
        true
    }

    pub fn selection_snapshot(&self) -> Option<PdfSelectionSnapshot> {
        let page_index = self.selection_page?;
        let selection = self.selection.as_ref()?;
        let (_, page) = self.pages.iter().find(|(candidate, _)| *candidate == page_index)?;
        let rects = selection.rects().iter().map(|rect| uncrop_rect(page.page_crop, *rect)).collect::<Vec<_>>();
        (!selection.text().is_empty() && !rects.is_empty()).then(|| PdfSelectionSnapshot { page_index, text: selection.text().to_owned(), rects })
    }

    pub fn set_annotations(&mut self, mut annotations: Vec<PdfAnnotationOverlay>, cx: &mut Context<Self>) {
        annotations.sort_by_key(|annotation| annotation.page_index);
        if self.annotations.as_ref() == annotations.as_slice() {
            return;
        }
        self.annotations = Arc::from(annotations);
        cx.notify();
    }

    pub fn reading_request(&self, page: usize) -> Option<impl std::future::Future<Output = PdfResult<Vec<pdf_reader_core::PdfReadingFragment>>> + 'static> {
        let session = self.session.clone()?;
        Some(async move { session.reading_fragments(page).await })
    }

    pub fn set_reading_highlight(&mut self, target: Option<(usize, Vec<NormalizedRect>)>, cx: &mut Context<Self>) {
        self.reading_highlight = target.map(|(page, rects)| (page, Arc::from(rects)));
        cx.notify();
    }
    pub fn follow_reading_fragment(&mut self, page: usize, rects: Vec<NormalizedRect>, cx: &mut Context<Self>) {
        let revision = self.navigation_revision;
        if let Some(bounds) = rects.first() {
            self.reveal_annotation(page, *bounds, cx);
        }
        self.navigation_revision = revision;
        self.set_reading_highlight(Some((page, rects)), cx);
    }
    pub fn advance_reading_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let revision = self.navigation_revision;
        self.set_page(page, cx);
        self.navigation_revision = revision;
    }

    pub fn set_active_annotation(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.active_annotation_id.as_deref() == id.as_deref() {
            return;
        }
        self.active_annotation_id = id.map(Arc::from);
        cx.notify();
    }

    /// Where the passage being annotated sits in the window: the active
    /// annotation if there is one, otherwise the live selection.
    ///
    /// A page rect becomes a window rect only when a page column is painted, so
    /// this reports what the last frame drew. The panel that reads it opens on
    /// the frame after the passage was marked, by which time the value is
    /// there; `None` means nothing was on screen to place against, and the
    /// caller falls back to the pointer.
    pub fn passage_bounds(&self) -> Option<Bounds<Pixels>> {
        self.passage_bounds.get()
    }

    pub fn reveal_annotation(&mut self, page_index: usize, bounds: NormalizedRect, cx: &mut Context<Self>) {
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        if page_index >= self.page_count() {
            return;
        }
        self.reveal_skipped_page(page_index);
        self.seek_viewport_anchor(FitWidthAnchor::new(page_index, 0.0), false, cx);
        self.pending_search_scroll = Some((page_index, bounds));
        self.source_position = PdfSourcePosition::new(page_index, 0.0);
        self.reset_page_scroll();
        self.clear_selection();
        self.request_current_pages(cx);
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn start_load(&mut self, load: impl FnOnce() -> pdf_reader_core::PdfResult<PdfDocumentSession> + Send + 'static, cx: &mut Context<Self>) {
        let generation = self.load_generation;
        cx.spawn(async move |this, cx| {
            let result = session::load(load).await;
            let _ = this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match result {
                    Ok(session) => {
                        this.right_to_left = session.info().right_to_left();
                        this.info = Some(session.info().clone());
                        this.session = Some(session);
                        let mut page_index = this.source_position.page_index().min(this.page_count().saturating_sub(1));
                        if let Some(info) = &this.info
                            && info.skippable_pages().binary_search(&page_index).is_ok()
                            && info.skippable_pages().len() < info.page_count()
                        {
                            page_index = (page_index..info.page_count()).chain(0..page_index).find(|page| info.skippable_pages().binary_search(page).is_err()).unwrap_or(page_index);
                        }
                        this.source_position = PdfSourcePosition::new(page_index, this.source_position.page_y());
                        this.rebuild_fit_width_plan();
                        if !this.fit_width_plan.columns().iter().any(|column| column.page_index() == this.current_page())
                            && let Some(first) = this.fit_width_plan.columns().first().copied()
                        {
                            this.source_position = PdfSourcePosition::new(first.page_index(), 0.0);
                        }
                        this.request_current_pages(cx);
                        if let Some((search_generation, query)) = this.pending_search.take().filter(|(search_generation, _)| *search_generation == this.search_generation) {
                            this.start_search(search_generation, query, cx);
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.error = Some(components::user_facing_error(&message, "Could not open this PDF. Try reopening it or download another copy.").to_owned());
                        cx.emit(PdfViewEvent::LoadFailed(message));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn byte_prefetch_window(&self) -> Vec<usize> {
        let PdfZoomMode::FitWidth { columns } = self.zoom_mode else { return Vec::new() };
        let Some(info) = &self.info else { return Vec::new() };
        let visible = self.visible_columns().iter().map(|column| column.page_index()).collect::<Vec<_>>();
        info.prefetch_window(self.current_page(), &visible, columns.clamp(MIN_VISIBLE_PAGE_COUNT, MAX_VISIBLE_PAGE_COUNT))
    }

    fn start_prefetch(&mut self, cx: &mut Context<Self>) {
        let pages = self.byte_prefetch_window();
        if self.prefetched_pages == pages {
            return;
        }
        self.prefetch_task.take();
        self.prefetched_pages = pages.clone();
        if pages.is_empty() {
            return;
        }
        let Some(session) = self.session.clone() else { return };
        // Rendering finishes before optional network warming begins. The byte
        // source chooses how many of these priority-ordered pages fit in cache.
        self.prefetch_task = Some(cx.spawn(async move |_, _| {
            let _ = session.prefetch_pages(pages).await;
        }));
    }

    fn request_current_pages(&mut self, cx: &mut Context<Self>) {
        if self.info.is_some() {
            self.rebuild_fit_width_plan();
        }
        let window = self.byte_prefetch_window();
        if self.prefetched_pages != window {
            self.prefetch_task.take();
            self.prefetched_pages.clear();
        }
        self.render_generation = self.render_generation.wrapping_add(1);
        let generation = self.render_generation;
        let missing_geometry = self.render_window_columns().iter().map(|column| PageGeometryKey { page_index: column.page_index(), trim_margins: self.trim_margins }).filter(|key| !self.page_geometry.contains_key(key)).collect::<Vec<_>>();
        if !missing_geometry.is_empty() && self.session.is_some() {
            if !self.geometry_loading {
                self.start_geometry(missing_geometry, cx);
            }
            self.emit_position_changed(cx);
            cx.notify();
            return;
        }
        if self.reveal_pending_search() {
            self.request_current_pages(cx);
            return;
        }
        let mut keys = self.visible_render_keys();
        if !window.is_empty() {
            keys.sort_by_key(|key| window.iter().position(|page| *page == key.page_index()).unwrap_or(usize::MAX));
        }
        if keys.is_empty() {
            self.pages.clear();
            self.emit_position_changed(cx);
            cx.notify();
            return;
        }
        self.pages = self.display_pages(&keys);
        let missing_keys = keys.into_iter().filter(|key| !self.display_cache.contains(*key)).collect::<Vec<_>>();
        if missing_keys.is_empty() {
            self.pending_render = None;
            self.start_prefetch(cx);
        } else if self.rendering {
            self.pending_render = Some((generation, missing_keys));
        } else {
            self.error = None;
            self.start_render(generation, missing_keys, cx);
        }
        self.emit_position_changed(cx);
        cx.notify();
    }

    fn start_geometry(&mut self, mut keys: Vec<PageGeometryKey>, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else { return };
        let window = self.byte_prefetch_window();
        keys.sort_by_key(|key| (window.iter().position(|page| *page == key.page_index).unwrap_or(usize::MAX), key.page_index));
        keys.dedup();
        self.geometry_loading = true;
        let load_generation = self.load_generation;
        let generation = self.render_generation;
        cx.spawn(async move |this, cx| {
            let mut completed = true;
            for key in keys {
                // Do not enqueue another page from a window that navigation or
                // settings have already replaced.
                if !this.update(cx, |this, _| this.load_generation == load_generation && this.render_generation == generation).unwrap_or(false) {
                    completed = false;
                    break;
                }
                let result = session.page_geometry(key.page_index, key.trim_margins).await;
                let proceed = this
                    .update(cx, |this, cx| {
                        if this.load_generation != load_generation {
                            return false;
                        }
                        match result {
                            Ok(geometry) => {
                                this.page_geometry.insert(key, geometry);
                                this.geometry_revision = this.geometry_revision.wrapping_add(1);
                                this.rebuild_fit_width_plan();
                                this.render_generation == generation
                            }
                            Err(error) => {
                                if this.render_generation == generation {
                                    this.error = Some(components::user_facing_error(&error.to_string(), "Could not analyze this PDF page. Try reopening the book.").to_owned());
                                    cx.notify();
                                }
                                false
                            }
                        }
                    })
                    .unwrap_or(false);
                if !proceed {
                    completed = false;
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.load_generation != load_generation {
                    return;
                }
                this.geometry_loading = false;
                if completed || this.render_generation != generation {
                    this.request_current_pages(cx);
                }
            });
        })
        .detach();
    }

    fn visible_render_keys(&self) -> Vec<PageRenderKey> {
        let mut keys = Vec::new();
        for column in self.render_window_columns() {
            let width = self.render_target_width(column.page_index());
            let Some(page) = self.info.as_ref().and_then(|info| info.page(column.page_index())) else { continue };
            let crop = self.geometry_crop(column.page_index());
            let full_height = (page.height_points() as f64 / page.width_points() as f64 * width as f64).round().max(1.0) as f32;
            let crop_top = (crop.top() * full_height).floor();
            let crop_height = (crop.bottom() * full_height).ceil() - crop_top;
            let source_top = crop.top() + column.page_rect().top() * crop.height();
            let source_bottom = crop.top() + column.page_rect().bottom() * crop.height();
            let top = (source_top * full_height).floor() - crop_top;
            let bottom = ((source_bottom * full_height).ceil() - crop_top).min(crop_height).max(1.0);
            let first = top.max(0.0) as u32 / DISPLAY_TILE_HEIGHT;
            let last = (bottom as u32 - 1) / DISPLAY_TILE_HEIGHT;
            for tile in first..=last {
                let key = PageRenderKey::with_margin_trim(column.page_index(), width, self.trim_margins).with_paper_color(self.paper_color).with_ink_color(self.ink_color).with_tile(tile);
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        keys
    }

    fn display_pages(&mut self, keys: &[PageRenderKey]) -> Vec<(usize, Arc<DisplayPage>)> {
        let mut pages = Vec::new();
        // Tile indices at different resolutions cover different PDF regions.
        // Keep the nearest older set in source coordinates, underneath exact
        // tiles, instead of substituting the same tile index at another size.
        for key in keys {
            if !self.display_cache.contains(*key) {
                for page in pdf_fallback_pages(&mut self.display_cache, *key) {
                    if !pages.iter().any(|(_, existing)| Arc::ptr_eq(existing, &page)) {
                        pages.push((key.page_index(), page));
                    }
                }
            }
        }
        for key in keys {
            if let Some(page) = self.display_cache.get(*key) {
                if !pages.iter().any(|(_, existing)| Arc::ptr_eq(existing, &page)) {
                    pages.push((key.page_index(), page));
                }
            }
        }
        pages
    }

    fn start_render(&mut self, generation: u64, keys: Vec<PageRenderKey>, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if keys.is_empty() {
            return;
        }
        self.rendering = true;
        cx.spawn(async move |this, cx| {
            for key in keys {
                // The previous generation may have finished this page while this
                // batch was pending. Never raster an already cached key again.
                match this.update(cx, |this, _| (this.render_generation == generation, this.display_cache.contains(key))) {
                    Ok((true, false)) => {}
                    Ok((true, true)) => continue,
                    _ => break,
                }
                let page_session = session.clone();
                let result = page_session.render_for_display(key).await;
                let keep_rendering = this.update(cx, |this, cx| this.apply_rendered_page(generation, key, result, cx)).unwrap_or(false);
                if !keep_rendering {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.rendering = false;
                let next_render = this.pending_render.take().filter(|(next_generation, _)| *next_generation == this.render_generation);
                if next_render.is_some() {
                    // Refresh both the display and missing keys: the finished
                    // render can now satisfy part or all of the pending window.
                    this.request_current_pages(cx);
                }
                if this.render_generation == generation {
                    if this.error.is_none() {
                        this.start_prefetch(cx);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn apply_rendered_page(&mut self, generation: u64, key: PageRenderKey, result: PdfResult<pdf_reader_core::RenderedPdfBgraPage>, cx: &mut Context<Self>) -> bool {
        let rendered = match result {
            Ok(rendered) => rendered,
            Err(error) => {
                if self.render_generation == generation {
                    self.error = Some(components::user_facing_error(&error.to_string(), "Could not display this PDF page. Try reopening the book.").to_owned());
                    cx.notify();
                }
                return false;
            }
        };
        let page = match DisplayPage::from_rendered(rendered) {
            Ok(page) => Arc::new(page),
            Err(error) => {
                if self.render_generation == generation {
                    self.error = Some(components::user_facing_error(&error, "Could not display this PDF page. Try reopening the book.").to_owned());
                    cx.notify();
                }
                return false;
            }
        };
        self.display_cache.insert(key, page.clone(), page.raster.byte_len);
        if self.render_generation != generation {
            return false;
        }
        self.pages = self.display_pages(&self.visible_render_keys());
        self.emit_position_changed(cx);
        cx.notify();
        true
    }

    fn emit_position_changed(&self, cx: &mut Context<Self>) {
        cx.emit(PdfViewEvent::PositionChanged { position: self.source_position, total: self.page_count() });
    }

    fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.selection_anchor_source_index = None;
        self.selection_active = None;
        self.selection_page = None;
        self.selection_page_column = None;
        self.selection_granularity = SelectionGranularity::Character;
        self.selection = None;
    }

    fn reset_page_scroll(&mut self) {
        let anchor = self.display_anchor(self.source_position);
        self.flow_offset = self.flow_offset_for_page_y(anchor.page_index(), anchor.page_y()).unwrap_or(0.0).clamp(0.0, self.max_flow_offset());
        self.scroll_handle.set_offset(point(Pixels::ZERO, Pixels::ZERO));
        self.update_source_position();
    }

    /// Rebuilds the visible columns directly from a source position. This is
    /// deliberately independent of the pager history: page jumps, search
    /// results, and future scrollbar targets may all name an anchor that has
    /// never been visible before.
    fn seek_viewport_anchor(&mut self, anchor: FitWidthAnchor, remember_previous: bool, cx: &mut Context<Self>) -> bool {
        self.scroll_animation = None;
        if !self.reflow_viewport_anchor(anchor, remember_previous) {
            return false;
        }
        self.request_current_pages(cx);
        true
    }

    fn viewport_starts_at(&self, anchor: FitWidthAnchor) -> bool {
        self.flow_offset == 0.0 && self.flow_anchor_at(0.0).is_some_and(|start| start.page_index() == anchor.page_index() && (start.page_y() - anchor.page_y()).abs() <= f32::EPSILON * 4.0)
    }

    /// Position changes in an anchored viewport must repack its columns. A
    /// vertical translation would carry the unused column tails into the
    /// middle of the screen and splice in the next preplanned column.
    fn reflow_viewport_anchor(&mut self, anchor: FitWidthAnchor, remember_previous: bool) -> bool {
        // Restoring the displayed source anchor must not invalidate the page
        // turn history or start an unnecessary layout pass.
        if self.viewport_pager.is_some() && self.viewport_starts_at(anchor) {
            return true;
        }
        let source = self.source_anchor(anchor);
        let source = FitWidthAnchor::new(source.page_index(), source.page_y());
        let reflowed = self.viewport_pager.as_mut().and_then(|pager| pager.seek(source, remember_previous).ok().map(|_| pager.window().plan().clone()));
        let Some(plan) = reflowed else {
            return false;
        };
        self.apply_viewport_plan(plan);
        true
    }

    fn apply_viewport_plan(&mut self, plan: FitWidthPlan) {
        self.fit_width_plan = plan;
        self.flow_offset = 0.0;
        self.scroll_handle.set_offset(point(Pixels::ZERO, Pixels::ZERO));
        if let Some(pager) = &self.viewport_pager {
            let anchor = pager.window().start();
            self.source_position = PdfSourcePosition::new(anchor.page_index(), anchor.page_y());
        } else {
            self.update_source_position();
        }
        self.navigation_offsets.take();
    }

    /// Switches between source-anchored viewport windows. The next window is
    /// planned from the old end anchor; backward restores the previous start
    /// anchor. No display-column index is carried across the transition.
    fn move_viewport_anchor(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some(pager) = self.viewport_pager.as_mut() else {
            return false;
        };
        let moved = if forward { pager.forward() } else { pager.backward() };
        let Ok(true) = moved else {
            return false;
        };
        let plan = pager.window().plan().clone();
        self.apply_viewport_plan(plan);
        self.clear_selection();
        self.request_current_pages(cx);
        true
    }

    fn animate_anchor_scroll(&mut self, anchor: FitWidthAnchor, cx: &mut Context<Self>) {
        let distance = self.viewport_pager.as_ref().and_then(|pager| pager.distance_to(anchor)).unwrap_or(0.0);
        if !PageTurn::<PdfFlowSnapshot>::enabled(cx) || distance == 0.0 {
            self.seek_viewport_anchor(self.display_anchor(PdfSourcePosition::new(anchor.page_index(), anchor.page_y())), false, cx);
            return;
        }
        self.animate_flow_scroll(-distance, cx);
        let target = PdfSourcePosition::new(anchor.page_index(), anchor.page_y());
        if let Some(animation) = &mut self.scroll_animation {
            animation.target_anchor = Some(target);
        }
    }

    fn animate_item_scroll(&mut self, target: f32, cx: &mut Context<Self>) {
        if !PageTurn::<PdfFlowSnapshot>::enabled(cx) {
            self.set_flow_offset(target, cx);
            return;
        }
        let target_anchor = self.viewport_pager.as_ref().and_then(|_| self.flow_anchor_at(target));
        self.animate_flow_scroll(self.flow_offset - target, cx);
        let target_anchor = target_anchor.map(|anchor| self.source_anchor(anchor));
        if let Some(animation) = &mut self.scroll_animation {
            animation.target_anchor = target_anchor;
        }
    }

    fn animate_flow_scroll(&mut self, delta_y: f32, cx: &mut Context<Self>) {
        if !delta_y.is_finite() || delta_y == 0.0 {
            return;
        }
        self.pending_search_scroll = None;
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
        self.page_turn = None;
        self.swipe_preview.clear();
        if !PageTurn::<PdfFlowSnapshot>::enabled(cx) {
            self.finish_scroll_animation(cx);
            self.sync_flow_scroll_target(delta_y, cx);
            return;
        }
        // Retarget from the position currently on screen, retaining all of
        // the wheel burst's unapplied distance, including direction changes.
        let distance = self.scroll_animation.take().map_or(0.0, |animation| animation.remaining()) - delta_y;
        self.scroll_animation = Some(PdfScrollAnimation { distance, applied: 0.0, started_at: Instant::now(), target_anchor: None });
        cx.notify();
    }

    fn finish_scroll_animation(&mut self, cx: &mut Context<Self>) {
        if let Some(animation) = self.scroll_animation.take() {
            if let Some(anchor) = animation.target_anchor {
                self.seek_viewport_anchor(self.display_anchor(anchor), false, cx);
            } else {
                self.sync_flow_scroll_target(-animation.remaining(), cx);
            }
        }
    }

    fn advance_scroll_animation(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(mut animation) = self.scroll_animation.take() else { return false };
        let (delta, finished) = animation.step(!PageTurn::<PdfFlowSnapshot>::enabled(cx));
        if finished && let Some(anchor) = animation.target_anchor {
            self.seek_viewport_anchor(self.display_anchor(anchor), false, cx);
        } else if delta != 0.0 {
            self.sync_flow_scroll_target(-delta, cx);
        }
        if !finished {
            self.scroll_animation = Some(animation);
        }
        !finished
    }

    fn sync_flow_scroll_target(&mut self, delta_y: f32, cx: &mut Context<Self>) {
        self.page_turn = None;
        self.swipe_preview.clear();
        if let Some(pager) = &mut self.viewport_pager {
            if pager.scroll_by(-delta_y).unwrap_or(false) {
                let plan = pager.window().plan().clone();
                self.apply_viewport_plan(plan);
                self.request_current_pages(cx);
            }
            return;
        }
        self.set_flow_offset(self.flow_offset - delta_y, cx);
    }

    fn page_drag(&self, travel: Point<f32>) -> Point<f32> {
        if self.zoom_mode == PdfZoomMode::FitHeight { travel } else { point(0.0, if travel.x.abs() > travel.y.abs() { if self.right_to_left { -travel.x } else { travel.x } } else { travel.y }) }
    }

    fn flow_scroll_delta(&self, travel: Point<f32>) -> f32 {
        if travel.y.abs() >= travel.x.abs() {
            travel.y
        } else if self.right_to_left {
            -travel.x
        } else {
            travel.x
        }
    }

    /// Touch swipes turn a complete viewport in every zoom mode. Physical
    /// wheel bursts also turn complete pages in fit-height mode.
    /// `travel` is this event's movement on both axes. `from_edge` says a
    /// swipe began in the bottom gesture area; it is read when the swipe
    /// starts.
    fn handle_page_scroll(&mut self, travel: Point<f32>, phase: TouchPhase, from_edge: bool, window: &Window, cx: &mut Context<Self>) {
        if cfg!(target_os = "android") || window.is_page_scroll_swipe() || self.touch_swipe.is_active() {
            let viewport = size(px(self.available_width), px(self.flow_column_height()));
            let travel = if travel.x.is_finite() && travel.y.is_finite() { travel } else { gpui::point(0.0, 0.0) };
            match phase {
                TouchPhase::Started => {
                    self.scroll_animation = None;
                    self.page_turn = None;
                    self.wheel_debounce.take();
                    self.touch_swipe.begin(from_edge);
                    if self.touch_swipe.push(travel.x, travel.y).is_some() {
                        if self.swipe_preview.drag_in_viewport(self.page_drag(travel), viewport) {
                            cx.notify();
                        }
                    }
                    return;
                }
                TouchPhase::Moved if self.touch_swipe.is_active() => {
                    if let Some((dx, dy)) = self.touch_swipe.push(travel.x, travel.y) {
                        let travel = point(dx, dy);
                        if self.swipe_preview.drag_in_viewport(self.page_drag(travel), viewport) {
                            cx.notify();
                        }
                    }
                    return;
                }
                TouchPhase::Ended if self.touch_swipe.is_active() => {
                    let forward = self.touch_swipe.finish().and_then(|(dx, dy)| components::swipe_turns_forward_with_direction(point(dx, dy), window, self.right_to_left));
                    match forward {
                        Some(forward) => {
                            if forward {
                                self.next_page(cx)
                            } else {
                                self.previous_page(cx)
                            }
                        }
                        None => self.swipe_preview.settle(),
                    }
                    cx.notify();
                    return;
                }
                TouchPhase::Cancelled => {
                    self.swipe_preview.settle();
                    cx.notify();
                    self.touch_swipe.cancel();
                    self.wheel_delta = 0.0;
                    self.wheel_debounce.take();
                    return;
                }
                _ => {}
            }
        }
        // A wheel or trackpad: whichever axis it moved on.
        let delta = self.flow_scroll_delta(travel);
        if !delta.is_finite() || delta == 0.0 {
            return;
        }
        self.wheel_delta += delta;
        self.wheel_debounce.take();
        let timer = cx.background_executor().timer(WHEEL_SCROLL_END_DELAY);
        self.wheel_debounce = Some(cx.spawn(async move |view, cx| {
            timer.await;
            let _ = view.update(cx, |view, cx| view.finish_fit_height_scroll(cx));
        }));
    }

    fn finish_fit_height_scroll(&mut self, cx: &mut Context<Self>) {
        self.wheel_debounce.take();
        let delta = std::mem::take(&mut self.wheel_delta);
        if self.zoom_mode != PdfZoomMode::FitHeight {
            return;
        }
        if delta < 0.0 {
            self.next_page(cx);
        } else if delta > 0.0 {
            self.previous_page(cx);
        }
    }

    /// Resolves one pointer position through the horizontally streamed column
    /// layout. All selection and annotation input uses this single mapping.
    fn interaction_target(&self, position: Point<Pixels>, surface: Bounds<Pixels>) -> Option<PdfInteractionTarget> {
        let column_height = self.flow_column_height().max(1.0);
        let relative_x = f32::from(position.x - surface.origin.x);
        let relative_y = f32::from(position.y - surface.origin.y);
        if relative_x < 0.0 || relative_y < 0.0 || relative_x >= f32::from(surface.size.width) || relative_y >= f32::from(surface.size.height) {
            return None;
        }
        let slots = page_slots(f32::from(surface.size.width), &self.screen_column_widths(), self.right_to_left, self.page_gap);
        let (slot_index, &(slot_left, column_width)) = slots.iter().enumerate().find(|(_, slot)| relative_x >= slot.0 && relative_x < slot.0 + slot.1)?;
        let streamed_y = relative_y + self.flow_remainder();
        let stream_index = (streamed_y / column_height).floor() as usize;
        if stream_index > 1 || (self.zoom_mode == PdfZoomMode::FitHeight && stream_index != 0) {
            return None;
        }
        let display_index = self.flow_base_index().checked_add(slot_index)?.checked_add(stream_index)?;
        let y_in_column = streamed_y - stream_index as f32 * column_height;
        let x_in_slot = relative_x - slot_left;
        for column in columns_for_display(self.fit_width_plan.columns(), display_index) {
            let fragment_top = self.column_offset_within_display(*column);
            let fragment_bottom = fragment_top + column.display_height();
            let fragment_width = column.display_width();
            let fragment_left = if self.zoom_mode == PdfZoomMode::FitHeight { ((column_width - fragment_width) * 0.5).max(0.0) } else { 0.0 };
            if fragment_left <= x_in_slot && x_in_slot <= fragment_left + fragment_width && fragment_top <= y_in_column && y_in_column <= fragment_bottom {
                let bounds = Bounds {
                    origin: point(surface.origin.x + px(slot_left + fragment_left), surface.origin.y - px(self.flow_remainder()) + px(stream_index as f32 * column_height + fragment_top)),
                    size: size(px(fragment_width), px(column.display_height())),
                };
                let page_point = column.column_point_to_page(normalized_point(position, bounds));
                let page_display_size = (fragment_width / column.page_rect().width().max(f32::EPSILON), column.display_height() / column.page_rect().height().max(f32::EPSILON));
                return Some(PdfInteractionTarget { page_index: column.page_index(), column: *column, page_point, page_display_size });
            }
        }
        None
    }

    fn annotation_at(&self, target: PdfInteractionTarget) -> Option<String> {
        let page = self.pages.iter().find(|(page_index, _)| *page_index == target.page_index)?.1.as_ref();
        annotations_for_page(&self.annotations, target.page_index)
            .iter()
            .find(|annotation| annotation.rects.iter().filter_map(|rect| page.map_rect(*rect)).any(|rect| normalized_rect_contains(rect, target.page_point)))
            .map(|annotation| annotation.id.clone())
    }

    fn selection_anchor_source_index(&self, target: PdfInteractionTarget) -> Option<usize> {
        let page = self.pages.iter().find(|(page_index, _)| *page_index == target.page_index)?.1.as_ref();
        // The displayed geometry is ready even while PDFium is busy rendering.
        // Distances are measured in screen pixels so tall/narrow pages retain
        // the same hit tolerance in both directions.
        cached_character_index(&page.text_layout, target.page_point, target.page_display_size)
    }

    fn reveal_pending_search(&mut self) -> bool {
        let Some((page_index, hit)) = self.pending_search_scroll.take() else {
            return false;
        };
        let Some(geometry) = self.page_geometry.get(&PageGeometryKey { page_index, trim_margins: self.trim_margins }) else {
            self.pending_search_scroll = Some((page_index, hit));
            return false;
        };
        let Some(hit) = crop_rect(geometry.crop, hit) else {
            return false;
        };
        if self.viewport_pager.is_some() {
            let anchor = FitWidthAnchor::new(page_index, hit.top());
            let changed = !self.viewport_starts_at(anchor);
            if self.reflow_viewport_anchor(anchor, false) {
                return changed;
            }
        }
        let before = self.source_position;
        let target_y = (hit.top() + hit.height() * 0.5).clamp(0.0, 1.0);
        self.flow_offset = self.flow_offset_for_page_y(page_index, target_y).unwrap_or(0.0).clamp(0.0, self.max_flow_offset());
        self.update_source_position();
        self.source_position != before
    }

    fn begin_selection(&mut self, page_index: usize, page_column_index: u32, position: (f32, f32), anchor_source_index: usize, granularity: SelectionGranularity, cx: &mut Context<Self>) {
        self.selection_page = Some(page_index);
        self.selection_page_column = Some(page_column_index);
        self.selection_anchor = Some(position);
        self.selection_anchor_source_index = Some(anchor_source_index);
        self.selection_active = Some(position);
        self.selection_granularity = granularity;
        self.update_selection();
        cx.notify();
    }

    fn update_selection_to(&mut self, page_index: usize, _page_column_index: u32, position: (f32, f32), cx: &mut Context<Self>) -> bool {
        if self.selection_anchor.is_none() || self.selection_page != Some(page_index) {
            return false;
        }
        self.selection_active = Some(position);
        self.update_selection();
        cx.notify();
        true
    }

    fn update_selection(&mut self) {
        self.selection = self
            .pages
            .iter()
            .find(|(page_index, _)| Some(*page_index) == self.selection_page)
            .and_then(|(_, page)| Some((page, self.selection_anchor_source_index?, self.selection_active?)))
            .and_then(|(page, anchor_source_index, active)| page.text_layout.select_from_source_index_with_options(anchor_source_index, active, SelectionOptions::default().with_granularity(self.selection_granularity)))
            .map(Arc::new);
    }

    /// The page the layout is measured against.
    ///
    /// Trimming changes a page's effective size, so this follows the page being
    /// read rather than the document, and the page count changes with it.
    fn reference_page_size(&self) -> (f32, f32) {
        self.info
            .as_ref()
            .and_then(|info| info.page(self.source_position.page_index()))
            .map(|page| {
                let crop = self.geometry_crop(self.source_position.page_index());
                (page.width_points() * crop.width(), page.height_points() * crop.height())
            })
            .unwrap_or((DEFAULT_PAGE_WIDTH_POINTS, DEFAULT_PAGE_HEIGHT_POINTS))
    }

    fn width_fit_zoom(&self, columns: usize) -> f32 {
        width_fit_zoom(columns, self.available_width, self.reference_page_size().0, self.page_gap)
    }

    fn resolve_layout(&self) -> ResolvedLayout {
        resolve_layout(self.zoom_mode, self.available_width, self.page_display_height(), self.reference_page_size(), self.page_gap)
    }

    fn page_display_width(&self) -> f32 {
        self.resolve_layout().column_width
    }

    fn page_display_height(&self) -> f32 {
        (self.available_height - FIT_WIDTH_SURFACE_PADDING * 2.0).max(1.0)
    }

    fn flow_column_height(&self) -> f32 {
        self.page_display_height()
    }

    fn rebuild_fit_width_plan(&mut self) {
        let Some(info) = &self.info else {
            self.fit_width_plan = FitWidthPlan::default();
            self.viewport_pager = None;
            self.navigation_offsets.take();
            return;
        };
        let Some(viewport) = FitWidthViewport::new(self.page_display_width(), self.page_display_height()) else {
            self.fit_width_plan = FitWidthPlan::default();
            self.viewport_pager = None;
            self.navigation_offsets.take();
            return;
        };
        let key = (self.page_display_width(), self.page_display_height(), self.visible_page_count(), self.zoom_mode == PdfZoomMode::FitHeight, self.trim_margins, self.geometry_revision);
        if self.plan_key == Some(key) {
            return;
        }
        let same_margin_mode = self.plan_key.is_some_and(|previous| previous.4 == self.trim_margins);
        self.plan_key = Some(key);
        self.navigation_offsets.take();
        let pages = info
            .pages()
            .iter()
            .enumerate()
            .filter(|(page_index, _)| *page_index == self.revealed_page.unwrap_or(usize::MAX) || info.skippable_pages().binary_search(page_index).is_err() || (info.skippable_pages().len() == info.page_count() && *page_index == 0))
            .filter_map(|(page_index, page)| {
                let crop = self.geometry_crop(page_index);
                let (width, height) = (page.width_points() * crop.width(), page.height_points() * crop.height());
                FitWidthPage::new(page_index, width, height).map(|page| page.with_source_crop(crop))
            })
            .collect::<Vec<_>>();
        self.fit_width_plan = match self.zoom_mode {
            PdfZoomMode::FitHeight => {
                self.viewport_pager = None;
                FitWidthPlan::fit_page(pages, viewport)
            }
            PdfZoomMode::FitWidth { .. } | PdfZoomMode::Fixed { .. } => {
                let desired = FitWidthAnchor::new(self.source_position.page_index(), self.source_position.page_y());
                let start = if pages.iter().any(|page| page.page_index() == desired.page_index()) {
                    desired
                } else {
                    let next = pages.iter().find(|page| page.page_index() >= desired.page_index()).or_else(|| pages.last());
                    FitWidthAnchor::new(next.map_or(0, |page| page.page_index()), 0.0)
                };
                let pager_pages = pages
                    .iter()
                    .copied()
                    .map(|page| {
                        let bands = self.page_geometry.get(&PageGeometryKey { page_index: page.page_index(), trim_margins: self.trim_margins }).map(|geometry| geometry.break_avoidance.as_ref()).unwrap_or(&[]).to_vec();
                        (page, bands)
                    })
                    .collect::<Vec<_>>();
                let columns = self.visible_page_count();
                let pager = match self.viewport_pager.take() {
                    Some(mut pager) if same_margin_mode => pager.reconfigure(pager_pages, viewport, columns, start).map(|()| pager),
                    _ => FitWidthAnchorPager::new(pager_pages, viewport, columns, start),
                };
                match pager {
                    Ok(pager) => {
                        let plan = pager.window().plan().clone();
                        self.viewport_pager = Some(pager);
                        plan
                    }
                    Err(error) => {
                        log::warn!("planning anchored PDF viewport: {error}; falling back to document plan");
                        self.viewport_pager = None;
                        self.wrapped_plan(&pages, viewport)
                    }
                }
            }
        };
        if self.viewport_pager.is_some() {
            self.flow_offset = 0.0;
        } else {
            self.flow_offset = self.flow_offset_for_page_y(self.source_position.page_index(), 0.0).unwrap_or(0.0).clamp(0.0, self.max_flow_offset());
        }
    }

    fn reveal_skipped_page(&mut self, page_index: usize) {
        if self.info.as_ref().is_some_and(|info| info.skippable_pages().binary_search(&page_index).is_ok()) && self.revealed_page != Some(page_index) {
            self.revealed_page = Some(page_index);
            self.plan_key = None;
            self.rebuild_fit_width_plan();
        }
    }

    /// Wraps every page into uninterrupted fit-width columns, degrading instead
    /// of leaving the reader with nothing to show.
    ///
    /// The planner rejects a whole document over one unplannable page, and an
    /// empty plan renders no columns, so the reader would sit on "Rendering PDF
    /// page…" forever. Internal slices use their exact geometric boundary;
    /// complete source pages continue below one another in a display column.
    fn wrapped_plan(&self, pages: &[FitWidthPage], viewport: FitWidthViewport) -> FitWidthPlan {
        let continuous_pages = pages.iter().copied().map(|page| {
            let bands = self.page_geometry.get(&PageGeometryKey { page_index: page.page_index(), trim_margins: self.trim_margins }).map(|geometry| geometry.break_avoidance.as_ref()).unwrap_or(&[]);
            (page, bands)
        });
        match FitWidthPlan::new_avoiding_breaks(continuous_pages, viewport) {
            Ok(plan) => plan,
            Err(error) => {
                log::warn!("planning continuous PDF fit-width columns: {error}; retrying without break avoidance");
                FitWidthPlan::new(pages.iter().copied(), viewport).unwrap_or_else(|error| {
                    log::warn!("planning PDF fit-width columns: {error}; falling back to whole pages");
                    FitWidthPlan::fit_page(pages.iter().copied(), viewport)
                })
            }
        }
    }

    fn visible_columns(&self) -> Vec<FitWidthColumn> {
        let Some(total) = self.display_column_count() else {
            return Vec::new();
        };
        let visible = visible_flow_indices(self.flow_base_index(), total, self.visible_page_count());
        let Some(first) = visible.first().copied() else { return Vec::new() };
        let last = visible.last().copied().unwrap_or(first);
        columns_for_display_range(self.fit_width_plan.columns(), first, last).to_vec()
    }

    fn visible_columns_count(&self) -> usize {
        self.visible_page_count().min(self.display_column_count().unwrap_or(0))
    }

    fn screen_column_widths(&self) -> SmallVec<[f32; MAX_VISIBLE_PAGE_COUNT]> {
        (0..self.visible_columns_count())
            .map(|slot| {
                if self.zoom_mode == PdfZoomMode::FitHeight {
                    columns_for_display(self.fit_width_plan.columns(), self.flow_base_index() + slot).first().map_or(self.page_display_width(), |column| column.display_width())
                } else {
                    self.page_display_width()
                }
            })
            .collect()
    }

    fn render_window_columns(&self) -> &[FitWidthColumn] {
        let Some(total) = self.display_column_count() else { return &[] };
        let first_visible = self.flow_base_index().min(total.saturating_sub(1));
        let first = first_visible.saturating_sub(1);
        let last = (first_visible + self.visible_page_count().max(1)).min(total.saturating_sub(1));
        columns_for_display_range(self.fit_width_plan.columns(), first, last)
    }

    fn display_column_count(&self) -> Option<usize> {
        self.fit_width_plan.columns().last().map(|column| column.display_column_index() + 1)
    }

    fn flow_base_index(&self) -> usize {
        split_flow_offset(self.flow_offset, self.flow_column_height()).0
    }

    fn flow_remainder(&self) -> f32 {
        split_flow_offset(self.flow_offset, self.flow_column_height()).1
    }

    fn max_flow_offset(&self) -> f32 {
        let total = self.display_column_count().unwrap_or(0);
        total.saturating_sub(self.visible_page_count().max(1)) as f32 * self.flow_column_height()
    }

    fn column_offset_within_display(&self, target: FitWidthColumn) -> f32 {
        if self.zoom_mode == PdfZoomMode::FitHeight {
            return ((self.flow_column_height() - target.display_height()) * 0.5).max(0.0);
        }
        columns_for_display(self.fit_width_plan.columns(), target.display_column_index()).iter().take_while(|column| column.flow_index() != target.flow_index()).map(|column| column.leading_page_gap() + column.display_height()).sum::<f32>()
            + target.leading_page_gap()
    }

    fn flow_offset_for_column(&self, column: FitWidthColumn, column_y: f32) -> f32 {
        if self.zoom_mode == PdfZoomMode::FitHeight {
            return column.display_column_index() as f32 * self.flow_column_height();
        }
        column.display_column_index() as f32 * self.flow_column_height() + self.column_offset_within_display(column) + column_y.clamp(0.0, 1.0) * column.display_height()
    }

    fn flow_offset_for_page_y(&self, page_index: usize, display_y: f32) -> Option<f32> {
        let column = columns_for_page(self.fit_width_plan.columns(), page_index).iter().copied().find(|column| column.page_point_to_column((0.5, display_y)).is_some())?;
        let column_y = column.page_point_to_column((0.5, display_y))?.1;
        Some(self.flow_offset_for_column(column, column_y))
    }

    fn flow_anchor_at(&self, offset: f32) -> Option<FitWidthAnchor> {
        let (display_index, remainder) = split_flow_offset(offset, self.flow_column_height());
        let columns = columns_for_display(self.fit_width_plan.columns(), display_index);
        let first = *columns.first()?;
        if self.zoom_mode == PdfZoomMode::FitHeight {
            return Some(FitWidthAnchor::new(first.page_index(), 0.0));
        }
        let mut start = 0.0;
        for column in columns {
            start += column.leading_page_gap();
            let end = start + column.display_height();
            if remainder <= end {
                let column_y = ((remainder - start) / column.display_height().max(1.0)).clamp(0.0, 1.0);
                return Some(FitWidthAnchor::new(column.page_index(), column.column_point_to_page((0.5, column_y)).1));
            }
            start = end;
        }
        let column = *columns.last().unwrap_or(&first);
        Some(FitWidthAnchor::new(column.page_index(), column.page_rect().bottom()))
    }

    fn set_flow_offset(&mut self, offset: f32, cx: &mut Context<Self>) {
        if self.viewport_pager.is_some()
            && let Some(anchor) = self.flow_anchor_at(offset.clamp(0.0, self.max_flow_offset()))
            && self.seek_viewport_anchor(anchor, false, cx)
        {
            return;
        }
        let previous_base = self.flow_base_index();
        self.flow_offset = if self.zoom_mode == PdfZoomMode::FitHeight { whole_page_offset(offset, self.flow_column_height()) } else { offset }.clamp(0.0, self.max_flow_offset());
        self.scroll_handle.set_offset(point(Pixels::ZERO, Pixels::ZERO));
        self.update_source_position();
        if self.flow_base_index() != previous_base {
            self.request_current_pages(cx);
        } else {
            self.emit_position_changed(cx);
            cx.notify();
        }
    }

    fn atomic_flow_offsets(&mut self) -> Arc<[f32]> {
        let extent = (self.flow_column_height(), self.max_flow_offset());
        if let Some(cached) = self.navigation_offsets.as_ref()
            && (cached.column_height, cached.maximum_offset) == extent
        {
            return cached.offsets.clone();
        }
        let mut offsets = vec![0.0, self.max_flow_offset()];
        offsets.extend(self.fit_width_plan.columns().iter().copied().map(|column| self.flow_offset_for_column(column, 0.0)));
        for (key, geometry) in &self.page_geometry {
            if key.trim_margins != self.trim_margins {
                continue;
            }
            offsets.extend(geometry.break_avoidance.iter().filter_map(|(top, _)| self.flow_offset_for_page_y(key.page_index, *top)));
        }
        offsets.retain(|offset| offset.is_finite() && *offset >= 0.0 && *offset <= self.max_flow_offset());
        offsets.sort_by(f32::total_cmp);
        offsets.dedup_by(|left, right| (*left - *right).abs() < 0.5);
        let offsets: Arc<[f32]> = offsets.into();
        self.navigation_offsets = Some(NavigationOffsets { column_height: extent.0, maximum_offset: extent.1, offsets: offsets.clone() });
        offsets
    }

    fn geometry_crop(&self, page_index: usize) -> NormalizedRect {
        self.page_geometry.get(&PageGeometryKey { page_index, trim_margins: self.trim_margins }).map_or(NormalizedRect::new(0.0, 0.0, 1.0, 1.0), |geometry| geometry.crop)
    }

    fn display_anchor(&self, source: PdfSourcePosition) -> FitWidthAnchor {
        let crop = self.geometry_crop(source.page_index());
        FitWidthAnchor::new(source.page_index(), (source.page_y() - crop.top()) / crop.height())
    }

    fn source_anchor(&self, display: FitWidthAnchor) -> PdfSourcePosition {
        let crop = self.geometry_crop(display.page_index());
        PdfSourcePosition::new(display.page_index(), crop.top() + display.page_y() * crop.height())
    }

    fn update_source_position(&mut self) {
        if let Some(anchor) = self.flow_anchor_at(self.flow_offset) {
            self.source_position = self.source_anchor(anchor);
        }
    }

    fn current_page_position(&self) -> f32 {
        self.source_position.page_y()
    }

    fn restore_page_position(&mut self, full_page_position: f32) {
        if !full_page_position.is_finite() {
            return;
        }
        self.source_position = PdfSourcePosition::new(self.current_page(), full_page_position);
        let anchor = self.display_anchor(self.source_position);
        if self.viewport_pager.is_some() && self.viewport_starts_at(anchor) {
            return;
        }
        let source = FitWidthAnchor::new(self.source_position.page_index(), self.source_position.page_y());
        if let Some(plan) = self.viewport_pager.as_mut().and_then(|pager| pager.restore_start(source).ok().map(|()| pager.window().plan().clone())) {
            self.apply_viewport_plan(plan);
            // Keep positions inside trimmed margins stable across mode changes.
            self.source_position = PdfSourcePosition::new(self.current_page(), full_page_position);
        } else {
            self.flow_offset = self.flow_offset_for_page_y(self.current_page(), anchor.page_y()).unwrap_or(0.0).clamp(0.0, self.max_flow_offset());
        }
    }
}

/// The zoom at which `columns` pages fill the viewport width.
fn width_fit_zoom(columns: usize, available_width: f32, page_width: f32, page_gap: f32) -> f32 {
    let gap = effective_page_gap(columns, available_width, page_gap);
    let columns = columns.max(1) as f32;
    let usable_width = (available_width - gap * (columns - 1.0)).max(1.0);
    PageZoom::FitWidth.scale((page_width, 1.0), (usable_width / columns, 1.0)).max(f32::MIN_POSITIVE)
}

/// How many columns of `column_width` fit across the viewport.
fn columns_that_fit(column_width: f32, available_width: f32, page_gap: f32) -> usize {
    let columns = ((available_width.max(1.0) + page_gap) / (column_width + page_gap).max(f32::MIN_POSITIVE)).floor();
    (columns.max(1.0) as usize).clamp(MIN_VISIBLE_PAGE_COUNT, MAX_VISIBLE_PAGE_COUNT)
}

/// Settles a [`PdfZoomMode`] into the zoom, column width and column count the
/// layout runs on.
///
/// One page filling the width is the ceiling in every mode: pages are only ever
/// split downwards, so anything wider would hang off the side with no way to
/// reach it.
fn resolve_layout(zoom_mode: PdfZoomMode, available_width: f32, available_height: f32, page_size: (f32, f32), page_gap: f32) -> ResolvedLayout {
    let (page_width, _) = page_size;
    let widest_zoom = width_fit_zoom(MIN_VISIBLE_PAGE_COUNT, available_width, page_width, page_gap);
    let (columns, zoom) = match zoom_mode {
        PdfZoomMode::FitWidth { columns } => {
            let columns = columns.clamp(MIN_VISIBLE_PAGE_COUNT, MAX_VISIBLE_PAGE_COUNT);
            (columns, width_fit_zoom(columns, available_width, page_width, page_gap))
        }
        PdfZoomMode::Fixed { zoom } => {
            let zoom = zoom.clamp(MIN_ZOOM, widest_zoom);
            (columns_that_fit(page_width * zoom, available_width, page_gap), zoom)
        }
        PdfZoomMode::FitHeight => {
            let zoom = PageZoom::FitPage.scale(page_size, (available_width, available_height)).clamp(MIN_ZOOM, widest_zoom);
            (columns_that_fit(page_width * zoom, available_width, page_gap), zoom)
        }
    };
    ResolvedLayout { columns, column_width: (page_width * zoom).max(1.0), zoom }
}

fn visible_flow_indices(current: usize, total: usize, page_count: usize) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    let page_count = page_count.max(1);
    let first = flow_window_start(current, total, page_count);
    (first..(first + page_count).min(total)).collect()
}

fn columns_for_display(columns: &[FitWidthColumn], display_index: usize) -> &[FitWidthColumn] {
    columns_for_display_range(columns, display_index, display_index)
}

fn columns_for_display_range(columns: &[FitWidthColumn], first: usize, last: usize) -> &[FitWidthColumn] {
    if first > last {
        return &[];
    }
    let start = columns.partition_point(|column| column.display_column_index() < first);
    let end = columns.partition_point(|column| column.display_column_index() <= last);
    &columns[start..end]
}

fn columns_for_page(columns: &[FitWidthColumn], page_index: usize) -> &[FitWidthColumn] {
    let start = columns.partition_point(|column| column.page_index() < page_index);
    let end = columns.partition_point(|column| column.page_index() <= page_index);
    &columns[start..end]
}

fn annotations_for_page(annotations: &[PdfAnnotationOverlay], page_index: usize) -> &[PdfAnnotationOverlay] {
    let start = annotations.partition_point(|annotation| annotation.page_index < page_index);
    let end = annotations.partition_point(|annotation| annotation.page_index <= page_index);
    &annotations[start..end]
}

fn screen_column_stream(base_index: usize, slot_index: usize, total: usize) -> impl Iterator<Item = usize> {
    let current = base_index.saturating_add(slot_index);
    [current, current.saturating_add(1)].into_iter().filter(move |index| *index < total)
}

fn flow_window_start(current: usize, total: usize, page_count: usize) -> usize {
    current.min(total.saturating_sub(page_count.max(1)))
}

fn split_flow_offset(offset: f32, column_height: f32) -> (usize, f32) {
    let column_height = column_height.max(1.0);
    let offset = offset.max(0.0);
    ((offset / column_height).floor() as usize, offset.rem_euclid(column_height))
}

fn whole_page_offset(offset: f32, viewport_height: f32) -> f32 {
    let height = viewport_height.max(1.0);
    (offset.max(0.0) / height).round() * height
}

/// Preserve room for content when a saved gap is large relative to the viewport.
fn effective_page_gap(columns: usize, available_width: f32, page_gap: f32) -> f32 {
    if columns <= 1 { 0.0 } else { page_gap.max(0.0).min(((available_width - 100.0) / (columns - 1) as f32).max(0.0)) }
}

fn page_slots(surface_width: f32, widths: &[f32], right_to_left: bool, page_gap: f32) -> SmallVec<[(f32, f32); MAX_VISIBLE_PAGE_COUNT]> {
    let gap = effective_page_gap(widths.len(), surface_width, page_gap);
    let mut left = ((surface_width - widths.iter().sum::<f32>() - gap * widths.len().saturating_sub(1) as f32) * 0.5).max(0.0);
    widths
        .iter()
        .map(|&width| {
            let slot = (if right_to_left { surface_width - left - width } else { left }, width);
            left += width + gap;
            slot
        })
        .collect()
}

fn next_snap_offset(current: f32, offsets: &[f32]) -> Option<f32> {
    offsets.get(offsets.partition_point(|offset| *offset <= current + 0.5)).copied()
}

fn previous_snap_offset(current: f32, offsets: &[f32]) -> Option<f32> {
    offsets.partition_point(|offset| *offset < current - 0.5).checked_sub(1).map(|index| offsets[index])
}

fn forward_page_snap_offset(current: f32, target: f32, offsets: &[f32]) -> Option<f32> {
    offsets.partition_point(|offset| *offset <= target + 0.5).checked_sub(1).map(|index| offsets[index]).filter(|offset| *offset > current + 0.5).or_else(|| next_snap_offset(current, offsets))
}

fn backward_page_snap_offset(current: f32, target: f32, offsets: &[f32]) -> Option<f32> {
    offsets.get(offsets.partition_point(|offset| *offset < target - 0.5)).copied().filter(|offset| *offset < current - 0.5).or_else(|| previous_snap_offset(current, offsets))
}

fn cached_character_index(layout: &PdfPageTextLayout, point: (f32, f32), display_size: (f32, f32)) -> Option<usize> {
    if !point.0.is_finite() || !point.1.is_finite() || !display_size.0.is_finite() || !display_size.1.is_finite() || display_size.0 <= 0.0 || display_size.1 <= 0.0 {
        return None;
    }
    layout
        .geometry()
        .characters()
        .iter()
        .filter(|character| !character.character().is_control())
        .map(|character| {
            let bounds = character.bounds();
            let dx = (point.0 - point.0.clamp(bounds.left(), bounds.right())) * display_size.0;
            let dy = (point.1 - point.1.clamp(bounds.top(), bounds.bottom())) * display_size.1;
            (character.source_index(), dx * dx + dy * dy)
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .filter(|(_, distance)| *distance <= TEXT_HIT_TOLERANCE_PX * TEXT_HIT_TOLERANCE_PX)
        .map(|(index, _)| index)
}

impl DisplayPage {
    fn from_rendered(page: pdf_reader_core::RenderedPdfBgraPage) -> Result<Self, String> {
        let page = page.into_parts();
        let image = RgbaImage::from_raw(page.pixel_width, page.pixel_height, page.pixels).ok_or_else(|| "PDF renderer returned an invalid RGBA buffer".to_owned())?;
        Ok(Self { ruler_lines: ruler::text_lines(page.text_layout.geometry()), raster: PageImage::from_bgra(image), text_layout: page.text_layout, raster_crop: page.raster_crop, page_crop: page.page_crop })
    }

    fn map_rect(&self, rect: NormalizedRect) -> Option<NormalizedRect> {
        crop_rect(self.page_crop, rect)
    }
}

fn crop_rect(crop: NormalizedRect, rect: NormalizedRect) -> Option<NormalizedRect> {
    let left = rect.left().max(crop.left());
    let top = rect.top().max(crop.top());
    let right = rect.right().min(crop.right());
    let bottom = rect.bottom().min(crop.bottom());
    (left < right && top < bottom).then(|| NormalizedRect::new((left - crop.left()) / crop.width(), (top - crop.top()) / crop.height(), (right - left) / crop.width(), (bottom - top) / crop.height()))
}

fn uncrop_rect(crop: NormalizedRect, rect: NormalizedRect) -> NormalizedRect {
    NormalizedRect::new(crop.left() + rect.left() * crop.width(), crop.top() + rect.top() * crop.height(), rect.width() * crop.width(), rect.height() * crop.height())
}

fn pdf_fallback_pages(cache: &mut DisplayPageCache, key: PageRenderKey) -> Vec<Arc<DisplayPage>> {
    let matches = |candidate: &&PageRenderKey| {
        candidate.page_index() == key.page_index()
            && candidate.trim_margins() == key.trim_margins()
            && candidate.paper_color() == key.paper_color()
            && candidate.ink_color() == key.ink_color()
            && (candidate.target_width() != key.target_width() || candidate.tile().is_none())
    };
    let Some(width) = cache.keys().filter(matches).map(PageRenderKey::target_width).min_by_key(|width| width.abs_diff(key.target_width())) else { return Vec::new() };
    let mut keys = cache.keys().filter(matches).filter(|candidate| candidate.target_width() == width).copied().collect::<Vec<_>>();
    keys.sort_unstable_by_key(PageRenderKey::tile);
    keys.into_iter().filter_map(|key| cache.get(key)).collect()
}

impl IntoElement for PdfPageElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl PdfPageElement {
    fn raster_is_ready(&self) -> bool {
        let mut ranges = self
            .tiles
            .iter()
            .map(|tile| {
                let crop = tile.raster_crop;
                let top = (crop.top() as f32 / crop.full_height() as f32 - tile.page_crop.top()) / tile.page_crop.height();
                let bottom = ((crop.top() + crop.height()) as f32 / crop.full_height() as f32 - tile.page_crop.top()) / tile.page_crop.height();
                (top, bottom, 1.0 / crop.full_height() as f32 / tile.page_crop.height())
            })
            .collect::<Vec<_>>();
        ranges.sort_unstable_by(|left, right| left.0.total_cmp(&right.0));
        let mut covered = self.column.page_rect().top();
        for (top, bottom, tolerance) in ranges {
            if top > covered + tolerance {
                break;
            }
            covered = covered.max(bottom);
            if covered + tolerance >= self.column.page_rect().bottom() {
                return true;
            }
        }
        false
    }

    fn record_passage(&self, bounds: Bounds<Pixels>) {
        let union = match self.passage_bounds.get() {
            Some(existing) => existing.union(&bounds),
            None => bounds,
        };
        self.passage_bounds.set(Some(union));
    }
}

impl Element for PdfPageElement {
    type RequestLayoutState = ();
    type PrepaintState = PdfPagePrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(format!("gpui-pdf-column-{}-{}-{}", self.page_index, self.column.page_column_index(), self.instance_index).into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = px(self.column.display_width()).into();
        style.size.height = px(self.column.display_height()).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut Self::RequestLayoutState, _: &mut Window, _: &mut App) -> Self::PrepaintState {
        let page_height = self.column.display_height() / self.column.page_rect().height().max(f32::EPSILON);
        let image_bounds = self
            .tiles
            .iter()
            .map(|tile| {
                let crop = tile.raster_crop;
                let left = (crop.left() as f32 / crop.full_width() as f32 - tile.page_crop.left()) / tile.page_crop.width();
                let top = (crop.top() as f32 / crop.full_height() as f32 - tile.page_crop.top()) / tile.page_crop.height();
                Bounds {
                    origin: point(bounds.origin.x + bounds.size.width * left, bounds.origin.y + px((top - self.column.page_rect().top()) * page_height)),
                    size: size(bounds.size.width * (crop.width() as f32 / crop.full_width() as f32 / tile.page_crop.width()), px(page_height * crop.height() as f32 / crop.full_height() as f32 / tile.page_crop.height())),
                }
            })
            .collect();
        PdfPagePrepaint { column_bounds: bounds, image_bounds }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        for tile in &self.tiles {
            track_image(&tile.raster.image, cx);
        }
        window.with_content_mask(Some(ContentMask { bounds: prepaint.column_bounds }), |window| {
            for (tile, bounds) in self.tiles.iter().zip(&prepaint.image_bounds) {
                let _ = window.paint_image(*bounds, *bounds, Corners::default(), tile.raster.image.clone(), 0, false);
            }
            for (index, hit) in self.search_hits.iter().flat_map(|page| page.hits.iter().enumerate().map(move |(index, hit)| (page.first_index + index, hit))) {
                let Some(rect) = self.page.map_rect(hit.bounds()).and_then(|rect| self.column.page_rect_to_column(rect)) else { continue };
                let color = if self.active_search_hit == Some(index) { self.interaction_style.active_search_match } else { self.interaction_style.search_match };
                window.paint_quad(fill(normalized_bounds(rect, prepaint.column_bounds), rgba(color.packed_rgba())));
            }
            for annotation in annotations_for_page(&self.annotations, self.page_index) {
                let active = self.active_annotation_id.as_deref() == Some(annotation.id.as_str());
                for source_rect in &annotation.rects {
                    let Some(rect) = self.page.map_rect(*source_rect).and_then(|rect| self.column.page_rect_to_column(rect)) else { continue };
                    if active {
                        self.record_passage(normalized_bounds(rect, prepaint.column_bounds));
                        window.paint_quad(fill(normalized_bounds(rect, prepaint.column_bounds), rgba(self.interaction_style.active_annotation.packed_rgba())));
                    }
                    for rect in annotation_paint_rects(rect, annotation.style) {
                        window.paint_quad(fill(normalized_bounds(rect, prepaint.column_bounds), rgba(annotation.color)));
                    }
                }
            }
            if let Some(rects) = &self.reading_highlight {
                // Narration rectangles use full-page coordinates, just like
                // annotations. Apply the margin crop before the column slice.
                for rect in rects.iter().filter_map(|rect| self.page.map_rect(*rect).and_then(|rect| self.column.page_rect_to_column(rect))) {
                    window.paint_quad(fill(normalized_bounds(rect, prepaint.column_bounds), rgba(self.interaction_style.reading.packed_rgba())));
                }
            }
            if let Some(selection) = &self.selection {
                for rect in selection.rects().iter().filter_map(|rect| self.column.page_rect_to_column(*rect)) {
                    let bounds = normalized_bounds(rect, prepaint.column_bounds);
                    // Only when no annotation is active: a selection left over
                    // from marking the passage must not enlarge the box of the
                    // annotation that replaced it.
                    if self.active_annotation_id.is_none() {
                        self.record_passage(bounds);
                    }
                    window.paint_quad(fill(bounds, rgba(self.interaction_style.selection.packed_rgba())));
                }
            }
        });
    }
}

impl PdfView {
    fn flow_snapshot(&self, passive: bool) -> PdfFlowSnapshot {
        // Outgoing highlights must not overwrite the live annotation position.
        let passage_bounds = if passive { Rc::new(Cell::new(None)) } else { self.passage_bounds.clone() };
        let base_index = self.flow_base_index();
        let total = self.display_column_count().unwrap_or(0);
        let fit_height = self.zoom_mode == PdfZoomMode::FitHeight;
        let slots = self
            .screen_column_widths()
            .iter()
            .copied()
            .enumerate()
            .map(|(slot_index, width)| {
                let streams = screen_column_stream(base_index, slot_index, total)
                    .take(if fit_height { 1 } else { 2 })
                    .enumerate()
                    .map(|(stream_index, current_index)| {
                        columns_for_display(self.fit_width_plan.columns(), current_index)
                            .iter()
                            .copied()
                            .map(|column| {
                                let page_index = column.page_index();
                                let page = self.pages.iter().find(|(candidate, _)| *candidate == page_index).map(|(_, page)| PdfPageElement {
                                    page_index,
                                    column,
                                    page: page.clone(),
                                    tiles: self.pages.iter().filter(|(candidate, _)| *candidate == page_index).map(|(_, page)| page.clone()).collect(),
                                    selection: (self.selection_page == Some(page_index)).then(|| self.selection.clone()).flatten(),
                                    annotations: self.annotations.clone(),
                                    reading_highlight: self.reading_highlight.as_ref().filter(|(page, _)| *page == page_index).map(|(_, rects)| rects.clone()),
                                    active_annotation_id: self.active_annotation_id.clone(),
                                    search_hits: self.search_page_hits.get(&page_index).cloned(),
                                    active_search_hit: self.active_search_hit,
                                    interaction_style: self.interaction_style,
                                    instance_index: slot_index * 2 + stream_index,
                                    passage_bounds: passage_bounds.clone(),
                                });
                                PdfFlowFragment { column, page }
                            })
                            .collect()
                    })
                    .collect();
                PdfFlowSlot { width, streams }
            })
            .collect();
        PdfFlowSnapshot { ruler: self.prepare_reading_ruler(), page_gap: effective_page_gap(self.visible_columns_count(), self.available_width, self.page_gap), column_height: self.flow_column_height(), remainder: self.flow_remainder(), fit_height, right_to_left: self.right_to_left, slots }
    }
}

fn pdf_document_colors(theme: components::BrowserTheme) -> ([u8; 3], [u8; 3]) {
    let rgb = |color: Hsla| {
        let color = color.to_rgb();
        [color.r, color.g, color.b].map(|channel| (channel * 255.0).round() as u8)
    };
    // Light themes retain the original dark ink; dark themes need light ink.
    let ink = if theme.page_bg.l < 0.5 { rgb(theme.text) } else { [0; 3] };
    (rgb(theme.page_bg), ink)
}

impl Render for PdfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let (paper_color, ink_color) = pdf_document_colors(theme);
        if self.paper_color != paper_color || self.ink_color != ink_color {
            self.paper_color = paper_color;
            self.ink_color = ink_color;
            self.page_turn = None;
            self.scroll_animation = None;
            self.swipe_preview.clear();
            self.request_current_pages(cx);
        }
        self.device_pixel_ratio = window.scale_factor();
        // Cleared before the page elements are built, so what they record in
        // paint describes this frame and not the last one.
        self.passage_bounds.set(None);
        if self.error.is_some() {
            self.page_turn = None;
            self.scroll_animation = None;
        }
        if self.advance_scroll_animation(cx) {
            window.request_animation_frame();
        }
        if self.pages.is_empty() && self.page_turn.is_none() {
            return match self.error.clone() {
                Some(message) => components::reader_document_message(message, theme).into_any_element(),
                None if self.session.is_none() => components::reader_document_message("Opening PDF…", theme).into_any_element(),
                None => components::reader_document_message("Rendering PDF page…", theme).into_any_element(),
            };
        }
        let view = cx.entity();
        let column_height = self.flow_column_height();
        let incoming = self.flow_snapshot(false);
        let viewport = size(px(self.available_width), px(column_height));
        let turn_frame = self.page_turn.as_mut().and_then(|turn| turn.frame(viewport, incoming.is_ready(), window, cx));
        if turn_frame.is_none() {
            self.page_turn = None;
        }
        let offset = turn_frame.as_ref().map_or_else(|| self.swipe_preview.offset(window, cx), |frame| frame.incoming);
        let mut document = div().relative().w_full().h(px(column_height)).flex_shrink_0();
        if let Some(frame) = &turn_frame
            && let Some(turn) = &self.page_turn
        {
            document = document.child(div().id("pdf-turn-outgoing").absolute().inset_0().child(turn.outgoing.element(frame.outgoing, theme.page_bg).bg(theme.page_bg)));
        }
        document = document.child(div().id("pdf-flow").size_full().when(turn_frame.as_ref().is_some_and(|frame| !frame.ready), |page| page.invisible()).child(incoming.element(offset, theme.page_bg).bg(theme.page_bg)));
        let surface_bounds = Rc::new(Cell::new(Bounds::default()));
        let measured_bounds = surface_bounds.clone();
        let measure_target = view.clone();
        let mouse_down_bounds = surface_bounds.clone();
        let mouse_down_target = view.clone();
        let mouse_move_bounds = surface_bounds.clone();
        let mouse_move_target = view.clone();
        let mouse_up_target = view.clone();
        let mouse_up_out_target = view.clone();
        let key_target = view.clone();
        let pinch_target = view.clone();
        let wheel_target = view;
        components::reader_pdf_scroll_surface(&self.scroll_handle, theme, cx)
            .track_focus(&self.focus)
            .key_context("PdfReaderDocument")
            .cursor_text()
            .on_prepaint(move |bounds, _, cx| {
                measured_bounds.set(bounds);
                let viewport_width = f32::from(bounds.size.width);
                let viewport_height = f32::from(bounds.size.height);
                if viewport_width > 0.0 && viewport_height > 0.0 {
                    measure_target.update(cx, |view, cx| view.set_available_size(viewport_width, viewport_height, cx));
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                mouse_down_target.update(cx, |view, cx| {
                    view.scroll_animation = None;
                    let Some(target) = view.interaction_target(event.position, mouse_down_bounds.get()) else { return };
                    view.focus.focus(window, cx);
                    window.prevent_default();
                    if let Some(id) = view.annotation_at(target) {
                        cx.emit(PdfViewEvent::AnnotationActivated { id, position: event.position });
                        cx.stop_propagation();
                        return;
                    }
                    let granularity = match event.click_count {
                        1 => SelectionGranularity::Character,
                        2 => SelectionGranularity::Word,
                        _ => SelectionGranularity::Paragraph,
                    };
                    let Some(anchor_source_index) = view.selection_anchor_source_index(target) else { return };
                    view.begin_selection(target.page_index, target.column.page_column_index(), target.page_point, anchor_source_index, granularity, cx);
                    view.tap_start = Some((event.position, Instant::now()));
                    cx.stop_propagation();
                });
            })
            .on_mouse_move(move |event, _, cx| {
                if !event.dragging() {
                    return;
                }
                mouse_move_target.update(cx, |view, cx| {
                    let Some(target) = view.interaction_target(event.position, mouse_move_bounds.get()) else { return };
                    if view.update_selection_to(target.page_index, target.column.page_column_index(), target.page_point, cx) {
                        cx.stop_propagation();
                    }
                });
            })
            .on_mouse_up(MouseButton::Left, move |event, _, cx| {
                mouse_up_target.update(cx, |view, cx| {
                    if view.selection_anchor.take().is_some() {
                        if let Some((start, started_at)) = view.tap_start.take() {
                            let moved = f32::from(event.position.x - start.x).abs() > 8.0 || f32::from(event.position.y - start.y).abs() > 8.0;
                            if !moved && event.click_count == 1 && started_at.elapsed() < Duration::from_millis(600) {
                                cx.emit(PdfViewEvent::PageTapped);
                            }
                        }
                        view.selection_anchor_source_index = None;
                        view.selection_active = None;
                        cx.stop_propagation();
                    }
                });
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                mouse_up_out_target.update(cx, |view, _| {
                    view.selection_anchor = None;
                    view.tap_start = None;
                    view.selection_anchor_source_index = None;
                    view.selection_active = None;
                });
            })
            .on_key_down(move |event, _, cx| {
                let modifiers = event.keystroke.modifiers;
                if (modifiers.control || modifiers.platform) && event.keystroke.key.as_str() == "c" {
                    key_target.update(cx, |view, cx| {
                        if view.copy_selection(cx) {
                            cx.stop_propagation();
                        }
                    });
                }
            })
            .on_scroll_wheel(move |event, window, cx| {
                let (vertical, horizontal) = match event.delta {
                    ScrollDelta::Pixels(delta) => (f32::from(delta.y), f32::from(delta.x)),
                    ScrollDelta::Lines(delta) => (delta.y * LINE_SCROLL_HEIGHT, delta.x * LINE_SCROLL_HEIGHT),
                    ScrollDelta::Pages(delta) => {
                        let view = wheel_target.read(cx);
                        let direction = view.flow_scroll_delta(delta);
                        (direction * view.flow_column_height(), 0.0)
                    }
                };
                let flow_delta = wheel_target.read(cx).flow_scroll_delta(point(horizontal, vertical));
                if window.is_page_scroll_swipe() {
                    let from_edge = components::in_bottom_gesture_edge(event.position, window);
                    wheel_target.update(cx, |view, cx| view.handle_page_scroll(gpui::point(horizontal, vertical), event.touch_phase, from_edge, window, cx));
                    cx.stop_propagation();
                    return;
                }
                if matches!(event.delta, ScrollDelta::Pages(_)) {
                    wheel_target.update(cx, |view, cx| {
                        view.touch_swipe.cancel();
                        if flow_delta < 0.0 {
                            view.next_page(cx);
                        } else if flow_delta > 0.0 {
                            view.previous_page(cx);
                        } else {
                            view.swipe_preview.settle();
                            cx.notify();
                        }
                    });
                    cx.stop_propagation();
                    return;
                }
                if wheel_target.read(cx).zoom_mode == PdfZoomMode::FitHeight {
                    if matches!(event.delta, ScrollDelta::Pixels(_)) || wheel_target.read(cx).touchpad.is_active() || matches!(event.delta, ScrollDelta::Lines(delta) if delta.x.abs() > delta.y.abs()) {
                        wheel_target.update(cx, |view, cx| view.handle_touchpad_scroll(event, cx));
                        cx.stop_propagation();
                        return;
                    }
                    let from_edge = components::in_bottom_gesture_edge(event.position, window);
                    wheel_target.update(cx, |view, cx| {
                        view.handle_page_scroll(gpui::point(horizontal, vertical), event.touch_phase, from_edge, window, cx);
                    });
                    cx.stop_propagation();
                    return;
                }
                if flow_delta != 0.0 {
                    wheel_target.update(cx, |view, cx| {
                        view.navigation_revision = view.navigation_revision.wrapping_add(1);
                        view.animate_flow_scroll(flow_delta, cx);
                    });
                    cx.stop_propagation();
                }
            })
            .on_pinch(move |event, _, cx| {
                pinch_target.update(cx, |view, cx| view.handle_pinch(event, cx));
                cx.stop_propagation();
            })
            .child(document)
            .into_any_element()
    }
}

fn normalized_bounds(rect: NormalizedRect, page: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds { origin: point(page.origin.x + page.size.width * rect.left(), page.origin.y + page.size.height * rect.top()), size: size(page.size.width * rect.width(), page.size.height * rect.height()) }
}

fn annotation_paint_rects(rect: NormalizedRect, style: PdfAnnotationStyle) -> SmallVec<[NormalizedRect; 16]> {
    let mut painted = SmallVec::new();
    match style {
        PdfAnnotationStyle::Highlight => painted.push(rect),
        PdfAnnotationStyle::Underline => {
            let height = (rect.height() * 0.12).max(0.0025).min(rect.height());
            painted.push(NormalizedRect::new(rect.left(), rect.bottom() - height, rect.width(), height));
        }
        PdfAnnotationStyle::Squiggly => {
            let height = (rect.height() * 0.12).max(0.0025).min(rect.height());
            let segment_count = ((rect.width() / (rect.height() * 0.35).max(0.008)).round() as usize).clamp(4, 24);
            let segment_width = rect.width() / segment_count as f32;
            for index in 0..segment_count {
                let offset = if index % 2 == 0 { height } else { 0.0 };
                painted.push(NormalizedRect::new(rect.left() + segment_width * index as f32, rect.bottom() - height - offset, segment_width, height));
            }
        }
        PdfAnnotationStyle::Strikethrough => {
            let height = (rect.height() * 0.1).max(0.0025).min(rect.height());
            painted.push(NormalizedRect::new(rect.left(), rect.top() + (rect.height() - height) * 0.52, rect.width(), height));
        }
    }
    painted
}

fn normalized_point(position: Point<Pixels>, page: Bounds<Pixels>) -> (f32, f32) {
    ((f32::from(position.x - page.origin.x) / f32::from(page.size.width).max(1.0)).clamp(0.0, 1.0), (f32::from(position.y - page.origin.y) / f32::from(page.size.height).max(1.0)).clamp(0.0, 1.0))
}

fn normalized_rect_contains(rect: NormalizedRect, point: (f32, f32)) -> bool {
    rect.left() <= point.0 && point.0 <= rect.right() && rect.top() <= point.1 && point.1 <= rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext};

    #[cfg(not(target_arch = "wasm32"))]
    mod navigation {
        include!("navigation_tests.rs");
    }

    pub(super) fn setup(cx: &mut TestAppContext) -> (Entity<PdfView>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        cx.add_window_view(|window, cx| {
            let mut view = PdfView::empty(0, window, cx);
            view.zoom_mode = PdfZoomMode::FitHeight;
            view.available_width = 1500.0;
            view.available_height = 800.0;
            let viewport = FitWidthViewport::new(view.page_display_width(), view.page_display_height()).unwrap();
            view.fit_width_plan = FitWidthPlan::fit_page((0..5).map(|index| FitWidthPage::new(index, 600.0, if index == 1 { 1600.0 } else { 400.0 }).unwrap()), viewport);
            view
        })
    }

    #[test]
    fn page_spacing_participates_in_all_zoom_modes_and_fits_small_viewports() {
        let fit = resolve_layout(PdfZoomMode::FitWidth { columns: 2 }, 1200.0, 800.0, (600.0, 800.0), 32.0);
        assert_eq!(fit.columns, 2);
        assert!((fit.column_width * 2.0 + 32.0 - 1200.0).abs() < 0.001);
        for mode in [PdfZoomMode::Fixed { zoom: 1.0 }, PdfZoomMode::FitHeight] {
            assert_eq!(resolve_layout(mode, 1200.0, 800.0, (600.0, 800.0), 0.0).columns, 2);
            assert_eq!(resolve_layout(mode, 1200.0, 800.0, (600.0, 800.0), 32.0).columns, 1);
        }
        let small = resolve_layout(PdfZoomMode::FitWidth { columns: 6 }, 120.0, 200.0, (600.0, 800.0), 200.0);
        let gap = effective_page_gap(6, 120.0, 200.0);
        assert!(small.column_width >= 1.0);
        assert!((small.column_width * 6.0 + gap * 5.0 - 120.0).abs() < 0.001);
        assert_eq!(effective_page_gap(1, 120.0, 200.0), 0.0);
    }

    #[test]
    fn document_colors_follow_light_and_dark_themes() {
        let mut theme = components::BrowserTheme::default();
        theme.page_bg = rgba(0xf0e6d2ff).into();
        assert_eq!(pdf_document_colors(theme), ([240, 230, 210], [0; 3]));
        theme.page_bg = rgba(0x202020ff).into();
        theme.text = rgba(0xe6ebf0ff).into();
        assert_eq!(pdf_document_colors(theme), ([32; 3], [230, 235, 240]));
    }

    #[gpui::test]
    fn cached_resolution_fallback_never_uses_another_paper_color(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update(cx, |view, _| {
            install_test_pages(view);
            let white = PageRenderKey::new(0, 640);
            view.display_cache.insert(white, view.pages[0].1.clone(), view.pages[0].1.raster.byte_len);
            assert!(!pdf_fallback_pages(&mut view.display_cache, PageRenderKey::new(0, 800)).is_empty());
            assert!(pdf_fallback_pages(&mut view.display_cache, white.with_paper_color([240, 230, 210])).is_empty());
            let dark = white.with_paper_color([32; 3]).with_ink_color([230, 235, 240]);
            view.display_cache.insert(dark, view.pages[0].1.clone(), view.pages[0].1.raster.byte_len);
            assert!(!pdf_fallback_pages(&mut view.display_cache, dark).is_empty());
            assert!(pdf_fallback_pages(&mut view.display_cache, dark.with_ink_color([255; 3])).is_empty());
            let tile = PageRenderKey::new(1, 640).with_tile(0);
            view.display_cache.insert(tile, view.pages[0].1.clone(), view.pages[0].1.raster.byte_len);
            view.display_cache.insert(tile.with_tile(1), view.pages[0].1.clone(), view.pages[0].1.raster.byte_len);
            assert_eq!(pdf_fallback_pages(&mut view.display_cache, PageRenderKey::new(1, 800).with_tile(0)).len(), 2, "a new tile index cannot stand in for its old-resolution counterpart");
            assert!(pdf_fallback_pages(&mut view.display_cache, PageRenderKey::new(1, 800).with_tile(0).with_paper_color([240, 230, 210])).is_empty());
        });
    }

    pub(super) fn install_test_pages(view: &mut PdfView) {
        let page = Arc::new(DisplayPage { raster: PageImage::from_bgra(RgbaImage::new(1, 1)), text_layout: Arc::new(PdfPageTextLayout::default()), ruler_lines: Arc::from([]), raster_crop: PixelCrop::full(1, 1), page_crop: NormalizedRect::new(0.0, 0.0, 1.0, 1.0) });
        view.pages = (0..5).map(|index| (index, page.clone())).collect();
    }

    #[gpui::test]
    fn pinch_zooms_from_fit_height_and_rejects_invalid_deltas(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update(cx, |view, cx| {
            let initial = view.zoom();
            let source = view.source_position;
            let pinch = |delta, phase| PinchEvent { delta, phase, ..Default::default() };
            view.handle_pinch(&pinch(0.0, TouchPhase::Started), cx);
            assert_eq!(view.zoom_mode, PdfZoomMode::FitHeight, "finger down alone must not change the fit mode");
            view.handle_pinch(&pinch(-0.25, TouchPhase::Moved), cx);
            assert!((view.zoom() - initial * 0.75).abs() < 0.001);
            assert!(matches!(view.zoom_mode, PdfZoomMode::Fixed { .. }));
            let zoom = view.zoom();
            for (delta, phase) in [(f32::NAN, TouchPhase::Moved), (f32::INFINITY, TouchPhase::Moved), (-1.0, TouchPhase::Moved), (0.5, TouchPhase::Cancelled)] {
                view.handle_pinch(&pinch(delta, phase), cx);
                assert_eq!(view.zoom(), zoom);
            }
            view.handle_pinch(&pinch(0.25, TouchPhase::Moved), cx);
            assert!((view.zoom() - zoom * 1.25).abs() < 0.001);
            view.handle_pinch(&pinch(1000.0, TouchPhase::Moved), cx);
            assert_eq!(view.zoom(), view.width_fit_zoom(MIN_VISIBLE_PAGE_COUNT));
            view.handle_pinch(&pinch(-0.9999, TouchPhase::Moved), cx);
            assert_eq!(view.zoom(), MIN_ZOOM);
            assert_eq!(view.source_position, source, "zoom preserves the document position");
        });
    }

    #[gpui::test]
    fn touchpad_turns_once_per_burst_and_respects_reading_direction(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update(cx, |view, cx| {
            let scroll = |x, y, touch_phase| ScrollWheelEvent { delta: ScrollDelta::Pixels(point(px(x), px(y))), touch_phase, ..Default::default() };
            install_test_pages(view);
            view.handle_touchpad_scroll(&scroll(-20.0, 0.0, TouchPhase::Started), cx);
            assert_eq!(view.flow_offset, 0.0);
            view.handle_touchpad_scroll(&scroll(-60.0, 0.0, TouchPhase::Moved), cx);
            assert_eq!(view.flow_offset, 0.0, "phased gestures wait for release");
            view.handle_touchpad_scroll(&scroll(0.0, 0.0, TouchPhase::Ended), cx);
            assert_eq!(view.flow_offset, 800.0);
            view.handle_touchpad_scroll(&scroll(-160.0, 0.0, TouchPhase::Moved), cx);
            assert_eq!(view.flow_offset, 800.0, "momentum cannot skip another page");
            install_test_pages(view);
            view.handle_touchpad_scroll(&scroll(60.0, 0.0, TouchPhase::Started), cx);
            view.handle_touchpad_scroll(&scroll(0.0, 0.0, TouchPhase::Ended), cx);
            assert_eq!(view.flow_offset, 0.0);
            view.right_to_left = true;
            assert_eq!(view.flow_scroll_delta(point(60.0, -2.0)), -60.0, "minor vertical drift must not reverse a horizontal RTL gesture");
            install_test_pages(view);
            view.handle_touchpad_scroll(&scroll(60.0, 0.0, TouchPhase::Started), cx);
            view.handle_touchpad_scroll(&scroll(0.0, 0.0, TouchPhase::Ended), cx);
            assert_eq!(view.flow_offset, 800.0, "horizontal navigation follows RTL");
            install_test_pages(view);
            view.handle_touchpad_scroll(&scroll(0.0, -60.0, TouchPhase::Started), cx);
            view.handle_touchpad_scroll(&scroll(0.0, 0.0, TouchPhase::Ended), cx);
            assert_eq!(view.flow_offset, 1600.0, "vertical navigation keeps document order");
            view.handle_touchpad_scroll(&scroll(10.0, 0.0, TouchPhase::Started), cx);
            view.handle_touchpad_scroll(&scroll(60.0, 0.0, TouchPhase::Cancelled), cx);
            view.handle_touchpad_scroll(&scroll(60.0, 0.0, TouchPhase::Ended), cx);
            assert_eq!(view.flow_offset, 1600.0);
        });
    }

    #[gpui::test]
    fn swipes_turn_once_and_leave_system_edge_gestures_and_cancellation_alone(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                install_test_pages(view);
                // An active swipe also routes through the touch path on desktop.
                view.touch_swipe.begin(false);
                view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Started, false, window, cx);
                view.handle_page_scroll(point(0.0, -160.0), TouchPhase::Moved, false, window, cx);
                view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Ended, false, window, cx);
                assert_eq!(view.flow_offset, 800.0);
                view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Ended, false, window, cx);
                assert_eq!(view.flow_offset, 800.0, "release cannot turn twice");

                view.touch_swipe.begin(false);
                view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Started, true, window, cx);
                view.handle_page_scroll(point(0.0, -160.0), TouchPhase::Moved, false, window, cx);
                view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Ended, false, window, cx);
                assert_eq!(view.flow_offset, 800.0);

                view.touch_swipe.begin(false);
                view.handle_page_scroll(point(0.0, -160.0), TouchPhase::Started, false, window, cx);
                view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Cancelled, false, window, cx);
                view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Ended, false, window, cx);
                assert_eq!(view.flow_offset, 800.0);
            });
        });
    }

    #[gpui::test]
    fn page_turn_retains_images_and_waits_for_the_destination(cx: &mut TestAppContext) {
        let (view, visual) = setup(cx);
        visual.update(|window, cx| {
            view.update(cx, |view, cx| {
                install_test_pages(view);
                let image = view.pages[0].1.raster.image.clone();
                view.swipe_preview.drag(point(0.0, -120.0), window);
                view.next_page(cx);
                assert_eq!(view.flow_offset, 800.0);
                if !PageTurn::<PdfFlowSnapshot>::enabled(cx) {
                    assert!(view.page_turn.is_none());
                    return;
                }
                let incoming = view.flow_snapshot(false);
                assert!(!incoming.is_ready());
                let turn = view.page_turn.as_mut().expect("keyboard, tap and swipe navigation share a transition");
                let outgoing = turn.outgoing.slots[0].streams[0][0].page.as_ref().unwrap();
                assert!(Arc::ptr_eq(&outgoing.page.raster.image, &image), "keep the existing raster rather than copying it");
                assert!(!Rc::ptr_eq(&outgoing.passage_bounds, &view.passage_bounds), "outgoing paint is passive");
                let viewport = size(px(view.available_width), px(800.0));
                let frame = turn.frame(viewport, false, window, cx).unwrap();
                assert!(!frame.ready);
                assert_eq!(frame.outgoing, point(px(0.0), px(-120.0)));
                install_test_pages(view);
                assert!(view.flow_snapshot(false).is_ready());
            });
        });
    }

    #[gpui::test]
    fn boundary_turns_do_not_animate_and_repeated_turns_keep_navigation_order(cx: &mut TestAppContext) {
        let (view, visual) = setup(cx);
        view.update(visual, |view, cx| {
            install_test_pages(view);
            view.previous_page(cx);
            assert!(view.page_turn.is_none());
            assert_eq!(view.flow_offset, 0.0);
            for expected in [800.0, 1600.0, 2400.0] {
                install_test_pages(view);
                view.next_page(cx);
                assert_eq!(view.flow_offset, expected);
            }
            install_test_pages(view);
            view.previous_page(cx);
            assert_eq!(view.flow_offset, 1600.0);
        });
    }

    #[gpui::test]
    fn fit_height_navigation_keeps_whole_pages_even_when_pages_are_short(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update(cx, |view, cx| {
            assert_eq!(view.flow_column_height(), 800.0);
            let column = view.fit_width_plan.columns()[0];
            assert_eq!(view.flow_offset_for_column(column, 0.7), 0.0);
            view.next_item(cx);
            assert_eq!(view.flow_offset, 800.0);
            assert_eq!(view.flow_remainder(), 0.0);
            assert_eq!(view.visible_page_range(), Some((1, 2)));
            view.previous_item(cx);
            assert_eq!(view.flow_offset, 0.0);
            view.previous_page(cx);
            assert_eq!(view.flow_offset, 0.0);
            for _ in 0..8 {
                view.next_page(cx);
            }
            assert_eq!(view.visible_page_range(), Some((3, 4)));
            assert_eq!(view.flow_remainder(), 0.0);
        });
    }

    #[gpui::test]
    fn page_gaps_and_hit_testing_follow_actual_page_widths(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update(cx, |view, _| {
            view.page_gap = 32.0;
            let slots = page_slots(view.available_width, &view.screen_column_widths(), false, view.page_gap);
            let gap = slots[0].0;
            assert!(gap > 0.0);
            assert!((slots[1].0 - (slots[0].0 + slots[0].1) - view.page_gap).abs() < 0.001);
            assert!((view.available_width - (slots[1].0 + slots[1].1) - gap).abs() < 0.001);
            let surface = Bounds { origin: point(px(30.0), px(40.0)), size: size(px(view.available_width), px(view.available_height)) };
            for (index, &(left, width)) in slots.iter().enumerate() {
                let position = point(surface.origin.x + px(left + width * 0.5), surface.origin.y + px(400.0));
                let target = view.interaction_target(position, surface).unwrap();
                assert_eq!(target.page_index, index);
                assert!((target.page_point.0 - 0.5).abs() < 0.001);
                assert!((target.page_point.1 - 0.5).abs() < 0.001);
            }
            assert!(view.interaction_target(point(surface.origin.x + px(gap * 0.5), surface.origin.y + px(400.0)), surface).is_none());
            let between = slots[0].0 + slots[0].1 + view.page_gap * 0.5;
            assert!(view.interaction_target(point(surface.origin.x + px(between), surface.origin.y + px(400.0)), surface).is_none());
            let single_page = page_slots(1500.0, &[600.0], false, 32.0);
            assert_eq!(single_page.as_slice(), &[(450.0, 600.0)]);
        });
    }

    #[gpui::test]
    fn rtl_spreads_mirror_hit_targets_and_keep_source_positions_and_page_numbers(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        view.update(cx, |view, _| {
            let source = view.source_position;
            let position = view.current_page_position();
            let range = view.visible_page_range();
            let widths = view.screen_column_widths();
            assert!(widths.len() >= 2);
            assert_ne!(widths[0], widths[1], "include mixed page widths");
            let ltr = page_slots(view.available_width, &widths, false, view.page_gap);
            view.right_to_left = true;
            let rtl = page_slots(view.available_width, &widths, view.right_to_left(), view.page_gap);
            assert!(rtl[0].0 > rtl[1].0);
            assert!(view.flow_snapshot(false).right_to_left);
            let surface = Bounds { origin: point(px(30.0), px(40.0)), size: size(px(view.available_width), px(view.available_height)) };
            for (index, &(left, width)) in rtl.iter().enumerate() {
                assert!((left - (view.available_width - ltr[index].0 - width)).abs() < 0.001);
                let target = view.interaction_target(point(surface.origin.x + px(left + width * 0.5), surface.origin.y + px(400.0)), surface).unwrap();
                assert_eq!(target.page_index, index);
                assert!((target.page_point.0 - 0.5).abs() < 0.001);
                assert!((target.page_point.1 - 0.5).abs() < 0.001);
            }
            assert_eq!(view.source_position, source);
            assert_eq!(view.current_page_position(), position);
            assert_eq!(view.visible_page_range(), range);
            view.right_to_left = false;
            assert_eq!(view.source_position, source);
        });
    }

    #[gpui::test]
    fn rtl_horizontal_controls_and_swipes_reverse_while_vertical_navigation_advances(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.right_to_left = true;
                view.horizontal_page(false, cx);
                assert_eq!(view.flow_offset, 800.0, "left advances in RTL");
                view.horizontal_page(true, cx);
                assert_eq!(view.flow_offset, 0.0);
                view.next_page(cx);
                assert_eq!(view.flow_offset, 800.0, "semantic next remains forward");
                view.previous_page(cx);
                for (travel, expected) in [(point(180.0, 0.0), 800.0), (point(-180.0, 0.0), 0.0), (point(0.0, -180.0), 800.0), (point(0.0, 180.0), 0.0)] {
                    install_test_pages(view);
                    view.touch_swipe.begin(false);
                    view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Started, false, window, cx);
                    view.handle_page_scroll(travel, TouchPhase::Moved, false, window, cx);
                    view.handle_page_scroll(point(0.0, 0.0), TouchPhase::Ended, false, window, cx);
                    assert_eq!(view.flow_offset, expected);
                    let viewport = size(px(view.available_width), px(view.flow_column_height()));
                    let frame = view.page_turn.as_mut().unwrap().frame(viewport, false, window, cx).unwrap();
                    assert_eq!(frame.outgoing, point(px(travel.x), px(travel.y)), "animation follows the finger in either direction");
                }
                view.handle_page_scroll(point(180.0, 0.0), TouchPhase::Moved, false, window, cx);
                view.finish_fit_height_scroll(cx);
                assert_eq!(view.flow_offset, 800.0, "horizontal wheel follows RTL too");
                view.handle_page_scroll(point(0.0, -180.0), TouchPhase::Moved, false, window, cx);
                view.finish_fit_height_scroll(cx);
                assert_eq!(view.flow_offset, 1600.0, "vertical wheel remains forward");
            })
        });
    }

    #[gpui::test]
    fn a_wheel_burst_turns_one_page_and_fit_height_ignores_zoom_shortcuts(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.change_zoom(0.1, cx);
                assert_eq!(view.zoom_mode, PdfZoomMode::FitHeight);
                for _ in 0..5 {
                    view.handle_page_scroll(gpui::point(0.0, -40.0), TouchPhase::Moved, false, window, cx);
                }
                assert_eq!(view.flow_offset, 0.0);
            })
        });
        cx.run_until_parked();
        cx.executor().advance_clock(WHEEL_SCROLL_END_DELAY);
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.flow_offset, 800.0);
                assert_eq!(view.flow_remainder(), 0.0);
                view.handle_page_scroll(gpui::point(0.0, 40.0), TouchPhase::Moved, false, window, cx);
            })
        });
        cx.run_until_parked();
        cx.executor().advance_clock(WHEEL_SCROLL_END_DELAY);
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(view.flow_offset, 0.0));
    }

    fn setup_fit_width(cx: &mut TestAppContext) -> (Entity<PdfView>, &mut VisualTestContext) {
        let (view, cx) = setup(cx);
        view.update(cx, |view, _| {
            view.zoom_mode = PdfZoomMode::FitWidth { columns: 2 };
            view.available_width = 1200.0;
            let viewport = FitWidthViewport::new(600.0, 800.0).unwrap();
            let page = FitWidthPage::new(0, 600.0, 2400.0).unwrap();
            let bands = vec![(0.32, 0.35), (0.42, 0.45), (0.65, 0.68), (0.75, 0.78)];
            let pager = FitWidthAnchorPager::new([(page, bands)], viewport, 2, FitWidthAnchor::new(0, 0.0)).unwrap();
            view.fit_width_plan = pager.window().plan().clone();
            view.viewport_pager = Some(pager);
        });
        (view, cx)
    }

    #[gpui::test]
    fn restored_fit_width_position_repacks_columns_without_a_scroll(cx: &mut TestAppContext) {
        let (view, cx) = setup_fit_width(cx);
        view.update(cx, |view, cx| {
            view.restore_page_position(0.1);
            assert_eq!(view.flow_offset, 0.0);
            assert_eq!(view.flow_remainder(), 0.0);
            assert_eq!(view.fit_width_plan.columns()[0].page_rect().top(), 0.1);
            assert!((view.fit_width_plan.columns()[0].display_height() - 768.0).abs() < 0.001);
            let pager = view.viewport_pager.as_ref().unwrap();
            assert_eq!(view.fit_width_plan, pager.reflow_at(0.0).unwrap());

            view.sync_flow_scroll_target(-16.0, cx);
            let position = view.current_page_position();
            let plan = view.fit_width_plan.clone();
            // Cropping coordinates can round slightly on their round trip.
            view.restore_page_position(position + f32::EPSILON * 0.25);
            assert_eq!(view.fit_width_plan, plan);
            assert!((view.current_page_position() - position).abs() < 0.00001);
            assert_eq!(view.flow_offset, 0.0);
        });
    }

    #[gpui::test]
    fn fit_width_line_navigation_repacks_from_its_target(cx: &mut TestAppContext) {
        let (view, cx) = setup_fit_width(cx);
        view.update(cx, |view, cx| {
            let offsets = view.atomic_flow_offsets();
            let target = next_snap_offset(view.flow_offset, &offsets).unwrap();
            let anchor = view.flow_anchor_at(target).unwrap();
            view.next_item(cx);
            if let Some(animation) = &mut view.scroll_animation {
                animation.started_at = Instant::now() - SMOOTH_SCROLL_DURATION / 2;
                assert!(view.advance_scroll_animation(cx));
                let intermediate = view.flow_anchor_at(0.0).unwrap();
                assert!(intermediate.page_y() > 0.0 && intermediate.page_y() < anchor.page_y());
            }
            view.finish_scroll_animation(cx);
            assert_eq!(view.flow_offset, 0.0);
            assert!(view.fit_width_plan.columns()[0].page_rect().top() > 0.0);
            assert_eq!(view.fit_width_plan, view.viewport_pager.as_ref().unwrap().reflow_at(0.0).unwrap());
            assert_eq!(view.flow_anchor_at(0.0), Some(anchor));
        });
    }

    #[gpui::test]
    fn fit_width_page_turns_within_one_pdf_page_animate_vertically(cx: &mut TestAppContext) {
        let (view, visual) = setup_fit_width(cx);
        visual.update(|window, cx| {
            view.update(cx, |view, cx| {
                install_test_pages(view);
                let viewport = size(px(view.available_width), px(view.flow_column_height()));
                for forward in [true, false] {
                    if forward {
                        view.next_page(cx)
                    } else {
                        view.previous_page(cx)
                    }
                    if !PageTurn::<PdfFlowSnapshot>::enabled(cx) {
                        assert!(view.page_turn.is_none());
                        continue;
                    }
                    assert_eq!(view.current_page(), 0, "both viewports belong to the same source page");
                    let turn = view.page_turn.as_mut().expect("source position changes must animate even within one PDF page");
                    let frame = turn.frame(viewport, false, window, cx).unwrap();
                    assert_eq!(frame.outgoing.x, Pixels::ZERO);
                    assert_eq!(frame.incoming.x, Pixels::ZERO);
                    assert_eq!(frame.outgoing.y - frame.incoming.y, viewport.height * if forward { -1.0 } else { 1.0 });
                    install_test_pages(view);
                }
                assert_eq!(view.page_drag(point(-100.0, 2.0)), point(0.0, -100.0));
            });
        });
    }

    #[gpui::test]
    fn smooth_fit_width_scrolls_accumulate_reverse_and_finish_at_the_requested_position(cx: &mut TestAppContext) {
        let (view, visual) = setup_fit_width(cx);
        visual.update(|_, cx| {
            view.update(cx, |view, cx| {
                if !PageTurn::<PdfFlowSnapshot>::enabled(cx) {
                    return;
                }
                view.animate_flow_scroll(-80.0, cx);
                assert_eq!(view.current_page_position(), 0.0, "scrolling begins on the next animation frame");
                view.scroll_animation.as_mut().unwrap().started_at = Instant::now() - SMOOTH_SCROLL_DURATION / 2;
                assert!(view.advance_scroll_animation(cx));
                assert!(view.current_page_position() > 0.0 && view.current_page_position() * 2400.0 < 80.0);
                view.animate_flow_scroll(-40.0, cx);
                view.animate_flow_scroll(20.0, cx);
                view.scroll_animation.as_mut().unwrap().started_at = Instant::now() - SMOOTH_SCROLL_DURATION;
                assert!(!view.advance_scroll_animation(cx));
                assert!(view.scroll_animation.is_none());
                assert!((view.current_page_position() * 2400.0 - 100.0).abs() < 0.01, "retargeting preserves the complete signed wheel distance");
                assert_eq!(view.flow_offset, 0.0);
                assert_eq!(view.fit_width_plan, view.viewport_pager.as_ref().unwrap().reflow_at(0.0).unwrap());

                view.animate_flow_scroll(-30.0, cx);
                cx.set_reduce_motion(true);
                assert!(!view.advance_scroll_animation(cx));
                assert!(view.scroll_animation.is_none());
                assert!((view.current_page_position() * 2400.0 - 130.0).abs() < 0.01);
                view.animate_flow_scroll(10.0, cx);
                assert!(view.scroll_animation.is_none());
                assert!((view.current_page_position() * 2400.0 - 120.0).abs() < 0.01);
            });
        });
    }

    #[gpui::test]
    fn scrolling_line_keys_and_page_keys_share_one_position(cx: &mut TestAppContext) {
        let (view, cx) = setup_fit_width(cx);
        view.update(cx, |view, cx| {
            view.next_item(cx);
            view.finish_scroll_animation(cx);
            let next = view.source_position;
            assert!(next.page_y() > 0.0);
            view.previous_item(cx);
            view.finish_scroll_animation(cx);
            assert_eq!(view.source_position, PdfSourcePosition::new(0, 0.0));
            view.sync_flow_scroll_target(-127.0, cx);
            let scrolled = view.source_position;
            assert!((scrolled.page_y() * 2400.0 - 127.0).abs() < 0.01);
            view.next_page(cx);
            view.previous_page(cx);
            assert_eq!(view.source_position, scrolled);
        });
    }

    #[gpui::test]
    fn position_events_include_scrolls_inside_the_same_pdf_page(cx: &mut TestAppContext) {
        let (view, cx) = setup_fit_width(cx);
        let events = cx.new(|cx| {
            cx.subscribe(&view, |positions: &mut Vec<PdfSourcePosition>, _, event, _| {
                if let PdfViewEvent::PositionChanged { position, .. } = event {
                    positions.push(*position);
                }
            })
            .detach();
            Vec::new()
        });
        view.update(cx, |view, cx| {
            view.sync_flow_scroll_target(-16.0, cx);
            view.sync_flow_scroll_target(-16.0, cx);
        });
        events.read_with(cx, |positions, _| {
            assert_eq!(positions.len(), 2);
            assert_eq!(positions[0].page_index(), positions[1].page_index());
            assert!((positions[0].page_y() * 2400.0 - 16.0).abs() < 0.001);
            assert!((positions[1].page_y() * 2400.0 - 32.0).abs() < 0.001);
        });
    }

    #[gpui::test]
    #[cfg(not(target_arch = "wasm32"))]
    async fn resize_geometry_and_raster_completion_preserve_the_original_pdf_position(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let session = PdfDocumentSession::from_bytes("position-transitions", include_bytes!("../../../../../../shared/pdf-reader-core/tests/fixtures/cover-annotations.pdf").to_vec()).unwrap();
        let info = session.info().clone();
        let width = info.page(0).unwrap().width_points();
        let key = PageRenderKey::new(0, 640).with_tile(0);
        let raster = session.render_for_display(key).await.unwrap();
        let (view, cx) = setup(cx);
        view.update(cx, |view, cx| {
            view.info = Some(info);
            view.trim_margins = false;
            view.zoom_mode = PdfZoomMode::Fixed { zoom: 1.0 };
            view.available_width = width * 2.0;
            view.available_height = 200.0;
            view.source_position = PdfSourcePosition::new(0, 0.2);
            view.rebuild_fit_width_plan();
            assert_eq!(view.visible_page_count(), 2);
            let original = view.source_position;
            view.set_available_size(width * 3.0, 200.0, cx);
            assert_eq!(view.visible_page_count(), 3);
            assert_eq!(view.viewport_pager.as_ref().unwrap().window().start().page_y(), 0.2);
            assert_eq!(view.source_position, original);

            // Analysis may complete after a resize. Its crop changes the local
            // layout, while the source anchor remains in original PDF space.
            view.page_geometry.insert(PageGeometryKey { page_index: 0, trim_margins: false }, PdfPageGeometry { crop: NormalizedRect::new(0.1, 0.1, 0.8, 0.8), break_avoidance: Arc::from([]) });
            view.geometry_revision += 1;
            view.rebuild_fit_width_plan();
            assert_eq!(view.source_position, original);
            assert!((view.fit_width_plan.columns()[0].page_rect().top() - 0.125).abs() < 0.00001);
            let layout = view.fit_width_plan.clone();
            let generation = view.render_generation;
            assert!(view.apply_rendered_page(generation, key, Ok(raster), cx));
            assert_eq!(view.source_position, original);
            assert_eq!(view.fit_width_plan, layout, "raster completion cannot repaginate");

            view.set_page(0, cx);
            assert_eq!(view.fit_width_plan.columns()[0].page_rect().top(), 0.0, "a TOC jump to the current source page returns to its top");
            assert!((view.source_position.page_y() - 0.1).abs() < 0.00001);
        });
    }

    #[gpui::test]
    #[cfg(not(target_arch = "wasm32"))]
    async fn loading_follows_each_pdf_direction_and_preserves_the_reading_position(cx: &mut TestAppContext) {
        struct DirectionLoadTest(Entity<PdfView>);
        impl Render for DirectionLoadTest {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }

        cx.executor().allow_parking();
        cx.update(gpui_component::init);
        let fixture = String::from_utf8(include_bytes!("../../../../../../shared/pdf-reader-core/tests/fixtures/rtl-viewer-preferences.pdf").to_vec()).unwrap();
        for (name, expected) in [("/R2L", true), ("/L2R", false), ("/Foo", false)] {
            let bytes = fixture.replace("/R2L", name).into_bytes();
            let (holder, visual) = cx.add_window_view(|window, cx| {
                // Metadata loading is independent of painting a full viewport.
                DirectionLoadTest(cx.new(|cx| {
                    let mut view = PdfView::from_bytes("document-direction".into(), bytes, 0, window, cx);
                    view.set_available_size(64.0, 64.0, cx);
                    view.set_trim_margins(false, cx);
                    assert!(!view.right_to_left());
                    view.set_full_page_position(0.3, cx);
                    view
                }))
            });
            let view = holder.read_with(visual, |holder, _| holder.0.clone());
            visual
                .condition(&view, |view, _| {
                    assert!(view.error.is_none(), "{:?}", view.error);
                    view.session.is_some()
                })
                .await;
            view.read_with(visual, |view, _| {
                assert_eq!(view.right_to_left(), expected, "{name}");
                assert_eq!(view.source_position.page_y(), 0.3);
            });
            visual.update(|window, _| window.remove_window());
        }
    }

    #[gpui::test]
    #[cfg(not(target_arch = "wasm32"))]
    async fn loading_geometry_tiles_and_restoring_a_position_use_the_same_pdf_anchor(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(gpui_component::init);
        let bytes = include_bytes!("../../../../../../shared/pdf-reader-core/tests/fixtures/cover-annotations.pdf").to_vec();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = PdfView::from_bytes("async-position".into(), bytes.clone(), 0, window, cx);
            view.set_available_size(900.0, 500.0, cx);
            view.set_full_page_position(0.3, cx);
            view
        });
        cx.condition(&view, |view, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            view.session.is_some() && !view.geometry_loading && !view.rendering && view.flow_snapshot(false).is_ready()
        })
        .await;
        view.update(cx, |view, cx| {
            assert_eq!(view.source_position.page_y(), 0.3);
            assert!(view.visible_render_keys().iter().all(|key| key.tile().is_some()));
            assert!(view.display_cache.byte_len() <= DISPLAY_CACHE_BUDGET);
            view.sync_flow_scroll_target(-16.0, cx);
        });
        cx.condition(&view, |view, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            !view.geometry_loading && !view.rendering && view.flow_snapshot(false).is_ready()
        })
        .await;
        let saved = view.read_with(cx, |view, _| view.source_position);
        let restored = cx.update(|window, cx| {
            cx.new(|cx| {
                let mut view = PdfView::from_bytes("restored-position".into(), bytes, saved.page_index(), window, cx);
                view.set_available_size(1200.0, 600.0, cx);
                view.set_visible_page_count(2, cx);
                view.set_full_page_position(saved.page_y(), cx);
                view
            })
        });
        cx.condition(&restored, |view, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            view.session.is_some() && !view.geometry_loading && !view.rendering && view.flow_snapshot(false).is_ready()
        })
        .await;
        restored.read_with(cx, |view, _| assert_eq!(view.source_position, saved));
    }

    #[gpui::test]
    #[cfg(not(target_arch = "wasm32"))]
    async fn switching_to_fit_width_and_following_speech_keep_padding_at_column_ends(cx: &mut TestAppContext) {
        // PDFium runs on its own OS thread, outside GPUI's test scheduler.
        cx.executor().allow_parking();
        let session = PdfDocumentSession::from_bytes("fit-width-regression", include_bytes!("../../../../../../shared/pdf-reader-core/tests/fixtures/cover-annotations.pdf").to_vec()).unwrap();
        let info = session.info().clone();
        let key = PageRenderKey::with_margin_trim(0, 640, false);
        let page = DisplayPage::from_rendered(session.render_for_display(key).await.unwrap()).unwrap();
        let page = Arc::new(page);
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = PdfView::empty(0, window, cx);
            view.info = Some(info);
            view.zoom_mode = PdfZoomMode::FitHeight;
            view.available_width = 1200.0;
            view.available_height = 400.0;
            view.trim_margins = false;
            view.page_geometry.insert(PageGeometryKey { page_index: 0, trim_margins: false }, PdfPageGeometry { crop: page.page_crop, break_avoidance: Arc::from([(0.42, 0.46), (0.62, 0.66), (0.72, 0.76)]) });
            view.pages = vec![(0, page.clone())];
            view.display_cache.insert(key, page.clone(), page.raster.byte_len);
            view.rebuild_fit_width_plan();
            view
        });
        view.update(cx, |view, cx| {
            view.device_pixel_ratio = 1.0;
            view.set_available_size(1200.0, 400.0, cx);
            view.set_zoom_mode(PdfZoomMode::FitWidth { columns: 2 }, cx);
            assert_eq!(view.flow_offset, 0.0);
            view.set_full_page_position(0.2, cx);
            assert_eq!(view.flow_offset, 0.0);
            assert!((view.fit_width_plan.columns()[0].page_rect().top() - 0.2).abs() < 0.001);
            assert!(
                view.fit_width_plan.columns()[0].display_height() < view.page_display_height(),
                "column {:?}, viewport {:?}, bands {:?}, keys {:?}",
                view.fit_width_plan.columns()[0],
                (view.available_width, view.available_height),
                view.page_geometry.values().collect::<Vec<_>>(),
                view.visible_render_keys()
            );

            let revision = view.navigation_revision;
            view.follow_reading_fragment(0, vec![NormalizedRect::new(0.1, 0.3, 0.4, 0.02)], cx);
            assert_eq!(view.navigation_revision, revision);
            assert_eq!(view.flow_offset, 0.0);
            assert!((view.fit_width_plan.columns()[0].page_rect().top() - 0.3).abs() < 0.001);
            assert!(view.fit_width_plan.columns()[0].display_height() < view.page_display_height());
            assert_eq!(view.fit_width_plan, view.viewport_pager.as_ref().unwrap().reflow_at(0.0).unwrap());
        });
    }
}
