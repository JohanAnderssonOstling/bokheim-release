use super::*;
use gpui::EventEmitter;
use gpui_component::button::ButtonVariants as _;
use gpui_component::menu::DropdownMenu as _;
use ui_components::{FlexItem as _, SPACE_SM, SPACE_XS, SPACE_XXS};
use std::rc::Rc;

pub enum DockAction {
    Expand,
    Close,
}

/// Fixed cover box for the skinny docked bar. A `w`/`h` pair rather than one
/// dimension plus the book's own aspect ratio, so the row's height — and
/// everything sized off it — never shifts with the cover a given book
/// happens to have, or is missing one to draw at all.
const SKINNY_COVER_SIZE: (f32, f32) = (48.0, 60.0);
const DESKTOP_DOCK_HEIGHT: f32 = 72.0;
const DESKTOP_COVER_WIDTH: f32 = 48.0;
const DESKTOP_CONTROL_SIZE: f32 = 44.0;
/// The docked bar's play button, centred in the bar's height with the cover and
/// the titles.
const SKINNY_PLAY_SIZE: (f32, f32) = (48.0, 44.0);
/// The close, collapse and menu buttons beside the title in the expanded
/// mobile player; the speed button is wider to fit its label.
const COMPACT_TOOL_SIZE: (f32, f32) = (36.0, 28.0);
const COMPACT_SPEED_WIDTH: f32 = 44.0;
/// Height of the transport controls in the expanded mobile player.
const MOBILE_CONTROL_HEIGHT: f32 = 56.0;
/// The progress line is drawn thin but has to be easy to hit, so its hit area
/// is taller than the line inside it.
const TRACK_HIT_HEIGHT: f32 = 16.0;
const PROGRESS_LINE_HEIGHT: f32 = 3.0;
/// How far a touch must travel sideways across the docked bar to change chapter.
const SWIPE_THRESHOLD: f32 = 40.0;

/// Anchor compact sheets to the window. The dock itself is only a short
/// footer, so using it as the sheet's containing block clips the menu.
fn page_sheet(sheet: gpui::AnyElement, window: &Window) -> gpui::AnyElement {
    let viewport = window.viewport_size();
    let usable_height = viewport.height - window.insets().safe_area.bottom;
    gpui::anchored().position(gpui::point(px(0.0), px(0.0))).anchor(gpui::Anchor::TopLeft).child(div().relative().w(viewport.width).h(usable_height).child(sheet)).into_any_element()
}

/// Compact projection of the retained playback entity. It has no decoder or
/// streaming resources of its own.
pub struct AudiobookDock {
    player: Entity<PlaybackSession>,
    snapshot: (String, Option<String>, Option<String>, bool, Option<String>, bool, Option<u64>, Option<u64>),
    speed_menu_open: bool,
    sleep_menu_open: bool,
    mobile_expanded: bool,
    /// The fraction of the current chapter the pointer/finger is dragging the
    /// position to. Present only during a drag, and what the progress bar
    /// reads while it is.
    scrub: Option<f32>,
    progress_hover: Option<f32>,
    /// Where the progress track was last laid out, so a drag that has left it
    /// can still be turned into a position.
    progress_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The distance a touch swipe on the docked bar has travelled, while one is
    /// in progress.
    swipe: Option<gpui::Point<f32>>,
}

impl AudiobookDock {
    pub fn new(player: Entity<PlaybackSession>, cx: &mut Context<Self>) -> Self {
        cx.observe(&player, |dock, player, cx| {
            let next = Self::snapshot(player.read(cx));
            if next != dock.snapshot {
                dock.snapshot = next;
                cx.notify();
            }
        })
        .detach();
        let snapshot = Self::snapshot(player.read(cx));
        Self { player, snapshot, speed_menu_open: false, sleep_menu_open: false, mobile_expanded: false, scrub: None, progress_hover: None, progress_bounds: Rc::new(Cell::new(Bounds::default())), swipe: None }
    }

    /// Collapses the mobile expanded view back to the skinny docked bar, e.g.
    /// when the book detail page it was expanded for has been navigated away
    /// from. A no-op if it was already collapsed.
    pub fn collapse_mobile(&mut self, cx: &mut Context<Self>) {
        if self.mobile_expanded {
            self.mobile_expanded = false;
            cx.notify();
        }
    }

    /// A horizontal swipe across the docked bar changes chapter when the finger
    /// lifts: leftwards to the next chapter, rightwards to the previous one.
    fn handle_swipe(&mut self, event: &gpui::ScrollWheelEvent, cx: &mut Context<Self>) {
        let gpui::ScrollDelta::Pixels(delta) = event.delta else { return };
        let delta = gpui::point(f32::from(delta.x), f32::from(delta.y));
        match event.touch_phase {
            gpui::TouchPhase::Started => self.swipe = Some(delta),
            gpui::TouchPhase::Moved => {
                if let Some(swipe) = &mut self.swipe {
                    *swipe = gpui::point(swipe.x + delta.x, swipe.y + delta.y);
                }
            }
            gpui::TouchPhase::Ended => {
                let Some(swipe) = self.swipe.take() else { return };
                if swipe.x.abs() < SWIPE_THRESHOLD || swipe.x.abs() < swipe.y.abs() {
                    return;
                }
                if swipe.x < 0.0 { self.next_chapter(cx) } else { self.previous_chapter(cx) }
            }
            gpui::TouchPhase::Cancelled => self.swipe = None,
        }
        cx.stop_propagation();
    }

    /// The fraction of the current chapter at a window x coordinate.
    fn track_fraction(&self, x: Pixels) -> Option<f32> {
        let bounds = self.progress_bounds.get();
        let width = f32::from(bounds.size.width);
        (width > 0.0).then(|| (f32::from(x - bounds.left()) / width).clamp(0.0, 1.0))
    }

    fn begin_scrub(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(fraction) = self.track_fraction(x) else { return };
        self.scrub = Some(fraction);
        self.progress_hover = Some(fraction);
        cx.notify();
    }

    /// Reports where the pointer is while it is not dragging.
    fn hover_scrub(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(fraction) = self.track_fraction(x) else { return };
        if self.scrub.is_some() {
            self.scrub = Some(fraction);
        }
        if self.progress_hover != Some(fraction) {
            self.progress_hover = Some(fraction);
            cx.notify();
        }
    }

    /// Commits the drag: the seek happens once, where the pointer was released.
    fn finish_scrub(&mut self, cx: &mut Context<Self>) {
        let Some(fraction) = self.scrub.take() else { return };
        self.player.update(cx, |player, cx| {
            if let Some(chapter) = player.book.as_ref().and_then(|book| book.chapters.get(player.active_chapter_index())) {
                let target = super::chapter_position_at_fraction(chapter.start, chapter.end, fraction);
                player.seek_to(target);
                player.ensure_source(cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    fn snapshot(player: &PlaybackSession) -> (String, Option<String>, Option<String>, bool, Option<String>, bool, Option<u64>, Option<u64>) {
        let title = player.book.as_ref().map(|book| book.title.clone()).unwrap_or_else(|| "Opening audiobook…".into());
        let subtitle = player.book.as_ref().and_then(|book| book.author.clone().or_else(|| book.narrator.clone()));
        let chapter = player.book.as_ref().and_then(|book| book.chapters.get(player.active_chapter_index())).map(|chapter| super::chapter_display_title(&chapter.title));
        let minutes_left = player.book.as_ref().filter(|book| !book.duration.is_zero()).map(|book| book.duration.saturating_sub(player.position).as_secs().div_ceil(60));
        (title, subtitle, chapter, player.transport_playing(), player.error.clone(), player.is_buffering(), player.book.as_ref().and_then(|book| book.cover.as_ref()).map(|cover| cover.id()), minutes_left)
    }
}

impl EventEmitter<DockAction> for AudiobookDock {}

/// Window-wide mouse listeners installed for the duration of a scrub, so a
/// drag that strays off the (thin) progress bar still tracks and still ends
/// the drag on release.
fn dock_scrub_capture(target: gpui::WeakEntity<AudiobookDock>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _: (), window, _| {
            let moved = target.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    let _ = moved.update(cx, |dock, cx| dock.hover_scrub(event.position.x, cx));
                }
            });
            let released = target.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    let _ = released.update(cx, |dock, cx| dock.finish_scrub(cx));
                }
            });
        },
    )
    .absolute()
    .size_full()
}

/// Each action re-renders the dock itself. Toggling playback is the exception:
/// play/pause is in the snapshot, so the player's own notification reaches the
/// dock through the observer. A seek may leave the snapshot unchanged — it
/// ignores the playback clock — so it has to notify the dock directly.
impl AudiobookDock {
    fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.request_toggle(cx));
    }
    fn seek_by(&mut self, seconds: f64, cx: &mut Context<Self>) {
        self.update_position(cx, |player| player.seek_by(seconds));
    }
    fn previous_chapter(&mut self, cx: &mut Context<Self>) {
        self.update_position(cx, PlaybackSession::previous_chapter);
    }
    fn next_chapter(&mut self, cx: &mut Context<Self>) {
        self.update_position(cx, PlaybackSession::next_chapter);
    }
    fn update_position(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut PlaybackSession)) {
        self.player.update(cx, |player, cx| {
            change(player);
            player.ensure_source(cx);
            cx.notify();
        });
        cx.notify();
    }
    pub(super) fn set_rate(&mut self, rate: f64, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| {
            player.set_rate(rate, cx);
            cx.notify();
        });
        self.speed_menu_open = false;
        cx.notify();
    }
    pub(super) fn choose_sleep(&mut self, choice: super::SleepChoice, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| {
            player.set_sleep(choice.plan(player.active_chapter_index()));
            cx.notify();
        });
        self.sleep_menu_open = false;
        cx.notify();
    }
    pub(super) fn toggle_sleep_menu(&mut self, cx: &mut Context<Self>) {
        self.sleep_menu_open = !self.sleep_menu_open;
        self.speed_menu_open = false;
        cx.notify();
    }
    pub(super) fn toggle_speed_menu(&mut self, cx: &mut Context<Self>) {
        self.speed_menu_open = !self.speed_menu_open;
        self.sleep_menu_open = false;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DockHost {
        dock: Entity<AudiobookDock>,
        expanded: usize,
        closed: usize,
    }
    impl Render for DockHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().flex().flex_col().child(div().flex_1().min_h_0().debug_selector(|| "dock-test-browsing".into()).child("Browsing stays visible")).child(self.dock.clone())
        }
    }

    #[gpui::test]
    fn rendered_dock_is_compact_and_routes_body_and_close_separately(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (sender, _) = async_channel::unbounded();
        let player = cx.new(|_| {
            let mut player = PlaybackSession::new(1.0, sender);
            player.stopped = true;
            player.book = Some(Arc::new(AudiobookBook {
                title: "A very long audiobook title that must not push the transport buttons outside the screen".into(),
                author: None,
                narrator: None,
                duration: Duration::from_secs(100),
                chapters: vec![],
                cover: None,
            }));
            player
        });
        let dock = cx.new(|cx| AudiobookDock::new(player, cx));
        let host = cx.new(|cx| {
            cx.subscribe(&dock, |host: &mut DockHost, _, action, cx| {
                match action {
                    DockAction::Expand => host.expanded += 1,
                    DockAction::Close => host.closed += 1,
                }
                cx.notify();
            })
            .detach();
            DockHost { dock, expanded: 0, closed: 0 }
        });
        let (_, cx) = cx.add_window_view(|window, cx| gpui_component::Root::new(host.clone(), window, cx));

        // Docked on a narrow window: skinny bar only, no transport controls.
        cx.simulate_resize(gpui::size(px(320.0), px(800.0)));
        cx.run_until_parked();
        let dock = cx.debug_bounds("audiobook-dock").unwrap();
        let browsing = cx.debug_bounds("dock-test-browsing").unwrap();
        let skinny_height = dock.size.height;
        let progress = cx.debug_bounds("audiobook-dock-progress").unwrap();
        assert!(progress.top() <= dock.top() + px(2.0), "compact progress must sit on the dock's top edge");
        assert!(skinny_height < px(100.0), "docked bar must be skinny (was {:?})", skinny_height);
        assert!(browsing.size.height > px(600.0));
        assert!(browsing.bottom() <= dock.top());
        assert!(cx.debug_bounds("dock-seek-back").is_none(), "transport controls must not show while docked");

        // Tapping the docked bar expands it into the full mobile player.
        let expand = cx.debug_bounds("audiobook-dock-expand").unwrap();
        cx.simulate_click(expand.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        let dock = cx.debug_bounds("audiobook-dock").unwrap();
        let progress = cx.debug_bounds("audiobook-dock-progress").unwrap();
        assert!(progress.top() <= dock.top() + px(2.0), "expanded progress must sit on the dock's top edge");
        let progress_meta = cx.debug_bounds("audiobook-dock-progress-meta").unwrap();
        let collapse = cx.debug_bounds("audiobook-dock-collapse").unwrap();
        assert!(progress_meta.bottom() <= collapse.top(), "expanded progress text must sit above the title and controls");
        assert!(dock.size.height > skinny_height, "expanded mobile player must be taller than the skinny dock (was {:?})", dock.size.height);
        for selector in ["dock-play", "dock-seek-back", "dock-seek-forward"] {
            let control = cx.debug_bounds(selector).unwrap_or_else(|| panic!("missing bounds for {selector} while expanded"));
            assert!(control.left() >= dock.left() && control.right() <= dock.right());
        }

        let speed = cx.debug_bounds("dock-playback-rate").unwrap();
        cx.simulate_click(speed.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(host.read_with(cx, |host, cx| host.dock.read(cx).speed_menu_open));
        let sheet = cx.debug_bounds("playback-speed-sheet").unwrap();
        assert!(sheet.top() < dock.top(), "speed sheet should extend into the page above the dock");
        assert!(sheet.size.height > dock.size.height, "speed sheet should use page height, not dock height");
        cx.simulate_click(gpui::point(px(10.0), px(10.0)), gpui::Modifiers::none());
        cx.run_until_parked();

        // The chevron collapses it back to the skinny bar.
        let collapse = cx.debug_bounds("audiobook-dock-collapse").unwrap();
        cx.simulate_click(collapse.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        let dock = cx.debug_bounds("audiobook-dock").unwrap();
        assert!(dock.size.height <= skinny_height, "docked bar must collapse back to skinny");

        // Wide windows always show the full desktop dock with a close button.
        for width in [600.0, 1200.0] {
            cx.simulate_resize(gpui::size(px(width), px(800.0)));
            cx.run_until_parked();
            let dock = cx.debug_bounds("audiobook-dock").unwrap();
            let progress = cx.debug_bounds("audiobook-dock-progress").unwrap();
            assert!(progress.top() <= dock.top() + px(2.0), "desktop progress must sit on the dock's top edge");
            let progress_meta = cx.debug_bounds("audiobook-dock-progress-meta").unwrap();
            let play = cx.debug_bounds("dock-play").unwrap();
            assert!(progress_meta.bottom() <= play.top(), "desktop progress text must sit above the title and controls");
            let browsing = cx.debug_bounds("dock-test-browsing").unwrap();
            assert!(dock.size.height <= px(DESKTOP_DOCK_HEIGHT + 2.0), "desktop dock must stay at cover height (was {:?})", dock.size.height);
            assert!(browsing.size.height > px(500.0));
            assert!(browsing.bottom() <= dock.top());
            for selector in ["dock-play", "dock-seek-back", "dock-seek-forward", "dock-close"] {
                let control = cx.debug_bounds(selector).unwrap_or_else(|| panic!("missing bounds for {selector} at width {width}"));
                assert!(control.left() >= dock.left() && control.right() <= dock.right());
            }
        }
        let close = cx.debug_bounds("dock-close").unwrap();
        cx.simulate_click(close.center(), gpui::Modifiers::none());
        host.read_with(cx, |host, _| {
            assert_eq!(host.expanded, 1);
            assert_eq!(host.closed, 1);
        });
    }

    #[test]
    fn snapshot_tracks_cover_but_not_playback_clock() {
        let (sender, _) = async_channel::unbounded();
        let mut player = PlaybackSession::new(1.0, sender);
        player.stopped = true;
        player.book = Some(Arc::new(AudiobookBook { title: "Book".into(), author: None, narrator: None, duration: Duration::from_secs(100), chapters: vec![], cover: None }));
        let initial = AudiobookDock::snapshot(&player);
        player.position = Duration::from_secs(10);
        assert_eq!(initial, AudiobookDock::snapshot(&player));
        player.position = Duration::from_secs(40);
        assert_ne!(initial, AudiobookDock::snapshot(&player));
        player.position = Duration::from_secs(10);
        Arc::make_mut(player.book.as_mut().unwrap()).cover = Some(Arc::new(Image::from_bytes(ImageFormat::Jpeg, vec![0xff, 0xd8, 0xff])));
        assert_ne!(initial, AudiobookDock::snapshot(&player));
    }

    #[gpui::test]
    fn clock_notifications_do_not_invalidate_dock_but_metadata_changes_do(cx: &mut gpui::TestAppContext) {
        let (sender, _) = async_channel::unbounded();
        let player = cx.new(|_| {
            let mut player = PlaybackSession::new(1.0, sender);
            player.stopped = true;
            player.book = Some(Arc::new(AudiobookBook { title: "Book".into(), author: None, narrator: None, duration: Duration::from_secs(100), chapters: vec![], cover: None }));
            player
        });
        let dock = cx.new(|cx| AudiobookDock::new(player.clone(), cx));
        let notifications = Rc::new(Cell::new(0));
        let recorded = notifications.clone();
        let _observer = cx.new(|cx| {
            cx.observe(&dock, move |_: &mut (), _, _| recorded.set(recorded.get() + 1)).detach();
        });
        cx.run_until_parked();
        let before = notifications.get();
        for second in 1..=20 {
            player.update(cx, |player, cx| {
                player.position = Duration::from_secs(second);
                cx.notify();
            });
            cx.run_until_parked();
        }
        assert_eq!(notifications.get(), before);
        player.update(cx, |player, cx| {
            Arc::make_mut(player.book.as_mut().unwrap()).title = "Updated metadata".into();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(notifications.get(), before + 1);
    }
}


/// The chapter's progress as a thin line.
fn progress_line(fraction: f32, theme: ui_components::BrowserTheme) -> gpui::Div {
    ui_components::progress_track(theme).h(px(PROGRESS_LINE_HEIGHT)).child(ui_components::progress_fill(fraction, theme))
}

impl Render for AudiobookDock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ui_components::browser_theme(cx);
        let player = self.player.read(cx);
        let cover = player.book.as_ref().and_then(|book| book.cover.clone());
        let compact = ui_components::WindowWidthClass::for_window(window).is_compact();
        let target = cx.entity().downgrade();

        let (chapter_start, chapter_position, chapter_duration) = player
            .book
            .as_ref()
            .and_then(|book| book.chapters.get(player.active_chapter_index()))
            .map(|chapter| {
                let duration = chapter.end.saturating_sub(chapter.start);
                (chapter.start, player.position.saturating_sub(chapter.start).min(duration), duration)
            })
            .unwrap_or_default();
        let progress_fraction = if chapter_duration.is_zero() { 0.0 } else { (chapter_position.as_secs_f64() / chapter_duration.as_secs_f64()) as f32 };
        let current_chapter = self.snapshot.2.clone().unwrap_or_else(|| "No chapter".into());
        // While scrubbing (dragging, or the finger/pointer resting after a
        // press), the bar and the time readout follow the drag instead of
        // live playback, so what the digits say matches where the fill is.
        let display_fraction = self.progress_hover.unwrap_or(progress_fraction);
        let display_position = self.progress_hover.map(|fraction| super::chapter_position_at_fraction(Duration::ZERO, chapter_duration, fraction)).unwrap_or(chapter_position);
        let display_book_position = if self.progress_hover.is_some() { chapter_start.saturating_add(display_position) } else { player.position };
        let book_remaining = player.book.as_ref().filter(|book| !book.duration.is_zero()).map(|book| super::format_time_left(book.duration.saturating_sub(display_book_position)));
        let chapter_time = format!("{} / {}", super::format_time(display_position), super::format_time(chapter_duration));

        if compact && !self.mobile_expanded {
            let chapter_line = ui_components::row(SPACE_SM).min_w_0().child(ui_components::muted_line(current_chapter, theme).fill()).children(book_remaining.map(|left| ui_components::muted_line(left, theme).flex_none()));
            let summary = ui_components::column(SPACE_XXS)
                .id("audiobook-dock-expand")
                .debug_selector(|| "audiobook-dock-expand".into())
                .fill()
                .cursor_pointer()
                .on_click(cx.listener(|dock, _, _, cx| {
                    dock.mobile_expanded = true;
                    cx.emit(DockAction::Expand);
                    cx.notify();
                }))
                .child(ui_components::single_line(self.snapshot.0.clone()))
                .child(chapter_line);
            let play = ui_components::outlined_icon_button("dock-play", if self.snapshot.3 { "Pause" } else { "Play" }, Icon::new(if self.snapshot.3 { IconName::Pause } else { IconName::Play }).size(px(22.0)), theme)
                .fixed(SKINNY_PLAY_SIZE.0, SKINNY_PLAY_SIZE.1)
                .debug_selector(|| "dock-play".into())
                .on_click(cx.listener(|dock, _, _, cx| dock.toggle_playback(cx)));
            let cover = div().fixed(SKINNY_COVER_SIZE.0, SKINNY_COVER_SIZE.1).children(cover.map(|cover| ui_components::book_cover_image("audiobook-dock-cover", cover, 1.0, false, theme)));
            // The cover's height is the whole docked bar.
            return ui_components::bottom_bar(theme)
                .id("audiobook-dock")
                .debug_selector(|| "audiobook-dock".into())
                .occlude()
                .relative()
                .h(px(SKINNY_COVER_SIZE.1))
                .on_scroll_wheel(cx.listener(|dock, event, _, cx| dock.handle_swipe(event, cx)))
                .child(ui_components::row(SPACE_SM).fill().h_full().pr(px(SPACE_SM)).child(cover).child(summary).child(play))
                .child(ui_components::top_edge().debug_selector(|| "audiobook-dock-progress".into()).child(progress_line(progress_fraction, theme)))
                .into_any_element();
        }

        let progress_bounds = self.progress_bounds.clone();
        // The line is drawn thin, inside a taller hit area.
        let scrub_track = ui_components::top_edge()
            .id("audiobook-dock-progress")
            .debug_selector(|| "audiobook-dock-progress".into())
            .h(px(TRACK_HIT_HEIGHT))
            .cursor_pointer()
            .child(progress_line(display_fraction, theme))
            .on_prepaint(move |bounds, _, _| progress_bounds.set(bounds))
            // Pressing starts a scrub rather than seeking outright, so a
            // press-and-drag gesture is one action: the seek happens once,
            // wherever the finger/pointer has got to when it lifts.
            .on_mouse_down(MouseButton::Left, cx.listener(|dock, event: &gpui::MouseDownEvent, _, cx| dock.begin_scrub(event.position.x, cx)))
            .on_mouse_move(cx.listener(|dock, event: &MouseMoveEvent, _, cx| dock.hover_scrub(event.position.x, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|dock, _, _, cx| dock.finish_scrub(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|dock, _, _, cx| dock.finish_scrub(cx)))
            .on_hover(cx.listener(|dock, hovered: &bool, _, cx| {
                if !*hovered && dock.scrub.is_none() && dock.progress_hover.take().is_some() {
                    cx.notify();
                }
            }));
        let sleep_label = player.sleep_label();
        let rate = player.rate;
        let playing = self.snapshot.3;

        // The controls both expanded layouts share; each only sizes them.
        let transport = [
            ui_components::base_button("dock-previous-chapter")
                .ghost()
                .tooltip("Previous chapter")
                .child(ui_components::audiobook_chapter_icon(false, theme))
                .debug_selector(|| "dock-previous-chapter".into())
                .on_click(cx.listener(|dock, _, _, cx| dock.previous_chapter(cx))),
            ui_components::outlined_icon_button("dock-seek-back", "Back 15 seconds", Icon::new(IconName::Undo2).size(px(28.0)), theme)
                .debug_selector(|| "dock-seek-back".into())
                .on_click(cx.listener(|dock, _, _, cx| dock.seek_by(super::SKIP_BACK_SECONDS, cx))),
            ui_components::selectable_button(ui_components::outlined_icon_button("dock-play", if playing { "Pause" } else { "Play" }, Icon::new(if playing { IconName::Pause } else { IconName::Play }).size(px(22.0)), theme), true, theme)
                .debug_selector(|| "dock-play".into())
                .on_click(cx.listener(|dock, _, _, cx| dock.toggle_playback(cx))),
            ui_components::outlined_icon_button("dock-seek-forward", "Forward 30 seconds", Icon::new(IconName::Redo2).size(px(28.0)), theme)
                .debug_selector(|| "dock-seek-forward".into())
                .on_click(cx.listener(|dock, _, _, cx| dock.seek_by(super::SKIP_FORWARD_SECONDS, cx))),
            ui_components::base_button("dock-next-chapter")
                .ghost()
                .tooltip("Next chapter")
                .child(ui_components::audiobook_chapter_icon(true, theme))
                .debug_selector(|| "dock-next-chapter".into())
                .on_click(cx.listener(|dock, _, _, cx| dock.next_chapter(cx))),
        ];
        let close = ui_components::outlined_icon_button("dock-close", "Close player", Icon::new(IconName::Close).size(px(18.0)), theme).debug_selector(|| "dock-close".into()).on_click(cx.listener(|_, _, _, cx| cx.emit(DockAction::Close)));
        // The alarm glyph, or the time left once a timer is running.
        let sleep = match sleep_label.clone() {
            Some(label) => ui_components::base_button("dock-sleep-timer").ghost().label(label).tooltip("Sleep timer"),
            None => ui_components::outlined_icon_button("dock-sleep-timer", "Sleep timer", Icon::empty().path("icons/alarm.svg").size(px(18.0)), theme),
        };
        let sleep = ui_components::selectable_button(sleep, self.sleep_menu_open || sleep_label.is_some(), theme).debug_selector(|| "dock-sleep-timer".into());
        let speed = ui_components::base_button("dock-playback-rate").ghost().label(super::format_playback_rate(rate)).tooltip("Playback speed");
        let speed = ui_components::selectable_button(speed, self.speed_menu_open, theme).debug_selector(|| "dock-playback-rate".into());

        if compact {
            let titles = ui_components::column(0.0)
                .id("audiobook-dock-title")
                .fill()
                .cursor_pointer()
                .on_click(cx.listener(|dock, _, _, cx| dock.collapse_mobile(cx)))
                .child(ui_components::single_line(self.snapshot.0.clone()))
                .child(ui_components::caption_line(current_chapter, theme));
            let collapse = ui_components::outlined_icon_button("dock-collapse", "Collapse player", Icon::new(IconName::ChevronDown).size(px(18.0)), theme)
                .debug_selector(|| "audiobook-dock-collapse".into())
                .fixed(COMPACT_TOOL_SIZE.0, COMPACT_TOOL_SIZE.1)
                .on_click(cx.listener(|dock, _, _, cx| dock.collapse_mobile(cx)));
            let top_row = ui_components::row(SPACE_XS)
                .child(collapse)
                .child(titles)
                .child(sleep.fixed(COMPACT_TOOL_SIZE.0, COMPACT_TOOL_SIZE.1).on_click(cx.listener(|dock, _, _, cx| dock.toggle_sleep_menu(cx))))
                .child(speed.fixed(COMPACT_SPEED_WIDTH, COMPACT_TOOL_SIZE.1).on_click(cx.listener(|dock, _, _, cx| dock.toggle_speed_menu(cx))))
                .child(close.fixed(COMPACT_TOOL_SIZE.0, COMPACT_TOOL_SIZE.1));

            // The menus are rendered in a window-sized anchored layer.
            let sleep_sheet = self.sleep_menu_open.then(|| gpui::deferred(page_sheet(super::sleep_sheet(target.clone(), sleep_label, theme), window)).with_priority(1));
            let speed_sheet = self.speed_menu_open.then(|| gpui::deferred(page_sheet(super::playback_speed_sheet(target.clone(), rate, theme), window)).with_priority(1));

            let progress_meta = ui_components::spread_row(SPACE_SM)
                .debug_selector(|| "audiobook-dock-progress-meta".into())
                .child(ui_components::caption_line(chapter_time, theme).flex_none())
                .children(book_remaining.map(|left| ui_components::caption_line(left, theme).text_right()));
            let controls = ui_components::row(0.0).children(transport.map(|control| control.fill().h(px(MOBILE_CONTROL_HEIGHT))));

            return ui_components::bottom_bar(theme)
                .id("audiobook-dock")
                .debug_selector(|| "audiobook-dock".into())
                .occlude()
                .relative()
                .px(px(SPACE_SM))
                .pt(px(TRACK_HIT_HEIGHT))
                .pb(px(SPACE_XS))
                .child(ui_components::column(SPACE_XXS).fill().child(progress_meta).child(top_row).child(controls))
                .child(scrub_track)
                .children(sleep_sheet)
                .children(speed_sheet)
                .when(self.scrub.is_some(), |dock| dock.child(dock_scrub_capture(target.clone())))
                .into_any_element();
        }

        let book = ui_components::column(0.0)
            .id("audiobook-dock-expand")
            .debug_selector(|| "audiobook-dock-expand".into())
            .fill()
            .cursor_pointer()
            .on_click(cx.listener(|_, _, _, cx| cx.emit(DockAction::Expand)))
            .child(ui_components::single_line(self.snapshot.0.clone()))
            .children(self.snapshot.1.clone().map(|subtitle| ui_components::muted_line(subtitle, theme)))
            .child(ui_components::caption_line(current_chapter, theme));
        let book_area = ui_components::row(SPACE_SM)
            .fill()
            .h_full()
            .children(cover.map(|cover| div().fixed(DESKTOP_COVER_WIDTH, DESKTOP_DOCK_HEIGHT).child(ui_components::book_cover_image("audiobook-dock-cover", cover, 1.0, false, theme))))
            .child(book);

        let transport = ui_components::column(SPACE_XXS)
            .flex_none()
            .items_center()
            .child(ui_components::caption_line(chapter_time, theme).debug_selector(|| "audiobook-dock-progress-meta".into()))
            .child(ui_components::row(SPACE_XXS).children(transport.map(|control| control.fixed(DESKTOP_CONTROL_SIZE, DESKTOP_CONTROL_SIZE))));

        let sleep_target = target.clone();
        let sleep = sleep.fixed(DESKTOP_CONTROL_SIZE, DESKTOP_CONTROL_SIZE).dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| super::sleep_items(menu, sleep_target.clone(), sleep_label.clone()));
        let speed_target = target.clone();
        let speed = speed.fixed(DESKTOP_CONTROL_SIZE, DESKTOP_CONTROL_SIZE).dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |menu, _, _| super::playback_speed_items(menu, speed_target.clone(), rate));
        // A matched-width counterpart to `book_area`: both sides fill, so the
        // fixed-width transport between them lands in the header's true
        // center rather than wherever the book area's content leaves off.
        let tools = ui_components::column(SPACE_XXS)
            .fill()
            .items_end()
            .children(book_remaining.map(|left| ui_components::caption_line(left, theme)))
            .child(ui_components::row(SPACE_SM).child(ui_components::row(SPACE_XS).child(sleep).child(speed)).child(close.fixed(COMPACT_TOOL_SIZE.0, DESKTOP_CONTROL_SIZE)));

        ui_components::bottom_bar(theme)
            .id("audiobook-dock")
            .debug_selector(|| "audiobook-dock".into())
            .occlude()
            .relative()
            .child(ui_components::row(SPACE_SM).fill().h(px(DESKTOP_DOCK_HEIGHT)).pr(px(SPACE_SM)).child(book_area).child(transport).child(tools))
            .child(scrub_track)
            .when(self.scrub.is_some(), |dock| dock.child(dock_scrub_capture(target.clone())))
            .into_any_element()
    }
}
