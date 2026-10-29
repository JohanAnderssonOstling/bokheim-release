//! Framework-independent PDF sessions, rendering, searching, selection, and page caching.

#[cfg(all(feature = "packaged-pdfium", not(target_os = "linux")))]
compile_error!("the packaged-pdfium feature is only supported by Linux application packages");

use book_model::{BookTocEntry, pdf_toc_target};
#[cfg(test)]
use image::RgbaImage;
pub use pdf_view_common::{
    FitWidthAnchor, FitWidthAnchorPager, FitWidthColumn, FitWidthPage, FitWidthPlan, FitWidthPlanError, FitWidthViewport, FitWidthViewportWindow, NormalizedRect, PageSelection as PdfTextSelection, PageTextGeometry, PixelCrop,
    SelectionGranularity, SelectionOptions,
};
use pdf_view_common::{PageRotation, PageTransform, TextCharacter, merge_line_rects};
use pdfium_render::prelude::*;
mod interaction_style;
mod reading;
pub use interaction_style::{ReaderInteractionStyle, RgbaColor};
pub use reading::PdfReadingFragment;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::io::SeekFrom;
use std::io::{Read, Seek};
use std::mem::{size_of, size_of_val};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

mod annotations;
mod metadata;
#[cfg(not(target_arch = "wasm32"))]
mod native;
mod prefetch;
mod runtime;
pub use metadata::{inspect_reader_metadata, inspect_reader_metadata_with_identity};
#[cfg(not(target_arch = "wasm32"))]
pub use native::PdfDocumentSession;
pub use pdf_view_common::{PdfPageSource, PdfPrefetchCancellation};
pub use pdf_view_common::{PdfReaderMetadata, PdfStoredAnnotationKind};
#[cfg(not(target_arch = "wasm32"))]
const PDFIUM_THIRD_PARTY_NOTICES: &str = include_str!(env!("PDFIUM_NOTICES_PATH"));
const DEFAULT_RENDER_WIDTH: u16 = 1400;
const MIN_RENDER_WIDTH: u16 = 320;
const MAX_RENDER_WIDTH: u16 = 3200;
/// Vertical tile stride. Rasters include one neighboring pixel at each edge
/// so texture filtering cannot introduce seams between tiles.
pub const DISPLAY_TILE_HEIGHT: u32 = 512;
const MAX_RASTER_PIXELS: u64 = 8 * 1024 * 1024;
const ELEMENT_CROP_PADDING: f32 = 0.008;

#[cfg(target_arch = "wasm32")]
pub mod browser;

static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();

pub type PdfResult<T> = Result<T, PdfError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfError {
    message: String,
}

impl PdfError {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl fmt::Display for PdfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PdfError {}

#[cfg(target_arch = "wasm32")]
pub struct PdfDocumentSession {
    document: runtime::Document,
    info: PdfDocumentInfo,
    analysis: RefCell<PageAnalysisCache>,
    page_source: Option<Arc<dyn PdfPageSource>>,
}
#[cfg(target_arch = "wasm32")]
type LocalDocumentSession = PdfDocumentSession;
#[cfg(not(target_arch = "wasm32"))]
struct LocalDocumentSession {
    document: runtime::Document,
    info: PdfDocumentInfo,
    analysis: RefCell<PageAnalysisCache>,
    page_source: Option<Arc<dyn PdfPageSource>>,
}

// Geometry is independent of raster resolution. Bound retained analysis separately
// from the display cache so reading a long document cannot retain all its text.
const ANALYSIS_CACHE_BUDGET: usize = 16 * 1024 * 1024;
const MAX_ANALYSIS_CACHE_PAGES: usize = 128;

struct PageAnalysis {
    text: Arc<PdfPageTextLayout>,
    images: Vec<NormalizedRect>,
    trim_bounds: OnceLock<Option<NormalizedRect>>,
    geometry: [OnceLock<PdfPageGeometry>; 2],
    cropped_text: OnceLock<Arc<PdfPageTextLayout>>,
}

impl PageAnalysis {
    fn byte_len(&self) -> usize {
        // Reserve for lazy cropped text and both sets of bands up front. This
        // charge must remain constant while the OnceLocks are populated.
        size_of::<Self>() + 2 * size_of_val(self.text.geometry.characters()) + self.images.capacity() * size_of::<NormalizedRect>() + 2 * (self.text.geometry.characters().len() + self.images.len()) * size_of::<(f32, f32)>()
    }
}

#[derive(Default)]
struct PageAnalysisCache {
    pages: VecDeque<(usize, Arc<PageAnalysis>)>,
    byte_len: usize,
}

impl PageAnalysisCache {
    fn get(&mut self, index: usize) -> Option<Arc<PageAnalysis>> {
        let position = self.pages.iter().position(|(page, _)| *page == index)?;
        let entry = self.pages.remove(position)?;
        let analysis = entry.1.clone();
        self.pages.push_back(entry);
        Some(analysis)
    }

    fn insert(&mut self, index: usize, analysis: Arc<PageAnalysis>) {
        let bytes = analysis.byte_len();
        if bytes > ANALYSIS_CACHE_BUDGET {
            return;
        }
        while self.byte_len + bytes > ANALYSIS_CACHE_BUDGET || self.pages.len() >= MAX_ANALYSIS_CACHE_PAGES {
            let Some((_, oldest)) = self.pages.pop_front() else { break };
            self.byte_len -= oldest.byte_len();
        }
        self.byte_len += bytes;
        self.pages.push_back((index, analysis));
    }
}

/// A page raster without reader-only text, selection, or margin analysis.
///
/// This is intentionally smaller than [`RenderedPdfPage`] so callers such as
/// cover generation do not pay the cost of constructing a full reader session.
#[derive(Clone, Debug)]
pub struct RenderedPdfImage {
    rgba: Vec<u8>,
    pixel_width: u32,
    pixel_height: u32,
    source: RenderedPdfImageSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderedPdfImageSource {
    EmbeddedThumbnail,
    RenderedPage,
}

impl RenderedPdfImage {
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    pub fn pixel_width(&self) -> u32 {
        self.pixel_width
    }

    pub fn pixel_height(&self) -> u32 {
        self.pixel_height
    }

    pub fn source(&self) -> RenderedPdfImageSource {
        self.source
    }

    pub fn into_parts(self) -> (Vec<u8>, u32, u32) {
        (self.rgba, self.pixel_width, self.pixel_height)
    }
}

/// Opens a PDF and renders only its first page.
///
/// Unlike [`PdfDocumentSession::open`], this does not inspect every page for
/// reader metadata, text geometry, repeating headers, or trim bounds.
pub fn render_first_page_image(path: impl AsRef<Path>, target_width: u16) -> PdfResult<RenderedPdfImage> {
    let path = path.as_ref();
    let reader = fs::File::open(path).map_err(|error| PdfError::new(format!("opening {}: {error}", path.display())))?;
    render_first_page_image_from_reader(reader, target_width)
}

/// Renders a cover from seekable storage without requiring a filesystem path.
pub fn render_first_page_image_from_reader(reader: impl Read + Seek + Send + 'static, target_width: u16) -> PdfResult<RenderedPdfImage> {
    runtime::run(move || render_first_page_image_local(reader, target_width))
}

fn render_first_page_image_local(reader: impl Read + Seek + 'static, target_width: u16) -> PdfResult<RenderedPdfImage> {
    let document = load_document(reader).map_err(|error| PdfError::new(format!("opening PDF cover: {error}")))?;
    let page = document.pages().get(0).map_err(|_| PdfError::new("the PDF has no pages"))?;
    // Render the actual page so stored thumbnails cannot reintroduce annotations.
    let width = target_width.clamp(1, MAX_RENDER_WIDTH) as i32;
    cover_render_height(page.width().value, page.height().value, width)?;
    let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(width).render_annotations(false).render_form_data(false)).map_err(|error| PdfError::new(format!("rendering PDF page 1: {error}")))?;
    Ok(RenderedPdfImage { rgba: bitmap.as_rgba_bytes(), pixel_width: bitmap.width() as u32, pixel_height: bitmap.height() as u32, source: RenderedPdfImageSource::RenderedPage })
}

fn cover_render_height(page_width: f32, page_height: f32, width: i32) -> PdfResult<i32> {
    let height = (f64::from(page_height) * f64::from(width) / f64::from(page_width)).round().max(1.);
    if !page_width.is_finite() || !page_height.is_finite() || page_width <= 0. || page_height <= 0. || !height.is_finite() || height > 20_000. || height * f64::from(width) > 16_000_000. {
        return Err(PdfError::new("PDF cover dimensions exceed rendering limits"));
    }
    Ok(height as i32)
}

#[derive(Clone, Debug)]
struct PageTextLine {
    text: String,
    bounds: NormalizedRect,
}

#[derive(Clone, Debug, Default)]
struct PageElements {
    text_lines: Vec<PageTextLine>,
    graphics: Vec<NormalizedRect>,
}

#[derive(Clone, Copy, Debug, Default)]
struct RepeatingPageBands {
    header_bottom: Option<f32>,
    footer_top: Option<f32>,
}

/// Read bounded opening/closing page text using the reader's visual-line geometry.
/// No rendering or document-wide page analysis is performed.
pub fn inspect_page_text(reader: impl Read + Seek + Send + 'static, edge_pages: usize, max_text_bytes: usize) -> PdfResult<Vec<(usize, String)>> {
    inspect_page_text_with_count(reader, edge_pages, max_text_bytes).map(|(_, pages)| pages)
}

pub fn inspect_page_text_with_count(reader: impl Read + Seek + Send + 'static, edge_pages: usize, max_text_bytes: usize) -> PdfResult<(usize, Vec<(usize, String)>)> {
    runtime::run(move || inspect_page_text_local(reader, edge_pages, max_text_bytes))
}

fn inspect_page_text_local(reader: impl Read + Seek + 'static, edge_pages: usize, max_text_bytes: usize) -> PdfResult<(usize, Vec<(usize, String)>)> {
    let document = load_document(reader)?;
    let count = document.pages().len() as usize;
    let mut result = Vec::new();
    for index in 0..count {
        if index >= edge_pages && index + edge_pages < count {
            continue;
        }
        let page = document.pages().get(index as i32).map_err(|e| PdfError::new(e.to_string()))?;
        let text = page.text().map_err(|e| PdfError::new(e.to_string()))?;
        if text.len() as usize > max_text_bytes {
            continue;
        }
        drop(text);
        let layout = extract_text_layout(&page, index)?;
        let value = text_lines(layout.geometry()).into_iter().map(|line| line.text).collect::<Vec<_>>().join("\n");
        if value.len() <= max_text_bytes {
            result.push((index + 1, value));
        }
    }
    Ok((count, result))
}

/// Renders a bounded set of inspection pages one at a time. The visitor can stop
/// immediately after finding evidence; no document-wide reader analysis is run.
#[cfg(not(target_arch = "wasm32"))]
pub fn inspect_page_images(reader: impl Read + Seek + Send + 'static, pages: &[usize], mut visit: impl FnMut(usize, RenderedPdfImage) -> bool) -> PdfResult<()> {
    let session = PdfDocumentSession::for_inspection(reader)?;
    for &number in pages.iter().take(12) {
        if let Some(image) = session.inspection_image(number)? {
            // OCR and image processing consume owned pixels on the caller.
            if !visit(number, image) {
                break;
            }
        }
    }
    Ok(())
}

// Both native and WASM keep a seekable source alive for on-demand reads.
fn load_document(reader: impl Read + Seek + 'static) -> PdfResult<runtime::Document> {
    shared_pdfium()?.load_pdf_from_reader(PdfiumExactReader::new(reader), None).map_err(|error| PdfError::new(error.to_string()))
}

/// Adapts Rust's short-read semantics to Pdfium's custom file-access contract.
///
/// `pdfium-render` treats any non-zero return from `Read::read()` as a successful
/// read of the complete requested block. A conforming reader may return fewer
/// bytes without reaching EOF, particularly when its data comes from a remote
/// or buffered provider. Make each callback read all-or-error so Pdfium never
/// parses an only partially initialized block.
struct PdfiumExactReader<R> {
    inner: R,
}

impl<R> PdfiumExactReader<R> {
    fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: Read> Read for PdfiumExactReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read_exact(buffer)?;
        Ok(buffer.len())
    }
}

impl<R: Seek> Seek for PdfiumExactReader<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(position)
    }
}

impl LocalDocumentSession {
    pub fn open(path: impl Into<PathBuf>) -> PdfResult<Self> {
        let path = path.into();
        let bytes = fs::read(&path).map_err(|error| PdfError::new(format!("reading {}: {error}", path.display())))?;
        Self::from_bytes(path.display().to_string(), bytes)
    }

    pub fn from_bytes(source_name: impl Into<String>, bytes: Vec<u8>) -> PdfResult<Self> {
        let source_name = source_name.into();
        let document = shared_pdfium()?.load_pdf_from_byte_vec(bytes, None).map_err(|error| PdfError::new(format!("opening {source_name}: {error}")))?;
        Self::from_document(source_name, document)
    }

    pub fn from_reader<R>(source_name: impl Into<String>, reader: R) -> PdfResult<Self>
    where
        R: Read + Seek + 'static,
    {
        let source_name = source_name.into();
        let document = load_document(reader).map_err(|error| PdfError::new(format!("opening {source_name}: {error}")))?;
        Self::from_document(source_name, document)
    }

    fn from_document(source_name: String, document: runtime::Document) -> PdfResult<Self> {
        let pages = document
            .pages()
            .page_sizes()
            .map_err(|error| PdfError::new(format!("reading PDF page dimensions: {error}")))?
            .into_iter()
            .map(|size| PdfPageInfo { width_points: size.width().value, height_points: size.height().value })
            .collect::<Vec<_>>();
        if pages.is_empty() {
            return Err(PdfError::new("the PDF has no pages"));
        }
        let outline = document_outline(&document);
        let right_to_left = document.catalog().is_right_to_left();
        Ok(Self { document, info: PdfDocumentInfo { source_name, pages, outline, skippable_pages: Vec::new(), right_to_left }, analysis: RefCell::new(PageAnalysisCache::default()), page_source: None })
    }

    pub fn info(&self) -> &PdfDocumentInfo {
        &self.info
    }

    /// Inspect the source pages without rasterizing them. An unreadable page is
    /// retained so classification cannot silently hide content.
    pub fn inspect_skippable_pages(&self) -> Vec<usize> {
        (0..self.info.page_count())
            .filter(|&index| {
                if self.page_source.as_ref().is_some_and(|source| source.prepare_page(index).is_err()) {
                    return false;
                }
                self.document.pages().get(index as i32).ok().is_some_and(|page| page_is_skippable(&page))
            })
            .collect()
    }

    pub fn set_skippable_pages(&mut self, pages: Vec<usize>) {
        self.info.skippable_pages = pages;
    }

    #[cfg(target_arch = "wasm32")]
    pub fn render_page(&self, page_index: usize, target_width: Option<u16>) -> PdfResult<RenderedPdfPage> {
        self.render_page_with_margin_trim(page_index, target_width, false)
    }

    pub fn render_page_with_margin_trim(&self, page_index: usize, target_width: Option<u16>, trim_margins: bool) -> PdfResult<RenderedPdfPage> {
        self.render_page_pixels(page_index, target_width, trim_margins, false, None, None)
    }

    /// Renders GPUI's BGRA format directly on the document worker, preserving embedded image colors.
    pub fn render_page_for_display(&self, key: PageRenderKey) -> PdfResult<RenderedPdfBgraPage> {
        self.render_page_pixels(key.page_index(), Some(key.target_width()), key.trim_margins(), true, Some((key.paper_color(), key.ink_color())), key.tile()).map(RenderedPdfBgraPage)
    }

    fn page_analysis(&self, page: &PdfPage<'_>, page_index: usize) -> PdfResult<Arc<PageAnalysis>> {
        let cached = self.analysis.borrow_mut().get(page_index);
        if let Some(analysis) = cached {
            return Ok(analysis);
        }
        let analysis =
            Arc::new(PageAnalysis { text: Arc::new(extract_text_layout(page, page_index)?), images: extract_image_bounds(page), trim_bounds: OnceLock::new(), geometry: [OnceLock::new(), OnceLock::new()], cropped_text: OnceLock::new() });
        self.analysis.borrow_mut().insert(page_index, analysis.clone());
        Ok(analysis)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn prefetch_pages(&self, pages: &[usize]) -> PdfResult<()> {
        self.prefetch_pages_cancellable(pages, &PdfPrefetchCancellation::default())
    }

    pub fn prefetch_pages_cancellable(&self, pages: &[usize], cancellation: &PdfPrefetchCancellation) -> PdfResult<()> {
        if let Some(source) = &self.page_source {
            source.prefetch_pages_cancellable(pages, cancellation).map_err(PdfError::new)?;
        }
        Ok(())
    }

    /// Logical page analysis is independent of raster resolution and theme.
    pub fn page_geometry(&self, page_index: usize, trim_margins: bool) -> PdfResult<PdfPageGeometry> {
        if let Some(source) = &self.page_source {
            source.prepare_page(page_index).map_err(PdfError::new)?;
        }
        let page = self.document.pages().get(page_index as i32).map_err(|_| PdfError::new(format!("PDF page {} does not exist", page_index + 1)))?;
        let analysis = self.page_analysis(&page, page_index)?;
        Ok(analyzed_geometry(&page, &analysis, trim_margins))
    }

    fn render_page_pixels(&self, page_index: usize, target_width: Option<u16>, trim_margins: bool, bgra: bool, colors: Option<([u8; 3], [u8; 3])>, tile: Option<u32>) -> PdfResult<RenderedPdfPage> {
        if let Some(source) = &self.page_source {
            source.prepare_page(page_index).map_err(PdfError::new)?;
        }
        let mut page = self.document.pages().get(page_index as i32).map_err(|_| PdfError::new(format!("PDF page {} does not exist", page_index + 1)))?;
        suppress_bokheim_annotation_rendering(&mut page);
        let requested_width = target_width.unwrap_or(DEFAULT_RENDER_WIDTH).clamp(MIN_RENDER_WIDTH, MAX_RENDER_WIDTH) as u32;
        let analysis = self.page_analysis(&page, page_index)?;
        let geometry = analyzed_geometry(&page, &analysis, trim_margins);
        let aspect = page.height().value as f64 / page.width().value as f64;
        let (pixel_width, pixel_height) = raster_dimensions(requested_width, aspect, tile.is_some())?;
        let crop = pixel_crop_from_normalized(geometry.crop, pixel_width, pixel_height);
        let raster_crop = match tile {
            Some(tile) => {
                let offset = tile.checked_mul(DISPLAY_TILE_HEIGHT).filter(|offset| *offset < crop.height()).ok_or_else(|| PdfError::new("PDF tile is outside the page"))?;
                let top = offset.saturating_sub(1);
                let bottom = offset.saturating_add(DISPLAY_TILE_HEIGHT + 1).min(crop.height());
                PixelCrop::new(pixel_width, pixel_height, crop.left(), crop.top() + top, crop.width(), bottom - top)
            }
            None => crop,
        };
        let mut bitmap = PdfBitmap::empty(raster_crop.width() as i32, raster_crop.height() as i32, PdfBitmapFormat::BGRA).map_err(|error| PdfError::new(format!("allocating PDF page raster: {error}")))?;
        let config = PdfRenderConfig::new()
            .set_fixed_size(pixel_width as i32, pixel_height as i32)
            .set_origin(-(raster_crop.left() as i32), -(raster_crop.top() as i32))
            .set_format(PdfBitmapFormat::BGRA)
            .set_reverse_byte_order(!bgra)
            .render_annotations(true);
        page.render_into_bitmap_with_config(&mut bitmap, &config).map_err(|error| PdfError::new(format!("rendering PDF page {}: {error}", page_index + 1)))?;
        let mut rgba = bitmap.as_raw_bytes();
        if let Some((paper, ink)) = colors {
            recolor_around_images(&mut rgba, raster_crop, &analysis.images, paper, ink);
        }
        let text_layout = if trim_margins {
            analysis
                .cropped_text
                .get_or_init(|| {
                    let mut layout = (*analysis.text).clone();
                    layout.apply_normalized_crop(geometry.crop);
                    Arc::new(layout)
                })
                .clone()
        } else {
            analysis.text.clone()
        };
        Ok(RenderedPdfPage { rgba, pixel_width: raster_crop.width(), pixel_height: raster_crop.height(), crop, raster_crop, page_crop: geometry.crop, text_layout, break_avoidance: geometry.break_avoidance })
    }

    pub fn search(&self, query: &str) -> PdfResult<Vec<PdfSearchHit>> {
        (0..self.info.page_count()).try_fold(Vec::new(), |mut hits, page_index| {
            hits.extend(self.search_page(page_index, query)?);
            Ok(hits)
        })
    }

    /// Searches a single page.
    ///
    /// Scanning a long document takes long enough to matter, and the session
    /// shares the PDFium worker with page rendering, so readers search a
    /// chunk of pages at a time rather than holding the session for the whole
    /// document.
    pub fn search_page(&self, page_index: usize, query: &str) -> PdfResult<Vec<PdfSearchHit>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let page = self.document.pages().get(page_index as i32).map_err(|_| PdfError::new(format!("PDF page {} does not exist", page_index + 1)))?;
        let coordinates = PdfPageCoordinates::new(&page);
        let text = page.text().map_err(|error| PdfError::new(format!("extracting text from page {}: {error}", page_index + 1)))?;
        let search = text.search(query, &PdfSearchOptions::new()).map_err(|error| PdfError::new(format!("searching page {}: {error}", page_index + 1)))?;
        let mut hits = Vec::new();
        search.iter(PdfSearchDirection::SearchForward).for_each(|segments| {
            segments.iter().for_each(|segment| {
                let bounds = segment.bounds();
                if let Some(normalized) = coordinates.normalize(bounds) {
                    hits.push(PdfSearchHit { page_index, bounds: normalized, excerpt: search_excerpt(&text, bounds, coordinates.bounds) });
                }
            });
        });
        Ok(hits)
    }

    /// Returns the Pdfium text index at or near a normalized full-page point.
    /// Tolerances are expressed in PDF points so the UI can derive them from
    /// its current display scale.
    pub fn character_index_near_point(&self, page_index: usize, point: (f32, f32), tolerance: (f32, f32)) -> PdfResult<Option<usize>> {
        if let Some(source) = &self.page_source {
            source.prepare_page(page_index).map_err(PdfError::new)?;
        }
        let page = self.document.pages().get(page_index as i32).map_err(|_| PdfError::new(format!("PDF page {} does not exist", page_index + 1)))?;
        let coordinates = PdfPageCoordinates::new(&page);
        let text = page.text().map_err(|error| PdfError::new(format!("extracting text from page {}: {error}", page_index + 1)))?;
        let (x, y) = coordinates.pdf_point(point);
        let tolerance_x = PdfPoints::new(tolerance.0.max(0.0));
        let tolerance_y = PdfPoints::new(tolerance.1.max(0.0));
        Ok(text.chars().get_char_near_point(x, tolerance_x, y, tolerance_y).map(|character| character.index()))
    }
}

/// Bokheim annotations remain ordinary, printable PDF annotations on disk.
/// In the live reader PDFium must not paint them a second time underneath the
/// editable GPUI overlay. This changes only the in-memory PDFium document; the
/// source bytes and their standard annotation flags are not saved.
fn suppress_bokheim_annotation_rendering(page: &mut PdfPage<'_>) {
    let annotation_range = page.annotations().as_range();
    for annotation_index in annotation_range {
        let Ok(mut annotation) = page.annotations_mut().get(annotation_index) else { continue };
        if annotation.name().as_deref().is_some_and(|name| name.starts_with("bokheim:")) {
            let _ = annotation.set_is_printable_but_not_viewable(true);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfDocumentInfo {
    source_name: String,
    pages: Vec<PdfPageInfo>,
    outline: Vec<BookTocEntry>,
    skippable_pages: Vec<usize>,
    right_to_left: bool,
}

impl PdfDocumentInfo {
    pub fn open(path: impl Into<PathBuf>) -> PdfResult<Self> {
        Ok(PdfDocumentSession::open(path)?.info().clone())
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page(&self, page_index: usize) -> Option<&PdfPageInfo> {
        self.pages.get(page_index)
    }

    pub fn pages(&self) -> &[PdfPageInfo] {
        &self.pages
    }

    pub fn outline(&self) -> &[BookTocEntry] {
        &self.outline
    }

    pub fn skippable_pages(&self) -> &[usize] {
        &self.skippable_pages
    }

    /// Twice the configured visible-page count, including visible pages.
    /// Prefer forward reading while retaining a previous page when space allows.
    pub fn prefetch_window(&self, current: usize, visible: &[usize], configured_pages: usize) -> Vec<usize> {
        if current >= self.page_count() {
            return Vec::new();
        }
        let budget = configured_pages.max(1).saturating_mul(2).min(self.page_count());
        let mut pages = Vec::new();
        for page in std::iter::once(current).chain(visible.iter().copied()) {
            if page < self.page_count() && !pages.contains(&page) {
                pages.push(page);
                if pages.len() == budget {
                    return pages;
                }
            }
        }
        let readable = |page: &usize| self.skippable_pages.binary_search(page).is_err();
        let mut forward = (current + 1..self.page_count()).filter(readable);
        let mut backward = (0..current).rev().filter(readable);
        let previous = if budget - pages.len() > 1 { backward.find(|page| !pages.contains(page)) } else { None };
        let forward_limit = budget - usize::from(previous.is_some());
        while pages.len() < forward_limit {
            let Some(page) = forward.next() else { break };
            if !pages.contains(&page) {
                pages.push(page);
            }
        }
        if let Some(page) = previous {
            pages.push(page);
        }
        // At either document edge, use the other side to fill available slots.
        for page in forward.chain(backward) {
            if pages.len() == budget {
                break;
            }
            if !pages.contains(&page) {
                pages.push(page);
            }
        }
        pages
    }

    /// The document's `/ViewerPreferences /Direction`, defaulting to L2R.
    pub fn right_to_left(&self) -> bool {
        self.right_to_left
    }
}

fn page_is_skippable(page: &PdfPage<'_>) -> bool {
    if !page.annotations().is_empty() || page.objects().iter().any(|object| object.as_text_object().is_none()) {
        return false;
    }
    let Ok(text) = page.text() else { return false };
    is_blank_page_text(&text.all())
}

fn is_blank_page_text(text: &str) -> bool {
    let words = text.split_whitespace().collect::<Vec<_>>();
    words.is_empty() || words.join(" ").trim_end_matches('.').eq_ignore_ascii_case("This page intentionally left blank")
}

/// Bounds on one walk. A PDF's bookmark graph is not required to be acyclic,
/// and nothing downstream wants an outline larger than a reader can show.
const MAX_OUTLINE_ENTRIES: usize = 4096;
const MAX_OUTLINE_DEPTH: usize = 256;

/// The outline as the tree PDFium already holds, rather than a flat list whose
/// nesting has to be guessed back from an ancestor count. `first_child` and
/// `next_sibling` are what `/First` and `/Next` are called here.
///
/// An entry whose destination will not resolve is dropped and its children take
/// its place, which is the decision the flat form could not express: there,
/// depth was absolute, so losing a parent silently re-filed its children under
/// whichever ancestor happened to survive.
///
/// `remaining` is spent across the whole tree and `depth` is capped, because a
/// PDF's bookmark graph is not required to be acyclic.
fn outline_entries(first: PdfBookmark<'_>, depth: usize, remaining: &mut usize) -> Vec<BookTocEntry> {
    let mut entries = Vec::new();
    let mut next = Some(first);
    while let Some(bookmark) = next.take() {
        if *remaining == 0 {
            break;
        }
        let children = match bookmark.first_child() {
            Some(child) if depth < MAX_OUTLINE_DEPTH => outline_entries(child, depth + 1, remaining),
            _ => Vec::new(),
        };
        match (bookmark.title().map(|title| title.trim().to_owned()).filter(|title| !title.is_empty()), bookmark_page(&bookmark)) {
            (Some(title), Some(page_index)) => {
                *remaining -= 1;
                entries.push(BookTocEntry { title, target: pdf_toc_target(page_index), children });
            }
            _ => entries.extend(children),
        }
        next = bookmark.next_sibling();
    }
    entries
}

/// The whole outline of one open document.
fn document_outline(document: &runtime::Document) -> Vec<BookTocEntry> {
    let mut remaining = MAX_OUTLINE_ENTRIES;
    document.bookmarks().root().map(|root| outline_entries(root, 0, &mut remaining)).unwrap_or_default()
}

/// The page a bookmark opens, whether it says so directly or through an action.
fn bookmark_page(bookmark: &PdfBookmark<'_>) -> Option<usize> {
    let page_index = bookmark.destination().and_then(|destination| destination.page_index().ok()).or_else(|| bookmark.action()?.as_local_destination_action()?.destination().ok()?.page_index().ok())?;
    Some(page_index as usize)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfPageInfo {
    width_points: f32,
    height_points: f32,
}

impl PdfPageInfo {
    pub fn width_points(&self) -> f32 {
        self.width_points
    }

    pub fn height_points(&self) -> f32 {
        self.height_points
    }

    pub fn aspect_ratio(&self) -> f32 {
        self.width_points / self.height_points.max(1.0)
    }
}

/// Stable geometry in the original, normalized PDF coordinate system.
/// Neither device pixels nor the render cache determine layout.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfPageGeometry {
    pub crop: NormalizedRect,
    /// Protected vertical bands in the cropped page's normalized coordinates.
    pub break_avoidance: Arc<[(f32, f32)]>,
}

impl PdfPageGeometry {
    pub fn full_to_display_y(&self, y: f32) -> f32 {
        ((y - self.crop.top()) / self.crop.height()).clamp(0.0, 1.0)
    }

    pub fn display_to_full_y(&self, y: f32) -> f32 {
        self.crop.top() + y * self.crop.height()
    }
}

fn crop_rect(crop: NormalizedRect, rect: NormalizedRect) -> Option<NormalizedRect> {
    let left = rect.left().max(crop.left());
    let top = rect.top().max(crop.top());
    let right = rect.right().min(crop.right());
    let bottom = rect.bottom().min(crop.bottom());
    (left < right && top < bottom).then(|| NormalizedRect::new((left - crop.left()) / crop.width(), (top - crop.top()) / crop.height(), (right - left) / crop.width(), (bottom - top) / crop.height()))
}

fn analyzed_geometry(page: &PdfPage<'_>, analysis: &PageAnalysis, trim_margins: bool) -> PdfPageGeometry {
    analysis.geometry[usize::from(trim_margins)]
        .get_or_init(|| {
            let crop = if trim_margins {
                analysis.trim_bounds.get_or_init(|| content_bounds(&analyze_page_elements(page, &analysis.text), RepeatingPageBands::default())).unwrap_or(NormalizedRect::new(0.0, 0.0, 1.0, 1.0))
            } else {
                NormalizedRect::new(0.0, 0.0, 1.0, 1.0)
            };
            let text_bands =
                merge_line_rects(analysis.text.geometry.characters().iter().filter(|character| !character.character().is_control() && !character.character().is_whitespace()).filter_map(|character| crop_rect(crop, character.bounds())));
            let bands = text_bands.into_iter().chain(analysis.images.iter().filter_map(|bounds| crop_rect(crop, *bounds))).map(|bounds| ((bounds.top() - 0.002).max(0.0), (bounds.bottom() + 0.002).min(1.0)));
            PdfPageGeometry { crop, break_avoidance: sorted_vertical_bands(bands).into() }
        })
        .clone()
}

fn raster_dimensions(requested_width: u32, aspect: f64, tiled: bool) -> PdfResult<(u32, u32)> {
    if !aspect.is_finite() || aspect <= 0.0 {
        return Err(PdfError::new("PDF page raster dimensions are out of range"));
    }
    let width = if tiled { requested_width } else { requested_width.min((MAX_RASTER_PIXELS as f64 / aspect).sqrt().floor().max(1.0) as u32) };
    let height = (width as f64 * aspect).round().max(1.0);
    if height >= i32::MAX as f64 || (!tiled && width as f64 * height > MAX_RASTER_PIXELS as f64) {
        return Err(PdfError::new("PDF page raster dimensions are out of range"));
    }
    Ok((width, height as u32))
}

#[derive(Clone, Debug)]
pub struct RenderedPdfPage {
    rgba: Vec<u8>,
    pixel_width: u32,
    pixel_height: u32,
    crop: PixelCrop,
    raster_crop: PixelCrop,
    page_crop: NormalizedRect,
    text_layout: Arc<PdfPageTextLayout>,
    break_avoidance: Arc<[(f32, f32)]>,
}

/// Owned page data in the pixel order of the rendering operation that produced it.
pub struct RenderedPdfPageParts {
    pub pixels: Vec<u8>,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub crop: PixelCrop,
    pub raster_crop: PixelCrop,
    pub page_crop: NormalizedRect,
    pub text_layout: Arc<PdfPageTextLayout>,
    pub break_avoidance: Arc<[(f32, f32)]>,
}

/// A page whose pixels are already in GPUI's BGRA order. Kept distinct from
/// `RenderedPdfPage` so RGBA consumers cannot accidentally swap its colors.
pub struct RenderedPdfBgraPage(RenderedPdfPage);

impl RenderedPdfBgraPage {
    pub fn into_parts(self) -> RenderedPdfPageParts {
        self.0.into_parts()
    }
}

impl RenderedPdfPage {
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    pub fn pixel_width(&self) -> u32 {
        self.pixel_width
    }

    pub fn pixel_height(&self) -> u32 {
        self.pixel_height
    }

    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }

    pub fn crop(&self) -> PixelCrop {
        self.crop
    }

    pub fn break_avoidance(&self) -> &[(f32, f32)] {
        &self.break_avoidance
    }

    pub fn into_parts(self) -> RenderedPdfPageParts {
        RenderedPdfPageParts {
            pixels: self.rgba,
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
            crop: self.crop,
            raster_crop: self.raster_crop,
            page_crop: self.page_crop,
            text_layout: self.text_layout,
            break_avoidance: self.break_avoidance,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PdfPageTextLayout {
    page_index: usize,
    geometry: PageTextGeometry,
}

impl PdfPageTextLayout {
    pub fn select(&self, start: (f32, f32), end: (f32, f32)) -> Option<PdfTextSelection> {
        let _ = self.page_index;
        self.geometry.select(start, end)
    }

    pub fn select_with_options(&self, start: (f32, f32), end: (f32, f32), options: SelectionOptions) -> Option<PdfTextSelection> {
        let _ = self.page_index;
        self.geometry.select_with_options(start, end, options)
    }

    pub fn select_from_source_index_with_options(&self, anchor_source_index: usize, end: (f32, f32), options: SelectionOptions) -> Option<PdfTextSelection> {
        let _ = self.page_index;
        self.geometry.select_from_source_index_with_options(anchor_source_index, end, options)
    }

    pub fn geometry(&self) -> &PageTextGeometry {
        &self.geometry
    }

    fn apply_normalized_crop(&mut self, crop: NormalizedRect) {
        self.geometry = PageTextGeometry::new(self.geometry.characters().iter().filter_map(|character| {
            let bounds = crop_rect(crop, character.bounds())?;
            Some(TextCharacter::new(character.source_index(), character.character(), bounds))
        }));
    }
}

/// The text a search result shows: the matched line with one line either side,
/// read across the page's full width so a hit in a narrow column still gets
/// its sentence, and folded onto one line.
fn search_excerpt(text: &PdfPageText<'_>, bounds: PdfRect, page_bounds: PdfRect) -> Arc<str> {
    let line = bounds.height().value.max(1.0);
    let area = PdfRect::new(PdfPoints::new(bounds.bottom().value - line), page_bounds.left(), PdfPoints::new(bounds.top().value + line), page_bounds.right());
    text.inside_rect(area).split_whitespace().collect::<Vec<_>>().join(" ").into()
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfSearchHit {
    page_index: usize,
    bounds: NormalizedRect,
    excerpt: Arc<str>,
}

impl PdfSearchHit {
    pub fn page_index(&self) -> usize {
        self.page_index
    }

    pub fn bounds(&self) -> NormalizedRect {
        self.bounds
    }

    /// The text around the match, for a results list.
    pub fn excerpt(&self) -> &str {
        &self.excerpt
    }
}

/// Map BGRA pixels from black ink and white paper to the display colors.
/// Antialiased edges blend into the paper.
fn recolor_page(pixels: &mut [u8], [red, green, blue]: [u8; 3], [ink_red, ink_green, ink_blue]: [u8; 3]) {
    if [red, green, blue] == [255; 3] && [ink_red, ink_green, ink_blue] == [0; 3] {
        return;
    }
    for pixel in pixels.chunks_exact_mut(4) {
        for ((channel, paper), ink) in pixel[..3].iter_mut().zip([blue, green, red]).zip([ink_blue, ink_green, ink_red]) {
            let source = u16::from(*channel);
            *channel = ((source * u16::from(paper) + (255 - source) * u16::from(ink) + 127) / 255) as u8;
        }
    }
}

/// Preserve the original raster within image bounds, including after margin trimming.
fn recolor_around_images(pixels: &mut [u8], crop: PixelCrop, images: &[NormalizedRect], paper: [u8; 3], ink: [u8; 3]) {
    if paper == [255; 3] && ink == [0; 3] {
        return;
    }
    let images = images.iter().filter_map(|bounds| crop.full_rect_to_display(*bounds)).map(|bounds| pixel_crop_from_normalized(bounds, crop.width(), crop.height())).collect::<Vec<_>>();
    if images.is_empty() {
        recolor_page(pixels, paper, ink);
        return;
    }
    let mut spans = Vec::with_capacity(images.len());
    for (y, row) in pixels.chunks_exact_mut(crop.width() as usize * 4).enumerate() {
        spans.clear();
        spans.extend(images.iter().filter(|image| y >= image.top() as usize && y < (image.top() + image.height()) as usize).map(|image| (image.left() as usize * 4, (image.left() + image.width()) as usize * 4)));
        spans.sort_unstable();
        let mut start = 0;
        for &(left, right) in &spans {
            if left > start {
                recolor_page(&mut row[start..left], paper, ink);
            }
            start = start.max(right);
        }
        recolor_page(&mut row[start..], paper, ink);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageRenderKey {
    page_index: usize,
    target_width: u16,
    tile: Option<u32>,
    trim_margins: bool,
    paper_color: [u8; 3],
    ink_color: [u8; 3],
}

impl PageRenderKey {
    pub fn new(page_index: usize, target_width: u16) -> Self {
        Self::with_margin_trim(page_index, target_width, false)
    }

    pub fn with_margin_trim(page_index: usize, target_width: u16, trim_margins: bool) -> Self {
        Self { page_index, target_width: target_width.clamp(MIN_RENDER_WIDTH, MAX_RENDER_WIDTH), tile: None, trim_margins, paper_color: [255; 3], ink_color: [0; 3] }
    }

    pub fn with_tile(mut self, tile: u32) -> Self {
        self.tile = Some(tile);
        self
    }

    pub fn tile(&self) -> Option<u32> {
        self.tile
    }

    /// RGB paper color for display rasters. White with black ink preserves the original PDF.
    pub fn with_paper_color(mut self, color: [u8; 3]) -> Self {
        self.paper_color = color;
        self
    }

    pub fn paper_color(&self) -> [u8; 3] {
        self.paper_color
    }

    /// RGB color for originally black pixels outside embedded images.
    pub fn with_ink_color(mut self, color: [u8; 3]) -> Self {
        self.ink_color = color;
        self
    }

    pub fn ink_color(&self) -> [u8; 3] {
        self.ink_color
    }

    pub fn page_index(&self) -> usize {
        self.page_index
    }

    pub fn target_width(&self) -> u16 {
        self.target_width
    }

    pub fn trim_margins(&self) -> bool {
        self.trim_margins
    }
}

pub struct PageRenderCache {
    byte_budget: usize,
    byte_len: usize,
    pages: HashMap<PageRenderKey, Arc<RenderedPdfPage>>,
    recency: VecDeque<PageRenderKey>,
}

impl PageRenderCache {
    pub fn new(byte_budget: usize) -> Self {
        Self { byte_budget, byte_len: 0, pages: HashMap::new(), recency: VecDeque::new() }
    }

    pub fn get(&mut self, key: PageRenderKey) -> Option<Arc<RenderedPdfPage>> {
        let page = self.pages.get(&key)?.clone();
        self.promote(key);
        Some(page)
    }

    pub fn insert(&mut self, key: PageRenderKey, page: Arc<RenderedPdfPage>) {
        if page.byte_len() > self.byte_budget {
            return;
        }
        if let Some(previous) = self.pages.remove(&key) {
            self.byte_len = self.byte_len.saturating_sub(previous.byte_len());
            self.recency.retain(|candidate| *candidate != key);
        }
        while self.byte_len + page.byte_len() > self.byte_budget {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            if let Some(evicted) = self.pages.remove(&oldest) {
                self.byte_len = self.byte_len.saturating_sub(evicted.byte_len());
            }
        }
        self.byte_len += page.byte_len();
        self.pages.insert(key, page);
        self.recency.push_back(key);
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    fn promote(&mut self, key: PageRenderKey) {
        self.recency.retain(|candidate| *candidate != key);
        self.recency.push_back(key);
    }
}

pub fn is_pdf_path(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

pub fn pdfium_third_party_notices() -> &'static str {
    #[cfg(not(target_arch = "wasm32"))]
    {
        PDFIUM_THIRD_PARTY_NOTICES
    }
    #[cfg(target_arch = "wasm32")]
    {
        "PDFium and third-party licenses are distributed in pdfium/LICENSE in the web asset bundle."
    }
}

fn extract_text_layout(page: &PdfPage<'_>, page_index: usize) -> PdfResult<PdfPageTextLayout> {
    let coordinates = PdfPageCoordinates::new(page);
    let text = page.text().map_err(|error| PdfError::new(format!("extracting text from page {}: {error}", page_index + 1)))?;
    let characters = text
        .chars()
        .iter()
        .filter_map(|character| {
            let unicode = character.unicode_char()?;
            let tight = character.tight_bounds().ok();
            let loose = character.loose_bounds().ok();
            let bounds = match (tight, loose) {
                // Tight side bearings align with the painted glyph. The loose
                // ascent and descent produce the expected full-height selection.
                (Some(tight), Some(loose)) => PdfRect::new(loose.bottom(), tight.left(), loose.top(), tight.right()),
                (Some(bounds), None) | (None, Some(bounds)) => bounds,
                (None, None) => return None,
            };
            Some(TextCharacter::new(character.index(), unicode, coordinates.normalize(bounds)?))
        })
        .collect::<Vec<_>>();
    Ok(PdfPageTextLayout { page_index, geometry: PageTextGeometry::new(characters) })
}

fn analyze_page_elements(page: &PdfPage<'_>, text_layout: &PdfPageTextLayout) -> PageElements {
    let coordinates = PdfPageCoordinates::new(page);
    let text_lines = text_lines(text_layout.geometry());
    let mut graphics = Vec::new();
    for object in page.objects().iter() {
        collect_graphic_bounds(&object, PdfMatrix::IDENTITY, coordinates, &mut graphics);
    }
    PageElements { text_lines, graphics }
}

fn collect_graphic_bounds(object: &PdfPageObject<'_>, parent: PdfMatrix, coordinates: PdfPageCoordinates, graphics: &mut Vec<NormalizedRect>) {
    if object.as_text_object().is_some() {
        return;
    }
    if let Some(form) = object.as_x_object_form_object() {
        let transform = form.matrix().unwrap_or(PdfMatrix::IDENTITY) * parent;
        for child in form.iter() {
            collect_graphic_bounds(&child, transform, coordinates, graphics);
        }
        return;
    }
    let Some(bounds) = object.bounds().ok().and_then(|bounds| coordinates.normalize(bounds.transform(parent).to_rect())) else {
        return;
    };
    let is_page_background = bounds.width() >= 0.98 && bounds.height() >= 0.98;
    let is_margin_rule = (bounds.width() >= 0.5 && bounds.height() <= 0.006) || (bounds.height() >= 0.5 && bounds.width() <= 0.006);
    if !is_page_background && !is_margin_rule && bounds.width() > 0.0001 && bounds.height() > 0.0001 {
        graphics.push(bounds);
    }
}

fn text_lines(geometry: &PageTextGeometry) -> Vec<PageTextLine> {
    let mut lines = Vec::new();
    let mut text = String::new();
    let mut bounds: Option<NormalizedRect> = None;
    for character in geometry.characters() {
        let value = character.character();
        if value.is_control() {
            push_text_line(&mut lines, &mut text, &mut bounds);
            continue;
        }
        if value.is_whitespace() {
            if !text.ends_with(' ') && !text.is_empty() {
                text.push(' ');
            }
            continue;
        }
        let character_bounds = character.bounds();
        if bounds.is_some_and(|line_bounds| !same_visual_text_line(line_bounds, character_bounds)) {
            push_text_line(&mut lines, &mut text, &mut bounds);
        }
        text.push(value);
        bounds = Some(bounds.map_or(character_bounds, |line_bounds| line_bounds.union(character_bounds)));
    }
    push_text_line(&mut lines, &mut text, &mut bounds);
    lines
}

fn push_text_line(lines: &mut Vec<PageTextLine>, text: &mut String, bounds: &mut Option<NormalizedRect>) {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(bounds) = bounds.take().filter(|_| !normalized.is_empty()) {
        lines.push(PageTextLine { text: normalized, bounds });
    }
    text.clear();
}

fn same_visual_text_line(left: NormalizedRect, right: NormalizedRect) -> bool {
    let vertical_overlap = left.bottom().min(right.bottom()) - left.top().max(right.top());
    let horizontal_gap = if left.right() < right.left() {
        right.left() - left.right()
    } else if right.right() < left.left() {
        left.left() - right.right()
    } else {
        0.0
    };
    vertical_overlap > left.height().min(right.height()) * 0.3 && horizontal_gap <= 0.08
}

fn content_bounds(elements: &PageElements, bands: RepeatingPageBands) -> Option<NormalizedRect> {
    let is_body = |bounds: NormalizedRect| {
        let center = bounds.top() + bounds.height() * 0.5;
        bands.header_bottom.is_none_or(|bottom| center >= bottom) && bands.footer_top.is_none_or(|top| center <= top)
    };
    let mut bounds = elements.text_lines.iter().map(|line| line.bounds).chain(elements.graphics.iter().copied()).filter(|bounds| is_body(*bounds)).reduce(NormalizedRect::union)?;
    let left = (bounds.left() - ELEMENT_CROP_PADDING).max(0.0);
    let top = (bounds.top() - ELEMENT_CROP_PADDING).max(bands.header_bottom.unwrap_or(0.0));
    let right = (bounds.right() + ELEMENT_CROP_PADDING).min(1.0);
    let bottom = (bounds.bottom() + ELEMENT_CROP_PADDING).min(bands.footer_top.unwrap_or(1.0));
    if right <= left || bottom <= top {
        return None;
    }
    bounds = NormalizedRect::new(left, top, right - left, bottom - top);
    Some(bounds)
}

fn pixel_crop_from_normalized(bounds: NormalizedRect, width: u32, height: u32) -> PixelCrop {
    let left = (bounds.left() * width as f32).floor() as u32;
    let top = (bounds.top() * height as f32).floor() as u32;
    let right = (bounds.right() * width as f32).ceil() as u32;
    let bottom = (bounds.bottom() * height as f32).ceil() as u32;
    PixelCrop::new(width, height, left, top, right.saturating_sub(left), bottom.saturating_sub(top))
}

/// PDF objects use absolute, unrotated PDF coordinates. PDFium rasters the
/// intersection of MediaBox and CropBox, then applies the page rotation.
#[derive(Clone, Copy)]
struct PdfPageCoordinates {
    bounds: PdfRect,
    transform: PageTransform,
}

impl PdfPageCoordinates {
    fn new(page: &PdfPage<'_>) -> Self {
        let rotation = match page.rotation().unwrap_or(PdfPageRenderRotation::None) {
            PdfPageRenderRotation::None => PageRotation::None,
            PdfPageRenderRotation::Degrees90 => PageRotation::Clockwise90,
            PdfPageRenderRotation::Degrees180 => PageRotation::Clockwise180,
            PdfPageRenderRotation::Degrees270 => PageRotation::Clockwise270,
        };
        let bounds = page.boundaries().bounding().map(|boundary| boundary.bounds).unwrap_or_else(|_| {
            let (width, height) = if matches!(rotation, PageRotation::Clockwise90 | PageRotation::Clockwise270) { (page.height(), page.width()) } else { (page.width(), page.height()) };
            PdfRect::new(PdfPoints::ZERO, PdfPoints::ZERO, height, width)
        });
        Self { bounds, transform: PageTransform::new(PixelCrop::full(1, 1), rotation) }
    }

    fn normalize(self, bounds: PdfRect) -> Option<NormalizedRect> {
        let left = bounds.left().value.max(self.bounds.left().value);
        let bottom = bounds.bottom().value.max(self.bounds.bottom().value);
        let right = bounds.right().value.min(self.bounds.right().value);
        let top = bounds.top().value.min(self.bounds.top().value);
        if right <= left || top <= bottom {
            return None;
        }
        self.transform.full_rect_to_display(NormalizedRect::new(
            (left - self.bounds.left().value) / self.bounds.width().value.max(1.0),
            (self.bounds.top().value - top) / self.bounds.height().value.max(1.0),
            (right - left) / self.bounds.width().value.max(1.0),
            (top - bottom) / self.bounds.height().value.max(1.0),
        ))
    }

    fn pdf_point(self, point: (f32, f32)) -> (PdfPoints, PdfPoints) {
        let (x, y) = self.transform.display_point_to_full(point);
        (PdfPoints::new(self.bounds.left().value + x * self.bounds.width().value), PdfPoints::new(self.bounds.top().value - y * self.bounds.height().value))
    }
}

fn extract_image_bounds(page: &PdfPage<'_>) -> Vec<NormalizedRect> {
    let coordinates = PdfPageCoordinates::new(page);
    let mut images = Vec::new();
    for object in page.objects().iter() {
        collect_image_bounds(&object, PdfMatrix::IDENTITY, coordinates, &mut images);
    }
    images
}

fn collect_image_bounds(object: &PdfPageObject<'_>, parent: PdfMatrix, coordinates: PdfPageCoordinates, images: &mut Vec<NormalizedRect>) {
    if object.as_image_object().is_some() {
        if let Some(bounds) = object.bounds().ok().and_then(|bounds| coordinates.normalize(bounds.transform(parent).to_rect())) {
            images.push(bounds);
        }
    } else if let Some(form) = object.as_x_object_form_object() {
        let transform = form.matrix().unwrap_or(PdfMatrix::IDENTITY) * parent;
        for child in form.iter() {
            collect_image_bounds(&child, transform, coordinates, images);
        }
    }
}

fn sorted_vertical_bands(bands: impl IntoIterator<Item = (f32, f32)>) -> Vec<(f32, f32)> {
    let mut bands = bands.into_iter().filter(|(top, bottom)| top.is_finite() && bottom.is_finite() && bottom > top).collect::<Vec<_>>();
    bands.sort_by(|left, right| left.0.total_cmp(&right.0));
    bands
}

pub fn crop_normalized_rect(crop: PixelCrop, rect: NormalizedRect) -> Option<NormalizedRect> {
    crop.full_rect_to_display(rect)
}

fn shared_pdfium() -> PdfResult<&'static Pdfium> {
    #[cfg(not(target_arch = "wasm32"))]
    assert!(native::on_worker(), "PDFium can only be accessed by its owning worker");
    let pdfium = PDFIUM.get_or_init(runtime::initialize);
    pdfium.as_ref().map_err(|error| PdfError::new(error.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn viewer_direction_is_read_from_the_pdf_with_and_without_cached_geometry() -> PdfResult<()> {
        let fixture = String::from_utf8(include_bytes!("../tests/fixtures/rtl-viewer-preferences.pdf").to_vec()).unwrap();
        for (name, expected) in [("/R2L", true), ("/L2R", false), ("/Foo", false), ("(R) ", false)] {
            // Equal-length substitutions preserve the real fixture's xref offsets.
            let bytes = fixture.replace("/R2L", name).into_bytes();
            let session = PdfDocumentSession::from_bytes("direction", bytes.clone())?;
            assert_eq!(session.info().right_to_left(), expected, "{name}");
            let (reader, metadata, _) = inspect_reader_metadata(Cursor::new(bytes))?;
            let hash = metadata.content_hash.clone();
            let cached = PdfDocumentSession::from_reader_with_metadata("cached-direction", reader, &hash, Some(metadata))?;
            assert_eq!(cached.info().right_to_left(), expected, "cached {name}");
        }
        let missing = fixture.replace("/Direction", "/Unknown__").into_bytes();
        assert!(!PdfDocumentSession::from_bytes("no-direction", missing)?.info().right_to_left());
        Ok(())
    }

    #[test]
    fn paper_tint_matches_white_pixels_and_preserves_black_and_alpha() {
        let mut pixels = [255, 255, 255, 255, 0, 0, 0, 255, 128, 128, 128, 127, 20, 80, 160, 255];
        let original = pixels;
        recolor_page(&mut pixels, [255; 3], [0; 3]);
        assert_eq!(pixels, original);
        recolor_page(&mut pixels, [240, 230, 210], [0; 3]);
        assert_eq!(pixels, [210, 230, 240, 255, 0, 0, 0, 255, 105, 115, 120, 127, 16, 72, 151, 255]);
    }

    #[test]
    fn paper_tints_have_distinct_render_cache_keys() {
        let white = PageRenderKey::with_margin_trim(2, 640, true);
        let cream = white.with_paper_color([240, 230, 210]);
        assert_eq!(white.paper_color(), [255; 3]);
        assert_ne!(white, cream);
        let mut cache = PageRenderCache::new(1024);
        cache.insert(
            white,
            Arc::new(RenderedPdfPage {
                rgba: vec![255; 4],
                pixel_width: 1,
                pixel_height: 1,
                crop: PixelCrop::full(1, 1),
                raster_crop: PixelCrop::full(1, 1),
                page_crop: NormalizedRect::new(0.0, 0.0, 1.0, 1.0),
                text_layout: Arc::new(PdfPageTextLayout::default()),
                break_avoidance: Arc::from([]),
            }),
        );
        assert!(cache.get(white).is_some());
        assert!(cache.get(cream).is_none());
        let dark = white.with_paper_color([24, 30, 36]).with_ink_color([230, 235, 240]);
        assert_ne!(dark, dark.with_ink_color([255; 3]));
        assert!(cache.get(dark).is_none());
    }

    #[test]
    fn dark_page_colors_map_text_paper_and_antialiased_edges() {
        let mut pixels = [255, 255, 255, 255, 0, 0, 0, 255, 128, 128, 128, 127];
        recolor_page(&mut pixels, [24, 30, 36], [230, 235, 240]);
        assert_eq!(pixels, [36, 30, 24, 255, 240, 235, 230, 255, 138, 132, 127, 127]);
    }

    #[test]
    fn image_colors_survive_cropping_and_overlapping_image_bounds() {
        let crop = PixelCrop::new(8, 4, 2, 1, 4, 2);
        let original = [12, 34, 56, 255].repeat(8);
        let mut pixels = original.clone();
        let images = [NormalizedRect::new(0.125, 0.25, 0.5, 0.25), NormalizedRect::new(0.5, 0.25, 0.375, 0.5), NormalizedRect::new(0.0, 0.0, 0.125, 0.25)];
        recolor_around_images(&mut pixels, crop, &images, [24, 30, 36], [230, 235, 240]);
        assert_eq!(&pixels[..16], &original[..16], "overlapping image bounds preserve the first row");
        assert_ne!(&pixels[16..24], &original[16..24], "uncovered pixels follow the theme");
        assert_eq!(&pixels[24..], &original[24..], "the second image extends into the next row");
    }

    #[test]
    fn full_page_scans_keep_their_original_colors() {
        let original = vec![255, 255, 255, 255, 0, 0, 0, 255];
        let mut pixels = original.clone();
        recolor_around_images(&mut pixels, PixelCrop::full(2, 1), &[NormalizedRect::new(0.0, 0.0, 1.0, 1.0)], [24, 30, 36], [230, 235, 240]);
        assert_eq!(pixels, original);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn document_with_image() -> runtime::Document {
        let pdfium = shared_pdfium().unwrap();
        let mut document = pdfium.create_new_pdf().unwrap();
        let font = document.fonts_mut().helvetica();
        let mut bytes = [20, 80, 200, 255, 255, 255, 255, 255, 0, 0, 0, 255, 200, 80, 20, 255].repeat(4);
        let bitmap = PdfBitmap::from_bytes(4, 4, PdfBitmapFormat::BGRA, &mut bytes).unwrap();
        let mut image = PdfPageImageObject::new(&document).unwrap();
        image.set_bitmap(&bitmap).unwrap();
        image.scale(120.0, 80.0).unwrap();
        image.translate(PdfPoints::new(80.0), PdfPoints::new(100.0)).unwrap();
        {
            let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).unwrap();
            page.objects_mut().add_image_object(image).unwrap();
            page.objects_mut().create_text_object(PdfPoints::new(72.0), PdfPoints::new(720.0), "Dark theme text", font, PdfPoints::new(18.0)).unwrap();
        }
        document
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn assert_image_colors_preserved(document: runtime::Document) -> PdfResult<()> {
        let session = LocalDocumentSession::from_document("image-colors".into(), document)?;
        let rotation = session.document.pages().get(0).unwrap().rotation().unwrap();
        let trim_modes: &[bool] = if rotation == PdfPageRenderRotation::None { &[false, true] } else { &[false] };
        for &trim in trim_modes {
            let key = PageRenderKey::with_margin_trim(0, 640, trim);
            let original = session.render_page_for_display(key)?.into_parts();
            for (paper, ink) in [([24, 30, 36], [230, 235, 240]), ([240, 230, 210], [0; 3])] {
                let themed = session.render_page_for_display(key.with_paper_color(paper).with_ink_color(ink))?.into_parts();
                assert_eq!(themed.crop, original.crop);
                let mut colored_pixels = 0;
                let mut changed_pixels = 0;
                let mut light_text_pixels = 0;
                for (source, output) in original.pixels.chunks_exact(4).zip(themed.pixels.chunks_exact(4)) {
                    if source[0] != source[1] || source[1] != source[2] {
                        colored_pixels += 1;
                        assert_eq!(output, source, "embedded image colors changed after cropping or rotation");
                    }
                    if source != output {
                        changed_pixels += 1;
                    }
                    if source[0] == source[1] && source[1] == source[2] && source[0] < 128 && output[..3].iter().zip(source).all(|(output, source)| output > source) {
                        light_text_pixels += 1;
                    }
                }
                assert!(colored_pixels > 100, "fixture must contain a visible color image");
                assert!(changed_pixels > 100, "page background must still follow the theme");
                assert!(ink == [0; 3] || light_text_pixels > 10, "text outside the image must still become light in dark mode: rotation={rotation:?}, trim={trim}, light_text_pixels={light_text_pixels}");
            }
        }
        Ok(())
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn themed_rendering_preserves_images_on_rotated_and_trimmed_pages() -> PdfResult<()> {
        runtime::run(|| {
            for rotation in [PdfPageRenderRotation::None, PdfPageRenderRotation::Degrees90, PdfPageRenderRotation::Degrees180, PdfPageRenderRotation::Degrees270] {
                let document = document_with_image();
                document.pages().get(0).unwrap().set_rotation(rotation);
                assert_image_colors_preserved(document)?;
            }
            Ok(())
        })
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn themed_rendering_preserves_images_in_nested_transformed_groups() -> PdfResult<()> {
        runtime::run(|| {
            let mut document = document_with_image();
            for (scale, x, y) in [(0.6, 50.0, 60.0), (0.8, 20.0, 30.0)] {
                let bytes = {
                    let source_page = document.pages().get(0).unwrap();
                    let mut destination = shared_pdfium()?.create_new_pdf().unwrap();
                    let mut form = source_page.objects().copy_into_x_object_form_object(&mut destination).unwrap();
                    form.scale(scale, scale).unwrap();
                    form.translate(PdfPoints::new(x), PdfPoints::new(y)).unwrap();
                    destination.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).unwrap().objects_mut().add_object(form).unwrap();
                    destination.save_to_bytes().unwrap()
                };
                document = shared_pdfium()?.load_pdf_from_byte_vec(bytes, None).unwrap();
            }
            assert_image_colors_preserved(document)
        })
    }

    fn fixture_path(label: &str) -> PdfResult<PathBuf> {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| PdfError::new(error.to_string()))?.as_nanos();
        Ok(std::env::temp_dir().join(format!("bokheim-pdf-{label}-{suffix}.pdf")))
    }

    include!("streaming_tests.rs");

    fn create_fixture_pdf(path: &Path) -> PdfResult<()> {
        let path = path.to_owned();
        runtime::run(move || {
            let pdfium = shared_pdfium()?;
            let mut document = pdfium.create_new_pdf().map_err(|error| PdfError::new(error.to_string()))?;
            let font = document.fonts_mut().helvetica();
            for text in ["First searchable page", "Second searchable page"] {
                let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).map_err(|error| PdfError::new(error.to_string()))?;
                page.objects_mut().create_text_object(PdfPoints::new(72.0), PdfPoints::new(720.0), text, font, PdfPoints::new(18.0)).map_err(|error| PdfError::new(error.to_string()))?;
            }
            document.save_to_file(&path).map_err(|error| PdfError::new(error.to_string()))
        })
    }

    #[test]
    fn narration_and_rendering_reuse_the_same_page_analysis() -> PdfResult<()> {
        let path = fixture_path("narration-analysis")?;
        create_fixture_pdf(&path)?;
        runtime::run(move || {
            let document = LocalDocumentSession::open(&path)?;
            let fragments = document.extract_reading_fragments(0)?;
            assert_eq!(fragments[0].text, "First searchable page");
            let first = document.analysis.borrow_mut().get(0).unwrap();
            let rendered = document.render_page_pixels(0, Some(320), false, false, None, None)?;
            assert!(Arc::ptr_eq(&first.text, &rendered.text_layout), "TTS-first extraction is also used by rendering");
            document.render_page_pixels(1, Some(320), true, false, None, None)?;
            let second = document.analysis.borrow_mut().get(1).unwrap();
            // A sentinel cached layout proves narration uses cached geometry
            // rather than re-extracting the PDF page's original text.
            let text = "Cached narration";
            let geometry = PageTextGeometry::new(text.chars().enumerate().map(|(index, ch)| TextCharacter::new(index, ch, NormalizedRect::new(0.1 + index as f32 * 0.015, 0.2, 0.015, 0.02))));
            let cached = Arc::new(PageAnalysis {
                text: Arc::new(PdfPageTextLayout { page_index: 1, geometry }),
                images: second.images.clone(),
                trim_bounds: OnceLock::new(),
                geometry: [OnceLock::new(), OnceLock::new()],
                cropped_text: OnceLock::new(),
            });
            let mut cache = document.analysis.borrow_mut();
            let entry = cache.pages.iter_mut().find(|(index, _)| *index == 1).unwrap();
            entry.1 = cached.clone();
            cache.byte_len = cache.pages.iter().map(|(_, analysis)| analysis.byte_len()).sum();
            drop(cache);
            assert_eq!(document.extract_reading_fragments(1)?[0].text, text);
            assert!(Arc::ptr_eq(&document.analysis.borrow_mut().get(1).unwrap(), &cached));
            assert_eq!(document.analysis.borrow().pages.len(), 2);
            fs::remove_file(path).map_err(|error| PdfError::new(error.to_string()))?;
            Ok(())
        })
    }

    #[test]
    fn search_hits_keep_page_context_in_their_excerpts() -> PdfResult<()> {
        let path = fixture_path("search-excerpts")?;
        create_fixture_pdf(&path)?;
        let bytes = fs::read(&path).map_err(|error| PdfError::new(error.to_string()))?;
        let session = PdfDocumentSession::from_bytes("search-excerpts", bytes)?;
        let hits = session.search("searchable")?;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].page_index(), 0);
        assert_eq!(hits[0].excerpt(), "First searchable page");
        assert_eq!(hits[1].page_index(), 1);
        assert_eq!(hits[1].excerpt(), "Second searchable page");
        assert_eq!(hits[0].clone(), hits[0]);
        fs::remove_file(path).map_err(|error| PdfError::new(error.to_string()))?;
        Ok(())
    }

    #[test]
    fn blank_page_classification_keeps_pages_with_other_text() -> PdfResult<()> {
        runtime::run(move || {
            let path = fixture_path("blank-pages")?;
            {
                let pdfium = shared_pdfium()?;
                let mut document = pdfium.create_new_pdf().map_err(|error| PdfError::new(error.to_string()))?;
                let font = document.fonts_mut().helvetica();
                for text in ["", "This page intentionally left blank", "This page intentionally left blank. More text", "Ordinary content"] {
                    let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).map_err(|error| PdfError::new(error.to_string()))?;
                    if !text.is_empty() {
                        page.objects_mut().create_text_object(PdfPoints::new(72.0), PdfPoints::new(720.0), text, font, PdfPoints::new(18.0)).map_err(|error| PdfError::new(error.to_string()))?;
                    }
                }
                document.save_to_file(&path).map_err(|error| PdfError::new(error.to_string()))?;
            }
            let session = LocalDocumentSession::open(&path)?;
            assert_eq!(session.inspect_skippable_pages(), [0, 1]);
            let (_, metadata, _) = inspect_reader_metadata(fs::File::open(&path).map_err(|error| PdfError::new(error.to_string()))?)?;
            assert_eq!(metadata.skippable_pages.as_deref(), Some(&[0, 1][..]));
            fs::remove_file(path).ok();
            Ok(())
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_extraction_and_reader_operations_share_pdfium_access() -> PdfResult<()> {
        let path = fixture_path("sequential-extraction")?;
        create_fixture_pdf(&path)?;
        let bytes = fs::read(&path).map_err(|error| PdfError::new(error.to_string()))?;
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let workers = (0..4)
            .map(|_| {
                let bytes = bytes.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || -> PdfResult<()> {
                    barrier.wait();
                    for _ in 0..4 {
                        let cover = render_first_page_image_from_reader(Cursor::new(bytes.clone()), 320)?;
                        assert_eq!(cover.pixel_width(), 320);
                        let (reader, metadata, _) = inspect_reader_metadata(Cursor::new(bytes.clone()))?;
                        assert_eq!(metadata.pages.len(), 2);
                        let content_hash = metadata.content_hash.clone();
                        let session = PdfDocumentSession::from_reader_with_metadata("concurrent", reader, &content_hash, Some(metadata))?;
                        assert!(!session.search_page(0, "searchable")?.is_empty());
                        session.render_page(0, Some(320))?;
                    }
                    Ok(())
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap()?;
        }
        fs::remove_file(path).map_err(|error| PdfError::new(error.to_string()))?;
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn extracted_image_processing_does_not_hold_pdfium_access() -> PdfResult<()> {
        let path = fixture_path("image-processing")?;
        create_fixture_pdf(&path)?;
        let bytes = fs::read(&path).map_err(|error| PdfError::new(error.to_string()))?;
        inspect_page_images(Cursor::new(bytes.clone()), &[1], |_, _image| {
            let bytes = bytes.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = render_first_page_image_from_reader(Cursor::new(bytes), 320);
                let _ = sender.send(result.map(|image| image.pixel_width()));
            });
            assert_eq!(receiver.recv_timeout(std::time::Duration::from_secs(5)).expect("pixel processing must allow another extraction").unwrap(), 320);
            worker.join().unwrap();
            false
        })?;
        fs::remove_file(path).map_err(|error| PdfError::new(error.to_string()))?;
        Ok(())
    }

    #[test]
    fn ingestion_metadata_round_trips_and_rejects_a_different_revision() -> PdfResult<()> {
        let path = fixture_path("ingestion")?;
        create_fixture_pdf(&path)?;
        let bytes = fs::read(&path).map_err(|e| PdfError::new(e.to_string()))?;
        let (reader, metadata, _) = inspect_reader_metadata(Cursor::new(bytes.clone()))?;
        assert_eq!(reader.position(), 0);
        assert_eq!(metadata.content_hash, blake3::hash(&bytes).to_hex().to_string());
        assert_eq!(metadata.pages.len(), 2);
        let content_hash = metadata.content_hash.clone();
        let expected = PdfDocumentSession::from_bytes("fixture", bytes.clone())?;
        let cached = PdfDocumentSession::from_reader_with_metadata("fixture", reader, &content_hash, Some(metadata.clone()))?;
        assert_eq!(cached.info().pages(), expected.info().pages());
        assert_eq!(cached.info().outline(), expected.info().outline());
        assert_eq!(cached.info().skippable_pages(), metadata.skippable_pages.as_deref().unwrap_or_default());
        assert_eq!(cached.render_page(0, Some(320))?.rgba, expected.render_page(0, Some(320))?.rgba);
        let mut wrong = metadata.clone();
        wrong.content_hash = "0".repeat(64);
        wrong.pages[0].width = 999.0;
        let fallback = PdfDocumentSession::from_reader_with_metadata("fixture", Cursor::new(bytes), &content_hash, Some(wrong))?;
        assert_eq!(fallback.info(), expected.info());
        let mut invalid = metadata;
        invalid.pages[0].width = f32::NAN;
        assert!(!invalid.valid_for(&content_hash));
        let _ = fs::remove_file(path);
        Ok(())
    }

    struct ChunkedReader<R> {
        inner: R,
        chunk_size: usize,
    }

    impl<R: Read> Read for ChunkedReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let chunk_size = self.chunk_size.min(buffer.len());
            self.inner.read(&mut buffer[..chunk_size])
        }
    }

    impl<R: Seek> Seek for ChunkedReader<R> {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn cover_ignores_stored_thumbnail_and_visible_annotations() -> PdfResult<()> {
        let bytes = include_bytes!("../tests/fixtures/cover-annotations.pdf").to_vec();
        let rendered = render_first_page_image_from_reader(Cursor::new(bytes), 100)?;
        assert_eq!(rendered.source(), RenderedPdfImageSource::RenderedPage);
        assert_eq!((rendered.pixel_width(), rendered.pixel_height()), (100, 150));
        assert!(rendered.rgba().chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
        Ok(())
    }

    #[test]
    fn cover_raster_limits_reject_extreme_page_geometry() {
        assert_eq!(cover_render_height(200., 300., 600).unwrap(), 900);
        for (w, h) in [(1., 20_000.), (0., 300.), (200., f32::INFINITY), (f32::NAN, 300.)] {
            assert!(cover_render_height(w, h, 600).is_err());
        }
        assert!(cover_render_height(100., 600., 3200).is_err());
    }

    #[test]
    fn concurrent_background_inspections_keep_documents_valid() -> PdfResult<()> {
        let path = fixture_path("concurrent-inspection")?;
        create_fixture_pdf(&path)?;
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let path = path.clone();
                scope.spawn(move || {
                    for _ in 0..4 {
                        let (count, pages) = inspect_page_text_with_count(std::fs::File::open(&path).unwrap(), 2, 65536).unwrap();
                        assert_eq!(count, 2);
                        assert_eq!(pages.len(), 2);
                    }
                });
            }
        });
        std::fs::remove_file(path).ok();
        Ok(())
    }

    #[test]
    fn offset_page_boxes_keep_crops_text_graphics_and_selection_aligned() -> PdfResult<()> {
        runtime::run(|| {
            let rect = |left, bottom, right, top| PdfRect::new(PdfPoints::new(bottom), PdfPoints::new(left), PdfPoints::new(top), PdfPoints::new(right));
            for (media, crop) in [
                (rect(0.0, 0.0, 612.0, 792.0), Some(rect(90.0, 72.0, 522.0, 720.0))),
                (rect(-90.0, -72.0, 522.0, 720.0), None),
                (rect(90.0, 72.0, 522.0, 720.0), Some(rect(60.0, 40.0, 550.0, 750.0))),
            ] {
                for rotation in [PdfPageRenderRotation::None, PdfPageRenderRotation::Degrees90, PdfPageRenderRotation::Degrees180, PdfPageRenderRotation::Degrees270] {
                    let pdfium = shared_pdfium()?;
                    let mut document = pdfium.create_new_pdf().unwrap();
                    let font = document.fonts_mut().helvetica();
                    let mut pixels = [20, 80, 200, 255].repeat(16);
                    let bitmap = PdfBitmap::from_bytes(4, 4, PdfBitmapFormat::BGRA, &mut pixels).unwrap();
                    let mut image = PdfPageImageObject::new(&document).unwrap();
                    image.set_bitmap(&bitmap).unwrap();
                    image.scale(70.0, 100.0).unwrap();
                    image.translate(PdfPoints::new(180.0), PdfPoints::new(250.0)).unwrap();
                    {
                        let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).unwrap();
                        page.objects_mut().create_text_object(PdfPoints::new(150.0), PdfPoints::new(500.0), "Visible text", font, PdfPoints::new(18.0)).unwrap();
                        page.objects_mut().add_image_object(image).unwrap();
                        page.boundaries_mut().set_media(media).unwrap();
                        if let Some(crop) = crop {
                            page.boundaries_mut().set_crop(crop).unwrap();
                        }
                        page.set_rotation(rotation);
                    }
                    let session = LocalDocumentSession::from_document("offset-boxes".into(), document)?;
                    let page = session.document.pages().get(0).unwrap();
                    let layout = extract_text_layout(&page, 0)?;
                    let config = PdfRenderConfig::new().set_target_size(20000, 20000);
                    for character in layout.geometry.characters().iter().filter(|character| !character.character().is_whitespace()) {
                        // Compare against PDFium's own page-to-device transform,
                        // independent of our crop and rotation calculations.
                        let raw = page.text().unwrap().chars().get(character.source_index()).unwrap().tight_bounds().unwrap();
                        let a = page.points_to_pixels(raw.left(), raw.bottom(), &config).unwrap();
                        let b = page.points_to_pixels(raw.right(), raw.top(), &config).unwrap();
                        let center = ((a.0 + b.0) as f32 / 40000.0, (a.1 + b.1) as f32 / 40000.0);
                        let bounds = character.bounds();
                        assert!(center.0 >= bounds.left() - 0.0001 && center.0 <= bounds.right() + 0.0001, "horizontal text alignment: rotation={rotation:?}");
                        assert!(center.1 >= bounds.top() - 0.0001 && center.1 <= bounds.bottom() + 0.0001, "vertical text alignment: rotation={rotation:?}");
                        assert_eq!(session.character_index_near_point(0, center, (0.0, 0.0))?, Some(character.source_index()));
                    }
                    let hit = session.search_page(0, "Visible")?.remove(0).bounds;
                    let first = layout.geometry.characters()[0].bounds();
                    assert!(hit.left() <= first.right() && hit.right() >= first.left());
                    assert!(hit.top() <= first.bottom() && hit.bottom() >= first.top());
                    let geometry = session.page_geometry(0, true)?;
                    let full = session.render_page_with_margin_trim(0, Some(800), false)?;
                    let cropped = session.render_page_with_margin_trim(0, Some(800), true)?;
                    let ink = |pixels: &[u8]| pixels.chunks_exact(4).filter(|pixel| pixel[..3].iter().any(|value| *value < 240)).count();
                    assert!(ink(full.rgba()) > 1000);
                    assert_eq!(ink(cropped.rgba()), ink(full.rgba()), "crop must retain all visible content: rotation={rotation:?}");
                    let colored = |pixels: &[u8]| pixels.chunks_exact(4).filter(|pixel| pixel[0] != pixel[1] || pixel[1] != pixel[2]).count();
                    let themed = session.render_page_for_display(PageRenderKey::with_margin_trim(0, 800, true).with_paper_color([24; 3]).with_ink_color([230; 3]))?.into_parts();
                    assert_eq!(colored(&themed.pixels), colored(cropped.rgba()), "offset image bounds must preserve image colors");
                    for image in extract_image_bounds(&page) {
                        assert!(image.left() >= geometry.crop.left() && image.right() <= geometry.crop.right());
                        assert!(image.top() >= geometry.crop.top() && image.bottom() <= geometry.crop.bottom());
                    }
                    let width = cropped.pixel_width() as usize;
                    let xs = cropped.rgba().chunks_exact(4).enumerate().filter(|(_, pixel)| pixel[..3].iter().any(|value| *value < 240)).map(|(index, _)| index % width).collect::<Vec<_>>();
                    assert!(*xs.iter().min().unwrap() < 24, "left crop margin is too wide");
                    assert!(width - xs.iter().max().unwrap() < 24, "right crop margin is too wide");
                }
            }
            Ok(())
        })
    }

    #[test]
    fn direct_crop_matches_full_raster_and_display_pixels_preserve_colors() -> PdfResult<()> {
        runtime::run(move || {
            let path = fixture_path("direct-crop")?;
            create_fixture_pdf(&path)?;
            let document = shared_pdfium()?.load_pdf_from_byte_vec(fs::read(&path).unwrap(), None).unwrap();
            document.pages().get(1).unwrap().set_rotation(PdfPageRenderRotation::Degrees90);
            let session = LocalDocumentSession::from_document("crop".into(), document)?;
            for page_index in 0..2 {
                for width in [640, 1024] {
                    let page = session.document.pages().get(page_index as i32).unwrap();
                    let full = page.render_with_config(&PdfRenderConfig::new().set_target_width(width)).unwrap();
                    let image = RgbaImage::from_raw(full.width() as u32, full.height() as u32, full.as_rgba_bytes()).unwrap();
                    let cropped = session.render_page_with_margin_trim(page_index, Some(width as u16), true)?;
                    let crop = cropped.crop();
                    let reference = image::imageops::crop_imm(&image, crop.left(), crop.top(), crop.width(), crop.height()).to_image().into_raw();
                    assert_eq!(cropped.rgba(), reference, "direct crop changed pixels on page {page_index} at {width}");
                    let display = session.render_page_for_display(PageRenderKey::with_margin_trim(page_index, width as u16, true))?;
                    let display = display.into_parts();
                    assert_eq!((display.pixel_width, display.pixel_height, display.crop), (crop.width(), crop.height(), crop));
                    assert_eq!(display.text_layout.geometry, cropped.text_layout.geometry);
                    for (display, rgba) in display.pixels.chunks_exact(4).zip(reference.chunks_exact(4)) {
                        assert_eq!(display, [rgba[2], rgba[1], rgba[0], rgba[3]]);
                    }
                }
            }
            fs::remove_file(path).unwrap();
            Ok(())
        })
    }

    #[test]
    fn display_tiles_reassemble_the_full_raster_without_changing_geometry() -> PdfResult<()> {
        runtime::run(move || {
            let path = fixture_path("display-tiles")?;
            create_fixture_pdf(&path)?;
            let document = shared_pdfium()?.load_pdf_from_byte_vec(fs::read(&path).unwrap(), None).unwrap();
            document.pages().get(1).unwrap().set_rotation(PdfPageRenderRotation::Degrees90);
            let session = LocalDocumentSession::from_document("tiles".into(), document)?;
            for page_index in 0..2 {
                for trim in [false, true] {
                    let geometry = session.page_geometry(page_index, trim)?;
                    for width in [640, 1024] {
                        let key = PageRenderKey::with_margin_trim(page_index, width, trim).with_paper_color([32; 3]).with_ink_color([230; 3]);
                        let full = session.render_page_for_display(key)?.into_parts();
                        let mut assembled = Vec::new();
                        for tile in 0..full.pixel_height.div_ceil(DISPLAY_TILE_HEIGHT) {
                            let rendered = session.render_page_for_display(key.with_tile(tile))?.into_parts();
                            assert!(rendered.pixel_height <= DISPLAY_TILE_HEIGHT + 2);
                            assert_eq!(rendered.pixel_width, full.pixel_width);
                            assert_eq!(rendered.page_crop, geometry.crop);
                            assert_eq!(rendered.break_avoidance, geometry.break_avoidance);
                            assert!(Arc::ptr_eq(&rendered.text_layout, &full.text_layout), "tiles share page text instead of copying it");
                            let start_row = full.crop.top() + tile * DISPLAY_TILE_HEIGHT - rendered.raster_crop.top();
                            let rows = DISPLAY_TILE_HEIGHT.min(full.pixel_height - tile * DISPLAY_TILE_HEIGHT);
                            let stride = full.pixel_width as usize * 4;
                            assembled.extend_from_slice(&rendered.pixels[start_row as usize * stride..(start_row + rows) as usize * stride]);
                        }
                        assert_eq!(assembled, full.pixels, "tile seams or missing pixels on page {page_index}, trim={trim}, width={width}");
                        assert_eq!(session.page_geometry(page_index, trim)?, geometry);
                    }
                }
            }
            fs::remove_file(path).unwrap();
            Ok(())
        })
    }

    #[test]
    fn full_rasters_have_a_pixel_budget_and_tiles_keep_the_requested_resolution() {
        for aspect in [0.01, 1.0, 10.0, 1000.0] {
            let (width, height) = raster_dimensions(3200, aspect, false).unwrap();
            assert!(u64::from(width) * u64::from(height) <= MAX_RASTER_PIXELS);
            let (width, _) = raster_dimensions(3200, aspect, true).unwrap();
            assert_eq!(width, 3200);
            assert!(u64::from(width) * u64::from(DISPLAY_TILE_HEIGHT + 2) <= MAX_RASTER_PIXELS);
        }
        for aspect in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            assert!(raster_dimensions(3200, aspect, false).is_err());
            assert!(raster_dimensions(3200, aspect, true).is_err());
        }
    }

    #[test]
    fn lightweight_page_dimensions_match_loaded_rotated_pages() -> PdfResult<()> {
        runtime::run(move || {
            let mut document = shared_pdfium()?.create_new_pdf().map_err(|error| PdfError::new(error.to_string()))?;
            let mut expected = Vec::new();
            for rotation in [PdfPageRenderRotation::None, PdfPageRenderRotation::Degrees90, PdfPageRenderRotation::Degrees180, PdfPageRenderRotation::Degrees270] {
                let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).map_err(|error| PdfError::new(error.to_string()))?;
                page.set_rotation(rotation);
                expected.push((page.width().value, page.height().value));
            }
            let session = LocalDocumentSession::from_document("rotated".into(), document)?;
            assert_eq!(session.info.pages.iter().map(|page| (page.width_points, page.height_points)).collect::<Vec<_>>(), expected);
            Ok(())
        })
    }

    #[test]
    fn streamed_pdf_first_page_skips_later_page_image_content() -> PdfResult<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let image_size = 2048 * 2048;
        let first = b"BT /F1 18 Tf 72 720 Td (Streamed first page) Tj ET";
        let second = b"q 400 0 0 400 72 200 cm /Im1 Do Q BT /F1 18 Tf 72 720 Td (Later page) Tj ET";
        let stream = |bytes: &[u8]| {
            let mut object = format!("<< /Length {} >>\nstream\n", bytes.len()).into_bytes();
            object.extend_from_slice(bytes);
            object.extend_from_slice(b"\nendstream");
            object
        };
        let mut image = format!("<< /Type /XObject /Subtype /Image /Width 2048 /Height 2048 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {image_size} >>\nstream\n").into_bytes();
        image.extend(std::iter::repeat_n(255, image_size));
        image.extend_from_slice(b"\nendstream");
        let objects = [
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 8 0 R >> >> /Contents 7 0 R >>".to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            stream(first),
            stream(second),
            image,
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            bytes.extend_from_slice(object);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 9\n0000000000 65535 f \n");
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(format!("trailer\n<< /Size 9 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
        struct Counted {
            bytes: Cursor<Vec<u8>>,
            read: Arc<AtomicUsize>,
        }
        impl Read for Counted {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let count = self.bytes.read(out)?;
                self.read.fetch_add(count, Ordering::Relaxed);
                Ok(count)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                self.bytes.seek(from)
            }
        }
        let index = pdf_range_reader::dependency_index(Cursor::new(bytes.clone())).expect("fixture dependency index");
        let middle_of_image = (xref - image_size / 2) as u64;
        let covers_image = |ranges: &[pdf_view_common::PdfByteRange]| ranges.iter().any(|r| r.offset <= middle_of_image && r.offset + r.length > middle_of_image);
        assert!(!covers_image(&index.startup), "global fonts must not pull in later-page images");
        assert!(!covers_image(&index.pages[0]), "page one must not depend on page two's image");
        assert!(covers_image(&index.pages[1]), "image payload belongs in the page bundle");
        let read = Arc::new(AtomicUsize::new(0));
        let session = PdfDocumentSession::from_reader("streamed.pdf", Counted { bytes: Cursor::new(bytes), read: read.clone() })?;
        assert_eq!(session.info().page_count(), 2);
        let first = session.render_page(0, Some(300))?;
        assert!(first.rgba().chunks_exact(4).any(|pixel| pixel[0] < 100));
        assert!(read.load(Ordering::Relaxed) < image_size / 4, "opening page 1 fetched later-page content");
        session.render_page(1, Some(300))?;
        assert!(read.load(Ordering::Relaxed) >= image_size, "visiting page 2 should load its image");
        Ok(())
    }

    #[test]
    fn reader_short_reads_are_completed_before_pdfium_parses_them() -> PdfResult<()> {
        let path = fixture_path("short-reads")?;
        create_fixture_pdf(&path)?;
        let bytes = fs::read(&path).map_err(|error| PdfError::new(error.to_string()))?;
        let reader = ChunkedReader { inner: Cursor::new(bytes), chunk_size: 7 };
        let session = PdfDocumentSession::from_reader(path.display().to_string(), reader)?;
        assert_eq!(session.info().page_count(), 2);

        drop(session);
        fs::remove_file(path).map_err(|error| PdfError::new(error.to_string()))?;
        Ok(())
    }
}
