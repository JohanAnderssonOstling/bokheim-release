use std::cell::Cell;
mod dock;
mod session;
pub use dock::{AudiobookDock, DockAction};
pub use session::{ActiveAudiobook, PlaybackSession};
#[cfg(not(target_arch = "wasm32"))]
use std::io::{Read, Seek, SeekFrom};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Bounds, Context, DispatchPhase, Entity, Image, ImageFormat, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString, Styled, Window, canvas, div, prelude::*,
    px,
};
use web_time::Instant;

// Stage boundaries also appear on early returns, making failed opens visible
// in the Kobo launcher log without enabling per-sample audio logging.
struct OpeningTiming {
    phase: &'static str,
    started: Instant,
}
impl OpeningTiming {
    fn new(phase: &'static str) -> Self {
        eprintln!("AUDIOBOOK_TIMING phase={phase} event=start");
        Self { phase, started: Instant::now() }
    }
}
impl Drop for OpeningTiming {
    fn drop(&mut self) {
        eprintln!("AUDIOBOOK_TIMING phase={} event=end elapsed_ms={}", self.phase, self.started.elapsed().as_millis());
    }
}
#[cfg(any(target_os = "android", test))]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[path = "android_audio.rs"]
pub mod android;
#[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
mod audio_engine;

#[cfg(target_os = "android")]
use android::AudioEngine;
#[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
use audio_engine::AudioEngine;
#[cfg(target_arch = "wasm32")]
mod web_audio;
use book_model::AudiobookChapter;
use gpui_component::menu::PopupMenu;
use gpui_component::{ElementExt, Icon, IconName};
use library_backend::LibraryClient;
use library_backend::ResolvedBook;
use library_model::BookLocator;
#[cfg(target_arch = "wasm32")]
use web_audio::AudioEngine;

const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(5);
type PlaybackPosition = (Duration, f32);
/// How far the transport's two skip controls move. Back is shorter than
/// forward: it is used to hear something again.
const SKIP_BACK_SECONDS: f64 = -15.0;
const SKIP_FORWARD_SECONDS: f64 = 30.0;
/// The speeds offered by the playback menu.
const PLAYBACK_SPEEDS: [f64; 6] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
/// The sleep timer's fixed lengths, in minutes.
const SLEEP_MINUTES: [u64; 5] = [5, 15, 30, 45, 60];

// Browser media callbacks report fractional progress, whereas the library stores
// percentages. Reject invalid media values before they reach persistence.
#[cfg(any(target_arch = "wasm32", test))]
fn browser_position_update(seconds: f64, fraction: f64) -> Option<PlaybackPosition> {
    if !fraction.is_finite() {
        return None;
    }
    let position = Duration::try_from_secs_f64(seconds).ok()?;
    Some((position, (fraction.clamp(0.0, 1.0) * 100.0) as f32))
}

#[cfg(test)]
mod browser_position_tests {
    use super::*;

    #[test]
    fn browser_progress_uses_library_percentages() {
        assert_eq!(browser_position_update(150.25, 0.5), Some((Duration::from_millis(150250), 50.0)));
        assert_eq!(browser_position_update(300.0, 1.0), Some((Duration::from_secs(300), 100.0)));
        assert_eq!(browser_position_update(0.0, 0.0), Some((Duration::ZERO, 0.0)));
    }

    #[test]
    fn browser_progress_clamps_finite_rounding_overshoot() {
        assert_eq!(browser_position_update(1.0, 1.01).unwrap().1, 100.0);
        assert_eq!(browser_position_update(0.0, -0.01).unwrap().1, 0.0);
    }

    #[test]
    fn invalid_browser_media_values_are_not_saved() {
        for seconds in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
            assert_eq!(browser_position_update(seconds, 0.5), None);
        }
        for fraction in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(browser_position_update(1.0, fraction), None);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
trait ReadSeek: Read + Seek + Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Read + Seek + Send + Sync> ReadSeek for T {}

#[cfg(not(target_arch = "wasm32"))]
struct AudiobookSource {
    stream_control: Option<app::AudioStreamControl>,
    cached_metadata: Option<app::PlaybackMetadata>,
    track: Option<book_model::AudiobookTrack>,
    display_path: PathBuf,
    #[cfg(target_os = "android")]
    playback_file: Option<std::fs::File>,
    reader: Box<dyn ReadSeek>,
}

#[cfg(not(target_arch = "wasm32"))]
impl AudiobookSource {
    fn new(reader: impl Read + Seek + Send + Sync + 'static, display_path: PathBuf) -> Self {
        Self {
            stream_control: None,
            cached_metadata: None,
            track: None,
            display_path,
            reader: Box::new(reader),
            #[cfg(target_os = "android")]
            playback_file: None,
        }
    }
}

#[derive(Clone)]
struct AudiobookBook {
    title: String,
    author: Option<String>,
    narrator: Option<String>,
    duration: Duration,
    chapters: Vec<AudiobookChapter>,
    cover: Option<Arc<Image>>,
}

/// Persists the narration speed. It belongs to the reader's settings rather
/// than to this session: it describes how someone listens, not what to.
type SaveSpeed = fn(f64, &mut App);

/// What the sleep timer is waiting for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SleepPlan {
    /// A wall clock: it runs down whether or not anything is playing, so a
    /// pause to answer the door does not silently extend the night.
    Until(Instant),
    /// The end of the chapter that was playing when it was set.
    EndOfChapter(usize),
}

/// A choice in the sleep timer menu, shared by the compact sheet and the wide
/// dropdown.
#[derive(Clone, Copy)]
enum SleepChoice {
    Minutes(u64),
    EndOfChapter,
    Off,
}

impl SleepChoice {
    /// "Off" is only offered while a timer is running.
    fn offered(armed: bool) -> impl Iterator<Item = SleepChoice> {
        SLEEP_MINUTES.into_iter().map(SleepChoice::Minutes).chain([SleepChoice::EndOfChapter]).chain(armed.then_some(SleepChoice::Off))
    }

    fn id(self) -> SharedString {
        match self {
            SleepChoice::Minutes(minutes) => format!("sleep-{minutes}").into(),
            SleepChoice::EndOfChapter => "sleep-chapter".into(),
            SleepChoice::Off => "sleep-off".into(),
        }
    }

    fn label(self) -> SharedString {
        match self {
            SleepChoice::Minutes(minutes) => format!("{minutes} minutes").into(),
            SleepChoice::EndOfChapter => "End of chapter".into(),
            SleepChoice::Off => "Off".into(),
        }
    }

    fn plan(self, active_chapter: usize) -> Option<SleepPlan> {
        match self {
            SleepChoice::Minutes(minutes) => Some(SleepPlan::Until(Instant::now() + Duration::from_secs(minutes * 60))),
            SleepChoice::EndOfChapter => Some(SleepPlan::EndOfChapter(active_chapter)),
            SleepChoice::Off => None,
        }
    }
}

/// What a running sleep timer is waiting for, heading its menu.
fn sleep_status(remaining: &str) -> String {
    if remaining == "Chapter" { "Until end of chapter".to_owned() } else { format!("{remaining} remaining") }
}

fn playback_speed_items(mut menu: PopupMenu, target: gpui::WeakEntity<AudiobookDock>, current_rate: f64) -> PopupMenu {
    for rate in PLAYBACK_SPEEDS {
        let target = target.clone();
        menu = menu.item(
            ui_components::menu_item(format_playback_rate(rate), move |_, _, cx| {
                let _ = target.update(cx, |dock, cx| dock.set_rate(rate, cx));
            })
            .checked(same_rate(rate, current_rate)),
        );
    }
    menu
}

fn sleep_items(mut menu: PopupMenu, target: gpui::WeakEntity<AudiobookDock>, remaining: Option<SharedString>) -> PopupMenu {
    if let Some(remaining) = &remaining {
        menu = menu.item(ui_components::menu_section(sleep_status(remaining)));
    }
    for choice in SleepChoice::offered(remaining.is_some()) {
        let target = target.clone();
        menu = menu.item(ui_components::menu_item(choice.label(), move |_, _, cx| {
            let _ = target.update(cx, |dock, cx| dock.choose_sleep(choice, cx));
        }));
    }
    menu
}

fn playback_speed_sheet(target: gpui::WeakEntity<AudiobookDock>, current_rate: f64, theme: ui_components::BrowserTheme) -> gpui::AnyElement {
    let rows = PLAYBACK_SPEEDS
        .into_iter()
        .map(|rate| {
            let target = target.clone();
            ui_components::bottom_sheet_row(SharedString::from(format!("speed-{rate}")), format_playback_rate(rate), same_rate(rate, current_rate), theme)
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = target.update(cx, |dock, cx| dock.set_rate(rate, cx));
                })
                .into_any_element()
        })
        .collect();
    menu_sheet("playback-speed", "Playback speed", None, rows, move |cx| drop(target.update(cx, |dock, cx| dock.toggle_speed_menu(cx))), theme)
}

fn sleep_sheet(target: gpui::WeakEntity<AudiobookDock>, remaining: Option<SharedString>, theme: ui_components::BrowserTheme) -> gpui::AnyElement {
    let rows = SleepChoice::offered(remaining.is_some())
        .map(|choice| {
            let target = target.clone();
            ui_components::bottom_sheet_row(choice.id(), choice.label(), false, theme)
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = target.update(cx, |dock, cx| dock.choose_sleep(choice, cx));
                })
                .into_any_element()
        })
        .collect();
    menu_sheet("sleep", "Sleep timer", remaining.as_deref().map(sleep_status), rows, move |cx| drop(target.update(cx, |dock, cx| dock.toggle_sleep_menu(cx))), theme)
}

/// A playback menu as a bottom sheet, for a compact window.
fn menu_sheet(id: &'static str, title: &'static str, status: Option<String>, rows: Vec<gpui::AnyElement>, dismiss: impl Fn(&mut App) + 'static, theme: ui_components::BrowserTheme) -> gpui::AnyElement {
    let rows = ui_components::column(0.0).id(SharedString::from(format!("{id}-rows"))).flex_1().min_h_0().overflow_y_scroll().children(status.map(|status| ui_components::bottom_sheet_section(status, theme))).children(rows);
    let sheet = ui_components::bottom_sheet_surface(theme).id(SharedString::from(format!("{id}-sheet"))).debug_selector(move || format!("{id}-sheet")).child(ui_components::bottom_sheet_header(title, theme)).child(rows);
    ui_components::bottom_sheet_overlay().child(ui_components::modal_scrim(SharedString::from(format!("{id}-scrim")), theme).on_click(move |_, _, cx| dismiss(cx))).child(sheet).into_any_element()
}

fn format_playback_rate(rate: f64) -> String {
    let formatted = format!("{rate:.2}");
    format!("{}×", formatted.trim_end_matches('0').trim_end_matches('.'))
}

fn active_chapter_index(chapters: &[AudiobookChapter], position: Duration) -> usize {
    chapters.iter().rposition(|chapter| chapter.start <= position).unwrap_or(0)
}

fn clamp_position(position: Duration, duration: Duration) -> Duration {
    position.min(duration)
}

fn previous_chapter_position(chapters: &[AudiobookChapter], position: Duration) -> Duration {
    let index = active_chapter_index(chapters, position);
    let chapter_start = chapters.get(index).map(|chapter| chapter.start).unwrap_or_default();
    if position.saturating_sub(chapter_start) > Duration::from_secs(3) { chapter_start } else { chapters.get(index.saturating_sub(1)).map(|chapter| chapter.start).unwrap_or_default() }
}

fn next_chapter_position(chapters: &[AudiobookChapter], position: Duration, duration: Duration) -> Duration {
    chapters.get(active_chapter_index(chapters, position) + 1).map(|chapter| chapter.start).unwrap_or(duration)
}

fn relative_seek_position(position: Duration, seconds: f64, duration: Duration) -> Duration {
    if !seconds.is_finite() {
        return position.min(duration);
    }
    Duration::from_secs_f64((position.as_secs_f64() + seconds).clamp(0.0, duration.as_secs_f64()))
}

fn tempo_for(rate: f64) -> Result<f32, String> {
    if !rate.is_finite() {
        return Err("playback rate must be finite".to_owned());
    }
    Ok(rate as f32)
}

fn position_save_due(previous: Option<Duration>, position: Duration) -> bool {
    previous.is_none_or(|previous| previous.abs_diff(position) >= POSITION_SAVE_INTERVAL)
}

fn position_save_required(previous: Option<Duration>, position: Duration, force: bool) -> bool {
    previous != Some(position) && (force || position_save_due(previous, position))
}

fn playback_progress(position: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        return 0.0;
    }
    (position.as_secs_f64() / duration.as_secs_f64() * 100.0).clamp(0.0, 100.0) as f32
}

fn chapter_position_at_fraction(start: Duration, end: Duration, fraction: f32) -> Duration {
    if !fraction.is_finite() {
        return start;
    }
    start + end.saturating_sub(start).mul_f32(fraction.clamp(0.0, 1.0))
}

/// Whether a sleep plan has come due.
#[cfg(any(not(target_os = "android"), test))]
fn sleep_remaining(plan: SleepPlan, position: Duration, chapters: &[AudiobookChapter], now: Instant, rate: f64) -> Duration {
    match plan {
        SleepPlan::Until(deadline) => deadline.saturating_duration_since(now),
        SleepPlan::EndOfChapter(index) => chapters.get(index).map_or(Duration::ZERO, |chapter| chapter.end.saturating_sub(position).div_f64(rate.max(0.01))),
    }
}

#[cfg(any(not(target_os = "android"), test))]
fn sleep_gain(remaining: Duration) -> f32 {
    (remaining.as_secs_f32() / 30.0).clamp(0.0, 1.0)
}

#[cfg(test)]
fn sleep_elapsed(plan: SleepPlan, position: Duration, chapters: &[AudiobookChapter], now: Instant) -> bool {
    match plan {
        SleepPlan::Until(deadline) => now >= deadline,
        // A chapter the book no longer has cannot be waited for, so the timer
        // is treated as due rather than left armed forever.
        SleepPlan::EndOfChapter(chapter) => chapters.get(chapter).is_none_or(|chapter| position >= chapter.end),
    }
}

/// Whether two speeds are the same offered step. Playback rates are a short
/// list of round numbers, so exact comparison is what is meant.
fn same_rate(left: f64, right: f64) -> bool {
    (left - right).abs() < f64::EPSILON
}

fn format_time(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

fn format_duration_short(duration: Duration) -> String {
    format_minutes_short(duration.as_secs() / 60)
}

fn format_minutes_short(total_minutes: u64) -> String {
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if hours > 0 { format!("{hours} h {minutes} m") } else { format!("{minutes} m") }
}

fn format_time_left(duration: Duration) -> String {
    format!("{} left", format_minutes_short(duration.as_secs().div_ceil(60)))
}

fn chapter_display_title(title: &str) -> String {
    let title = title.trim();
    for separator in [" - ", " – ", " — "] {
        if let Some((name, suffix)) = title.rsplit_once(separator)
            && is_timestamp(suffix)
        {
            return name.trim().to_owned();
        }
    }
    title.to_owned()
}

fn is_timestamp(value: &str) -> bool {
    let parts = value.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit())) {
        return false;
    }
    parts[parts.len() - 2].parse::<u64>().is_ok_and(|minutes| minutes < 60) && parts[parts.len() - 1].parse::<u64>().is_ok_and(|seconds| seconds < 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(title: &str, start: u64, end: u64) -> AudiobookChapter {
        AudiobookChapter { title: title.to_owned(), start: Duration::from_secs(start), end: Duration::from_secs(end) }
    }

    fn chapters() -> Vec<AudiobookChapter> {
        vec![chapter("One", 0, 10), chapter("Two", 10, 30), chapter("Three", 30, 60)]
    }

    #[test]
    fn sleep_fade_uses_final_thirty_seconds_and_playback_rate() {
        let now = Instant::now();
        let plan = SleepPlan::Until(now + Duration::from_secs(60));
        assert_eq!(sleep_gain(sleep_remaining(plan, Duration::ZERO, &chapters(), now, 1.0)), 1.0);
        assert_eq!(sleep_gain(sleep_remaining(plan, Duration::ZERO, &chapters(), now + Duration::from_secs(45), 1.0)), 0.5);
        assert_eq!(sleep_gain(sleep_remaining(plan, Duration::ZERO, &chapters(), now + Duration::from_secs(60), 1.0)), 0.0);
        assert_eq!(sleep_gain(sleep_remaining(SleepPlan::EndOfChapter(1), Duration::ZERO, &chapters(), now, 2.0)), 0.5);
    }

    #[test]
    fn a_sleep_deadline_comes_due_on_the_clock() {
        let now = Instant::now();
        let plan = SleepPlan::Until(now + Duration::from_secs(60));
        assert!(!sleep_elapsed(plan, Duration::ZERO, &chapters(), now));
        assert!(sleep_elapsed(plan, Duration::ZERO, &chapters(), now + Duration::from_secs(61)));
    }

    #[test]
    fn an_end_of_chapter_sleep_waits_for_that_chapter_only() {
        let now = Instant::now();
        let plan = SleepPlan::EndOfChapter(1);
        assert!(!sleep_elapsed(plan, Duration::from_secs(20), &chapters(), now));
        assert!(sleep_elapsed(plan, Duration::from_secs(30), &chapters(), now));
        // A chapter the book does not have cannot be waited for.
        assert!(sleep_elapsed(SleepPlan::EndOfChapter(9), Duration::ZERO, &chapters(), now));
    }

    #[test]
    fn positions_are_clamped_to_the_book_duration() {
        assert_eq!(clamp_position(Duration::from_secs(600), Duration::from_secs(60)), Duration::from_secs(60));
        assert_eq!(clamp_position(Duration::MAX, Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn chapter_boundaries_select_the_chapter_that_starts_there() {
        let chapters = chapters();
        for (second, expected_index) in [(0, 0), (9, 0), (10, 1), (29, 1), (30, 2), (60, 2)] {
            assert_eq!(active_chapter_index(&chapters, Duration::from_secs(second)), expected_index, "at {second}s");
        }
    }

    #[test]
    fn navigation_targets_respect_chapters_and_book_boundaries() {
        let chapters = chapters();
        let duration = Duration::from_secs(60);

        assert_eq!(previous_chapter_position(&chapters, Duration::from_secs(35)), Duration::from_secs(30));
        assert_eq!(previous_chapter_position(&chapters, Duration::from_secs(30)), Duration::from_secs(10));
        assert_eq!(next_chapter_position(&chapters, Duration::from_secs(10), duration), Duration::from_secs(30));
        assert_eq!(next_chapter_position(&chapters, duration, duration), duration);
    }

    #[test]
    fn relative_seeking_clamps_and_ignores_non_finite_offsets() {
        let position = Duration::from_secs(20);
        let duration = Duration::from_secs(60);

        assert_eq!(relative_seek_position(position, -100.0, duration), Duration::ZERO);
        assert_eq!(relative_seek_position(position, 1_000.0, duration), duration);
        for offset in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(relative_seek_position(position, offset, duration), position);
        }
    }

    #[test]
    fn previous_chapter_uses_the_three_second_restart_threshold() {
        let chapters = chapters();
        assert_eq!(previous_chapter_position(&chapters, Duration::from_secs(13)), Duration::ZERO);
        assert_eq!(previous_chapter_position(&chapters, Duration::from_millis(13_001)), Duration::from_secs(10));
    }

    #[test]
    fn empty_chapter_navigation_stays_inside_the_book() {
        assert_eq!(active_chapter_index(&[], Duration::from_secs(10)), 0);
        assert_eq!(previous_chapter_position(&[], Duration::from_secs(10)), Duration::ZERO);
        assert_eq!(next_chapter_position(&[], Duration::from_secs(10), Duration::from_secs(60)), Duration::from_secs(60));
    }

    #[test]
    fn playback_rates_must_be_finite() {
        for rate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(tempo_for(rate), Err("playback rate must be finite".to_owned()));
        }
        assert_eq!(tempo_for(1.5), Ok(1.5));
    }

    #[test]
    fn time_format_covers_minute_and_hour_forms() {
        assert_eq!(format_time(Duration::from_secs(9)), "0:09");
        assert_eq!(format_time(Duration::from_secs(3_725)), "1:02:05");
    }

    #[test]
    fn book_time_left_rounds_up_until_playback_finishes() {
        assert_eq!(format_time_left(Duration::from_secs(1)), "1 m left");
        assert_eq!(format_time_left(Duration::from_secs(3_601)), "1 h 1 m left");
        assert_eq!(format_time_left(Duration::ZERO), "0 m left");
    }

    #[test]
    fn chapter_display_title_removes_only_a_trailing_duration() {
        assert_eq!(chapter_display_title("Chapter 001 - 00:30:37"), "Chapter 001");
        assert_eq!(chapter_display_title("Chapter 002 – 30:37"), "Chapter 002");
        assert_eq!(chapter_display_title("A title - with words"), "A title - with words");
        assert_eq!(chapter_display_title("A title - 72:99"), "A title - 72:99");
        assert_eq!(chapter_display_title("  Plain title  "), "Plain title");
    }

    #[test]
    fn playback_position_persistence_is_bounded_and_throttled() {
        assert!(position_save_due(None, Duration::ZERO));
        assert!(!position_save_due(Some(Duration::from_secs(10)), Duration::from_secs(14)));
        assert!(position_save_due(Some(Duration::from_secs(10)), Duration::from_secs(15)));
        assert!(position_save_due(Some(Duration::from_secs(10)), Duration::from_secs(5)));
        assert!(!position_save_required(Some(Duration::from_secs(10)), Duration::from_secs(10), true));
        assert!(position_save_required(Some(Duration::from_secs(10)), Duration::from_secs(11), true));
        assert!(!position_save_required(Some(Duration::from_secs(10)), Duration::from_secs(11), false));
        assert_eq!(playback_progress(Duration::from_secs(30), Duration::from_secs(120)), 25.0);
        assert_eq!(playback_progress(Duration::from_secs(200), Duration::from_secs(120)), 100.0);
        assert_eq!(playback_progress(Duration::from_secs(10), Duration::ZERO), 0.0);
    }

    #[test]
    fn chapter_progress_seek_stays_inside_the_active_chapter() {
        let start = Duration::from_secs(120);
        let end = Duration::from_secs(240);
        assert_eq!(chapter_position_at_fraction(start, end, -1.0), start);
        assert_eq!(chapter_position_at_fraction(start, end, 0.25), Duration::from_secs(150));
        assert_eq!(chapter_position_at_fraction(start, end, 2.0), end);
        assert_eq!(chapter_position_at_fraction(start, end, f32::NAN), start);
        assert_eq!(chapter_position_at_fraction(end, start, 0.5), end);
    }

    #[test]
    fn playback_rate_labels_are_compact() {
        assert_eq!(format_playback_rate(0.75), "0.75×");
        assert_eq!(format_playback_rate(1.0), "1×");
        assert_eq!(format_playback_rate(1.5), "1.5×");
        assert_eq!(format_playback_rate(2.0), "2×");
    }
}
