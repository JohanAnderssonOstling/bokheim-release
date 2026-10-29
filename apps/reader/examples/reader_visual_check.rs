//! Open the real reader with an imported book and isolated, persistent settings.
//! cargo run --release -p reader-ui --example reader_visual_check -- BOOK.epub ISOLATED_DIRECTORY [CHAPTER_URI]
//! Reuse the directory to check font persistence; never use live application data.

use std::{borrow::Cow, fs, path::PathBuf, rc::Rc};

use app::{AppClient, AppDataLocation, host::BackendLaunch};
use gpui::prelude::*;
use gpui::{App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Window, div, px, size};
use library_backend::ROOT_DIR_ID;
use library_model::BookLocator;
use reader_ui::{ReaderPreferences, ReaderView};

struct Harness {
    reader: Option<Entity<ReaderView>>,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.reader.clone())
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let book = PathBuf::from(args.next().expect("book path"));
    let directory = PathBuf::from(args.next().expect("isolated directory"));
    let target = args.next().map(|arg| arg.to_string_lossy().into_owned());
    let test_font = std::env::var_os("BOKHEIM_CJK_TEST_FONT").map(|path| fs::read(path).expect("test font"));
    assert!(args.next().is_none(), "unexpected argument");
    let marker = directory.join("reader-visual-check");
    assert!(!directory.exists() || marker.exists() || fs::read_dir(&directory).unwrap().next().is_none(), "directory contains unrelated data");
    fs::create_dir_all(&directory).unwrap();
    fs::write(marker, b"isolated reader visual check\n").unwrap();
    let settings_path = directory.join("reader-preferences.json");
    let preferences: ReaderPreferences = fs::read(&settings_path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    let mut settings = serde_json::to_value(preferences).unwrap();
    settings["active_sidebar_tab"] = "Settings".into();
    settings["sidebar_open"] = true.into();
    settings["chrome_hidden"] = false.into();
    let preferences = serde_json::from_value(settings).unwrap();
    let launch = BackendLaunch::initialize(AppDataLocation::native_path(directory)).expect("isolated backend");
    let (backend, _) = AppClient::start_native(launch).expect("backend worker");
    gpui_platform::application().with_assets(ui_components::Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        ui_components::init_page_scrolling(cx);
        ui_components::register_fonts(cx).unwrap();
        if let Some(bytes) = test_font {
            cx.text_system().add_fonts(vec![Cow::Owned(bytes)]).expect("register isolated test font");
        }
        ui_components::apply_appearance(&app_preferences::BrowsingPreferences::default(), cx);
        reader_ui::bind_keys(cx);
        reader_ui::configure(preferences, Rc::new(move |preferences, _, _| fs::write(&settings_path, serde_json::to_vec_pretty(&preferences).unwrap()).unwrap()), cx);
        let bounds = gpui::Bounds::centered(None, size(px(1200.0), px(850.0)), cx);
        let window = cx
            .open_window(gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions { title: Some("CJK font validation".into()), ..Default::default() }),
                ..Default::default()
            }, |window, cx| {
                let harness = cx.new(|_| Harness { reader: None });
                cx.new(|cx| gpui_component::Root::new(harness, window, cx))
            })
            .unwrap();
        cx.activate(true);
        cx.spawn(async move |cx| {
            let library = Rc::new(backend.library_for_import(None).await.expect("isolated library"));
            let name = book.file_name().unwrap().to_string_lossy().into_owned();
            let content = library.import_downloaded_book(ROOT_DIR_ID, name.clone(), fs::read(book).unwrap()).await.expect("import test book");
            let locator = BookLocator::new(*library.id(), content);
            window
                .update(cx, |root, window, cx| {
                    let harness = root.view().clone().downcast::<Harness>().ok().unwrap();
                    let reader = cx.new(|cx| ReaderView::new(locator, name, target, backend, library, window, cx));
                    harness.update(cx, |harness, cx| {
                        harness.reader = Some(reader);
                        cx.notify();
                    });
                })
                .unwrap();
        })
        .detach();
    });
}
