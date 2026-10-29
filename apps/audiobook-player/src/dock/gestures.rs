//! Horizontal touchpad seeking while the audiobook's full view is visible.

use gpui::prelude::*;
use gpui::{Context, DispatchPhase, IntoElement, Point, ScrollDelta, ScrollWheelEvent, TouchPhase, Window, canvas, point};
use gpui_component::{Root, WindowExt};
use std::time::Duration;
use web_time::Instant;

use super::AudiobookDock;

/// 120 logical pixels of travel seek 30 seconds, in either direction.
const SEEK_SECONDS_PER_PIXEL: f64 = 0.25;
const AXIS_LOCK_DISTANCE: f32 = 6.0;
const SEEK_IDLE_DELAY: Duration = Duration::from_millis(200);

#[derive(Default)]
enum SwipeAxis {
    #[default]
    Pending,
    Horizontal,
    Vertical,
}

#[derive(Default)]
pub(super) struct SeekSwipe {
    last_event: Option<Instant>,
    travel: Point<f32>,
    axis: SwipeAxis,
    origin: Duration,
    target: Duration,
    phased: bool,
    ended: bool,
}

enum SeekAction {
    Move(Duration),
    Finish,
    Cancel(Duration),
}

impl SeekSwipe {
    pub(super) fn is_active(&self) -> bool {
        matches!(self.axis, SwipeAxis::Horizontal) && !self.ended && (self.phased || self.last_event.is_some_and(|last| last.elapsed() < SEEK_IDLE_DELAY))
    }

    fn consume(&mut self) {
        self.ended = true;
        self.phased = false;
        self.last_event = Some(Instant::now());
    }

    fn update(&mut self, event: &ScrollWheelEvent, position: Duration, duration: Duration) -> Option<SeekAction> {
        let now = Instant::now();
        if event.touch_phase == TouchPhase::Started || self.last_event.is_none() || (!self.phased && self.last_event.is_some_and(|last| now.duration_since(last) >= SEEK_IDLE_DELAY)) {
            *self = Self { origin: position, target: position, phased: event.touch_phase == TouchPhase::Started, ..Default::default() };
        }
        self.last_event = Some(now);
        if self.ended {
            return None;
        }
        if matches!(event.touch_phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            self.ended = true;
            self.phased = false;
            return matches!(self.axis, SwipeAxis::Horizontal).then(|| if event.touch_phase == TouchPhase::Cancelled { SeekAction::Cancel(self.origin) } else { SeekAction::Finish });
        }
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => point(f32::from(delta.x), f32::from(delta.y)),
            ScrollDelta::Lines(delta) => delta * 32.0,
            ScrollDelta::Pages(_) => return None,
        };
        if !delta.x.is_finite() || !delta.y.is_finite() {
            return None;
        }
        let distance = match self.axis {
            SwipeAxis::Horizontal => delta.x,
            SwipeAxis::Vertical => return None,
            SwipeAxis::Pending => {
                self.travel += delta;
                let x = self.travel.x.abs();
                let y = self.travel.y.abs();
                if x >= AXIS_LOCK_DISTANCE && x >= y * 1.25 {
                    self.axis = SwipeAxis::Horizontal;
                    self.travel.x
                } else {
                    if y >= AXIS_LOCK_DISTANCE && y >= x * 1.25 {
                        self.axis = SwipeAxis::Vertical;
                    }
                    return None;
                }
            }
        };
        if distance == 0.0 {
            return None;
        }
        // GPUI reports content movement on every backend. Moving toward later
        // content has a negative delta, just like page navigation and scrolling
        // a horizontal list. Respect the system's scrolling preference once.
        self.target = crate::relative_seek_position(self.target, -f64::from(distance) * SEEK_SECONDS_PER_PIXEL, duration);
        Some(SeekAction::Move(self.target))
    }
}

impl AudiobookDock {
    fn handle_seek_swipe(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.modifiers;
        let overlay_open = window.root::<Root>().flatten().is_some() && (window.has_active_dialog(cx) || window.has_active_sheet(cx));
        if !self.inline
            || self.scrub.is_some()
            || overlay_open
            || window.is_page_scroll_swipe()
            || matches!(event.delta, ScrollDelta::Pages(_))
            || modifiers.control
            || modifiers.alt
            || modifiers.platform
            || modifiers.shift
        {
            if self.seek_finish.take().is_some() || self.seek_swipe.is_active() {
                self.player.update(cx, |player, cx| player.finish_scrub(cx));
            }
            self.seek_swipe.consume();
            return;
        }

        let horizontal = match event.delta {
            ScrollDelta::Pixels(delta) => delta.x.abs() > delta.y.abs(),
            ScrollDelta::Lines(delta) | ScrollDelta::Pages(delta) => delta.x.abs() > delta.y.abs(),
        };
        let was_active = self.seek_swipe.is_active();
        let player = self.player.read(cx);
        let duration = player.book.as_ref().map(|book| book.duration).unwrap_or_default();
        let action = self.seek_swipe.update(event, player.position, duration);
        let handled = action.is_some();
        match action {
            Some(SeekAction::Move(position)) => {
                self.player.update(cx, |player, cx| player.scrub_to(position, cx));
                self.seek_finish.take();
                if !self.seek_swipe.phased {
                    let timer = cx.background_executor().timer(SEEK_IDLE_DELAY);
                    self.seek_finish = Some(cx.spawn(async move |dock, cx| {
                        timer.await;
                        let _ = dock.update(cx, |dock, cx| {
                            dock.seek_finish.take();
                            dock.seek_swipe = SeekSwipe::default();
                            dock.player.update(cx, |player, cx| player.finish_scrub(cx));
                            cx.notify();
                        });
                    }));
                }
            }
            Some(SeekAction::Finish) => {
                self.seek_finish.take();
                self.player.update(cx, |player, cx| player.finish_scrub(cx));
            }
            Some(SeekAction::Cancel(position)) => {
                self.seek_finish.take();
                self.player.update(cx, |player, cx| {
                    player.scrub_to(position, cx);
                    player.finish_scrub(cx);
                });
            }
            None => {}
        }
        if horizontal || was_active || handled {
            cx.stop_propagation();
        }
    }
}

/// Installed only by the detail-page controls and expanded player. A window
/// capture listener deliberately has no hit test: seeking does not depend on
/// where the pointer happens to be, or which child normally handles scrolling.
pub(super) fn seek_swipe_capture(cx: &Context<AudiobookDock>) -> impl IntoElement {
    let listener = cx.listener(AudiobookDock::handle_seek_swipe);
    canvas(
        |_, _, _| (),
        move |_, _: (), window, _| {
            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    listener(event, window, cx);
                }
            });
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudiobookBook, PlaybackSession};
    use gpui::{Entity, Modifiers, Render, Subscription, TestAppContext, TouchPhase, VisualTestContext, div, point, px, size};
    use std::{sync::Arc, time::Duration};

    struct PlayerHost {
        player: Entity<PlaybackSession>,
        dock: Entity<AudiobookDock>,
        visible: bool,
        controls_offscreen: bool,
        full_width: bool,
        scrolls: usize,
        expands: usize,
        _dock_events: Subscription,
        positions: async_channel::Receiver<crate::PlaybackPosition>,
    }

    impl Render for PlayerHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .relative()
                .when(self.visible, |host| {
                    let controls = div().w_full().when(!self.full_width, |controls| controls.w(px(240.0))).child(self.dock.clone());
                    if self.controls_offscreen { host.child(div().w(px(240.0)).h(px(20.0)).overflow_hidden().child(div().mt(px(500.0)).child(controls))) } else { host.child(controls) }
                })
                .child(div().absolute().left(px(280.0)).top(px(380.0)).size(px(80.0)).occlude().on_scroll_wheel(cx.listener(|host, _, _, _| host.scrolls += 1)))
        }
    }

    fn setup(cx: &mut TestAppContext, inline: bool) -> (Entity<PlayerHost>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        let (host, cx) = cx.add_window_view(|_, cx| {
            let (sender, receiver) = async_channel::unbounded();
            let player = cx.new(|_| {
                let mut player = PlaybackSession::new(1.0, sender);
                player.book = Some(Arc::new(AudiobookBook {
                    title: "Gesture test".into(),
                    author: None,
                    narrator: None,
                    duration: Duration::from_secs(300),
                    chapters: vec![book_model::AudiobookChapter { title: "Chapter".into(), start_ms: 0, end_ms: 300_000 }],
                    cover: None,
                }));
                player.position = Duration::from_secs(60);
                player
            });
            let dock = cx.new(|cx| if inline { AudiobookDock::inline(player.clone(), cx) } else { AudiobookDock::new(player.clone(), cx) });
            let dock_events = cx.subscribe(&dock, |host: &mut PlayerHost, _, action: &crate::DockAction, _| {
                if matches!(action, crate::DockAction::Expand) {
                    host.expands += 1;
                }
            });
            PlayerHost { player, dock, visible: true, controls_offscreen: false, full_width: false, scrolls: 0, expands: 0, _dock_events: dock_events, positions: receiver }
        });
        cx.simulate_resize(size(px(1000.0), px(800.0)));
        cx.run_until_parked();
        (host, cx)
    }

    fn scroll(cx: &mut VisualTestContext, x: f32, y: f32, touch_phase: TouchPhase) {
        // Outside the inline controls and over an occluding sibling.
        cx.simulate_event(ScrollWheelEvent { position: point(px(300.0), px(400.0)), delta: ScrollDelta::Pixels(point(px(x), px(y))), touch_phase, ..Default::default() });
        cx.run_until_parked();
    }

    fn position(host: &Entity<PlayerHost>, cx: &mut VisualTestContext) -> Duration {
        host.read_with(cx, |host, cx| host.player.read(cx).position)
    }

    #[test]
    fn normalized_pixel_and_line_scrolling_seek_in_the_same_direction() {
        // macOS and Windows precision gestures report phased pixels; Wayland
        // and browsers use unphased pixels; X11 uses unphased line deltas.
        for (delta, phase) in [(ScrollDelta::Pixels(point(px(-32.0), px(0.0))), TouchPhase::Started), (ScrollDelta::Pixels(point(px(-32.0), px(0.0))), TouchPhase::Moved), (ScrollDelta::Lines(point(-1.0, 0.0)), TouchPhase::Moved)] {
            let mut swipe = SeekSwipe::default();
            let event = ScrollWheelEvent { delta, touch_phase: phase, ..Default::default() };
            let Some(SeekAction::Move(target)) = swipe.update(&event, Duration::from_secs(60), Duration::from_secs(300)) else { panic!("forward scrolling must seek") };
            assert_eq!(target, Duration::from_secs(68));
            let reverse = ScrollWheelEvent {
                delta: match delta {
                    ScrollDelta::Pixels(delta) => ScrollDelta::Pixels(-delta),
                    ScrollDelta::Lines(delta) => ScrollDelta::Lines(-delta),
                    ScrollDelta::Pages(_) => unreachable!(),
                },
                touch_phase: TouchPhase::Moved,
                ..Default::default()
            };
            let Some(SeekAction::Move(target)) = swipe.update(&reverse, Duration::from_secs(68), Duration::from_secs(300)) else { panic!("backward scrolling must seek") };
            assert_eq!(target, Duration::from_secs(60));
        }
    }

    #[gpui::test]
    fn dock_background_reopens_details_without_transport_or_scrub_clicks(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx, false);
        host.update(cx, |host, cx| {
            host.full_width = true;
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("audiobook-dock").unwrap();
        cx.simulate_click(point(bounds.left() + bounds.size.width * 0.75, bounds.center().y), Modifiers::default());
        host.read_with(cx, |host, _| assert_eq!(host.expands, 1));
        let forward = cx.debug_bounds("audiobook-seek-forward").unwrap();
        cx.simulate_click(forward.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(position(&host, cx), Duration::from_secs(90));
        host.read_with(cx, |host, _| assert_eq!(host.expands, 1, "transport clicks must not open book details"));
        cx.simulate_click(point(bounds.center().x, bounds.top() + px(2.0)), Modifiers::default());
        cx.run_until_parked();
        host.read_with(cx, |host, _| assert_eq!(host.expands, 1, "scrubbing must not open book details"));
    }

    #[gpui::test]
    fn detail_audio_buttons_share_one_row(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx, true);
        host.update(cx, |host, cx| {
            host.full_width = true;
            cx.notify();
        });
        cx.run_until_parked();
        let play = cx.debug_bounds("audiobook-play").unwrap();
        for selector in ["audiobook-inline-speed", "audiobook-previous-chapter", "audiobook-seek-back", "audiobook-seek-forward", "audiobook-next-chapter", "audiobook-inline-sleep"] {
            let button = cx.debug_bounds(selector).unwrap();
            assert_eq!(button.center().y, play.center().y, "{selector} must share the play button's row");
        }
    }

    #[gpui::test]
    fn mobile_detail_keeps_information_tools_below_playback_without_clipping(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx, true);
        host.update(cx, |host, cx| {
            host.full_width = true;
            cx.notify();
        });
        cx.simulate_resize(size(px(390.0), px(640.0)));
        cx.run_until_parked();
        let play = cx.debug_bounds("audiobook-play").unwrap();
        for selector in ["book-contents", "book-description"] {
            let button = cx.debug_bounds(selector).expect("mobile information tools must be present");
            assert!(button.top() >= play.bottom());
            assert!(button.left() >= px(0.0) && button.right() <= px(390.0));
            assert!(button.bottom() <= px(640.0));
        }
    }

    #[gpui::test]
    fn detail_page_seeks_without_hover_and_ignores_momentum_and_hidden_controls(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx, true);
        scroll(cx, -20.0, 0.0, TouchPhase::Started);
        assert_eq!(position(&host, cx), Duration::from_secs(65), "playback moves before fingers lift");
        scroll(cx, -40.0, 2.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(75), "twice the additional distance seeks twice as far");
        host.read_with(cx, |host, cx| {
            assert!(host.dock.read(cx).seek_swipe.is_active());
            assert!(host.positions.is_empty(), "live movement does not enqueue persistence writes");
        });
        scroll(cx, -100.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(75), "release does not apply another movement");
        host.read_with(cx, |host, cx| {
            assert!(!host.dock.read(cx).seek_swipe.is_active());
            assert_eq!(host.positions.len(), 1, "release commits the final position");
        });
        scroll(cx, -160.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(75));
        scroll(cx, 60.0, 0.0, TouchPhase::Started);
        assert_eq!(position(&host, cx), Duration::from_secs(60));
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(60));
        host.update(cx, |host, cx| {
            host.controls_offscreen = true;
            cx.notify();
        });
        cx.run_until_parked();
        scroll(cx, -60.0, 0.0, TouchPhase::Started);
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(75), "seeking remains available when detail-page controls are scrolled out of view");
        host.update(cx, |host, cx| {
            host.visible = false;
            cx.notify();
        });
        cx.run_until_parked();
        scroll(cx, -60.0, 0.0, TouchPhase::Started);
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(75));
    }

    #[gpui::test]
    fn vertical_short_cancelled_and_scrubbing_gestures_do_not_seek(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx, true);
        for (x, y, phase) in [(0.0, -160.0, TouchPhase::Ended), (2.0, 0.0, TouchPhase::Ended), (-60.0, 0.0, TouchPhase::Cancelled)] {
            scroll(cx, x, y, TouchPhase::Started);
            if phase == TouchPhase::Cancelled {
                assert_eq!(position(&host, cx), Duration::from_secs(75));
            }
            scroll(cx, 0.0, 0.0, phase);
            assert_eq!(position(&host, cx), Duration::from_secs(60));
        }
        host.read_with(cx, |host, cx| {
            assert!(host.scrolls > 0, "vertical scrolling remains available to the page");
            assert_eq!(host.player.read(cx).rate, 1.0);
        });
        let dock = host.read_with(cx, |host, _| host.dock.clone());
        dock.update(cx, |dock, _| dock.scrub = Some(0.5));
        scroll(cx, -60.0, 0.0, TouchPhase::Started);
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(60));
    }

    #[gpui::test]
    fn seeking_is_available_in_mobile_book_detail_but_not_dock_or_miniplayer(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx, false);
        scroll(cx, -60.0, 0.0, TouchPhase::Started);
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(60));
        cx.simulate_resize(size(px(400.0), px(800.0)));
        cx.run_until_parked();
        scroll(cx, -60.0, 0.0, TouchPhase::Started);
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(60));
        let bounds = cx.debug_bounds("audiobook-dock").unwrap();
        cx.simulate_click(point(bounds.left() + px(90.0), bounds.center().y), Default::default());
        cx.run_until_parked();
        host.read_with(cx, |host, _| assert_eq!(host.expands, 1, "the mini-player must request the shared book detail page"));
        let dock = host.read_with(cx, |host, _| host.dock.clone());
        dock.update(cx, |dock, cx| {
            dock.inline = true;
            cx.notify();
        });
        cx.run_until_parked();
        scroll(cx, -60.0, 0.0, TouchPhase::Started);
        assert_eq!(position(&host, cx), Duration::from_secs(75), "the mobile book detail player updates during the swipe");
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(75));
        dock.update(cx, |dock, cx| {
            dock.inline = false;
            cx.notify();
        });
        cx.run_until_parked();
        scroll(cx, -60.0, 0.0, TouchPhase::Started);
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(75));
    }

    #[gpui::test]
    fn swipe_reverses_live_and_clamps_at_book_boundaries(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx, true);
        scroll(cx, -80.0, 0.0, TouchPhase::Started);
        assert_eq!(position(&host, cx), Duration::from_secs(80));
        scroll(cx, 40.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(70));
        scroll(cx, 400.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::ZERO);
        scroll(cx, -40.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(10), "reversing at the boundary responds immediately");
        scroll(cx, -2000.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(300));
        scroll(cx, 20.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(295));
        scroll(cx, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(position(&host, cx), Duration::from_secs(295));
    }

    #[gpui::test]
    fn unphased_swipes_update_continuously_and_commit_after_idle(cx: &mut TestAppContext) {
        let (host, cx) = setup(cx, true);
        scroll(cx, -24.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(66));
        scroll(cx, -12.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(69));
        host.read_with(cx, |host, _| assert!(host.positions.is_empty()));
        cx.executor().advance_clock(SEEK_IDLE_DELAY);
        cx.run_until_parked();
        host.read_with(cx, |host, cx| {
            assert_eq!(host.positions.len(), 1);
            assert!(!host.dock.read(cx).seek_swipe.is_active());
        });
        assert_eq!(position(&host, cx), Duration::from_secs(69));
        scroll(cx, 12.0, 0.0, TouchPhase::Moved);
        assert_eq!(position(&host, cx), Duration::from_secs(66));
    }
}
