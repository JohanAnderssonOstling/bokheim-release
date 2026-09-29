use std::cell::Cell;
mod dock;
mod session;
pub use dock::{AudiobookDock, DockAction};
pub use session::{ActiveAudiobook, PlaybackSession};
#[cfg(not(target_arch = "wasm32"))]
use std::io::{Read, Seek, SeekFrom};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Bounds, Context, DispatchPhase, Entity, FontWeight, Image, ImageFormat, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, Render, SharedString, Styled,
    StyledImage, Window, canvas, div, img, prelude::*, px, relative,
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
use gpui_component::{ElementExt, Icon, IconName};
use library_backend::LibraryClient;
use library_backend::ResolvedBook;
use library_model::BookLocator;
#[cfg(target_arch = "wasm32")]
use web_audio::AudioEngine;

const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(5);
/// How far the transport's two skip controls move. Back is shorter than
/// forward: it is used to hear something again.
const SKIP_BACK_SECONDS: f64 = -15.0;
const SKIP_FORWARD_SECONDS: f64 = 30.0;
/// The speeds offered by the playback menu.
const PLAYBACK_SPEEDS: [f64; 6] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
/// The sleep timer's fixed lengths, in minutes.
const SLEEP_MINUTES: [u64; 5] = [5, 15, 30, 45, 60];
/// Width of the playback menus on a wide window.
const MENU_WIDTH: f32 = 176.0;

#[derive(Clone, Copy)]
struct MenuLayout {
    compact: bool,
    max_width: f32,
    max_height: f32,
}

impl MenuLayout {
    fn for_window(window: &Window) -> Self {
        let compact = ui_components::WindowWidthClass::for_window(window).is_compact();
        Self {
            compact,
            max_width: (f32::from(window.viewport_size().width) - if compact { 0.0 } else { 2.0 * ui_components::SPACE_MD }).max(1.0),
            max_height: (f32::from(window.viewport_size().height) - if compact { ui_components::SPACE_MD } else { 56.0 + 2.0 * ui_components::SPACE_MD }).max(1.0),
        }
    }
}
/// The progress track is drawn thin but has to be easy to hit, so its hit area
/// is taller than the bar inside it.
const TRACK_HIT_HEIGHT: f32 = 16.0;

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
type SaveSpeed = Rc<dyn Fn(f64, &mut App)>;

/// What the sleep timer is waiting for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SleepPlan {
    /// A wall clock: it runs down whether or not anything is playing, so a
    /// pause to answer the door does not silently extend the night.
    Until(Instant),
    /// The end of the chapter that was playing when it was set.
    EndOfChapter(usize),
}

trait PlaybackTarget: 'static {
    fn toggle_playback(&mut self, cx: &mut App);
    fn seek_by(&mut self, seconds: f64, cx: &mut App);
    fn previous_chapter(&mut self, cx: &mut App);
    fn next_chapter(&mut self, cx: &mut App);
    fn set_rate(&mut self, rate: f64, cx: &mut App);
    fn set_sleep(&mut self, plan: Option<SleepPlan>, cx: &mut App);
    fn toggle_sleep_menu(&mut self);
    fn toggle_speed_menu(&mut self);
    fn active_chapter_index(&self, cx: &App) -> usize;
    fn sleep_label(&self, cx: &App) -> Option<SharedString>;
}

fn menu_trigger(id: &'static str, label: impl IntoElement, active: bool, theme: ui_components::BrowserTheme) -> gpui::Stateful<gpui::Div> {
    // Active is already the solid accent, so the ordinary translucent hover
    // would just erase it; it darkens instead, the same distinction
    // `accent_hover` exists for everywhere else in the interface.
    let hovered = if active { theme.accent_hover } else { theme.hover };
    div()
        .id(id)
        .cursor_pointer()
        .h(px(56.0))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(if active { theme.accent } else { gpui::Hsla::transparent_black() })
        .text_color(if active { theme.accent_text } else { theme.text_muted })
        .hover(move |style| style.bg(hovered))
        .text_size(gpui::rems(ui_components::TEXT_MD))
        .font_weight(FontWeight::BOLD)
        .child(label)
}

/// The small popup anchored above the transport, for a wide window. A
/// compact window uses a full-width bottom sheet instead — see the `layout.compact`
/// branch in `sleep_menu`/`playback_speed_menu`, which build one directly from
/// `ui_components::bottom_sheet_*` rather than through this function.
fn menu_panel(theme: ui_components::BrowserTheme, layout: MenuLayout, title: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(title)
        .absolute()
        .bottom(px(56.0))
        .w(gpui::rems(MENU_WIDTH / 16.0))
        .max_w(px(layout.max_width))
        .max_h(px(layout.max_height))
        .overflow_y_scroll()
        .whitespace_normal()
        .p(px(ui_components::SPACE_XS))
        .border_1()
        .border_color(theme.border)
        .bg(theme.page_bg)
        .shadow_lg()
        .flex()
        .flex_col()
        .gap(px(ui_components::SPACE_XXS))
        .child(
            div()
                .flex_none()
                .min_w_0()
                .px(px(ui_components::SPACE_SM))
                .pt(px(ui_components::SPACE_XS))
                .pb(px(ui_components::SPACE_XS))
                .text_size(gpui::rems(ui_components::TEXT_MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(title),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

fn menu_option(id: String, label: String, selected: bool, theme: ui_components::BrowserTheme) -> gpui::Stateful<gpui::Div> {
    // Selected is already the solid accent, so the ordinary translucent
    // hover would just erase it; it darkens instead, the same distinction
    // `accent_hover` exists for everywhere else in the interface.
    let hovered = if selected { theme.accent_hover } else { theme.hover };
    div()
        .id(gpui::SharedString::from(id))
        .flex_none()
        .min_w_0()
        .cursor_pointer()
        .min_h(px(48.0))
        .flex()
        .items_center()
        .justify_between()
        .px(px(ui_components::SPACE_MD))
        .py(px(ui_components::SPACE_SM))
        .text_size(gpui::rems(ui_components::TEXT_MD))
        .hover(move |style| style.bg(hovered))
        .when(selected, |row| row.bg(theme.accent).text_color(theme.accent_text))
        .child(div().flex_1().min_w_0().whitespace_normal().child(label))
        .when(selected, |row| row.child(Icon::new(IconName::Check).size(px(15.0))))
}

fn playback_speed_menu<T: PlaybackTarget>(target: gpui::WeakEntity<T>, current_rate: f64, layout: MenuLayout, theme: ui_components::BrowserTheme) -> gpui::AnyElement {
    if layout.compact {
        let mut rows = div().id("playback-speed-rows").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        for rate in PLAYBACK_SPEEDS {
            let option_target = target.clone();
            rows = rows.child(menu_option(format!("speed-{rate}"), format_playback_rate(rate), same_rate(rate, current_rate), theme).on_click(move |_, _, cx| {
                cx.stop_propagation();
                let _ = option_target.update(cx, |dock, cx| {
                    dock.set_rate(rate, cx);
                    cx.notify();
                });
            }));
        }
        let dismiss_target = target;
        let sheet = ui_components::bottom_sheet_surface(theme).id("playback-speed-sheet").debug_selector(|| "playback-speed-sheet".into()).child(ui_components::bottom_sheet_header("Playback speed", theme)).child(rows);
        return ui_components::bottom_sheet_overlay()
            .child(ui_components::modal_scrim("playback-speed-scrim", theme).on_click(move |_, _, cx| {
                let _ = dismiss_target.update(cx, |dock, cx| {
                    dock.toggle_speed_menu();
                    cx.notify();
                });
            }))
            .child(sheet)
            .into_any_element();
    }
    let mut menu = menu_panel(theme, layout, "Playback speed").right_0();
    for rate in PLAYBACK_SPEEDS {
        let option_target = target.clone();
        menu = menu.child(menu_option(format!("speed-{rate}"), format_playback_rate(rate), same_rate(rate, current_rate), theme).on_click(move |_, _, cx| {
            cx.stop_propagation();
            let _ = option_target.update(cx, |dock, cx| {
                dock.set_rate(rate, cx);
                cx.notify();
            });
        }));
    }
    menu.into_any_element()
}

fn sleep_menu<T: PlaybackTarget>(target: gpui::WeakEntity<T>, remaining: Option<SharedString>, layout: MenuLayout, theme: ui_components::BrowserTheme) -> gpui::AnyElement {
    let armed = remaining.is_some();
    if layout.compact {
        let mut rows = div().id("sleep-rows").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().when_some(remaining, |rows, remaining| {
            let label = if remaining.as_ref() == "Chapter" { "Until end of chapter".to_owned() } else { format!("{remaining} remaining") };
            rows.child(div().flex_none().min_w_0().px(px(ui_components::SPACE_SM)).py(px(ui_components::SPACE_SM)).text_size(gpui::rems(ui_components::TEXT_MD)).font_weight(FontWeight::SEMIBOLD).child(label))
        });
        for minutes in SLEEP_MINUTES {
            let option_target = target.clone();
            rows = rows.child(menu_option(format!("sleep-{minutes}"), format!("{minutes} minutes"), false, theme).on_click(move |_, _, cx| {
                cx.stop_propagation();
                let _ = option_target.update(cx, |dock, cx| {
                    dock.set_sleep(Some(SleepPlan::Until(Instant::now() + Duration::from_secs(minutes * 60))), cx);
                    cx.notify();
                });
            }));
        }
        let chapter_target = target.clone();
        rows = rows.child(menu_option("sleep-chapter".to_owned(), "End of chapter".to_owned(), false, theme).on_click(move |_, _, cx| {
            cx.stop_propagation();
            let _ = chapter_target.update(cx, |dock, cx| {
                let chapter = dock.active_chapter_index(cx);
                dock.set_sleep(Some(SleepPlan::EndOfChapter(chapter)), cx);
                cx.notify();
            });
        }));
        if armed {
            let off_target = target.clone();
            rows = rows.child(menu_option("sleep-off".to_owned(), "Off".to_owned(), false, theme).on_click(move |_, _, cx| {
                cx.stop_propagation();
                let _ = off_target.update(cx, |dock, cx| {
                    dock.set_sleep(None, cx);
                    cx.notify();
                });
            }));
        }
        let dismiss_target = target;
        let sheet = ui_components::bottom_sheet_surface(theme).id("sleep-sheet").debug_selector(|| "sleep-sheet".into()).child(ui_components::bottom_sheet_header("Sleep timer", theme)).child(rows);
        return ui_components::bottom_sheet_overlay()
            .child(ui_components::modal_scrim("sleep-scrim", theme).on_click(move |_, _, cx| {
                let _ = dismiss_target.update(cx, |dock, cx| {
                    dock.toggle_sleep_menu();
                    cx.notify();
                });
            }))
            .child(sheet)
            .into_any_element();
    }
    let mut menu = menu_panel(theme, layout, "Sleep timer").left_0().when_some(remaining, |menu, remaining| {
        let label = if remaining.as_ref() == "Chapter" { "Until end of chapter".to_owned() } else { format!("{remaining} remaining") };
        menu.child(div().flex_none().min_w_0().px(px(ui_components::SPACE_SM)).py(px(ui_components::SPACE_SM)).text_size(gpui::rems(ui_components::TEXT_MD / 16.0)).font_weight(FontWeight::SEMIBOLD).child(label))
    });
    for minutes in SLEEP_MINUTES {
        let option_target = target.clone();
        menu = menu.child(menu_option(format!("sleep-{minutes}"), format!("{minutes} minutes"), false, theme).on_click(move |_, _, cx| {
            cx.stop_propagation();
            let _ = option_target.update(cx, |dock, cx| {
                dock.set_sleep(Some(SleepPlan::Until(Instant::now() + Duration::from_secs(minutes * 60))), cx);
                cx.notify();
            });
        }));
    }
    let chapter_target = target.clone();
    menu = menu.child(menu_option("sleep-chapter".to_owned(), "End of chapter".to_owned(), false, theme).on_click(move |_, _, cx| {
        cx.stop_propagation();
        let _ = chapter_target.update(cx, |dock, cx| {
            let chapter = dock.active_chapter_index(cx);
            dock.set_sleep(Some(SleepPlan::EndOfChapter(chapter)), cx);
            cx.notify();
        });
    }));
    if armed {
        menu = menu.child(menu_option("sleep-off".to_owned(), "Off".to_owned(), false, theme).on_click(move |_, _, cx| {
            cx.stop_propagation();
            let _ = target.update(cx, |dock, cx| {
                dock.set_sleep(None, cx);
                cx.notify();
            });
        }));
    }
    menu.into_any_element()
}

fn format_playback_rate(rate: f64) -> String {
    let formatted = format!("{rate:.2}");
    format!("{}×", formatted.trim_end_matches('0').trim_end_matches('.'))
}

fn transport_control(id: impl Into<SharedString>, content: impl IntoElement, theme: ui_components::BrowserTheme) -> gpui::Stateful<gpui::Div> {
    transport_control_base(id, content, theme).hover(|style| style.bg(theme.hover))
}

fn transport_control_base(id: impl Into<SharedString>, content: impl IntoElement, theme: ui_components::BrowserTheme) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex_1()
        .h(px(56.0))
        .min_w_0()
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::Hsla::transparent_black())
        .text_color(theme.text_muted)
        .font_weight(FontWeight::BOLD)
        .child(content)
}

fn chapter_icon(forward: bool, theme: ui_components::BrowserTheme) -> gpui::Div {
    let chevron = Icon::new(if forward { IconName::ChevronRight } else { IconName::ChevronLeft }).size(px(19.0));
    let bar = div().w(px(2.0)).h(px(17.0)).bg(theme.text_muted);
    let icon = div().flex().items_center();
    if forward { icon.child(chevron).child(bar) } else { icon.child(bar).child(chevron) }
}

fn skip_icon(forward: bool, seconds: &'static str) -> gpui::Div {
    div()
        .relative()
        .size(px(32.0))
        .child(Icon::new(if forward { IconName::Redo2 } else { IconName::Undo2 }).size(px(32.0)))
        .child(div().absolute().inset_0().flex().items_center().justify_center().text_size(gpui::rems(ui_components::TEXT_XS / 16.0)).child(seconds))
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
