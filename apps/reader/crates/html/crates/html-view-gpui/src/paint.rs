use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use web_time::Instant;

use gpui::{Bounds, ContentMask, Corners, Edges, Hsla, PaintGlyph, Pixels, RenderImage, SvgRenderer, Window, fill, point, px, rgba, size};
use html_view_core::text_backend::{GlyphId, TextRunId};
use html_view_core::{Painter, TextRunFragment, UsedBorderRadii};
use image::{Frame, RgbaImage, imageops::FilterType};
use kurbo::{Point, Rect};
use peniko::{Color, Image as PenikoImage};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer};
use smallvec::SmallVec;

use crate::text::{GlyphStore, GpuiGlyph, GpuiTextRun, RunStore};

const MAX_IMAGE_PIXELS: f64 = 4_194_304.0;
const MAX_IMAGE_EDGE: f64 = 4096.0;
const IMAGE_CACHE_BYTES: usize = 32 * 1024 * 1024;

// Bound rasterization independently of authored layout size.
fn raster_size(width: f64, height: f64) -> Option<(u32, u32)> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let width = width.max(1.0);
    let height = height.max(1.0);
    let scale = (MAX_IMAGE_EDGE / width).min(MAX_IMAGE_EDGE / height).min(1.0);
    let width = width * scale;
    let height = height * scale;
    let scale = (MAX_IMAGE_PIXELS / (width * height)).sqrt().min(1.0);
    Some(((width * scale).floor().max(1.0) as u32, (height * scale).floor().max(1.0) as u32))
}

#[derive(Default)]
pub(crate) struct ImageCache {
    entries: VecDeque<(Vec<u8>, Arc<RenderImage>, usize)>,
    bytes: usize,
}

impl ImageCache {
    fn get(&mut self, key: &[u8]) -> Option<Arc<RenderImage>> {
        let index = self.entries.iter().position(|(stored, _, _)| stored == key)?;
        let entry = self.entries.remove(index)?;
        let image = entry.1.clone();
        self.entries.push_back(entry);
        Some(image)
    }

    fn insert(&mut self, key: Vec<u8>, image: Arc<RenderImage>) {
        let bytes = image.as_bytes(0).map_or(0, <[u8]>::len);
        if bytes > IMAGE_CACHE_BYTES {
            return;
        }
        while self.bytes + bytes > IMAGE_CACHE_BYTES || self.entries.len() >= 256 {
            if let Some((_, _, evicted)) = self.entries.pop_front() {
                self.bytes -= evicted;
            } else {
                break;
            }
        }
        self.bytes += bytes;
        self.entries.push_back((key, image, bytes));
    }
}

#[derive(Clone)]
pub(crate) enum PaintCommand {
    PushClip { rect: Rect },
    PopClip,
    Fill { rect: Rect, color: Hsla },
    RoundedFill { rect: Rect, radii: UsedBorderRadii, color: Hsla },
    RoundedBorder { rect: Rect, radii: UsedBorderRadii, width: f32, color: Hsla },
    Glyph { glyph: GlyphId, origin: Point, color: Option<Hsla> },
    TextBatch { fragments: Arc<Vec<GpuiTextRunFragment>> },
    Image { rect: Rect, image: Arc<RenderImage> },
}

#[derive(Clone)]
pub(crate) struct GpuiTextRunFragment {
    run: Arc<GpuiTextRun>,
    range: Range<u32>,
    glyph_ranges: SmallVec<[Range<usize>; 2]>,
    origin: Point,
    color: Option<Hsla>,
}

impl GpuiTextRunFragment {
    fn new(run: Arc<GpuiTextRun>, range: Range<u32>, origin: Point, color: Option<Hsla>) -> Self {
        let glyph_ranges = run.glyph_ranges(range.clone());
        Self { run, range, glyph_ranges, origin, color }
    }

    fn resolved(run: Arc<GpuiTextRun>, range: Range<u32>, glyph_ranges: SmallVec<[Range<usize>; 2]>, origin: Point, color: Option<Hsla>) -> Self {
        Self { run, range, glyph_ranges, origin, color }
    }

    fn extend_to(&mut self, end: u32) {
        self.range.end = end;
        self.glyph_ranges = self.run.glyph_ranges(self.range.clone());
    }
}

pub(crate) struct GpuiCommandPainter<'a> {
    images: Option<&'a mut ImageCache>,
    glyphs: GlyphStore,
    text_runs: RunStore,
    pub(crate) commands: Vec<PaintCommand>,
}

impl<'a> GpuiCommandPainter<'a> {
    pub(crate) fn new(images: &'a mut ImageCache, glyphs: GlyphStore, text_runs: RunStore, mut commands: Vec<PaintCommand>) -> Self {
        commands.clear();
        Self { images: Some(images), glyphs, text_runs, commands }
    }

    pub(crate) fn commands_only(glyphs: GlyphStore, text_runs: RunStore, mut commands: Vec<PaintCommand>) -> Self {
        commands.clear();
        Self { images: None, glyphs, text_runs, commands }
    }

    pub(crate) fn into_commands(self) -> Vec<PaintCommand> {
        self.commands
    }

    fn push_text_fragment(&mut self, fragment: GpuiTextRunFragment) {
        if let Some(PaintCommand::TextBatch { fragments }) = self.commands.last_mut() {
            let fragments = Arc::make_mut(fragments);
            if let Some(previous) = fragments.last_mut()
                && Arc::ptr_eq(&previous.run, &fragment.run)
                && previous.range.end == fragment.range.start
                && previous.origin == fragment.origin
                && previous.color == fragment.color
            {
                previous.extend_to(fragment.range.end);
                return;
            }
            fragments.push(fragment);
        } else {
            self.commands.push(PaintCommand::TextBatch { fragments: Arc::new(vec![fragment]) });
        }
    }

    fn push_text_fragments(&mut self, fragments: impl IntoIterator<Item = GpuiTextRunFragment>) {
        for fragment in fragments {
            self.push_text_fragment(fragment);
        }
    }

    fn push_glyph(&mut self, glyph: GlyphId, origin: Point, color: Option<Hsla>) {
        let native = self.glyphs.borrow().get(glyph as usize).cloned();
        match native {
            Some(GpuiGlyph::Empty) => {}
            Some(GpuiGlyph::RunFragment { run, range, glyph_ranges, natural_offset, .. }) => {
                let origin = Point::new(origin.x - f64::from(natural_offset), origin.y);
                self.push_text_fragment(GpuiTextRunFragment::resolved(run, range, glyph_ranges, origin, color));
            }
            Some(GpuiGlyph::Layout { .. }) | None => self.commands.push(PaintCommand::Glyph { glyph, origin, color }),
        }
    }
}

fn gpui_color(color: Color) -> Hsla {
    Hsla::from(rgba(u32::from_be_bytes([color.r, color.g, color.b, color.a])))
}

fn render_image(image: &PenikoImage, target_width: u32, target_height: u32) -> Option<Arc<RenderImage>> {
    let expected = (image.width as usize).checked_mul(image.height as usize)?.checked_mul(4)?;
    if image.data.data().len() != expected {
        return None;
    }
    let mut bytes = image.data.data().to_vec();
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(image.width, image.height, bytes)?;
    let buffer = if image.width == target_width && image.height == target_height { buffer } else { image::imageops::resize(&buffer, target_width, target_height, FilterType::Triangle) };
    Some(Arc::new(RenderImage::new(SmallVec::from_vec(vec![Frame::new(buffer)]))))
}

fn scaled_image_cache_key(hash: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut key = Vec::with_capacity(hash.len() + 9);
    key.push(b'r');
    key.extend_from_slice(&width.to_le_bytes());
    key.extend_from_slice(&height.to_le_bytes());
    key.extend_from_slice(hash);
    key
}

fn render_svg(bytes: &[u8], _intrinsic_size: (u32, u32), target_width: u32, target_height: u32) -> Option<Arc<RenderImage>> {
    let bytes = svg_with_viewport(bytes, target_width, target_height)?;
    SvgRenderer::new(Arc::new(())).render_single_frame(&bytes, 1.0).ok()
}

fn svg_with_viewport(bytes: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let mut reader = Reader::from_reader(bytes);
    let mut writer = Writer::new(Vec::with_capacity(bytes.len() + 32));
    let mut root_rewritten = false;
    loop {
        let event = reader.read_event().ok()?;
        let event = match event {
            Event::Start(start) if !root_rewritten && start.local_name().as_ref() == b"svg" => {
                root_rewritten = true;
                Event::Start(svg_start_with_viewport(&start, width, height)?)
            }
            Event::Empty(start) if !root_rewritten && start.local_name().as_ref() == b"svg" => {
                root_rewritten = true;
                Event::Empty(svg_start_with_viewport(&start, width, height)?)
            }
            Event::Eof => break,
            event => event.into_owned(),
        };
        writer.write_event(event).ok()?;
    }
    root_rewritten.then(|| writer.into_inner())
}

fn svg_start_with_viewport(start: &BytesStart<'_>, width: u32, height: u32) -> Option<BytesStart<'static>> {
    let name = std::str::from_utf8(start.name().as_ref()).ok()?.to_owned();
    let mut rewritten = BytesStart::new(name);
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.ok()?;
        if !matches!(attribute.key.local_name().as_ref(), b"width" | b"height") {
            rewritten.push_attribute(attribute);
        }
    }
    let width = width.to_string();
    let height = height.to_string();
    rewritten.push_attribute(("width", width.as_str()));
    rewritten.push_attribute(("height", height.as_str()));
    Some(rewritten.into_owned())
}

impl Painter for GpuiCommandPainter<'_> {
    fn push_clip(&mut self, rect: Rect) {
        self.commands.push(PaintCommand::PushClip { rect });
    }

    fn pop_clip(&mut self) {
        self.commands.push(PaintCommand::PopClip);
    }

    fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.commands.push(PaintCommand::Fill { rect, color: gpui_color(color) });
    }

    fn fill_rounded_rect(&mut self, rect: Rect, radii: UsedBorderRadii, color: Color) {
        self.commands.push(PaintCommand::RoundedFill { rect, radii, color: gpui_color(color) });
    }

    fn stroke_rounded_rect(&mut self, rect: Rect, radii: UsedBorderRadii, width: f32, color: Color) {
        self.commands.push(PaintCommand::RoundedBorder { rect, radii, width, color: gpui_color(color) });
    }

    fn draw_glyph(&mut self, glyph: GlyphId, origin: Point) {
        self.push_glyph(glyph, origin, None);
    }

    fn draw_glyph_with_color(&mut self, glyph: GlyphId, origin: Point, color: u32) {
        self.push_glyph(glyph, origin, Some(Hsla::from(rgba(color))));
    }

    fn supports_text_runs(&self) -> bool {
        true
    }

    fn draw_text_run(&mut self, run: TextRunId, origin: Point, color: Option<u32>) {
        self.draw_text_run_fragment(run, 0..u32::MAX, origin, color);
    }

    fn draw_text_run_fragment(&mut self, run: TextRunId, range: Range<u32>, origin: Point, color: Option<u32>) {
        let Some(run) = self.text_runs.borrow().get(run as usize).cloned() else {
            debug_assert!(false, "renderer referenced a missing shaped text run");
            return;
        };
        self.push_text_fragment(GpuiTextRunFragment::new(run, range, origin, color.map(|color| Hsla::from(rgba(color)))));
    }

    fn draw_text_run_batch(&mut self, fragments: &[TextRunFragment]) {
        let runs = self.text_runs.borrow();
        let mut batch = Vec::with_capacity(fragments.len());
        for fragment in fragments {
            let Some(run) = runs.get(fragment.run() as usize).cloned() else {
                debug_assert!(false, "renderer referenced a missing shaped text run");
                continue;
            };
            batch.push(GpuiTextRunFragment::new(run, fragment.range(), fragment.origin(), fragment.color().map(|color| Hsla::from(rgba(color)))));
        }
        drop(runs);
        self.push_text_fragments(batch);
    }

    fn draw_image(&mut self, image: &PenikoImage, hash: &[u8], rect: Rect) {
        let Some(images) = self.images.as_deref_mut() else {
            debug_assert!(false, "an image was emitted into a command-only paint layer");
            return;
        };
        let Some((target_width, target_height)) = raster_size(rect.width(), rect.height()) else { return };
        let cache_key = scaled_image_cache_key(hash, target_width, target_height);
        let rendered = images.get(&cache_key).or_else(|| {
            let rendered = render_image(image, target_width, target_height)?;
            images.insert(cache_key, rendered.clone());
            Some(rendered)
        });
        if let Some(image) = rendered {
            self.commands.push(PaintCommand::Image { rect, image });
        }
    }

    fn draw_svg(&mut self, bytes: &[u8], hash: &[u8], intrinsic_size: (u32, u32), rect: Rect) {
        let Some((target_width, target_height)) = raster_size(rect.width(), rect.height()) else { return };
        let mut cache_key = scaled_image_cache_key(hash, target_width, target_height);
        cache_key[0] = b's';
        let rendered = match self.images.as_deref_mut() {
            Some(images) => images.get(&cache_key).or_else(|| {
                let rendered = render_svg(bytes, intrinsic_size, target_width, target_height)?;
                images.insert(cache_key, rendered.clone());
                Some(rendered)
            }),
            None => render_svg(bytes, intrinsic_size, target_width, target_height),
        };
        if let Some(image) = rendered {
            self.commands.push(PaintCommand::Image { rect, image });
        }
    }
}

pub(crate) fn paint_commands(commands: &[PaintCommand], glyph_store: &GlyphStore, bounds: Bounds<Pixels>, window: &mut Window) {
    let started = Instant::now();
    let glyphs = glyph_store.borrow();
    let profile_enabled = std::env::var_os("BOKHEIM_PROFILE_RESIZE").is_some() || std::env::var_os("GPUI_KOBO_PROFILE").is_some();
    let mut profile = PaintProfile::default();
    paint_commands_inner(commands, &glyphs, bounds, window, &mut profile, profile_enabled);
    let duration_ms = started.elapsed().as_millis();
    if profile_enabled {
        println!(
            "HTML_GPUI_PAINT_DETAIL commands={} text_run_commands={} text_run_fragments={} standalone_glyph_commands={} glyphs={} text_run_us={} fragment_us={} layout_us={} fill_us={} image_us={} total_us={}",
            commands.len(),
            profile.text_run_commands,
            profile.text_run_fragments,
            profile.standalone_glyph_commands,
            profile.glyphs,
            profile.text_run_us,
            profile.fragment_us,
            profile.layout_us,
            profile.fill_us,
            profile.image_us,
            started.elapsed().as_micros(),
        );
    }
    if duration_ms >= 10 {
        println!("HTML_GPUI_SCENE commands={} duration_ms={duration_ms}", commands.len());
    }
}

#[derive(Default)]
struct PaintProfile {
    text_run_commands: usize,
    text_run_fragments: usize,
    standalone_glyph_commands: usize,
    glyphs: usize,
    text_run_us: u128,
    fragment_us: u128,
    layout_us: u128,
    fill_us: u128,
    image_us: u128,
}

fn paint_commands_inner(commands: &[PaintCommand], glyphs: &[GpuiGlyph], bounds: Bounds<Pixels>, window: &mut Window, profile: &mut PaintProfile, profile_enabled: bool) {
    let mut index = 0usize;
    while index < commands.len() {
        let command = &commands[index];
        match command {
            PaintCommand::PushClip { rect } => {
                let mut depth = 1usize;
                let mut end = index + 1;
                while end < commands.len() && depth > 0 {
                    match &commands[end] {
                        PaintCommand::PushClip { .. } => depth += 1,
                        PaintCommand::PopClip => depth -= 1,
                        _ => {}
                    }
                    end += 1;
                }
                debug_assert_eq!(depth, 0, "paint clip commands must be balanced");
                let inner_end = end.saturating_sub(1);
                let clip_bounds = Bounds { origin: point(bounds.origin.x + px(rect.x0 as f32), bounds.origin.y + px(rect.y0 as f32)), size: size(px(rect.width().max(0.0) as f32), px(rect.height().max(0.0) as f32)) };
                window.with_content_mask(Some(ContentMask { bounds: clip_bounds }), |window| paint_commands_inner(&commands[index + 1..inner_end], glyphs, bounds, window, profile, profile_enabled));
                index = end;
                continue;
            }
            PaintCommand::PopClip => {
                debug_assert!(false, "unmatched paint clip pop");
            }
            PaintCommand::Fill { rect, color } => {
                let operation_started = profile_enabled.then(Instant::now);
                let rect = Bounds { origin: point(bounds.origin.x + px(rect.x0 as f32), bounds.origin.y + px(rect.y0 as f32)), size: size(px(rect.width() as f32), px(rect.height() as f32)) };
                window.paint_quad(fill(rect, *color));
                if let Some(started) = operation_started {
                    profile.fill_us += started.elapsed().as_micros();
                }
            }
            PaintCommand::RoundedFill { rect, radii, color } => {
                let operation_started = profile_enabled.then(Instant::now);
                let rect = Bounds { origin: point(bounds.origin.x + px(rect.x0 as f32), bounds.origin.y + px(rect.y0 as f32)), size: size(px(rect.width() as f32), px(rect.height() as f32)) };
                window.paint_quad(fill(rect, *color).corner_radii(gpui_radii(*radii)));
                if let Some(started) = operation_started {
                    profile.fill_us += started.elapsed().as_micros();
                }
            }
            PaintCommand::RoundedBorder { rect, radii, width, color } => {
                let operation_started = profile_enabled.then(Instant::now);
                let rect = Bounds { origin: point(bounds.origin.x + px(rect.x0 as f32), bounds.origin.y + px(rect.y0 as f32)), size: size(px(rect.width() as f32), px(rect.height() as f32)) };
                let widths = Edges { top: px(*width), right: px(*width), bottom: px(*width), left: px(*width) };
                window.paint_quad(fill(rect, rgba(0)).corner_radii(gpui_radii(*radii)).border_widths(widths).border_color(*color));
                if let Some(started) = operation_started {
                    profile.fill_us += started.elapsed().as_micros();
                }
            }
            PaintCommand::Glyph { glyph, origin, color } => {
                profile.standalone_glyph_commands += 1;
                let Some(glyph) = glyphs.get(*glyph as usize) else {
                    debug_assert!(false, "paint command referenced a missing interned glyph");
                    continue;
                };
                match glyph {
                    GpuiGlyph::Empty => {}
                    GpuiGlyph::Layout { layout, color: glyph_color } => {
                        let operation_started = profile_enabled.then(Instant::now);
                        let origin = point(bounds.origin.x + px(origin.x as f32), bounds.origin.y + px(origin.y as f32) + layout.ascent);
                        for run in &layout.runs {
                            profile.glyphs += run.glyphs.len();
                            let paint_glyphs = run.glyphs.iter().map(|shaped_glyph| PaintGlyph {
                                origin: point(origin.x + shaped_glyph.position.x, origin.y + shaped_glyph.position.y),
                                font_id: run.font_id,
                                glyph_id: shaped_glyph.id,
                                font_size: layout.font_size,
                                color: (*color).unwrap_or(*glyph_color),
                                is_emoji: shaped_glyph.is_emoji,
                            });
                            if let Err(error) = window.paint_glyphs(paint_glyphs) {
                                eprintln!("failed to paint GPUI glyph batch: {error:#}");
                            }
                        }
                        if let Some(started) = operation_started {
                            profile.layout_us += started.elapsed().as_micros();
                        }
                    }
                    GpuiGlyph::RunFragment { run, glyph_ranges, natural_offset, color: glyph_color, .. } => {
                        let operation_started = profile_enabled.then(Instant::now);
                        let baseline = point(bounds.origin.x + px(origin.x as f32), bounds.origin.y + px(origin.y as f32) + run.ascent);
                        let selected = glyph_ranges.iter().flat_map(|range| run.glyphs[range.clone()].iter());
                        let mut glyph_count = 0usize;
                        let paint_glyphs = selected
                            .map(|shaped_glyph| PaintGlyph {
                                origin: point(baseline.x + shaped_glyph.position.x - px(*natural_offset), baseline.y + shaped_glyph.position.y),
                                font_id: shaped_glyph.font_id,
                                glyph_id: shaped_glyph.glyph_id,
                                font_size: shaped_glyph.font_size,
                                color: (*color).unwrap_or(*glyph_color),
                                is_emoji: shaped_glyph.is_emoji,
                            })
                            .inspect(|_| glyph_count += 1);
                        if let Err(error) = window.paint_glyphs(paint_glyphs) {
                            eprintln!("failed to paint GPUI glyph batch: {error:#}");
                        }
                        profile.glyphs += glyph_count;
                        if let Some(started) = operation_started {
                            profile.fragment_us += started.elapsed().as_micros();
                        }
                    }
                }
            }
            PaintCommand::TextBatch { fragments } => {
                profile.text_run_commands += 1;
                profile.text_run_fragments += fragments.len();
                let operation_started = profile_enabled.then(Instant::now);
                let mut glyph_count = 0usize;
                let paint_glyphs = fragments
                    .iter()
                    .flat_map(|fragment| {
                        let baseline = point(bounds.origin.x + px(fragment.origin.x as f32), bounds.origin.y + px(fragment.origin.y as f32) + fragment.run.ascent);
                        fragment.glyph_ranges.iter().flat_map(|range| fragment.run.glyphs[range.clone()].iter()).map(move |glyph| PaintGlyph {
                            origin: point(baseline.x + glyph.position.x, baseline.y + glyph.position.y),
                            font_id: glyph.font_id,
                            glyph_id: glyph.glyph_id,
                            font_size: glyph.font_size,
                            color: fragment.color.unwrap_or_else(|| fragment.run.color_at(glyph.byte_index)),
                            is_emoji: glyph.is_emoji,
                        })
                    })
                    .inspect(|_| glyph_count += 1);
                if let Err(error) = window.paint_glyphs(paint_glyphs) {
                    eprintln!("failed to paint GPUI glyph batch: {error:#}");
                }
                profile.glyphs += glyph_count;
                if let Some(started) = operation_started {
                    profile.text_run_us += started.elapsed().as_micros();
                }
            }
            PaintCommand::Image { rect, image } => {
                let operation_started = profile_enabled.then(Instant::now);
                let image_bounds = Bounds { origin: point(bounds.origin.x + px(rect.x0 as f32), bounds.origin.y + px(rect.y0 as f32)), size: size(px(rect.width() as f32), px(rect.height() as f32)) };
                let _ = window.paint_image(image_bounds, image_bounds, Corners::default(), image.clone(), 0, false);
                if let Some(started) = operation_started {
                    profile.image_us += started.elapsed().as_micros();
                }
            }
        }
        index += 1;
    }
}

fn gpui_radii(radii: UsedBorderRadii) -> Corners<Pixels> {
    // GPUI's quad primitive has circular rather than elliptical corners. The
    // smaller axis stays inside the CSS ellipse and avoids overpainting.
    Corners {
        top_left: px(radii.top_left.0.min(radii.top_left.1)),
        top_right: px(radii.top_right.0.min(radii.top_right.1)),
        bottom_right: px(radii.bottom_right.0.min(radii.bottom_right.1)),
        bottom_left: px(radii.bottom_left.0.min(radii.bottom_left.1)),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn raster_limits_cover_huge_and_invalid_layout_rectangles() {
        use super::*;
        for (width, height) in [(100_000.0, 100_000.0), (f64::MAX, 1.0), (1.0, f64::MAX), (8192.0, 1024.0)] {
            let (w, h) = raster_size(width, height).unwrap();
            assert!(w > 0 && h > 0 && w <= 4096 && h <= 4096);
            assert!(u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS as u64);
        }
        assert_eq!(raster_size(800.0, 600.0), Some((800, 600)));
        for value in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            assert_eq!(raster_size(value, 100.0), None);
            assert_eq!(raster_size(100.0, value), None);
        }
    }

    #[test]
    fn image_cache_evicts_cold_buffers_and_preserves_recent_hits() {
        use super::*;
        let mut cache = ImageCache::default();
        let mut first = None;
        for id in 0..8 {
            let image = Arc::new(RenderImage::new(SmallVec::from_vec(vec![Frame::new(RgbaImage::new(1024, 1024))])));
            if id == 0 {
                first = Some(Arc::downgrade(&image));
            }
            cache.insert(vec![id], image);
        }
        assert!(cache.get(&[1]).is_some());
        cache.insert(vec![8], Arc::new(RenderImage::new(SmallVec::from_vec(vec![Frame::new(RgbaImage::new(1024, 1024))]))));
        assert!(cache.get(&[0]).is_none());
        assert!(first.unwrap().upgrade().is_none());
        assert!(cache.get(&[1]).is_some());
        assert!(cache.bytes <= IMAGE_CACHE_BYTES);
    }

    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use html_view_core::Painter;

    use super::{GpuiCommandPainter, PaintCommand, render_svg, svg_with_viewport};
    use crate::text::{GpuiGlyph, GpuiRunGlyph, GpuiTextRun};

    const RELATIVE_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" height="50%"><rect width="200" height="100" fill="blue"/></svg>"#;

    #[test]
    fn adjacent_native_glyphs_become_one_text_run_command() {
        let run = Arc::new(GpuiTextRun::new(Arc::<[GpuiRunGlyph]>::from([]), gpui::px(12.0), gpui::px(4.0), Arc::<[usize]>::from([0, 1, 2]), Arc::from([(0..2, gpui::Hsla::from(gpui::rgba(0x202020ff)))])));
        let glyphs = Rc::new(RefCell::new(vec![
            GpuiGlyph::RunFragment { run: run.clone(), range: 0..1, glyph_ranges: run.glyph_ranges(0..1), natural_offset: 0.0, color: gpui::Hsla::from(gpui::rgba(0x202020ff)) },
            GpuiGlyph::RunFragment { run: run.clone(), range: 1..2, glyph_ranges: run.glyph_ranges(1..2), natural_offset: 10.0, color: gpui::Hsla::from(gpui::rgba(0x202020ff)) },
        ]));
        let runs = Rc::new(RefCell::new(vec![run.clone()]));
        let mut painter = GpuiCommandPainter::commands_only(glyphs, runs, Vec::new());

        painter.draw_glyph(0, kurbo::Point::new(100.0, 20.0));
        painter.draw_glyph(1, kurbo::Point::new(110.0, 20.0));

        let commands = painter.into_commands();
        assert_eq!(commands.len(), 1);
        let PaintCommand::TextBatch { fragments } = &commands[0] else { panic!("native glyphs must not be retained as per-glyph commands") };
        assert_eq!(fragments.len(), 1);
        assert!(Arc::ptr_eq(&fragments[0].run, &run));
        assert_eq!(fragments[0].range, 0..2);
        assert_eq!(fragments[0].origin, kurbo::Point::new(100.0, 20.0));
        assert_eq!(fragments[0].color, None);
    }

    #[test]
    fn adjacent_render_core_batches_share_one_gpui_submission() {
        let run = Arc::new(GpuiTextRun::new(Arc::<[GpuiRunGlyph]>::from([]), gpui::px(12.0), gpui::px(4.0), Arc::<[usize]>::from([0, 1, 2]), Arc::from([(0..2, gpui::Hsla::from(gpui::rgba(0x202020ff)))])));
        let glyphs = Rc::new(RefCell::new(Vec::new()));
        let runs = Rc::new(RefCell::new(vec![run]));
        let mut painter = GpuiCommandPainter::commands_only(glyphs, runs, Vec::new());

        painter.draw_text_run_batch(&[html_view_core::TextRunFragment::new(0, 0..1, kurbo::Point::new(10.0, 20.0), None)]);
        painter.draw_text_run_batch(&[html_view_core::TextRunFragment::new(0, 1..2, kurbo::Point::new(30.0, 20.0), None)]);

        let commands = painter.into_commands();
        assert_eq!(commands.len(), 1);
        let PaintCommand::TextBatch { fragments } = &commands[0] else { panic!("adjacent semantic text batches must share one GPUI command") };
        assert_eq!(fragments.len(), 2);
    }

    #[test]
    fn rewrites_svg_root_to_the_used_css_viewport() {
        let rewritten = svg_with_viewport(RELATIVE_SVG, 300, 96).expect("valid SVG");
        let rewritten = std::str::from_utf8(&rewritten).unwrap();
        assert!(rewritten.contains("width=\"300\""));
        assert!(rewritten.contains("height=\"96\""));
        assert!(!rewritten.contains("height=\"50%\""));
    }

    #[test]
    fn renders_full_color_svg_at_the_used_css_viewport() {
        assert!(render_svg(RELATIVE_SVG, (300, 150), 300, 96).is_some());
    }

    #[test]
    fn renders_an_image_embedded_in_svg_as_a_data_url() {
        use base64::Engine;
        use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

        let mut png = Vec::new();
        PngEncoder::new(&mut png).write_image(&[255, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8).expect("encode test PNG");
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 1 1"><image width="1" height="1" xlink:href="data:image/png;base64,{encoded}"/></svg>"#);
        let rendered = render_svg(svg.as_bytes(), (1, 1), 16, 16).expect("SVG with an embedded image should render");
        assert!(rendered.as_bytes(0).expect("first SVG frame").chunks_exact(4).any(|pixel| pixel[3] != 0), "embedded image must produce visible pixels");
    }
}
