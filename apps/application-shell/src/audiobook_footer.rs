//! Presentation only: loading, errors and playback share one footer placement.
use gpui::{Context, Entity, EventEmitter, Render, SharedString, Window, prelude::*};

pub(super) struct CancelPending;

#[derive(Default)]
pub(super) struct AudiobookFooter {
    dock: Option<Entity<reader_ui::AudiobookDock>>,
    pending: Option<SharedString>,
}

impl AudiobookFooter {
    pub(super) fn set(&mut self, dock: Option<Entity<reader_ui::AudiobookDock>>, pending: Option<SharedString>, cx: &mut Context<Self>) {
        if self.dock == dock && self.pending == pending {
            return;
        }
        self.dock = dock;
        self.pending = pending;
        cx.notify();
    }
}

impl EventEmitter<CancelPending> for AudiobookFooter {}

impl Render for AudiobookFooter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        gpui::div().w_full().flex_none().flex().flex_col().children(self.dock.clone()).when_some(self.pending.as_ref(), |footer, message| {
            footer.child(
                gpui::div()
                    .debug_selector(|| "pending-audiobook-footer".into())
                    .flex_none()
                    .flex()
                    .items_center()
                    .p(gpui::px(12.0))
                    .child(gpui::div().flex_1().min_w_0().child(message.clone()))
                    .child(gpui::div().id("cancel-audiobook-open").debug_selector(|| "cancel-audiobook-open".into()).cursor_pointer().child("Close").on_click(cx.listener(|_, _, _, cx| cx.emit(CancelPending)))),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn loading_and_error_share_a_cancellable_footer(cx: &mut gpui::TestAppContext) {
        let (footer, cx) = cx.add_window_view(|_, _| AudiobookFooter::default());
        let cancellations = cx.new(|cx| {
            cx.subscribe(&footer, |count: &mut usize, _, _: &CancelPending, _| *count += 1).detach();
            0usize
        });
        for message in ["Opening audiobook…", "Could not authorize playback"] {
            footer.update(cx, |footer, cx| footer.set(None, Some(message.into()), cx));
            cx.run_until_parked();
            let bounds = cx.debug_bounds("pending-audiobook-footer").unwrap();
            assert!(bounds.size.height > gpui::px(0.0) && bounds.size.height < gpui::px(100.0));
            let close = cx.debug_bounds("cancel-audiobook-open").unwrap();
            cx.simulate_click(close.center(), gpui::Modifiers::none());
        }
        assert_eq!(cancellations.read_with(cx, |count, _| *count), 2);
        footer.update(cx, |footer, cx| footer.set(None, None, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("pending-audiobook-footer").is_none());
    }
}
