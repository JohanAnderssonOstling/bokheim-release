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
pub use reader_style::{ReaderInteractionStyle, RgbaColor};
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashSet;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::io::SeekFrom;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

mod annotations;
mod metadata;
mod runtime;
pub use metadata::inspect_reader_metadata;
pub use pdf_view_common::PdfPageSource;
pub use pdf_view_common::{PdfReaderMetadata, PdfStoredAnnotationKind};
#[cfg(not(target_arch = "wasm32"))]
const PDFIUM_THIRD_PARTY_NOTICES: &str = include_str!(env!("PDFIUM_NOTICES_PATH"));
const DEFAULT_RENDER_WIDTH: u16 = 1400;
const MIN_RENDER_WIDTH: u16 = 320;
const MAX_RENDER_WIDTH: u16 = 3200;
#[cfg(test)]
const HEADER_ZONE_BOTTOM: f32 = 0.18;
#[cfg(test)]
const FOOTER_ZONE_TOP: f32 = 0.82;
const ELEMENT_CROP_PADDING: f32 = 0.008;

#[cfg(target_arch = "wasm32")]
pub mod browser;

static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();

#[cfg(test)]
static ANALYZED_PAGE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
static TEXT_EXTRACTION_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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

pub struct PdfDocumentSession {
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
}

impl PageAnalysis {
    fn byte_len(&self) -> usize {
        std::mem::size_of::<Self>() + self.text.geometry.characters().len() * std::mem::size_of::<TextCharacter>() + self.images.capacity() * std::mem::size_of::<NormalizedRect>()
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
pub fn render_first_page_image_from_reader(reader: impl Read + Seek + 'static, target_width: u16) -> PdfResult<RenderedPdfImage> {
    let _pdfium = runtime::lock();
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
pub fn inspect_page_text(reader: impl Read + Seek + 'static, edge_pages: usize, max_text_bytes: usize) -> PdfResult<Vec<(usize, String)>> {
    inspect_page_text_with_count(reader, edge_pages, max_text_bytes).map(|(_, pages)| pages)
}

pub fn inspect_page_text_with_count(reader: impl Read + Seek + 'static, edge_pages: usize, max_text_bytes: usize) -> PdfResult<(usize, Vec<(usize, String)>)> {
    let _pdfium = runtime::lock();
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
pub fn inspect_page_images(reader: impl Read + Seek + 'static, pages: &[usize], mut visit: impl FnMut(usize, RenderedPdfImage) -> bool) -> PdfResult<()> {
    let document = load_document(reader)?;
    for &number in pages.iter().take(12) {
        let Some(index) = number.checked_sub(1).and_then(|index| i32::try_from(index).ok()) else { continue };
        let image = {
            let _pdfium = runtime::lock();
            let Ok(page) = document.pages().get(index) else { continue };
            let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(2200).set_maximum_width(2600).set_maximum_height(3200)).map_err(|error| PdfError::new(format!("rendering inspection page {number}: {error}")))?;
            let image = RenderedPdfImage { rgba: bitmap.as_rgba_bytes(), pixel_width: bitmap.width() as u32, pixel_height: bitmap.height() as u32, source: RenderedPdfImageSource::RenderedPage };
            image
        };
        // OCR and image processing operate on owned pixels outside PDFium.
        if !visit(number, image) {
            break;
        }
    }
    Ok(())
}

// Both native and WASM keep a seekable source alive for on-demand reads.
fn load_document(reader: impl Read + Seek + 'static) -> PdfResult<runtime::Document> {
    let _pdfium = runtime::lock();
    shared_pdfium()?.load_pdf_from_reader(PdfiumExactReader::new(reader), None).map(runtime::Document::from).map_err(|error| PdfError::new(error.to_string()))
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

impl PdfDocumentSession {
    pub fn open(path: impl Into<PathBuf>) -> PdfResult<Self> {
        let path = path.into();
        let bytes = fs::read(&path).map_err(|error| PdfError::new(format!("reading {}: {error}", path.display())))?;
        Self::from_bytes(path.display().to_string(), bytes)
    }

    pub fn from_bytes(source_name: impl Into<String>, bytes: Vec<u8>) -> PdfResult<Self> {
        let _pdfium = runtime::lock();
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

    fn from_document(source_name: String, document: impl Into<runtime::Document>) -> PdfResult<Self> {
        let _pdfium = runtime::lock();
        let document = document.into();
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
        Ok(Self { document, info: PdfDocumentInfo { source_name, pages, outline, skippable_pages: Vec::new() }, analysis: RefCell::new(PageAnalysisCache::default()), page_source: None })
    }

    pub fn info(&self) -> &PdfDocumentInfo {
        &self.info
    }

    /// Inspect the source pages without rasterizing them. An unreadable page is
    /// retained so classification cannot silently hide content.
    pub fn inspect_skippable_pages(&self) -> Vec<usize> {
        let _pdfium = runtime::lock();
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

    pub fn render_page(&self, page_index: usize, target_width: Option<u16>) -> PdfResult<RenderedPdfPage> {
        self.render_page_with_margin_trim(page_index, target_width, false)
    }

    pub fn render_page_with_margin_trim(&self, page_index: usize, target_width: Option<u16>, trim_margins: bool) -> PdfResult<RenderedPdfPage> {
        self.render_page_pixels(page_index, target_width, trim_margins, false)
    }

    /// Renders GPUI's BGRA format directly on the document worker.
    pub fn render_page_for_display(&self, key: PageRenderKey) -> PdfResult<RenderedPdfBgraPage> {
        self.render_page_pixels(key.page_index(), Some(key.target_width()), key.trim_margins(), true).map(RenderedPdfBgraPage)
    }

    fn render_page_pixels(&self, page_index: usize, target_width: Option<u16>, trim_margins: bool, bgra: bool) -> PdfResult<RenderedPdfPage> {
        if let Some(source) = &self.page_source {
            source.prepare_page(page_index).map_err(PdfError::new)?;
        }
        let _pdfium = runtime::lock();
        let mut page = self.document.pages().get(page_index as i32).map_err(|_| PdfError::new(format!("PDF page {} does not exist", page_index + 1)))?;
        suppress_bokheim_annotation_rendering(&mut page);
        let width = target_width.unwrap_or(DEFAULT_RENDER_WIDTH).clamp(MIN_RENDER_WIDTH, MAX_RENDER_WIDTH) as i32;
        let cached = self.analysis.borrow_mut().get(page_index);
        let analysis = if let Some(analysis) = cached {
            analysis
        } else {
            let analysis = Arc::new(PageAnalysis { text: Arc::new(extract_text_layout(&page, page_index)?), images: extract_image_bounds(&page), trim_bounds: OnceLock::new() });
            self.analysis.borrow_mut().insert(page_index, analysis.clone());
            analysis
        };
        // Match PDFium's target-width scaling, retaining full-page dimensions for
        // selection transforms while allocating only the retained crop.
        let pixel_width = width as u32;
        let height = (page.height().value * (width as f32 / page.width().value)).round();
        if !height.is_finite() || height < 1.0 || height >= i32::MAX as f32 {
            return Err(PdfError::new("PDF page raster dimensions are out of range"));
        }
        let pixel_height = height as u32;
        let crop = if trim_margins {
            analysis
                .trim_bounds
                .get_or_init(|| {
                    let elements = analyze_page_elements(&page, &analysis.text);
                    content_bounds(&elements, RepeatingPageBands::default())
                })
                .map(|bounds| pixel_crop_from_normalized(bounds, pixel_width, pixel_height))
                .unwrap_or_else(|| PixelCrop::full(pixel_width, pixel_height))
        } else {
            PixelCrop::full(pixel_width, pixel_height)
        };
        let mut bitmap = PdfBitmap::empty(crop.width() as i32, crop.height() as i32, PdfBitmapFormat::BGRA).map_err(|error| PdfError::new(format!("allocating PDF page raster: {error}")))?;
        let config = PdfRenderConfig::new().set_fixed_size(width, pixel_height as i32).set_origin(-(crop.left() as i32), -(crop.top() as i32)).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(!bgra).render_annotations(true);
        page.render_into_bitmap_with_config(&mut bitmap, &config).map_err(|error| PdfError::new(format!("rendering PDF page {}: {error}", page_index + 1)))?;
        let rgba = bitmap.as_raw_bytes();
        let mut text_layout = analysis.text.clone();
        if !crop.is_full_page() {
            Arc::make_mut(&mut text_layout).apply_crop(crop);
        }
        let image_bands = analysis.images.iter().filter_map(|bounds| crop.full_rect_to_display(*bounds)).map(|bounds| ((bounds.top() - 0.002).max(0.0), (bounds.bottom() + 0.002).min(1.0)));
        let break_avoidance = sorted_vertical_bands(text_layout.break_avoidance_bands().into_iter().chain(image_bands));
        Ok(RenderedPdfPage { rgba, pixel_width: crop.width(), pixel_height: crop.height(), crop, text_layout, break_avoidance: break_avoidance.into() })
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
    /// is behind the same lock that page rendering needs, so readers search a
    /// chunk of pages at a time rather than holding the session for the whole
    /// document.
    pub fn search_page(&self, page_index: usize, query: &str) -> PdfResult<Vec<PdfSearchHit>> {
        let _pdfium = runtime::lock();
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let page = self.document.pages().get(page_index as i32).map_err(|_| PdfError::new(format!("PDF page {} does not exist", page_index + 1)))?;
        let page_width = page.width().value.max(1.0);
        let page_height = page.height().value.max(1.0);
        let text = page.text().map_err(|error| PdfError::new(format!("extracting text from page {}: {error}", page_index + 1)))?;
        let search = text.search(query, &PdfSearchOptions::new()).map_err(|error| PdfError::new(format!("searching page {}: {error}", page_index + 1)))?;
        let mut hits = Vec::new();
        search.iter(PdfSearchDirection::SearchForward).for_each(|segments| {
            segments.iter().for_each(|segment| {
                hits.push(PdfSearchHit { page_index, bounds: normalized_pdf_bounds(segment.bounds(), page_width, page_height) });
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
        let _pdfium = runtime::lock();
        let page = self.document.pages().get(page_index as i32).map_err(|_| PdfError::new(format!("PDF page {} does not exist", page_index + 1)))?;
        let width = page.width().value.max(1.0);
        let height = page.height().value.max(1.0);
        let text = page.text().map_err(|error| PdfError::new(format!("extracting text from page {}: {error}", page_index + 1)))?;
        let x = PdfPoints::new(point.0.clamp(0.0, 1.0) * width);
        let y = PdfPoints::new((1.0 - point.1.clamp(0.0, 1.0)) * height);
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
}

impl PdfDocumentInfo {
    pub fn open(path: impl Into<PathBuf>) -> PdfResult<Self> {
        Ok(PdfDocumentSession::open(path)?.info)
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
}

fn page_is_skippable(page: &PdfPage<'_>) -> bool {
    if page.annotations().len() != 0 || page.objects().iter().any(|object| object.as_text_object().is_none()) {
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

#[derive(Clone, Debug)]
pub struct RenderedPdfPage {
    rgba: Vec<u8>,
    pixel_width: u32,
    pixel_height: u32,
    crop: PixelCrop,
    text_layout: Arc<PdfPageTextLayout>,
    break_avoidance: Arc<[(f32, f32)]>,
}

/// A page whose pixels are already in GPUI's BGRA order. Kept distinct from
/// `RenderedPdfPage` so RGBA consumers cannot accidentally swap its colors.
pub struct RenderedPdfBgraPage(RenderedPdfPage);

impl RenderedPdfBgraPage {
    pub fn into_parts(self) -> (Vec<u8>, u32, u32, PixelCrop, Arc<PdfPageTextLayout>, Arc<[(f32, f32)]>) {
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

    pub fn select_text(&self, start: (f32, f32), end: (f32, f32)) -> Option<PdfTextSelection> {
        self.text_layout.select(start, end)
    }

    pub fn into_parts(self) -> (Vec<u8>, u32, u32, PixelCrop, Arc<PdfPageTextLayout>, Arc<[(f32, f32)]>) {
        (self.rgba, self.pixel_width, self.pixel_height, self.crop, self.text_layout, self.break_avoidance)
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

    fn apply_crop(&mut self, crop: PixelCrop) {
        self.geometry = self.geometry.transformed(PageTransform::new(crop, PageRotation::None));
    }

    fn break_avoidance_bands(&self) -> Vec<(f32, f32)> {
        merge_line_rects(self.geometry.characters().iter().filter(|character| !character.character().is_control() && !character.character().is_whitespace()).map(|character| character.bounds()))
            .into_iter()
            .map(|line| ((line.top() - 0.002).max(0.0), (line.bottom() + 0.002).min(1.0)))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfSearchHit {
    page_index: usize,
    bounds: NormalizedRect,
}

impl PdfSearchHit {
    pub fn page_index(&self) -> usize {
        self.page_index
    }

    pub fn bounds(&self) -> NormalizedRect {
        self.bounds
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageRenderKey {
    page_index: usize,
    target_width: u16,
    trim_margins: bool,
}

impl PageRenderKey {
    pub fn new(page_index: usize, target_width: u16) -> Self {
        Self::with_margin_trim(page_index, target_width, false)
    }

    pub fn with_margin_trim(page_index: usize, target_width: u16, trim_margins: bool) -> Self {
        Self { page_index, target_width: target_width.clamp(MIN_RENDER_WIDTH, MAX_RENDER_WIDTH), trim_margins }
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
    #[cfg(test)]
    TEXT_EXTRACTION_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let width = page.width().value.max(1.0);
    let height = page.height().value.max(1.0);
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
            Some(TextCharacter::new(character.index(), unicode, normalized_pdf_bounds(bounds, width, height)))
        })
        .collect::<Vec<_>>();
    Ok(PdfPageTextLayout { page_index, geometry: PageTextGeometry::new(characters) })
}

fn analyze_page_elements(page: &PdfPage<'_>, text_layout: &PdfPageTextLayout) -> PageElements {
    #[cfg(test)]
    ANALYZED_PAGE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let page_width = page.width().value.max(1.0);
    let page_height = page.height().value.max(1.0);
    let text_lines = text_lines(text_layout.geometry());
    let mut graphics = Vec::new();
    for object in page.objects().iter() {
        collect_graphic_bounds(&object, page_width, page_height, &mut graphics);
    }
    PageElements { text_lines, graphics }
}

fn collect_graphic_bounds(object: &PdfPageObject<'_>, page_width: f32, page_height: f32, graphics: &mut Vec<NormalizedRect>) {
    if object.as_text_object().is_some() {
        return;
    }
    if let Some(form) = object.as_x_object_form_object() {
        for child in form.iter() {
            collect_graphic_bounds(&child, page_width, page_height, graphics);
        }
        return;
    }
    let Some(bounds) = object.bounds().ok().map(|bounds| normalized_pdf_bounds(bounds.to_rect(), page_width, page_height)) else {
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

#[cfg(test)]
fn repeating_page_bands(pages: &[PageElements]) -> Vec<RepeatingPageBands> {
    let required_pages = ((pages.len() as f32 * 0.4).ceil() as usize).max(2);
    if pages.len() < required_pages {
        return vec![RepeatingPageBands::default(); pages.len()];
    }
    pages
        .iter()
        .enumerate()
        .map(|(page_index, page)| {
            let mut bands = RepeatingPageBands::default();
            for line in &page.text_lines {
                let zone = line_zone(line.bounds);
                let Some(zone) = zone else { continue };
                let matching_pages = pages
                    .iter()
                    .enumerate()
                    .filter(|(candidate_page, _)| *candidate_page != page_index)
                    .filter(|(_, candidate)| candidate.text_lines.iter().any(|candidate| line_zone(candidate.bounds) == Some(zone) && vertically_aligned(line.bounds, candidate.bounds) && text_overlap(&line.text, &candidate.text) >= 0.8))
                    .count()
                    + 1;
                if matching_pages < required_pages {
                    continue;
                }
                match zone {
                    PageLineZone::Header => bands.header_bottom = Some(bands.header_bottom.unwrap_or(0.0).max((line.bounds.bottom() + 0.01).min(1.0))),
                    PageLineZone::Footer => bands.footer_top = Some(bands.footer_top.unwrap_or(1.0).min((line.bounds.top() - 0.01).max(0.0))),
                }
            }
            bands
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
enum PageLineZone {
    Header,
    Footer,
}

#[cfg(test)]
fn line_zone(bounds: NormalizedRect) -> Option<PageLineZone> {
    let center = bounds.top() + bounds.height() * 0.5;
    if center <= HEADER_ZONE_BOTTOM {
        Some(PageLineZone::Header)
    } else if center >= FOOTER_ZONE_TOP {
        Some(PageLineZone::Footer)
    } else {
        None
    }
}

#[cfg(test)]
fn vertically_aligned(left: NormalizedRect, right: NormalizedRect) -> bool {
    let overlap = left.bottom().min(right.bottom()) - left.top().max(right.top());
    let overlap_ratio = overlap.max(0.0) / left.height().min(right.height()).max(f32::EPSILON);
    let center_distance = ((left.top() + left.height() * 0.5) - (right.top() + right.height() * 0.5)).abs();
    overlap_ratio >= 0.5 || center_distance <= 0.012
}

#[cfg(test)]
fn text_overlap(left: &str, right: &str) -> f32 {
    let left = normalized_text_tokens(left);
    let right = normalized_text_tokens(right);
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let common = left.intersection(&right).count() as f32;
    2.0 * common / (left.len() + right.len()) as f32
}

#[cfg(test)]
fn normalized_text_tokens(text: &str) -> HashSet<String> {
    text.to_lowercase().split(|character: char| !character.is_alphanumeric()).filter(|token| !token.is_empty()).map(|token| if token.chars().all(|character| character.is_ascii_digit()) { "#".to_owned() } else { token.to_owned() }).collect()
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

fn normalized_pdf_bounds(bounds: PdfRect, page_width: f32, page_height: f32) -> NormalizedRect {
    NormalizedRect::new(bounds.left().value / page_width, 1.0 - bounds.top().value / page_height, bounds.width().value / page_width, bounds.height().value / page_height)
}

fn extract_image_bounds(page: &PdfPage<'_>) -> Vec<NormalizedRect> {
    let page_width = page.width().value.max(1.0);
    let page_height = page.height().value.max(1.0);
    page.objects().iter().filter(|object| object.as_image_object().is_some()).filter_map(|object| object.bounds().ok().map(|bounds| bounds.to_rect())).map(|bounds| normalized_pdf_bounds(bounds, page_width, page_height)).collect()
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
    let pdfium = PDFIUM.get_or_init(runtime::initialize);
    pdfium.as_ref().map_err(|error| PdfError::new(error.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    static PDFIUM_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn fixture_path(label: &str) -> PdfResult<PathBuf> {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| PdfError::new(error.to_string()))?.as_nanos();
        Ok(std::env::temp_dir().join(format!("bokheim-pdf-{label}-{suffix}.pdf")))
    }

    include!("streaming_tests.rs");

    fn create_fixture_pdf(path: &Path) -> PdfResult<()> {
        let pdfium = shared_pdfium()?;
        let mut document = pdfium.create_new_pdf().map_err(|error| PdfError::new(error.to_string()))?;
        let font = document.fonts_mut().helvetica();
        for text in ["First searchable page", "Second searchable page"] {
            let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).map_err(|error| PdfError::new(error.to_string()))?;
            page.objects_mut().create_text_object(PdfPoints::new(72.0), PdfPoints::new(720.0), text, font, PdfPoints::new(18.0)).map_err(|error| PdfError::new(error.to_string()))?;
        }
        document.save_to_file(path).map_err(|error| PdfError::new(error.to_string()))
    }

    #[test]
    fn blank_page_classification_keeps_pages_with_other_text() -> PdfResult<()> {
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let session = PdfDocumentSession::open(&path)?;
        assert_eq!(session.inspect_skippable_pages(), [0, 1]);
        let (_, metadata, _) = inspect_reader_metadata(fs::File::open(&path).map_err(|error| PdfError::new(error.to_string()))?)?;
        assert_eq!(metadata.skippable_pages.as_deref(), Some(&[0, 1][..]));
        fs::remove_file(path).ok();
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_extraction_and_reader_operations_share_pdfium_access() -> PdfResult<()> {
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
                        let checksum = metadata.checksum.clone();
                        let session = PdfDocumentSession::from_reader_with_metadata("concurrent", reader, &checksum, Some(metadata))?;
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
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = fixture_path("ingestion")?;
        create_fixture_pdf(&path)?;
        let bytes = fs::read(&path).map_err(|e| PdfError::new(e.to_string()))?;
        let (reader, metadata, _) = inspect_reader_metadata(Cursor::new(bytes.clone()))?;
        assert_eq!(reader.position(), 0);
        assert_eq!(metadata.checksum, blake3::hash(&bytes).to_hex().to_string());
        assert_eq!(metadata.pages.len(), 2);
        let checksum = metadata.checksum.clone();
        let expected = PdfDocumentSession::from_bytes("fixture", bytes.clone())?;
        let cached = PdfDocumentSession::from_reader_with_metadata("fixture", reader, &checksum, Some(metadata.clone()))?;
        assert_eq!(cached.info().pages(), expected.info().pages());
        assert_eq!(cached.info().outline(), expected.info().outline());
        assert_eq!(cached.info().skippable_pages(), metadata.skippable_pages.as_deref().unwrap_or_default());
        assert_eq!(cached.render_page(0, Some(320))?.rgba, expected.render_page(0, Some(320))?.rgba);
        let mut wrong = metadata.clone();
        wrong.checksum = "0".repeat(64);
        wrong.pages[0].width = 999.0;
        let fallback = PdfDocumentSession::from_reader_with_metadata("fixture", Cursor::new(bytes), &checksum, Some(wrong))?;
        assert_eq!(fallback.info(), expected.info());
        let mut invalid = metadata;
        invalid.pages[0].width = f32::NAN;
        assert!(!invalid.valid_for(&checksum));
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

    fn fake_page(byte_len: usize) -> Arc<RenderedPdfPage> {
        Arc::new(RenderedPdfPage {
            rgba: vec![0; byte_len],
            pixel_width: 1,
            pixel_height: 1,
            crop: PixelCrop::full(1, 1),
            text_layout: Arc::new(PdfPageTextLayout { page_index: 0, geometry: PageTextGeometry::default() }),
            break_avoidance: Arc::from([]),
        })
    }

    #[test]
    fn cover_ignores_stored_thumbnail_and_visible_annotations() -> PdfResult<()> {
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    fn page_render_keys_bound_requested_width() {
        assert_eq!(PageRenderKey::new(2, 1).target_width(), MIN_RENDER_WIDTH);
        assert_eq!(PageRenderKey::new(2, u16::MAX).target_width(), MAX_RENDER_WIDTH);
        assert_ne!(PageRenderKey::new(2, 800), PageRenderKey::with_margin_trim(2, 800, true));
    }

    #[test]
    fn page_cache_evicts_least_recently_used_pages() {
        let mut cache = PageRenderCache::new(20);
        let first = PageRenderKey::new(0, 800);
        let second = PageRenderKey::new(1, 800);
        let third = PageRenderKey::new(2, 800);
        cache.insert(first, fake_page(10));
        cache.insert(second, fake_page(10));
        assert!(cache.get(first).is_some());
        cache.insert(third, fake_page(10));
        assert!(cache.get(first).is_some());
        assert!(cache.get(second).is_none());
        assert!(cache.get(third).is_some());
        assert_eq!(cache.byte_len(), 20);
    }

    #[test]
    fn oversized_page_is_not_cached() {
        let mut cache = PageRenderCache::new(5);
        cache.insert(PageRenderKey::new(0, 800), fake_page(6));
        assert!(cache.is_empty());
    }

    #[test]
    fn selection_rects_merge_characters_on_the_same_line() {
        let layout = PdfPageTextLayout { page_index: 0, geometry: PageTextGeometry::new([TextCharacter::new(0, 'A', NormalizedRect::new(0.1, 0.2, 0.04, 0.03)), TextCharacter::new(1, 'B', NormalizedRect::new(0.14, 0.2, 0.04, 0.03))]) };
        let selection = layout.select((0.11, 0.21), (0.17, 0.21)).expect("text selection");
        assert_eq!(selection.text(), "AB");
        assert_eq!(selection.rects().len(), 1);
        assert!((selection.rects()[0].width() - 0.08).abs() < 0.0001);
    }

    #[test]
    fn concurrent_background_inspections_keep_documents_valid() -> PdfResult<()> {
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    fn prepared_pdfium_session_opens_once_renders_searches_and_selects() -> PdfResult<()> {
        let _pdfium_guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = fixture_path("reader")?;
        create_fixture_pdf(&path)?;
        let session = PdfDocumentSession::open(&path)?;
        assert_eq!(session.info().page_count(), 2);
        assert!(session.info().pages().iter().all(|page| page.aspect_ratio() > 0.0));
        let rendered = session.render_page(1, Some(640))?;
        assert_eq!(rendered.pixel_width(), 640);
        assert!(rendered.pixel_height() > rendered.pixel_width());
        assert_eq!(rendered.rgba().len(), rendered.pixel_width() as usize * rendered.pixel_height() as usize * 4);
        assert!(!rendered.break_avoidance().is_empty(), "text lines should protect fit-width slice boundaries");
        let hits = session.search("Second searchable")?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page_index(), 1);
        let hit = hits[0].bounds();
        let selection = rendered.select_text((hit.left() + 0.001, hit.top() + hit.height() * 0.5), (hit.left() + hit.width(), hit.top() + hit.height() * 0.5)).expect("selection from search bounds");
        assert!(selection.text().contains("Second searchable"));

        let trimmed = session.render_page_with_margin_trim(1, Some(640), true)?;
        assert!(!trimmed.crop().is_full_page(), "element trim bounds should crop the rendered page");
        assert!(trimmed.pixel_width() < trimmed.crop().full_width());
        assert_eq!(trimmed.rgba().len(), trimmed.pixel_width() as usize * trimmed.pixel_height() as usize * 4);
        let mapped = crop_normalized_rect(trimmed.crop(), hit).expect("search hit remains inside the trimmed page");
        let trimmed_selection = trimmed.select_text((mapped.left() + 0.001, mapped.top() + mapped.height() * 0.5), (mapped.left() + mapped.width(), mapped.top() + mapped.height() * 0.5)).expect("selection from trimmed search bounds");
        assert!(trimmed_selection.text().contains("Second searchable"));

        drop(session);
        fs::remove_file(path).map_err(|error| PdfError::new(error.to_string()))?;
        Ok(())
    }

    #[test]
    fn direct_crop_matches_full_raster_and_display_pixels_preserve_colors() -> PdfResult<()> {
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = fixture_path("direct-crop")?;
        create_fixture_pdf(&path)?;
        let document = shared_pdfium()?.load_pdf_from_byte_vec(fs::read(&path).unwrap(), None).unwrap();
        document.pages().get(1).unwrap().set_rotation(PdfPageRenderRotation::Degrees90);
        let session = PdfDocumentSession::from_document("crop".into(), document)?;
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
                let (bgra, w, h, display_crop, layout, _) = display.into_parts();
                assert_eq!((w, h, display_crop), (crop.width(), crop.height(), crop));
                assert_eq!(layout.geometry, cropped.text_layout.geometry);
                for (display, rgba) in bgra.chunks_exact(4).zip(reference.chunks_exact(4)) {
                    assert_eq!(display, [rgba[2], rgba[1], rgba[0], rgba[3]]);
                }
            }
        }
        fs::remove_file(path).unwrap();
        Ok(())
    }

    #[test]
    fn lightweight_page_dimensions_match_loaded_rotated_pages() -> PdfResult<()> {
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut document = shared_pdfium()?.create_new_pdf().map_err(|error| PdfError::new(error.to_string()))?;
        let mut expected = Vec::new();
        for rotation in [PdfPageRenderRotation::None, PdfPageRenderRotation::Degrees90, PdfPageRenderRotation::Degrees180, PdfPageRenderRotation::Degrees270] {
            let mut page = document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).map_err(|error| PdfError::new(error.to_string()))?;
            page.set_rotation(rotation);
            expected.push((page.width().value, page.height().value));
        }
        let session = PdfDocumentSession::from_document("rotated".into(), document)?;
        assert_eq!(session.info.pages.iter().map(|page| (page.width_points, page.height_points)).collect::<Vec<_>>(), expected);
        Ok(())
    }

    #[test]
    fn page_analysis_is_reused_across_zoom_and_trim_without_changing_output() -> PdfResult<()> {
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = fixture_path("analysis-cache")?;
        create_fixture_pdf(&path)?;
        TEXT_EXTRACTION_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        ANALYZED_PAGE_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        let session = PdfDocumentSession::open(&path)?;
        assert_eq!(TEXT_EXTRACTION_COUNT.load(std::sync::atomic::Ordering::Relaxed), 0);
        let first = session.render_page(0, Some(640))?;
        let zoomed = session.render_page(0, Some(1280))?;
        assert!(Arc::ptr_eq(&first.text_layout, &zoomed.text_layout));
        let trimmed = session.render_page_with_margin_trim(0, Some(640), true)?;
        assert!(!trimmed.crop.is_full_page());
        assert!(!trimmed.text_layout.geometry.characters().is_empty());
        let _ = session.render_page_with_margin_trim(0, Some(1280), true)?;
        assert_eq!(TEXT_EXTRACTION_COUNT.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(ANALYZED_PAGE_COUNT.load(std::sync::atomic::Ordering::Relaxed), 1);
        *session.analysis.borrow_mut() = PageAnalysisCache::default();
        let cold = session.render_page_with_margin_trim(0, Some(640), true)?;
        assert_eq!(trimmed.rgba, cold.rgba);
        assert_eq!(trimmed.crop, cold.crop);
        assert_eq!(trimmed.text_layout.geometry, cold.text_layout.geometry);
        assert_eq!(trimmed.break_avoidance, cold.break_avoidance);
        let restored = session.render_page(0, Some(640))?;
        assert_eq!(first.text_layout.geometry, restored.text_layout.geometry, "cropping must not mutate cached full-page text");
        assert_eq!(first.rgba, restored.rgba);
        fs::remove_file(path).map_err(|error| PdfError::new(error.to_string()))?;
        Ok(())
    }

    #[test]
    fn analysis_cache_bounds_empty_pages_and_rejects_oversized_pages() {
        let mut cache = PageAnalysisCache::default();
        for page in 0..=MAX_ANALYSIS_CACHE_PAGES {
            cache.insert(page, Arc::new(PageAnalysis { text: Arc::new(PdfPageTextLayout::default()), images: Vec::new(), trim_bounds: OnceLock::new() }));
        }
        assert_eq!(cache.pages.len(), MAX_ANALYSIS_CACHE_PAGES);
        assert!(cache.get(0).is_none());
        let oversized =
            Arc::new(PageAnalysis { text: Arc::new(PdfPageTextLayout::default()), images: vec![NormalizedRect::new(0.0, 0.0, 1.0, 1.0); ANALYSIS_CACHE_BUDGET / std::mem::size_of::<NormalizedRect>()], trim_bounds: OnceLock::new() });
        cache.insert(MAX_ANALYSIS_CACHE_PAGES + 1, oversized);
        assert!(cache.get(MAX_ANALYSIS_CACHE_PAGES + 1).is_none());
        assert!(cache.get(MAX_ANALYSIS_CACHE_PAGES).is_some());
    }

    #[test]
    fn analysis_cache_evicts_by_memory_and_recency() {
        let analysis =
            || Arc::new(PageAnalysis { text: Arc::new(PdfPageTextLayout::default()), images: vec![NormalizedRect::new(0.0, 0.0, 1.0, 1.0); ANALYSIS_CACHE_BUDGET / 3 / std::mem::size_of::<NormalizedRect>()], trim_bounds: OnceLock::new() });
        let mut cache = PageAnalysisCache::default();
        cache.insert(0, analysis());
        cache.insert(1, analysis());
        assert!(cache.get(0).is_some());
        cache.insert(2, analysis());
        assert!(cache.get(0).is_some());
        assert!(cache.get(1).is_none());
        assert!(cache.get(2).is_some());
        assert!(cache.byte_len <= ANALYSIS_CACHE_BUDGET);
    }

    #[test]
    fn first_page_image_skips_document_wide_reader_analysis() -> PdfResult<()> {
        let _pdfium_guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = fixture_path("first-page-image")?;
        create_fixture_pdf(&path)?;
        ANALYZED_PAGE_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);

        let rendered = render_first_page_image(&path, MIN_RENDER_WIDTH)?;

        assert_eq!(rendered.source(), RenderedPdfImageSource::RenderedPage);
        assert_eq!(rendered.pixel_width(), MIN_RENDER_WIDTH as u32);
        assert!(rendered.pixel_height() > rendered.pixel_width());
        assert_eq!(rendered.rgba().len(), rendered.pixel_width() as usize * rendered.pixel_height() as usize * 4);
        assert_eq!(ANALYZED_PAGE_COUNT.load(std::sync::atomic::Ordering::Relaxed), 0);

        ANALYZED_PAGE_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        let session = PdfDocumentSession::open(&path)?;
        assert_eq!(ANALYZED_PAGE_COUNT.load(std::sync::atomic::Ordering::Relaxed), 0, "opening the reader must not analyze every page before page 1 can render");
        let reader_rendered = session.render_page(0, Some(300))?;
        assert_eq!(rendered.pixel_width(), reader_rendered.pixel_width());
        assert_eq!(rendered.pixel_height(), reader_rendered.pixel_height());
        // Covers disable form rendering, selecting PDFium's matrix render path.
        // Its antialiasing can differ from the reader's integer-coordinate path.
        let ink = |pixels: &[u8]| pixels.chunks_exact(4).filter(|p| p[..3].iter().any(|v| *v < 128)).count();
        let cover_ink = ink(rendered.rgba());
        let reader_ink = ink(reader_rendered.rgba());
        assert!(cover_ink > 0 && cover_ink.abs_diff(reader_ink) * 10 <= reader_ink, "cover and reader text coverage differs: {cover_ink}/{reader_ink}");
        assert_eq!(ANALYZED_PAGE_COUNT.load(std::sync::atomic::Ordering::Relaxed), 0);

        let _ = session.render_page_with_margin_trim(0, Some(300), true)?;
        assert_eq!(ANALYZED_PAGE_COUNT.load(std::sync::atomic::Ordering::Relaxed), 1, "margin analysis should be limited to the page being rendered");

        drop(session);
        fs::remove_file(path).map_err(|error| PdfError::new(error.to_string()))?;
        Ok(())
    }

    #[test]
    fn streamed_pdf_first_page_skips_later_page_image_content() -> PdfResult<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let _pdfium_guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

    #[test]
    fn vertical_break_avoidance_bands_stay_independent() {
        assert_eq!(sorted_vertical_bands([(0.5, 0.7), (0.1, 0.2), (0.18, 0.3), (0.8, 0.9)]), vec![(0.1, 0.2), (0.18, 0.3), (0.5, 0.7), (0.8, 0.9)]);
    }

    #[test]
    fn text_break_avoidance_groups_characters_by_line_only() {
        let layout = PdfPageTextLayout {
            page_index: 0,
            geometry: PageTextGeometry::new([
                TextCharacter::new(0, 'A', NormalizedRect::new(0.1, 0.20, 0.04, 0.03)),
                TextCharacter::new(1, 'B', NormalizedRect::new(0.14, 0.20, 0.04, 0.03)),
                TextCharacter::new(2, 'C', NormalizedRect::new(0.1, 0.25, 0.04, 0.03)),
            ]),
        };
        assert_eq!(layout.break_avoidance_bands().len(), 2);
    }

    #[test]
    fn repeating_aligned_headers_and_footers_are_excluded_from_element_bounds() {
        let page = |page_number: usize, body_top: f32| PageElements {
            text_lines: vec![
                PageTextLine { text: "Scientific Reports | nature.com".to_owned(), bounds: NormalizedRect::new(0.08, 0.03, 0.5, 0.025) },
                PageTextLine { text: format!("Page {page_number}"), bounds: NormalizedRect::new(0.45, 0.94, 0.1, 0.02) },
                PageTextLine { text: format!("Distinct body text {page_number}"), bounds: NormalizedRect::new(0.12, body_top, 0.7, 0.3) },
            ],
            graphics: vec![NormalizedRect::new(0.05, 0.06, 0.9, 0.002)],
        };
        let pages = vec![page(1, 0.2), page(2, 0.21), page(3, 0.19), page(4, 0.2)];
        let bands = repeating_page_bands(&pages);
        assert!(bands.iter().all(|bands| bands.header_bottom.is_some() && bands.footer_top.is_some()));
        let cropped = content_bounds(&pages[0], bands[0]).expect("body content bounds");
        assert!(cropped.top() > 0.15);
        assert!(cropped.bottom() < 0.9);
    }

    #[test]
    fn similar_text_at_different_vertical_positions_is_not_a_running_header() {
        let pages = [0.03, 0.25, 0.45]
            .into_iter()
            .map(|top| PageElements { text_lines: vec![PageTextLine { text: "Repeated chapter wording".to_owned(), bounds: NormalizedRect::new(0.1, top, 0.6, 0.03) }], graphics: Vec::new() })
            .collect::<Vec<_>>();
        assert!(repeating_page_bands(&pages).iter().all(|bands| bands.header_bottom.is_none()));
    }
}
