//! A pannable, zoomable surface.
//!
//! Children are laid out in world coordinates and moved together by one element
//! transform, so gpui paints, rasterizes and hit-tests them where they appear. The
//! children stay passive: the canvas owns every pointer gesture and reports taps,
//! context clicks and hover as world points for the page to hit-test against its
//! own layout.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Bounds, ElementTransform, Entity, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Point, RenderOnce, ScrollDelta, StatefulInteractiveElement, Styled, Window,
    canvas, div, point, px, size,
};

use crate::browser_theme;

/// Movement, in screen pixels, after which a press becomes a pan instead of a tap.
const TAP_SLOP: f32 = 6.0;
/// Zoom change per mouse-wheel line.
const ZOOM_PER_LINE: f32 = 1.12;
/// Zoom exponent per screen pixel of a smooth scroll gesture.
const ZOOM_PER_PIXEL: f32 = 0.0015;

/// Where the world sits on screen: a world point `p` is drawn at `pan + p * zoom`,
/// in pixels relative to the canvas origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasCamera {
    pub pan: Point<Pixels>,
    pub zoom: f32,
}

impl CanvasCamera {
    /// Canvas-relative screen position of a world point.
    pub fn to_screen(&self, world: Point<Pixels>) -> Point<Pixels> {
        self.pan + point(world.x * self.zoom, world.y * self.zoom)
    }

    /// World point under a canvas-relative screen position.
    pub fn to_world(&self, screen: Point<Pixels>) -> Point<Pixels> {
        point((screen.x - self.pan.x) / self.zoom, (screen.y - self.pan.y) / self.zoom)
    }

    /// The same camera at a new zoom, keeping the world point under `anchor` in place.
    fn zoomed_about(self, anchor: Point<Pixels>, zoom: f32) -> Self {
        let world = self.to_world(anchor);
        Self { pan: anchor - point(world.x * zoom, world.y * zoom), zoom }
    }

    fn element_transform(&self) -> ElementTransform {
        ElementTransform::default()
            .translate(self.pan)
            .scale(size(self.zoom, self.zoom))
            .origin(point(0.0, 0.0))
    }
}

impl Default for CanvasCamera {
    fn default() -> Self {
        Self { pan: point(px(0.0), px(0.0)), zoom: 1.0 }
    }
}

/// A press in progress, in window coordinates.
#[derive(Clone, Copy)]
struct Press {
    start: Point<Pixels>,
    camera: CanvasCamera,
    panning: bool,
}

/// State behind an [`infinite_canvas`]: the camera, its zoom limits and the
/// gesture in progress. The page owns it and sets the camera to frame content.
pub struct InfiniteCanvas {
    pub camera: CanvasCamera,
    min_zoom: f32,
    max_zoom: f32,
    /// Where the canvas was laid out in the window last frame; `None` before its first layout.
    pub bounds: Option<Bounds<Pixels>>,
    press: Option<Press>,
}

impl InfiniteCanvas {
    pub fn new(min_zoom: f32, max_zoom: f32) -> Self {
        Self { camera: CanvasCamera::default(), min_zoom, max_zoom, bounds: None, press: None }
    }

    fn local(&self, window_position: Point<Pixels>) -> Point<Pixels> {
        self.bounds.map_or(window_position, |bounds| window_position - bounds.origin)
    }

    fn world(&self, window_position: Point<Pixels>) -> Point<Pixels> {
        self.camera.to_world(self.local(window_position))
    }

    fn zoom_at(&mut self, window_position: Point<Pixels>, zoom: f32) {
        let zoom = zoom.clamp(self.min_zoom, self.max_zoom);
        self.camera = self.camera.zoomed_about(self.local(window_position), zoom);
    }

    /// Follows a press. Returns whether the camera moved and should repaint now;
    /// on e-ink the pan is applied only when the press ends.
    fn drag_to(&mut self, position: Point<Pixels>) -> bool {
        let Some(press) = self.press.as_mut() else { return false };
        let delta = position - press.start;
        if !press.panning && f32::from(delta.x).hypot(f32::from(delta.y)) < TAP_SLOP {
            return false;
        }
        press.panning = true;
        if cfg!(feature = "kobo") {
            return false;
        }
        self.camera = CanvasCamera { pan: press.camera.pan + delta, ..press.camera };
        true
    }

    /// Ends a press. Returns the world point of a tap, or `None` when the press panned.
    fn release(&mut self, position: Point<Pixels>) -> Option<Point<Pixels>> {
        let press = self.press.take()?;
        if press.panning {
            self.camera = CanvasCamera { pan: press.camera.pan + (position - press.start), ..press.camera };
            return None;
        }
        Some(self.world(position))
    }
}

type PointHandler = Rc<dyn Fn(Point<Pixels>, &mut Window, &mut App)>;
type HoverHandler = Rc<dyn Fn(Option<Point<Pixels>>, &mut Window, &mut App)>;

/// A pannable, zoomable view of `state`'s world. Add children positioned in world
/// coordinates; they must not handle pointer input themselves.
pub fn infinite_canvas(state: &Entity<InfiniteCanvas>) -> InfiniteCanvasView {
    InfiniteCanvasView { state: state.clone(), children: Vec::new(), on_tap: None, on_context: None, on_hover: None, debug_zoom: false }
}

#[derive(IntoElement)]
pub struct InfiniteCanvasView {
    state: Entity<InfiniteCanvas>,
    children: Vec<AnyElement>,
    on_tap: Option<PointHandler>,
    on_context: Option<PointHandler>,
    on_hover: Option<HoverHandler>,
    debug_zoom: bool,
}

impl InfiniteCanvasView {
    /// Temporarily shows the live camera scale above the canvas content.
    pub fn debug_zoom(mut self) -> Self {
        self.debug_zoom = true;
        self
    }

    /// A press released without panning, at a world point.
    pub fn on_tap(mut self, handler: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static) -> Self {
        self.on_tap = Some(Rc::new(handler));
        self
    }

    /// A secondary click, at a world point.
    pub fn on_context(mut self, handler: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static) -> Self {
        self.on_context = Some(Rc::new(handler));
        self
    }

    /// The pointer moving without a press, at a world point, or `None` when it leaves.
    pub fn on_hover(mut self, handler: impl Fn(Option<Point<Pixels>>, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Rc::new(handler));
        self
    }
}

impl ParentElement for InfiniteCanvasView {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for InfiniteCanvasView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = browser_theme(cx);
        let state = self.state;
        let camera = state.read(cx).camera;
        let debug_zoom = self.debug_zoom;
        div()
            .id("infinite-canvas")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.page_bg)
            .on_mouse_down(MouseButton::Left, {
                let state = state.clone();
                move |event, _, cx| {
                    state.update(cx, |canvas, _| {
                        canvas.press = Some(Press { start: event.position, camera: canvas.camera, panning: false });
                    });
                }
            })
            .on_mouse_move({
                let state = state.clone();
                let on_hover = self.on_hover.clone();
                move |event, window, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        state.update(cx, |canvas, cx| {
                            if canvas.drag_to(event.position) {
                                cx.notify();
                            }
                        });
                    } else if let Some(on_hover) = &on_hover {
                        let world = state.read(cx).world(event.position);
                        on_hover(Some(world), window, cx);
                    }
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let state = state.clone();
                let on_tap = self.on_tap.clone();
                move |event, window, cx| {
                    let tap = state.update(cx, |canvas, cx| {
                        let tap = canvas.release(event.position);
                        cx.notify();
                        tap
                    });
                    if let (Some(world), Some(on_tap)) = (tap, &on_tap) {
                        on_tap(world, window, cx);
                    }
                }
            })
            .on_mouse_up_out(MouseButton::Left, {
                let state = state.clone();
                move |event, _, cx| {
                    state.update(cx, |canvas, cx| {
                        canvas.release(event.position);
                        cx.notify();
                    });
                }
            })
            .when_some(self.on_context, |this, on_context| {
                let state = state.clone();
                this.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    let world = state.read(cx).world(event.position);
                    on_context(world, window, cx);
                })
            })
            .when_some(self.on_hover, |this, on_hover| {
                this.on_hover(move |hovered, window, cx| {
                    if !hovered {
                        on_hover(None, window, cx);
                    }
                })
            })
            // Wheel and trackpad scrolling both zoom around the pointer.
            .on_scroll_wheel({
                let state = state.clone();
                move |event, _, cx| {
                    state.update(cx, |canvas, cx| {
                        match event.delta {
                            ScrollDelta::Lines(lines) => {
                                let zoom = canvas.camera.zoom * ZOOM_PER_LINE.powf(lines.y);
                                canvas.zoom_at(event.position, zoom);
                            }
                            ScrollDelta::Pixels(delta) => {
                                if cfg!(target_os = "android") {
                                    // Android sends a one-finger drag as pixel scrolling.
                                    canvas.camera.pan += delta;
                                } else {
                                    let zoom = canvas.camera.zoom * (f32::from(delta.y) * ZOOM_PER_PIXEL).exp();
                                    canvas.zoom_at(event.position, zoom);
                                }
                            }
                        }
                        cx.notify();
                    });
                    cx.stop_propagation();
                }
            })
            .on_pinch({
                let state = state.clone();
                move |event, _, cx| {
                    state.update(cx, |canvas, cx| {
                        let zoom = canvas.camera.zoom * (1.0 + event.delta);
                        canvas.zoom_at(event.position, zoom);
                        cx.notify();
                    });
                }
            })
            .child(
                canvas(
                    {
                        let state = state.clone();
                        move |bounds, _, cx| state.update(cx, |canvas, _| canvas.bounds = Some(bounds))
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .transform(camera.element_transform())
                    .children(self.children),
            )
            .when(debug_zoom, |canvas| {
                canvas.child(
                    div()
                        .absolute()
                        .top(px(12.0))
                        .left(px(12.0))
                        .px(px(8.0))
                        .py(px(5.0))
                        .rounded(px(4.0))
                        .bg(theme.raised_bg)
                        .border_1()
                        .border_color(theme.rule)
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(format!("zoom {:.2}×", camera.zoom)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_and_screen_positions_round_trip() {
        let camera = CanvasCamera { pan: point(px(40.0), px(-12.0)), zoom: 1.5 };
        let world = point(px(10.0), px(20.0));
        assert_eq!(camera.to_screen(world), point(px(55.0), px(18.0)));
        assert_eq!(camera.to_world(camera.to_screen(world)), world);
    }

    #[test]
    fn zooming_keeps_the_anchor_on_the_same_world_point() {
        let camera = CanvasCamera { pan: point(px(30.0), px(30.0)), zoom: 1.0 };
        let anchor = point(px(100.0), px(80.0));
        let zoomed = camera.zoomed_about(anchor, 2.0);
        assert_eq!(zoomed.zoom, 2.0);
        assert_eq!(zoomed.to_world(anchor), camera.to_world(anchor));
    }

    #[test]
    fn a_short_press_is_a_tap_and_a_long_one_pans() {
        let mut canvas = InfiniteCanvas::new(0.25, 4.0);
        let start = point(px(50.0), px(50.0));
        canvas.press = Some(Press { start, camera: canvas.camera, panning: false });
        assert!(!canvas.drag_to(start + point(px(2.0), px(2.0))));
        assert_eq!(canvas.release(start), Some(start));

        canvas.press = Some(Press { start, camera: canvas.camera, panning: false });
        canvas.drag_to(start + point(px(30.0), px(0.0)));
        assert_eq!(canvas.release(start + point(px(30.0), px(0.0))), None);
        assert_eq!(canvas.camera.pan, point(px(30.0), px(0.0)));
    }
}
