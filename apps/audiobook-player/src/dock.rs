use super::*;
use gpui::{EventEmitter, TouchEvent, TouchId, TouchPhase, rems};
use gpui_component::menu::DropdownMenu as _;
use std::rc::Rc;
use ui_components::{Button, FlexItem as _, PLAYER_DOCK_CAPTION_SIZE_REM, PLAYER_DOCK_HEIGHT_REM, PLAYER_DOCK_TEXT_SIZE_REM, PlaybackTransportAction, SPACE_SM, SPACE_XXS, TransportLayout, playback_secondary_button, playback_skip_forward_button};

mod gestures;

pub enum DockAction {
    /// Open the book's detail page, which also hosts its full mobile player.
    Expand,
    Close,
}

/// The compact phone player and its edge-to-edge square cover.
const MINI_PLAYER_HEIGHT_REM: f32 = 4.0;
/// How far a touch must travel sideways across the docked bar to change chapter.
const SWIPE_THRESHOLD: f32 = 40.0;

/// Compact projection of the retained playback entity. It has no decoder or
/// streaming resources of its own.
pub struct AudiobookDock {
    player: Entity<PlaybackSession>,
    /// Drawn as the controls inside the playing book's detail page rather
    /// than as the dock along the bottom of the window.
    inline: bool,
    /// The fraction of the current chapter the pointer/finger is dragging the
    /// position to. Present only during a drag, and what the progress bar
    /// reads while it is.
    scrub: Option<f32>,
    scrub_touch: Option<TouchId>,
    /// Where the progress track was last laid out, so a drag that has left it
    /// can still be turned into a position.
    progress_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The distance a touch swipe on the docked bar has travelled, while one is
    /// in progress.
    swipe: Option<gpui::Point<f32>>,
    seek_swipe: gestures::SeekSwipe,
    seek_finish: Option<gpui::Task<()>>,
}

impl AudiobookDock {
    pub fn new(player: Entity<PlaybackSession>, cx: &mut Context<Self>) -> Self {
        Self::create(player, false, cx)
    }

    /// The same controls, for the playing book's detail page.
    pub fn inline(player: Entity<PlaybackSession>, cx: &mut Context<Self>) -> Self {
        Self::create(player, true, cx)
    }

    fn create(player: Entity<PlaybackSession>, inline: bool, cx: &mut Context<Self>) -> Self {
        cx.observe(&player, |_, _, cx| cx.notify()).detach();
        Self {
            player,
            inline,
            scrub: None,
            scrub_touch: None,
            progress_bounds: Rc::new(Cell::new(Bounds::default())),
            swipe: None,
            seek_swipe: gestures::SeekSwipe::default(),
            seek_finish: None,
        }
    }

    /// A horizontal swipe across the docked bar changes chapter when the finger
    /// lifts: leftwards to the next chapter, rightwards to the previous one.
    fn handle_swipe(&mut self, event: &gpui::ScrollWheelEvent, cx: &mut Context<Self>) {
        if let gpui::ScrollDelta::Pages(delta) = event.delta {
            if self.swipe.take().is_some() && delta.x != 0.0 {
                if delta.x < 0.0 { self.next_chapter(cx) } else { self.previous_chapter(cx) }
            }
            cx.stop_propagation();
            return;
        }
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
        cx.notify();
    }

    /// Follows the pointer while a press is dragging. A pointer merely passing
    /// over the bar moves nothing: the position changes only by pressing.
    fn hover_scrub(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if self.scrub.is_none() {
            return;
        }
        let Some(fraction) = self.track_fraction(x) else { return };
        if self.scrub != Some(fraction) {
            self.scrub = Some(fraction);
            cx.notify();
        }
    }

    /// Commits the drag: the seek happens once, where the pointer was released.
    fn finish_scrub(&mut self, cx: &mut Context<Self>) {
        let Some(fraction) = self.scrub.take() else { return };
        self.player.update(cx, |player, cx| player.seek_in_chapter(fraction, cx));
        cx.notify();
    }

    fn handle_scrub_touch(&mut self, event: &TouchEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.phase == TouchPhase::Started && self.scrub.is_none() {
            self.begin_scrub(event.position.x, cx);
            if self.scrub.is_some() {
                self.scrub_touch = Some(event.id);
            }
        }
        if self.scrub_touch != Some(event.id) {
            return;
        }
        window.prevent_default();
        cx.stop_propagation();
        match event.phase {
            TouchPhase::Started | TouchPhase::Moved => self.hover_scrub(event.position.x, cx),
            TouchPhase::Ended => {
                self.hover_scrub(event.position.x, cx);
                self.scrub_touch = None;
                self.finish_scrub(cx);
            }
            TouchPhase::Cancelled => {
                self.scrub_touch = None;
                self.scrub = None;
                cx.notify();
            }
        }
    }

    /// Chapter skip, the seconds skips and play, at the size `layout` gives them.
    fn transport(&self, playing: bool, layout: TransportLayout, theme: ui_components::BrowserTheme, cx: &Context<Self>) -> Vec<(PlaybackTransportAction, Button)> {
        ui_components::playback_transport_buttons("now-playing", playing, layout, Some(["Previous chapter", "Next chapter"]), ui_components::PlaybackSkipKind::Seconds, theme)
            .into_iter()
            .map(|(action, button)| {
                let selector = match action {
                    PlaybackTransportAction::Previous => "audiobook-previous-chapter",
                    PlaybackTransportAction::Back => "audiobook-seek-back",
                    PlaybackTransportAction::PlayPause => "audiobook-play",
                    PlaybackTransportAction::Forward => "audiobook-seek-forward",
                    PlaybackTransportAction::Next => "audiobook-next-chapter",
                };
                let button = button
                    .debug_selector(move || selector.into())
                    .on_click(cx.listener(move |dock, _, _, cx| match action {
                        PlaybackTransportAction::Previous => dock.previous_chapter(cx),
                        PlaybackTransportAction::Back => dock.skip(super::SKIP_BACK_SECONDS, cx),
                        PlaybackTransportAction::PlayPause => dock.toggle_playback(cx),
                        PlaybackTransportAction::Forward => dock.skip(super::SKIP_FORWARD_SECONDS, cx),
                        PlaybackTransportAction::Next => dock.next_chapter(cx),
                    }));
                (action, button)
            })
            .collect()
    }

    /// Makes a progress track scrubbable. Only one is on screen at a time, so
    /// they share an id and the recorded bounds.
    fn scrubbable(&self, track: gpui::Div, cx: &Context<Self>) -> gpui::Stateful<gpui::Div> {
        let progress_bounds = self.progress_bounds.clone();
        track
            .id("audiobook-dock-progress")
            .cursor_pointer()
            .on_prepaint(move |bounds, _, _| progress_bounds.set(bounds))
            // Pressing starts a scrub rather than seeking outright, so a
            // press-and-drag gesture is one action: the seek happens once,
            // wherever the finger/pointer has got to when it lifts.
            .on_mouse_down(MouseButton::Left, cx.listener(|dock, event: &gpui::MouseDownEvent, _, cx| dock.begin_scrub(event.position.x, cx)))
            .on_mouse_move(cx.listener(|dock, event: &MouseMoveEvent, _, cx| dock.hover_scrub(event.position.x, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|dock, _, _, cx| dock.finish_scrub(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|dock, _, _, cx| dock.finish_scrub(cx)))
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(ui_components::playback_scrub_touch_listener(cx.listener(Self::handle_scrub_touch)))
    }
}

impl EventEmitter<DockAction> for AudiobookDock {}

#[cfg(test)]
mod touch_tests {
    use super::*;
    use gpui::{MouseDownEvent, MouseUpEvent, Subscription, TestAppContext, VisualTestContext, point};

    struct ScrubTest {
        dock: Entity<AudiobookDock>,
        commits: usize,
        covered: bool,
        _subscription: Subscription,
    }

    impl Render for ScrubTest {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let track = self.dock.update(cx, |dock, cx| dock.scrubbable(ui_components::now_playing_scrub_track(0.0, ui_components::browser_theme(cx)).w(px(200.0)), cx));
            div().size_full().relative().child(track).when(self.covered, |view| view.child(div().absolute().top_0().left_0().w(px(200.0)).h(px(24.0)).occlude()))
        }
    }

    fn touch(cx: &mut VisualTestContext, id: u64, phase: TouchPhase, x: f32, y: f32) {
        cx.simulate_event(TouchEvent { id: TouchId(id), phase, position: point(px(x), px(y)), force: None });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn touch_scrub_follows_its_contact_and_commits_only_on_release(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, cx| {
            let (sender, _) = async_channel::unbounded();
            let player = cx.new(|_| PlaybackSession::new(1.0, sender));
            let subscription = cx.observe(&player, |view: &mut ScrubTest, _, _| view.commits += 1);
            let dock = cx.new(|cx| AudiobookDock::new(player, cx));
            ScrubTest { dock, commits: 0, covered: false, _subscription: subscription }
        });
        cx.run_until_parked();
        let dock = view.read_with(cx, |view, _| view.dock.clone());
        touch(cx, 1, TouchPhase::Started, 40.0, 12.0);
        dock.read_with(cx, |dock, _| assert_eq!(dock.scrub, Some(0.2)));
        touch(cx, 1, TouchPhase::Moved, 150.0, 80.0);
        dock.read_with(cx, |dock, _| assert_eq!(dock.scrub, Some(0.75)));
        touch(cx, 2, TouchPhase::Started, 80.0, 12.0);
        touch(cx, 2, TouchPhase::Ended, 80.0, 12.0);
        dock.read_with(cx, |dock, _| assert_eq!(dock.scrub, Some(0.75)));
        view.read_with(cx, |view, _| assert_eq!(view.commits, 0));
        touch(cx, 1, TouchPhase::Ended, 180.0, 80.0);
        dock.read_with(cx, |dock, _| assert!(dock.scrub.is_none() && dock.scrub_touch.is_none()));
        view.read_with(cx, |view, _| assert_eq!(view.commits, 1));
        touch(cx, 1, TouchPhase::Ended, 180.0, 80.0);
        touch(cx, 3, TouchPhase::Started, 40.0, 12.0);
        touch(cx, 3, TouchPhase::Moved, 150.0, 80.0);
        touch(cx, 3, TouchPhase::Cancelled, 150.0, 80.0);
        dock.read_with(cx, |dock, _| assert!(dock.scrub.is_none() && dock.scrub_touch.is_none()));
        view.read_with(cx, |view, _| assert_eq!(view.commits, 1));
        touch(cx, 4, TouchPhase::Started, 40.0, 80.0);
        touch(cx, 4, TouchPhase::Ended, 40.0, 80.0);
        view.update(cx, |view, cx| {
            view.covered = true;
            cx.notify();
        });
        cx.run_until_parked();
        touch(cx, 5, TouchPhase::Started, 40.0, 12.0);
        touch(cx, 5, TouchPhase::Ended, 40.0, 12.0);
        view.read_with(cx, |view, _| assert_eq!(view.commits, 1));
        view.update(cx, |view, cx| {
            view.covered = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: point(px(40.0), px(12.0)), ..Default::default() });
        cx.simulate_event(MouseMoveEvent { pressed_button: Some(MouseButton::Left), position: point(px(150.0), px(12.0)), ..Default::default() });
        dock.read_with(cx, |dock, _| assert_eq!(dock.scrub, Some(0.75)));
        cx.simulate_event(MouseUpEvent { button: MouseButton::Left, position: point(px(150.0), px(80.0)), ..Default::default() });
        cx.run_until_parked();
        dock.read_with(cx, |dock, _| assert!(dock.scrub.is_none()));
        view.read_with(cx, |view, _| assert_eq!(view.commits, 2));
    }
}

/// A window-wide mouse listener installed for the duration of a scrub, so a
/// drag that strays off the (thin) progress bar still tracks. Releasing off
/// the bar is the track's own `on_mouse_up_out`.
fn dock_scrub_capture(cx: &Context<AudiobookDock>) -> impl IntoElement {
    let hover = cx.listener(|dock, event: &MouseMoveEvent, _, cx| dock.hover_scrub(event.position.x, cx));
    canvas(
        |_, _, _| (),
        move |_, _: (), window, _| {
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble {
                    hover(event, window, cx);
                }
            });
        },
    )
    .absolute()
    .size_full()
}

impl AudiobookDock {
    fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.request_toggle(cx));
    }
    fn skip(&mut self, seconds: f64, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.skip(seconds, cx));
    }
    fn previous_chapter(&mut self, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.previous_chapter(cx));
    }
    fn next_chapter(&mut self, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.next_chapter(cx));
    }
}

/// Title over the current chapter, both truncated: the same block in every
/// layout.
fn title_block(title: String, chapter: String, theme: ui_components::BrowserTheme) -> gpui::Div {
    ui_components::column(0.0)
        .fill()
        .child(ui_components::single_line(title).text_size(rems(PLAYER_DOCK_TEXT_SIZE_REM)).line_height_auto())
        .child(ui_components::caption_line(chapter, theme).text_size(rems(PLAYER_DOCK_CAPTION_SIZE_REM)).line_height_auto())
}

/// The cover's square box; empty when the book has no cover.
fn cover_box(cover: Option<Arc<Image>>, size_rem: f32, theme: ui_components::BrowserTheme) -> gpui::Div {
    div().size(rems(size_rem)).flex_none().children(cover.map(|cover| ui_components::book_cover_image("audiobook-dock-cover", cover, 1.0, false, theme)))
}

impl Render for AudiobookDock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ui_components::browser_theme(cx);
        let player = self.player.read(cx);
        let cover = player.book.as_ref().and_then(|book| book.cover.clone());
        let compact = ui_components::WindowWidthClass::for_window(window).is_compact();
        if !self.inline {
            self.seek_swipe = gestures::SeekSwipe::default();
        }

        let position = player.position;
        let (chapter_start, chapter_position, chapter_duration) = player
            .book
            .as_ref()
            .and_then(|book| book.chapters.get(player.active_chapter_index()))
            .map(|chapter| {
                let duration = chapter.end().saturating_sub(chapter.start());
                (chapter.start(), position.saturating_sub(chapter.start()).min(duration), duration)
            })
            .unwrap_or_default();
        let progress_fraction = if chapter_duration.is_zero() { 0.0 } else { (chapter_position.as_secs_f64() / chapter_duration.as_secs_f64()) as f32 };
        let title = player.book.as_ref().map(|book| book.title.clone()).unwrap_or_else(|| "Opening audiobook…".into());
        let current_chapter = player.book.as_ref().and_then(|book| book.chapters.get(player.active_chapter_index())).map(|chapter| super::chapter_display_title(&chapter.title)).unwrap_or_else(|| "No chapter".into());
        let playing = player.transport_playing();
        // Pointer and touchpad scrubbing show exact seconds as the position
        // changes. Ordinary playback uses compact whole-minute captions.
        let display_fraction = self.scrub.unwrap_or(progress_fraction);
        let display_position = self.scrub.map(|fraction| super::chapter_position_at_fraction(Duration::ZERO, chapter_duration, fraction)).unwrap_or(chapter_position);
        let display_book_position = if self.scrub.is_some() { chapter_start.saturating_add(display_position) } else { position };
        let book_remaining = player.book.as_ref().filter(|book| !book.duration.is_zero()).map(|book| super::format_time_left(book.duration.saturating_sub(display_book_position)));
        let sleep_label = player.sleep_label();
        let rate = player.rate;
        // The scrubber's ends in the phone player and the detail page: elapsed
        // and still to come in this chapter.
        let chapter_remaining = chapter_duration.saturating_sub(display_position);
        let (elapsed, remaining) = if self.scrub.is_some() || self.seek_swipe.is_active() {
            (super::format_time(display_position), format!("−{}", super::format_time(chapter_remaining)))
        } else {
            (super::format_duration_short(display_position), format!("−{}", super::format_duration_short(chapter_remaining)))
        };

        // The playing book's detail page holds these controls in place of its
        // actions; the page itself already shows the cover, title and
        // chapters, and the dock is hidden while it is open.
        if self.inline {
            if compact {
                let player_entity = self.player.clone();
                let menu_label = sleep_label.clone();
                let sleep = ui_components::now_playing_tool_button("now-playing-sleep", sleep_label.clone().unwrap_or_else(|| "Sleep".into()), Some(Icon::empty().path("icons/alarm.svg")), sleep_label.is_some(), theme)
                    .tooltip("Sleep timer")
                    .on_click(move |_, window, cx| {
                        let player = player_entity.clone();
                        super::sleep_menu(menu_label.as_deref()).open_sheet(window, cx, move |choice, cx| player.update(cx, |player, cx| player.choose_sleep(choice, cx)));
                    });
                let player_entity = self.player.clone();
                let speed = ui_components::now_playing_tool_button("now-playing-speed", super::format_playback_rate(rate), None, !super::same_rate(rate, 1.0), theme)
                    .tooltip("Playback speed")
                    .on_click(move |_, window, cx| {
                        let player = player_entity.clone();
                        super::speed_menu(rate).open_sheet(window, cx, move |rate, cx| player.update(cx, |player, cx| player.set_rate(rate, cx)));
                    });
                let narrow = window.viewport_size().width < px(360.0);
                let transport = ui_components::now_playing_transport().id("mobile-playback-controls").overflow_x_scroll().child(speed.when(narrow, |button| button.size(px(32.0)))).children(self.transport(playing, TransportLayout::Player, theme, cx).into_iter().map(|(action, button)| {
                    button.when(narrow, |button| button.size(px(if action == PlaybackTransportAction::PlayPause { 64.0 } else { 32.0 })))
                })).child(sleep.w_auto().min_w(rems(2.5)).px(px(4.0)));
                let tools = ui_components::book_detail_mobile_tools(theme);
                return ui_components::now_playing_inline()
                    .relative()
                    .child(ui_components::now_playing_chapter(current_chapter))
                    .child(self.scrubbable(ui_components::now_playing_scrub_track(display_fraction, theme), cx))
                    .child(ui_components::now_playing_times(elapsed, book_remaining.map(SharedString::from), remaining, theme))
                    .child(transport)
                    .child(tools)
                    .child(gestures::seek_swipe_capture(cx))
                    .when(self.scrub.is_some(), |controls| controls.child(dock_scrub_capture(cx)))
                    .into_any_element();
            }
            let player_entity = self.player.clone();
            let choose_sleep = move |choice, cx: &mut App| player_entity.update(cx, |player, cx| player.choose_sleep(choice, cx));
            let menu_label = sleep_label.clone();
            let sleep = ui_components::now_playing_sleep_button("now-playing-sleep", sleep_label.clone(), theme)
                .debug_selector(|| "audiobook-inline-sleep".into())
                .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| super::sleep_menu(menu_label.as_deref()).dropdown(menu, choose_sleep.clone()));
            let player_entity = self.player.clone();
            let set_rate = move |rate, cx: &mut App| player_entity.update(cx, |player, cx| player.set_rate(rate, cx));
            let speed = ui_components::now_playing_speed_button("now-playing-speed", super::format_playback_rate(rate), !super::same_rate(rate, 1.0), theme)
                .debug_selector(|| "audiobook-inline-speed".into())
                .dropdown_menu_with_anchor(gpui::Anchor::TopLeft, move |menu, _, _| super::speed_menu(rate).dropdown(menu, set_rate.clone()));
            let transport = self.transport(playing, TransportLayout::Inline, theme, cx);
            let controls = ui_components::now_playing_inline_controls().id("audiobook-inline-controls").overflow_x_scroll().child(speed).children(transport.into_iter().map(|(_, button)| button)).child(sleep);
            return ui_components::now_playing_inline()
                .relative()
                .child(ui_components::now_playing_inline_times(elapsed, current_chapter, remaining, theme))
                .child(self.scrubbable(ui_components::now_playing_scrub_track(display_fraction, theme), cx))
                .child(controls)
                .child(gestures::seek_swipe_capture(cx))
                .when(self.scrub.is_some(), |controls| controls.child(dock_scrub_capture(cx)))
                .into_any_element();
        }

        // The mini-player and the book card open the same detail page.
        if compact {
            let summary = title_block(title.clone(), current_chapter.clone(), theme).id("audiobook-dock-expand").cursor_pointer().on_click(cx.listener(|_, _, _, cx| {
                cx.emit(DockAction::Expand);
            }));
            let play = ui_components::playback_play_button("dock-play", playing, TransportLayout::MiniPlayer, theme).on_click(cx.listener(|dock, _, _, cx| dock.toggle_playback(cx)));
            let forward = playback_skip_forward_button("dock-forward", TransportLayout::MiniPlayer, theme).on_click(cx.listener(|dock, _, _, cx| dock.skip(super::SKIP_FORWARD_SECONDS, cx)));
            let bar = ui_components::bottom_bar(theme)
                .id("audiobook-dock")
                .debug_selector(|| "audiobook-dock".into())
                .occlude()
                .relative()
                .on_scroll_wheel(cx.listener(|dock, event, _, cx| dock.handle_swipe(event, cx)))
                .child(
                    ui_components::row(0.0)
                        .gap(rems(SPACE_SM / 16.0))
                        .fill()
                        .h(rems(MINI_PLAYER_HEIGHT_REM))
                        .px(rems(SPACE_SM / 16.0))
                        .child(cover_box(cover.clone(), MINI_PLAYER_HEIGHT_REM, theme))
                        .child(summary)
                        .child(play)
                        .child(forward),
                )
                .child(ui_components::top_edge().child(ui_components::player_dock_progress_line(progress_fraction, theme)));
            return bar.into_any_element();
        }

        let chapter_time = match self.scrub {
            Some(_) => format!("{} / {}", super::format_time(display_position), super::format_time(chapter_duration)),
            None => format!("{} / {}", super::format_duration_short(display_position), super::format_duration_short(chapter_duration)),
        };
        // The line is drawn thin, inside a taller hit area.
        let scrub_track = self.scrubbable(ui_components::player_dock_track(display_fraction, theme), cx);

        let transport = self.transport(playing, TransportLayout::Dock, theme, cx).into_iter().map(|(_, button)| button);
        let close = playback_secondary_button(ui_components::outlined_icon_button("dock-close", "Close player", IconName::Close)).on_click(cx.listener(|_, _, _, cx| cx.emit(DockAction::Close)));
        // The alarm glyph, or the time left once a timer is running.
        let sleep = match sleep_label.clone() {
            Some(label) => ui_components::player_dock_text_button("dock-sleep-timer", label).tooltip("Sleep timer"),
            None => ui_components::outlined_icon_button("dock-sleep-timer", "Sleep timer", Icon::empty().path("icons/alarm.svg")),
        };
        let sleep = playback_secondary_button(sleep).when(sleep_label.is_some(), |button| button.text_color(theme.text_accent));
        let speed = ui_components::player_dock_text_button("dock-playback-rate", super::format_playback_rate(rate)).tooltip("Playback speed");

        let book = title_block(title, current_chapter, theme).child(ui_components::caption_line(chapter_time, theme).text_size(rems(PLAYER_DOCK_CAPTION_SIZE_REM)).line_height_auto()).id("audiobook-dock-expand").cursor_pointer();
        let transport = ui_components::player_dock_transport(None, transport, theme).id("audiobook-dock-transport").on_click(|_, _, cx| cx.stop_propagation());

        let player = self.player.clone();
        let choose_sleep = move |choice, cx: &mut App| player.update(cx, |player, cx| player.choose_sleep(choice, cx));
        let sleep = sleep.dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| super::sleep_menu(sleep_label.as_deref()).dropdown(menu, choose_sleep.clone()));
        let player = self.player.clone();
        let set_rate = move |rate, cx: &mut App| player.update(cx, |player, cx| player.set_rate(rate, cx));
        let speed = speed.dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |menu, _, _| super::speed_menu(rate).dropdown(menu, set_rate.clone()));
        let tools = ui_components::column(0.0)
            .gap(rems(SPACE_XXS / 16.0))
            .items_end()
            .children(book_remaining.map(|left| ui_components::caption_line(left, theme).text_size(rems(PLAYER_DOCK_CAPTION_SIZE_REM)).line_height_auto()))
            .child(ui_components::player_dock_tools().id("audiobook-dock-tools").on_click(|_, _, cx| cx.stop_propagation()).child(sleep).child(speed).child(close));

        ui_components::player_dock(div().flex().items_center().gap(rems(SPACE_SM / 16.0)).h_full().min_w_0().flex_1().child(cover_box(cover, PLAYER_DOCK_HEIGHT_REM, theme)).child(book), transport, tools, theme)
            .id("audiobook-dock")
            .debug_selector(|| "audiobook-dock".into())
            .cursor_pointer()
            .on_click(cx.listener(|_, _, _, cx| cx.emit(DockAction::Expand)))
            .child(scrub_track)
            .when(self.scrub.is_some(), |dock| dock.child(dock_scrub_capture(cx)))
            .into_any_element()
    }
}
