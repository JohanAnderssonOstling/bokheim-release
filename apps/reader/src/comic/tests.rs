use super::*;
use book_content::comic::ComicBook;
use gpui::{Entity, TestAppContext, VisualTestContext, point, size};
use image::{DynamicImage, ImageFormat, RgbaImage};
use library_backend::{BackendError, BookId, LibraryHandle, LibraryId, client::ClientTransport, executor::BoxedBackendFuture};
use std::{
    future,
    io::{Cursor, Write},
};
use zip::{ZipWriter, write::SimpleFileOptions};

struct IdleLibrary;

impl ClientTransport for IdleLibrary {
    fn handle(&self) -> BoxedBackendFuture<'_, Result<&LibraryHandle, BackendError>> {
        Box::pin(future::pending())
    }
}

fn setup(cx: &mut TestAppContext) -> (Entity<ComicReaderView>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    cx.update(|cx| cx.set_global(crate::settings::ReaderSettings { preferences: Default::default(), save: Rc::new(|_, _, _| {}) }));
    let mut image = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(RgbaImage::new(400, 1000)).write_to(&mut image, ImageFormat::Png).unwrap();
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..8 {
        archive.start_file(format!("{index}.png"), SimpleFileOptions::default()).unwrap();
        archive.write_all(image.get_ref()).unwrap();
    }
    let book = ComicBook::open_reader(archive.finish().unwrap()).unwrap();
    let reader = futures_lite::future::block_on(book.into_reader()).unwrap();
    let (root, visual) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            let library = Rc::new(LibraryClient::new(LibraryId::nil(), IdleLibrary));
            let mut view = ComicReaderView {
                reader,
                title: "Comic navigation test".into(),
                current: 0,
                columns: 1,
                page_gap: 0.0,
                right_to_left: false,
                zoom: Zoom::FitWidth,
                offset: 0.0,
                pan_x: 0.0,
                viewport: Bounds::default(),
                drag: None,
                dragged: false,
                swipe: PageSwipe::default(),
                touchpad: Default::default(),
                pages: PageCache::new(DISPLAY_CACHE_BUDGET),
                errors: HashMap::new(),
                load_task: None,
                requested_pages: VecDeque::new(),
                loading_page: None,
                page_sizes: HashMap::new(),
                positions: ReadingPositionWriter::new(cx, BookId::from_bytes([0; 32]), library),
                navigation_revision: 0,
                sidebar: SidebarState::from_preferences(cx),
                chrome: chrome::ChromeState::from_preferences(Component::ComicShell, cx),
                focus,
                sidebar_focus: cx.focus_handle(),
                rail_focus: cx.focus_handle(),
                settings_navigation: SettingsNavigation::new(window, cx),
                close_reader: Rc::new(|_, _| {}),
            };
            view.chrome.hide(cx);
            view.request_pages(cx);
            view
        });
        gpui_component::Root::new(view, window, cx)
    });
    let view = visual.update(|_, cx| root.read(cx).view().clone().downcast::<ComicReaderView>().ok().unwrap());
    visual.simulate_resize(size(px(852.0), px(600.0)));
    visual.run_until_parked();
    (view, visual)
}

#[gpui::test]
fn turns_reveal_tall_pages_and_scrolling_paints_neighboring_pages(cx: &mut TestAppContext) {
    let (view, visual) = setup(cx);
    view.update(visual, |view, cx| {
        assert!(view.pages.contains(0) && view.pages.contains(1), "visible pages and the following page are decoded");
        view.turn(1, cx);
        assert_eq!(view.current, 0);
        assert!((view.flow().scroll_top(view.current, view.offset) - 600.0).abs() < 0.01);
        view.scroll(0.0, -1350.0, cx);
        assert_eq!(view.current, 0);
    });
    visual.run_until_parked();
    let first = visual.debug_bounds("comic-page-0").unwrap();
    let next = visual.debug_bounds("comic-page-1").unwrap();
    assert_eq!(first.bottom(), next.top(), "adjacent page images must share a continuous boundary");
    view.update(visual, |view, cx| {
        view.scroll(0.0, -100.0, cx);
        assert_eq!(view.current, 1);
        assert!((view.flow().scroll_top(view.current, view.offset) - 2050.0).abs() < 0.01, "the wheel delta retains its remainder past the boundary");
        view.turn(-1, cx);
        assert_eq!(view.current, 0);
        assert!((view.flow().scroll_top(view.current, view.offset) - 1450.0).abs() < 0.01);
    });
}

#[gpui::test]
fn clicks_leave_sidebar_visibility_alone_and_page_stepper_changes_columns(cx: &mut TestAppContext) {
    let (view, visual) = setup(cx);
    let document = visual.debug_bounds("comic-document").unwrap();
    for fraction in [0.5, 0.9] {
        visual.simulate_click(point(document.left() + document.size.width * fraction, document.center().y), Default::default());
        visual.run_until_parked();
        view.read_with(visual, |view, _| assert!(!view.chrome.is_visible()));
    }
    visual.update(|window, cx| view.update(cx, |view, cx| view.select_sidebar_tab(SidebarTab::Settings, window, cx)));
    visual.run_until_parked();
    let document = visual.debug_bounds("comic-document").unwrap();
    visual.simulate_click(document.center(), Default::default());
    visual.run_until_parked();
    view.read_with(visual, |view, _| assert!(view.chrome.is_visible()));
    for _ in 0..8 {
        let increase = visual.debug_bounds("comic-pages-increase").unwrap();
        visual.simulate_click(increase.center(), Default::default());
        visual.run_until_parked();
    }
    view.read_with(visual, |view, _| assert_eq!(view.columns, MAX_COLUMNS));
    let decrease = visual.debug_bounds("comic-pages-decrease").unwrap();
    visual.simulate_click(decrease.center(), Default::default());
    visual.run_until_parked();
    view.read_with(visual, |view, _| assert_eq!(view.columns, MAX_COLUMNS - 1));
}

#[gpui::test]
fn comic_spacing_controls_inset_the_document_and_separate_spreads(cx: &mut TestAppContext) {
    use crate::settings::ReaderSettings;
    use crate::shell::settings_navigation::SettingsRow;

    let (view, visual) = setup(cx);
    let before = visual.debug_bounds("comic-document").unwrap();
    view.update(visual, |view, cx| {
        view.adjust_columns(1, cx);
        view.adjust_spacing(SettingsRow::SideMargins, 16.0, cx);
        view.adjust_spacing(SettingsRow::VerticalMargins, 24.0, cx);
        view.adjust_spacing(SettingsRow::ColumnGap, 32.0, cx);
        let preferences = ReaderSettings::preferences(cx);
        assert_eq!(preferences.pdf_horizontal_margin_px, 0.0);
        assert_eq!(preferences.pdf_vertical_margin_px, 0.0);
        assert_eq!(preferences.pdf_page_gap_px, 0.0);
    });
    visual.run_until_parked();
    let after = visual.debug_bounds("comic-document").unwrap();
    assert_eq!(after.left() - before.left(), px(16.0));
    assert_eq!(after.top() - before.top(), px(24.0));
    assert_eq!(before.size.width - after.size.width, px(32.0));
    assert_eq!(before.size.height - after.size.height, px(48.0));
    let first = visual.debug_bounds("comic-page-0").unwrap();
    let second = visual.debug_bounds("comic-page-1").unwrap();
    assert_eq!(second.left() - first.right(), px(32.0));
    view.update(visual, |view, cx| { view.right_to_left = true; cx.notify(); });
    visual.run_until_parked();
    let first = visual.debug_bounds("comic-page-0").unwrap();
    let second = visual.debug_bounds("comic-page-1").unwrap();
    assert_eq!(first.left() - second.right(), px(32.0));
}
