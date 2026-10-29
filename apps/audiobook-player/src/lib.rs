#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

#[cfg(not(target_arch = "wasm32"))]
use library_backend::library::AudioStreamControl;
use std::cell::Cell;
use std::rc::Rc;
mod dock;
mod session;
pub use dock::{AudiobookDock, DockAction};
pub use session::{ActiveAudiobook, PlaybackSession};
#[cfg(not(target_arch = "wasm32"))]
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use gpui::{App, Bounds, ClickEvent, Context, DispatchPhase, Entity, Image, ImageFormat, IntoElement, MouseButton, MouseMoveEvent, Pixels, Render, SharedString, Styled, Window, canvas, div, prelude::*, px, rems};
use gpui_component::{Placement, WindowExt as _};
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

// The service bridge and source-reader tests need no Android runtime.
#[cfg(all(test, not(target_arch = "wasm32"), not(target_os = "android")))]
#[path = "android_audio.rs"]
mod android_audio_tests;

#[cfg(target_os = "android")]
use android::AudioEngine;
#[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
use audio_engine::AudioEngine;
#[cfg(target_arch = "wasm32")]
mod web_audio;
use book_model::{AudiobookChapter, BookTocEntry, ChapterReadStatus, audiobook_toc_target};
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::{ElementExt, Icon, IconName};
use library_backend::LibraryClient;
use library_backend::ResolvedBook;
use library_model::BookLocator;
#[cfg(target_arch = "wasm32")]
use web_audio::AudioEngine;

const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(5);
#[derive(Debug, PartialEq)]
enum PlaybackPosition {
    Position { position: Duration, progress: f32 },
    ResetReadStatus { target: Option<String> },
}

impl PlaybackPosition {
    fn new(position: Duration, progress: f32) -> Self {
        Self::Position { position, progress }
    }
}

/// Recover small playback-state records after a worker unwinds. Source readers
/// use fallible locks instead: a poisoned reader must not be reused.
#[cfg(not(target_arch = "wasm32"))]
fn lock_state<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    match state.lock() {
        Ok(guard) => guard,
        Err(error) => {
            log::error!("Recovering poisoned audiobook state");
            let guard = error.into_inner();
            state.clear_poison();
            guard
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod lock_tests {
    use super::lock_state;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Mutex;

    #[test]
    fn playback_state_remains_available_after_a_worker_unwinds() {
        let state = Mutex::new(42);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = state.lock().unwrap();
            panic!("worker failed while holding playback state");
        }));
        assert!(state.is_poisoned());
        *lock_state(&state) += 1;
        assert!(!state.is_poisoned());
        assert_eq!(*lock_state(&state), 43);
    }
}
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
    Some(PlaybackPosition::new(position, (fraction.clamp(0.0, 1.0) * 100.0) as f32))
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
pub enum SleepPlan {
    /// A wall clock: it runs down whether or not anything is playing, so a
    /// pause to answer the door does not silently extend the night.
    Until(Instant),
    /// The end of the chapter that was playing when it was set.
    EndOfChapter(usize),
}

impl SleepPlan {
    pub fn deadline(self) -> Option<Instant> {
        match self {
            Self::Until(deadline) => Some(deadline),
            Self::EndOfChapter(_) => None,
        }
    }

    pub fn chapter(self) -> Option<usize> {
        match self {
            Self::EndOfChapter(chapter) => Some(chapter),
            Self::Until(_) => None,
        }
    }

    pub fn label(self, now: Instant) -> SharedString {
        match self {
            Self::Until(deadline) => format!("{} min", deadline.saturating_duration_since(now).as_secs().div_ceil(60)).into(),
            Self::EndOfChapter(_) => "Chapter".into(),
        }
    }
}

/// A choice in the sleep timer menu, shared by the compact sheet and the wide
/// dropdown.
#[derive(Clone, Copy)]
pub enum SleepChoice {
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

    pub fn plan(self, active_chapter: usize) -> Option<SleepPlan> {
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
pub struct PlaybackMenu<T> {
    id: &'static str,
    title: &'static str,
    status: Option<String>,
    choices: Vec<PlaybackChoice<T>>,
    presets: bool,
}

struct PlaybackChoice<T> {
    id: SharedString,
    label: SharedString,
    checked: bool,
    value: T,
    trailing: Option<SharedString>,
    short_label: Option<SharedString>,
}

pub fn speed_menu(current_rate: f64) -> PlaybackMenu<f64> {
    let choices = PLAYBACK_SPEEDS.into_iter().map(|rate| PlaybackChoice { id: format!("speed-{rate}").into(), label: format_playback_rate(rate).into(), checked: same_rate(rate, current_rate), value: rate, trailing: None, short_label: None }).collect();
    PlaybackMenu { id: "playback-speed", title: "Playback speed", status: None, choices, presets: true }
}

pub fn sleep_menu(remaining: Option<&str>) -> PlaybackMenu<SleepChoice> {
    sleep_menu_for_section(remaining, "chapter")
}

/// Reader narration can end at a PDF page instead of an audio chapter.
pub fn sleep_menu_for_section(remaining: Option<&str>, section: &str) -> PlaybackMenu<SleepChoice> {
    let choices = SleepChoice::offered(remaining.is_some())
        .map(|choice| {
            let short_label = match choice { SleepChoice::Minutes(minutes) => format!("{minutes}m"), SleepChoice::EndOfChapter => "End".into(), SleepChoice::Off => "Off".into() };
            PlaybackChoice { id: choice.id(), label: if matches!(choice, SleepChoice::EndOfChapter) { format!("End of {section}").into() } else { choice.label() }, checked: false, value: choice, trailing: None, short_label: Some(short_label.into()) }
        })
        .collect();
    let status = remaining.map(|remaining| if remaining == "Chapter" { format!("Until end of {section}") } else { sleep_status(remaining) });
    PlaybackMenu { id: "sleep", title: "Sleep timer", status, choices, presets: true }
}

impl<T: Copy + 'static> PlaybackMenu<T> {
    pub fn new(id: &'static str, title: &'static str, choices: impl IntoIterator<Item = (T, SharedString, bool)>) -> Self {
        let choices = choices.into_iter().enumerate().map(|(index, (value, label, checked))| PlaybackChoice { id: format!("{id}-{index}").into(), label, checked, value, trailing: None, short_label: None }).collect();
        Self { id, title, status: None, choices, presets: false }
    }

    pub fn title(mut self, title: &'static str) -> Self {
        self.title = title;
        self
    }

    /// A single horizontal row of equal square presets. Larger text can scroll
    /// the row horizontally instead of shrinking or wrapping the controls.
    fn preset_row<H: Fn(&ClickEvent, &mut Window, &mut App) + 'static>(&self, choose: impl Fn(usize, T) -> H, theme: ui_components::BrowserTheme) -> gpui::AnyElement {
        let buttons = self.choices.iter().enumerate().map(|(index, choice)| {
            let label = choice.short_label.clone().unwrap_or_else(|| choice.label.clone());
            let button = ui_components::playback_secondary_button(ui_components::base_button(choice.id.clone()).ghost())
                .text_color(theme.text)
                .bg(theme.page_bg)
                .border_1()
                .border_color(if choice.checked { theme.accent } else { theme.rule })
                .tooltip(choice.label.clone())
                .debug_selector({ let id = choice.id.to_string(); move || id.clone() })
                .child(div().text_size(rems(ui_components::TEXT_XS)).whitespace_nowrap().child(label));
            ui_components::selectable_button(button, choice.checked, theme).on_click(choose(index, choice.value))
        });
        div().id(SharedString::from(format!("{}-presets", self.id))).w_full().flex_none().p(px(ui_components::SPACE_MD)).overflow_x_scroll().child(
            ui_components::row(0.0).w_full().min_w(rems(self.choices.len() as f32 * (ui_components::PLAYBACK_SECONDARY_SIZE_REM + 0.25) + 0.25)).justify_evenly().children(buttons),
        ).into_any_element()
    }

    /// The window owns focus, dismissal and layering, so sheets opened from a
    /// short dock cover the reader and consume native Back before navigation.
    pub fn open_sheet(self, window: &mut Window, cx: &mut App, choose: impl Fn(T, &mut App) + 'static) {
        let choose = Rc::new(choose);
        window.open_sheet_at(Placement::Bottom, cx, move |sheet, window, cx| {
            let theme = ui_components::browser_theme(cx);
            let safe_area = window.insets().effective();
            let rem = f32::from(window.rem_size());
            let usable_height = (window.viewport_size().height - safe_area.top).max(px(0.0));
            let row_height = (ui_components::TEXT_MD * rem * 1.5 + ui_components::SPACE_SM * 2.0).max(48.0);
            let header_height = (ui_components::TEXT_MD * rem * 1.5).max(24.0) + 16.0;
            let status_height = if !self.presets && self.status.is_some() { ui_components::TEXT_XS * rem * 1.5 + 10.0 } else { 0.0 };
            let choices_height = if self.presets { ui_components::PLAYBACK_SECONDARY_SIZE_REM * rem + ui_components::SPACE_MD * 2.0 } else { row_height * self.choices.len() as f32 };
            let height = (header_height + choices_height + status_height + f32::from(safe_area.bottom) + 1.0).min(f32::from(usable_height) * 0.8);
            let content = if self.presets {
                self.preset_row(|_, value| {
                    let choose = choose.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        window.close_sheet(cx);
                        choose(value, cx);
                    }
                }, theme)
            } else {
                let rows = self.choices.iter().map(|choice| {
                    let choose = choose.clone();
                    let value = choice.value;
                    let label = div()
                        .flex()
                        .items_center()
                        .gap(px(ui_components::SPACE_SM))
                        .child(div().flex_1().min_w_0().child(choice.label.clone()))
                        .when_some(choice.trailing.clone(), |row, amount| row.child(ui_components::bottom_sheet_row_caption(amount, choice.checked, theme).flex_none()));
                    ui_components::bottom_sheet_row(choice.id.clone(), label, choice.checked, theme)
                        .debug_selector({
                            let id = choice.id.to_string();
                            move || id.clone()
                        })
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            window.close_sheet(cx);
                            choose(value, cx);
                        })
                });
                ui_components::column(0.0).children(self.status.as_ref().map(|status| ui_components::bottom_sheet_section(status.clone(), theme))).children(rows).into_any_element()
            };
            sheet
                .title(div().text_size(rems(ui_components::TEXT_MD)).child(self.title))
                .size(px(height))
                .resizable(false)
                .p_0()
                .pl(safe_area.left)
                .pr(safe_area.right)
                .bg(theme.page_bg)
                .border_color(theme.border)
                .child(div().w_full().flex().flex_col().border_t_1().border_color(theme.rule).child(content).child(div().h(safe_area.bottom).flex_none()))
        });
    }

    pub fn dropdown(self, mut menu: PopupMenu, choose: impl Fn(T, &mut App) + Clone + 'static) -> PopupMenu {
        if self.presets {
            let selected = Rc::new(Cell::new(self.choices.iter().position(|choice| choice.checked).unwrap_or(0)));
            let render_selected = selected.clone();
            let choices = Rc::new(self);
            let activate = choices.clone();
            // The child records the clicked preset. The enclosing menu item
            // applies it and uses the popup's normal selection dismissal.
            return menu.min_w(px(320.0)).item(PopupMenuItem::element(move |_, cx| {
                let selected = render_selected.clone();
                choices.preset_row(move |index, _| {
                    let selected = selected.clone();
                    move |_, _, _| selected.set(index)
                }, ui_components::browser_theme(cx))
            }).on_click(move |_, _, cx| {
                if let Some(choice) = activate.choices.get(selected.get()) { choose(choice.value, cx); }
            }));
        }
        if let Some(status) = self.status {
            menu = menu.item(ui_components::menu_section(status));
        }
        for choice in self.choices {
            let choose = choose.clone();
            let label = choice.trailing.map(|amount| format!("{} · {amount}", choice.label).into()).unwrap_or(choice.label);
            menu = menu.item(ui_components::menu_item(label, move |_, _, cx| choose(choice.value, cx)).checked(choice.checked));
        }
        menu
    }


}

pub fn format_playback_rate(rate: f64) -> String {
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

#[cfg(test)]
mod playback_sheet_tests {
    use super::*;
    use gpui::{DismissEvent, FocusHandle, Focusable as _, TestAppContext, size};
    use gpui_component::Root;

    struct SheetHost {
        focus: FocusHandle,
    }

    struct MenuHost {
        menu: Entity<PopupMenu>,
    }

    impl Render for MenuHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.menu.clone()
        }
    }

    impl Render for SheetHost {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().track_focus(&self.focus).children(Root::render_sheet_layer(window, cx).map(|sheet| gpui::deferred(sheet).with_priority(2)))
        }
    }

    #[gpui::test]
    fn playback_preset_popup_selection_applies_and_dismisses(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let chosen = Rc::new(Cell::new(1.25));
        let selected = chosen.clone();
        let (host, cx) = cx.add_window_view(move |window, cx| {
            let menu = PopupMenu::build(window, cx, move |menu, _, _| speed_menu(1.25).dropdown(menu, move |rate, _| selected.set(rate)));
            menu.read(cx).focus_handle(cx).focus(window, cx);
            MenuHost { menu }
        });
        let menu = host.read_with(cx, |host, _| host.menu.clone());
        cx.simulate_resize(size(px(390.0), px(844.0)));
        let dismissed = Rc::new(Cell::new(false));
        let observed = dismissed.clone();
        let _subscription = cx.update(|_, cx| cx.subscribe(&menu, move |_, _: &DismissEvent, _| observed.set(true)));
        cx.run_until_parked();
        cx.update(|window, cx| { let _ = window.draw(cx); });
        let preset = cx.debug_bounds("speed-1.5").expect("popup speed presets must render");
        cx.simulate_click(preset.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(chosen.get(), 1.5);
        assert!(dismissed.get(), "clicking a preset must use the menu's selection dismissal");
    }

    #[gpui::test]
    fn playback_sheet_selection_and_dismissal_restore_reader_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                SheetHost { focus }
            });
            Root::new(host, window, cx).bordered(false)
        });
        cx.simulate_resize(size(px(390.0), px(844.0)));
        cx.run_until_parked();
        let previous = cx.update(|window, cx| window.focused(cx).unwrap());
        let chosen = Rc::new(Cell::new(1.25));
        let selected = chosen.clone();
        cx.update(|window, cx| speed_menu(1.25).title("Reading speed").open_sheet(window, cx, move |rate, _| selected.set(rate)));
        cx.run_until_parked();
        cx.update(|window, cx| { let _ = window.draw(cx); });
        let row = cx.debug_bounds("speed-1.5").expect("speed choices must render in the sheet");
        let viewport = cx.update(|window, _| window.viewport_size());
        assert_eq!(row.size.width, row.size.height, "presets should be square");
        assert!(row.size.width < viewport.width / 4.0, "presets should share one horizontal row");
        let next = cx.debug_bounds("speed-1.75").expect("adjacent speed must render");
        assert_eq!(row.top(), next.top());
        assert!(next.left() >= row.right());
        cx.simulate_click(row.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(chosen.get(), 1.5);
        cx.update(|window, cx| {
            assert!(!window.has_active_sheet(cx));
            assert!(previous.is_focused(window));
        });

        for native_back in [false, true] {
            let selected = chosen.clone();
            cx.update(|window, cx| {
                PlaybackMenu::new("tts-pauses", "Pauses", [(2.0, "Long pauses".into(), false)]).open_sheet(window, cx, move |rate, _| selected.set(rate));
            });
            cx.run_until_parked();
            if native_back {
                cx.update(|window, cx| assert!(Root::dismiss_active_overlay(window, cx)));
            } else {
                cx.simulate_keystrokes("escape");
            }
            cx.run_until_parked();
            assert_eq!(chosen.get(), 1.5, "dismissing must not apply a choice");
            cx.update(|window, cx| {
                assert!(!window.has_active_sheet(cx));
                assert!(previous.is_focused(window));
            });
        }

        let selected = chosen.clone();
        cx.update(|window, cx| {
            PlaybackMenu::new("tts-voice", "Narration voice", [(1.5, "Alba".into(), true), (2.0, "George".into(), false)]).open_sheet(window, cx, move |voice, _| selected.set(voice));
        });
        cx.run_until_parked();
        cx.update(|window, cx| { let _ = window.draw(cx); });
        let row = cx.debug_bounds("tts-voice-1").expect("shared choice rows must render in the sheet");
        cx.simulate_click(row.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(chosen.get(), 2.0);
        cx.update(|window, cx| {
            assert!(!window.has_active_sheet(cx));
            assert!(previous.is_focused(window));
        });
    }
}
