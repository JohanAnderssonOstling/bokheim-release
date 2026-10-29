use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, Weak},
};

use gpui::{Bounds, Context, Modifiers, MouseButton, MouseMoveEvent, Render, RenderImage, TestAppContext, Window, black, div, linear_color_stop, linear_gradient, point, prelude::*, px, size, transparent_black, white};
use image::{Frame, Rgba, RgbaImage};
use ui_components::{
    BOOK_COVER_BADGE_INSET, BOOK_COVER_PROGRESS_HEIGHT, BOOK_COVER_PROGRESS_SCRIM_HEIGHT, BrowserTheme, DOWNLOAD_ACTION_SIZE_REM, TOUCH_TARGET_REM, book_cover_add_badge, book_cover_buy_badge, book_cover_download_badge, book_cover_frame,
    book_cover_generated, book_cover_in_library_badge, book_cover_play_button, book_cover_progress, book_cover_queued_badge, book_cover_with_generated_art, book_cover_with_image, touch_ui,
};

fn image() -> Arc<RenderImage> {
    Arc::new(RenderImage::new(vec![Frame::new(RgbaImage::from_pixel(20, 30, Rgba([200, 100, 50, 255])))]))
}

struct CoverFixture {
    image: Weak<RenderImage>,
    width: f32,
    clipped: bool,
    progress: Option<f32>,
    image_bounds: Rc<Cell<Option<Bounds<gpui::Pixels>>>>,
}

impl Render for CoverFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let image_bounds = self.image_bounds.clone();
        div().relative().size_full().overflow_hidden().when_some(self.image.upgrade(), |root, image| {
            root.child(
                book_cover_with_image(image, 2.0 / 3.0, 1.0, self.progress, BrowserTheme::default())
                    .on_children_prepainted(move |bounds, _, _| image_bounds.set(bounds.first().copied()))
                    .id("cover")
                    .absolute()
                    .left(px(if self.clipped { 200.0 } else { 0.0 }))
                    .w(px(self.width)),
            )
        })
    }
}

struct ClickFixture {
    image: Arc<RenderImage>,
    opens: Rc<Cell<usize>>,
    downloads: Rc<Cell<usize>>,
    generated: bool,
}

struct ProgressFixture;

struct BadgeFixture(f32);

impl Render for BadgeFixture {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.0));
        let theme = BrowserTheme::default();
        let badges = [
            ("idle", book_cover_download_badge("idle", false, theme)),
            ("pending", book_cover_download_badge("pending", true, theme)),
            ("queued", book_cover_queued_badge("queued", theme)),
            ("add", book_cover_add_badge("add", false, theme)),
            ("buy", book_cover_buy_badge("buy", theme)),
            ("held", book_cover_in_library_badge("held", theme)),
        ];
        div().size_full().flex().flex_col().children(badges.into_iter().enumerate().map(|(index, (selector, badge))| div().relative().w(px(200.0)).h(px(100.0)).flex_none().child(badge.debug_selector(move || selector.into())).id(index)))
    }
}

#[gpui::test]
fn badges_preserve_plate_geometry_hover_and_pointer_or_touch_targets(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for rem in [16.0, 24.0] {
        let window = cx.open_window(size(px(200.0), px(700.0)), |_, _| BadgeFixture(rem));
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        for (index, selector) in ["idle", "pending", "queued", "add", "buy", "held"].into_iter().enumerate() {
            let bounds = visual.debug_bounds(selector).expect("badge target");
            let target_size = rem * if touch_ui() { TOUCH_TARGET_REM } else { DOWNLOAD_ACTION_SIZE_REM };
            assert_eq!(bounds.size, size(px(target_size), px(target_size)), "{selector} target at {rem}px/rem");
            let plate =
                Bounds::new(point(px(200.0 - BOOK_COVER_BADGE_INSET - rem * DOWNLOAD_ACTION_SIZE_REM), px(index as f32 * 100.0 + BOOK_COVER_BADGE_INSET)), size(px(rem * DOWNLOAD_ACTION_SIZE_REM), px(rem * DOWNLOAD_ACTION_SIZE_REM)));
            let check_plate = |window: &Window, hovered: bool| {
                let theme = BrowserTheme::default();
                let background = if hovered && matches!(selector, "idle" | "add" | "buy") { theme.page_bg.blend(theme.hover) } else { theme.page_bg };
                let quads = window.painted_quads();
                let quad = quads.iter().find(|quad| quad.bounds == plate.scale(window.scale_factor())).expect("badge plate");
                assert_eq!(quad.background, background.into(), "{selector} hover={hovered}");
                assert!(quads.iter().any(|quad| quad.border_color == theme.rule && quad.bounds.intersects(&plate.scale(window.scale_factor()))), "{selector} border");
            };
            visual.update(|window, _| check_plate(window, false));
            visual.simulate_event(MouseMoveEvent { position: bounds.center(), ..Default::default() });
            visual.run_until_parked();
            visual.update(|window, _| check_plate(window, true));
        }
    }
}

impl Render for ProgressFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        book_cover_frame(BrowserTheme::default(), 2.0 / 3.0).w(px(100.0)).children(book_cover_progress(25.0, BrowserTheme::default()))
    }
}

#[gpui::test]
fn progress_on_empty_cover_frames_stays_anchored_without_flow_children(cx: &mut TestAppContext) {
    let window = cx.open_window(size(px(100.0), px(150.0)), |_, _| ProgressFixture);
    let (quads, scale) = window.update(cx, |_, window, _| (window.painted_quads(), window.scale_factor())).unwrap();
    let bar = quads.iter().find(|quad| quad.background == BrowserTheme::default().accent.into()).expect("progress fill");
    assert_eq!(bar.bounds, Bounds::new(point(px(0.0), px(147.0)), size(px(25.0), px(3.0))).scale(scale));
}

impl Render for ClickFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let opened = self.opens.clone();
        let downloaded = self.downloads.clone();
        let cover = if self.generated {
            book_cover_with_generated_art("Book", Some("Subtitle"), "Writer", 9, BrowserTheme::default()).children(book_cover_progress(25.0, BrowserTheme::default()))
        } else {
            book_cover_with_image(self.image.clone(), 2.0 / 3.0, 1.0, Some(25.0), BrowserTheme::default())
        };
        cover.id("cover").w(px(100.0)).debug_selector(|| "cover".into()).on_click(move |_, _, _| opened.set(opened.get() + 1)).child(
            book_cover_download_badge("download", false, BrowserTheme::default()).debug_selector(|| "download".into()).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, _, cx| {
                cx.stop_propagation();
                downloaded.set(downloaded.get() + 1);
            }),
        )
    }
}

#[gpui::test]
fn loaded_cover_preserves_binding_and_progress_geometry(cx: &mut TestAppContext) {
    let theme = BrowserTheme::default();
    for width in [60.0, 100.0, 180.0] {
        let height = width * 1.5;
        let image = image();
        let image_bounds = Rc::new(Cell::new(None));
        let window = cx.open_window(size(px(width), px(height)), |_, _| CoverFixture { image: Arc::downgrade(&image), width, clipped: false, progress: Some(25.0), image_bounds: image_bounds.clone() });
        assert_eq!(image_bounds.get(), Some(Bounds::new(point(px(1.0), px(1.0)), size(px(width - 2.0), px(height - 2.0)))));
        let (quads, scale) = window.update(cx, |_, window, _| (window.painted_quads(), window.scale_factor())).unwrap();
        let spine = linear_gradient(90.0, linear_color_stop(black().opacity(0.35), 0.0), linear_color_stop(transparent_black(), 1.0));
        let quad = quads.iter().find(|quad| quad.background == spine).expect("spine gradient");
        assert_eq!(quad.bounds, Bounds::new(point(px(1.0), px(1.0)), size(px(10.0), px(height - 2.0))).scale(scale));
        let stops = [transparent_black(), black().opacity(0.12), black().opacity(0.28), white().opacity(0.14), transparent_black()];
        for (index, pair) in stops.windows(2).enumerate() {
            let gradient = linear_gradient(90.0, linear_color_stop(pair[0], 0.0), linear_color_stop(pair[1], 1.0));
            let band = quads.iter().find(|quad| quad.background == gradient).expect("hinge band");
            assert_eq!(band.bounds, Bounds::new(point(px(11.0 + index as f32), px(1.0)), size(px(1.0), px(height - 2.0))).scale(scale));
        }
        let scrim = linear_gradient(0.0, linear_color_stop(black().opacity(0.6), 0.0), linear_color_stop(transparent_black(), 1.0));
        let quad = quads.iter().find(|quad| quad.background == scrim).expect("progress scrim");
        assert_eq!(quad.bounds, Bounds::new(point(px(0.0), px(height - BOOK_COVER_PROGRESS_SCRIM_HEIGHT)), size(px(width), px(BOOK_COVER_PROGRESS_SCRIM_HEIGHT))).scale(scale));
        let bar = quads.iter().find(|quad| quad.background == theme.accent.into()).expect("progress fill");
        assert_eq!(bar.bounds, Bounds::new(point(px(0.0), px(height - BOOK_COVER_PROGRESS_HEIGHT)), size(px(width * 0.25), px(BOOK_COVER_PROGRESS_HEIGHT))).scale(scale));
    }
}

#[gpui::test]
fn loaded_cover_keeps_visible_pixels_until_the_last_window_releases_them(cx: &mut TestAppContext) {
    let image = image();
    let weak = Arc::downgrade(&image);
    let first = cx.open_window(size(px(100.0), px(150.0)), |_, _| CoverFixture { image: weak.clone(), width: 100.0, clipped: false, progress: None, image_bounds: Default::default() });
    let second = cx.open_window(size(px(100.0), px(150.0)), |_, _| CoverFixture { image: weak.clone(), width: 100.0, clipped: false, progress: None, image_bounds: Default::default() });
    drop(image);
    first
        .update(cx, |view, _, cx| {
            view.image = Weak::new();
            cx.notify();
        })
        .unwrap();
    assert!(weak.upgrade().is_some(), "the other visible cover must protect its pixels");
    second
        .update(cx, |view, _, cx| {
            view.image = Weak::new();
            cx.notify();
        })
        .unwrap();
    assert!(weak.upgrade().is_none(), "the final cover leaving the frame must release its pixels");
}

#[gpui::test]
fn loaded_cover_does_not_retain_fully_clipped_pixels(cx: &mut TestAppContext) {
    let image = image();
    let weak = Arc::downgrade(&image);
    let _window = cx.open_window(size(px(100.0), px(150.0)), |_, _| CoverFixture { image: weak.clone(), width: 100.0, clipped: true, progress: None, image_bounds: Default::default() });
    drop(image);
    assert!(weak.upgrade().is_none());
}

#[gpui::test]
fn cover_badge_keeps_its_click_target_separate_from_opening_the_book(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for generated in [false, true] {
        let opens = Rc::new(Cell::new(0));
        let downloads = Rc::new(Cell::new(0));
        let (_, visual) = cx.add_window_view(|_, _| ClickFixture { image: image(), opens: opens.clone(), downloads: downloads.clone(), generated });
        visual.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let badge = visual.debug_bounds("download").expect("download badge");
        visual.simulate_click(badge.center(), Modifiers::default());
        assert_eq!(downloads.get(), 1);
        assert_eq!(opens.get(), 0);
        // On touch this corner lies outside the visible plate but inside its target.
        visual.simulate_click(badge.origin + point(px(1.0), px(1.0)), Modifiers::default());
        assert_eq!(downloads.get(), 2);
        assert_eq!(opens.get(), 0);
        let cover = visual.debug_bounds("cover").expect("cover bounds");
        visual.simulate_click(point(cover.left() + px(40.0), cover.bottom() - px(40.0)), Modifiers::default());
        assert_eq!(opens.get(), 1);
    }
}

struct GeneratedFixture {
    width: f32,
    progress: f32,
    legacy: bool,
}

impl Render for GeneratedFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = BrowserTheme::default();
        let cover = if self.legacy {
            book_cover_frame(theme, ui_components::BOOK_COVER_ASPECT_RATIO).child(book_cover_generated("Book", Some("Subtitle"), "Writer", 9))
        } else {
            book_cover_with_generated_art("Book", Some("Subtitle"), "Writer", 9, theme)
        };
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(cover.id("generated").debug_selector(|| "generated".into()).absolute().left(px(20.0)).top(px(10.0)).w(px(self.width)).child(book_cover_play_button("play", theme)).children(book_cover_progress(self.progress, theme)))
    }
}

#[gpui::test]
fn generated_cover_merge_preserves_paint_clipping_progress_and_group_hover(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for width in [40.0, 100.0, 180.0] {
        for progress in [25.0, 100.0] {
            let mut snapshots = Vec::new();
            for legacy in [true, false] {
                let window = cx.open_window(size(px(300.0), px(400.0)), |_, _| GeneratedFixture { width, progress, legacy });
                let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
                visual.run_until_parked();
                let bounds = visual.debug_bounds("generated").unwrap();
                assert_eq!(bounds, Bounds::new(point(px(20.0), px(10.0)), size(px(width), px(width / ui_components::BOOK_COVER_ASPECT_RATIO))));
                for hovered in [false, true] {
                    if hovered {
                        visual.simulate_event(MouseMoveEvent { position: bounds.center(), ..Default::default() });
                        visual.run_until_parked();
                    }
                    snapshots
                        .push(visual.update(|window, _| window.painted_quads().into_iter().map(|quad| (quad.bounds, quad.content_mask.bounds, quad.background, quad.border_color, quad.border_widths, quad.corner_radii)).collect::<Vec<_>>()));
                }
            }
            assert_eq!(snapshots[0], snapshots[2], "initial generated cover at {width}px, progress={progress}");
            assert_eq!(snapshots[1], snapshots[3], "hovered generated cover at {width}px, progress={progress}");
        }
    }
}
