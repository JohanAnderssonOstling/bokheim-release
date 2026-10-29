use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, EntityId, HitboxBehavior, Image, ImageFormat, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderOnce, ScrollWheelEvent, Subscription, Window,
    canvas, div, point, px, size,
};
use gpui_component::{WindowExt, notification::Notification};
use std::io::Cursor;
use ui_components::BrowserTheme;

#[derive(Default)]
struct Selection {
    drag: Option<Drag>,
    swallow_release: bool,
    busy: bool,
    _escape: Option<Subscription>,
}

#[derive(Clone, Copy)]
struct Drag {
    start: Point<Pixels>,
    end: Point<Pixels>,
    viewport: Bounds<Pixels>,
}

impl Drag {
    fn rectangle(self) -> Bounds<Pixels> {
        let clamp = |position: Point<Pixels>| point(position.x.clamp(self.viewport.left(), self.viewport.right()), position.y.clamp(self.viewport.top(), self.viewport.bottom()));
        let start = clamp(self.start);
        let end = clamp(self.end);
        Bounds::new(point(start.x.min(end.x), start.y.min(end.y)), size((end.x - start.x).abs(), (end.y - start.y).abs()))
    }
}

#[derive(IntoElement)]
struct ScreenshotDocument {
    document: AnyElement,
    owner: EntityId,
    theme: BrowserTheme,
}

impl RenderOnce for ScreenshotDocument {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(("reader-screenshot", self.owner), cx, |window, cx: &mut Context<Selection>| {
            let target = cx.weak_entity();
            let owner_window = window.window_handle();
            let escape = cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != owner_window {
                    return;
                }
                let Some(target) = target.upgrade() else { return };
                if target.read(cx).swallow_release {
                    if event.keystroke.key == "escape" {
                        target.update(cx, |state: &mut Selection, cx| {
                            state.drag = None;
                            cx.notify();
                        });
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                }
            });
            cx.observe_window_activation(window, |state: &mut Selection, window, cx| {
                if !window.is_window_active() {
                    state.drag = None;
                    state.swallow_release = false;
                    cx.notify();
                }
            })
            .detach();
            Selection { _escape: Some(escape), ..Default::default() }
        });
        let selection = state.read(cx).drag.map(|drag| (drag.rectangle(), drag.viewport));
        let events = state.clone();
        let tracker = canvas(
            |bounds, window, _| (bounds, window.insert_hitbox(bounds, HitboxBehavior::Normal)),
            move |_, (bounds, hitbox), window, _| {
                let begin = events.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture() || !hitbox.is_hovered(window) || event.button != MouseButton::Left || !event.modifiers.control {
                        return;
                    }
                    begin.update(cx, |state, cx| {
                        if !state.busy {
                            state.drag = Some(Drag { start: event.position, end: event.position, viewport: bounds });
                        }
                        state.swallow_release = true;
                        cx.notify();
                    });
                    window.prevent_default();
                    cx.stop_propagation();
                });
                let movement = events.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if !phase.capture() || !movement.read(cx).swallow_release {
                        return;
                    }
                    movement.update(cx, |state, cx| {
                        if let Some(drag) = &mut state.drag {
                            drag.end = event.position;
                            cx.notify();
                        }
                    });
                    cx.stop_propagation();
                });
                let scrolling = events.clone();
                window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, cx| {
                    if phase.capture() && scrolling.read(cx).swallow_release {
                        cx.stop_propagation();
                    }
                });
                let finish = events.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if !phase.capture() || event.button != MouseButton::Left || !finish.read(cx).swallow_release {
                        return;
                    }
                    let rectangle = finish.update(cx, |state, cx| {
                        state.swallow_release = false;
                        let rectangle = state
                            .drag
                            .take()
                            .map(|mut drag| {
                                drag.end = event.position;
                                drag.rectangle()
                            })
                            .filter(|bounds| bounds.size.width >= px(2.0) && bounds.size.height >= px(2.0));
                        if rectangle.is_some() {
                            state.busy = true;
                        }
                        cx.notify();
                        rectangle
                    });
                    window.prevent_default();
                    cx.stop_propagation();
                    let Some(rectangle) = rectangle else { return };
                    let capture = window.capture_region(rectangle, cx);
                    let target = finish.downgrade();
                    window
                        .spawn(cx, async move |cx| {
                            let result = match capture.await {
                                Ok(pixels) => {
                                    cx.background_executor()
                                        .spawn(async move {
                                            let mut encoded = Cursor::new(Vec::new());
                                            image::DynamicImage::ImageRgba8(pixels).write_to(&mut encoded, image::ImageFormat::Png).map_err(|error| error.to_string())?;
                                            Ok(Image::from_bytes(ImageFormat::Png, encoded.into_inner()))
                                        })
                                        .await
                                }
                                Err(error) => Err(error.to_string()),
                            };
                            if let Err(error) = cx.update(|window, cx| {
                                let Some(target) = target.upgrade() else { return };
                                target.update(cx, |state, cx| {
                                    state.busy = false;
                                    cx.notify();
                                });
                                match result {
                                    Ok(image) => {
                                        cx.write_to_clipboard(ClipboardItem::new_image(&image));
                                        window.push_notification(Notification::success("Screenshot copied to clipboard").id::<Selection>(), cx);
                                    }
                                    Err(error) => {
                                        log::warn!("Reader screenshot failed: {error}");
                                        window.push_notification(Notification::error(format!("Could not capture screenshot: {error}")).id::<Selection>(), cx);
                                    }
                                }
                            }) {
                                log::debug!("Screenshot window closed: {error}");
                            }
                        })
                        .detach();
                });
            },
        )
        .absolute()
        .size_full();
        let document = div()
            .id("reader-screenshot-document")
            .debug_selector(|| "reader-screenshot-document".into())
            .relative()
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            // Register capture listeners before the document's listeners, including
            // on the first frame before a drag has caused any redraw.
            .child(tracker)
            .child(self.document);
        match selection {
            Some((rectangle, viewport)) => document.cursor(gpui::CursorStyle::Crosshair).child(
                div()
                    .id("reader-screenshot-selection")
                    .debug_selector(|| "reader-screenshot-selection".into())
                    .absolute()
                    .left(rectangle.left() - viewport.left())
                    .top(rectangle.top() - viewport.top())
                    .w(rectangle.size.width)
                    .h(rectangle.size.height)
                    .border_1()
                    .border_color(self.theme.accent)
                    .bg(self.theme.accent.opacity(0.12)),
            ),
            None => document,
        }
    }
}

pub(super) fn document(document: AnyElement, owner: EntityId, theme: BrowserTheme) -> AnyElement {
    ScreenshotDocument { document, owner, theme }.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, Modifiers, Render, TestAppContext, VisualTestContext};

    struct DocumentTest {
        events: Vec<&'static str>,
        selection: Option<Entity<Selection>>,
    }

    impl Render for DocumentTest {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let document = div()
                .id("test-document")
                .size_full()
                .on_mouse_down(MouseButton::Left, cx.listener(|view, _, _, _| view.events.push("down")))
                .on_mouse_move(cx.listener(|view, _, _, _| view.events.push("move")))
                .on_mouse_up(MouseButton::Left, cx.listener(|view, _, _, _| view.events.push("up")))
                .on_click(cx.listener(|view, _, _, _| view.events.push("click")));
            let document = ScreenshotDocument { document: document.into_any_element(), owner: cx.entity_id(), theme: BrowserTheme::default() }.render(window, cx).into_any_element();
            self.selection = Some(window.use_keyed_state(("reader-screenshot", cx.entity_id()), cx, |_, _| panic!("screenshot state was not initialized")));
            document
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<DocumentTest>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| DocumentTest { events: Vec::new(), selection: None });
        cx.simulate_resize(size(px(400.0), px(300.0)));
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui::test]
    fn ctrl_drag_captures_input_and_escape_cancels_until_release(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        let control = Modifiers { control: true, ..Default::default() };
        cx.simulate_mouse_down(point(px(250.0), px(200.0)), MouseButton::Left, control);
        cx.simulate_mouse_move(point(px(50.0), px(40.0)), Some(MouseButton::Left), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("reader-screenshot-selection").unwrap(), Bounds::new(point(px(50.0), px(40.0)), size(px(200.0), px(160.0))));
        assert!(view.read_with(cx, |view, _| view.events.is_empty()));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-screenshot-selection").is_none());
        cx.simulate_mouse_up(point(px(50.0), px(40.0)), MouseButton::Left, Modifiers::default());
        assert!(view.read_with(cx, |view, _| view.events.is_empty()));
        assert!(cx.read_from_clipboard().is_none());
        cx.simulate_click(point(px(20.0), px(20.0)), Modifiers::default());
        let mut events = view.read_with(cx, |view, _| view.events.clone());
        events.sort_unstable();
        assert_eq!(events, vec!["click", "down", "up"]);
    }

    #[gpui::test]
    fn control_click_without_a_rectangle_leaves_clipboard_unchanged(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("keep".into())));
        let control = Modifiers { control: true, ..Default::default() };
        cx.simulate_click(point(px(20.0), px(20.0)), control);
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-screenshot-selection").is_none());
        assert_eq!(cx.read_from_clipboard().unwrap().text().as_deref(), Some("keep"));
    }

    #[gpui::test]
    fn busy_capture_ignores_followup_gestures_until_completion(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        let selection = view.read_with(cx, |view, _| view.selection.clone().unwrap());
        selection.update(cx, |state, _| state.busy = true);
        let control = Modifiers { control: true, ..Default::default() };
        cx.simulate_click(point(px(20.0), px(20.0)), control);
        assert!(selection.read_with(cx, |state, _| state.busy));
        cx.simulate_mouse_down(point(px(20.0), px(20.0)), MouseButton::Left, control);
        cx.simulate_mouse_move(point(px(80.0), px(60.0)), Some(MouseButton::Left), control);
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-screenshot-selection").is_none());
        cx.simulate_mouse_up(point(px(80.0), px(60.0)), MouseButton::Left, control);
        assert!(selection.read_with(cx, |state, _| state.busy && state.drag.is_none() && !state.swallow_release));
        assert!(view.read_with(cx, |view, _| view.events.is_empty()));
        assert!(cx.read_from_clipboard().is_none());

        selection.update(cx, |state, _| state.busy = false);
        cx.simulate_mouse_down(point(px(20.0), px(20.0)), MouseButton::Left, control);
        cx.simulate_mouse_move(point(px(80.0), px(60.0)), Some(MouseButton::Left), control);
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-screenshot-selection").is_some());
        cx.simulate_keystrokes("escape");
        cx.simulate_mouse_up(point(px(80.0), px(60.0)), MouseButton::Left, control);
    }

    #[test]
    fn rectangle_handles_reverse_drag_and_clips_to_document() {
        let viewport = Bounds::new(point(px(100.0), px(50.0)), size(px(300.0), px(200.0)));
        let drag = Drag { start: point(px(350.0), px(200.0)), end: point(px(20.0), px(10.0)), viewport };
        assert_eq!(drag.rectangle(), Bounds::new(point(px(100.0), px(50.0)), size(px(250.0), px(150.0))));
    }
}
