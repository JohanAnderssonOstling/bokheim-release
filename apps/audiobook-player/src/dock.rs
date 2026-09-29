use super::*;
use gpui::EventEmitter;

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
/// How far a touch must travel sideways across the docked bar to change chapter.
const SWIPE_THRESHOLD: f32 = 40.0;

/// Anchor compact sheets to the window. The dock itself is only a short
/// footer, so using it as the sheet's containing block clips the menu.
fn page_sheet(sheet: gpui::AnyElement, window: &Window) -> gpui::AnyElement {
    let viewport = window.viewport_size();
    let usable_height = viewport.height - window.insets().safe_area.bottom;
    gpui::anchored()
        .position(gpui::point(px(0.0), px(0.0)))
        .anchor(gpui::Anchor::TopLeft)
        .child(div().relative().w(viewport.width).h(usable_height).child(sheet))
        .into_any_element()
}

fn mobile_cover_slot(cover: Option<Arc<Image>>, size: (f32, f32), theme: ui_components::BrowserTheme) -> gpui::Div {
    let slot = div().w(px(size.0)).h(px(size.1)).flex_none().overflow_hidden().bg(theme.rule);
    match cover {
        Some(cover) => slot.child(img(cover).size_full().object_fit(ObjectFit::Cover)),
        None => slot,
    }
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
                if swipe.x.abs() < SWIPE_THRESHOLD || swipe.x.abs() < swipe.y.abs() { return; }
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

impl super::PlaybackTarget for AudiobookDock {
    fn toggle_playback(&mut self, cx: &mut gpui::App) {
        self.player.update(cx, |player, cx| player.request_toggle(cx));
    }
    fn seek_by(&mut self, seconds: f64, cx: &mut gpui::App) {
        self.player.update(cx, |player, cx| {
            player.seek_by(seconds);
            player.ensure_source(cx);
            cx.notify();
        });
    }
    fn previous_chapter(&mut self, cx: &mut gpui::App) {
        self.player.update(cx, |player, cx| {
            player.previous_chapter();
            player.ensure_source(cx);
            cx.notify();
        });
    }
    fn next_chapter(&mut self, cx: &mut gpui::App) {
        self.player.update(cx, |player, cx| {
            player.next_chapter();
            player.ensure_source(cx);
            cx.notify();
        });
    }
    fn set_rate(&mut self, rate: f64, cx: &mut gpui::App) {
        self.player.update(cx, |player, cx| {
            player.set_rate(rate, cx);
            cx.notify();
        });
        self.speed_menu_open = false;
    }
    fn set_sleep(&mut self, plan: Option<super::SleepPlan>, cx: &mut gpui::App) {
        self.player.update(cx, |player, cx| {
            player.set_sleep(plan);
            cx.notify();
        });
        self.sleep_menu_open = false;
    }
    fn toggle_sleep_menu(&mut self) {
        self.sleep_menu_open = !self.sleep_menu_open;
        self.speed_menu_open = false;
    }
    fn toggle_speed_menu(&mut self) {
        self.speed_menu_open = !self.speed_menu_open;
        self.sleep_menu_open = false;
    }
    fn active_chapter_index(&self, cx: &gpui::App) -> usize {
        self.player.read(cx).active_chapter_index()
    }
    fn sleep_label(&self, cx: &gpui::App) -> Option<gpui::SharedString> {
        self.player.read(cx).sleep_label()
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

impl Render for AudiobookDock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ui_components::browser_theme(cx);
        let player = self.player.read(cx);
        let cover = player.book.as_ref().and_then(|book| book.cover.clone());
        let layout = super::MenuLayout::for_window(window);
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
        let book_remaining = player.book.as_ref().filter(|book| !book.duration.is_zero()).map(|book| {
            super::format_time_left(book.duration.saturating_sub(display_book_position))
        });
        let progress_bounds = self.progress_bounds.clone();
        let pressed_target = target.clone();
        let moved_target = target.clone();
        let released_target = target.clone();
        let released_outside_target = target.clone();
        let hover_ended_target = target.clone();
        let progress_track = div()
            .id("audiobook-dock-progress")
            .debug_selector(|| "audiobook-dock-progress".into())
            .absolute()
            .left_0()
            .right_0()
            .top_0()
            .min_w_0()
            .h(px(super::TRACK_HIT_HEIGHT))
            .cursor_pointer()
            .flex()
            .items_start()
            .on_prepaint(move |bounds, _, _| progress_bounds.set(bounds))
            // Pressing starts a scrub rather than seeking outright, so a
            // press-and-drag gesture is one action: the seek happens once,
            // wherever the finger/pointer has got to when it lifts.
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let _ = pressed_target.update(cx, |dock, cx| dock.begin_scrub(event.position.x, cx));
            })
            .on_mouse_move(move |event, _, cx| {
                let _ = moved_target.update(cx, |dock, cx| dock.hover_scrub(event.position.x, cx));
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                let _ = released_target.update(cx, |dock, cx| dock.finish_scrub(cx));
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                let _ = released_outside_target.update(cx, |dock, cx| dock.finish_scrub(cx));
            })
            .on_hover(move |hovered, _, cx| {
                if !hovered {
                    let _ = hover_ended_target.update(cx, |dock, cx| {
                        if dock.scrub.is_none() && dock.progress_hover.take().is_some() {
                            cx.notify();
                        }
                    });
                }
            })
            .child(div().relative().w_full().h(px(3.0)).bg(theme.rule).child(div().absolute().left_0().top_0().h_full().w(gpui::relative(display_fraction)).bg(theme.accent)));

        if layout.compact && !self.mobile_expanded {
            let title = self.snapshot.0.clone();
            let chapter = self.snapshot.2.clone().unwrap_or_else(|| "No chapter".into());
            let mobile_cover = mobile_cover_slot(cover.clone(), SKINNY_COVER_SIZE, theme);
            let title_line = div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title);
            let chapter_line = div()
                .min_w_0()
                .flex()
                .gap(px(8.0))
                .text_color(theme.text_muted)
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(chapter))
                .children(book_remaining.clone().map(|left| div().flex_none().child(left)));
            // The edge line is positioned so it takes no height from the bar.
            let skinny_track = div()
                .id("audiobook-dock-progress")
                .debug_selector(|| "audiobook-dock-progress".into())
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px(3.0))
                .bg(theme.rule)
                .child(div().absolute().left_0().top_0().h_full().w(gpui::relative(progress_fraction)).bg(theme.accent));
            // The cover's height remains the whole docked bar.
            let mobile_content = div()
                .id("audiobook-dock-expand")
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(2.0))
                .cursor_pointer()
                .debug_selector(|| "audiobook-dock-expand".into())
                .on_click(cx.listener(|dock, _, _, cx| {
                    dock.mobile_expanded = true;
                    cx.emit(DockAction::Expand);
                    cx.notify();
                }))
                .child(title_line)
                .child(chapter_line);
            let mobile_play_pause = super::transport_control_base("dock-play", Icon::new(if self.snapshot.3 { IconName::Pause } else { IconName::Play }).size(px(22.0)), theme)
                .w(px(SKINNY_PLAY_SIZE.0))
                .h(px(SKINNY_PLAY_SIZE.1))
                .flex_none()
                .on_click(cx.listener(|dock, _, _, cx| dock.player.update(cx, |player, cx| player.request_toggle(cx))));
            let beside_cover = div().relative().flex_1().min_w_0().h_full().flex().items_center().gap(px(8.0)).child(mobile_content).child(mobile_play_pause);
            return div()
                .id("audiobook-dock")
                .debug_selector(|| "audiobook-dock".into())
                .occlude()
                .relative()
                .w_full()
                .flex_none()
                .flex()
                .items_center()
                .h(px(SKINNY_COVER_SIZE.1))
                .gap(px(8.0))
                .pr(px(8.0))
                .border_t_1()
                .border_color(theme.rule)
                .bg(theme.page_bg)
                .text_color(theme.text)
                .on_scroll_wheel(cx.listener(|dock, event, _, cx| dock.handle_swipe(event, cx)))
                .child(mobile_cover)
                .child(beside_cover)
                .child(skinny_track)
                .into_any_element();
        }
        if layout.compact {
            let title = self.snapshot.0.clone();
            let title_line = div()
                .id("audiobook-dock-title")
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(|dock, _, _, cx| {
                    dock.mobile_expanded = false;
                    cx.notify();
                }))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(gpui::rems(ui_components::TEXT_XS)).text_color(theme.text_muted).child(current_chapter.clone()));
            let collapse = super::transport_control("dock-collapse", Icon::new(IconName::ChevronDown).size(px(18.0)), theme)
                .h(px(28.0))
                .w(px(36.0))
                .flex_none()
                .debug_selector(|| "audiobook-dock-collapse".into())
                .on_click(cx.listener(|dock, _, _, cx| {
                    dock.mobile_expanded = false;
                    cx.notify();
                }));
            let close = super::transport_control("dock-close", Icon::new(IconName::Close).size(px(18.0)), theme)
                .h(px(28.0))
                .w(px(36.0))
                .flex_none()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(DockAction::Close)));

            // The menus are rendered in a window-sized anchored layer below.
            let sleep_target = target.clone();
            let sleep_label = player.sleep_label();
            let sleep_content = sleep_label.clone().map(|label| label.into_any_element()).unwrap_or_else(|| Icon::empty().path("icons/alarm.svg").size(px(18.0)).into_any_element());
            let mobile_sleep = super::menu_trigger("dock-sleep-timer", sleep_content, self.sleep_menu_open || sleep_label.is_some(), theme).h(px(28.0)).w(px(36.0)).flex_none().on_click(move |_, _, cx| {
                let _ = sleep_target.update(cx, |dock, cx| {
                    dock.toggle_sleep_menu();
                    cx.notify();
                });
            });
            let speed_target = target.clone();
            let mobile_speed = super::menu_trigger("dock-playback-rate", super::format_playback_rate(player.rate), self.speed_menu_open, theme).h(px(28.0)).w(px(44.0)).flex_none().debug_selector(|| "dock-playback-rate".into()).on_click(
                move |_, _, cx| {
                    let _ = speed_target.update(cx, |dock, cx| {
                        dock.toggle_speed_menu();
                        cx.notify();
                    });
                },
            );
            let top_row = div().w_full().flex().items_center().gap(px(4.0)).child(collapse).child(title_line).child(mobile_sleep).child(mobile_speed).child(close);
            let sleep_popover = self.sleep_menu_open.then(|| gpui::deferred(page_sheet(super::sleep_menu(target.clone(), sleep_label, layout, theme), window)).with_priority(1));
            let speed_popover = self.speed_menu_open.then(|| gpui::deferred(page_sheet(super::playback_speed_menu(target.clone(), player.rate, layout, theme), window)).with_priority(1));

            let mobile_progress_meta = div()
                .debug_selector(|| "audiobook-dock-progress-meta".into())
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .text_size(gpui::rems(ui_components::TEXT_XS))
                .text_color(theme.text_muted)
                .child(format!("{} / {}", super::format_time(display_position), super::format_time(chapter_duration)))
                .children(book_remaining.clone().map(|left| div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_right().child(left)));

            let previous_target = target.clone();
            let back_target = target.clone();
            let play_target = target.clone();
            let forward_target = target.clone();
            let next_target = target.clone();
            let mobile_controls = div()
                .w_full()
                .flex()
                .items_center()
                .child(super::transport_control("dock-previous-chapter", super::chapter_icon(false, theme), theme).on_click(move |_, _, cx| {
                    let _ = previous_target.update(cx, |dock, cx| {
                        dock.previous_chapter(cx);
                        cx.notify();
                    });
                }))
                .child(super::transport_control("dock-seek-back", super::skip_icon(false, "15"), theme).on_click(move |_, _, cx| {
                    let _ = back_target.update(cx, |dock, cx| {
                        dock.seek_by(super::SKIP_BACK_SECONDS, cx);
                        cx.notify();
                    });
                }))
                .child(
                    super::transport_control_base("dock-play", Icon::new(if self.snapshot.3 { IconName::Pause } else { IconName::Play }).size(px(22.0)), theme)
                        .bg(theme.accent)
                        .text_color(theme.accent_text)
                        .hover(|style| style.bg(theme.accent_hover))
                        .on_click(move |_, _, cx| {
                            let _ = play_target.update(cx, |dock, cx| {
                                dock.toggle_playback(cx);
                                cx.notify();
                            });
                        }),
                )
                .child(super::transport_control("dock-seek-forward", super::skip_icon(true, "30"), theme).on_click(move |_, _, cx| {
                    let _ = forward_target.update(cx, |dock, cx| {
                        dock.seek_by(super::SKIP_FORWARD_SECONDS, cx);
                        cx.notify();
                    });
                }))
                .child(super::transport_control("dock-next-chapter", super::chapter_icon(true, theme), theme).on_click(move |_, _, cx| {
                    let _ = next_target.update(cx, |dock, cx| {
                        dock.next_chapter(cx);
                        cx.notify();
                    });
                }));

            let content_column = div().min_w_0().flex_1().flex().flex_col().justify_center().gap(px(2.0)).child(mobile_progress_meta).child(top_row).child(mobile_controls);

            return div()
                .id("audiobook-dock")
                .occlude()
                .relative()
                .w_full()
                .flex_none()
                .flex()
                .items_stretch()
                .px(px(8.0))
                .pt(px(super::TRACK_HIT_HEIGHT))
                .pb(px(6.0))
                .debug_selector(|| "audiobook-dock".into())
                .border_t_1()
                .border_color(theme.rule)
                .bg(theme.page_bg)
                .text_color(theme.text)
                .child(content_column)
                .child(progress_track)
                .children(sleep_popover)
                .children(speed_popover)
                .when(self.scrub.is_some(), |dock| dock.child(dock_scrub_capture(target.clone())))
                .into_any_element();
        }
        let desktop_cover = cover.map(|cover| img(cover).w(px(DESKTOP_COVER_WIDTH)).h(px(DESKTOP_DOCK_HEIGHT)).flex_none().object_fit(ObjectFit::Cover).into_any_element());
        let desktop_title = div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(self.snapshot.0.clone());
        let desktop_subtitle = self.snapshot.1.clone().map(|subtitle| div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(theme.text_muted).child(subtitle));
        let desktop_book = div()
            .id("audiobook-dock-expand")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .justify_center()
            .cursor_pointer()
            .debug_selector(|| "audiobook-dock-expand".into())
            .on_click(cx.listener(|_, _, _, cx| cx.emit(DockAction::Expand)))
            .child(desktop_title)
            .children(desktop_subtitle)
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(gpui::rems(ui_components::TEXT_XS)).text_color(theme.text_muted).child(current_chapter));
        let desktop_book_area = div().flex_1().min_w_0().h_full().flex().items_center().gap(px(8.0)).children(desktop_cover).child(desktop_book);

        let previous_target = target.clone();
        let back_target = target.clone();
        let play_target = target.clone();
        let forward_target = target.clone();
        let next_target = target.clone();
        let desktop_control_row = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(super::transport_control("dock-previous-chapter", super::chapter_icon(false, theme), theme).w(px(DESKTOP_CONTROL_SIZE)).h(px(DESKTOP_CONTROL_SIZE)).flex_none().mx(px(1.0)).on_click(move |_, _, cx| {
                let _ = previous_target.update(cx, |dock, cx| {
                    dock.previous_chapter(cx);
                    cx.notify();
                });
            }))
            .child(super::transport_control("dock-seek-back", super::skip_icon(false, "15"), theme).w(px(DESKTOP_CONTROL_SIZE)).h(px(DESKTOP_CONTROL_SIZE)).flex_none().mx(px(1.0)).on_click(move |_, _, cx| {
                let _ = back_target.update(cx, |dock, cx| {
                    dock.seek_by(super::SKIP_BACK_SECONDS, cx);
                    cx.notify();
                });
            }))
            .child(
                super::transport_control_base("dock-play", Icon::new(if self.snapshot.3 { IconName::Pause } else { IconName::Play }).size(px(22.0)), theme)
                    .w(px(DESKTOP_CONTROL_SIZE))
                    .h(px(DESKTOP_CONTROL_SIZE))
                    .flex_none()
                    .mx(px(1.0))
                    .bg(theme.accent)
                    .text_color(theme.accent_text)
                    .hover(|style| style.bg(theme.accent_hover))
                    .on_click(move |_, _, cx| {
                        let _ = play_target.update(cx, |dock, cx| {
                            dock.toggle_playback(cx);
                            cx.notify();
                        });
                    }),
            )
            .child(super::transport_control("dock-seek-forward", super::skip_icon(true, "30"), theme).w(px(DESKTOP_CONTROL_SIZE)).h(px(DESKTOP_CONTROL_SIZE)).flex_none().mx(px(1.0)).on_click(move |_, _, cx| {
                let _ = forward_target.update(cx, |dock, cx| {
                    dock.seek_by(super::SKIP_FORWARD_SECONDS, cx);
                    cx.notify();
                });
            }))
            .child(super::transport_control("dock-next-chapter", super::chapter_icon(true, theme), theme).w(px(DESKTOP_CONTROL_SIZE)).h(px(DESKTOP_CONTROL_SIZE)).flex_none().mx(px(1.0)).on_click(move |_, _, cx| {
                let _ = next_target.update(cx, |dock, cx| {
                    dock.next_chapter(cx);
                    cx.notify();
                });
            }));
        let desktop_controls = div()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(2.0))
            .child(div().debug_selector(|| "audiobook-dock-progress-meta".into()).text_size(gpui::rems(ui_components::TEXT_XS)).text_color(theme.text_muted).child(format!("{} / {}", super::format_time(display_position), super::format_time(chapter_duration))))
            .child(desktop_control_row);

        let sleep_target = target.clone();
        let sleep_label = player.sleep_label();
        let sleep_content = sleep_label.clone().map(|label| label.into_any_element()).unwrap_or_else(|| Icon::empty().path("icons/alarm.svg").size(px(18.0)).into_any_element());
        let sleep = div().relative().w(px(DESKTOP_CONTROL_SIZE)).flex_none().when(self.sleep_menu_open, |wrapper| wrapper.child(gpui::deferred(super::sleep_menu(target.clone(), sleep_label.clone(), layout, theme)).with_priority(1))).child(
            super::menu_trigger("dock-sleep-timer", sleep_content, self.sleep_menu_open || sleep_label.is_some(), theme).w(px(DESKTOP_CONTROL_SIZE)).h(px(DESKTOP_CONTROL_SIZE)).on_click(move |_, _, cx| {
                let _ = sleep_target.update(cx, |dock, cx| {
                    dock.toggle_sleep_menu();
                    cx.notify();
                });
            }),
        );
        let speed_target = target.clone();
        let speed = div().relative().w(px(DESKTOP_CONTROL_SIZE)).flex_none().when(self.speed_menu_open, |wrapper| wrapper.child(gpui::deferred(super::playback_speed_menu(speed_target.clone(), player.rate, layout, theme)).with_priority(1))).child(
            super::menu_trigger("dock-playback-rate", super::format_playback_rate(player.rate), self.speed_menu_open, theme).w(px(DESKTOP_CONTROL_SIZE)).h(px(DESKTOP_CONTROL_SIZE)).flex_none().debug_selector(|| "dock-playback-rate".into()).on_click(move |_, _, cx| {
                let _ = speed_target.update(cx, |dock, cx| {
                    dock.toggle_speed_menu();
                    cx.notify();
                });
            }),
        );
        // A matched-width counterpart to `desktop_book_area`: both sides are
        // `flex_1`, so the flex-none transport controls between them land in
        // the header's true center rather than wherever the book area's
        // content happens to leave off.
        let close = super::transport_control("dock-close", Icon::new(IconName::Close).size(px(18.0)), theme).w(px(36.0)).h(px(DESKTOP_CONTROL_SIZE)).flex_none().on_click(cx.listener(|_, _, _, cx| cx.emit(DockAction::Close)));
        let desktop_tools = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_end()
            .justify_center()
            .gap(px(2.0))
            .children(book_remaining.map(|left| div().text_size(gpui::rems(ui_components::TEXT_XS)).text_color(theme.text_muted).child(left)))
            .child(div().flex().items_center().gap(px(8.0)).child(div().flex().items_center().gap(px(4.0)).child(sleep).child(speed)).child(close));

        let desktop_header = div().w_full().h(px(DESKTOP_DOCK_HEIGHT)).flex().items_center().gap(px(8.0)).pr(px(8.0)).child(desktop_book_area).child(desktop_controls).child(desktop_tools);
        div()
            .id("audiobook-dock")
            .occlude()
            .relative()
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .debug_selector(|| "audiobook-dock".into())
            .border_t_1()
            .border_color(theme.rule)
            .bg(theme.page_bg)
            .text_color(theme.text)
            .child(desktop_header)
            .child(progress_track)
            .when(self.scrub.is_some(), |dock| dock.child(dock_scrub_capture(target.clone())))
            .into_any_element()
    }
}
