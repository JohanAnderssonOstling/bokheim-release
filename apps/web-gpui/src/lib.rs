#![cfg(target_family = "wasm")]

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use app::AppClient;
use application_shell::ApplicationRoot;
use browser_ui::{AppServices, AppStartup};
use gpui::{App, AppContext, Bounds, SharedString, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

mod browser_back;

thread_local! {
    static APPLICATION: RefCell<Option<gpui::ApplicationHandle>> = const { RefCell::new(None) };
}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    // Parser workers initialize this module too, but only the window owns UI.
    if web_sys::window().is_none() {
        return;
    }
    console_error_panic_hook::set_once();
    gpui_platform::web_init();
    let application = gpui_platform::application().with_assets(ui_components::Assets::default());
    wasm_bindgen_futures::spawn_local(async move {
        let (backend, state) = match connect_backend().await {
            Ok(initialized) => initialized,
            Err(error) => {
                log::error!("failed to start SQLite web backend: {error}");
                notify_startup("__BOKHEIM_STARTUP_FAILED__", &error);
                return;
            }
        };
        let startup = AppStartup { libraries: state.libraries, browsing_preferences: state.browsing_preferences, account_status: state.account_status };
        let mut preferences = reader_ui::ReaderPreferences::default();
        preferences.apply_synchronized(&state.reader_preferences);
        let handle = application.run_embedded(move |cx| {
            open_bokheim(backend, startup, preferences, cx);
            notify_startup("__BOKHEIM_STARTUP_READY__", "");
        });
        APPLICATION.with(|application| *application.borrow_mut() = Some(handle));
    });
}

fn notify_startup(name: &str, detail: &str) {
    if let Ok(callback) = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str(name)) {
        if let Some(callback) = callback.dyn_ref::<js_sys::Function>() {
            let _ = callback.call1(&JsValue::UNDEFINED, &JsValue::from_str(detail));
        }
    }
}

async fn connect_backend() -> Result<(AppClient, app::AppStartupState), String> {
    const ATTEMPTS: usize = 6;
    let asset_base = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__BOKHEIM_ASSET_BASE__")).ok().and_then(|value| value.as_string()).unwrap_or_else(|| "./pkg".to_owned());
    let broker_url = format!("{asset_base}/web_backend_worker_loader.js");
    let mut last_error = String::new();
    for attempt in 0..ATTEMPTS {
        match AppClient::connect_web(backend_connection::configuration(&broker_url)).await {
            Ok(initialized) => return Ok(initialized),
            Err(error) => {
                let error = error.to_string();
                let access_handle_conflict = error.contains("creating sync access handle");
                last_error = error;
                if !access_handle_conflict || attempt + 1 == ATTEMPTS {
                    break;
                }
                browser_delay(250 * (attempt as i32 + 1)).await;
            }
        }
    }
    if last_error.contains("creating sync access handle") {
        Err(format!("{last_error}\n\nAn older Bokheim session is still using browser storage. Close Bokheim tabs opened before this update, then reload this page."))
    } else {
        Err(last_error)
    }
}

async fn browser_delay(milliseconds: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let callback = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        });
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), milliseconds);
        }
    });
    let _ = JsFuture::from(promise).await;
}

fn open_bokheim(backend: AppClient, startup: AppStartup, reader_preferences: reader_ui::ReaderPreferences, cx: &mut App) {
    if let Err(error) =
        cx.text_system().add_fonts(vec![Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/LibertinusSerif-Regular.otf")), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/LibertinusSerif-Bold.otf")), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/LibertinusSerif-Italic.otf")), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/BodoniModa-Regular.ttf")), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/BodoniModa-SemiBold.ttf"))])
    {
        log::warn!("failed to install Bokheim fonts: {error}");
    }

    gpui_component::init(cx);
    ui_components::init_page_scrolling(cx);
    browser_ui::bind_keys(cx);
    reader_ui::bind_keys(cx);
    let saved_reader_preferences = Rc::new(RefCell::new(reader_preferences.clone()));
    let reader_preferences_backend = backend.clone();
    let save_reader_settings: reader_ui::SaveReaderSettings = Rc::new(move |preferences, cx| {
        let patches = saved_reader_preferences.borrow().synchronized_patches(&preferences);
        *saved_reader_preferences.borrow_mut() = preferences;
        if !patches.is_empty() {
            let backend = reader_preferences_backend.clone();
            cx.spawn(async move |_cx| {
                if let Err(error) = backend.update_reader_preferences(patches).await {
                    log::warn!("failed to persist reader preference: {error}");
                }
            })
            .detach();
        }
    });
    reader_ui::configure(reader_preferences, save_reader_settings, cx);

    let application_backend = backend.clone();
    let services = AppServices::new(backend, startup);
    let bounds = Bounds::centered(None, size(px(1240.0), px(820.0)), cx);

    cx.open_window(
        WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), titlebar: Some(gpui::TitlebarOptions { title: Some(SharedString::from("Bokheim")), ..Default::default() }), focus: true, ..Default::default() },
        move |window, cx| {
            let application = cx.new(|cx| ApplicationRoot::new(services, application_backend, "Bokheim", None, window, cx));
            browser_back::install(window, cx);
            cx.new(|cx| Root::new(application, window, cx))
        },
    )
    .expect("failed to open the Bokheim browser window");
    cx.activate(true);
}

#[cfg(target_arch = "wasm32")]
#[path = "../../web-backend-worker/connection.rs"]
mod backend_connection;
