use super::sidebar_sheet;
use gpui::{Context, LongPressEvent, MouseButton, MouseDownEvent, MouseUpEvent, Render, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchId, Window, div, point, prelude::*, px, size};

struct PanelFixture {
    partial: bool,
    visible: bool,
    document_downs: usize,
    document_clicks: usize,
    document_holds: usize,
    document_scrolls: usize,
    panel_clicks: usize,
    panel_scrolls: usize,
}

impl Render for PanelFixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let document = div()
            .id("input-test-document")
            .size_full()
            .on_any_mouse_down(cx.listener(|this, _, _, _| this.document_downs += 1))
            .on_click(cx.listener(|this, _, _, _| this.document_clicks += 1))
            .on_long_press(cx.listener(|this, _, _, _| this.document_holds += 1))
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.document_scrolls += 1));
        let mut page = div().relative().size_full().child(document);
        if self.visible {
            let panel = div()
                .size_full()
                .debug_selector(|| "input-test-panel".into())
                .bg(gpui::white())
                .on_scroll_wheel(cx.listener(|this, _, _, _| this.panel_scrolls += 1))
                .child(div().id("input-test-control").w(px(140.0)).h(px(44.0)).debug_selector(|| "input-test-control".into()).on_click(cx.listener(|this, _, _, _| this.panel_clicks += 1)));
            page = page.child(sidebar_sheet(panel.into_any_element(), self.partial, window));
        }
        page
    }
}

#[gpui::test]
fn sheets_block_reader_events_while_their_controls_and_exposed_page_work(cx: &mut TestAppContext) {
    for viewport in [size(px(632.0), px(840.0)), size(px(840.0), px(632.0))] {
        for partial in [false, true] {
            let (view, visual) = cx.add_window_view(|_, _| PanelFixture { partial, visible: true, document_downs: 0, document_clicks: 0, document_holds: 0, document_scrolls: 0, panel_clicks: 0, panel_scrolls: 0 });
            visual.simulate_resize(viewport);
            visual.run_until_parked();
            let panel = visual.debug_bounds("input-test-panel").unwrap();
            let control = visual.debug_bounds("input-test-control").unwrap();
            let blank = point(panel.right() - px(20.0), panel.bottom() - px(20.0));
            visual.simulate_click(blank, Default::default());
            visual.simulate_event(MouseDownEvent { button: MouseButton::Right, position: blank, ..Default::default() });
            visual.simulate_event(MouseUpEvent { button: MouseButton::Right, position: blank, ..Default::default() });
            visual.simulate_event(LongPressEvent { id: TouchId(0), position: blank });
            visual.simulate_event(ScrollWheelEvent { position: blank, delta: ScrollDelta::Pages(point(0.0, -1.0)), ..Default::default() });
            visual.simulate_click(control.center(), Default::default());
            view.read_with(visual, |view, _| {
                assert_eq!(view.panel_clicks, 1, "panel controls must remain interactive");
                assert_eq!(view.panel_scrolls, 1);
                assert_eq!((view.document_downs, view.document_clicks, view.document_holds, view.document_scrolls), (0, 0, 0, 0), "panel input must not reach the reader underneath");
            });
            if panel.top() > px(0.0) {
                visual.simulate_click(point(panel.center().x, panel.top() / 2.0), Default::default());
                view.read_with(visual, |view, _| assert_eq!(view.document_clicks, 1, "the page exposed above Settings remains interactive"));
            }
            let before = view.read_with(visual, |view, _| view.document_clicks);
            view.update(visual, |view, cx| {
                view.visible = false;
                cx.notify();
            });
            visual.run_until_parked();
            visual.simulate_click(panel.center(), Default::default());
            view.read_with(visual, |view, _| assert_eq!(view.document_clicks, before + 1, "closing the panel restores reader taps"));
        }
    }
}
