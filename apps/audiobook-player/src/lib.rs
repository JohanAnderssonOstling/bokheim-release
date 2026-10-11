#[cfg(not(target_arch = "wasm32"))]
use library_backend::library::AudioStreamControl;
use std::cell::Cell;
mod dock;
mod session;
pub use dock::{AudiobookDock, DockAction};
pub use session::{ActiveAudiobook, PlaybackSession};
#[cfg(not(target_arch = "wasm32"))]
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Bounds, ClickEvent, Context, DispatchPhase, Entity, Image, ImageFormat, IntoElement, MouseButton, MouseMoveEvent, Pixels, Render, SharedString, Styled, Window, canvas, div, prelude::*, px};
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
#[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
mod audio_engine;

#[cfg(target_os = "android")]
#[path = "android_audio.rs"]
pub mod android;
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


#[cfg(not(target_arch = "wasm32"))]
trait ReadSeek: Read + Seek + Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Read + Seek + Send + Sync> ReadSeek for T {}

#[cfg(not(target_arch = "wasm32"))]
struct AudiobookSource {
    stream_control: Option<AudioStreamControl>,
    cached_metadata: Option<library_model::PlaybackMetadata>,
    track: Option<book_model::AudiobookTrack>,
    #[cfg(target_os = "android")]
    playback_file: Option<std::fs::File>,
    reader: Box<dyn ReadSeek>,
}

#[cfg(not(target_arch = "wasm32"))]
impl AudiobookSource {
    fn new(reader: impl Read + Seek + Send + Sync + 'static) -> Self {
        Self {
            stream_control: None,
            cached_metadata: None,
            track: None,
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

/// A playback menu's contents, shared by the wide dropdown and the compact
/// sheet: an optional status line over its choices.
struct PlaybackMenu<T> {
    id: &'static str,
    title: &'static str,
    status: Option<String>,
    choices: Vec<PlaybackChoice<T>>,
}

struct PlaybackChoice<T> {
    id: SharedString,
    label: SharedString,
    checked: bool,
    value: T,
}

fn speed_menu(current_rate: f64) -> PlaybackMenu<f64> {
    let choices = PLAYBACK_SPEEDS.into_iter().map(|rate| PlaybackChoice { id: format!("speed-{rate}").into(), label: format_playback_rate(rate).into(), checked: same_rate(rate, current_rate), value: rate }).collect();
    PlaybackMenu { id: "playback-speed", title: "Playback speed", status: None, choices }
}

fn sleep_menu(remaining: Option<&str>) -> PlaybackMenu<SleepChoice> {
    let choices = SleepChoice::offered(remaining.is_some()).map(|choice| PlaybackChoice { id: choice.id(), label: choice.label(), checked: false, value: choice }).collect();
    PlaybackMenu { id: "sleep", title: "Sleep timer", status: remaining.map(sleep_status), choices }
}

impl<T: Copy + 'static> PlaybackMenu<T> {
    fn dropdown(self, mut menu: PopupMenu, choose: impl Fn(T, &mut App) + Clone + 'static) -> PopupMenu {
        if let Some(status) = self.status {
            menu = menu.item(ui_components::menu_section(status));
        }
        for choice in self.choices {
            let choose = choose.clone();
            menu = menu.item(ui_components::menu_item(choice.label, move |_, _, cx| choose(choice.value, cx)).checked(choice.checked));
        }
        menu
    }

    /// Choosing also closes the sheet, which is the dock's state, so the dock
    /// supplies each row's handler.
    fn sheet<H: Fn(&ClickEvent, &mut Window, &mut App) + 'static>(self, choose: impl Fn(T) -> H, dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, theme: ui_components::BrowserTheme) -> gpui::AnyElement {
        let id = self.id;
        let rows = self.choices.into_iter().map(|choice| {
            let choose = choose(choice.value);
            ui_components::bottom_sheet_row(choice.id, choice.label, choice.checked, theme).on_click(move |event, window, cx| {
                cx.stop_propagation();
                choose(event, window, cx);
            })
        });
        let rows = ui_components::column(0.0).id(SharedString::from(format!("{id}-rows"))).flex_1().min_h_0().overflow_y_scroll().children(self.status.map(|status| ui_components::bottom_sheet_section(status, theme))).children(rows);
        let sheet = ui_components::bottom_sheet_surface(theme).id(SharedString::from(format!("{id}-sheet"))).child(ui_components::bottom_sheet_header(self.title, theme)).child(rows);
        ui_components::bottom_sheet_overlay().child(ui_components::modal_scrim(SharedString::from(format!("{id}-scrim")), theme).on_click(dismiss)).child(sheet).into_any_element()
    }
}

fn format_playback_rate(rate: f64) -> String {
    let formatted = format!("{rate:.2}");
    format!("{}×", formatted.trim_end_matches('0').trim_end_matches('.'))
}

fn active_chapter_index(chapters: &[AudiobookChapter], position: Duration) -> usize {
    chapters.iter().rposition(|chapter| chapter.start() <= position).unwrap_or(0)
}

fn clamp_position(position: Duration, duration: Duration) -> Duration {
    position.min(duration)
}

fn previous_chapter_position(chapters: &[AudiobookChapter], position: Duration) -> Duration {
    let index = active_chapter_index(chapters, position);
    let chapter_start = chapters.get(index).map(|chapter| chapter.start()).unwrap_or_default();
    if position.saturating_sub(chapter_start) > Duration::from_secs(3) { chapter_start } else { chapters.get(index.saturating_sub(1)).map(|chapter| chapter.start()).unwrap_or_default() }
}

fn next_chapter_position(chapters: &[AudiobookChapter], position: Duration, duration: Duration) -> Duration {
    chapters.get(active_chapter_index(chapters, position) + 1).map(|chapter| chapter.start()).unwrap_or(duration)
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
        SleepPlan::EndOfChapter(index) => chapters.get(index).map_or(Duration::ZERO, |chapter| chapter.end().saturating_sub(position).div_f64(rate.max(0.01))),
    }
}

#[cfg(any(not(target_os = "android"), test))]
fn sleep_gain(remaining: Duration) -> f32 {
    (remaining.as_secs_f32() / 30.0).clamp(0.0, 1.0)
}


/// Whether two speeds are the same offered step. Playback rates are a short
/// list of round numbers, so exact comparison is what is meant.
fn same_rate(left: f64, right: f64) -> bool {
    (left - right).abs() < f64::EPSILON
}

/// Playback position as the dock shows it: whole minutes of the book. Every
/// readout derives from this one value, so the dock changes at most once a
/// minute while playing, all at once, rather than ticking every second (which
/// on e-ink is a partial refresh each time).
fn shown_position(position: Duration) -> Duration {
    Duration::from_secs(position.as_secs() / 60 * 60)
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
