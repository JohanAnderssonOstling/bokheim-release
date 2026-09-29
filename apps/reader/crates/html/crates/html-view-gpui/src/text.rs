use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use web_time::Instant;

use gpui::{Hsla, TextRun, font, px, rgba};
use html_view_core::text_backend::{
    FontMetricsRequest, FontRelativeMetrics, FontSlant, GlyphId, GlyphMetric, GlyphResourceStore, GlyphShaper, OpenTypeFeature, ShapeError, ShapedLine, ShapedTextRun, TextRunShapeRequest, TextShapeRequest, TextStyleSpan,
};
use smallvec::SmallVec;

/// OpenType features that form the book renderer's quality baseline. Required
/// composition and positioning are explicit so behavior is stable across
/// platform backends; standard and contextual Latin ligatures remain enabled
/// unless CSS overrides their tag with zero.
const BOOK_QUALITY_FEATURES: [([u8; 4], u32); 9] = [(*b"ccmp", 1), (*b"rlig", 1), (*b"rclt", 1), (*b"mark", 1), (*b"mkmk", 1), (*b"kern", 1), (*b"liga", 1), (*b"clig", 1), (*b"calt", 1)];

fn font_slant_key(slant: FontSlant) -> u8 {
    match slant {
        FontSlant::Normal => 0,
        FontSlant::Italic => 1,
        FontSlant::Oblique => 2,
    }
}

fn book_quality_font_features(overrides: &[OpenTypeFeature]) -> gpui::FontFeatures {
    let mut features = BOOK_QUALITY_FEATURES.to_vec();
    for feature in overrides {
        let entry = (feature.tag(), feature.value());
        if let Some(index) = features.iter().position(|(tag, _)| *tag == entry.0) {
            features[index] = entry;
        } else {
            features.push(entry);
        }
    }
    gpui::FontFeatures(Arc::new(features.into_iter().map(|(tag, value)| (String::from_utf8_lossy(&tag).into_owned(), value)).collect()))
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct GlyphKey {
    ch: char,
    font_size: u32,
    font_weight: u16,
    font_style: u8,
    color: u32,
    family: String,
}

#[derive(Clone)]
pub(crate) enum GpuiGlyph {
    Empty,
    Layout { layout: Arc<gpui::LineLayout>, color: Hsla },
    RunFragment { run: Arc<GpuiTextRun>, range: Range<u32>, glyph_ranges: SmallVec<[Range<usize>; 2]>, natural_offset: f32, color: Hsla },
}

pub(crate) type GlyphStore = Rc<RefCell<Vec<GpuiGlyph>>>;

#[derive(Clone)]
pub(crate) struct GpuiTextRun {
    pub(crate) glyphs: Arc<[GpuiRunGlyph]>,
    pub(crate) ascent: gpui::Pixels,
    pub(crate) descent: gpui::Pixels,
    character_bytes: Arc<[usize]>,
    glyph_segments: Arc<[GpuiGlyphSegment]>,
    colors: Arc<[(Range<usize>, Hsla)]>,
}

impl GpuiTextRun {
    pub(crate) fn new(glyphs: Arc<[GpuiRunGlyph]>, ascent: gpui::Pixels, descent: gpui::Pixels, character_bytes: Arc<[usize]>, colors: Arc<[(Range<usize>, Hsla)]>) -> Self {
        let glyph_segments = Arc::from(build_glyph_segments(&glyphs));
        Self { glyphs, ascent, descent, character_bytes, glyph_segments, colors }
    }

    fn with_colors(&self, colors: Arc<[(Range<usize>, Hsla)]>) -> Self {
        Self { glyphs: self.glyphs.clone(), ascent: self.ascent, descent: self.descent, character_bytes: self.character_bytes.clone(), glyph_segments: self.glyph_segments.clone(), colors }
    }

    /// Resolve a logical character range to slices of the visually ordered
    /// glyph array. Most text produces one slice. Mixed-direction text may
    /// produce several, which remain in the exact order GPUI shaped them.
    pub(crate) fn glyph_ranges(&self, range: Range<u32>) -> SmallVec<[Range<usize>; 2]> {
        let start = self.character_bytes.get(range.start as usize).copied().unwrap_or(0);
        let end = self.character_bytes.get(range.end as usize).copied().unwrap_or(usize::MAX);
        if start >= end {
            return SmallVec::new();
        }

        let mut ranges = SmallVec::<[Range<usize>; 2]>::new();
        for segment in self.glyph_segments.iter() {
            let glyphs = &self.glyphs[segment.glyphs.clone()];
            let (local_start, local_end) = match segment.order {
                GlyphByteOrder::Ascending => (glyphs.partition_point(|glyph| glyph.byte_index < start), glyphs.partition_point(|glyph| glyph.byte_index < end)),
                GlyphByteOrder::Descending => (glyphs.partition_point(|glyph| glyph.byte_index >= end), glyphs.partition_point(|glyph| glyph.byte_index >= start)),
            };
            if local_start == local_end {
                continue;
            }
            let selected = segment.glyphs.start + local_start..segment.glyphs.start + local_end;
            if let Some(previous) = ranges.last_mut()
                && previous.end == selected.start
            {
                previous.end = selected.end;
            } else {
                ranges.push(selected);
            }
        }
        ranges
    }

    pub(crate) fn color_at(&self, byte_index: usize) -> Hsla {
        self.colors.iter().find(|(range, _)| range.contains(&byte_index)).map(|(_, color)| *color).unwrap_or_else(|| Hsla::from(rgba(0x000000ff)))
    }

    fn colors_match(&self, styles: &[TextStyleSpan<'_>]) -> bool {
        self.colors.len() == styles.len() && self.colors.iter().zip(styles).all(|((range, color), style)| *range == style.byte_range() && *color == Hsla::from(rgba(style.color())))
    }
}

#[derive(Clone, Copy)]
enum GlyphByteOrder {
    Ascending,
    Descending,
}

#[derive(Clone)]
struct GpuiGlyphSegment {
    glyphs: Range<usize>,
    order: GlyphByteOrder,
}

fn build_glyph_segments(glyphs: &[GpuiRunGlyph]) -> Vec<GpuiGlyphSegment> {
    if glyphs.is_empty() {
        return Vec::new();
    }

    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut order = None;
    for index in 1..glyphs.len() {
        let next_order = match glyphs[index].byte_index.cmp(&glyphs[index - 1].byte_index) {
            std::cmp::Ordering::Less => Some(GlyphByteOrder::Descending),
            std::cmp::Ordering::Greater => Some(GlyphByteOrder::Ascending),
            std::cmp::Ordering::Equal => None,
        };
        let Some(next_order) = next_order else { continue };
        if let Some(current_order) = order
            && !matches!((current_order, next_order), (GlyphByteOrder::Ascending, GlyphByteOrder::Ascending) | (GlyphByteOrder::Descending, GlyphByteOrder::Descending))
        {
            segments.push(GpuiGlyphSegment { glyphs: start..index, order: current_order });
            start = index;
            order = None;
        } else {
            order = Some(next_order);
        }
    }
    segments.push(GpuiGlyphSegment { glyphs: start..glyphs.len(), order: order.unwrap_or(GlyphByteOrder::Ascending) });
    segments
}

#[derive(Clone, Debug)]
pub(crate) struct GpuiRunGlyph {
    pub(crate) font_id: gpui::FontId,
    pub(crate) glyph_id: gpui::GlyphId,
    pub(crate) position: gpui::Point<gpui::Pixels>,
    pub(crate) font_size: gpui::Pixels,
    pub(crate) byte_index: usize,
    pub(crate) is_emoji: bool,
}

pub(crate) type RunStore = Rc<RefCell<Vec<Arc<GpuiTextRun>>>>;

struct DocumentTextResources {
    resources: GlyphResourceStore,
    table: HashMap<GlyphKey, GlyphId>,
    glyphs: GlyphStore,
    runs: RunStore,
}

struct AppendTextResources {
    resources: GlyphResourceStore,
    table: HashMap<GlyphKey, GlyphId>,
    glyphs: Vec<GpuiGlyph>,
    runs: Vec<Arc<GpuiTextRun>>,
}

#[derive(Clone)]
struct CachedGlyphShape {
    layout: Arc<gpui::LineLayout>,
    metric: GlyphMetric,
    color: Hsla,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LineStyleIdentity {
    byte_len: usize,
    font_size: u32,
    font_weight: u16,
    font_style: u8,
    family: Option<String>,
    features: Vec<([u8; 4], u32)>,
}

struct CachedLineShape {
    text: Box<str>,
    styles: Box<[LineStyleIdentity]>,
    geometry: CachedLineGeometry,
    natural_run: Option<Arc<GpuiTextRun>>,
}

#[derive(Default)]
struct WesternShapingBenchmark {
    runs: u64,
    characters: u64,
    unsupported_runs: u64,
    legacy_glyph_path: Duration,
    full_run_shaping: Duration,
    authoritative_registration: Duration,
    joined_cluster_boundaries: u64,
}

impl WesternShapingBenchmark {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn report(&self) {
        if self.runs == 0 && self.unsupported_runs == 0 {
            return;
        }
        let legacy_us = self.legacy_glyph_path.as_micros().saturating_add(self.full_run_shaping.as_micros());
        let optimized_us = self.authoritative_registration.as_micros().saturating_add(self.full_run_shaping.as_micros());
        let speedup = if optimized_us == 0 { 0.0 } else { legacy_us as f64 / optimized_us as f64 };
        println!(
            "HTML_WESTERN_SHAPING_BENCH runs={} characters={} unsupported_runs={} legacy_total_us={} legacy_cached_glyph_us={} optimized_total_us={} authoritative_run_us={} authoritative_registration_us={} speedup={speedup:.2} width_error_percent=0.000 joined_cluster_boundaries={}",
            self.runs,
            self.characters,
            self.unsupported_runs,
            legacy_us,
            self.legacy_glyph_path.as_micros(),
            optimized_us,
            self.full_run_shaping.as_micros(),
            self.authoritative_registration.as_micros(),
            self.joined_cluster_boundaries,
        );
    }
}

fn western_shaping_benchmark_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("BOKHEIM_WESTERN_SHAPING_BENCHMARK").is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes")))
}

fn is_western_benchmark_text(text: &str) -> bool {
    text.chars().all(|character| {
        character.is_ascii()
            || matches!(
                character as u32,
                0x00a0..=0x024f
                    | 0x0300..=0x036f
                    | 0x1e00..=0x1eff
                    | 0x2000..=0x206f
                    | 0x20a0..=0x20cf
            )
    })
}

/// Cheaply cloned cache payload. Cache identity remains borrowed in the map,
/// so a hit only bumps these reference counts instead of cloning text, style
/// slices, and font-family strings.
#[derive(Clone)]
struct CachedLineGeometry {
    glyphs: Arc<[GpuiRunGlyph]>,
    ascent: f32,
    descent: f32,
    caret_stops: Arc<[f32]>,
    cluster_boundaries: Arc<[bool]>,
    character_bytes: Arc<[usize]>,
}

impl CachedLineShape {
    fn matches(&self, text: &str, styles: &[TextStyleSpan<'_>]) -> bool {
        self.text.as_ref() == text
            && self.styles.len() == styles.len()
            && self.styles.iter().zip(styles).all(|(cached, style)| {
                cached.byte_len == style.byte_range().len()
                    && cached.font_size == style.font_size().to_bits()
                    && cached.font_weight == style.font_weight()
                    && cached.font_style == font_slant_key(style.font_slant())
                    && cached.family.as_deref() == style.font_family()
                    && cached.features.iter().copied().eq(style.font_features().iter().map(|feature| (feature.tag(), feature.value())))
            })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextShapeCacheStats {
    pub glyph_hits: u64,
    pub glyph_misses: u64,
    pub glyph_evictions: u64,
    pub retained_glyphs: usize,
    pub line_hits: u64,
    pub line_misses: u64,
    pub line_evictions: u64,
    pub retained_lines: usize,
}

const SHAPE_CACHE_CAPACITY: usize = 4096;
const LINE_SHAPE_CACHE_CAPACITY: usize = 1024;

pub(crate) struct GpuiTextCache {
    text_system: Arc<gpui::WindowTextSystem>,
    font_families: HashMap<String, String>,
    resources: GlyphResourceStore,
    table: HashMap<GlyphKey, GlyphId>,
    glyphs: GlyphStore,
    runs: RunStore,
    previous_document: Option<DocumentTextResources>,
    previous_append: Option<AppendTextResources>,
    shapes: HashMap<GlyphKey, CachedGlyphShape>,
    shape_order: VecDeque<GlyphKey>,
    shape_cache_stats: TextShapeCacheStats,
    line_shapes: HashMap<u64, CachedLineShape>,
    line_shape_order: VecDeque<u64>,
    western_benchmark: WesternShapingBenchmark,
}

impl GpuiTextCache {
    pub(crate) fn new(text_system: Arc<gpui::WindowTextSystem>) -> Self {
        let font_families = text_system.all_font_names().into_iter().map(|family| (family.to_lowercase(), family)).collect();
        Self {
            text_system,
            font_families,
            resources: GlyphResourceStore::default(),
            table: HashMap::new(),
            glyphs: Rc::new(RefCell::new(Vec::new())),
            runs: Rc::new(RefCell::new(Vec::new())),
            previous_document: None,
            previous_append: None,
            shapes: HashMap::with_capacity(SHAPE_CACHE_CAPACITY),
            shape_order: VecDeque::with_capacity(SHAPE_CACHE_CAPACITY),
            shape_cache_stats: TextShapeCacheStats::default(),
            line_shapes: HashMap::with_capacity(LINE_SHAPE_CACHE_CAPACITY),
            line_shape_order: VecDeque::with_capacity(LINE_SHAPE_CACHE_CAPACITY),
            western_benchmark: WesternShapingBenchmark::default(),
        }
    }

    pub(crate) fn store(&self) -> GlyphStore {
        self.glyphs.clone()
    }

    pub(crate) fn run_store(&self) -> RunStore {
        self.runs.clone()
    }

    pub(crate) fn shape_cache_stats(&self) -> TextShapeCacheStats {
        TextShapeCacheStats { retained_glyphs: self.shapes.len(), retained_lines: self.line_shapes.len(), ..self.shape_cache_stats }
    }

    fn insert_shape(&mut self, key: GlyphKey, shape: CachedGlyphShape) {
        self.shapes.insert(key.clone(), shape);
        self.shape_order.push_back(key);
        while self.shapes.len() > SHAPE_CACHE_CAPACITY {
            let Some(evicted) = self.shape_order.pop_front() else { break };
            if self.shapes.remove(&evicted).is_some() {
                self.shape_cache_stats.glyph_evictions += 1;
            }
        }
    }

    fn insert_line_shape(&mut self, key: u64, shape: CachedLineShape) {
        self.line_shapes.insert(key, shape);
        self.line_shape_order.push_back(key);
        while self.line_shapes.len() > LINE_SHAPE_CACHE_CAPACITY {
            let Some(evicted) = self.line_shape_order.pop_front() else { break };
            if self.line_shapes.remove(&evicted).is_some() {
                self.shape_cache_stats.line_evictions += 1;
            }
        }
    }

    fn available_family(&self, candidates: &[&str]) -> Option<String> {
        candidates.iter().find_map(|candidate| self.font_families.get(&candidate.to_lowercase()).cloned())
    }

    fn resolve_family(&self, family_list: Option<&str>) -> String {
        for family in family_list.into_iter().flat_map(|list| list.split(',')) {
            let family = family.trim().trim_matches(['\'', '"']);
            if family.is_empty() {
                continue;
            }
            let generic = match family.to_ascii_lowercase().as_str() {
                "serif" => self.available_family(&["Libertinus Serif", "Noto Serif", "DejaVu Serif", "Times New Roman"]),
                "sans-serif" => self.available_family(&["Noto Sans", "DejaVu Sans", "Arial", ".SystemUIFont"]),
                "monospace" | "ui-monospace" => self.available_family(&["Noto Sans Mono", "DejaVu Sans Mono", ".ZedMono"]),
                "system-ui" | "ui-sans-serif" => Some(".SystemUIFont".to_owned()),
                "ui-serif" => self.available_family(&["Libertinus Serif", "Noto Serif", "DejaVu Serif"]),
                _ => None,
            };
            if let Some(generic) = generic {
                return generic;
            }
            if let Some(available) = self.font_families.get(&family.to_lowercase()) {
                return available.clone();
            }
        }
        ".SystemUIFont".to_owned()
    }

    fn line_shape_fingerprint(text: &str, styles: &[TextStyleSpan<'_>]) -> u64 {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        styles.len().hash(&mut hasher);
        for style in styles {
            style.byte_range().len().hash(&mut hasher);
            style.font_size().to_bits().hash(&mut hasher);
            style.font_weight().hash(&mut hasher);
            font_slant_key(style.font_slant()).hash(&mut hasher);
            style.font_family().hash(&mut hasher);
            style.font_features().hash(&mut hasher);
        }
        hasher.finish()
    }

    fn set_text_run_colors(&mut self, run_id: u32, text: &str, colors: &[u32]) {
        let Some(spans) = paint_color_spans(text, colors) else { return };
        let mut runs = self.runs.borrow_mut();
        let Some(existing) = runs.get(run_id as usize).cloned() else { return };
        runs[run_id as usize] = Arc::new(existing.with_colors(Arc::from(spans)));
    }
}

fn paint_color_spans(text: &str, colors: &[u32]) -> Option<Vec<(Range<usize>, Hsla)>> {
    if colors.len() != text.chars().count() || colors.is_empty() {
        return None;
    }
    let mut character_bytes = text.char_indices().map(|(byte, _)| byte).collect::<Vec<_>>();
    character_bytes.push(text.len());
    let mut spans = Vec::new();
    let mut start = 0usize;
    let mut color = colors[0];
    for index in 1..colors.len() {
        if colors[index] == color {
            continue;
        }
        spans.push((character_bytes[start]..character_bytes[index], Hsla::from(rgba(color))));
        start = index;
        color = colors[index];
    }
    spans.push((character_bytes[start]..text.len(), Hsla::from(rgba(color))));
    Some(spans)
}

impl GlyphShaper for GpuiTextCache {
    fn reset(&mut self) {
        self.resources.clear();
        self.table.clear();
        self.glyphs.borrow_mut().clear();
        self.runs.borrow_mut().clear();
    }

    fn glyph_resources(&mut self) -> &mut GlyphResourceStore {
        &mut self.resources
    }

    fn begin_append_shaping(&mut self) -> Result<(), ShapeError> {
        debug_assert!(self.previous_append.is_none(), "append shaping transactions cannot be nested");
        self.previous_append = Some(AppendTextResources {
            resources: self.resources.clone(),
            table: self.table.clone(),
            glyphs: self.glyphs.borrow().clone(),
            runs: self.runs.borrow().clone(),
        });
        Ok(())
    }

    fn commit_append_shaping(&mut self) {
        self.previous_append = None;
    }

    fn rollback_append_shaping(&mut self) {
        if let Some(previous) = self.previous_append.take() {
            self.resources = previous.resources;
            self.table = previous.table;
            *self.glyphs.borrow_mut() = previous.glyphs;
            *self.runs.borrow_mut() = previous.runs;
        }
    }

    fn begin_document_shaping(&mut self) {
        if western_shaping_benchmark_enabled() {
            self.western_benchmark.reset();
        }
        debug_assert!(self.previous_document.is_none(), "document shaping transactions cannot be nested");
        let previous =
            DocumentTextResources { resources: std::mem::take(&mut self.resources), table: std::mem::take(&mut self.table), glyphs: std::mem::replace(&mut self.glyphs, Rc::new(RefCell::new(Vec::new()))), runs: std::mem::replace(&mut self.runs, Rc::new(RefCell::new(Vec::new()))) };
        self.previous_document = Some(previous);
    }

    fn commit_document_shaping(&mut self) {
        if western_shaping_benchmark_enabled() {
            self.western_benchmark.report();
        }
        self.previous_document = None;
    }

    fn rollback_document_shaping(&mut self) {
        if let Some(previous) = self.previous_document.take() {
            self.resources = previous.resources;
            self.table = previous.table;
            self.glyphs = previous.glyphs;
            self.runs = previous.runs;
        }
    }

    fn font_relative_metrics(&mut self, request: FontMetricsRequest<'_>) -> Result<FontRelativeMetrics, ShapeError> {
        let family = self.resolve_family(request.font_family());
        let mut gpui_font = font(family);
        gpui_font.weight = gpui::FontWeight(request.font_weight() as f32);
        gpui_font.style = match request.font_slant() {
            FontSlant::Normal => gpui::FontStyle::Normal,
            FontSlant::Italic => gpui::FontStyle::Italic,
            FontSlant::Oblique => gpui::FontStyle::Oblique,
        };
        let font_id = self.text_system.resolve_font(&gpui_font);
        // CSS `ex` is the selected face's x-height, not the ink bounds of its
        // `x` glyph. Those differ for fonts such as WPT's Ahem and can also
        // differ because of hinting. GPUI exposes the face metric directly.
        let font_size = request.font_size();
        let x_height_ratio = f32::from(self.text_system.x_height(font_id, px(font_size))) / font_size;
        let cap_height_ratio = f32::from(self.text_system.cap_height(font_id, px(font_size))) / font_size;
        let zero_run = TextRun { len: 1, font: gpui_font, color: Hsla::from(rgba(0x000000FF)), background_color: None, underline: None, strikethrough: None };
        let zero_layout = self.text_system.layout_line("0", px(font_size), &[zero_run], None);
        let ch_advance_ratio = f32::from(zero_layout.width) / font_size;
        let ascent_ratio = f32::from(self.text_system.ascent(font_id, px(font_size))) / font_size;
        // GPUI exposes descent below the baseline as a signed value; layout's
        // metric contract stores its positive magnitude.
        let descent_ratio = -f32::from(self.text_system.descent(font_id, px(font_size))) / font_size;
        Ok(FontRelativeMetrics::from_line_ratios(x_height_ratio, ch_advance_ratio, cap_height_ratio, ascent_ratio, descent_ratio).unwrap_or_else(FontRelativeMetrics::fallback))
    }

    fn shape_glyph(&mut self, ch: char, font_size: f32, font_weight: u16, font_slant: FontSlant, color: u32, family: Option<&str>) -> Result<GlyphId, ShapeError> {
        let family = self.resolve_family(family);
        let font_style_key = font_slant_key(font_slant);
        let key = GlyphKey { ch, font_size: font_size.to_bits(), font_weight, font_style: font_style_key, color, family: family.clone() };
        if let Some(glyph) = self.table.get(&key)
            && self.resources.contains(*glyph)
        {
            return Ok(*glyph);
        }

        if let Some(cached) = self.shapes.get(&key).cloned() {
            self.shape_cache_stats.glyph_hits += 1;
            let registry_glyph = self.resources.register(cached.metric)?;
            let mut glyphs = self.glyphs.borrow_mut();
            debug_assert_eq!(glyphs.len(), registry_glyph as usize);
            glyphs.push(GpuiGlyph::Layout { layout: cached.layout, color: cached.color });
            drop(glyphs);
            self.table.insert(key, registry_glyph);
            return Ok(registry_glyph);
        }
        self.shape_cache_stats.glyph_misses += 1;

        // GPUI may clamp a zero-pixel request to paintable fallback geometry.
        // CSS defines this case as no ink and no metric contribution, so do
        // not send it through native shaping.
        if font_size == 0.0 {
            let metric = GlyphMetric::try_new(ch, 0.0, 0.0, 0.0, 0.0).map_err(ShapeError::rejected_metric)?;
            let registry_glyph = self.resources.register(metric)?;
            let mut glyphs = self.glyphs.borrow_mut();
            debug_assert_eq!(glyphs.len(), registry_glyph as usize);
            glyphs.push(GpuiGlyph::Empty);
            drop(glyphs);
            self.table.insert(key, registry_glyph);
            return Ok(registry_glyph);
        }

        let mut gpui_font = font(family);
        gpui_font.weight = gpui::FontWeight(font_weight as f32);
        gpui_font.style = match font_slant {
            FontSlant::Normal => gpui::FontStyle::Normal,
            FontSlant::Italic => gpui::FontStyle::Italic,
            FontSlant::Oblique => gpui::FontStyle::Oblique,
        };
        let mut utf8 = [0; 4];
        let text = ch.encode_utf8(&mut utf8);
        let color = Hsla::from(rgba(color));
        let run = TextRun { len: text.len(), font: gpui_font, color, background_color: None, underline: None, strikethrough: None };
        let layout = self.text_system.layout_line(text, px(font_size), &[run], None);
        let mut glyphs = self.glyphs.borrow_mut();
        let glyph = glyphs.len() as GlyphId;
        let metric = GlyphMetric::try_new(ch, f32::from(layout.width), f32::from(layout.ascent), f32::from(layout.descent), f32::from(layout.ascent)).map_err(ShapeError::rejected_metric)?;
        let registry_glyph = self.resources.register(metric)?;
        debug_assert_eq!(glyph, registry_glyph);
        glyphs.push(GpuiGlyph::Layout { layout: layout.clone(), color });
        drop(glyphs);

        self.table.insert(key.clone(), registry_glyph);
        self.insert_shape(key, CachedGlyphShape { layout, metric, color });
        Ok(registry_glyph)
    }

    fn shape_glyph_run(&mut self, request: TextRunShapeRequest<'_>, glyphs: &mut [GlyphId]) -> Result<Option<ShapedTextRun>, ShapeError> {
        let benchmark_enabled = western_shaping_benchmark_enabled();
        let run_text = request.text();
        let run_style = request.style().clone();
        let paint_colors = request.paint_colors();
        let western = is_western_benchmark_text(run_text);
        let full_run_started = Instant::now();
        let shaped = self.shape_text_run(request);
        let full_run_shaping = full_run_started.elapsed();

        // Benchmark the previous path without using its metrics for layout.
        // This is intentionally gated because it allocates legacy resources.
        let mut legacy_glyphs = benchmark_enabled.then(|| vec![GlyphId::MAX; glyphs.len()]);
        let legacy_started = Instant::now();
        if let Some(legacy_glyphs) = legacy_glyphs.as_mut() {
            let mut ascii = [GlyphId::MAX; 128];
            let mut non_ascii = HashMap::<char, GlyphId>::new();
            let style = &run_style;
            for (slot, character) in legacy_glyphs.iter_mut().zip(run_text.chars()) {
                let cached = if character.is_ascii() { ascii[character as usize] } else { non_ascii.get(&character).copied().unwrap_or(GlyphId::MAX) };
                *slot = if cached != GlyphId::MAX {
                    cached
                } else {
                    let glyph = self.shape_glyph(character, style.font_size(), style.font_weight(), style.font_slant(), style.color(), style.font_family())?;
                    if character.is_ascii() {
                        ascii[character as usize] = glyph
                    } else {
                        non_ascii.insert(character, glyph);
                    }
                    glyph
                };
            }
        }
        let legacy_glyph_path = legacy_started.elapsed();

        let joined_cluster_boundaries = shaped.as_ref().ok().and_then(Option::as_ref).map_or(0, |run| run.cluster_boundaries().iter().filter(|boundary| !**boundary).count());
        let registration_started = Instant::now();
        let fast_registered = if western || run_style.font_size() == 0.0 {
            if let Ok(Some(native)) = &shaped
                && let (Some(run_id), Some(caret_stops)) = (native.backend_run(), native.caret_stops())
                && let Some(run) = self.runs.borrow().get(run_id as usize).cloned()
            {
                let mut store = self.glyphs.borrow_mut();
                for (index, ((slot, character), advance)) in glyphs.iter_mut().zip(run_text.chars()).zip(native.advances()).enumerate() {
                    let color = Hsla::from(rgba(paint_colors.and_then(|colors| colors.get(index)).copied().unwrap_or_else(|| run_style.color())));
                    let metric = GlyphMetric::try_new(character, *advance, f32::from(run.ascent), f32::from(run.descent), f32::from(run.ascent)).map_err(ShapeError::rejected_metric)?;
                    let glyph = self.resources.register(metric)?;
                    debug_assert_eq!(glyph as usize, store.len());
                    let range = index as u32..index as u32 + 1;
                    let glyph_ranges = run.glyph_ranges(range.clone());
                    store.push(GpuiGlyph::RunFragment { run: run.clone(), range, glyph_ranges, natural_offset: caret_stops[index], color });
                    *slot = glyph;
                }
                true
            } else {
                false
            }
        } else {
            false
        };
        let authoritative_registration = registration_started.elapsed();

        if !fast_registered {
            if let Some(legacy_glyphs) = legacy_glyphs {
                glyphs.copy_from_slice(&legacy_glyphs);
            } else if let Some(paint_colors) = paint_colors {
                let style = &run_style;
                for (index, (slot, character)) in glyphs.iter_mut().zip(run_text.chars()).enumerate() {
                    let color = paint_colors.get(index).copied().unwrap_or_else(|| style.color());
                    *slot = self.shape_glyph(character, style.font_size(), style.font_weight(), style.font_slant(), color, style.font_family())?;
                }
            } else {
                let mut ascii = [GlyphId::MAX; 128];
                let mut non_ascii = HashMap::<char, GlyphId>::new();
                let style = &run_style;
                for (slot, character) in glyphs.iter_mut().zip(run_text.chars()) {
                    let cached = if character.is_ascii() { ascii[character as usize] } else { non_ascii.get(&character).copied().unwrap_or(GlyphId::MAX) };
                    *slot = if cached != GlyphId::MAX {
                        cached
                    } else {
                        let glyph = self.shape_glyph(character, style.font_size(), style.font_weight(), style.font_slant(), style.color(), style.font_family())?;
                        if character.is_ascii() {
                            ascii[character as usize] = glyph
                        } else {
                            non_ascii.insert(character, glyph);
                        }
                        glyph
                    };
                }
            }
        }

        if benchmark_enabled {
            self.western_benchmark.runs += 1;
            self.western_benchmark.characters += glyphs.len() as u64;
            self.western_benchmark.unsupported_runs += u64::from(!fast_registered);
            self.western_benchmark.legacy_glyph_path += legacy_glyph_path;
            self.western_benchmark.full_run_shaping += full_run_shaping;
            self.western_benchmark.authoritative_registration += if fast_registered { authoritative_registration } else { legacy_glyph_path };
            self.western_benchmark.joined_cluster_boundaries += joined_cluster_boundaries as u64;
        }
        shaped
    }

    fn shape_text_run(&mut self, request: TextRunShapeRequest<'_>) -> Result<Option<ShapedTextRun>, ShapeError> {
        let character_count = request.text().chars().count();
        let paint_colors = request.paint_colors();
        let placements = vec![html_view_core::text_backend::CharacterPlacement::default(); character_count];
        let styles = [request.style().clone()];
        let Some(line_request) = TextShapeRequest::new(0, 0..character_count as u32, request.text(), &styles, &placements) else { return Ok(None) };
        let shaped = self.shape_line(line_request);
        let Some(shaped) = shaped? else { return Ok(None) };
        if let Some(colors) = paint_colors {
            self.set_text_run_colors(shaped.run, request.text(), colors);
        }
        Ok(shaped_text_run_from_line(&shaped))
    }

    fn measure_line(&mut self, request: TextShapeRequest<'_>) -> Result<Option<ShapedTextRun>, ShapeError> {
        let retained_runs = self.runs.borrow().len();
        let shaped = self.shape_line(request);
        self.runs.borrow_mut().truncate(retained_runs);
        let Some(shaped) = shaped? else { return Ok(None) };
        let portable = shaped_text_run_from_line(&shaped).and_then(|run| ShapedTextRun::new(Arc::from(run.advances()), Arc::from(run.cluster_boundaries())));
        Ok(portable)
    }

    fn shape_line(&mut self, request: TextShapeRequest<'_>) -> Result<Option<ShapedLine>, ShapeError> {
        let text = request.text();
        let text_range = request.text_range();
        let styles = request.styles();
        let placements = request.placements();
        let key = Self::line_shape_fingerprint(text, styles);
        let placements_are_natural = placements.iter().all(|placement| placement.extra_advance() == 0.0 && placement.advance_override().is_none() && placement.baseline_shift() == 0.0 && placement.paints());
        let mut colors = None;
        let cached = if let Some(cached) = self.line_shapes.get(&key).filter(|cached| cached.matches(text, styles)) {
            self.shape_cache_stats.line_hits += 1;
            cached.geometry.clone()
        } else {
            self.shape_cache_stats.line_misses += 1;
            let mut character_bytes = text.char_indices().map(|(byte, _)| byte).collect::<Vec<_>>();
            character_bytes.push(text.len());
            let mut text_runs = Vec::with_capacity(styles.len());
            let mut miss_colors = Vec::with_capacity(styles.len());
            for style in styles {
                let family = self.resolve_family(style.font_family());
                let mut gpui_font = font(family);
                gpui_font.weight = gpui::FontWeight(style.font_weight() as f32);
                gpui_font.style = match style.font_slant() {
                    FontSlant::Normal => gpui::FontStyle::Normal,
                    FontSlant::Italic => gpui::FontStyle::Italic,
                    FontSlant::Oblique => gpui::FontStyle::Oblique,
                };
                gpui_font.features = book_quality_font_features(style.font_features());
                let color = Hsla::from(rgba(style.color()));
                let byte_range = style.byte_range();
                text_runs.push(TextRun { len: byte_range.len(), font: gpui_font, color, background_color: None, underline: None, strikethrough: None });
                miss_colors.push((byte_range, color));
            }
            let mut raw_caret_stops = vec![0.0; character_bytes.len()];
            let mut raw_cluster_boundaries = vec![false; character_bytes.len()];
            raw_cluster_boundaries[0] = true;
            *raw_cluster_boundaries.last_mut().expect("validated non-empty line") = true;
            let mut glyphs = Vec::new();
            let mut x_base = 0.0f32;
            let mut ascent = 0.0f32;
            let mut descent = 0.0f32;
            let mut first_style = 0usize;
            while first_style < styles.len() {
                let font_size_bits = styles[first_style].font_size().to_bits();
                let mut end_style = first_style + 1;
                while end_style < styles.len() && styles[end_style].font_size().to_bits() == font_size_bits {
                    end_style += 1;
                }
                let byte_start = styles[first_style].byte_range().start;
                let byte_end = styles[end_style - 1].byte_range().end;
                let group_text = &text[byte_start..byte_end];
                let character_start = character_bytes.partition_point(|byte| *byte < byte_start);
                if styles[first_style].font_size() == 0.0 {
                    let character_count = group_text.chars().count();
                    for offset in 0..=character_count {
                        raw_caret_stops[character_start + offset] = x_base;
                        raw_cluster_boundaries[character_start + offset] = true;
                    }
                    first_style = end_style;
                    continue;
                }
                let layout = self.text_system.layout_line(group_text, px(styles[first_style].font_size()), &text_runs[first_style..end_style], None);
                let (mut local_caret_stops, local_cluster_boundaries) = build_text_boundaries(group_text, &layout);
                let mut group_width = f32::from(layout.width);
                if group_text.chars().last().is_some_and(char::is_whitespace) && local_caret_stops.len() >= 2 && (local_caret_stops[local_caret_stops.len() - 1] - local_caret_stops[local_caret_stops.len() - 2]).abs() <= f32::EPSILON {
                    // Native line layout trims trailing whitespace from its
                    // caret geometry. Probe with a following printable glyph
                    // so the same font shapes that whitespace as interior
                    // content, then retain only the caret before the probe.
                    let mut probe_text = String::with_capacity(group_text.len() + 1);
                    probe_text.push_str(group_text);
                    probe_text.push('x');
                    let mut probe_runs = text_runs[first_style..end_style].to_vec();
                    if let Some(last) = probe_runs.last_mut() {
                        last.len += 1;
                    }
                    let probe_layout = self.text_system.layout_line(&probe_text, px(styles[first_style].font_size()), &probe_runs, None);
                    let (probe_carets, _) = build_text_boundaries(&probe_text, &probe_layout);
                    let original_character_count = group_text.chars().count();
                    if let Some(&corrected_end) = probe_carets.get(original_character_count) {
                        Arc::make_mut(&mut local_caret_stops)[original_character_count] = corrected_end;
                        group_width = group_width.max(corrected_end);
                    }
                }
                for (offset, caret) in local_caret_stops.iter().copied().enumerate() {
                    raw_caret_stops[character_start + offset] = x_base + caret;
                }
                for (offset, boundary) in local_cluster_boundaries.iter().copied().enumerate() {
                    raw_cluster_boundaries[character_start + offset] |= boundary;
                }
                for shaped_run in &layout.runs {
                    for glyph in &shaped_run.glyphs {
                        glyphs.push(GpuiRunGlyph {
                            font_id: shaped_run.font_id,
                            glyph_id: glyph.id,
                            position: gpui::point(px(x_base) + glyph.position.x, glyph.position.y),
                            font_size: layout.font_size,
                            byte_index: byte_start + glyph.index,
                            is_emoji: glyph.is_emoji,
                        });
                    }
                }
                x_base += group_width;
                ascent = ascent.max(f32::from(layout.ascent));
                descent = descent.max(f32::from(layout.descent));
                first_style = end_style;
            }
            let identities = styles
                .iter()
                .map(|style| LineStyleIdentity {
                    byte_len: style.byte_range().len(),
                    font_size: style.font_size().to_bits(),
                    font_weight: style.font_weight(),
                    font_style: font_slant_key(style.font_slant()),
                    family: style.font_family().map(str::to_owned),
                    features: style.font_features().iter().map(|feature| (feature.tag(), feature.value())).collect(),
                })
                .collect::<Box<[_]>>();
            let geometry = CachedLineGeometry { glyphs: Arc::from(glyphs), ascent, descent, caret_stops: Arc::from(raw_caret_stops), cluster_boundaries: Arc::from(raw_cluster_boundaries), character_bytes: Arc::from(character_bytes) };
            self.insert_line_shape(key, CachedLineShape { text: text.into(), styles: identities, geometry: geometry.clone(), natural_run: None });
            colors = Some(miss_colors);
            geometry
        };

        let (glyphs, caret_stops) = if placements_are_natural {
            (cached.glyphs.clone(), cached.caret_stops.clone())
        } else {
            let mut character_bytes = text.char_indices().map(|(byte, _)| byte).collect::<Vec<_>>();
            character_bytes.push(text.len());
            let mut glyphs = cached.glyphs.to_vec();
            let Some(caret_stops) = apply_line_placements(&request, &character_bytes, &cached.caret_stops, &mut glyphs) else { return Ok(None) };
            (Arc::from(glyphs), caret_stops)
        };

        let reusable_run = placements_are_natural.then(|| self.line_shapes.get(&key).filter(|cached| cached.matches(text, styles)).and_then(|cached| cached.natural_run.as_ref()).filter(|run| run.colors_match(styles)).cloned()).flatten();
        let run_resource = if let Some(run) = reusable_run {
            run
        } else {
            let colors = Arc::from(colors.unwrap_or_else(|| styles.iter().map(|style| (style.byte_range(), Hsla::from(rgba(style.color())))).collect::<Vec<_>>()));
            let run = Arc::new(GpuiTextRun::new(glyphs, px(cached.ascent), px(cached.descent), cached.character_bytes.clone(), colors));
            if placements_are_natural && let Some(cached) = self.line_shapes.get_mut(&key).filter(|cached| cached.matches(text, styles)) {
                cached.natural_run = Some(run.clone());
            }
            run
        };
        let run = self.runs.borrow().len() as u32;
        self.runs.borrow_mut().push(run_resource);
        Ok(Some(ShapedLine { line_index: request.line_index(), text_range, run, ascent: cached.ascent, caret_stops, cluster_boundaries: cached.cluster_boundaries }))
    }

    fn begin_line_shaping(&mut self) {}
}

fn shaped_text_run_from_line(shaped: &ShapedLine) -> Option<ShapedTextRun> {
    let character_count = shaped.caret_stops.len().checked_sub(1)?;
    let mut advances = vec![0.0; character_count];
    let boundary_indices = shaped.cluster_boundaries.iter().enumerate().filter_map(|(index, boundary)| boundary.then_some(index)).collect::<Vec<_>>();
    for cluster in boundary_indices.windows(2) {
        let start = cluster[0];
        let end = cluster[1];
        if start >= end || end > character_count {
            continue;
        }
        let width = (shaped.caret_stops[end] - shaped.caret_stops[start]).abs();
        advances[start..end].fill(width / (end - start) as f32);
    }
    ShapedTextRun::with_backend_run(Arc::from(advances), shaped.cluster_boundaries.clone(), shaped.run, shaped.caret_stops.clone(), shaped.ascent)
}

fn apply_line_placements(request: &TextShapeRequest<'_>, character_bytes: &[usize], raw_caret_stops: &[f32], glyphs: &mut Vec<GpuiRunGlyph>) -> Option<Arc<[f32]>> {
    debug_assert_eq!(character_bytes.len(), raw_caret_stops.len());
    let character_count = character_bytes.len().saturating_sub(1);
    let caret_stops = request.adjusted_caret_stops(raw_caret_stops)?;
    let placements = request.placements();

    for glyph in glyphs.iter_mut() {
        let character_index = character_bytes.partition_point(|byte| *byte <= glyph.byte_index).saturating_sub(1).min(character_count - 1);
        glyph.position.x += px(caret_stops[character_index] - raw_caret_stops[character_index]);
        glyph.position.y -= px(placements[character_index].baseline_shift());
    }
    glyphs.retain(|glyph| {
        let character_index = character_bytes.partition_point(|byte| *byte <= glyph.byte_index).saturating_sub(1).min(character_count - 1);
        placements[character_index].paints()
    });
    Some(caret_stops)
}

fn build_text_boundaries(text: &str, layout: &gpui::LineLayout) -> (Arc<[f32]>, Arc<[bool]>) {
    let mut character_bytes = text.char_indices().map(|(byte, _)| byte).collect::<Vec<_>>();
    character_bytes.push(text.len());
    if character_bytes.len() <= 1 {
        return (Arc::from([]), Arc::from([true]));
    }

    let mut cluster_bytes = layout.runs.iter().flat_map(|run| run.glyphs.iter().map(|glyph| glyph.index)).collect::<Vec<_>>();
    cluster_bytes.push(0);
    cluster_bytes.push(text.len());
    cluster_bytes.sort_unstable();
    cluster_bytes.dedup();

    let mut caret_stops = vec![0.0; character_bytes.len()];
    let mut cluster_boundaries = vec![false; character_bytes.len()];
    cluster_boundaries[0] = true;
    *cluster_boundaries.last_mut().expect("non-empty boundary list") = true;
    for bytes in cluster_bytes.windows(2) {
        let byte_start = bytes[0].min(text.len());
        let byte_end = bytes[1].min(text.len());
        if byte_start >= byte_end {
            continue;
        }
        let char_start = character_bytes.partition_point(|byte| *byte < byte_start);
        let char_end = character_bytes.partition_point(|byte| *byte < byte_end);
        if char_start >= char_end {
            continue;
        }
        cluster_boundaries[char_start] = true;
        cluster_boundaries[char_end] = true;
        let x0 = f32::from(layout.x_for_index(byte_start));
        // GPUI's index lookup places an end-of-run caret before trailing
        // whitespace, while the line width includes that whitespace. CSS may
        // keep such a space when an atomic inline item follows in the parent
        // formatting context, so retain the full shaped run advance here.
        let x1 = if byte_end == text.len() { f32::from(layout.width) } else { f32::from(layout.x_for_index(byte_end)) };
        let count = char_end - char_start;
        for index in 0..=count {
            caret_stops[char_start + index] = x0 + (x1 - x0) * index as f32 / count as f32;
        }
    }
    (Arc::from(caret_stops), Arc::from(cluster_boundaries))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{FontSlant, GpuiRunGlyph, GpuiTextCache, GpuiTextRun, TextStyleSpan, is_western_benchmark_text, paint_color_spans};
    use html_view_core::text_backend::OpenTypeFeature;

    fn mapped_run(byte_indices: &[usize]) -> GpuiTextRun {
        let glyphs = byte_indices
            .iter()
            .copied()
            .map(|byte_index| GpuiRunGlyph { font_id: gpui::FontId(0), glyph_id: gpui::GlyphId(0), position: gpui::point(gpui::px(0.0), gpui::px(0.0)), font_size: gpui::px(16.0), byte_index, is_emoji: false })
            .collect::<Vec<_>>();
        GpuiTextRun::new(Arc::from(glyphs), gpui::px(12.0), gpui::px(4.0), Arc::from([0, 1, 2, 3, 4, 5, 6]), Arc::from([(0..6, gpui::Hsla::from(gpui::rgba(0x000000ff)))]))
    }

    #[test]
    fn line_cache_identity_includes_open_type_features() {
        let natural = TextStyleSpan::new(0..6, 18.0, 400, FontSlant::Normal, 0, None).unwrap();
        let small_caps = natural.clone().with_font_features(vec![OpenTypeFeature::new(*b"smcp", 1)]);

        assert_ne!(GpuiTextCache::line_shape_fingerprint("office", &[natural]), GpuiTextCache::line_shape_fingerprint("office", &[small_caps]));
    }

    #[test]
    fn line_cache_identity_distinguishes_italic_and_oblique() {
        let italic = TextStyleSpan::new(0..1, 18.0, 400, FontSlant::Italic, 0, None).unwrap();
        let oblique = TextStyleSpan::new(0..1, 18.0, 400, FontSlant::Oblique, 0, None).unwrap();

        assert_ne!(GpuiTextCache::line_shape_fingerprint("x", &[italic]), GpuiTextCache::line_shape_fingerprint("x", &[oblique]));
    }

    #[test]
    fn western_authoritative_runs_include_combining_marks() {
        assert!(is_western_benchmark_text("A\u{030a}ngstro\u{0308}m"));
    }

    #[test]
    fn paint_colors_become_utf8_byte_spans_without_splitting_geometry() {
        let spans = paint_color_spans("AéB", &[0xff0000ff, 0xff0000ff, 0x0000ffff]).unwrap();

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, 0..3);
        assert_eq!(spans[1].0, 3..4);
    }

    #[test]
    fn glyph_mapping_preserves_visual_order_across_direction_changes() {
        // Visual order: an LTR prefix, then an RTL span, then an LTR suffix.
        let run = mapped_run(&[0, 0, 1, 4, 3, 2, 5]);

        assert_eq!(run.glyph_ranges(0..1).as_slice(), &[0..2]);
        assert_eq!(run.glyph_ranges(1..3).as_slice(), &[2..3, 5..6]);
        assert_eq!(run.glyph_ranges(2..5).as_slice(), &[3..6]);
        assert_eq!(run.glyph_ranges(0..6).as_slice(), &[0..7]);
    }
}
