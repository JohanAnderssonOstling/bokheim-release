//! Painted covers survive between frames even after the browse cache evicts
//! them. GPUI releases this state when the cover leaves the rendered tree.

#[cfg(test)]
mod tests {
    use super::super::cover_cache::CoverCache;
    use gpui::{Context, Render, RenderImage, TestAppContext, Window, div, prelude::*, px, size};
    use image::{Frame, RgbaImage};
    use library_backend::ThumbnailResolution;
    use std::sync::{Arc, Weak};
    use ui_components::{BrowserTheme, book_cover_with_image};

    struct CoverFixture {
        image: Weak<RenderImage>,
        clipped: bool,
    }

    impl Render for CoverFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            // Like a cached card, the fixture itself must not own the pixels.
            div()
                .relative()
                .size_full()
                .overflow_hidden()
                .when_some(self.image.upgrade(), |root, image| root.child(book_cover_with_image(image, 1.0, 1.0, None, BrowserTheme::default()).id("cover").absolute().left(px(if self.clipped { 200.0 } else { 0.0 })).size(px(100.0))))
        }
    }

    #[gpui::test]
    fn visible_cover_survives_repeated_cache_eviction_and_releases_offscreen(cx: &mut TestAppContext) {
        let mut cache = CoverCache::default();
        let image = Arc::new(RenderImage::new(vec![Frame::new(RgbaImage::new(1, 1))]));
        let visible = Arc::downgrade(&image);
        cache.insert(0, &image, ThumbnailResolution::Browse);
        let window = cx.open_window(size(px(100.0), px(100.0)), |_, _| CoverFixture { image: visible.clone(), clipped: false });
        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        drop(image);

        for key in 1..100 {
            let image = Arc::new(RenderImage::new(vec![Frame::new(RgbaImage::new(1, 1))]));
            cache.clear();
            cache.insert(key, &image, ThumbnailResolution::Browse);
            // The next frame can reuse the same pixels without another decode
            // or insertion, even though they no longer occupy the cache.
            assert!(visible.upgrade().is_some(), "painted cover retained");
            window.update(cx, |_, _, cx| cx.notify()).unwrap();
            assert!(cache.get(&0).is_none());
        }

        window
            .update(cx, |view, _, cx| {
                view.image = Weak::new();
                cx.notify();
            })
            .unwrap();
        assert!(visible.upgrade().is_none());
    }

    #[gpui::test]
    fn fully_clipped_covers_do_not_retain_pixels(cx: &mut TestAppContext) {
        let image = Arc::new(RenderImage::new(vec![Frame::new(RgbaImage::new(1, 1))]));
        let weak = Arc::downgrade(&image);
        let window = cx.open_window(size(px(100.0), px(100.0)), |_, _| CoverFixture { image: weak.clone(), clipped: true });
        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        drop(image);
        assert!(weak.upgrade().is_none());
    }

    #[gpui::test]
    fn leaving_browse_trims_cached_pixels_after_the_outgoing_frame_releases_them(cx: &mut TestAppContext) {
        use crate::library::widgets::evict_covers;
        use gpui::VisualTestContext;
        use gpui_image_eviction::track_image;

        let image = Arc::new(RenderImage::new(vec![Frame::new(RgbaImage::new(2, 2))]));
        let weak = Arc::downgrade(&image);
        cx.update(|cx| track_image(&image, cx));
        let window = cx.open_window(size(px(100.0), px(100.0)), |_, _| CoverFixture { image: weak.clone(), clipped: false });
        drop(image);
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|_, cx| evict_covers(cx));
        assert!(visual.update(|window, cx| window.simulate_next_frame(cx)) > 0);
        assert!(visual.update(|window, cx| window.simulate_next_frame(cx)) > 0);
        assert!(weak.upgrade().is_some(), "a still-visible frame protects its cover");
        window
            .update(cx, |view, _, cx| {
                view.image = Weak::new();
                cx.notify();
            })
            .unwrap();
        visual.update(|_, cx| evict_covers(cx));
        assert!(visual.update(|window, cx| window.simulate_next_frame(cx)) > 0);
        assert!(visual.update(|window, cx| window.simulate_next_frame(cx)) > 0);
        cx.run_until_parked();
        assert!(weak.upgrade().is_none(), "transition trimming must not wait for new image pressure");
    }
}
