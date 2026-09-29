use gpui::{AppContext as _, Empty, HeadlessAppContext, PlatformTextSystem as _, px, size};
use gpui_wgpu::CosmicTextSystem;
use html_view_core::{RendererInitialConfig, ResourceProvider};
use html_view_gpui::HtmlView;
use image::RgbaImage;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const VIEWPORT_WIDTH: u32 = 800;
const VIEWPORT_HEIGHT: u32 = 600;

struct FixtureProvider {
    root: PathBuf,
}

impl ResourceProvider for FixtureProvider {
    fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        fs::read(self.root.join(uri.trim_start_matches('/')))
    }

    fn exists(&self, uri: &str) -> bool {
        self.root.join(uri.trim_start_matches('/')).is_file()
    }

    fn resolve(&self, base: &str, href: &str) -> String {
        if href.contains("://") || href.starts_with('#') {
            return href.to_owned();
        }
        let base = Path::new(base);
        let base_dir = if base.as_os_str().to_string_lossy().ends_with('/') { base } else { base.parent().unwrap_or_else(|| Path::new("")) };
        base_dir.join(href).to_string_lossy().replace('\\', "/")
    }

    fn list_html_candidates(&self, _root: &str) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

#[test]
#[ignore = "writes human-inspectable GPUI screenshots; run explicitly"]
fn renders_semantic_reader_pagination_artifacts() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/reader-pagination");
    let artifact_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../visual-artifacts/reader-pagination");
    fs::create_dir_all(&artifact_root).expect("create reader pagination visual artifact directory");

    let text_system = Arc::new(CosmicTextSystem::new("IBM Plex Sans"));
    assert!(!text_system.all_font_names().is_empty(), "no system fonts available");
    let mut context = HeadlessAppContext::with_platform_and_scale_factor(text_system, Arc::new(()), gpui_platform::current_headless_renderer, 1.0);
    let window = context.open_window(size(px(VIEWPORT_WIDTH as f32), px(VIEWPORT_HEIGHT as f32)), |_, cx| cx.new(|_| Empty)).expect("open headless reader window");
    let window = window.into();

    for (fixture, page_count) in
        [("forced-break.html", 2), ("avoid-inside.html", 2), ("heading-keep.html", 2), ("figure-placement.html", 2), ("table-placement.html", 2), ("table-row-groups.html", 3), ("widows-orphans.html", 2), ("popup-only-footnote.xhtml", 1)]
    {
        let provider: Arc<dyn ResourceProvider> = Arc::new(FixtureProvider { root: fixture_root.clone() });
        let uri = fixture.to_owned();
        let view = context
            .update_window(window, |_, window, cx| {
                window.replace_root(cx, move |window, cx| {
                    let config = RendererInitialConfig { font_size: 18.0, column_width: VIEWPORT_WIDTH as f64, max_column_count: Some(1), ..RendererInitialConfig::default() };
                    HtmlView::from_provider(provider, vec![uri], 0, None, config, window, cx)
                })
            })
            .expect("replace reader visual root");
        context.run_until_parked();

        let stem = Path::new(fixture).file_stem().unwrap().to_string_lossy();
        let mut pages = Vec::new();
        for page_number in 1..=page_count {
            let screenshot = context.capture_screenshot(window).expect("capture reader pagination screenshot");
            assert_eq!(screenshot.dimensions(), (VIEWPORT_WIDTH, VIEWPORT_HEIGHT));
            assert!(painted_pixel_count(&screenshot) > 1_000, "{fixture} page {page_number} rendered blank");
            screenshot.save(artifact_root.join(format!("{stem}-page-{page_number}.png"))).expect("save reader pagination screenshot");
            pages.push(screenshot);

            if page_number < page_count {
                context.update(|cx| view.update(cx, |view, cx| view.next_page(cx)));
                context.run_until_parked();
            }
        }

        if pages.len() == 2 {
            assert!(different_pixel_count(&pages[0], &pages[1]) > 10_000, "{fixture} pages unexpectedly look identical");
        }
        match fixture {
            "forced-break.html" => {
                assert_eq!(purple_pixel_count(&pages[0]), 0, "forced chapter panel leaked onto page 1");
                assert!(purple_pixel_count(&pages[1]) > 1_000, "forced chapter panel is missing from page 2");
            }
            "avoid-inside.html" => {
                assert_eq!(green_pixel_count(&pages[0]), 0, "avoid-inside card leaked onto page 1");
                assert!(green_pixel_count(&pages[1]) > 1_000, "avoid-inside card is missing from page 2");
            }
            "heading-keep.html" => {
                assert_eq!(blue_pixel_count(&pages[0]), 0, "automatically kept heading leaked onto page 1");
                assert!(blue_pixel_count(&pages[1]) > 1_000, "automatically kept heading is missing from page 2");
            }
            "figure-placement.html" => {
                assert_eq!(orange_pixel_count(&pages[0]), 0, "automatically kept figure leaked onto page 1");
                assert!(orange_pixel_count(&pages[1]) > 1_000, "automatically kept figure is missing from page 2");
            }
            "table-placement.html" => {
                assert_eq!(cyan_pixel_count(&pages[0]), 0, "automatically kept table leaked onto page 1");
                assert!(cyan_pixel_count(&pages[1]) > 1_000, "automatically kept table is missing from page 2");
            }
            "table-row-groups.html" => {
                assert_eq!(cyan_pixel_count(&pages[0]), 0, "oversized table leaked onto page 1 instead of starting fresh");
                assert!(green_row_pixel_count(&pages[1]) > 10_000, "the first atomic row is missing from page 2");
                assert_eq!(lavender_row_pixel_count(&pages[1]), 0, "the rowspan group leaked onto page 2");
                assert!(lavender_row_pixel_count(&pages[2]) > 20_000, "the complete rowspan group is missing from page 3");
            }
            "widows-orphans.html" => {
                assert_eq!(yellow_pixel_count(&pages[0]), 0, "widow/orphan-protected paragraph left a fragment on page 1");
                assert!(yellow_pixel_count(&pages[1]) > 1_000, "widow/orphan-protected paragraph is missing from page 2");
            }
            "popup-only-footnote.xhtml" => {
                assert_eq!(magenta_leak_pixel_count(&pages[0]), 0, "hidden footnote body painted into the reading page");
            }
            _ => unreachable!(),
        }
    }
}

fn painted_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel.0[..3] != [255, 255, 255]).count()
}

fn different_pixel_count(left: &RgbaImage, right: &RgbaImage) -> usize {
    left.pixels().zip(right.pixels()).filter(|(left, right)| left != right).count()
}

fn magenta_leak_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] > 230 && pixel[1] < 40 && pixel[2] > 80).count()
}

fn purple_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] > 100 && pixel[0] < 150 && pixel[1] > 60 && pixel[1] < 110 && pixel[2] > 180).count()
}

fn green_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] < 40 && pixel[1] > 100 && pixel[1] < 160 && pixel[2] > 70 && pixel[2] < 140).count()
}

fn yellow_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] > 245 && pixel[1] > 220 && pixel[1] < 250 && pixel[2] > 170 && pixel[2] < 220).count()
}

fn blue_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] > 200 && pixel[0] < 235 && pixel[1] > 225 && pixel[1] < 250 && pixel[2] > 245).count()
}

fn orange_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] > 245 && pixel[1] > 210 && pixel[1] < 240 && pixel[2] > 175 && pixel[2] < 215).count()
}

fn cyan_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] > 205 && pixel[0] < 235 && pixel[1] > 235 && pixel[2] > 235).count()
}

fn green_row_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] > 215 && pixel[0] < 235 && pixel[1] > 235 && pixel[1] < 250 && pixel[2] > 215 && pixel[2] < 235).count()
}

fn lavender_row_pixel_count(image: &RgbaImage) -> usize {
    image.pixels().filter(|pixel| pixel[0] > 225 && pixel[1] > 215 && pixel[1] < 240 && pixel[2] > 245).count()
}
