use gpui::{AppContext as _, Empty, HeadlessAppContext, PlatformTextSystem as _, px, size};
use gpui_wgpu::CosmicTextSystem;
use html::testing::{QuirksMode, ReftestFuzzyTolerance, ReftestRelation, document_quirks_mode, document_requires_http, extract_reftest_fuzzy_tolerances, extract_reftest_references, wpt_root};
use html_view_core::{ImageSizingPolicy, RendererCommand, ResourceProvider, TextCompositionPolicy};
use html_view_gpui::HtmlView;
use image::RgbaImage;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TESTS: &str = include_str!("wpt/gpui-reftests.txt");
const KNOWN_FAILURES: &str = include_str!("wpt/known-gpui-reftest-failures.txt");
const UNSUPPORTED_QUIRKS_MODE: &str = include_str!("wpt/unsupported-quirks-mode-gpui-reftests.txt");
const UNSUPPORTED_HTTP: &str = include_str!("wpt/unsupported-http-gpui-reftests.txt");
const KNOWN_FAILURE_CAUSES: &str = include_str!("wpt/known-gpui-reftest-causes.txt");
const KNOWN_TIMEOUTS: &str = include_str!("wpt/known-gpui-reftest-timeouts.txt");
const VIEWPORT_WIDTH: u32 = 800;
const VIEWPORT_HEIGHT: u32 = 600;
const MAX_REFERENCE_CACHE_ENTRIES: usize = 32;
const HARNESS_REFRESH_INTERVAL: usize = 250;
const RESOURCE_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
const RESOURCE_REPAINT_INTERVAL: Duration = Duration::from_millis(16);

struct WptResourceProvider {
    root: PathBuf,
}

impl ResourceProvider for WptResourceProvider {
    fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        fs::read(self.root.join(uri.trim_start_matches('/')))
    }

    fn exists(&self, uri: &str) -> bool {
        self.root.join(uri.trim_start_matches('/')).is_file()
    }

    fn resolve(&self, base: &str, href: &str) -> String {
        if href.contains("://") {
            return href.to_owned();
        }
        let href = href.split(['#', '?']).next().unwrap_or(href);
        let base = Path::new(base);
        let base_dir = if base.as_os_str().to_string_lossy().ends_with('/') { base } else { base.parent().unwrap_or_else(|| Path::new("")) };
        let path = if href.starts_with('/') { PathBuf::from(href.trim_start_matches('/')) } else { base_dir.join(href) };
        normalize_relative_path(&path)
    }

    fn list_html_candidates(&self, _root: &str) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ImageDifference {
    different_pixels: usize,
    maximum_channel_delta: u8,
}

#[test]
fn runs_pinned_static_wpt_reftests_through_gpui() {
    let wpt_root = wpt_root();
    let provider: Arc<dyn ResourceProvider> = Arc::new(WptResourceProvider { root: wpt_root.clone() });
    let text_system = Arc::new(CosmicTextSystem::new("IBM Plex Sans"));
    // Ahem is WPT's deterministic geometry font: its glyphs deliberately map
    // onto exact em-square shapes. `@font-face` loading is outside this static
    // host adapter today, so register the pinned WPT font with the backend
    // instead of silently substituting a system font and misdiagnosing layout.
    let ahem = fs::read(wpt_root.join("fonts/Ahem.ttf")).expect("read pinned WPT Ahem font");
    text_system.add_fonts(vec![Cow::Owned(ahem)]).expect("register pinned WPT Ahem font");
    // Force font database initialization before the render loop so failures
    // are infrastructure failures rather than per-reference mismatches.
    assert!(!text_system.all_font_names().is_empty(), "no system fonts available");
    assert!(text_system.all_font_names().iter().any(|family| family.eq_ignore_ascii_case("Ahem")), "pinned WPT Ahem font was not registered");
    let (mut context, mut window) = create_harness(text_system.clone());

    let all_tests = manifest_entries();
    assert_eq!(all_tests.len(), manifest_expected_count(), "the pinned GPUI reftest selection changed");
    let manifest_set = all_tests.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(manifest_set.len(), all_tests.len(), "duplicate entries in GPUI WPT manifest");
    let all_known = known_failures();
    let orphaned_known = all_known.difference(&manifest_set).cloned().collect::<Vec<_>>();
    assert!(orphaned_known.is_empty(), "GPUI WPT failure entries outside the manifest:\n{}", orphaned_known.join("\n"));
    let all_causes = known_failure_causes();
    assert_failure_causes_are_consistent(&manifest_set, &all_known, &all_causes);
    let all_timeouts = known_timeouts();
    let orphaned_timeouts = all_timeouts.difference(&manifest_set).cloned().collect::<Vec<_>>();
    assert!(orphaned_timeouts.is_empty(), "GPUI WPT timeout entries outside the manifest:\n{}", orphaned_timeouts.join("\n"));
    let tests = selected_shard(all_tests);
    let selected = tests.iter().cloned().collect::<BTreeSet<_>>();
    let unsupported_quirks = unsupported_quirks_mode();
    assert!(unsupported_quirks.iter().all(|test| manifest_set.contains(test)), "quirks-dependent GPUI WPT entries outside the manifest");
    let unsupported_http = unsupported_http();
    assert!(unsupported_http.iter().all(|test| manifest_set.contains(test)), "HTTP-dependent GPUI WPT entries outside the manifest");
    let mut known = all_known.into_iter().filter(|test| selected.contains(test)).collect::<BTreeSet<_>>();
    let known_count = known.len();
    let mut reference_cache = HashMap::<String, RgbaImage>::new();
    let mut observed_failures = Vec::new();
    let mut observed_panics = 0usize;
    let mut unexpected_failures = Vec::new();
    let mut unexpected_passes = Vec::new();
    let mut observed_timeouts = 0usize;
    let mut observed_unsupported_quirks = 0usize;
    let mut observed_unsupported_http = 0usize;

    let update_path = std::env::var("HTML_WPT_UPDATE_GPUI_FAILURES").ok();
    let progress_interval = std::env::var("HTML_WPT_GPUI_PROGRESS_INTERVAL").ok().map(|value| value.parse::<usize>().expect("invalid GPUI WPT progress interval")).unwrap_or(100).max(1);

    for (offset, test) in tests.iter().enumerate() {
        if unsupported_quirks.contains(test) {
            observed_unsupported_quirks += 1;
            known.remove(test);
            continue;
        }
        if unsupported_http.contains(test) {
            observed_unsupported_http += 1;
            continue;
        }
        if all_timeouts.contains(test) {
            observed_timeouts += 1;
            observed_failures.push(test.clone());
            if !known.remove(test) {
                unexpected_failures.push(format!("{test}|known-timeout"));
            }
            continue;
        }
        if offset > 0 && offset % HARNESS_REFRESH_INTERVAL == 0 {
            reference_cache.clear();
            (context, window) = create_harness(text_system.clone());
        }
        if offset % progress_interval == 0 {
            eprintln!("GPUI WPT progress: {offset}/{} ({test})", tests.len());
        }
        let evaluation = catch_unwind(AssertUnwindSafe(|| evaluate_reftest(&wpt_root, provider.clone(), &mut context, window, &mut reference_cache, test)));
        let (passed, closest) = match evaluation {
            Ok(result) => result,
            Err(payload) => {
                observed_panics += 1;
                observed_failures.push(test.clone());
                if !known.remove(test) {
                    unexpected_failures.push(format!("{test}|panic={}", panic_message(payload)));
                }
                (context, window) = create_harness(text_system.clone());
                continue;
            }
        };
        if passed {
            if known.remove(test) {
                let cause = all_causes.get(test).map(|cause| format!("|previous-cause={cause}")).unwrap_or_default();
                unexpected_passes.push(format!("{test}{cause}"));
            }
        } else {
            observed_failures.push(test.clone());
            if !known.remove(test) {
                unexpected_failures.push(format!("{test}|different-pixels={}|maximum-channel-delta={}", closest.different_pixels, closest.maximum_channel_delta));
            }
        }
    }

    eprintln!(
        "GPUI WPT reftests: {} tests, {} unsupported quirks, {} unsupported HTTP, {} panics, {} known timeouts, {} known failures, {} unexpected failures, {} unexpected passes",
        tests.len(),
        observed_unsupported_quirks,
        observed_unsupported_http,
        observed_panics,
        observed_timeouts,
        known_count - known.len() - observed_unsupported_quirks,
        unexpected_failures.len(),
        unexpected_passes.len()
    );
    if let Some(path) = update_path {
        let mut ledger = String::from("# Exact test paths whose current GPUI-rendered output does not yet satisfy its references.\n# Entries are reconciled: a newly passing test must be removed.\n");
        for failure in observed_failures {
            ledger.push_str(&failure);
            ledger.push('\n');
        }
        fs::write(&path, ledger).unwrap_or_else(|error| panic!("failed to write GPUI WPT ledger {path}: {error}"));
        eprintln!("wrote GPUI WPT failure ledger to {path}");
        return;
    }
    assert!(known.is_empty(), "stale GPUI WPT failure entries:\n{}", known.into_iter().collect::<Vec<_>>().join("\n"));
    assert!(unexpected_failures.is_empty(), "unexpected GPUI WPT failures:\n{}", unexpected_failures.join("\n"));
    assert!(unexpected_passes.is_empty(), "known GPUI WPT failures now pass:\n{}", unexpected_passes.join("\n"));
}

fn create_harness(text_system: Arc<CosmicTextSystem>) -> (HeadlessAppContext, gpui::AnyWindowHandle) {
    let mut context = HeadlessAppContext::with_platform_and_scale_factor(text_system, Arc::new(()), gpui_platform::current_headless_renderer, 1.0);
    let window = context.open_window(size(px(VIEWPORT_WIDTH as f32), px(VIEWPORT_HEIGHT as f32)), |_, cx| cx.new(|_| Empty)).expect("open persistent GPUI reftest window");
    (context, window.into())
}

fn evaluate_reftest(wpt_root: &Path, provider: Arc<dyn ResourceProvider>, context: &mut HeadlessAppContext, window: gpui::AnyWindowHandle, reference_cache: &mut HashMap<String, RgbaImage>, test: &str) -> (bool, ImageDifference) {
    let source_bytes = fs::read(wpt_root.join(test)).unwrap_or_else(|error| panic!("failed to read WPT {test}: {error}"));
    let source = String::from_utf8_lossy(&source_bytes);
    let references = extract_reftest_references(&source);
    let fuzzy_tolerances = extract_reftest_fuzzy_tolerances(&source);
    assert!(!references.is_empty(), "WPT has no match/mismatch reference: {test}");
    let actual = render_page(context, window, provider.clone(), test);
    let mut match_seen = false;
    let mut match_satisfied = false;
    let mut mismatches_satisfied = true;
    let mut closest = ImageDifference { different_pixels: usize::MAX, maximum_channel_delta: u8::MAX };

    for reference in references {
        let reference_uri = provider.resolve(test, &reference.href);
        if reference_cache.len() >= MAX_REFERENCE_CACHE_ENTRIES && !reference_cache.contains_key(&reference_uri) {
            reference_cache.clear();
        }
        let expected = reference_cache.entry(reference_uri.clone()).or_insert_with(|| render_page(context, window, provider.clone(), &reference_uri));
        let difference = image_difference(&actual, expected);
        let tolerance = fuzzy_tolerance_for_reference(&fuzzy_tolerances, provider.as_ref(), test, &reference_uri);
        let images_match = difference_within_tolerance(difference, tolerance);
        if !images_match {
            write_failure_artifacts(test, &reference_uri, &actual, expected);
        }
        if difference.different_pixels < closest.different_pixels {
            closest = difference;
        }
        match reference.relation {
            ReftestRelation::Match => {
                match_seen = true;
                match_satisfied |= images_match;
            }
            ReftestRelation::Mismatch => {
                mismatches_satisfied &= !images_match;
            }
        }
    }

    ((!match_seen || match_satisfied) && mismatches_satisfied, closest)
}

fn write_failure_artifacts(test: &str, reference: &str, actual: &RgbaImage, expected: &RgbaImage) {
    let Some(directory) = std::env::var_os("HTML_WPT_GPUI_ARTIFACT_DIR") else { return };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory).expect("create GPUI WPT artifact directory");
    let filename = |uri: &str| uri.replace(['/', '\\'], "__");
    actual.save(directory.join(format!("{}__actual.png", filename(test)))).expect("write GPUI WPT actual artifact");
    expected.save(directory.join(format!("{}__reference.png", filename(reference)))).expect("write GPUI WPT reference artifact");
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload.downcast_ref::<String>().map(String::as_str).or_else(|| payload.downcast_ref::<&str>().copied()).unwrap_or("non-string panic").replace(['\n', '\r'], " ")
}

fn render_page(context: &mut HeadlessAppContext, window: gpui::AnyWindowHandle, provider: Arc<dyn ResourceProvider>, uri: &str) -> RgbaImage {
    let source = provider.read_bytes(uri).unwrap_or_else(|error| panic!("failed to read GPUI reftest document {uri}: {error}"));
    let source = String::from_utf8_lossy(&source).to_ascii_lowercase();
    assert!(!source.contains("<script"), "interactive WPT document in static GPUI reftest closure: {uri}");
    assert!(!source.contains("reftest-wait"), "reftest-wait document in static GPUI reftest closure: {uri}");

    let uri = uri.to_owned();
    let timeout_uri = uri.clone();
    let view = context
        .update_window(window, |_, window, cx| {
            window.replace_root(cx, move |window, cx| {
                let mut view = HtmlView::from_provider(provider, vec![uri], 0, None, Default::default(), window, cx);
                // WPT reftests assume the CSS initial `medium` font size of
                // 16px. The reader UI defaults to 20px, which is appropriate
                // for books but changes `em`-based UA margins and can make two
                // otherwise equivalent reference constructions diverge.
                view.apply(RendererCommand::SetFontSize(16.0), cx);
                view.apply(RendererCommand::SetColumnWidth(VIEWPORT_WIDTH as f64), cx);
                view.apply(RendererCommand::SetMaxColumnCount(Some(1)), cx);
                // WPTs exercise browser layout. Do not apply the reader UI's
                // standalone-image expansion policy to image-backed oracles.
                view.apply(RendererCommand::SetImageSizingPolicy(ImageSizingPolicy::WebCompatible), cx);
                view.apply(RendererCommand::SetTextCompositionPolicy(TextCompositionPolicy::WebCompatible), cx);
                view
            })
        })
        .expect("replace GPUI reftest root");
    context.run_until_parked();

    // The first frame discovers visible images and starts their asynchronous
    // load/decode work. Keep rendering while that work is pending so a
    // reference screenshot cannot accidentally capture image placeholders.
    let deadline = Instant::now() + RESOURCE_SETTLE_TIMEOUT;
    let mut image = context.capture_screenshot(window).expect("capture initial GPUI reftest screenshot");
    while context.update(|cx| view.read(cx).has_pending_resources()) {
        assert!(Instant::now() < deadline, "timed out waiting for GPUI reftest resources: {timeout_uri}");
        context.advance_clock(RESOURCE_REPAINT_INTERVAL);
        context.run_until_parked();
        std::thread::yield_now();
        image = context.capture_screenshot(window).expect("capture settled GPUI reftest screenshot");
    }
    assert_eq!(image.dimensions(), (VIEWPORT_WIDTH, VIEWPORT_HEIGHT), "GPUI reftest device scale must remain 1x");
    image
}

fn image_difference(actual: &RgbaImage, expected: &RgbaImage) -> ImageDifference {
    if actual.dimensions() != expected.dimensions() {
        return ImageDifference { different_pixels: usize::MAX, maximum_channel_delta: u8::MAX };
    }
    let mut result = ImageDifference::default();
    for (actual, expected) in actual.pixels().zip(expected.pixels()) {
        let mut differs = false;
        for channel in 0..4 {
            let delta = actual[channel].abs_diff(expected[channel]);
            differs |= delta != 0;
            result.maximum_channel_delta = result.maximum_channel_delta.max(delta);
        }
        result.different_pixels += usize::from(differs);
    }
    result
}

fn fuzzy_tolerance_for_reference<'a>(tolerances: &'a [ReftestFuzzyTolerance], provider: &dyn ResourceProvider, test: &str, reference_uri: &str) -> Option<&'a ReftestFuzzyTolerance> {
    tolerances.iter().find(|tolerance| tolerance.reference.as_deref().is_some_and(|reference| provider.resolve(test, reference) == reference_uri)).or_else(|| tolerances.iter().find(|tolerance| tolerance.reference.is_none()))
}

fn difference_within_tolerance(difference: ImageDifference, tolerance: Option<&ReftestFuzzyTolerance>) -> bool {
    let maximum_channel_delta = tolerance.map_or(0, |value| value.maximum_channel_delta);
    let maximum_different_pixels = tolerance.map_or(0, |value| value.maximum_different_pixels);
    difference.maximum_channel_delta <= maximum_channel_delta && difference.different_pixels <= maximum_different_pixels
}

fn manifest_entries() -> Vec<String> {
    TESTS.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_owned).collect()
}

fn manifest_expected_count() -> usize {
    TESTS.lines().find_map(|line| line.trim().strip_prefix("# count:").map(str::trim)).expect("GPUI WPT manifest has no count").parse().expect("invalid GPUI WPT manifest count")
}

fn selected_shard(mut tests: Vec<String>) -> Vec<String> {
    if let Ok(filter) = std::env::var("HTML_WPT_GPUI_FILTER") {
        tests.retain(|test| test.contains(&filter));
        assert!(!tests.is_empty(), "HTML_WPT_GPUI_FILTER matched no manifest entries");
    }
    if let Ok(shard) = std::env::var("HTML_WPT_GPUI_SHARD") {
        let (index, total) = shard.split_once('/').expect("HTML_WPT_GPUI_SHARD must be INDEX/TOTAL");
        let index = index.parse::<usize>().expect("invalid GPUI WPT shard index");
        let total = total.parse::<usize>().expect("invalid GPUI WPT shard total");
        assert!(index >= 1 && index <= total, "GPUI WPT shard index must be in 1..=TOTAL");
        let start = tests.len() * (index - 1) / total;
        let end = tests.len() * index / total;
        tests = tests[start..end].to_vec();
    }
    if let Ok(range) = std::env::var("HTML_WPT_GPUI_RANGE") {
        let (start, end) = range.split_once("..").expect("HTML_WPT_GPUI_RANGE must be START..END");
        let start = start.parse::<usize>().expect("invalid GPUI WPT range start");
        let end = end.parse::<usize>().expect("invalid GPUI WPT range end");
        assert!(start <= end && end <= tests.len(), "GPUI WPT range must be within the selected shard");
        tests = tests[start..end].to_vec();
    }
    tests
}

fn known_failures() -> BTreeSet<String> {
    KNOWN_FAILURES.lines().chain(UNSUPPORTED_QUIRKS_MODE.lines()).map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_owned).collect()
}

fn unsupported_quirks_mode() -> BTreeSet<String> {
    UNSUPPORTED_QUIRKS_MODE.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_owned).collect()
}

fn unsupported_http() -> BTreeSet<String> {
    UNSUPPORTED_HTTP.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_owned).collect()
}

fn known_failure_causes() -> BTreeMap<String, String> {
    let mut causes = BTreeMap::new();
    let failures = known_failures();
    for (line_index, line) in KNOWN_FAILURE_CAUSES.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.splitn(3, '|').map(str::trim);
        let test = fields.next().unwrap_or_default();
        let code = fields.next().unwrap_or_default();
        let explanation = fields.next().unwrap_or_default();
        assert!(!test.is_empty() && !code.is_empty() && !explanation.is_empty(), "invalid GPUI WPT cause entry on line {}: expected TEST | CATEGORY:FEATURE | EXPLANATION", line_index + 1);
        let (category, feature) = code.split_once(':').unwrap_or_else(|| panic!("invalid GPUI WPT cause code on line {}: {code}", line_index + 1));
        assert!(matches!(category, "unsupported" | "dependency" | "bug" | "infrastructure" | "needs-investigation"), "unknown GPUI WPT cause category on line {}: {category}", line_index + 1);
        assert!(!feature.is_empty() && feature.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'), "invalid GPUI WPT cause feature on line {}: {feature}", line_index + 1);
        if let Some(prefix) = test.strip_suffix("/**") {
            let prefix = format!("{}/", prefix.trim_end_matches('/'));
            let matches = failures.iter().filter(|failure| failure.starts_with(&prefix)).collect::<Vec<_>>();
            assert!(!matches.is_empty(), "GPUI WPT cause prefix on line {} matches no tracked failures: {test}", line_index + 1);
            for failure in matches {
                assert!(causes.insert(failure.clone(), code.to_owned()).is_none(), "duplicate GPUI WPT cause entry: {failure}");
            }
        } else {
            assert!(causes.insert(test.to_owned(), code.to_owned()).is_none(), "duplicate GPUI WPT cause entry: {test}");
        }
    }
    for test in unsupported_quirks_mode() {
        causes.insert(test, "unsupported:quirks-mode".to_owned());
    }
    causes
}

fn assert_failure_causes_are_consistent(manifest: &BTreeSet<String>, known_failures: &BTreeSet<String>, causes: &BTreeMap<String, String>) {
    let outside_manifest = causes.keys().filter(|test| !manifest.contains(*test)).cloned().collect::<Vec<_>>();
    assert!(outside_manifest.is_empty(), "GPUI WPT cause entries outside the manifest:\n{}", outside_manifest.join("\n"));
    let without_failure = causes.keys().filter(|test| !known_failures.contains(*test)).cloned().collect::<Vec<_>>();
    assert!(without_failure.is_empty(), "GPUI WPT cause entries whose tests no longer fail:\n{}", without_failure.join("\n"));
}

#[test]
fn known_failure_cause_registry_is_well_formed() {
    let manifest = manifest_entries().into_iter().collect::<BTreeSet<_>>();
    let failures = known_failures();
    let causes = known_failure_causes();
    assert_failure_causes_are_consistent(&manifest, &failures, &causes);
}

#[test]
fn quirks_mode_reftests_are_explicitly_unsupported() {
    let wpt_root = wpt_root();
    let provider = WptResourceProvider { root: wpt_root.clone() };
    let known = known_failures();
    let causes = known_failure_causes();
    let mut quirks_dependent = BTreeSet::new();

    for test in manifest_entries() {
        let source = fs::read(wpt_root.join(&test)).unwrap_or_else(|error| panic!("read WPT document {test}: {error}"));
        let source_text = String::from_utf8_lossy(&source);
        let references = extract_reftest_references(&source_text);
        let mut documents = vec![(test.clone(), source)];
        for reference in references {
            let reference = provider.resolve(&test, &reference.href);
            let bytes = fs::read(wpt_root.join(&reference)).unwrap_or_else(|error| panic!("read WPT reference {reference}: {error}"));
            documents.push((reference, bytes));
        }
        let depends_on_quirks = documents.iter().any(|(path, bytes)| matches!(Path::new(path).extension().and_then(|extension| extension.to_str()), Some("html" | "htm")) && document_quirks_mode(bytes) != QuirksMode::NoQuirks);
        if depends_on_quirks {
            quirks_dependent.insert(test);
        }
    }
    let unsupported = unsupported_quirks_mode();
    assert_eq!(unsupported, quirks_dependent, "the quirks-mode unsupported ledger must exactly match manifest entries whose test or reference depends on quirks mode");
    assert!(unsupported.iter().all(|test| known.contains(test) && causes.get(test).map(String::as_str) == Some("unsupported:quirks-mode")));
}

#[test]
fn http_dependent_reftests_are_explicitly_unsupported() {
    let wpt_root = wpt_root();
    let mut http_dependent = BTreeSet::new();
    for test in manifest_entries() {
        let bytes = fs::read(wpt_root.join(&test)).unwrap_or_else(|error| panic!("read WPT document {test}: {error}"));
        if document_requires_http(&bytes, &test).unwrap_or_else(|error| panic!("parse WPT document {test}: {error}")) {
            http_dependent.insert(test);
        }
    }
    assert_eq!(unsupported_http(), http_dependent, "the HTTP unsupported ledger must exactly match manifest entries carrying the WPT `http` flag");
}

fn known_timeouts() -> BTreeSet<String> {
    KNOWN_TIMEOUTS.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_owned).collect()
}

fn normalize_relative_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts.join("/")
}
