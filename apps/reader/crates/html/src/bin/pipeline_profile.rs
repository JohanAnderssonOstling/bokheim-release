use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use html::engine::Engine;
use html::layout::{FontSlant, GlyphId, GlyphMetric, GlyphRegistry, GlyphShaper, LayoutConstraints, LayoutTimings, ShapedTextRun, TextRunShapeRequest};
use html::pipeline::{
    BookStylesheetCache, BuildPipelineTimings, DocumentFactory, FontEnvironmentRevision, ImageMetricsRevision, PaintSettingsRevision, PipelineInputs, ResourceRevision, SourceRevision, StyleEnvironment, StylesheetRevision,
    parse_html_document,
};
use html::resources::ResourceProvider;
use html_book::{BookSourceFormat, BookSource};

#[derive(Default)]
struct ProfileShaper {
    glyphs: HashMap<(char, u32), GlyphId>,
}

impl GlyphShaper for ProfileShaper {
    fn reset(&mut self) {
        self.glyphs.clear();
    }

    fn shape_glyph<'a>(&mut self, glyph_metrics: &mut GlyphRegistry<'a>, ch: char, font_size: f32, _font_weight: u16, _font_slant: FontSlant, _color: u32, _family: Option<&str>) -> Result<GlyphId, html::layout::ShapeError> {
        let key = (ch, font_size.to_bits());
        if let Some(&glyph) = self.glyphs.get(&key) {
            return Ok(glyph);
        }

        let metric = GlyphMetric::try_new(ch, font_size * 0.5, font_size * 0.75, font_size * 0.25, 0.0).map_err(html::layout::ShapeError::rejected_metric)?;
        let glyph = glyph_metrics.register(metric)?;
        self.glyphs.insert(key, glyph);
        Ok(glyph)
    }

    fn shape_glyph_run<'a>(&mut self, glyph_metrics: &mut GlyphRegistry<'a>, request: TextRunShapeRequest<'_>, glyphs: &mut [GlyphId]) -> Result<Option<ShapedTextRun>, html::layout::ShapeError> {
        let mut ascii = [GlyphId::MAX; 128];
        let mut non_ascii = HashMap::<char, GlyphId>::new();
        let style = request.style();
        for (slot, character) in glyphs.iter_mut().zip(request.text().chars()) {
            let cached = if character.is_ascii() { ascii[character as usize] } else { non_ascii.get(&character).copied().unwrap_or(GlyphId::MAX) };
            let glyph = if cached != GlyphId::MAX {
                cached
            } else {
                let glyph = self.shape_glyph(glyph_metrics, character, style.font_size(), style.font_weight(), style.font_slant(), style.color(), style.font_family())?;
                if character.is_ascii() {
                    ascii[character as usize] = glyph;
                } else {
                    non_ascii.insert(character, glyph);
                }
                glyph
            };
            *slot = glyph;
        }
        Ok(None)
    }
}

#[derive(Default)]
struct Totals {
    source_read: Duration,
    parse_html: Duration,
    build: BuildPipelineTimings,
    shape: Duration,
    layout: Duration,
    layout_detail: LayoutTimings,
    failures: usize,
    epubs: usize,
    files: usize,
    stylesheet_cache_entries: usize,
    stylesheet_cache_bytes: usize,
}

#[derive(Default)]
struct WarmTotals {
    initial_update: Duration,
    cached_update: Duration,
    epubs: usize,
    files: usize,
    failures: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProfileMode {
    Cold,
    Warm,
    Both,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BookCacheMode {
    Off,
    On,
}

impl Totals {
    fn add_build(&mut self, timing: BuildPipelineTimings) {
        self.build.build_dom_tree += timing.build_dom_tree;
        self.build.parse_default_css += timing.parse_default_css;
        self.build.parse_author_css += timing.parse_author_css;
        self.build.resolve_css_imports += timing.resolve_css_imports;
        self.build.prepare_style_rules += timing.prepare_style_rules;
        self.build.resolve_styles += timing.resolve_styles;
        self.build.selector_index += timing.selector_index;
        self.build.resolver_setup += timing.resolver_setup;
        self.build.selector_matching += timing.selector_matching;
        self.build.cascade += timing.cascade;
        self.build.style_store += timing.style_store;
        self.build.build_layout_inputs += timing.build_layout_inputs;
        self.build.rebuild_document_toc += timing.rebuild_document_toc;
    }

    fn add_layout(&mut self, timing: LayoutTimings) {
        self.layout_detail.clear_layout_output += timing.clear_layout_output;
        self.layout_detail.layout_tree_traversal += timing.layout_tree_traversal;
        self.layout_detail.root_box_layout += timing.root_box_layout;
        self.layout_detail.finalize_layout += timing.finalize_layout;
        self.layout_detail.sort_lines_and_remap_images += timing.sort_lines_and_remap_images;
        self.layout_detail.collect_inline_decorations += timing.collect_inline_decorations;
        self.layout_detail.rebuild_image_fragments_by_line += timing.rebuild_image_fragments_by_line;
        self.layout_detail.layout_box_total += timing.layout_box_total;
        self.layout_detail.layout_block_children += timing.layout_block_children;
        self.layout_detail.layout_table += timing.layout_table;
        self.layout_detail.layout_table_row += timing.layout_table_row;
        self.layout_detail.layout_runs += timing.layout_runs;
        self.layout_detail.layout_runs_around_float_exclusions += timing.layout_runs_around_float_exclusions;
        self.layout_detail.build_inline_tokens += timing.build_inline_tokens;
        self.layout_detail.build_inline_tokens_from_runs += timing.build_inline_tokens_from_runs;
        self.layout_detail.break_lines += timing.break_lines;
        self.layout_detail.break_lines_knuth += timing.break_lines_knuth;
        self.layout_detail.emit_lines += timing.emit_lines;
        self.layout_detail.measure_line += timing.measure_line;
        self.layout_detail.write_line_fragments += timing.write_line_fragments;
        self.layout_detail.place_float_anchor += timing.place_float_anchor;
    }
}

fn main() {
    let (mode, book_cache_mode, root) = parse_arguments();
    let mut epub_files = Vec::new();
    collect_epub_files(&root, &mut epub_files);
    epub_files.sort();
    assert!(!epub_files.is_empty(), "no EPUB files found under {}", root.display());

    if mode != ProfileMode::Warm {
        let mut totals = Totals::default();
        for epub_path in &epub_files {
            totals.epubs += 1;
            let book = BookSource::from_reader(BookSourceFormat::Epub, fs::File::open(epub_path).expect("failed to open EPUB")).expect("failed to parse EPUB");
            let provider = book.provider();
            let stylesheet_cache = (book_cache_mode == BookCacheMode::On).then(BookStylesheetCache::default);
            let mut chapters = book.document_uris().to_vec();
            chapters.sort();
            for uri in chapters {
                match profile_file(provider.clone(), &uri, stylesheet_cache.as_ref(), &mut totals) {
                    Ok(()) => totals.files += 1,
                    Err(error) => {
                        totals.failures += 1;
                        eprintln!("{}::{uri}: {error}", epub_path.display());
                    }
                }
            }
            if let Some(cache) = stylesheet_cache {
                let stats = cache.stats();
                totals.stylesheet_cache_entries += stats.entries;
                totals.stylesheet_cache_bytes += stats.bytes;
            }
        }
        print_cold_totals(&totals);
        if totals.failures != 0 {
            std::process::exit(1);
        }
    }

    if mode != ProfileMode::Cold {
        let mut totals = WarmTotals::default();
        for epub_path in &epub_files {
            totals.epubs += 1;
            let book = BookSource::from_reader(BookSourceFormat::Epub, fs::File::open(epub_path).expect("failed to open EPUB")).expect("failed to parse EPUB");
            let provider = book.provider();
            let mut chapters = book.document_uris().to_vec();
            chapters.sort();
            for uri in chapters {
                match profile_warm_file(provider.clone(), &uri, &mut totals) {
                    Ok(()) => totals.files += 1,
                    Err(error) => {
                        totals.failures += 1;
                        eprintln!("{}::{uri}: {error}", epub_path.display());
                    }
                }
            }
        }
        println!("\n=== html-pipeline warm session profile ===");
        println!("EPUBs: {}  chapters: {}  failures: {}", totals.epubs, totals.files, totals.failures);
        print_timing("initial session update", totals.initial_update, totals.epubs);
        print_timing("cached session update", totals.cached_update, totals.epubs);
        if totals.failures != 0 {
            std::process::exit(1);
        }
    }
}

fn parse_arguments() -> (ProfileMode, BookCacheMode, PathBuf) {
    let mut mode = ProfileMode::Cold;
    let mut book_cache_mode = BookCacheMode::Off;
    let mut root = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--mode" => match arguments.next().as_deref() {
                Some("cold") => mode = ProfileMode::Cold,
                Some("warm") => mode = ProfileMode::Warm,
                Some("both") => mode = ProfileMode::Both,
                _ => panic!("--mode must be cold, warm, or both"),
            },
            "--book-cache" => match arguments.next().as_deref() {
                Some("off") => book_cache_mode = BookCacheMode::Off,
                Some("on") => book_cache_mode = BookCacheMode::On,
                _ => panic!("--book-cache must be on or off"),
            },
            "--help" | "-h" => {
                println!("Usage: pipeline_profile [--mode cold|warm|both] [--book-cache on|off] [TestData directory]");
                std::process::exit(0);
            }
            _ if root.is_none() => root = Some(PathBuf::from(argument)),
            _ => panic!("unexpected argument; usage: pipeline_profile [--mode cold|warm|both] [--book-cache on|off] [TestData directory]"),
        }
    }
    (mode, book_cache_mode, root.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../TestData")))
}

fn print_cold_totals(totals: &Totals) {
    println!("\n=== html-pipeline profile ===");
    println!("EPUBs: {}  chapters: {}  failures: {}", totals.epubs, totals.files, totals.failures);
    if totals.stylesheet_cache_entries != 0 {
        println!("book stylesheet cache: {} entries, {:.1} KiB", totals.stylesheet_cache_entries, totals.stylesheet_cache_bytes as f64 / 1024.0);
    }
    print_timing("source read", totals.source_read, totals.epubs);
    print_timing("parse HTML", totals.parse_html, totals.epubs);
    print_timing("DOM build", totals.build.build_dom_tree, totals.epubs);
    print_timing("TOC build", totals.build.rebuild_document_toc, totals.epubs);
    print_timing("default CSS parse", totals.build.parse_default_css, totals.epubs);
    print_timing("author CSS parse", totals.build.parse_author_css, totals.epubs);
    print_timing("CSS import resolve", totals.build.resolve_css_imports, totals.epubs);
    print_timing("style rule prepare", totals.build.prepare_style_rules, totals.epubs);
    print_timing("style resolution", totals.build.resolve_styles, totals.epubs);
    print_timing("  selector index", totals.build.selector_index, totals.epubs);
    print_timing("  resolver setup", totals.build.resolver_setup, totals.epubs);
    print_timing("  selector matching", totals.build.selector_matching, totals.epubs);
    print_timing("  cascade", totals.build.cascade, totals.epubs);
    print_timing("  style interning", totals.build.style_store, totals.epubs);
    print_timing("layout-input prepare", totals.build.build_layout_inputs, totals.epubs);
    print_timing("glyph shaping", totals.shape, totals.epubs);
    print_timing("layout total", totals.layout, totals.epubs);

    println!("\n--- layout flow (inclusive; children overlap parents) ---");
    print_timing("clear output", totals.layout_detail.clear_layout_output, totals.epubs);
    print_timing("root box layout", totals.layout_detail.root_box_layout, totals.epubs);
    print_timing("  all box calls", totals.layout_detail.layout_box_total, totals.epubs);
    print_timing("    block children", totals.layout_detail.layout_block_children, totals.epubs);
    print_timing("    tables", totals.layout_detail.layout_table, totals.epubs);
    print_timing("    table rows", totals.layout_detail.layout_table_row, totals.epubs);
    print_timing("    inline runs", totals.layout_detail.layout_runs, totals.epubs);
    print_timing("      inline runs/float exclusions", totals.layout_detail.layout_runs_around_float_exclusions, totals.epubs);
    print_timing("      build tokens", totals.layout_detail.build_inline_tokens, totals.epubs);
    print_timing("        tokens from runs", totals.layout_detail.build_inline_tokens_from_runs, totals.epubs);
    print_timing("      line breaking", totals.layout_detail.break_lines, totals.epubs);
    print_timing("      Knuth-Plass", totals.layout_detail.break_lines_knuth, totals.epubs);
    print_timing("      emit lines", totals.layout_detail.emit_lines, totals.epubs);
    print_timing("        measure lines", totals.layout_detail.measure_line, totals.epubs);
    print_timing("        write fragments", totals.layout_detail.write_line_fragments, totals.epubs);
    print_timing("      place floats", totals.layout_detail.place_float_anchor, totals.epubs);
    print_timing("finalize layout", totals.layout_detail.finalize_layout, totals.epubs);
    print_timing("  sort lines/images", totals.layout_detail.sort_lines_and_remap_images, totals.epubs);
    print_timing("  inline decorations", totals.layout_detail.collect_inline_decorations, totals.epubs);
    print_timing("  image line index", totals.layout_detail.rebuild_image_fragments_by_line, totals.epubs);
}

fn profile_file(provider: Arc<dyn ResourceProvider>, uri: &str, stylesheet_cache: Option<&BookStylesheetCache>, totals: &mut Totals) -> Result<(), String> {
    let read_started = Instant::now();
    let html = provider.read_string(uri).map_err(|error| error.to_string())?;
    totals.source_read += read_started.elapsed();

    let parse_started = Instant::now();
    let parsed = parse_html_document(&html);
    totals.parse_html += parse_started.elapsed();

    let mut factory = DocumentFactory::new();
    factory.set_resource_context(provider, uri.to_owned());
    if let Some(stylesheet_cache) = stylesheet_cache {
        factory.set_book_stylesheet_cache(stylesheet_cache.clone());
    }
    let (prepared, build_timing) = factory.build_pipeline_from_parsed_timed(&parsed, &[]);
    totals.add_build(build_timing);

    let mut shaper = ProfileShaper::default();
    let shape_started = Instant::now();
    let shaped = prepared.shape(&mut shaper).map_err(|error| error.to_string())?;
    totals.shape += shape_started.elapsed();

    let layout_started = Instant::now();
    let (_document, layout_timing) = shaped.layout_with_timings(LayoutConstraints::new(600.0, 16.0).map_err(|error| error.to_string())?);
    totals.layout += layout_started.elapsed();
    totals.add_layout(layout_timing);
    Ok(())
}

fn profile_warm_file(provider: Arc<dyn ResourceProvider>, uri: &str, totals: &mut WarmTotals) -> Result<(), String> {
    let html = provider.read_string(uri).map_err(|error| error.to_string())?;
    let inputs = PipelineInputs {
        source: html,
        markup_syntax: html::pipeline::MarkupSyntax::from_uri(uri),
        user_styles: Vec::new(),
        reader_overrides: Default::default(),
        note_flow: Default::default(),
        source_revision: SourceRevision::INITIAL,
        base_uri: uri.to_owned(),
        resource_revision: ResourceRevision::INITIAL,
        stylesheet_revision: StylesheetRevision::INITIAL,
        style_environment: StyleEnvironment::default(),
        font_environment: FontEnvironmentRevision::INITIAL,
        image_metrics_revision: ImageMetricsRevision::INITIAL,
        layout: html::pipeline::LayoutConstraints {
            viewport_width: 600.0,
            viewport_height: None,
            line_height: 16.0,
            image_sizing_policy: html::pipeline::ImageSizingPolicy::WebCompatible,
            text_composition_policy: html::pipeline::TextCompositionPolicy::WebCompatible,
        },
        image_metrics: Default::default(),
        paint: PaintSettingsRevision::INITIAL,
    };
    let mut session = Engine::new(provider);
    let mut shaper = ProfileShaper::default();
    let started = Instant::now();
    session.update(inputs.clone(), &mut shaper).map_err(|error| error.to_string())?;
    totals.initial_update += started.elapsed();
    let started = Instant::now();
    session.update(inputs, &mut shaper).map_err(|error| error.to_string())?;
    totals.cached_update += started.elapsed();
    Ok(())
}

fn collect_epub_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_epub_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("epub")) {
            files.push(path);
        }
    }
}

fn print_timing(name: &str, duration: Duration, epubs: usize) {
    let per_epub = if epubs == 0 { 0.0 } else { duration.as_secs_f64() * 1_000.0 / epubs as f64 };
    println!("{name:<24} {:>10.3} ms total  {per_epub:>8.3} ms/EPUB", duration.as_secs_f64() * 1_000.0);
}
