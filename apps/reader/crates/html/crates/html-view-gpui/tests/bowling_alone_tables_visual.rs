use epub_provider::EpubProvider;
use gpui::{AppContext as _, Empty, HeadlessAppContext, PlatformTextSystem as _, px, size};
use gpui_wgpu::CosmicTextSystem;
use html_view_core::{RendererInitialConfig, ResourceProvider};
use html_view_gpui::HtmlView;
use image::RgbaImage;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const EPUB_FILE: &str = "Robert D. Putnam - Bowling Alone_ Revised and Updated_ The Collapse and Revival of American Community (2020, Simon & Schuster) - libgen.li.epub";
const VIEWPORT_WIDTH: u32 = 1_000;
const VIEWPORT_HEIGHT: u32 = 700;
const MAX_PAGES_PER_TABLE: usize = 80;

struct ExtractedTableProvider {
    epub: EpubProvider,
    documents: BTreeMap<String, Vec<u8>>,
}

impl ResourceProvider for ExtractedTableProvider {
    fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        if let Some(document) = self.documents.get(uri.trim_start_matches('/')) {
            return Ok(document.clone());
        }
        self.epub.read_bytes(uri)
    }

    fn exists(&self, uri: &str) -> bool {
        self.documents.contains_key(uri.trim_start_matches('/')) || self.epub.exists(uri)
    }

    fn resolve(&self, base: &str, href: &str) -> String {
        self.epub.resolve(base, href)
    }

    fn list_html_candidates(&self, _root: &str) -> io::Result<Vec<String>> {
        Ok(self.documents.keys().cloned().collect())
    }
}

#[test]
#[ignore = "writes human-inspectable screenshots from the local Bowling Alone EPUB"]
fn renders_bowling_alone_large_tables_across_pages() {
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    let epub_path = std::env::var_os("BOWLING_ALONE_EPUB").map(PathBuf::from).unwrap_or_else(|| repository_root.join(EPUB_FILE));
    assert!(epub_path.is_file(), "Bowling Alone EPUB not found at {} (set BOWLING_ALONE_EPUB to override)", epub_path.display());

    let epub = EpubProvider::try_new(epub_path).expect("open Bowling Alone EPUB");
    let mut documents = BTreeMap::new();
    add_tables(&epub, &mut documents, "text/part0039.html", "appendix-ii-sources");
    add_tables(&epub, &mut documents, "text/part0040.html", "appendix-iii-associations");
    assert_eq!(documents.len(), 4, "the two source chapters should contain four large tables");
    let document_uris = documents.keys().cloned().collect::<Vec<_>>();
    let provider: Arc<dyn ResourceProvider> = Arc::new(ExtractedTableProvider { epub, documents });

    let artifact_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../visual-artifacts/bowling-alone-tables");
    fs::create_dir_all(&artifact_root).expect("create Bowling Alone visual artifact directory");

    let text_system = Arc::new(CosmicTextSystem::new("IBM Plex Sans"));
    assert!(!text_system.all_font_names().is_empty(), "no system fonts available");
    let mut context = HeadlessAppContext::with_platform_and_scale_factor(text_system, Arc::new(()), gpui_platform::current_headless_renderer, 1.0);
    let window = context.open_window(size(px(VIEWPORT_WIDTH as f32), px(VIEWPORT_HEIGHT as f32)), |_, cx| cx.new(|_| Empty)).expect("open headless reader window");
    let window = window.into();

    for uri in document_uris {
        let view_provider = provider.clone();
        let view_uri = uri.clone();
        let view = context
            .update_window(window, |_, window, cx| {
                window.replace_root(cx, move |window, cx| {
                    let config = RendererInitialConfig { font_size: 16.0, column_width: VIEWPORT_WIDTH as f64, max_column_count: Some(1), ..RendererInitialConfig::default() };
                    HtmlView::from_provider(view_provider, vec![view_uri], 0, None, config, window, cx)
                })
            })
            .expect("replace Bowling Alone visual root");
        context.run_until_parked();

        let stem = Path::new(&uri).file_stem().expect("generated document has a stem").to_string_lossy();
        remove_previous_pages(&artifact_root, &stem);
        let mut previous: Option<RgbaImage> = None;
        let mut rendered_pages = 0usize;
        for page_number in 1..=MAX_PAGES_PER_TABLE {
            let screenshot = context.capture_screenshot(window).expect("capture Bowling Alone table screenshot");
            assert_eq!(screenshot.dimensions(), (VIEWPORT_WIDTH, VIEWPORT_HEIGHT));
            if previous.as_ref().is_some_and(|previous| previous == &screenshot) {
                break;
            }
            assert!(painted_pixel_count(&screenshot) > 1_000, "{uri} page {page_number} rendered blank");
            screenshot.save(artifact_root.join(format!("{stem}-page-{page_number:02}.png"))).expect("save Bowling Alone table screenshot");
            rendered_pages += 1;
            previous = Some(screenshot);

            context.update(|cx| view.update(cx, |view, cx| view.next_page(cx)));
            context.run_until_parked();
        }
        assert!(rendered_pages >= 2, "{uri} should span multiple reader pages");
        assert!(rendered_pages < MAX_PAGES_PER_TABLE, "{uri} did not reach a stable final page within {MAX_PAGES_PER_TABLE} pages");
    }
}

#[test]
#[ignore = "profiles repeated desktop viewport changes over a large local EPUB table"]
fn profiles_bowling_alone_desktop_resize() {
    let epub_path = std::env::var_os("BOWLING_ALONE_EPUB").map(PathBuf::from).expect("set BOWLING_ALONE_EPUB to the local Bowling Alone EPUB");
    let epub = EpubProvider::try_new(epub_path).expect("open Bowling Alone EPUB");
    let mut documents = BTreeMap::new();
    add_tables(&epub, &mut documents, "text/part0039.html", "appendix-ii-sources");
    let (uri, _) = documents.first_key_value().expect("fixture contains a large table");
    let uri = uri.clone();
    let provider: Arc<dyn ResourceProvider> = Arc::new(ExtractedTableProvider { epub, documents });

    let text_system = Arc::new(CosmicTextSystem::new("IBM Plex Sans"));
    let mut context = HeadlessAppContext::with_platform_and_scale_factor(text_system, Arc::new(()), gpui_platform::current_headless_renderer, 1.0);
    let window = context.open_window(size(px(VIEWPORT_WIDTH as f32), px(VIEWPORT_HEIGHT as f32)), |_, cx| cx.new(|_| Empty)).expect("open headless reader window");
    let window = window.into();
    context
        .update_window(window, |_, window, cx| {
            window.replace_root(cx, move |window, cx| {
                let config = RendererInitialConfig { font_size: 16.0, column_width: VIEWPORT_WIDTH as f64, max_column_count: Some(1), ..RendererInitialConfig::default() };
                HtmlView::from_provider(provider, vec![uri], 0, None, config, window, cx)
            })
        })
        .expect("install resize profiling document");
    context.run_until_parked();
    let _ = context.capture_screenshot(window).expect("render initial frame");

    for width in [980, 960, 940, 920, 900, 920, 940, 960, 980, 1_000] {
        context
            .update_window(window, |_, window, cx| {
                window.resize(size(px(width as f32), px(VIEWPORT_HEIGHT as f32)));
                window.bounds_changed(cx);
            })
            .expect("resize headless reader window");
        context.run_until_parked();
        let screenshot = context.capture_screenshot(window).expect("render resized frame");
        assert_eq!(screenshot.height(), VIEWPORT_HEIGHT);
        assert_eq!(screenshot.width(), width);
    }
}

fn add_tables(epub: &EpubProvider, documents: &mut BTreeMap<String, Vec<u8>>, chapter_uri: &str, name: &str) {
    let source = String::from_utf8(epub.read_bytes(chapter_uri).unwrap_or_else(|error| panic!("read {chapter_uri}: {error}"))).unwrap_or_else(|error| panic!("decode {chapter_uri}: {error}"));
    let tables = extract_tables(&source).into_iter().filter(|table| table.matches("<tr").count() >= 40).collect::<Vec<_>>();
    for (index, table) in tables.into_iter().enumerate() {
        let uri = format!("{name}-table-{}.xhtml", index + 1);
        let title = format!("Bowling Alone — {name} — table {}", index + 1);
        let document = format!(
            "<!doctype html><html xmlns='http://www.w3.org/1999/xhtml' xmlns:epub='http://www.idpf.org/2007/ops'><head><meta charset='utf-8'/><title>{title}</title><link rel='stylesheet' href='stylesheet.css'/><link rel='stylesheet' href='page_styles.css'/><style>html,body,table{{margin-top:0!important;padding-top:0!important}}</style></head><body class='calibre'>{table}</body></html>"
        );
        documents.insert(uri, document.into_bytes());
    }
}

fn extract_tables(source: &str) -> Vec<&str> {
    let mut tables = Vec::new();
    let mut cursor = 0usize;
    while let Some(relative_start) = source[cursor..].find("<table") {
        let start = cursor + relative_start;
        let Some(relative_end) = source[start..].find("</table>") else {
            panic!("table starting at byte {start} has no closing tag");
        };
        let end = start + relative_end + "</table>".len();
        tables.push(&source[start..end]);
        cursor = end;
    }
    tables
}

fn painted_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel.0[..3] != [255, 255, 255]).count()
}

fn remove_previous_pages(artifact_root: &Path, stem: &str) {
    let prefix = format!("{stem}-page-");
    for entry in fs::read_dir(artifact_root).expect("read Bowling Alone visual artifact directory") {
        let entry = entry.expect("read Bowling Alone visual artifact entry");
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&prefix) && entry.path().extension().is_some_and(|extension| extension == "png") {
            fs::remove_file(entry.path()).expect("remove stale Bowling Alone table screenshot");
        }
    }
}

#[test]
fn extracts_each_large_appendix_table_independently() {
    let source = "<html><body><table><tr><td>A</td></tr></table><p>between</p><table><tr><td>B</td></tr></table></body></html>";
    let tables = extract_tables(source);
    assert_eq!(tables, ["<table><tr><td>A</td></tr></table>", "<table><tr><td>B</td></tr></table>"]);
}
