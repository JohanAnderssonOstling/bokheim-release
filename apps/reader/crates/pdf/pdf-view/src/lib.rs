mod session;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, ContentMask, Context, Corners, Element, ElementId, EventEmitter, FocusHandle, GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseButton, Pixels, Point, Render, RenderImage,
    ScrollDelta, ScrollHandle, Style, Window, div, fill, point, px, rgba, size,
};
use gpui_component::ElementExt;
use image::{Frame, RgbaImage};
use pdf_reader_core::{
    FitWidthAnchor, FitWidthAnchorPager, FitWidthColumn, FitWidthPage, FitWidthPlan, FitWidthViewport, NormalizedRect, PageRenderKey, PdfDocumentInfo, PdfDocumentSession, PdfPageTextLayout, PdfResult, PdfSearchHit, PdfTextSelection,
    PixelCrop, ReaderInteractionStyle, SelectionGranularity, SelectionOptions, crop_normalized_rect,
};
use smallvec::SmallVec;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;
use ui_components as components;

const DISPLAY_CACHE_BUDGET: usize = 128 * 1024 * 1024;
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
const PDF_PAGE_BOUNDARY_GAP: f32 = 1.0;
const DEFAULT_AVAILABLE_WIDTH: f32 = 948.0;
const DEFAULT_AVAILABLE_HEIGHT: f32 = 800.0;
/// Margin trimming is rasterized, so floor/ceil rounding shifts its measured
/// PDF-point dimensions slightly between render-width buckets. Keep those
/// sub-pixel changes out of the document layout while a window is resizing.
const LAYOUT_SIZE_JITTER_TOLERANCE_POINTS: f32 = 4.0;
/// Same tolerance in normalized display coordinates for line and image bands.
const LAYOUT_BAND_JITTER_TOLERANCE: f32 = 0.01;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FitWidthCursor {
    page_index: usize,
    page_column_index: u32,
}

#[derive(Clone, Debug)]
pub enum PdfViewEvent {
    PageChanged { current: usize, total: usize },
    SearchChanged { current: usize, total: usize },
    AnnotationActivated { id: String, position: Point<Pixels> },
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

pub struct PdfView {
    session: Option<session::Session>,
    info: Option<PdfDocumentInfo>,
    pages: Vec<(usize, Arc<DisplayPage>)>,
    display_cache: DisplayPageCache,
    effective_page_sizes: HashMap<(usize, bool), (f32, f32)>,
    page_break_avoidance: HashMap<(usize, bool), Arc<[(f32, f32)]>>,
    fit_width_plan: FitWidthPlan,
    /// Viewport-local fit-width pagination. The plan above is always the
    /// pager's current window, whose display index zero is `start_anchor`.
    viewport_pager: Option<FitWidthAnchorPager>,
    plan_key: Option<(f32, f32, bool, bool, u64)>,
    geometry_revision: u64,
    navigation_offsets: RefCell<Option<(f32, f32, Arc<[f32]>)>>,
    /// Filled by the page elements as they paint; see [`Self::passage_bounds`].
    passage_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    #[cfg(test)]
    plan_builds: usize,
    #[cfg(test)]
    raster_requests: usize,
    current_page: usize,
    current_page_column: u32,
    /// A direct jump can reveal a page excluded from ordinary reading flow.
    revealed_page: Option<usize>,
    zoom_mode: PdfZoomMode,
    /// Device pixels per logical pixel, sampled from the window each frame so
    /// pages are rastered at the resolution they are actually painted at.
    device_pixel_ratio: f32,
    available_width: f32,
    available_height: f32,
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
    active_annotation_id: Option<Arc<str>>,
    search_hits: Vec<PdfSearchHit>,
    search_page_hits: HashMap<usize, PageSearchHits>,
    active_search_hit: Option<usize>,
    pending_search_scroll: Option<(usize, NormalizedRect)>,
    pending_full_page_position: Option<f32>,
    interaction_style: ReaderInteractionStyle,
    /// Absolute offset through the wrapped fit-width stream. One viewport
    /// height is one logical display column; the remainder is painted as a
    /// vertical translation inside every visible screen column.
    flow_offset: f32,
    /// Offset within the anchored viewport. Unlike `flow_offset`, this is
    /// never painted as a shared stream translation: it is input to a fresh
    /// local forward layout.
    viewport_scroll_offset: f32,
    scroll_handle: ScrollHandle,
    focus: FocusHandle,
}

struct DisplayPage {
    image: Arc<RenderImage>,
    text_layout: Arc<PdfPageTextLayout>,
    crop: PixelCrop,
    byte_len: usize,
    break_avoidance: Arc<[(f32, f32)]>,
}

struct DisplayPageCache {
    base_byte_budget: usize,
    byte_budget: usize,
    byte_len: usize,
    pages: HashMap<PageRenderKey, Arc<DisplayPage>>,
    recency: VecDeque<PageRenderKey>,
}

#[derive(Clone)]
struct PageSearchHits {
    first_index: usize,
    hits: Arc<[PdfSearchHit]>,
}

struct PdfPageElement {
    page_index: usize,
    column: FitWidthColumn,
    page: Arc<DisplayPage>,
    selection: Option<Arc<PdfTextSelection>>,
    annotations: Arc<[PdfAnnotationOverlay]>,
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

struct PdfPagePrepaint {
    column_bounds: Bounds<Pixels>,
    image_bounds: Bounds<Pixels>,
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
                let checksum = metadata.as_ref().map(|m| m.checksum.clone()).unwrap_or_default();
                PdfDocumentSession::from_reader_with_page_source(source_name, reader, &checksum, metadata, page_source)
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
            effective_page_sizes: HashMap::new(),
            page_break_avoidance: HashMap::new(),
            fit_width_plan: FitWidthPlan::default(),
            viewport_pager: None,
            plan_key: None,
            geometry_revision: 0,
            navigation_offsets: RefCell::new(None),
            passage_bounds: Rc::new(Cell::new(None)),
            #[cfg(test)]
            plan_builds: 0,
            #[cfg(test)]
            raster_requests: 0,
            current_page: initial_page,
            current_page_column: 0,
            revealed_page: None,
            zoom_mode: PdfZoomMode::default(),
            device_pixel_ratio: 1.0,
            available_width: DEFAULT_AVAILABLE_WIDTH,
            available_height: DEFAULT_AVAILABLE_HEIGHT,
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
            active_annotation_id: None,
            search_hits: Vec::new(),
            search_page_hits: HashMap::new(),
            active_search_hit: None,
            pending_search_scroll: None,
            pending_full_page_position: None,
            interaction_style: ReaderInteractionStyle::default(),
            flow_offset: 0.0,
            viewport_scroll_offset: 0.0,
            scroll_handle: ScrollHandle::new(),
            focus,
        }
    }

    pub fn info(&self) -> Option<&PdfDocumentInfo> {
        self.info.as_ref()
    }

    pub fn current_page(&self) -> usize {
        self.current_page
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
        let page_position = self.current_page_position();
        if width_changed {
            self.available_width = available_width;
        }
        if height_changed {
            self.available_height = available_height;
        }
        self.rebuild_fit_width_plan();
        self.restore_page_position(page_position);
        self.request_current_pages(cx);
        cx.notify();
    }

    pub fn trim_margins(&self) -> bool {
        self.trim_margins
    }

    pub fn set_trim_margins(&mut self, trim_margins: bool, cx: &mut Context<Self>) {
        if self.trim_margins == trim_margins {
            return;
        }
        self.pending_full_page_position = Some(self.current_page_position());
        self.trim_margins = trim_margins;
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
        self.pending_full_page_position = Some(full_page_position);
        if !self.pages.is_empty() {
            self.restore_page_position(full_page_position);
        }
        self.request_current_pages(cx);
        cx.notify();
    }

    pub fn next_page(&mut self, cx: &mut Context<Self>) {
        if self.move_viewport_anchor(true, cx) {
            return;
        }
        let target = self.keyboard_page_target(true);
        self.clear_selection();
        self.set_flow_offset(target, cx);
    }

    pub fn previous_page(&mut self, cx: &mut Context<Self>) {
        if self.move_viewport_anchor(false, cx) {
            return;
        }
        let target = self.keyboard_page_target(false);
        self.clear_selection();
        self.set_flow_offset(target, cx);
    }

    fn keyboard_page_target(&self, forward: bool) -> f32 {
        let stride = self.flow_column_height();
        let unsnapped = if forward { self.flow_offset + stride } else { self.flow_offset - stride };
        if self.zoom_mode == PdfZoomMode::FitHeight {
            return unsnapped;
        }
        let offsets = self.atomic_flow_offsets();
        if forward { forward_page_snap_offset(self.flow_offset, unsnapped, &offsets).unwrap_or(unsnapped) } else { backward_page_snap_offset(self.flow_offset, unsnapped, &offsets).unwrap_or(unsnapped) }
    }

    pub fn next_item(&mut self, cx: &mut Context<Self>) {
        let offsets = self.atomic_flow_offsets();
        let Some(target) = next_snap_offset(self.flow_offset, &offsets) else {
            return;
        };
        self.clear_selection();
        self.set_flow_offset(target, cx);
    }

    pub fn previous_item(&mut self, cx: &mut Context<Self>) {
        let offsets = self.atomic_flow_offsets();
        let Some(target) = previous_snap_offset(self.flow_offset, &offsets) else {
            return;
        };
        self.clear_selection();
        self.set_flow_offset(target, cx);
    }

    pub fn set_page(&mut self, page_index: usize, cx: &mut Context<Self>) {
        if page_index >= self.page_count() || (page_index == self.current_page && self.current_page_column == 0) {
            return;
        }
        self.reveal_skipped_page(page_index);
        if self.seek_viewport_anchor(FitWidthAnchor::new(page_index, 0.0), false, cx) {
            self.clear_selection();
            return;
        }
        self.current_page = page_index;
        self.current_page_column = 0;
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
    fn render_target_width(&self) -> u16 {
        let device_width = self.page_display_width() * self.device_pixel_ratio.max(1.0);
        let bucketed = (device_width / RENDER_WIDTH_BUCKET).ceil() * RENDER_WIDTH_BUCKET;
        PageRenderKey::new(0, bucketed.clamp(0.0, u16::MAX as f32) as u16).target_width()
    }

    pub fn zoom_mode(&self) -> PdfZoomMode {
        self.zoom_mode
    }

    /// The zoom every mode resolves to, in pixels per PDF point.
    pub fn zoom(&self) -> f32 {
        self.resolve_layout().zoom
    }

    pub fn set_zoom_mode(&mut self, zoom_mode: PdfZoomMode, cx: &mut Context<Self>) {
        if self.zoom_mode == zoom_mode {
            return;
        }
        let page_position = self.current_page_position();
        self.zoom_mode = zoom_mode;
        self.rebuild_fit_width_plan();
        self.restore_page_position(page_position);
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
        self.set_zoom(self.zoom() + delta, cx);
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
                let state = this.update(cx, |this, _| (this.search_generation == generation, this.rendering));
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
                self.error = Some(error.to_string());
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
        if first_chunk_with_hits && let Some(hit) = self.search_hits.first().copied() {
            self.active_search_hit = Some(0);
            self.reveal_skipped_page(hit.page_index());
            self.seek_viewport_anchor(FitWidthAnchor::new(hit.page_index(), 0.0), false, cx);
            self.pending_search_scroll = Some((hit.page_index(), hit.bounds()));
            self.current_page = hit.page_index();
            self.current_page_column = 0;
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
        if self.search_hits.is_empty() {
            return;
        }
        let current = self.active_search_hit.unwrap_or(0);
        let next = if step < 0 { (current + self.search_hits.len() - 1) % self.search_hits.len() } else { (current + 1) % self.search_hits.len() };
        self.active_search_hit = Some(next);
        let hit = self.search_hits[next];
        self.reveal_skipped_page(hit.page_index());
        self.seek_viewport_anchor(FitWidthAnchor::new(hit.page_index(), 0.0), false, cx);
        self.pending_search_scroll = Some((hit.page_index(), hit.bounds()));
        self.current_page = hit.page_index();
        self.current_page_column = 0;
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
        let rects = selection.rects().iter().map(|rect| page.crop.display_rect_to_full(*rect)).collect::<Vec<_>>();
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
        if page_index >= self.page_count() {
            return;
        }
        self.reveal_skipped_page(page_index);
        self.seek_viewport_anchor(FitWidthAnchor::new(page_index, 0.0), false, cx);
        self.pending_search_scroll = Some((page_index, bounds));
        self.current_page = page_index;
        self.current_page_column = 0;
        self.reset_page_scroll();
        self.clear_selection();
        self.request_current_pages(cx);
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    fn start_load(&mut self, load: impl FnOnce() -> pdf_reader_core::PdfResult<PdfDocumentSession> + Send + 'static, cx: &mut Context<Self>) {
        let generation = self.load_generation;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = session::load(executor, load).await;
            let _ = this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match result {
                    Ok(session) => {
                        this.info = Some(session.info().clone());
                        this.session = Some(session);
                        this.current_page = this.current_page.min(this.page_count().saturating_sub(1));
                        if let Some(info) = &this.info {
                            if info.skippable_pages().binary_search(&this.current_page).is_ok() && info.skippable_pages().len() < info.page_count() {
                                this.current_page = (this.current_page..info.page_count()).chain(0..this.current_page).find(|page| info.skippable_pages().binary_search(page).is_err()).unwrap_or(this.current_page);
                            }
                        }
                        this.rebuild_fit_width_plan();
                        if !this.fit_width_plan.columns().iter().any(|column| column.page_index() == this.current_page) {
                            if let Some(first) = this.fit_width_plan.columns().first().copied() {
                                this.current_page = first.page_index();
                            }
                        }
                        this.request_current_pages(cx);
                        if let Some((search_generation, query)) = this.pending_search.take().filter(|(search_generation, _)| *search_generation == this.search_generation) {
                            this.start_search(search_generation, query, cx);
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.error = Some(message.clone());
                        cx.emit(PdfViewEvent::LoadFailed(message));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn request_current_pages(&mut self, cx: &mut Context<Self>) {
        self.render_generation = self.render_generation.wrapping_add(1);
        let generation = self.render_generation;
        let keys = self.visible_render_keys();
        if keys.is_empty() {
            self.pages.clear();
            cx.notify();
            return;
        }
        let pages = self.cached_pages(&keys);
        if pages.len() == keys.len() {
            // Cached rasters can introduce new crop geometry just like freshly
            // rendered pages. Preserve the full-page anchor before replacing
            // the display pages and rebuilding the document layout.
            let page_position = self.pending_full_page_position.take().unwrap_or_else(|| self.current_page_position());
            self.pages = pages;
            let page_geometry = self.pages.iter().map(|(page_index, page)| (*page_index, page.crop, page.break_avoidance.clone())).collect::<Vec<_>>();
            for (page_index, crop, break_avoidance) in page_geometry {
                if self.update_effective_page_size(page_index, crop) {
                    self.geometry_revision = self.geometry_revision.wrapping_add(1);
                }
                if self.update_break_avoidance(page_index, &break_avoidance) {
                    self.geometry_revision = self.geometry_revision.wrapping_add(1);
                    self.navigation_offsets.get_mut().take();
                }
            }
            self.rebuild_fit_width_plan();
            self.restore_page_position(page_position);
            self.pending_render = None;
            if self.reveal_pending_search() || self.visible_render_keys() != keys {
                self.request_current_pages(cx);
                return;
            }
            self.emit_page_changed(cx);
            cx.notify();
            return;
        }
        // Keep the closest cached resolution visible while exact rasters arrive.
        self.pages = self.display_pages(&keys);
        let missing_keys = keys.into_iter().filter(|key| !self.display_cache.contains(*key)).collect::<Vec<_>>();
        self.error = None;
        if self.rendering {
            self.pending_render = Some((generation, missing_keys));
            cx.notify();
            return;
        }
        self.start_render(generation, missing_keys, cx);
        cx.notify();
    }

    fn visible_render_keys(&self) -> Vec<PageRenderKey> {
        let mut page_indices = Vec::new();
        for column in self.render_window_columns() {
            if !page_indices.contains(&column.page_index()) {
                page_indices.push(column.page_index());
            }
        }
        page_indices.sort_unstable_by_key(|page_index| (*page_index != self.current_page, page_index.abs_diff(self.current_page)));
        page_indices.into_iter().map(|page_index| PageRenderKey::with_margin_trim(page_index, self.render_target_width(), self.trim_margins)).collect()
    }

    fn cached_pages(&mut self, keys: &[PageRenderKey]) -> Vec<(usize, Arc<DisplayPage>)> {
        let mut pages = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(page) = self.display_cache.get(*key) {
                pages.push((key.page_index(), page));
            }
        }
        pages
    }

    fn display_pages(&mut self, keys: &[PageRenderKey]) -> Vec<(usize, Arc<DisplayPage>)> {
        keys.iter().filter_map(|key| self.display_cache.get(*key).or_else(|| self.display_cache.closest(*key)).map(|page| (key.page_index(), page))).collect()
    }

    /// Returns whether the page's layout dimensions changed enough to warrant
    /// rebuilding the flow. Pixel-rounded crop bounds otherwise make a resize
    /// continuously change the measured page size by a few PDF points.
    fn update_effective_page_size(&mut self, page_index: usize, crop: PixelCrop) -> bool {
        let Some(size) = effective_page_size(self.info.as_ref(), page_index, crop) else {
            return false;
        };
        let key = (page_index, self.trim_margins);
        if self.effective_page_sizes.get(&key).is_some_and(|previous| nearly_equal_page_size(*previous, size)) {
            return false;
        }
        self.effective_page_sizes.insert(key, size);
        true
    }

    /// Text and image bands are expressed after raster cropping. Keep the
    /// first geometry until a material change arrives, rather than reflowing
    /// for the one-pixel floor/ceil differences of a new raster bucket.
    fn update_break_avoidance(&mut self, page_index: usize, bands: &Arc<[(f32, f32)]>) -> bool {
        let key = (page_index, self.trim_margins);
        if self.page_break_avoidance.get(&key).is_some_and(|previous| nearly_equal_bands(previous, bands)) {
            return false;
        }
        self.page_break_avoidance.insert(key, bands.clone());
        true
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
                #[cfg(test)]
                let _ = this.update(cx, |this, _| this.raster_requests += 1);
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
                    self.error = Some(error.to_string());
                    cx.notify();
                }
                return false;
            }
        };
        let page = match DisplayPage::from_rendered(rendered) {
            Ok(page) => Arc::new(page),
            Err(error) => {
                if self.render_generation == generation {
                    self.error = Some(error);
                    cx.notify();
                }
                return false;
            }
        };
        self.display_cache.reserve(self.pages.iter().map(|(_, page)| page.byte_len).sum::<usize>() + page.byte_len);
        self.display_cache.insert(key, page.clone());
        if self.render_generation != generation {
            return false;
        }

        let previous_keys = self.visible_render_keys();
        let page_position = self.pending_full_page_position.take().unwrap_or_else(|| self.current_page_position());
        if self.update_effective_page_size(key.page_index(), page.crop) {
            self.geometry_revision = self.geometry_revision.wrapping_add(1);
        }
        if self.update_break_avoidance(key.page_index(), &page.break_avoidance) {
            self.geometry_revision = self.geometry_revision.wrapping_add(1);
            self.navigation_offsets.get_mut().take();
        }
        self.rebuild_fit_width_plan();
        self.restore_page_position(page_position);

        let visible_keys = self.visible_render_keys();
        self.pages = self.display_pages(&visible_keys);
        // Restoring a saved position or updating page geometry can expose a
        // different render window while this page remains in its prefetch area.
        // Schedule that entire window, including newly visible pages.
        if visible_keys != previous_keys || !visible_keys.contains(&key) || self.reveal_pending_search() {
            self.request_current_pages(cx);
            return false;
        }
        self.emit_page_changed(cx);
        cx.notify();
        true
    }

    fn emit_page_changed(&self, cx: &mut Context<Self>) {
        cx.emit(PdfViewEvent::PageChanged { current: self.current_page + 1, total: self.page_count() });
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
        self.flow_offset = self.current_cursor().and_then(|cursor| self.column_for_cursor(cursor)).map(|column| self.flow_offset_for_column(column, 0.0)).unwrap_or(0.0).clamp(0.0, self.max_flow_offset());
        self.scroll_handle.set_offset(point(Pixels::ZERO, Pixels::ZERO));
    }

    /// Rebuilds the visible columns directly from a source position. This is
    /// deliberately independent of the pager history: page jumps, search
    /// results, and future scrollbar targets may all name an anchor that has
    /// never been visible before.
    fn seek_viewport_anchor(&mut self, anchor: FitWidthAnchor, remember_previous: bool, cx: &mut Context<Self>) -> bool {
        let reflowed = self.viewport_pager.as_mut().and_then(|pager| pager.seek(anchor, remember_previous).ok().map(|_| pager.window().plan().clone()));
        let Some(plan) = reflowed else {
            return false;
        };
        self.fit_width_plan = plan;
        self.flow_offset = 0.0;
        self.viewport_scroll_offset = 0.0;
        self.scroll_handle.set_offset(point(Pixels::ZERO, Pixels::ZERO));
        if let Some(column) = self.fit_width_plan.columns().first().copied() {
            self.set_cursor(FitWidthCursor { page_index: column.page_index(), page_column_index: column.page_column_index() });
        }
        self.navigation_offsets.get_mut().take();
        self.request_current_pages(cx);
        true
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
        self.fit_width_plan = pager.window().plan().clone();
        self.flow_offset = 0.0;
        self.viewport_scroll_offset = 0.0;
        self.scroll_handle.set_offset(point(Pixels::ZERO, Pixels::ZERO));
        if let Some(column) = self.fit_width_plan.columns().first().copied() {
            self.set_cursor(FitWidthCursor { page_index: column.page_index(), page_column_index: column.page_column_index() });
        }
        self.clear_selection();
        self.navigation_offsets.get_mut().take();
        self.request_current_pages(cx);
        true
    }

    fn sync_flow_scroll_target(&mut self, delta_y: f32, cx: &mut Context<Self>) {
        // Fit-width scrolling changes only this viewport's local offset, then
        // re-runs the forward local planner. Reverse packing is reserved for
        // crossing to the previous viewport, never for an ordinary wheel tick.
        if self.viewport_pager.is_some() && self.zoom_mode != PdfZoomMode::FitHeight {
            let viewport_height = self.flow_column_height().max(1.0);
            let mut target = self.viewport_scroll_offset - delta_y;
            let reflowed = self.viewport_pager.as_mut().and_then(|pager| {
                while target < 0.0 {
                    if !pager.backward().ok()? {
                        target = 0.0;
                        break;
                    }
                    target += viewport_height;
                }
                while target >= viewport_height {
                    if !pager.forward().ok()? {
                        target = viewport_height - f32::EPSILON;
                        break;
                    }
                    target -= viewport_height;
                }
                pager.reflow_at(target).ok()
            });
            if let Some(plan) = reflowed {
                self.fit_width_plan = plan;
                self.flow_offset = 0.0;
                self.viewport_scroll_offset = target;
                self.scroll_handle.set_offset(point(Pixels::ZERO, Pixels::ZERO));
                if let Some(column) = self.fit_width_plan.columns().first().copied() {
                    self.set_cursor(FitWidthCursor { page_index: column.page_index(), page_column_index: column.page_column_index() });
                }
                self.navigation_offsets.get_mut().take();
                self.request_current_pages(cx);
                return;
            }
        }
        // Fit-height remains an ordinary continuous vertical scroll.
        self.set_flow_offset(self.flow_offset - delta_y, cx);
    }

    /// Resolves one pointer position through the horizontally streamed column
    /// layout. All selection and annotation input uses this single mapping.
    fn interaction_target(&self, position: Point<Pixels>, surface: Bounds<Pixels>) -> Option<PdfInteractionTarget> {
        let column_width = self.page_display_width().max(1.0);
        let column_height = self.flow_column_height().max(1.0);
        let relative_x = f32::from(position.x - surface.origin.x);
        let relative_y = f32::from(position.y - surface.origin.y);
        if relative_x < 0.0 || relative_y < 0.0 || relative_x >= f32::from(surface.size.width) || relative_y >= f32::from(surface.size.height) {
            return None;
        }
        let slot_index = (relative_x / column_width).floor() as usize;
        if slot_index >= self.visible_page_count().max(1) {
            return None;
        }
        let streamed_y = relative_y + self.flow_remainder();
        let stream_index = (streamed_y / column_height).floor() as usize;
        if stream_index > 1 {
            return None;
        }
        let display_index = self.flow_base_index().checked_add(slot_index)?.checked_add(stream_index)?;
        let y_in_column = streamed_y - stream_index as f32 * column_height;
        let x_in_slot = relative_x - slot_index as f32 * column_width;
        for column in columns_for_display(self.fit_width_plan.columns(), display_index) {
            let fragment_top = self.column_offset_within_display(*column);
            let fragment_bottom = fragment_top + column.display_height();
            let fragment_width = column.display_width();
            let fragment_left = if self.zoom_mode == PdfZoomMode::FitHeight { ((column_width - fragment_width) * 0.5).max(0.0) } else { 0.0 };
            if fragment_left <= x_in_slot && x_in_slot <= fragment_left + fragment_width && fragment_top <= y_in_column && y_in_column <= fragment_bottom {
                let bounds = Bounds {
                    origin: point(surface.origin.x + px(slot_index as f32 * column_width + fragment_left), surface.origin.y - px(self.flow_remainder()) + px(stream_index as f32 * column_height + fragment_top)),
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
        let Some((_, page)) = self.pages.iter().find(|(visible_page, _)| *visible_page == page_index) else {
            self.pending_search_scroll = Some((page_index, hit));
            return false;
        };
        let Some(hit) = page.map_rect(hit) else {
            return false;
        };
        let previous_column = self.current_page_column;
        let target_y = (hit.top() + hit.height() * 0.5).clamp(0.0, 1.0);
        let page_columns = columns_for_page(self.fit_width_plan.columns(), page_index);
        if !page_columns.is_empty() {
            self.current_page_column = page_columns.iter().position(|column| column.page_point_to_column((hit.left(), target_y)).is_some()).unwrap_or_else(|| page_columns.len().saturating_sub(1)) as u32;
        }
        self.flow_offset = self
            .flow_offset_for_page_y(page_index, target_y)
            .unwrap_or_else(|| self.current_cursor().and_then(|cursor| self.column_for_cursor(cursor)).map(|column| self.flow_offset_for_column(column, 0.0)).unwrap_or(0.0))
            .clamp(0.0, self.max_flow_offset());
        self.current_page_column != previous_column
    }

    fn begin_selection(&mut self, page_index: usize, page_column_index: u32, position: (f32, f32), anchor_source_index: usize, granularity: SelectionGranularity, cx: &mut Context<Self>) {
        if self.current_page != page_index || self.current_page_column != page_column_index {
            self.current_page = page_index;
            self.current_page_column = page_column_index;
            self.emit_page_changed(cx);
        }
        self.selection_page = Some(page_index);
        self.selection_page_column = Some(page_column_index);
        self.selection_anchor = Some(position);
        self.selection_anchor_source_index = Some(anchor_source_index);
        self.selection_active = Some(position);
        self.selection_granularity = granularity;
        self.update_selection();
        cx.notify();
    }

    fn update_selection_to(&mut self, page_index: usize, page_column_index: u32, position: (f32, f32), cx: &mut Context<Self>) -> bool {
        if self.selection_anchor.is_none() || self.selection_page != Some(page_index) || self.selection_page_column != Some(page_column_index) {
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
        self.effective_page_sizes
            .get(&(self.current_page, self.trim_margins))
            .copied()
            .or_else(|| self.info.as_ref()?.page(self.current_page).map(|page| (page.width_points(), page.height_points())))
            .filter(|(width, height)| width.is_finite() && height.is_finite() && *width > 0.0 && *height > 0.0)
            .unwrap_or((DEFAULT_PAGE_WIDTH_POINTS, DEFAULT_PAGE_HEIGHT_POINTS))
    }

    fn width_fit_zoom(&self, columns: usize) -> f32 {
        width_fit_zoom(columns, self.available_width, self.reference_page_size().0)
    }

    fn resolve_layout(&self) -> ResolvedLayout {
        resolve_layout(self.zoom_mode, self.available_width, self.page_display_height(), self.reference_page_size())
    }

    fn page_display_width(&self) -> f32 {
        self.resolve_layout().column_width
    }

    fn page_display_height(&self) -> f32 {
        (self.available_height - FIT_WIDTH_SURFACE_PADDING * 2.0).max(1.0)
    }

    fn flow_column_height(&self) -> f32 {
        fit_page_flow_column_height(self.zoom_mode, self.visible_page_count(), self.page_display_height(), self.fit_width_plan.columns())
    }

    fn rebuild_fit_width_plan(&mut self) {
        let Some(info) = &self.info else {
            self.fit_width_plan = FitWidthPlan::default();
            self.viewport_pager = None;
            self.navigation_offsets.get_mut().take();
            return;
        };
        let Some(viewport) = FitWidthViewport::new(self.page_display_width(), self.page_display_height()) else {
            self.fit_width_plan = FitWidthPlan::default();
            self.viewport_pager = None;
            self.navigation_offsets.get_mut().take();
            return;
        };
        let key = (self.page_display_width(), self.page_display_height(), self.zoom_mode == PdfZoomMode::FitHeight, self.trim_margins, self.geometry_revision);
        if self.plan_key == Some(key) {
            return;
        }
        self.plan_key = Some(key);
        self.navigation_offsets.get_mut().take();
        #[cfg(test)]
        {
            self.plan_builds += 1;
        }
        let pages = info
            .pages()
            .iter()
            .enumerate()
            .filter(|(page_index, _)| *page_index == self.revealed_page.unwrap_or(usize::MAX) || info.skippable_pages().binary_search(page_index).is_err() || (info.skippable_pages().len() == info.page_count() && *page_index == 0))
            .filter_map(|(page_index, page)| {
                let (width, height) = self.effective_page_sizes.get(&(page_index, self.trim_margins)).copied().unwrap_or((page.width_points(), page.height_points()));
                FitWidthPage::new(page_index, width, height)
            })
            .collect::<Vec<_>>();
        self.fit_width_plan = match self.zoom_mode {
            PdfZoomMode::FitHeight => {
                self.viewport_pager = None;
                FitWidthPlan::fit_page(pages, viewport)
            }
            PdfZoomMode::FitWidth { .. } | PdfZoomMode::Fixed { .. } => {
                let desired = self.viewport_pager.as_ref().map(|pager| pager.window().start()).unwrap_or_else(|| FitWidthAnchor::new(self.current_page, 0.0));
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
                        let bands = self.page_break_avoidance.get(&(page.page_index(), self.trim_margins)).map(Arc::as_ref).unwrap_or(&[]).to_vec();
                        (page, bands)
                    })
                    .collect::<Vec<_>>();
                match FitWidthAnchorPager::new(pager_pages, viewport, self.visible_page_count(), start) {
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
            let bands = self.page_break_avoidance.get(&(page.page_index(), self.trim_margins)).map(Arc::as_ref).unwrap_or(&[]);
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

    fn page_column_count(&self, page_index: usize) -> usize {
        columns_for_page(self.fit_width_plan.columns(), page_index).len()
    }

    fn normalized_cursor(&self, cursor: FitWidthCursor) -> Option<FitWidthCursor> {
        if cursor.page_index >= self.page_count() {
            return None;
        }
        let column_count = self.page_column_count(cursor.page_index);
        if column_count == 0 {
            return None;
        }
        Some(FitWidthCursor { page_index: cursor.page_index, page_column_index: cursor.page_column_index.min(column_count.saturating_sub(1) as u32) })
    }

    fn current_cursor(&self) -> Option<FitWidthCursor> {
        self.normalized_cursor(FitWidthCursor { page_index: self.current_page, page_column_index: self.current_page_column })
    }

    fn set_cursor(&mut self, cursor: FitWidthCursor) {
        if let Some(cursor) = self.normalized_cursor(cursor) {
            self.current_page = cursor.page_index;
            self.current_page_column = cursor.page_column_index;
        }
    }

    fn column_for_cursor(&self, cursor: FitWidthCursor) -> Option<FitWidthColumn> {
        let cursor = self.normalized_cursor(cursor)?;
        columns_for_page(self.fit_width_plan.columns(), cursor.page_index).get(cursor.page_column_index as usize).copied()
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
        column.display_column_index() as f32 * self.flow_column_height() + self.column_offset_within_display(column) + column_y.clamp(0.0, 1.0) * column.display_height()
    }

    fn flow_offset_for_page_y(&self, page_index: usize, display_y: f32) -> Option<f32> {
        let column = columns_for_page(self.fit_width_plan.columns(), page_index).iter().copied().find(|column| column.page_point_to_column((0.5, display_y)).is_some())?;
        let column_y = column.page_point_to_column((0.5, display_y))?.1;
        Some(self.flow_offset_for_column(column, column_y))
    }

    fn flow_anchor(&self) -> Option<(FitWidthColumn, f32)> {
        self.flow_anchor_at(self.flow_offset).map(|anchor| {
            let column = columns_for_page(self.fit_width_plan.columns(), anchor.page_index())
                .iter()
                .copied()
                .find(|column| column.page_point_to_column((0.5, anchor.page_y())).is_some())
                .or_else(|| self.fit_width_plan.columns().iter().copied().find(|column| column.page_index() == anchor.page_index()))
                .expect("source anchor belongs to the current plan");
            let column_y = column.page_point_to_column((0.5, anchor.page_y())).map(|(_, y)| y).unwrap_or(0.0);
            (column, column_y)
        })
    }

    fn flow_anchor_at(&self, offset: f32) -> Option<FitWidthAnchor> {
        let (display_index, remainder) = split_flow_offset(offset, self.flow_column_height());
        let columns = columns_for_display(self.fit_width_plan.columns(), display_index);
        let first = *columns.first()?;
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
        let previous_base = self.flow_base_index();
        self.flow_offset = offset.clamp(0.0, self.max_flow_offset());
        self.scroll_handle.set_offset(point(Pixels::ZERO, Pixels::ZERO));
        if let Some((column, _)) = self.flow_anchor() {
            self.set_cursor(FitWidthCursor { page_index: column.page_index(), page_column_index: column.page_column_index() });
        }
        if self.flow_base_index() != previous_base {
            self.request_current_pages(cx);
        } else {
            self.emit_page_changed(cx);
            cx.notify();
        }
    }

    fn atomic_flow_offsets(&self) -> Arc<[f32]> {
        let extent = (self.flow_column_height(), self.max_flow_offset());
        if let Some((height, maximum, offsets)) = self.navigation_offsets.borrow().as_ref() {
            if (*height, *maximum) == extent {
                return offsets.clone();
            }
        }
        let mut offsets = vec![0.0, self.max_flow_offset()];
        offsets.extend(self.fit_width_plan.columns().iter().copied().map(|column| self.flow_offset_for_column(column, 0.0)));
        for (&(page_index, trim_margins), bands) in &self.page_break_avoidance {
            if trim_margins != self.trim_margins {
                continue;
            }
            offsets.extend(bands.iter().filter_map(|(top, _)| self.flow_offset_for_page_y(page_index, *top)));
        }
        offsets.retain(|offset| offset.is_finite() && *offset >= 0.0 && *offset <= self.max_flow_offset());
        offsets.sort_by(f32::total_cmp);
        offsets.dedup_by(|left, right| (*left - *right).abs() < 0.5);
        let offsets: Arc<[f32]> = offsets.into();
        *self.navigation_offsets.borrow_mut() = Some((extent.0, extent.1, offsets.clone()));
        offsets
    }

    fn current_page_position(&self) -> f32 {
        let Some((column, column_y)) = self.flow_anchor().or_else(|| self.current_cursor().and_then(|cursor| self.column_for_cursor(cursor)).map(|column| (column, 0.0))) else {
            return 0.0;
        };
        let display_y = column.column_point_to_page((0.5, column_y)).1;
        self.pages.iter().find(|(page_index, _)| *page_index == column.page_index()).map(|(_, page)| page.crop.display_point_to_full((0.5, display_y)).1).unwrap_or(display_y)
    }

    fn restore_page_position(&mut self, full_page_position: f32) {
        let columns = columns_for_page(self.fit_width_plan.columns(), self.current_page);
        if columns.is_empty() {
            self.current_page_column = 0;
            return;
        }
        let display_y = self.pages.iter().find(|(page_index, _)| *page_index == self.current_page).map(|(_, page)| page.crop.full_point_to_display((0.5, full_page_position)).1).unwrap_or(full_page_position);
        self.current_page_column = columns.iter().position(|column| column.page_point_to_column((0.5, display_y)).is_some()).unwrap_or_else(|| columns.len().saturating_sub(1)) as u32;
        self.flow_offset = self.flow_offset_for_page_y(self.current_page, display_y).unwrap_or(0.0).clamp(0.0, self.max_flow_offset());
    }
}

/// The zoom at which `columns` pages fill the viewport width.
fn width_fit_zoom(columns: usize, available_width: f32, page_width: f32) -> f32 {
    let columns = columns.max(1) as f32;
    let usable_width = (available_width - FIT_WIDTH_SURFACE_PADDING * 2.0).max(1.0);
    (usable_width / columns / page_width.max(f32::MIN_POSITIVE)).max(f32::MIN_POSITIVE)
}

/// How many columns of `column_width` fit across the viewport.
fn columns_that_fit(column_width: f32, available_width: f32) -> usize {
    let usable_width = (available_width - FIT_WIDTH_SURFACE_PADDING * 2.0).max(1.0);
    let columns = (usable_width / column_width.max(f32::MIN_POSITIVE)).floor();
    (columns.max(1.0) as usize).clamp(MIN_VISIBLE_PAGE_COUNT, MAX_VISIBLE_PAGE_COUNT)
}

/// Settles a [`PdfZoomMode`] into the zoom, column width and column count the
/// layout runs on.
///
/// One page filling the width is the ceiling in every mode: pages are only ever
/// split downwards, so anything wider would hang off the side with no way to
/// reach it.
fn resolve_layout(zoom_mode: PdfZoomMode, available_width: f32, available_height: f32, page_size: (f32, f32)) -> ResolvedLayout {
    let (page_width, page_height) = page_size;
    let widest_zoom = width_fit_zoom(MIN_VISIBLE_PAGE_COUNT, available_width, page_width);
    let (columns, zoom) = match zoom_mode {
        PdfZoomMode::FitWidth { columns } => {
            let columns = columns.clamp(MIN_VISIBLE_PAGE_COUNT, MAX_VISIBLE_PAGE_COUNT);
            (columns, width_fit_zoom(columns, available_width, page_width))
        }
        PdfZoomMode::Fixed { zoom } => {
            let zoom = zoom.clamp(MIN_ZOOM, widest_zoom);
            (columns_that_fit(page_width * zoom, available_width), zoom)
        }
        PdfZoomMode::FitHeight => {
            let zoom = (available_height / page_height.max(f32::MIN_POSITIVE)).clamp(MIN_ZOOM, widest_zoom);
            (columns_that_fit(page_width * zoom, available_width), zoom)
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

fn fit_page_flow_column_height(zoom_mode: PdfZoomMode, visible_page_count: usize, viewport_height: f32, columns: &[FitWidthColumn]) -> f32 {
    let viewport_height = viewport_height.max(1.0);
    if zoom_mode != PdfZoomMode::FitHeight || visible_page_count <= 1 {
        return viewport_height;
    }
    let tallest_page = columns.iter().copied().map(FitWidthColumn::display_height).fold(0.0, f32::max);
    if tallest_page <= 0.0 { viewport_height } else { (tallest_page + PDF_PAGE_BOUNDARY_GAP).min(viewport_height) }
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
        let (rgba, width, height, crop, text_layout, break_avoidance) = page.into_parts();
        let byte_len = rgba.len();
        let image = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| "PDF renderer returned an invalid RGBA buffer".to_owned())?;
        Ok(Self { image: Arc::new(RenderImage::new(SmallVec::from_vec(vec![Frame::new(image)]))), text_layout, crop, byte_len, break_avoidance })
    }

    fn map_rect(&self, rect: NormalizedRect) -> Option<NormalizedRect> {
        crop_normalized_rect(self.crop, rect)
    }
}

fn effective_page_size(info: Option<&PdfDocumentInfo>, page_index: usize, crop: PixelCrop) -> Option<(f32, f32)> {
    let page = info?.page(page_index)?;
    let full_width = crop.full_width().max(1) as f32;
    let full_height = crop.full_height().max(1) as f32;
    let width = page.width_points() * crop.width() as f32 / full_width;
    let height = page.height_points() * crop.height() as f32 / full_height;
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0).then_some((width, height))
}

fn nearly_equal_page_size(left: (f32, f32), right: (f32, f32)) -> bool {
    (left.0 - right.0).abs() <= LAYOUT_SIZE_JITTER_TOLERANCE_POINTS && (left.1 - right.1).abs() <= LAYOUT_SIZE_JITTER_TOLERANCE_POINTS
}

fn nearly_equal_bands(left: &[(f32, f32)], right: &[(f32, f32)]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|((left_top, left_bottom), (right_top, right_bottom))| (left_top - right_top).abs() <= LAYOUT_BAND_JITTER_TOLERANCE && (left_bottom - right_bottom).abs() <= LAYOUT_BAND_JITTER_TOLERANCE)
}

impl DisplayPageCache {
    fn new(byte_budget: usize) -> Self {
        Self { base_byte_budget: byte_budget, byte_budget, byte_len: 0, pages: HashMap::new(), recency: VecDeque::new() }
    }

    /// Raises the budget for as long as one render window needs more than it.
    ///
    /// A window that does not fit evicts the very pages it is about to ask for
    /// again, and the re-request renders and evicts in turn — the reader would
    /// render forever without ever painting. Holding the window is always
    /// cheaper than that loop.
    fn reserve(&mut self, window_bytes: usize) {
        self.byte_budget = self.base_byte_budget.max(window_bytes);
    }

    fn get(&mut self, key: PageRenderKey) -> Option<Arc<DisplayPage>> {
        let page = self.pages.get(&key)?.clone();
        self.promote(key);
        Some(page)
    }

    fn closest(&mut self, key: PageRenderKey) -> Option<Arc<DisplayPage>> {
        let nearest = self
            .pages
            .keys()
            .filter(|candidate| candidate.page_index() == key.page_index() && candidate.trim_margins() == key.trim_margins())
            .min_by_key(|candidate| (candidate.target_width().abs_diff(key.target_width()), std::cmp::Reverse(candidate.target_width())))
            .copied()?;
        self.get(nearest)
    }

    fn contains(&self, key: PageRenderKey) -> bool {
        self.pages.contains_key(&key)
    }

    fn insert(&mut self, key: PageRenderKey, page: Arc<DisplayPage>) {
        if page.byte_len > self.byte_budget {
            return;
        }
        if let Some(previous) = self.pages.remove(&key) {
            self.byte_len = self.byte_len.saturating_sub(previous.byte_len);
            self.recency.retain(|candidate| *candidate != key);
        }
        while self.byte_len + page.byte_len > self.byte_budget {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            if let Some(evicted) = self.pages.remove(&oldest) {
                self.byte_len = self.byte_len.saturating_sub(evicted.byte_len);
            }
        }
        self.byte_len += page.byte_len;
        self.pages.insert(key, page);
        self.recency.push_back(key);
    }

    fn promote(&mut self, key: PageRenderKey) {
        self.recency.retain(|candidate| *candidate != key);
        self.recency.push_back(key);
    }
}

impl IntoElement for PdfPageElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl PdfPageElement {
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
        // Planner geometry is authoritative; raster dimensions are rounded.
        let full_page_height = self.column.display_height() / self.column.page_rect().height().max(f32::EPSILON);
        let image_bounds = Bounds { origin: point(bounds.origin.x, bounds.origin.y - px(self.column.page_rect().top() * full_page_height)), size: size(bounds.size.width, px(full_page_height)) };
        PdfPagePrepaint { column_bounds: bounds, image_bounds }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut Self::RequestLayoutState, prepaint: &mut Self::PrepaintState, window: &mut Window, _cx: &mut App) {
        window.with_content_mask(Some(ContentMask { bounds: prepaint.column_bounds }), |window| {
            let _ = window.paint_image(prepaint.image_bounds, prepaint.image_bounds, Corners::default(), self.page.image.clone(), 0, false);
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
                        window.paint_quad(fill(normalized_bounds(rect, prepaint.column_bounds), rgba(0x2563eb28)));
                    }
                    for rect in annotation_paint_rects(rect, annotation.style) {
                        window.paint_quad(fill(normalized_bounds(rect, prepaint.column_bounds), rgba(annotation.color)));
                    }
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

impl Render for PdfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        self.device_pixel_ratio = window.scale_factor();
        // Cleared before the page elements are built, so what they record in
        // paint describes this frame and not the last one.
        self.passage_bounds.set(None);
        if self.pages.is_empty() {
            return match self.error.clone() {
                Some(message) => components::reader_document_message(message, theme).into_any_element(),
                None if self.session.is_none() => components::reader_document_message("Opening PDF…", theme).into_any_element(),
                None => components::reader_document_message("Rendering PDF page…", theme).into_any_element(),
            };
        }
        let view = cx.entity();
        let column_width = self.page_display_width();
        let column_height = self.flow_column_height();
        let base_index = self.flow_base_index();
        let remainder = self.flow_remainder();
        let total = self.display_column_count().unwrap_or(0);
        let build_logical_column = |display_column_index: usize, instance_index: usize| {
            let mut fragments = SmallVec::<[AnyElement; 4]>::new();
            for column in columns_for_display(self.fit_width_plan.columns(), display_column_index).iter().copied() {
                if column.leading_page_gap() > 0.0 {
                    fragments.push(div().h(px(column.leading_page_gap())).flex_shrink_0().bg(theme.page_bg).into_any_element());
                }
                let page_index = column.page_index();
                let element = self
                    .pages
                    .iter()
                    .find(|(candidate, _)| *candidate == page_index)
                    .map(|(_, page)| {
                        let page = page.clone();
                        let selection = (self.selection_page == Some(page_index)).then(|| self.selection.clone()).flatten();
                        PdfPageElement {
                            page_index,
                            column,
                            page,
                            selection,
                            annotations: self.annotations.clone(),
                            active_annotation_id: self.active_annotation_id.clone(),
                            search_hits: self.search_page_hits.get(&page_index).cloned(),
                            active_search_hit: self.active_search_hit,
                            interaction_style: self.interaction_style,
                            instance_index,
                            passage_bounds: self.passage_bounds.clone(),
                        }
                        .into_any_element()
                    })
                    .unwrap_or_else(|| div().w(px(column.display_width())).h(px(column.display_height())).flex_shrink_0().into_any_element());
                fragments.push(element);
            }
            div()
                .id(("pdf-logical-column", instance_index))
                .w(px(column_width))
                .h(px(column_height))
                .flex()
                .flex_col()
                .flex_shrink_0()
                .when(self.zoom_mode == PdfZoomMode::FitHeight, |column| column.items_center().justify_center())
                .children(fragments)
        };
        let mut screen_columns = SmallVec::<[AnyElement; MAX_VISIBLE_PAGE_COUNT]>::new();
        for slot_index in 0..self.visible_page_count().max(1) {
            let mut stream = SmallVec::<[AnyElement; 2]>::new();
            for (stream_index, current_index) in screen_column_stream(base_index, slot_index, total).enumerate() {
                stream.push(build_logical_column(current_index, slot_index * 2 + stream_index).into_any_element());
            }
            screen_columns
                .push(div().id(("pdf-flow-slot", slot_index)).w(px(column_width)).h(px(column_height)).overflow_hidden().flex_shrink_0().child(div().w_full().flex().flex_col().mt(px(-remainder)).children(stream)).into_any_element());
        }
        let document = div().id("pdf-flow").w_full().h(px(column_height)).flex().flex_row().items_start().flex_shrink_0().children(screen_columns);
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
            .on_scroll_wheel(move |event, _, cx| {
                let (vertical, horizontal) = match event.delta {
                    ScrollDelta::Pixels(delta) => (f32::from(delta.y), f32::from(delta.x)),
                    ScrollDelta::Lines(delta) => (delta.y * LINE_SCROLL_HEIGHT, delta.x * LINE_SCROLL_HEIGHT),
                };
                let flow_delta = if vertical != 0.0 { vertical } else { horizontal };
                if flow_delta != 0.0 {
                    wheel_target.update(cx, |view, cx| view.sync_flow_scroll_target(flow_delta, cx));
                    cx.stop_propagation();
                }
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

    #[test]
    fn wrapped_flow_keeps_screen_columns_fixed_while_the_remainder_moves_vertically() {
        assert_eq!(visible_flow_indices(0, 8, 3), vec![0, 1, 2]);
        assert_eq!(visible_flow_indices(1, 8, 3), vec![1, 2, 3]);
        assert_eq!(visible_flow_indices(2, 8, 3), vec![2, 3, 4]);
        assert_eq!(visible_flow_indices(7, 8, 3), vec![5, 6, 7]);
        assert_eq!(split_flow_offset(375.0, 800.0), (0, 375.0));
        assert_eq!(split_flow_offset(975.0, 800.0), (1, 175.0));
    }

    #[test]
    fn scrolling_pulls_each_right_hand_column_into_its_left_neighbor() {
        assert_eq!(screen_column_stream(0, 0, 4).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(screen_column_stream(0, 1, 4).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(screen_column_stream(0, 2, 4).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(screen_column_stream(1, 0, 4).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(screen_column_stream(0, 3, 4).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn arrow_navigation_snaps_to_one_atomic_boundary() {
        let offsets = [0.0, 120.0, 185.0, 430.0];
        assert_eq!(next_snap_offset(0.0, &offsets), Some(120.0));
        assert_eq!(next_snap_offset(120.0, &offsets), Some(185.0));
        assert_eq!(previous_snap_offset(185.0, &offsets), Some(120.0));
        assert_eq!(previous_snap_offset(0.0, &offsets), None);
    }

    #[test]
    fn keyboard_page_navigation_stops_at_atomic_boundaries_near_the_viewport_step() {
        let offsets = [0.0, 120.0, 185.0, 430.0, 610.0];

        assert_eq!(forward_page_snap_offset(0.0, 200.0, &offsets), Some(185.0));
        assert_eq!(forward_page_snap_offset(185.0, 385.0, &offsets), Some(430.0));
        assert_eq!(backward_page_snap_offset(430.0, 230.0, &offsets), Some(185.0));
        assert_eq!(backward_page_snap_offset(610.0, 410.0, &offsets), Some(430.0));
    }

    #[test]
    fn annotation_hit_testing_uses_column_normalized_coordinates() {
        let rect = NormalizedRect::new(0.2, 0.3, 0.4, 0.2);
        assert!(normalized_rect_contains(rect, (0.2, 0.3)));
        assert!(normalized_rect_contains(rect, (0.6, 0.5)));
        assert!(!normalized_rect_contains(rect, (0.19, 0.4)));
        assert!(!normalized_rect_contains(rect, (0.4, 0.51)));
    }

    #[test]
    fn a_fixed_zoom_decides_how_many_pages_fit_across() {
        assert_eq!(columns_that_fit(499.0, 1_000.0), 2);
        assert_eq!(columns_that_fit(500.0, 1_000.0), 2, "two columns should meet without an artificial boundary gutter");
        assert_eq!(columns_that_fit(332.0, 1_000.0), 3);
        assert_eq!(columns_that_fit(1_000.0, 1_000.0), 1);
        assert_eq!(columns_that_fit(4_000.0, 1_000.0), 1, "a page wider than the viewport still occupies one column");
    }

    #[test]
    fn fit_width_and_fixed_zoom_are_two_ends_of_one_quantity() {
        let fitted = resolve_layout(PdfZoomMode::FitWidth { columns: 2 }, 1_000.0, 800.0, (500.0, 700.0));
        assert_eq!(fitted.columns, 2);

        // Feeding that zoom back in as a fixed zoom must reproduce the layout
        // it came from.
        let fixed = resolve_layout(PdfZoomMode::Fixed { zoom: fitted.zoom }, 1_000.0, 800.0, (500.0, 700.0));
        assert_eq!(fixed.columns, 2);
        assert!((fixed.column_width - fitted.column_width).abs() < 0.001);
    }

    #[test]
    fn fit_height_takes_its_zoom_from_the_page_height() {
        let layout = resolve_layout(PdfZoomMode::FitHeight, 1_000.0, 800.0, (490.0, 800.0));
        assert!((layout.zoom - 1.0).abs() < 0.001, "an 800pt page in 800px of height is one pixel per point");
        assert_eq!(layout.columns, 2, "at that zoom two 490px pages fit across 1000px");
    }

    #[test]
    fn no_mode_lets_a_page_grow_wider_than_the_viewport() {
        for mode in [PdfZoomMode::FitHeight, PdfZoomMode::Fixed { zoom: 99.0 }, PdfZoomMode::FitWidth { columns: 1 }] {
            let layout = resolve_layout(mode, 400.0, 5_000.0, (500.0, 700.0));
            assert!(layout.column_width <= 400.0 + 0.001, "{mode:?} produced a {}px column in a 400px viewport", layout.column_width);
        }
    }

    #[test]
    fn raster_rounding_noise_does_not_change_layout_geometry() {
        assert!(nearly_equal_page_size((612.0, 792.0), (615.8, 788.2)));
        assert!(!nearly_equal_page_size((612.0, 792.0), (616.1, 792.0)));

        assert!(nearly_equal_bands(&[(0.20, 0.23), (0.40, 0.43)], &[(0.205, 0.225), (0.405, 0.435)]));
        assert!(!nearly_equal_bands(&[(0.20, 0.23)], &[(0.212, 0.23)]));
    }

    #[test]
    fn fit_page_spreads_use_the_fitted_page_height_as_the_vertical_stride() {
        let viewport = FitWidthViewport::new(400.0, 1_600.0).expect("viewport");
        let page = FitWidthPage::new(0, 600.0, 800.0).expect("page");
        let plan = FitWidthPlan::fit_page([page], viewport);
        let fitted_height = plan.columns()[0].display_height();

        assert!((fit_page_flow_column_height(PdfZoomMode::FitHeight, 2, 1_600.0, plan.columns()) - (fitted_height + PDF_PAGE_BOUNDARY_GAP)).abs() < 0.001);
        assert_eq!(fit_page_flow_column_height(PdfZoomMode::FitHeight, 1, 1_600.0, plan.columns()), 1_600.0);
        assert_eq!(fit_page_flow_column_height(PdfZoomMode::FitWidth { columns: 2 }, 2, 1_600.0, plan.columns()), 1_600.0);
    }

    static PDF_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn interaction_fixture() -> Vec<u8> {
        use lopdf::{Document, Object, Stream, dictionary};
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let font = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
        let contents = document.add_object(Stream::new(dictionary! {}, b"BT /F1 18 Tf 72 220 Td (Searchable text) Tj ET".to_vec()));
        let page = document.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 300.into(), 400.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } }, "Contents" => contents,
        });
        document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let root_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", root_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn lazy_annotation_suppression_preserves_external_annotations() {
        let _guard = PDF_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        use lopdf::{Document, Object, Stream, dictionary};
        for (name, visible) in [("external:highlight", true), ("bokheim:highlight", false)] {
            let mut document = Document::load_mem(&interaction_fixture()).unwrap();
            let page = *document.get_pages().values().next().unwrap();
            let appearance = document.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 50.into(), 50.into()],
                },
                b"1 0 0 rg 0 0 50 50 re f".to_vec(),
            ));
            let annotation = document.add_object(dictionary! {
                "Type" => "Annot", "Subtype" => "Square", "NM" => Object::string_literal(name),
                "Rect" => vec![20.into(), 20.into(), 70.into(), 70.into()], "F" => 4,
                "AP" => dictionary! { "N" => appearance },
            });
            document.get_object_mut(page).unwrap().as_dict_mut().unwrap().set("Annots", vec![annotation.into()]);
            let mut bytes = Vec::new();
            document.save_to(&mut bytes).unwrap();
            let session = PdfDocumentSession::from_bytes(name, bytes).unwrap();
            for width in [600, 800] {
                let raster = session.render_page(0, Some(width)).unwrap();
                let red = raster.rgba().chunks_exact(4).filter(|pixel| pixel[0] > 200 && pixel[1] < 50 && pixel[2] < 50).count();
                assert_eq!(red > 500, visible, "wrong annotation visibility for {name} at {width}px: {red} red pixels");
            }
        }
    }

    #[gpui::test]
    fn cached_navigation_and_zoom_preserve_pages_without_rebuilding(cx: &mut gpui::TestAppContext) {
        let _guard = PDF_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| {
            let mut view = PdfView::from_bytes("test.pdf".into(), interaction_fixture(), 0, window, cx);
            view.trim_margins = false;
            view
        });
        cx.run_until_parked();
        let initial_rasters = window
            .update(cx, |view, _, cx| {
                assert_eq!(view.pages.len(), 1);
                let builds = view.plan_builds;
                let rasters = view.raster_requests;
                view.request_current_pages(cx);
                view.request_current_pages(cx);
                assert_eq!(view.plan_builds, builds, "cached navigation must not rebuild document layout");
                assert_eq!(view.raster_requests, rasters);
                let previous = view.display_cache.pages.values().cloned().collect::<Vec<_>>();
                view.set_zoom_mode(PdfZoomMode::Fixed { zoom: 0.8 }, cx);
                assert!(previous.iter().any(|page| Arc::ptr_eq(page, &view.pages[0].1)), "zoom should retain a cached raster until replacement arrives");
                assert!(view.plan_builds > builds, "zoom must rebuild geometry");
                // Supersede the queued generation before it can start.
                view.request_current_pages(cx);
                view.request_current_pages(cx);
                rasters
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                assert!(!view.rendering);
                assert!(view.pending_render.is_none());
                assert_eq!(view.raster_requests, initial_rasters + 1, "superseded requests must not raster the same page repeatedly");
                assert!(view.visible_render_keys().iter().all(|key| view.display_cache.contains(*key)));
                let builds = view.plan_builds;
                view.request_current_pages(cx);
                assert_eq!(view.plan_builds, builds);
                view.set_trim_margins(true, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, _| {
                assert!(!view.pages[0].1.crop.is_full_page());
                assert!(!view.rendering);
                view.session = None;
            })
            .unwrap();
    }

    #[gpui::test]
    fn selection_uses_display_geometry_without_a_document_session(cx: &mut gpui::TestAppContext) {
        let _guard = PDF_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| PdfView::from_bytes("test.pdf".into(), interaction_fixture(), 0, window, cx));
        cx.run_until_parked();
        window
            .update(cx, |view, _, _| {
                view.session = None;
                let page = &view.pages[0].1;
                let first = page.text_layout.geometry().characters().iter().find(|ch| !ch.character().is_control()).unwrap();
                let bounds = first.bounds();
                let center = ((bounds.left() + bounds.right()) / 2.0, (bounds.top() + bounds.bottom()) / 2.0);
                let target = PdfInteractionTarget { page_index: 0, column: view.fit_width_plan.columns()[0], page_point: center, page_display_size: (2000.0, 500.0) };
                assert_eq!(view.selection_anchor_source_index(target), Some(first.source_index()));
                assert_eq!(cached_character_index(&page.text_layout, (bounds.left() - 7.0 / 2000.0, center.1), (2000.0, 500.0)), Some(first.source_index()));
                assert_eq!(cached_character_index(&page.text_layout, (bounds.left() - 9.0 / 2000.0, center.1), (2000.0, 500.0)), None);
                assert_eq!(cached_character_index(&page.text_layout, center, (0.0, 500.0)), None);
            })
            .unwrap();
    }

    #[gpui::test]
    fn navigation_offsets_are_reused_until_geometry_changes(cx: &mut gpui::TestAppContext) {
        let _guard = PDF_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| PdfView::from_bytes("test.pdf".into(), interaction_fixture(), 0, window, cx));
        cx.run_until_parked();
        let original = window
            .update(cx, |view, _, cx| {
                let original = view.atomic_flow_offsets();
                assert!(Arc::ptr_eq(&original, &view.atomic_flow_offsets()));
                view.request_current_pages(cx);
                assert!(Arc::ptr_eq(&original, &view.atomic_flow_offsets()), "cached navigation must reuse sorted offsets");
                view.set_trim_margins(false, cx);
                original
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                let untrimmed = view.atomic_flow_offsets();
                assert!(!Arc::ptr_eq(&original, &untrimmed));
                view.set_zoom_mode(PdfZoomMode::Fixed { zoom: 0.8 }, cx);
                assert!(!Arc::ptr_eq(&untrimmed, &view.atomic_flow_offsets()));
                view.session = None;
            })
            .unwrap();
        cx.run_until_parked();
    }

    #[test]
    fn binary_navigation_matches_linear_reference_at_tolerance_boundaries() {
        let offsets = [0.0, 1.0, 5.0, 10.0, 100.0];
        for current in [-1.0, 0.0, 0.5, 1.0, 4.5, 5.0, 5.5, 99.5, 100.0, 101.0] {
            for target in [-2.0, 0.0, 0.5, 1.0, 4.5, 5.0, 5.5, 99.5, 100.0, 102.0] {
                let next = offsets.iter().copied().find(|offset| *offset > current + 0.5);
                let previous = offsets.iter().copied().rev().find(|offset| *offset < current - 0.5);
                assert_eq!(next_snap_offset(current, &offsets), next);
                assert_eq!(previous_snap_offset(current, &offsets), previous);
                let forward = offsets.iter().copied().take_while(|offset| *offset <= target + 0.5).filter(|offset| *offset > current + 0.5).last().or(next);
                let backward = offsets.iter().copied().find(|offset| *offset >= target - 0.5 && *offset < current - 0.5).or(previous);
                assert_eq!(forward_page_snap_offset(current, target, &offsets), forward);
                assert_eq!(backward_page_snap_offset(current, target, &offsets), backward);
            }
        }
        assert_eq!(next_snap_offset(0.0, &[]), None);
        assert_eq!(previous_snap_offset(0.0, &[]), None);
    }

    #[gpui::test]
    fn streamed_search_preserves_page_slices_navigation_and_cancellation(cx: &mut gpui::TestAppContext) {
        let _guard = PDF_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        use lopdf::{Document, Object};
        let mut fixture = Document::load_mem(&interaction_fixture()).unwrap();
        let page_id = *fixture.get_pages().values().next().unwrap();
        let page = fixture.get_object(page_id).unwrap().clone();
        let parent = page.as_dict().unwrap().get(b"Parent").unwrap().as_reference().unwrap();
        let second = fixture.add_object(page.clone());
        let third = fixture.add_object(page);
        let pages = fixture.get_object_mut(parent).unwrap().as_dict_mut().unwrap();
        pages.set("Kids", vec![Object::Reference(page_id), Object::Reference(second), Object::Reference(third)]);
        pages.set("Count", 3);
        let mut bytes = Vec::new();
        fixture.save_to(&mut bytes).unwrap();
        let document = PdfDocumentSession::from_bytes("search", bytes).unwrap();
        let hits = document.search("Searchable").unwrap();
        assert_eq!(hits.len(), 3);
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| PdfView::empty(0, window, cx));
        window
            .update(cx, |view, _, cx| {
                let generation = view.search_generation;
                assert!(view.apply_search_chunk(generation, Ok(vec![hits[0]]), cx));
                let first_page = view.search_page_hits[&0].hits.clone();
                assert!(view.apply_search_chunk(generation, Ok(vec![hits[1]]), cx));
                assert!(Arc::ptr_eq(&first_page, &view.search_page_hits[&0].hits), "new pages must not copy earlier matches");
                view.navigate_search(1, cx);
                assert_eq!(view.active_search_hit, Some(1));
                assert!(view.apply_search_chunk(generation, Ok(vec![hits[2]]), cx));
                assert_eq!(view.active_search_hit, Some(1), "streaming must preserve the user's selected match");
                assert_eq!(view.search_hits, hits);
                for page in 0..3 {
                    assert_eq!(view.search_page_hits[&page].first_index, page);
                    assert_eq!(view.search_page_hits[&page].hits.as_ref(), &hits[page..page + 1]);
                }
                view.navigate_search(1, cx);
                assert_eq!(view.current_page, 2);
                view.navigate_search(1, cx);
                assert_eq!(view.current_page, 0);
                view.navigate_search(-1, cx);
                assert_eq!(view.current_page, 2);
                view.search(String::new(), cx);
                assert!(view.search_hits.is_empty());
                assert!(view.search_page_hits.is_empty());
                assert!(!view.apply_search_chunk(generation, Ok(hits), cx));
                assert!(view.search_page_hits.is_empty());
            })
            .unwrap();
    }

    #[gpui::test]
    fn searching_during_render_finishes_with_the_latest_query(cx: &mut gpui::TestAppContext) {
        let _guard = PDF_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| PdfView::from_bytes("test.pdf".into(), interaction_fixture(), 0, window, cx));
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                view.set_zoom_mode(PdfZoomMode::Fixed { zoom: 0.8 }, cx);
                view.search("missing".into(), cx);
                view.search("Searchable".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        cx.background_executor.advance_clock(std::time::Duration::from_millis(16));
        cx.run_until_parked();
        window
            .update(cx, |view, _, _| {
                assert!(!view.rendering);
                assert!(!view.search_in_flight);
                assert_eq!(view.search_hits.len(), 1);
                view.session = None;
            })
            .unwrap();
    }

    #[test]
    fn display_cache_evicts_oldest_page_with_a_byte_budget() {
        fn page(byte_len: usize) -> Arc<DisplayPage> {
            let image = RgbaImage::new(1, 1);
            Arc::new(DisplayPage { image: Arc::new(RenderImage::new(SmallVec::from_vec(vec![Frame::new(image)]))), text_layout: Arc::new(PdfPageTextLayout::default()), crop: PixelCrop::full(1, 1), byte_len, break_avoidance: Arc::from([]) })
        }

        let mut cache = DisplayPageCache::new(8);
        let first = PageRenderKey::new(0, 800);
        let second = PageRenderKey::new(1, 800);
        let third = PageRenderKey::new(2, 800);
        cache.insert(first, page(4));
        cache.insert(second, page(4));
        assert!(cache.get(first).is_some());
        cache.insert(third, page(4));
        assert!(cache.get(first).is_some());
        assert!(cache.get(second).is_none());
        assert!(cache.get(third).is_some());
        assert!(cache.closest(PageRenderKey::new(0, 1200)).is_some());
        assert!(cache.closest(PageRenderKey::new(3, 1200)).is_none());
        assert!(cache.closest(PageRenderKey::with_margin_trim(0, 1200, true)).is_none(), "fallback must preserve trim mode");
        cache.reserve(12);
        let sharper = PageRenderKey::new(0, 1600);
        cache.insert(sharper, page(4));
        let fallback = cache.closest(PageRenderKey::new(0, 1400)).unwrap();
        assert!(Arc::ptr_eq(&fallback, &cache.get(sharper).unwrap()));
        assert!(!cache.contains(PageRenderKey::new(0, 1400)), "a fallback does not satisfy an exact render request");
    }
}
