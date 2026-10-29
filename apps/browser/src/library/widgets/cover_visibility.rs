//! A cover view can be cached in memory without appearing in any window.

use gpui::Window;
use std::rc::{Rc, Weak};

#[derive(Default)]
pub(crate) struct CoverVisibility {
    mounts: Vec<Weak<()>>,
}

impl CoverVisibility {
    pub(crate) fn track(&mut self, window: &mut Window) {
        window.with_global_id("browse-cover-visibility".into(), |id, window| {
            window.with_element_state(id, |mount: Option<Rc<()>>, _| {
                let mount = mount.unwrap_or_else(|| Rc::new(()));
                self.mounts.retain(|mount| mount.strong_count() > 0);
                let weak = Rc::downgrade(&mount);
                if !self.mounts.iter().any(|previous| previous.ptr_eq(&weak)) {
                    self.mounts.push(weak);
                }
                ((), mount)
            });
        });
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.mounts.iter().any(|mount| mount.strong_count() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Entity, Render, StyleRefinement, TestAppContext, VisualTestContext, div, prelude::*, px, size};

    #[derive(Default)]
    struct CoverView {
        visibility: CoverVisibility,
        renders: usize,
    }

    impl Render for CoverView {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.visibility.track(window);
            self.renders += 1;
            div().size_full()
        }
    }

    struct Fixture(Option<Entity<CoverView>>);

    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().children(self.0.clone().map(|view| view.cached(StyleRefinement::default().size_full())))
        }
    }

    #[gpui::test]
    fn visibility_follows_cached_placements_in_every_window(cx: &mut TestAppContext) {
        let cover = cx.new(|_| CoverView::default());
        assert!(!cover.read_with(cx, |cover, _| cover.visibility.is_visible()));
        let first = cx.open_window(size(px(100.0), px(100.0)), |_, _| Fixture(Some(cover.clone())));
        let second = cx.open_window(size(px(100.0), px(100.0)), |_, _| Fixture(Some(cover.clone())));
        let mut visuals = [VisualTestContext::from_window(first.into(), cx), VisualTestContext::from_window(second.into(), cx)];
        for visual in &mut visuals {
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        let renders = cover.read_with(cx, |cover, _| cover.renders);
        for visual in &mut visuals {
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        cover.read_with(cx, |cover, _| {
            assert!(cover.visibility.is_visible());
            assert_eq!(cover.renders, renders, "cached frames must retain their visibility without rendering again");
        });
        for window in [first, second] {
            window
                .update(cx, |fixture, _, cx| {
                    fixture.0 = None;
                    cx.notify();
                })
                .unwrap();
            let mut visual = VisualTestContext::from_window(window.into(), cx);
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let expected = window == first;
            assert_eq!(cover.read_with(cx, |cover, _| cover.visibility.is_visible()), expected, "only removing the last placement makes the cover hidden");
        }
    }
}
