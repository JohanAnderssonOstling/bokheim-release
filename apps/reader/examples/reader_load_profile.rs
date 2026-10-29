//! Measure the existing local EPUB opening path with native GPUI text shaping.
//!
//! cargo run --release -p reader-ui --example reader_load_profile -- BOOK.epub [REPETITIONS]
//!
//! Each repetition opens a fresh book and renderer at the start and at a middle
//! chapter. OS file/font caches remain warm. Timings exclude library/database
//! lookup, reader chrome, image completion, and compositor presentation.
//! Set BOKHEIM_PROFILE_TEXT=1 to time trailing-whitespace recovery probes.
//! Use --probe-whitespace instead of a book path to exercise native shaping
//! of spaces, tabs, line breaks, nonbreaking spaces, and mixed scripts.

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use book_access::AsyncBookSource;
use book_content::{Book, BookFormat};
use gpui::{App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, px, size};
use html::text::{FontSlant, GlyphResourceStore, GlyphShaper, OpenTypeFeature, TextRunShapeRequest, TextStyleSpan};
use html_gpui::backend::GpuiTextCache;
use skrift_column_paginator::{RendererCommand, RendererInitialConfig, RendererPreparation};
use html_view_gpui::HtmlView;

#[path = "../crates/html/src/book.rs"]
mod book;
use book::BookSource;

struct Sample {
    started: Instant,
    open_ms: f64,
    text_ms: f64,
    style_ms: f64,
    create_ms: f64,
    plan_ms: f64,
    chapters: usize,
    text_bytes: u64,
    chapter: usize,
    chapter_bytes: u64,
    repetition: usize,
    middle: bool,
}

struct Harness {
    document: Option<Entity<HtmlView>>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.document.clone())
    }
}

fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

async fn prepare(path: PathBuf, repetition: usize, middle: bool, runtime: tokio::runtime::Handle) -> Result<(Arc<BookSource>, RendererPreparation, Sample), String> {
    println!("READER_LOAD_PROFILE_BEGIN repetition={repetition} middle={middle}");
    let started = Instant::now();
    let source = {
        let _guard = runtime.enter();
        AsyncBookSource::from_file(File::open(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?
    };
    let book = Book::open(BookFormat::Epub, move || source.reader()).await.map_err(|e| e.to_string())?;
    let open_ms = milliseconds(started);
    let stage = Instant::now();
    let book = Arc::new(BookSource::new(book.prepare_reading().await.map_err(|e| e.to_string())?));
    let text_ms = milliseconds(stage);
    let chapters = book.document_uris().len();
    let chapter = if middle { chapters / 2 } else { 0 };
    let chapter_bytes = book.document_weights()[chapter];
    let text_bytes = book.document_weights().iter().sum();
    // Exercise saved-position selection, rather than preparing chapter zero
    // and navigating to a different chapter afterwards.
    let nav_state = format!("{chapter}:0");
    let stage = Instant::now();
    let preparation = RendererPreparation::from_provider_with_nav_and_target(
        book.provider.clone(),
        book.document_uris().to_vec(),
        0,
        Some(&nav_state),
        None,
        RendererInitialConfig { font_size: 19.0, column_width: 600.0, max_column_count: Some(1), ..Default::default() },
    )?;
    let sample = Sample { started, open_ms, text_ms, style_ms: milliseconds(stage), create_ms: 0.0, plan_ms: 0.0, chapters, text_bytes, chapter, chapter_bytes, repetition, middle };
    Ok((book, preparation, sample))
}

fn observe_frame(document: Entity<HtmlView>, sample: Sample, done: async_channel::Sender<()>, window: &mut Window) {
    window.on_next_frame(move |window, cx| {
        // A frame callback can precede the first paint. Wait until the actual
        // HtmlView has composed a display list before reporting completion.
        if document.read(cx).paint_cache_stats().base_misses == 0 {
            observe_frame(document, sample, done, window);
            return;
        }
        let shapes = document.read(cx).text_shape_cache_stats();
        println!(
            "READER_LOAD_PROFILE {}",
            serde_json::json!({
                "repetition": sample.repetition,
                "position": if sample.middle { "middle" } else { "start" },
                "chapters": sample.chapters, "chapter": sample.chapter,
                "spine_bytes": sample.text_bytes, "chapter_bytes": sample.chapter_bytes,
                "open_ms": sample.open_ms, "text_prepare_ms": sample.text_ms,
                "parse_style_ms": sample.style_ms, "renderer_create_ms": sample.create_ms,
                "viewport_plan_ms": sample.plan_ms,
                "frame_observed_ms": milliseconds(sample.started),
                "glyph_misses": shapes.glyph_misses, "line_misses": shapes.line_misses,
                "font_setup_hits": shapes.font_setup_hits, "font_setup_misses": shapes.font_setup_misses, "retained_font_setups": shapes.retained_font_setups,
                "registered_glyphs": shapes.registered_glyphs, "backend_glyph_records": shapes.backend_glyph_records,
                "trailing_whitespace_probes": shapes.trailing_whitespace_probes,
                "trailing_whitespace_probe_characters": shapes.trailing_whitespace_probe_characters,
                "trailing_whitespace_probe_ms": shapes.trailing_whitespace_probe_time.as_secs_f64() * 1000.0,
            })
        );
        let _ = done.try_send(());
    });
}

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next().expect("usage: reader_load_profile BOOK.epub [REPETITIONS] | --probe-whitespace");
    if first == "--probe-whitespace" {
        assert!(arguments.next().is_none(), "unexpected argument");
        profile_whitespace();
        return;
    }
    let path = PathBuf::from(first);
    let repetitions: usize = arguments.next().map(|n| n.to_string_lossy().parse().expect("positive repetition count")).unwrap_or(5);
    assert!(repetitions > 0);
    assert!(arguments.next().is_none(), "unexpected argument");
    let runtime = tokio::runtime::Runtime::new().expect("I/O runtime");
    // AsyncBookSource pins this runtime's handle before its futures move to
    // GPUI's executor, just as they do in the application.
    let _runtime_guard = runtime.enter();
    let runtime_handle = runtime.handle().clone();
    gpui_platform::application().with_assets(ui_components::Assets).run(move |cx: &mut App| {
        ui_components::register_fonts(cx).expect("reader fonts");
        let bounds = gpui::Bounds::centered(None, size(px(800.0), px(800.0)), cx);
        let window = cx.open_window(gpui::WindowOptions { titlebar: None, window_bounds: Some(gpui::WindowBounds::Windowed(bounds)), ..Default::default() }, |_, cx| cx.new(|_| Harness { document: None })).expect("profile window");
        cx.activate(true);
        cx.spawn(async move |cx| {
            // Let platform/window initialization complete outside the sample.
            cx.background_executor().timer(Duration::from_millis(300)).await;
            for middle in [false, true] {
                for repetition in 0..repetitions {
                    let path = path.clone();
                    let handle = runtime_handle.clone();
                    let prepared = cx.background_executor().spawn(async move { prepare(path, repetition, middle, handle).await }).await;
                    let (book, preparation, mut sample) = prepared.expect("prepare EPUB");
                    let (done, complete) = async_channel::bounded(1);
                    window
                        .update(cx, |harness, window, cx| {
                            let executor = cx.background_executor().clone();
                            let preparation = preparation.with_image_loader(book.image_loader(move |task| executor.spawn(task).detach()));
                            let stage = Instant::now();
                            let document = HtmlView::create_from_preparation(preparation, window, cx).expect("native renderer");
                            sample.create_ms = milliseconds(stage);
                            let stage = Instant::now();
                            document.update(cx, |view, cx| view.apply(RendererCommand::SetColumnViewportPlan { available_width: 800.0, column_width: 600.0, column_count: 1, column_gap: 0.0 }, cx));
                            sample.plan_ms = milliseconds(stage);
                            harness.document = Some(document.clone());
                            observe_frame(document, sample, done, window);
                            cx.notify();
                        })
                        .expect("install document");
                    complete.recv().await.expect("observed frame");
                    // Let pending images settle before measuring another open.
                    cx.background_executor().timer(Duration::from_millis(300)).await;
                }
            }
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

fn profile_whitespace() {
    gpui_platform::application().with_assets(ui_components::Assets).run(|cx: &mut App| {
        ui_components::register_fonts(cx).expect("reader fonts");
        cx.open_window(Default::default(), |window, cx| {
            let mut shaper = GpuiTextCache::new(window.text_system().clone());
            let mut resources = GlyphResourceStore::default();
            let no_features = [];
            let disabled_features = [OpenTypeFeature::new(*b"liga", 0), OpenTypeFeature::new(*b"kern", 0)];
            let overridden_features = [OpenTypeFeature::new(*b"liga", 0), OpenTypeFeature::new(*b"liga", 1)];
            for (font_size, weight, slant, features) in [
                (19.0, 400, FontSlant::Normal, no_features.as_slice()),
                (26.0, 700, FontSlant::Italic, disabled_features.as_slice()),
                (13.0, 400, FontSlant::Oblique, overridden_features.as_slice()),
                (0.0, 400, FontSlant::Normal, no_features.as_slice()),
            ] {
                for family in ["serif", "sans-serif", "monospace"] {
                    for text in [" ", "   ", "word ", "word  ", "office ", "a\u{301} ", "אבג ", "سلام ", "word\u{a0}", "word\u{2003}", "word\u{2009}", "word\u{3000}", "word\t", "word\n", "word\u{2028}"] {
                        let before = shaper.shape_cache_stats();
                        let style = TextStyleSpan::new(0..text.len(), font_size, weight, slant, 0x000000ff, Some(family)).unwrap().with_font_features(features);
                        let request = TextRunShapeRequest::new(text, style).unwrap();
                        let mut glyphs = vec![0; text.chars().count()];
                        let shaped = shaper.shape_glyph_run(request, &mut glyphs, &mut resources).expect("native whitespace shaping").expect("native run");
                        let after = shaper.shape_cache_stats();
                        let metrics = glyphs
                            .iter()
                            .map(|glyph| {
                                let metric = resources.get(*glyph).expect("registered metric");
                                (metric.ch(), metric.advance(), metric.ascent(), metric.descent())
                            })
                            .collect::<Vec<_>>();
                        println!(
                            "READER_WHITESPACE_PROFILE {}",
                            serde_json::json!({
                                "family": family, "text": text, "width": shaped.advances().iter().sum::<f32>(),
                                "font_size": font_size, "weight": weight, "slant": format!("{slant:?}"),
                                "features": features.iter().map(|feature| (feature.tag(), feature.value())).collect::<Vec<_>>(),
                                "advances": shaped.advances(), "cluster_boundaries": shaped.cluster_boundaries(), "caret_stops": shaped.caret_stops(), "metrics": metrics,
                                "last_advance": shaped.advances().last(),
                                "probes": after.trailing_whitespace_probes - before.trailing_whitespace_probes,
                                "probe_ms": (after.trailing_whitespace_probe_time - before.trailing_whitespace_probe_time).as_secs_f64() * 1000.0,
                            })
                        );
                    }
                }
            }
            window.on_next_frame(|_, cx| cx.quit());
            cx.new(|_| Harness { document: None })
        })
        .expect("whitespace profile window");
    });
}
