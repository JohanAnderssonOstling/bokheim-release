use std::cell::Cell;
use std::panic::Location;
use std::rc::Rc;

use gpui::{
    App, Bounds, Div, Element, ElementId, Global, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce, ScrollHandle, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Window, canvas, div, point,
};
use gpui_component::StyledExt;
use gpui_component::scroll::{Scrollbar, ScrollbarAxis};

struct RegisteredScroll {
    id: ElementId,
    handle: ScrollHandle,
}

struct PageScrollRegistry {
    scrolls: Vec<RegisteredScroll>,
    _keystroke_subscription: Subscription,
}

impl Global for PageScrollRegistry {}

/// Installs the fallback that routes unhandled paging keys to the most
/// recently rendered vertical scroll surface. Reader key bindings run first,
/// so their page-turn semantics remain authoritative.
pub fn init_page_scrolling(cx: &mut App) {
    if cx.has_global::<PageScrollRegistry>() {
        return;
    }
    let subscription = cx.observe_keystrokes(|event, window, cx| {
        if event.action.is_some() || event.keystroke.modifiers != gpui::Modifiers::default() {
            return;
        }
        let Some(direction) = page_direction(&event.keystroke.key) else {
            return;
        };
        let handle = cx.global::<PageScrollRegistry>().scrolls.iter().rev().find_map(|registered| {
            let bounds = registered.handle.bounds();
            (bounds.size.height > gpui::Pixels::ZERO && registered.handle.max_offset().y > gpui::Pixels::ZERO).then(|| registered.handle.clone())
        });
        if let Some(handle) = handle {
            page_scroll(&handle, direction, window, cx);
        }
    });
    cx.set_global(PageScrollRegistry { scrolls: Vec::new(), _keystroke_subscription: subscription });
}

/// Registers an externally managed vertical scroll handle for application-wide
/// viewport paging.
pub fn register_page_scroll_handle(id: impl Into<ElementId>, handle: ScrollHandle, cx: &mut App) {
    if !cx.has_global::<PageScrollRegistry>() {
        return;
    }
    let id = id.into();
    let registry = cx.global_mut::<PageScrollRegistry>();
    registry.scrolls.retain(|registered| registered.id != id);
    registry.scrolls.push(RegisteredScroll { id, handle });
}

/// Adds Bokheim's keyboard paging behavior to a scrollable element.
pub trait ScrollableElement: InteractiveElement + Styled + ParentElement + Element {
    /// Adds a vertical scrollbar. Left pages up and right pages down by the
    /// scroll area's measured viewport height.
    #[track_caller]
    fn overflow_y_scrollbar(self) -> Scrollable<Self> {
        Scrollable::new(self)
    }

    /// Adds vertical scrolling without drawing a bar inside the surface.
    fn overflow_y_scroll_hidden(self) -> Scrollable<Self> {
        Scrollable::new(self).hide_scrollbar()
    }

    /// Adds a vertical scrollbar driven by a scroll handle the caller owns, for
    /// surfaces that have to move the offset themselves — bringing a control
    /// reached with the keyboard into view, for one.
    #[track_caller]
    fn overflow_y_scrollbar_with_handle(self, handle: &ScrollHandle) -> Scrollable<Self> {
        Scrollable { handle: Some(handle.clone()), ..Scrollable::new(self) }
    }

    /// Adds a vertical scrollbar with a stable identity supplied by the
    /// caller. This is useful for repeated scroll surfaces rendered from the
    /// same source location, such as the columns in a folder picker.
    fn overflow_y_scrollbar_with_id(self, id: impl Into<ElementId>) -> Scrollable<Self> {
        Scrollable { id: id.into(), ..Scrollable::new(self) }
    }
}

/// A vertical scroll container with an overlaid scrollbar and viewport paging.
#[derive(IntoElement)]
pub struct Scrollable<E: InteractiveElement + Styled + ParentElement + Element> {
    id: ElementId,
    element: E,
    show_scrollbar: bool,
    /// A handle supplied by the caller, for surfaces that scroll themselves.
    /// Without one the surface keeps its own offset across frames.
    handle: Option<ScrollHandle>,
}

impl<E> Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    #[track_caller]
    fn new(element: E) -> Self {
        Self { id: ElementId::CodeLocation(*Location::caller()), element, show_scrollbar: true, handle: None }
    }

    /// Keep scrolling and the surface's stable identity without drawing a bar.
    pub fn hide_scrollbar(mut self) -> Self {
        self.show_scrollbar = false;
        self
    }
}

impl<E> Styled for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    fn style(&mut self) -> &mut StyleRefinement {
        self.element.style()
    }
}

impl<E> ParentElement for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.element.extend(elements);
    }
}

impl<E> InteractiveElement for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element,
{
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.element.interactivity()
    }
}

impl<E> RenderOnce for Scrollable<E>
where
    E: InteractiveElement + Styled + ParentElement + Element + 'static,
{
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll_handle = match self.handle.take() {
            Some(handle) => handle,
            None => window.use_keyed_state(self.id.clone(), cx, |_, _| ScrollHandle::default()).read(cx).clone(),
        };
        register_page_scroll_handle(self.id.clone(), scroll_handle.clone(), cx);
        let root_style = outer_layout_style(&mut self.element);

        let content = self.element.id((self.id.clone(), "content")).flex_none().h_auto().min_h_full();
        let scroll_area = div().id((self.id.clone(), "area")).size_full().flex().track_scroll(&scroll_handle).flex_col().overflow_y_scroll().child(content);

        // Scrollbar is absolute but supplies no inset of its own. Anchor its
        // layer to the viewport so its static position cannot follow the
        // preceding scroll area and extend below a popup.
        let scrollbar = self.show_scrollbar.then(|| div().absolute().inset_0().child(Scrollbar::new(&scroll_handle).id((self.id.clone(), "scrollbar")).axis(ScrollbarAxis::Vertical)));
        div().id(self.id).size_full().relative().refine_style(&root_style).on_key_down(page_with_arrow_keys(scroll_handle.clone())).child(scroll_area).children(scrollbar)
    }
}

impl ScrollableElement for Div {}
impl<E> ScrollableElement for Stateful<E>
where
    E: ParentElement + Styled + Element,
    Self: InteractiveElement,
{
}

/// Records where the element it is placed in ended up, so a caller that does not
/// own the layout can still scroll that element into view.
///
/// It is positioned absolutely and paints nothing, so it measures its parent
/// without taking part in the parent's own layout. The parent must be
/// `relative()` for the measurement to be of the parent rather than of the
/// nearest positioned ancestor.
pub fn measured_bounds(bounds: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(move |laid_out, _, _| bounds.set(laid_out), |_, _: (), _, _| {}).absolute().size_full()
}
/// Scrolls `handle` the shortest distance that brings `target` inside the
/// viewport, with `margin` of room left around it. Returns whether the offset
/// moved.
///
/// Both rectangles are in window coordinates, which is what the scroll handle
/// and [`measured_bounds`] report, so the correction is the plain difference
/// between the two edges rather than anything expressed in content space.
///
/// A target taller than the viewport is aligned to its top edge: the start of a
/// section is what the keyboard has just arrived at.
pub fn scroll_to_reveal(handle: &ScrollHandle, target: Bounds<Pixels>, margin: Pixels) -> bool {
    let viewport = handle.bounds();
    if viewport.size.height <= Pixels::ZERO || handle.max_offset().y <= Pixels::ZERO {
        return false;
    }
    let above = (viewport.top() + margin) - target.top();
    let below = target.bottom() - (viewport.bottom() - margin);
    let correction = if above > Pixels::ZERO {
        above
    } else if below > Pixels::ZERO {
        -below.min(above.abs())
    } else {
        return false;
    };
    let offset = handle.offset();
    handle.set_offset(point(offset.x, offset.y + correction));
    handle.offset() != offset
}

fn page_with_arrow_keys(scroll_handle: ScrollHandle) -> impl Fn(&gpui::KeyDownEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        if event.keystroke.modifiers != gpui::Modifiers::default() {
            return;
        }

        let Some(direction) = page_direction(&event.keystroke.key) else { return };
        page_scroll(&scroll_handle, direction, window, cx);
    }
}

fn page_direction(key: &str) -> Option<f32> {
    match key {
        "left" | "pageup" => Some(1.0),
        "right" | "pagedown" => Some(-1.0),
        _ => None,
    }
}

fn page_scroll(scroll_handle: &ScrollHandle, direction: f32, window: &mut Window, cx: &mut App) {
    let bounds = scroll_handle.bounds();
    if bounds.size.height <= gpui::Pixels::ZERO || scroll_handle.max_offset().y <= gpui::Pixels::ZERO {
        return;
    }

    if scroll_handle.scroll_by_page(point(0.0, direction), window, cx) {
        cx.stop_propagation();
    }
}

fn outer_layout_style<E: Styled>(element: &mut E) -> StyleRefinement {
    let style = element.style();
    // Scrollable renders an extra viewport wrapper around the supplied
    // element. Positioning belongs to that outer layout box; leaving it on the
    // inner content makes an `absolute()` scroll surface participate in its
    // parent's flex layout and positions only the content inside the wrapper.
    let position = style.position.take();
    let inset = std::mem::take(&mut style.inset);
    StyleRefinement {
        position,
        inset,
        size: style.size.clone(),
        min_size: style.min_size.clone(),
        max_size: style.max_size.clone(),
        flex_grow: style.flex_grow,
        flex_shrink: style.flex_shrink,
        flex_basis: style.flex_basis,
        align_self: style.align_self,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, px};

    struct RepeatedScrolls;

    impl Render for RepeatedScrolls {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(280.0)).h(px(320.0)).flex().flex_col().children([("first-scroll", false), ("second-scroll", true)].map(|(id, hidden)| {
                let surface = div().w_full().h(px(160.0)).flex_none().child(div().h(px(2400.0)).flex_none()).overflow_y_scrollbar_with_id(id);
                if hidden { surface.hide_scrollbar() } else { surface }
            }))
        }
    }

    #[gpui::test]
    fn repeated_explicit_scroll_ids_retain_independent_offsets_with_hidden_bars(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        cx.update(gpui_component::init);
        cx.update(init_page_scrolling);
        let (surface, visual) = cx.add_window_view(|_, _| RepeatedScrolls);
        visual.run_until_parked();
        let handles = visual.update(|_, cx| ["first-scroll", "second-scroll"].map(|id| cx.global::<PageScrollRegistry>().scrolls.iter().find(|registered| registered.id == ElementId::from(id)).unwrap().handle.clone()));
        for handle in &handles {
            assert_eq!(handle.bounds().size.height, px(160.0));
            assert!(handle.max_offset().y > Pixels::ZERO);
        }
        visual.simulate_event(ScrollWheelEvent { position: handles[0].bounds().center(), delta: ScrollDelta::Pages(point(0.0, -1.0)), touch_phase: TouchPhase::Ended, ..Default::default() });
        visual.run_until_parked();
        assert_eq!(handles[0].offset().y, px(-160.0));
        assert_eq!(handles[1].offset().y, Pixels::ZERO);
        visual.simulate_event(ScrollWheelEvent { position: handles[1].bounds().center(), delta: ScrollDelta::Pixels(point(px(0.0), px(-37.0))), touch_phase: TouchPhase::Ended, ..Default::default() });
        visual.run_until_parked();
        assert_eq!(handles[0].offset().y, px(-160.0));
        assert_eq!(handles[1].offset().y, px(-37.0), "a hidden bar must retain ordinary scrolling");
        surface.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        let retained = visual.update(|_, cx| ["first-scroll", "second-scroll"].map(|id| cx.global::<PageScrollRegistry>().scrolls.iter().find(|registered| registered.id == ElementId::from(id)).unwrap().handle.offset().y));
        assert_eq!(retained, [px(-160.0), px(-37.0)], "rebuilding repeated surfaces must keep each explicit ID's own offset");
        visual.simulate_event(ScrollWheelEvent { position: handles[0].bounds().center(), delta: ScrollDelta::Pages(point(0.0, -1.0)), touch_phase: TouchPhase::Ended, ..Default::default() });
        visual.run_until_parked();
        assert_eq!(handles[0].offset().y, px(-320.0));
        assert_eq!(handles[1].offset().y, px(-37.0));
    }

    struct PagingSurface {
        height: Pixels,
        handle: ScrollHandle,
        wrapped: bool,
        mobile_paging: bool,
    }

    impl Render for PagingSurface {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            window.set_page_scroll_on_swipe(self.mobile_paging);
            let content = div().w_full().h(px(2400.0)).flex_none();
            let surface = if self.wrapped {
                div().child(content).overflow_y_scrollbar_with_handle(&self.handle).into_any_element()
            } else {
                div().id("native-scroll").size_full().track_scroll(&self.handle).overflow_y_scroll().child(content).into_any_element()
            };
            div().w(px(280.0)).h(self.height).child(surface)
        }
    }

    #[gpui::test]
    fn page_requests_use_each_surfaces_viewport_and_preserve_pixel_scrolling(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        cx.update(gpui_component::init);
        for wrapped in [false, true] {
            for height in [160.0, 480.0] {
                let handle = ScrollHandle::default();
                let (_, visual) = cx.add_window_view(|_, _| PagingSurface { height: px(height), handle: handle.clone(), wrapped, mobile_paging: false });
                visual.run_until_parked();
                let bounds = handle.bounds();
                assert_eq!(bounds.size.height, px(height));
                let scroll = |delta| ScrollWheelEvent { position: bounds.center(), delta, touch_phase: TouchPhase::Ended, ..Default::default() };
                visual.simulate_event(scroll(ScrollDelta::Pages(point(0.0, -1.0))));
                visual.run_until_parked();
                assert_eq!(handle.offset().y, px(-height));
                assert_eq!(bounds.top() - handle.offset().y, bounds.bottom());
                visual.simulate_event(scroll(ScrollDelta::Pixels(point(px(0.0), px(-37.0)))));
                visual.run_until_parked();
                assert_eq!(handle.offset().y, px(-height - 37.0));
                visual.simulate_event(scroll(ScrollDelta::Pages(point(0.0, 1.0))));
                visual.run_until_parked();
                assert!((f32::from(handle.offset().y) + 37.0).abs() < 0.01);
                visual.simulate_event(scroll(ScrollDelta::Pages(point(0.0, -100.0))));
                visual.run_until_parked();
                assert_eq!(handle.offset().y, -handle.max_offset().y);
                visual.simulate_event(scroll(ScrollDelta::Pages(point(0.0, 100.0))));
                visual.run_until_parked();
                assert_eq!(handle.offset().y, Pixels::ZERO);
            }
        }
    }
    #[gpui::test]
    fn mobile_swipes_track_the_finger_then_settle_once_and_cancel_cleanly(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        cx.update(gpui_component::init);
        for wrapped in [false, true] {
            let handle = ScrollHandle::default();
            let (_, visual) = cx.add_window_view(|_, _| PagingSurface { height: px(240.0), handle: handle.clone(), wrapped, mobile_paging: true });
            visual.run_until_parked();
            let bounds = handle.bounds();
            let scroll = |dy, phase| ScrollWheelEvent { position: bounds.center(), delta: ScrollDelta::Pixels(point(px(0.0), px(dy))), touch_phase: phase, ..Default::default() };
            visual.simulate_event(scroll(-20.0, TouchPhase::Started));
            visual.simulate_event(scroll(-140.0, TouchPhase::Moved));
            visual.run_until_parked();
            assert_eq!(handle.offset().y, px(-160.0), "movement must be visible before release");
            visual.simulate_event(ScrollWheelEvent { position: point(px(400.0), px(500.0)), ..scroll(0.0, TouchPhase::Ended) });
            visual.run_until_parked();
            assert_eq!(handle.offset().y, -bounds.size.height * 0.9);
            assert_eq!(bounds.top() - handle.offset().y, bounds.bottom() - bounds.size.height * 0.1);
            visual.simulate_event(scroll(20.0, TouchPhase::Started));
            visual.simulate_event(scroll(140.0, TouchPhase::Moved));
            visual.simulate_event(scroll(0.0, TouchPhase::Ended));
            visual.run_until_parked();
            assert_eq!(handle.offset().y, Pixels::ZERO);
            visual.simulate_event(scroll(-160.0, TouchPhase::Started));
            visual.simulate_event(scroll(0.0, TouchPhase::Cancelled));
            visual.simulate_event(scroll(0.0, TouchPhase::Ended));
            visual.run_until_parked();
            assert_eq!(handle.offset().y, Pixels::ZERO);
            visual.simulate_event(scroll(-10.0, TouchPhase::Started));
            visual.simulate_event(scroll(0.0, TouchPhase::Ended));
            visual.run_until_parked();
            assert_eq!(handle.offset().y, Pixels::ZERO);
            visual.simulate_event(scroll(-37.0, TouchPhase::Moved));
            visual.run_until_parked();
            assert_eq!(handle.offset().y, px(-37.0));
        }
    }

    #[gpui::test]
    fn mobile_system_edge_swipes_do_not_scroll_content(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        cx.update(gpui_component::init);
        let handle = ScrollHandle::default();
        let (_, visual) = cx.add_window_view(|window, _| PagingSurface { height: window.viewport_size().height, handle: handle.clone(), wrapped: false, mobile_paging: true });
        visual.run_until_parked();
        let bounds = handle.bounds();
        let scroll = |phase| ScrollWheelEvent { position: point(bounds.center().x, bounds.bottom() - px(1.0)), delta: ScrollDelta::Pixels(point(px(0.0), px(-160.0))), touch_phase: phase, ..Default::default() };
        visual.simulate_event(scroll(TouchPhase::Started));
        visual.simulate_event(scroll(TouchPhase::Ended));
        visual.run_until_parked();
        assert_eq!(handle.offset().y, Pixels::ZERO);
    }

    #[gpui::test]
    fn page_keys_move_the_shared_scroll_surface_without_focus(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        cx.update(gpui_component::init);
        cx.update(init_page_scrolling);
        let handle = ScrollHandle::default();
        let (_, visual) = cx.add_window_view(|_, _| PagingSurface { height: px(240.0), handle: handle.clone(), wrapped: true, mobile_paging: true });
        visual.run_until_parked();
        visual.simulate_keystrokes("pagedown");
        visual.run_until_parked();
        assert_eq!(handle.offset().y, -handle.bounds().size.height);
        visual.simulate_keystrokes("pageup");
        visual.run_until_parked();
        assert_eq!(handle.offset().y, Pixels::ZERO);
    }

    struct InputPagingSurface {
        input: gpui::Entity<gpui_component::input::InputState>,
    }

    impl Render for InputPagingSurface {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            window.set_page_scroll_on_swipe(true);
            div().w(px(280.0)).h(px(160.0)).child(gpui_component::input::Input::new(&self.input).size_full())
        }
    }

    #[gpui::test]
    fn input_swipes_preview_without_adding_drag_distance_to_the_release(cx: &mut TestAppContext) {
        use gpui::AppContext;
        use gpui_component::input::InputState;

        cx.update(gpui_component::init);
        let (surface, visual) = cx.add_window_view(|window, cx| InputPagingSurface { input: cx.new(|cx| InputState::new(window, cx).multi_line(true).default_value("line\n".repeat(100))) });
        visual.run_until_parked();
        let input = visual.update(|_, cx| surface.read(cx).input.clone());
        let position = point(px(30.0), px(30.0));
        visual.simulate_event(ScrollWheelEvent { position, delta: ScrollDelta::Pages(point(0.0, -1.0)), touch_phase: TouchPhase::Ended, ..Default::default() });
        visual.run_until_parked();
        let one_page = visual.update(|_, cx| input.read(cx).scroll_offset());
        assert!(one_page.y < px(-120.0), "the input must have a scrollable viewport");
        visual.update(|_, cx| input.update(cx, |input, cx| input.set_scroll_offset(gpui::Point::default(), cx)));
        visual.run_until_parked();
        let scroll = |dy, touch_phase| ScrollWheelEvent { position, delta: ScrollDelta::Pixels(point(px(0.0), px(dy))), touch_phase, ..Default::default() };
        visual.simulate_event(scroll(-20.0, TouchPhase::Started));
        visual.simulate_event(scroll(-100.0, TouchPhase::Moved));
        visual.run_until_parked();
        assert_eq!(visual.update(|_, cx| input.read(cx).scroll_offset().y), px(-120.0));
        visual.simulate_event(scroll(0.0, TouchPhase::Ended));
        visual.run_until_parked();
        assert_eq!(visual.update(|_, cx| input.read(cx).scroll_offset()), one_page * 0.9);
        visual.simulate_event(scroll(40.0, TouchPhase::Started));
        visual.simulate_event(scroll(0.0, TouchPhase::Cancelled));
        visual.run_until_parked();
        assert_eq!(visual.update(|_, cx| input.read(cx).scroll_offset()), one_page * 0.9);
    }

    struct NestedPagingSurface {
        outer: ScrollHandle,
        inner: ScrollHandle,
    }

    impl Render for NestedPagingSurface {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("outer-page-scroll")
                .w(px(280.0))
                .h(px(480.0))
                .flex()
                .flex_col()
                .track_scroll(&self.outer)
                .overflow_y_scroll()
                .child(div().id("inner-page-scroll").w_full().h(px(160.0)).flex_none().track_scroll(&self.inner).overflow_y_scroll().child(div().h(px(2400.0)).flex_none()))
                .child(div().h(px(1200.0)).flex_none())
        }
    }

    #[gpui::test]
    fn nested_touch_preview_stays_on_its_surface_and_settles_from_the_original_offset(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let outer = ScrollHandle::default();
        let inner = ScrollHandle::default();
        let (_, visual) = cx.add_window_view(|window, _| {
            window.set_page_scroll_on_swipe(true);
            NestedPagingSurface { outer: outer.clone(), inner: inner.clone() }
        });
        visual.run_until_parked();
        let position = inner.bounds().center();
        let scroll = |dy, touch_phase| ScrollWheelEvent { position, delta: ScrollDelta::Pixels(point(px(0.0), px(dy))), touch_phase, ..Default::default() };
        visual.simulate_event(scroll(-20.0, TouchPhase::Started));
        visual.simulate_event(scroll(-140.0, TouchPhase::Moved));
        visual.run_until_parked();
        assert_eq!(inner.offset().y, px(-160.0));
        assert_eq!(outer.offset().y, Pixels::ZERO);
        visual.simulate_event(scroll(0.0, TouchPhase::Ended));
        visual.run_until_parked();
        assert_eq!(inner.offset().y, px(-144.0), "the preview must not be added to the page target");
        assert_eq!(outer.offset().y, Pixels::ZERO);
        visual.simulate_event(scroll(-80.0, TouchPhase::Started));
        visual.simulate_event(scroll(0.0, TouchPhase::Cancelled));
        visual.run_until_parked();
        assert_eq!(inner.offset().y, px(-144.0));
        assert_eq!(outer.offset().y, Pixels::ZERO);
    }

    #[gpui::test]
    fn nested_scroll_surfaces_page_once_and_bubble_only_at_the_boundary(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let outer = ScrollHandle::default();
        let inner = ScrollHandle::default();
        let (_, visual) = cx.add_window_view(|_, _| NestedPagingSurface { outer: outer.clone(), inner: inner.clone() });
        visual.run_until_parked();
        let position = inner.bounds().center();
        let scroll = || ScrollWheelEvent { position, delta: ScrollDelta::Pages(point(0.0, -1.0)), touch_phase: TouchPhase::Ended, ..Default::default() };
        visual.simulate_event(scroll());
        visual.run_until_parked();
        assert_eq!(inner.offset().y, -inner.bounds().size.height);
        assert_eq!(outer.offset().y, Pixels::ZERO);
        visual.simulate_event(scroll());
        visual.run_until_parked();
        assert_eq!(inner.offset().y, -inner.bounds().size.height * 2.0);
        assert_eq!(outer.offset().y, Pixels::ZERO);
        inner.set_offset(point(Pixels::ZERO, -inner.max_offset().y));
        visual.update(|window, _| window.refresh());
        visual.run_until_parked();
        visual.simulate_event(scroll());
        visual.run_until_parked();
        assert_eq!(inner.offset().y, -inner.max_offset().y);
        assert_eq!(outer.offset().y, -outer.bounds().size.height);
    }
}
