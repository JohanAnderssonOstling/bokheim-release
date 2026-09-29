//! Framework-independent PDF crop, coordinate, and text-selection geometry.

mod metadata;
pub use metadata::{PdfByteRange, PdfDependencyIndex, PdfPageSource, PdfReaderMetadata, PdfStoredAnnotation, PdfStoredAnnotationKind, PdfStoredPage};

const DEFAULT_CONTENT_DIFFERENCE_THRESHOLD: u8 = 24;
const DEFAULT_PADDING_PIXELS: u32 = 12;
const DEFAULT_HIT_DISTANCE_SQUARED: f32 = 0.0016;
const MAX_FIT_WIDTH_COLUMNS_PER_PAGE: u32 = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropOptions {
    content_difference_threshold: u8,
    padding_pixels: u32,
}

impl CropOptions {
    pub const fn new(content_difference_threshold: u8, padding_pixels: u32) -> Self {
        Self { content_difference_threshold, padding_pixels }
    }

    pub const fn content_difference_threshold(self) -> u8 {
        self.content_difference_threshold
    }

    pub const fn padding_pixels(self) -> u32 {
        self.padding_pixels
    }
}

impl Default for CropOptions {
    fn default() -> Self {
        Self::new(DEFAULT_CONTENT_DIFFERENCE_THRESHOLD, DEFAULT_PADDING_PIXELS)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelCrop {
    full_width: u32,
    full_height: u32,
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

impl PixelCrop {
    pub fn new(full_width: u32, full_height: u32, left: u32, top: u32, width: u32, height: u32) -> Self {
        let left = left.min(full_width);
        let top = top.min(full_height);
        let width = width.min(full_width.saturating_sub(left));
        let height = height.min(full_height.saturating_sub(top));
        Self { full_width, full_height, left, top, width, height }
    }

    pub const fn full(full_width: u32, full_height: u32) -> Self {
        Self { full_width, full_height, left: 0, top: 0, width: full_width, height: full_height }
    }

    pub const fn full_width(self) -> u32 {
        self.full_width
    }

    pub const fn full_height(self) -> u32 {
        self.full_height
    }

    pub const fn left(self) -> u32 {
        self.left
    }

    pub const fn top(self) -> u32 {
        self.top
    }

    pub const fn width(self) -> u32 {
        self.width
    }

    pub const fn height(self) -> u32 {
        self.height
    }

    pub const fn is_full_page(self) -> bool {
        self.left == 0 && self.top == 0 && self.width == self.full_width && self.height == self.full_height
    }

    pub fn display_point_to_full(self, point: (f32, f32)) -> (f32, f32) {
        if self.full_width == 0 || self.full_height == 0 {
            return (0.0, 0.0);
        }
        (((self.left as f32 + point.0.clamp(0.0, 1.0) * self.width as f32) / self.full_width as f32).clamp(0.0, 1.0), ((self.top as f32 + point.1.clamp(0.0, 1.0) * self.height as f32) / self.full_height as f32).clamp(0.0, 1.0))
    }

    pub fn full_point_to_display(self, point: (f32, f32)) -> (f32, f32) {
        let x = point.0.clamp(0.0, 1.0) * self.full_width as f32;
        let y = point.1.clamp(0.0, 1.0) * self.full_height as f32;
        (((x - self.left as f32) / self.width.max(1) as f32).clamp(0.0, 1.0), ((y - self.top as f32) / self.height.max(1) as f32).clamp(0.0, 1.0))
    }

    pub fn full_rect_to_display(self, rect: NormalizedRect) -> Option<NormalizedRect> {
        let full_width = self.full_width as f32;
        let full_height = self.full_height as f32;
        let crop_left = self.left as f32;
        let crop_top = self.top as f32;
        let crop_right = (self.left + self.width) as f32;
        let crop_bottom = (self.top + self.height) as f32;
        let left = rect.left * full_width;
        let top = rect.top * full_height;
        let right = rect.right() * full_width;
        let bottom = rect.bottom() * full_height;
        let intersection_left = left.max(crop_left);
        let intersection_top = top.max(crop_top);
        let intersection_right = right.min(crop_right);
        let intersection_bottom = bottom.min(crop_bottom);
        if intersection_left >= intersection_right || intersection_top >= intersection_bottom {
            return None;
        }
        Some(NormalizedRect::new(
            (intersection_left - crop_left) / self.width.max(1) as f32,
            (intersection_top - crop_top) / self.height.max(1) as f32,
            (intersection_right - intersection_left) / self.width.max(1) as f32,
            (intersection_bottom - intersection_top) / self.height.max(1) as f32,
        ))
    }

    pub fn display_rect_to_full(self, rect: NormalizedRect) -> NormalizedRect {
        let top_left = self.display_point_to_full((rect.left, rect.top));
        let bottom_right = self.display_point_to_full((rect.right(), rect.bottom()));
        NormalizedRect::new(top_left.0, top_left.1, bottom_right.0 - top_left.0, bottom_right.1 - top_left.1)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NormalizedRect {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

impl NormalizedRect {
    pub fn new(left: f32, top: f32, width: f32, height: f32) -> Self {
        let left = left.clamp(0.0, 1.0);
        let top = top.clamp(0.0, 1.0);
        Self { left, top, width: width.clamp(0.0, 1.0 - left), height: height.clamp(0.0, 1.0 - top) }
    }

    pub const fn left(self) -> f32 {
        self.left
    }

    pub const fn top(self) -> f32 {
        self.top
    }

    pub const fn width(self) -> f32 {
        self.width
    }

    pub const fn height(self) -> f32 {
        self.height
    }

    pub fn right(self) -> f32 {
        self.left + self.width
    }

    pub fn bottom(self) -> f32 {
        self.top + self.height
    }

    pub fn union(self, other: Self) -> Self {
        let left = self.left.min(other.left);
        let top = self.top.min(other.top);
        Self::new(left, top, self.right().max(other.right()) - left, self.bottom().max(other.bottom()) - top)
    }

    pub fn distance_squared(self, point: (f32, f32)) -> f32 {
        let dx = if point.0 < self.left {
            self.left - point.0
        } else if point.0 > self.right() {
            point.0 - self.right()
        } else {
            0.0
        };
        let dy = if point.1 < self.top {
            self.top - point.1
        } else if point.1 > self.bottom() {
            point.1 - self.bottom()
        } else {
            0.0
        };
        dx * dx + dy * dy
    }
}

/// Effective page dimensions used when planning fit-width column flow.
///
/// Page units are arbitrary as long as width and height use the same unit.
/// Callers should pass dimensions after applying any crop or rotation that
/// affects display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitWidthPage {
    page_index: usize,
    width: f32,
    height: f32,
}

impl FitWidthPage {
    pub fn new(page_index: usize, width: f32, height: f32) -> Option<Self> {
        valid_layout_extent(width, height).then_some(Self { page_index, width, height })
    }

    pub const fn page_index(self) -> usize {
        self.page_index
    }

    pub const fn width(self) -> f32 {
        self.width
    }

    pub const fn height(self) -> f32 {
        self.height
    }
}

/// Width and usable height of one logical fit-width column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitWidthViewport {
    column_width: f32,
    column_height: f32,
}

/// A stable source position used to begin a bounded fit-width viewport.
///
/// Unlike a display-column index, this survives a change in column count or
/// viewport height: it always identifies a point in the original PDF page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitWidthAnchor {
    page_index: usize,
    page_y: f32,
}

/// A locally planned reading viewport. `start` and `end` are source anchors,
/// so a caller can discard its display columns and later reconstruct the same
/// reading position at a different viewport size.
#[derive(Clone, Debug, PartialEq)]
pub struct FitWidthViewportWindow {
    start: FitWidthAnchor,
    end: Option<FitWidthAnchor>,
    plan: FitWidthPlan,
}

impl FitWidthViewportWindow {
    pub const fn start(&self) -> FitWidthAnchor {
        self.start
    }

    /// The position immediately after this viewport's final column, if the
    /// document has more renderable content.
    pub const fn end(&self) -> Option<FitWidthAnchor> {
        self.end
    }

    pub fn plan(&self) -> &FitWidthPlan {
        &self.plan
    }
}

/// Stateful, source-anchored viewport pagination for fit-width PDFs.
///
/// It keeps only stable source anchors between windows. Display columns are
/// rebuilt from those anchors, so a change in viewport size cannot make a
/// previous display-column index point to unrelated content.
#[derive(Clone, Debug, PartialEq)]
pub struct FitWidthAnchorPager {
    pages: Vec<(FitWidthPage, Vec<(f32, f32)>)>,
    viewport: FitWidthViewport,
    column_count: usize,
    window: FitWidthViewportWindow,
}

impl FitWidthAnchorPager {
    pub fn new(pages: impl IntoIterator<Item = (FitWidthPage, Vec<(f32, f32)>)>, viewport: FitWidthViewport, column_count: usize, start: FitWidthAnchor) -> Result<Self, FitWidthPlanError> {
        let pages = pages.into_iter().collect::<Vec<_>>();
        let window = Self::build_window(&pages, viewport, column_count, start)?;
        Ok(Self { pages, viewport, column_count: column_count.max(1), window })
    }

    pub fn window(&self) -> &FitWidthViewportWindow {
        &self.window
    }

    pub fn can_forward(&self) -> bool {
        self.window.end.is_some()
    }

    pub fn can_backward(&self) -> bool {
        self.previous_window_start().is_some()
    }

    /// Promotes this viewport's end source anchor to the next viewport start.
    pub fn forward(&mut self) -> Result<bool, FitWidthPlanError> {
        let Some(next_start) = self.window.end else {
            return Ok(false);
        };
        self.window = Self::build_window(&self.pages, self.viewport, self.column_count, next_start)?;
        Ok(true)
    }

    /// Derives the prior viewport by packing backward from the current source
    /// start. It never depends on navigation history or a document-wide layout.
    pub fn backward(&mut self) -> Result<bool, FitWidthPlanError> {
        let Some(previous_start) = self.previous_window_start() else {
            return Ok(false);
        };
        self.window = Self::build_window(&self.pages, self.viewport, self.column_count, previous_start)?;
        Ok(true)
    }

    /// Reflows from an arbitrary source position reached during continuous
    /// scrolling, a page jump, a search hit, or a TOC target.
    pub fn seek(&mut self, start: FitWidthAnchor, _remember_previous: bool) -> Result<(), FitWidthPlanError> {
        if start == self.window.start {
            return Ok(());
        }
        self.window = Self::build_window(&self.pages, self.viewport, self.column_count, start)?;
        Ok(())
    }

    /// Builds the locally visible layout at an offset from this viewport's
    /// stable start anchor. The pager itself is not advanced; wheel scrolling
    /// may therefore move backward by reducing the offset and running this
    /// same forward pass again.
    pub fn reflow_at(&self, display_offset: f32) -> Result<FitWidthPlan, FitWidthPlanError> {
        let anchor = self.anchor_at_offset(display_offset).unwrap_or(self.window.start);
        Ok(Self::build_window(&self.pages, self.viewport, self.column_count, anchor)?.plan)
    }

    fn previous_window_start(&self) -> Option<FitWidthAnchor> {
        let mut page_position = self.pages.iter().position(|(page, _)| page.page_index() == self.window.start.page_index())?;
        let mut page_y = self.window.start.page_y();
        let had_preceding_content = page_position > 0 || page_y > f32::EPSILON;
        for _ in 0..self.column_count {
            let mut remaining_height = self.viewport.column_height();
            loop {
                let (page, bands) = self.pages.get(page_position)?;
                let scaled_height = page.height() / page.width() * self.viewport.column_width();
                let ideal_top = (page_y - remaining_height / scaled_height.max(f32::EPSILON)).max(0.0);
                let top = safe_slice_start(ideal_top, page_y, bands);
                remaining_height -= (page_y - top) * scaled_height;
                page_y = top;
                if remaining_height <= 0.5 {
                    break;
                }
                if page_position == 0 {
                    return had_preceding_content.then(|| FitWidthAnchor::new(page.page_index(), 0.0));
                }
                page_position -= 1;
                page_y = 1.0;
            }
        }
        Some(FitWidthAnchor::new(self.pages.get(page_position)?.0.page_index(), page_y))
    }

    fn anchor_at_offset(&self, display_offset: f32) -> Option<FitWidthAnchor> {
        let column_height = self.viewport.column_height().max(f32::EPSILON);
        let display_column = (display_offset.max(0.0) / column_height).floor() as usize;
        let mut y = display_offset.max(0.0) - display_column as f32 * column_height;
        let columns = self.window.plan.columns.iter().filter(|column| column.display_column_index == display_column);
        let mut last = None;
        for column in columns {
            y -= column.leading_page_gap;
            if y <= column.display_height {
                let fraction = (y / column.display_height.max(f32::EPSILON)).clamp(0.0, 1.0);
                return Some(FitWidthAnchor::new(column.page_index, column.page_rect.top() + fraction * column.page_rect.height()));
            }
            y -= column.display_height;
            last = Some(*column);
        }
        last.map(|column| FitWidthAnchor::new(column.page_index, column.page_rect.bottom()))
    }

    fn build_window(pages: &[(FitWidthPage, Vec<(f32, f32)>)], viewport: FitWidthViewport, column_count: usize, start: FitWidthAnchor) -> Result<FitWidthViewportWindow, FitWidthPlanError> {
        FitWidthPlan::viewport_window_from_anchor_avoiding_breaks(pages.iter().map(|(page, bands)| (*page, bands.as_slice())), viewport, start, column_count)
    }
}

impl FitWidthAnchor {
    pub fn new(page_index: usize, page_y: f32) -> Self {
        Self { page_index, page_y: page_y.clamp(0.0, 1.0) }
    }

    pub const fn page_index(self) -> usize {
        self.page_index
    }

    pub const fn page_y(self) -> f32 {
        self.page_y
    }
}

impl FitWidthViewport {
    pub fn new(column_width: f32, column_height: f32) -> Option<Self> {
        valid_layout_extent(column_width, column_height).then_some(Self { column_width, column_height })
    }

    pub const fn column_width(self) -> f32 {
        self.column_width
    }

    pub const fn column_height(self) -> f32 {
        self.column_height
    }
}

/// One logical column in fit-width reading order.
///
/// `page_rect` is normalized within the effective page passed to the planner.
/// It can be composed with `PageTransform` when margin trimming or rotation is
/// active. Planning does not render, clip, or otherwise mutate the PDF page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitWidthColumn {
    flow_index: usize,
    display_column_index: usize,
    page_index: usize,
    page_column_index: u32,
    page_column_count: u32,
    page_rect: NormalizedRect,
    display_width: f32,
    display_height: f32,
    leading_page_gap: f32,
}

impl FitWidthColumn {
    pub const fn flow_index(self) -> usize {
        self.flow_index
    }

    pub const fn display_column_index(self) -> usize {
        self.display_column_index
    }

    pub const fn page_index(self) -> usize {
        self.page_index
    }

    pub const fn page_column_index(self) -> u32 {
        self.page_column_index
    }

    pub const fn page_column_count(self) -> u32 {
        self.page_column_count
    }

    pub const fn page_rect(self) -> NormalizedRect {
        self.page_rect
    }

    pub const fn display_width(self) -> f32 {
        self.display_width
    }

    pub const fn display_height(self) -> f32 {
        self.display_height
    }

    /// Vertical space before this fragment caused by a real PDF page boundary.
    /// Internal fit-width slice boundaries always return zero.
    pub const fn leading_page_gap(self) -> f32 {
        self.leading_page_gap
    }

    pub fn page_point_to_column(self, point: (f32, f32)) -> Option<(f32, f32)> {
        if !point.0.is_finite() || !point.1.is_finite() || point.0 < self.page_rect.left() || point.0 > self.page_rect.right() || point.1 < self.page_rect.top() {
            return None;
        }
        let includes_bottom = self.page_column_index + 1 == self.page_column_count;
        if point.1 > self.page_rect.bottom() || (!includes_bottom && point.1 >= self.page_rect.bottom()) {
            return None;
        }
        Some(((point.0 - self.page_rect.left()) / self.page_rect.width().max(f32::EPSILON), (point.1 - self.page_rect.top()) / self.page_rect.height().max(f32::EPSILON)))
    }

    pub fn column_point_to_page(self, point: (f32, f32)) -> (f32, f32) {
        (self.page_rect.left() + point.0.clamp(0.0, 1.0) * self.page_rect.width(), self.page_rect.top() + point.1.clamp(0.0, 1.0) * self.page_rect.height())
    }

    pub fn page_rect_to_column(self, rect: NormalizedRect) -> Option<NormalizedRect> {
        let left = rect.left().max(self.page_rect.left());
        let top = rect.top().max(self.page_rect.top());
        let right = rect.right().min(self.page_rect.right());
        let bottom = rect.bottom().min(self.page_rect.bottom());
        if left >= right || top >= bottom {
            return None;
        }
        Some(NormalizedRect::new(
            (left - self.page_rect.left()) / self.page_rect.width().max(f32::EPSILON),
            (top - self.page_rect.top()) / self.page_rect.height().max(f32::EPSILON),
            (right - left) / self.page_rect.width().max(f32::EPSILON),
            (bottom - top) / self.page_rect.height().max(f32::EPSILON),
        ))
    }

    pub fn column_rect_to_page(self, rect: NormalizedRect) -> NormalizedRect {
        let top_left = self.column_point_to_page((rect.left(), rect.top()));
        let bottom_right = self.column_point_to_page((rect.right(), rect.bottom()));
        NormalizedRect::new(top_left.0, top_left.1, bottom_right.0 - top_left.0, bottom_right.1 - top_left.1)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FitWidthPlan {
    columns: Vec<FitWidthColumn>,
}

impl FitWidthPlan {
    pub fn new(pages: impl IntoIterator<Item = FitWidthPage>, viewport: FitWidthViewport) -> Result<Self, FitWidthPlanError> {
        Self::new_avoiding_breaks(pages.into_iter().map(|page| (page, &[][..])), viewport)
    }

    /// Builds a fit-width plan while moving slice boundaries away from text
    /// lines, images, and other caller-provided protected vertical bands.
    pub fn new_avoiding_breaks<'a>(pages: impl IntoIterator<Item = (FitWidthPage, &'a [(f32, f32)])>, viewport: FitWidthViewport) -> Result<Self, FitWidthPlanError> {
        Self::new_avoiding_breaks_with_page_gap(pages, viewport, 0.0)
    }

    /// Plans from a source anchor instead of from the document beginning.
    ///
    /// The returned plan retains ordinary `FitWidthColumn` geometry, but its
    /// display column zero begins at `anchor`. Callers can therefore fill one
    /// viewport at a time and retain the source position after that viewport as
    /// the next stable anchor.
    pub fn from_anchor_avoiding_breaks<'a>(pages: impl IntoIterator<Item = (FitWidthPage, &'a [(f32, f32)])>, viewport: FitWidthViewport, anchor: FitWidthAnchor) -> Result<Self, FitWidthPlanError> {
        let mut columns = Vec::new();
        let mut display_column_index = 0;
        let mut used_display_height = 0.0;
        let mut found_anchor = false;
        for (page, protected_bands) in pages {
            if !found_anchor {
                if page.page_index() != anchor.page_index() {
                    continue;
                }
                found_anchor = true;
                append_fit_width_columns_from(&mut columns, page, viewport, protected_bands, 0.0, anchor.page_y(), &mut display_column_index, &mut used_display_height)?;
            } else {
                append_fit_width_columns_from(&mut columns, page, viewport, protected_bands, 0.0, 0.0, &mut display_column_index, &mut used_display_height)?;
            }
        }
        Ok(Self { columns })
    }

    /// Plans exactly one visible reading viewport from `anchor`.
    ///
    /// The plan may retain later columns for cheap look-ahead, but `end` is
    /// always the source position at the first column outside `column_count`.
    pub fn viewport_window_from_anchor_avoiding_breaks<'a>(
        pages: impl IntoIterator<Item = (FitWidthPage, &'a [(f32, f32)])>, viewport: FitWidthViewport, anchor: FitWidthAnchor, column_count: usize,
    ) -> Result<FitWidthViewportWindow, FitWidthPlanError> {
        let plan = Self::from_anchor_avoiding_breaks(pages, viewport, anchor)?;
        let end = plan.anchor_for_display_column(column_count.max(1));
        Ok(FitWidthViewportWindow { start: anchor, end, plan })
    }

    /// Builds a fit-width plan with vertical spacing at real PDF page
    /// boundaries. A column may contain several complete pages; only a page
    /// that cannot fit is split into its following display column.
    pub fn new_avoiding_breaks_with_page_gap<'a>(pages: impl IntoIterator<Item = (FitWidthPage, &'a [(f32, f32)])>, viewport: FitWidthViewport, page_gap: f32) -> Result<Self, FitWidthPlanError> {
        let mut columns = Vec::new();
        let mut display_column_index = 0;
        let mut used_display_height = 0.0;
        for (page, protected_bands) in pages {
            let leading_page_gap = if columns.is_empty() || !page_gap.is_finite() { 0.0 } else { page_gap.max(0.0).min((viewport.column_height - f32::EPSILON).max(0.0)) };
            if leading_page_gap > 0.0 && used_display_height + leading_page_gap > viewport.column_height {
                display_column_index += 1;
                used_display_height = 0.0;
            }
            used_display_height += leading_page_gap;
            append_fit_width_columns_from(&mut columns, page, viewport, protected_bands, leading_page_gap, 0.0, &mut display_column_index, &mut used_display_height)?;
        }
        Ok(Self { columns })
    }

    /// Fits every complete page inside the available width and height.
    pub fn fit_page(pages: impl IntoIterator<Item = FitWidthPage>, viewport: FitWidthViewport) -> Self {
        let mut columns = Vec::new();
        for page in pages {
            let display_width = viewport.column_width.min(viewport.column_height * page.width / page.height);
            let display_height = display_width * page.height / page.width;
            columns.push(FitWidthColumn {
                flow_index: columns.len(),
                display_column_index: columns.len(),
                page_index: page.page_index,
                page_column_index: 0,
                page_column_count: 1,
                page_rect: NormalizedRect::new(0.0, 0.0, 1.0, 1.0),
                display_width,
                display_height,
                leading_page_gap: 0.0,
            });
        }
        Self { columns }
    }

    pub fn columns(&self) -> &[FitWidthColumn] {
        &self.columns
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// The source position at which this display column begins.
    ///
    /// This is the end anchor for a viewport that owns all preceding display
    /// columns. Empty tails are intentionally skipped: an anchor always lands
    /// on renderable PDF content.
    pub fn anchor_for_display_column(&self, display_column_index: usize) -> Option<FitWidthAnchor> {
        self.columns.iter().find(|column| column.display_column_index == display_column_index).map(|column| FitWidthAnchor::new(column.page_index, column.page_rect.top()))
    }
}

fn append_fit_width_columns_from(
    columns: &mut Vec<FitWidthColumn>, page: FitWidthPage, viewport: FitWidthViewport, protected_bands: &[(f32, f32)], leading_page_gap: f32, initial_top: f32, display_column_index: &mut usize, used_display_height: &mut f32,
) -> Result<(), FitWidthPlanError> {
    let mut leading_page_gap = leading_page_gap;
    let scaled_page_height = (page.height as f64 / page.width as f64 * viewport.column_width as f64) as f32;
    let estimated_columns = (scaled_page_height / viewport.column_height).ceil().max(1.0);
    if estimated_columns > MAX_FIT_WIDTH_COLUMNS_PER_PAGE as f32 {
        return Err(FitWidthPlanError::TooManyColumns { page_index: page.page_index, required_columns: estimated_columns.min(u32::MAX as f32) as u32 });
    }

    let mut segments = Vec::new();
    let mut top = initial_top.clamp(0.0, 1.0);
    while top < 1.0 {
        let available_height = (viewport.column_height - *used_display_height).max(0.0);
        if available_height <= f32::EPSILON {
            *display_column_index += 1;
            *used_display_height = 0.0;
            leading_page_gap = 0.0;
            continue;
        }
        let ideal_bottom = (top + available_height / scaled_page_height).min(1.0);
        let bottom = safe_slice_boundary(top, ideal_bottom, protected_bands);
        if bottom <= top + f32::EPSILON {
            return Err(FitWidthPlanError::TooManyColumns { page_index: page.page_index, required_columns: MAX_FIT_WIDTH_COLUMNS_PER_PAGE });
        }
        let display_height = (bottom - top) * scaled_page_height;
        if *used_display_height > 0.0 && display_height > available_height + 0.5 {
            *display_column_index += 1;
            *used_display_height = 0.0;
            // The unused tail of the preceding display column already
            // represents the real-page boundary gap.
            leading_page_gap = 0.0;
            continue;
        }
        let segment_leading_gap = if segments.is_empty() { leading_page_gap } else { 0.0 };
        segments.push((top, bottom, display_height, segment_leading_gap, *display_column_index));
        *used_display_height += display_height;
        top = bottom.min(1.0);
        if segments.len() > MAX_FIT_WIDTH_COLUMNS_PER_PAGE as usize {
            return Err(FitWidthPlanError::TooManyColumns { page_index: page.page_index, required_columns: MAX_FIT_WIDTH_COLUMNS_PER_PAGE });
        }
        if top < 1.0 || *used_display_height >= viewport.column_height - 0.5 {
            *display_column_index += 1;
            *used_display_height = 0.0;
        }
    }

    let page_column_count = segments.len() as u32;
    for (page_column_index, (top, bottom, display_height, leading_page_gap, segment_display_column)) in segments.into_iter().enumerate() {
        columns.push(FitWidthColumn {
            flow_index: columns.len(),
            display_column_index: segment_display_column,
            page_index: page.page_index,
            page_column_index: page_column_index as u32,
            page_column_count,
            page_rect: NormalizedRect::new(0.0, top, 1.0, bottom - top),
            display_width: viewport.column_width,
            display_height,
            leading_page_gap,
        });
    }
    Ok(())
}

fn safe_slice_boundary(top: f32, ideal_bottom: f32, protected_bands: &[(f32, f32)]) -> f32 {
    if ideal_bottom >= 1.0 {
        return 1.0;
    }
    let Some((protected_top, _)) =
        protected_bands.iter().copied().map(|(top, bottom)| (top.clamp(0.0, 1.0), bottom.clamp(0.0, 1.0))).find(|(protected_top, protected_bottom)| *protected_top < ideal_bottom && ideal_bottom < *protected_bottom)
    else {
        return ideal_bottom;
    };
    // Keep a text line or ordinary image whole by ending the column before
    // the protected band, even when that leaves a short tail. The tail belongs
    // to this display column; a following page or slice must not fill it.
    // A band that starts at the column top is larger than the column only in
    // exceptional cases (for example a scanned full-page image), where a
    // split is the only way to make progress.
    let before = protected_top - top;
    if before > f32::EPSILON { protected_top } else { ideal_bottom }
}

/// Reverse counterpart to [`safe_slice_boundary`].
fn safe_slice_start(ideal_top: f32, bottom: f32, protected_bands: &[(f32, f32)]) -> f32 {
    let ideal_top = ideal_top.clamp(0.0, bottom);
    let Some((_, protected_bottom)) = protected_bands.iter().copied().map(|(top, bottom)| (top.clamp(0.0, 1.0), bottom.clamp(0.0, 1.0))).find(|(protected_top, protected_bottom)| *protected_top < ideal_top && ideal_top < *protected_bottom)
    else {
        return ideal_top;
    };
    if protected_bottom < bottom - f32::EPSILON { protected_bottom } else { ideal_top }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitWidthPlanError {
    TooManyColumns { page_index: usize, required_columns: u32 },
}

impl std::fmt::Display for FitWidthPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyColumns { page_index, required_columns } => write!(formatter, "PDF page {} would require {required_columns} fit-width columns", page_index + 1),
        }
    }
}

impl std::error::Error for FitWidthPlanError {}

fn valid_layout_extent(width: f32, height: f32) -> bool {
    width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageRotation {
    #[default]
    None,
    Clockwise90,
    Clockwise180,
    Clockwise270,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageTransform {
    crop: PixelCrop,
    rotation: PageRotation,
}

impl PageTransform {
    pub const fn new(crop: PixelCrop, rotation: PageRotation) -> Self {
        Self { crop, rotation }
    }

    pub fn full_point_to_display(self, point: (f32, f32)) -> (f32, f32) {
        rotate_point(self.crop.full_point_to_display(point), self.rotation)
    }

    pub fn display_point_to_full(self, point: (f32, f32)) -> (f32, f32) {
        self.crop.display_point_to_full(rotate_point_inverse(point, self.rotation))
    }

    pub fn full_rect_to_display(self, rect: NormalizedRect) -> Option<NormalizedRect> {
        self.crop.full_rect_to_display(rect).map(|rect| rotate_rect(rect, self.rotation))
    }

    pub fn display_rect_to_full(self, rect: NormalizedRect) -> NormalizedRect {
        self.crop.display_rect_to_full(rotate_rect_inverse(rect, self.rotation))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextCharacter {
    source_index: usize,
    character: char,
    bounds: NormalizedRect,
}

impl TextCharacter {
    pub const fn new(source_index: usize, character: char, bounds: NormalizedRect) -> Self {
        Self { source_index, character, bounds }
    }

    pub const fn source_index(self) -> usize {
        self.source_index
    }

    pub const fn character(self) -> char {
        self.character
    }

    pub const fn bounds(self) -> NormalizedRect {
        self.bounds
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionGranularity {
    #[default]
    Character,
    Word,
    Paragraph,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionOptions {
    granularity: SelectionGranularity,
    anchor_max_distance_squared: Option<f32>,
    focus_max_distance_squared: Option<f32>,
    minimum_line_overlap: f32,
    maximum_line_gap: f32,
}

impl SelectionOptions {
    pub fn with_granularity(mut self, granularity: SelectionGranularity) -> Self {
        self.granularity = granularity;
        self
    }

    pub fn with_anchor_max_distance_squared(mut self, distance: Option<f32>) -> Self {
        self.anchor_max_distance_squared = distance;
        self
    }

    pub fn with_focus_max_distance_squared(mut self, distance: Option<f32>) -> Self {
        self.focus_max_distance_squared = distance;
        self
    }

    pub fn granularity(self) -> SelectionGranularity {
        self.granularity
    }
}

impl Default for SelectionOptions {
    fn default() -> Self {
        Self { granularity: SelectionGranularity::Character, anchor_max_distance_squared: Some(DEFAULT_HIT_DISTANCE_SQUARED), focus_max_distance_squared: None, minimum_line_overlap: 0.3, maximum_line_gap: 0.05 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextHit {
    position: usize,
    source_index: usize,
}

impl TextHit {
    pub const fn position(self) -> usize {
        self.position
    }

    pub const fn source_index(self) -> usize {
        self.source_index
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageTextGeometry {
    characters: Vec<TextCharacter>,
}

impl PageTextGeometry {
    pub fn new(characters: impl IntoIterator<Item = TextCharacter>) -> Self {
        Self { characters: characters.into_iter().collect() }
    }

    pub fn characters(&self) -> &[TextCharacter] {
        &self.characters
    }

    pub fn hit_test(&self, point: (f32, f32), max_distance_squared: Option<f32>) -> Option<TextHit> {
        self.characters
            .iter()
            .enumerate()
            .filter(|(_, character)| !character.character.is_control())
            .map(|(position, character)| (position, character, character.bounds.distance_squared(point)))
            .min_by(|left, right| left.2.total_cmp(&right.2))
            .filter(|(_, _, distance)| max_distance_squared.is_none_or(|maximum| *distance <= maximum))
            .map(|(position, character, _)| TextHit { position, source_index: character.source_index })
    }

    pub fn select(&self, anchor: (f32, f32), focus: (f32, f32)) -> Option<PageSelection> {
        self.select_with_options(anchor, focus, SelectionOptions::default())
    }

    pub fn select_with_options(&self, anchor: (f32, f32), focus: (f32, f32), options: SelectionOptions) -> Option<PageSelection> {
        let anchor = self.hit_test(anchor, options.anchor_max_distance_squared)?;
        let focus = self.hit_test(focus, options.focus_max_distance_squared)?;
        self.select_positions(anchor.position, focus.position, options)
    }

    pub fn select_from_source_index_with_options(&self, anchor_source_index: usize, focus: (f32, f32), options: SelectionOptions) -> Option<PageSelection> {
        let anchor = self.characters.iter().position(|character| character.source_index == anchor_source_index)?;
        let focus = self.hit_test(focus, options.focus_max_distance_squared)?;
        self.select_positions(anchor, focus.position, options)
    }

    pub fn transformed(&self, transform: PageTransform) -> Self {
        Self::new(self.characters.iter().filter_map(|character| transform.full_rect_to_display(character.bounds).map(|bounds| TextCharacter::new(character.source_index, character.character, bounds))))
    }

    fn select_positions(&self, anchor: usize, focus: usize, options: SelectionOptions) -> Option<PageSelection> {
        let (first, last) = if anchor <= focus { (anchor, focus) } else { (focus, anchor) };
        let (first, last) = expand_range(&self.characters, first, last, options.granularity);
        let selected = self.characters.get(first..=last)?;
        let text = selected.iter().map(|character| character.character).collect::<String>();
        let rects = merge_line_rects_with_options(selected.iter().filter(|character| !character.character.is_control()).map(|character| character.bounds), options.minimum_line_overlap, options.maximum_line_gap);
        let first_source_index = selected.first()?.source_index;
        let last_source_index = selected.last()?.source_index;
        (!text.is_empty() && !rects.is_empty()).then_some(PageSelection { first_source_index, last_source_index, text, rects })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageSelection {
    first_source_index: usize,
    last_source_index: usize,
    text: String,
    rects: Vec<NormalizedRect>,
}

impl PageSelection {
    pub fn first_source_index(&self) -> usize {
        self.first_source_index
    }

    pub fn last_source_index(&self) -> usize {
        self.last_source_index
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn rects(&self) -> &[NormalizedRect] {
        &self.rects
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PagePoint {
    page_index: usize,
    point: (f32, f32),
}

impl PagePoint {
    pub const fn new(page_index: usize, point: (f32, f32)) -> Self {
        Self { page_index, point }
    }

    pub const fn page_index(self) -> usize {
        self.page_index
    }

    pub const fn point(self) -> (f32, f32) {
        self.point
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentPageSelection {
    page_index: usize,
    selection: PageSelection,
}

impl DocumentPageSelection {
    pub const fn page_index(&self) -> usize {
        self.page_index
    }

    pub const fn selection(&self) -> &PageSelection {
        &self.selection
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentSelection {
    text: String,
    pages: Vec<DocumentPageSelection>,
}

impl DocumentSelection {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn pages(&self) -> &[DocumentPageSelection] {
        &self.pages
    }
}

pub fn select_document(pages: &[PageTextGeometry], anchor: PagePoint, focus: PagePoint, options: SelectionOptions) -> Option<DocumentSelection> {
    let anchor_hit = pages.get(anchor.page_index)?.hit_test(anchor.point, options.anchor_max_distance_squared)?;
    let focus_hit = pages.get(focus.page_index)?.hit_test(focus.point, options.focus_max_distance_squared)?;
    let (start_page, start_position, end_page, end_position) = if (anchor.page_index, anchor_hit.position) <= (focus.page_index, focus_hit.position) {
        (anchor.page_index, anchor_hit.position, focus.page_index, focus_hit.position)
    } else {
        (focus.page_index, focus_hit.position, anchor.page_index, anchor_hit.position)
    };
    let selections = (start_page..=end_page)
        .filter_map(|page_index| {
            let page = pages.get(page_index)?;
            let first = if page_index == start_page { start_position } else { first_selectable_position(page.characters())? };
            let last = if page_index == end_page { end_position } else { last_selectable_position(page.characters())? };
            page.select_positions(first, last, options).map(|selection| DocumentPageSelection { page_index, selection })
        })
        .collect::<Vec<_>>();
    if selections.is_empty() {
        return None;
    }
    let text = selections.iter().map(|page| page.selection.text()).collect::<Vec<_>>().join("\n");
    Some(DocumentSelection { text, pages: selections })
}

pub fn merge_line_rects(rects: impl IntoIterator<Item = NormalizedRect>) -> Vec<NormalizedRect> {
    merge_line_rects_with_options(rects, 0.3, 0.05)
}

pub fn detect_rgba(rgba: &[u8], width: u32, height: u32, options: CropOptions) -> PixelCrop {
    if width == 0 || height == 0 || rgba.len() < width as usize * height as usize * 4 {
        return PixelCrop::full(width, height);
    }
    let background = page_background(rgba, width, height);
    let mut bounds = None::<(u32, u32, u32, u32)>;
    rgba.chunks_exact(4).take(width as usize * height as usize).enumerate().for_each(|(index, pixel)| {
        if !is_content_pixel(pixel, background, options.content_difference_threshold) {
            return;
        }
        let x = index as u32 % width;
        let y = index as u32 / width;
        bounds = Some(match bounds {
            None => (x, y, x, y),
            Some((left, top, right, bottom)) => (left.min(x), top.min(y), right.max(x), bottom.max(y)),
        });
    });
    let Some((left, top, right, bottom)) = bounds else {
        return PixelCrop::full(width, height);
    };
    let left = left.saturating_sub(options.padding_pixels);
    let top = top.saturating_sub(options.padding_pixels);
    let right = right.saturating_add(options.padding_pixels).min(width.saturating_sub(1));
    let bottom = bottom.saturating_add(options.padding_pixels).min(height.saturating_sub(1));
    PixelCrop::new(width, height, left, top, right.saturating_sub(left).saturating_add(1), bottom.saturating_sub(top).saturating_add(1))
}

fn rotate_point(point: (f32, f32), rotation: PageRotation) -> (f32, f32) {
    match rotation {
        PageRotation::None => point,
        PageRotation::Clockwise90 => (1.0 - point.1, point.0),
        PageRotation::Clockwise180 => (1.0 - point.0, 1.0 - point.1),
        PageRotation::Clockwise270 => (point.1, 1.0 - point.0),
    }
}

fn rotate_point_inverse(point: (f32, f32), rotation: PageRotation) -> (f32, f32) {
    match rotation {
        PageRotation::None => point,
        PageRotation::Clockwise90 => (point.1, 1.0 - point.0),
        PageRotation::Clockwise180 => (1.0 - point.0, 1.0 - point.1),
        PageRotation::Clockwise270 => (1.0 - point.1, point.0),
    }
}

fn rotate_rect(rect: NormalizedRect, rotation: PageRotation) -> NormalizedRect {
    match rotation {
        PageRotation::None => rect,
        PageRotation::Clockwise90 => NormalizedRect::new(1.0 - rect.bottom(), rect.left, rect.height, rect.width),
        PageRotation::Clockwise180 => NormalizedRect::new(1.0 - rect.right(), 1.0 - rect.bottom(), rect.width, rect.height),
        PageRotation::Clockwise270 => NormalizedRect::new(rect.top, 1.0 - rect.right(), rect.height, rect.width),
    }
}

fn rotate_rect_inverse(rect: NormalizedRect, rotation: PageRotation) -> NormalizedRect {
    match rotation {
        PageRotation::None => rect,
        PageRotation::Clockwise90 => rotate_rect(rect, PageRotation::Clockwise270),
        PageRotation::Clockwise180 => rotate_rect(rect, PageRotation::Clockwise180),
        PageRotation::Clockwise270 => rotate_rect(rect, PageRotation::Clockwise90),
    }
}

fn merge_line_rects_with_options(rects: impl IntoIterator<Item = NormalizedRect>, minimum_overlap: f32, maximum_gap: f32) -> Vec<NormalizedRect> {
    rects.into_iter().fold(Vec::new(), |mut lines, rect| {
        if let Some(last) = lines.last_mut().filter(|last| same_text_line(**last, rect, minimum_overlap, maximum_gap)) {
            *last = last.union(rect);
        } else {
            lines.push(rect);
        }
        lines
    })
}

fn same_text_line(left: NormalizedRect, right: NormalizedRect, minimum_overlap: f32, maximum_gap: f32) -> bool {
    let overlap = left.bottom().min(right.bottom()) - left.top.max(right.top);
    let gap = if left.right() < right.left {
        right.left - left.right()
    } else if right.right() < left.left {
        left.left - right.right()
    } else {
        0.0
    };
    overlap > left.height.min(right.height) * minimum_overlap && gap <= maximum_gap
}

fn expand_range(characters: &[TextCharacter], first: usize, last: usize, granularity: SelectionGranularity) -> (usize, usize) {
    match granularity {
        SelectionGranularity::Character => (first, last),
        SelectionGranularity::Word => (word_start(characters, first), word_end(characters, last)),
        SelectionGranularity::Paragraph => (paragraph_start(characters, first), paragraph_end(characters, last)),
    }
}

fn word_start(characters: &[TextCharacter], position: usize) -> usize {
    let class = word_class(characters[position].character);
    (0..position).rev().take_while(|candidate| word_class(characters[*candidate].character) == class).last().unwrap_or(position)
}

fn word_end(characters: &[TextCharacter], position: usize) -> usize {
    let class = word_class(characters[position].character);
    ((position + 1)..characters.len()).take_while(|candidate| word_class(characters[*candidate].character) == class).last().unwrap_or(position)
}

fn paragraph_start(characters: &[TextCharacter], position: usize) -> usize {
    (0..position).rev().find_map(|candidate| paragraph_break_end(characters, candidate).filter(|end| *end < position)).map_or(0, |separator_end| separator_end + 1)
}

fn paragraph_end(characters: &[TextCharacter], position: usize) -> usize {
    (position..characters.len()).find_map(|candidate| paragraph_break_end(characters, candidate).map(|_| candidate)).unwrap_or_else(|| characters.len().saturating_sub(1))
}

fn paragraph_break_end(characters: &[TextCharacter], position: usize) -> Option<usize> {
    if characters.get(position).is_none_or(|character| character.character != '\n') {
        return None;
    }
    ((position + 1)..characters.len()).find(|candidate| !matches!(characters[*candidate].character, '\r' | ' ' | '\t')).filter(|candidate| characters[*candidate].character == '\n')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordClass {
    Word,
    Whitespace,
    Punctuation,
}

fn word_class(character: char) -> WordClass {
    if character.is_alphanumeric() || matches!(character, '_' | '\'' | '’') {
        WordClass::Word
    } else if character.is_whitespace() {
        WordClass::Whitespace
    } else {
        WordClass::Punctuation
    }
}

fn first_selectable_position(characters: &[TextCharacter]) -> Option<usize> {
    characters.iter().position(|character| !character.character.is_control())
}

fn last_selectable_position(characters: &[TextCharacter]) -> Option<usize> {
    characters.iter().rposition(|character| !character.character.is_control())
}

fn page_background(rgba: &[u8], width: u32, height: u32) -> [u8; 3] {
    let points = [(0, 0), (width.saturating_sub(1), 0), (0, height.saturating_sub(1)), (width.saturating_sub(1), height.saturating_sub(1))];
    std::array::from_fn(|channel| {
        let mut values = points.map(|(x, y)| rgba[((y * width + x) * 4) as usize + channel]);
        values.sort_unstable();
        ((values[1] as u16 + values[2] as u16) / 2) as u8
    })
}

fn is_content_pixel(pixel: &[u8], background: [u8; 3], threshold: u8) -> bool {
    pixel[3] > 0 && pixel[..3].iter().zip(background).any(|(channel, background)| channel.abs_diff(background) >= threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        std::iter::repeat_n(color, width as usize * height as usize).flatten().collect()
    }

    fn fill(rgba: &mut [u8], width: u32, left: u32, top: u32, right: u32, bottom: u32, color: [u8; 4]) {
        (top..bottom).for_each(|y| {
            (left..right).for_each(|x| {
                rgba[((y * width + x) * 4) as usize..((y * width + x) * 4 + 4) as usize].copy_from_slice(&color);
            });
        });
    }

    fn text_geometry(text: &str, page_top: f32) -> PageTextGeometry {
        PageTextGeometry::new(text.chars().enumerate().map(|(index, character)| {
            let line = text[..index].chars().filter(|candidate| *candidate == '\n').count() as f32;
            let column = text[..index].chars().rev().take_while(|candidate| *candidate != '\n').count() as f32;
            TextCharacter::new(index, character, NormalizedRect::new(0.1 + column * 0.03, page_top + line * 0.05, 0.025, 0.03))
        }))
    }

    #[test]
    fn content_detection_preserves_padding() {
        let mut rgba = image(100, 80, [255, 255, 255, 255]);
        fill(&mut rgba, 100, 40, 20, 60, 30, [0, 0, 0, 255]);
        assert_eq!(detect_rgba(&rgba, 100, 80, CropOptions::default()), PixelCrop::new(100, 80, 28, 8, 44, 34));
    }

    #[test]
    fn blank_page_is_not_trimmed() {
        let rgba = image(20, 30, [243, 244, 242, 255]);
        assert_eq!(detect_rgba(&rgba, 20, 30, CropOptions::default()), PixelCrop::full(20, 30));
    }

    #[test]
    fn fit_width_keeps_a_short_page_in_one_column() {
        let page = FitWidthPage::new(4, 600.0, 800.0).expect("valid page");
        let viewport = FitWidthViewport::new(450.0, 700.0).expect("valid viewport");
        let plan = FitWidthPlan::new([page], viewport).expect("fit-width plan");
        assert_eq!(plan.len(), 1);
        let column = plan.columns()[0];
        assert_eq!(column.flow_index(), 0);
        assert_eq!(column.page_index(), 4);
        assert_eq!(column.page_column_index(), 0);
        assert_eq!(column.page_column_count(), 1);
        assert_eq!(column.page_rect(), NormalizedRect::new(0.0, 0.0, 1.0, 1.0));
        assert!((column.display_width() - 450.0).abs() < 0.001);
        assert!((column.display_height() - 600.0).abs() < 0.001);
    }

    #[test]
    fn fit_width_flows_a_tall_page_into_consecutive_columns() {
        let page = FitWidthPage::new(2, 600.0, 1_200.0).expect("valid page");
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("valid viewport");
        let plan = FitWidthPlan::new([page], viewport).expect("fit-width plan");
        assert_eq!(plan.len(), 2);
        let first = plan.columns()[0];
        let second = plan.columns()[1];
        assert_eq!(first.page_rect(), NormalizedRect::new(0.0, 0.0, 1.0, 0.5));
        assert_eq!(second.page_rect(), NormalizedRect::new(0.0, 0.5, 1.0, 0.5));
        assert!(first.page_point_to_column((0.25, 0.5)).is_none(), "a shared boundary belongs to the following column");
        assert_eq!(second.page_point_to_column((0.25, 0.5)), Some((0.25, 0.0)));
        assert_eq!(second.column_point_to_page((0.25, 1.0)), (0.25, 1.0));
    }

    #[test]
    fn anchored_plan_starts_at_the_source_position_not_a_previous_display_column() {
        let pages = [FitWidthPage::new(0, 600.0, 1_200.0).expect("first page"), FitWidthPage::new(1, 600.0, 600.0).expect("second page")];
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("viewport");
        let plan = FitWidthPlan::from_anchor_avoiding_breaks(pages.into_iter().map(|page| (page, &[][..])), viewport, FitWidthAnchor::new(0, 0.25)).expect("anchored plan");
        assert_eq!(plan.columns()[0].display_column_index(), 0);
        assert_eq!(plan.columns()[0].page_rect(), NormalizedRect::new(0.0, 0.25, 1.0, 0.5));
        assert_eq!(plan.columns()[1].page_rect(), NormalizedRect::new(0.0, 0.75, 1.0, 0.25));
        assert_eq!(plan.columns()[2].page_index(), 1);
    }

    #[test]
    fn anchored_viewport_window_exposes_the_next_viewports_source_anchor() {
        let pages = [FitWidthPage::new(0, 600.0, 1_200.0).expect("first page"), FitWidthPage::new(1, 600.0, 600.0).expect("second page")];
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("viewport");
        let window = FitWidthPlan::viewport_window_from_anchor_avoiding_breaks(pages.into_iter().map(|page| (page, &[][..])), viewport, FitWidthAnchor::new(0, 0.0), 1).expect("viewport window");
        assert_eq!(window.start(), FitWidthAnchor::new(0, 0.0));
        assert_eq!(window.end(), Some(FitWidthAnchor::new(0, 0.5)));
        assert_eq!(window.plan().columns()[0].page_rect(), NormalizedRect::new(0.0, 0.0, 1.0, 0.5));
    }

    #[test]
    fn anchor_pager_promotes_the_end_anchor_and_restores_the_previous_window() {
        let pages = [(FitWidthPage::new(0, 600.0, 1_200.0).expect("first page"), Vec::new()), (FitWidthPage::new(1, 600.0, 600.0).expect("second page"), Vec::new())];
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("viewport");
        let mut pager = FitWidthAnchorPager::new(pages, viewport, 1, FitWidthAnchor::new(0, 0.0)).expect("pager");
        assert_eq!(pager.window().start(), FitWidthAnchor::new(0, 0.0));
        assert_eq!(pager.window().end(), Some(FitWidthAnchor::new(0, 0.5)));
        assert!(pager.forward().expect("forward"));
        assert_eq!(pager.window().start(), FitWidthAnchor::new(0, 0.5));
        assert_eq!(pager.window().end(), Some(FitWidthAnchor::new(1, 0.0)));
        assert!(pager.backward().expect("backward"));
        assert_eq!(pager.window().start(), FitWidthAnchor::new(0, 0.0));
    }

    #[test]
    fn anchor_pager_crosses_omitted_source_pages() {
        let pages = [(FitWidthPage::new(0, 600.0, 600.0).expect("first page"), Vec::new()), (FitWidthPage::new(2, 600.0, 600.0).expect("third page"), Vec::new())];
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("viewport");
        let mut pager = FitWidthAnchorPager::new(pages, viewport, 1, FitWidthAnchor::new(0, 0.0)).expect("pager");
        assert_eq!(pager.window().end(), Some(FitWidthAnchor::new(2, 0.0)));
        assert!(pager.forward().expect("forward"));
        assert_eq!(pager.window().start(), FitWidthAnchor::new(2, 0.0));
        assert!(pager.backward().expect("backward"));
        assert_eq!(pager.window().start(), FitWidthAnchor::new(0, 0.0));
    }

    #[test]
    fn anchor_pager_reflows_from_a_continuous_scroll_anchor() {
        let pages = [(FitWidthPage::new(0, 600.0, 1_200.0).expect("page"), Vec::new())];
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("viewport");
        let mut pager = FitWidthAnchorPager::new(pages, viewport, 1, FitWidthAnchor::new(0, 0.0)).expect("pager");
        pager.seek(FitWidthAnchor::new(0, 0.2), true).expect("reflow");
        assert_eq!(pager.window().start(), FitWidthAnchor::new(0, 0.2));
        assert_eq!(pager.window().plan().columns()[0].page_rect(), NormalizedRect::new(0.0, 0.2, 1.0, 0.5));
        assert!(pager.backward().expect("backward"));
        assert_eq!(pager.window().start(), FitWidthAnchor::new(0, 0.0));
    }

    #[test]
    fn fit_width_preserves_a_short_final_slice_and_overlay_coordinates() {
        let page = FitWidthPage::new(0, 600.0, 1_500.0).expect("valid page");
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("valid viewport");
        let plan = FitWidthPlan::new([page], viewport).expect("fit-width plan");
        assert_eq!(plan.len(), 3);
        let final_column = plan.columns()[2];
        assert!((final_column.page_rect().top() - 0.8).abs() < 0.0001);
        assert!((final_column.page_rect().height() - 0.2).abs() < 0.0001);
        assert!((final_column.display_height() - 150.0).abs() < 0.001);
        let page_rect = NormalizedRect::new(0.2, 0.85, 0.4, 0.1);
        let column_rect = final_column.page_rect_to_column(page_rect).expect("visible overlay");
        let restored = final_column.column_rect_to_page(column_rect);
        assert!((restored.left() - page_rect.left()).abs() < 0.0001);
        assert!((restored.top() - page_rect.top()).abs() < 0.0001);
        assert!((restored.width() - page_rect.width()).abs() < 0.0001);
        assert!((restored.height() - page_rect.height()).abs() < 0.0001);
    }

    #[test]
    fn annotation_rect_round_trips_from_full_page_through_trim_and_wrapped_column() {
        let crop = PixelCrop::new(1_000, 1_400, 100, 140, 800, 1_120);
        let full_rect = NormalizedRect::new(0.25, 0.55, 0.3, 0.08);
        let display_rect = crop.full_rect_to_display(full_rect).expect("annotation lies inside the trimmed page");
        let page = FitWidthPage::new(0, 800.0, 1_120.0).expect("trimmed page");
        let viewport = FitWidthViewport::new(400.0, 300.0).expect("viewport");
        let plan = FitWidthPlan::new([page], viewport).expect("fit-width plan");
        let column = plan.columns().iter().copied().find(|column| column.page_rect_to_column(display_rect).is_some()).expect("annotation column");
        let column_rect = column.page_rect_to_column(display_rect).expect("column annotation");
        let restored_display = column.column_rect_to_page(column_rect);
        let restored_full = crop.display_rect_to_full(restored_display);
        assert!((restored_full.left() - full_rect.left()).abs() < 0.0001);
        assert!((restored_full.top() - full_rect.top()).abs() < 0.0001);
        assert!((restored_full.width() - full_rect.width()).abs() < 0.0001);
        assert!((restored_full.height() - full_rect.height()).abs() < 0.0001);
    }

    #[test]
    fn fit_width_assigns_stable_flow_indices_across_pages() {
        let pages = [FitWidthPage::new(7, 600.0, 1_200.0).expect("page"), FitWidthPage::new(9, 600.0, 600.0).expect("page")];
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("viewport");
        let plan = FitWidthPlan::new(pages, viewport).expect("fit-width plan");
        assert_eq!(plan.columns().iter().map(|column| (column.flow_index(), column.page_index(), column.page_column_index())).collect::<Vec<_>>(), vec![(0, 7, 0), (1, 7, 1), (2, 9, 0)]);
    }

    #[test]
    fn fit_width_packs_the_next_page_below_a_short_final_slice() {
        let pages = [FitWidthPage::new(0, 600.0, 900.0).expect("first page"), FitWidthPage::new(1, 600.0, 600.0).expect("second page")];
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("viewport");
        let plan = FitWidthPlan::new(pages, viewport).expect("fit-width plan");
        assert_eq!(plan.columns().iter().map(|column| (column.page_index(), column.display_column_index())).collect::<Vec<_>>(), vec![(0, 0), (0, 1), (1, 1), (1, 2)]);
        assert!(plan.columns().iter().zip([300.0, 150.0, 150.0, 150.0]).all(|(column, expected)| (column.display_height() - expected).abs() < 0.001));
    }

    #[test]
    fn fit_width_adds_vertical_gap_only_at_real_page_boundaries() {
        let pages = [FitWidthPage::new(0, 600.0, 900.0).expect("first page"), FitWidthPage::new(1, 600.0, 600.0).expect("second page")];
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("viewport");
        let pages = pages.iter().copied().map(|page| (page, &[][..]));
        let plan = FitWidthPlan::new_avoiding_breaks_with_page_gap(pages, viewport, 12.0).expect("fit-width plan");

        assert_eq!(plan.columns().iter().map(|column| column.leading_page_gap()).collect::<Vec<_>>(), vec![0.0, 0.0, 12.0, 0.0]);
        assert_eq!(plan.columns()[1].display_column_index(), plan.columns()[2].display_column_index());
        assert!((plan.columns()[2].display_height() - 138.0).abs() < 0.001);
    }

    #[test]
    fn fit_width_moves_a_slice_boundary_before_protected_content() {
        let page = FitWidthPage::new(0, 600.0, 1_200.0).expect("valid page");
        let viewport = FitWidthViewport::new(300.0, 300.0).expect("valid viewport");
        let protected = [(0.48, 0.58)];
        let plan = FitWidthPlan::new_avoiding_breaks([(page, protected.as_slice())], viewport).expect("fit-width plan");
        assert_eq!(plan.columns().len(), 3);
        assert!((plan.columns()[0].page_rect().bottom() - 0.48).abs() < 0.0001);
        assert!((plan.columns()[1].page_rect().top() - 0.48).abs() < 0.0001);
    }

    #[test]
    fn a_slice_never_grows_taller_than_the_column_that_holds_it() {
        // The reader paints one logical column per viewport height and steps
        // the flow by that fixed stride, so a taller slice is clipped and the
        // rest of it cannot be scrolled to. A protected band wider than the
        // column has to be split rather than kept whole.
        let page = FitWidthPage::new(0, 600.0, 850.0).expect("A4-ish page");
        let viewport = FitWidthViewport::new(948.0, 800.0).expect("viewport");
        let scanned_sheet = [(0.0, 1.0)];

        let plan = FitWidthPlan::new_avoiding_breaks([(page, scanned_sheet.as_slice())], viewport).expect("fit-width plan");

        assert!(
            plan.columns().iter().all(|column| column.display_height() <= viewport.column_height() + 0.5),
            "a full-page image must be split across columns, got heights {:?}",
            plan.columns().iter().map(|column| column.display_height()).collect::<Vec<_>>()
        );
        assert!((plan.columns().last().expect("at least one column").page_rect().bottom() - 1.0).abs() < 0.0001, "the slices must still cover the whole page");
    }

    #[test]
    fn fit_page_preserves_the_whole_page_and_both_viewport_limits() {
        let portrait = FitWidthPage::new(0, 600.0, 800.0).expect("portrait");
        let landscape = FitWidthPage::new(1, 1_000.0, 500.0).expect("landscape");
        let viewport = FitWidthViewport::new(900.0, 600.0).expect("viewport");
        let plan = FitWidthPlan::fit_page([portrait, landscape], viewport);
        assert_eq!(plan.columns().len(), 2);
        assert_eq!(plan.columns()[0].page_rect(), NormalizedRect::new(0.0, 0.0, 1.0, 1.0));
        assert!((plan.columns()[0].display_width() - 450.0).abs() < 0.001);
        assert!((plan.columns()[0].display_height() - 600.0).abs() < 0.001);
        assert!((plan.columns()[1].display_width() - 900.0).abs() < 0.001);
        assert!((plan.columns()[1].display_height() - 450.0).abs() < 0.001);
    }

    #[test]
    fn fit_width_rejects_invalid_geometry() {
        assert!(FitWidthPage::new(0, 0.0, 100.0).is_none());
        assert!(FitWidthPage::new(0, f32::NAN, 100.0).is_none());
        assert!(FitWidthViewport::new(100.0, f32::INFINITY).is_none());
    }

    #[test]
    fn fit_width_rejects_pathological_column_counts() {
        let page = FitWidthPage::new(3, 1.0, 100_000.0).expect("valid page");
        let viewport = FitWidthViewport::new(1.0, 1.0).expect("valid viewport");
        assert_eq!(FitWidthPlan::new([page], viewport), Err(FitWidthPlanError::TooManyColumns { page_index: 3, required_columns: 100_000 }));
    }

    #[test]
    fn crop_and_rotation_round_trip_points_and_rectangles() {
        let transform = PageTransform::new(PixelCrop::new(1000, 800, 100, 80, 800, 640), PageRotation::Clockwise90);
        let point = transform.full_point_to_display((0.3, 0.5));
        let restored = transform.display_point_to_full(point);
        assert!((restored.0 - 0.3).abs() < 0.0001);
        assert!((restored.1 - 0.5).abs() < 0.0001);
        let rect = NormalizedRect::new(0.3, 0.5, 0.1, 0.05);
        let display = transform.full_rect_to_display(rect).expect("visible rectangle");
        let restored = transform.display_rect_to_full(display);
        assert!((restored.left() - rect.left()).abs() < 0.0001);
        assert!((restored.top() - rect.top()).abs() < 0.0001);
        assert!((restored.width() - rect.width()).abs() < 0.0001);
        assert!((restored.height() - rect.height()).abs() < 0.0001);
    }

    #[test]
    fn hit_testing_and_reverse_drag_share_the_same_selection() {
        let geometry = text_geometry("AB", 0.2);
        let forward = geometry.select((0.11, 0.21), (0.14, 0.21)).expect("forward selection");
        let reverse = geometry.select((0.14, 0.21), (0.11, 0.21)).expect("reverse selection");
        assert_eq!(forward, reverse);
        assert_eq!(forward.text(), "AB");
        assert_eq!(forward.rects().len(), 1);
        let pdfium_anchor = geometry.select_from_source_index_with_options(0, (0.14, 0.21), SelectionOptions::default()).expect("selection from Pdfium source index");
        assert_eq!(pdfium_anchor, forward);
    }

    #[test]
    fn line_merging_does_not_bridge_distant_columns() {
        let rects = merge_line_rects([NormalizedRect::new(0.1, 0.2, 0.05, 0.03), NormalizedRect::new(0.16, 0.2, 0.05, 0.03), NormalizedRect::new(0.7, 0.2, 0.05, 0.03)]);
        assert_eq!(rects.len(), 2);
    }

    #[test]
    fn word_and_paragraph_granularity_expand_endpoints() {
        let geometry = text_geometry("one two\n\nnext", 0.1);
        let word = geometry.select_with_options((0.22, 0.11), (0.22, 0.11), SelectionOptions::default().with_granularity(SelectionGranularity::Word)).expect("word");
        assert_eq!(word.text(), "two");
        let paragraph = geometry.select_with_options((0.11, 0.11), (0.11, 0.11), SelectionOptions::default().with_granularity(SelectionGranularity::Paragraph)).expect("paragraph");
        assert_eq!(paragraph.text(), "one two\n");
        let next_paragraph = geometry.select_with_options((0.11, 0.21), (0.11, 0.21), SelectionOptions::default().with_granularity(SelectionGranularity::Paragraph)).expect("next paragraph");
        assert_eq!(next_paragraph.text(), "next");
    }

    #[test]
    fn document_selection_spans_pages_and_preserves_page_rects() {
        let pages = [text_geometry("one", 0.1), text_geometry("two", 0.1)];
        let selection = select_document(&pages, PagePoint::new(0, (0.11, 0.11)), PagePoint::new(1, (0.17, 0.11)), SelectionOptions::default()).expect("document selection");
        assert_eq!(selection.text(), "one\ntwo");
        assert_eq!(selection.pages().len(), 2);
    }
}
