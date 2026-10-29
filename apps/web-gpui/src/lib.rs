#![cfg(target_family = "wasm")]

use std::cell::OnceCell;
use std::rc::Rc;

use app::{AppClient, host::startup_profile::StartupProfile};
use application_shell::ApplicationRoot;
use browser_ui::{AppServices, AppStartup, NavigationSession};
use gpui::{App, AppContext, Bounds, SharedString, WindowBounds, WindowOptions, profiler::startup::StartupProfile as GraphicsStartupProfile, px, size};
use gpui_component::Root;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

mod browser_back;
mod reader_preferences;

thread_local! {
    static APPLICATION: OnceCell<gpui::ApplicationHandle> = const { OnceCell::new() };
}

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    // Parser workers initialize this module too, but only the window owns UI.
    if web_sys::window().is_none() {
        return;
    }
    console_error_panic_hook::set_once();
    gpui_platform::web_init();
    if js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("__BOKHEIM_STARTUP_PROFILE__")).ok().and_then(|value| value.as_bool()) == Some(true) {
        StartupProfile::enable();
        GraphicsStartupProfile::enable();
    }
    let mut profile = StartupProfile::new("web_launch");
    let application = gpui_platform::application().with_assets(ui_components::Assets::default());
    profile.mark("platform_ready");
    wasm_bindgen_futures::spawn_local(async move {
        let (backend, state) = match connect_backend().await {
            Ok(initialized) => initialized,
            Err(error) => {
                log::error!("failed to start SQLite web backend: {error}");
                notify_startup("__BOKHEIM_STARTUP_FAILED__", &error);
                return;
            }
        };
        profile.mark("backend_connected");
        let startup = AppStartup { libraries: state.libraries, browsing_preferences: state.browsing_preferences, account_status: state.account_status };
        let mut preferences = reader_preferences::load();
        preferences.apply_synchronized(&state.reader_preferences);
        let handle = application.run_embedded(move |cx| {
            profile.mark("graphics_ready");
            match open_bokheim(backend, startup, preferences, cx) {
                Ok(()) => notify_startup("__BOKHEIM_STARTUP_READY__", ""),
                Err(error) => {
                    log::error!("failed to open the Bokheim browser window: {error}");
                    notify_startup("__BOKHEIM_STARTUP_FAILED__", &error);
                    cx.quit();
                }
            }
        });
        APPLICATION.with(|application| assert!(application.set(handle).is_ok(), "web application already initialized"));
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
    let mut attempt = 0;
    let error = loop {
        match AppClient::connect_web(backend_connection::configuration(&broker_url)).await {
            Ok(initialized) => return Ok(initialized),
            // A tab that is closing may still hold storage; give it time.
            Err(error) if error.failure() == Some(app::Failure::StorageBusy) && attempt + 1 < ATTEMPTS => {
                attempt += 1;
                browser_delay(250 * attempt as i32).await;
            }
            Err(error) => break error,
        }
    };
    if error.failure() == Some(app::Failure::StorageBusy) {
        Err(format!("{error}\n\nAn older Bokheim session is still using browser storage. Close Bokheim tabs opened before this update, then reload this page."))
    } else {
        Err(error.to_string())
    }
}

async fn browser_delay(milliseconds: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let callback = Closure::once_aborting(move || {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        })
        .into_js_value();
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), milliseconds);
        }
    });
    let _ = JsFuture::from(promise).await;
}

fn open_bokheim(backend: AppClient, startup: AppStartup, reader_preferences: reader_ui::ReaderPreferences, cx: &mut App) -> Result<(), String> {
    let mut profile = StartupProfile::new("open_ui");
    if let Err(error) = ui_components::register_fonts(cx) {
        log::warn!("failed to install Bokheim fonts: {error}");
    }
    profile.mark("fonts_ready");

    gpui_component::init(cx);
    ui_components::init_page_scrolling(cx);
    browser_ui::bind_keys(cx);
    reader_ui::bind_keys(cx);
    let reader_preferences_backend = backend.clone();
    let save_reader_settings: reader_ui::SaveReaderSettings = Rc::new(move |preferences, patches, cx| {
        reader_preferences::save(&preferences);
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
    profile.mark("components_ready");

    let application_backend = backend.clone();
    let services = AppServices::new(backend, startup);
    let bounds = Bounds::centered(None, size(px(1240.0), px(820.0)), cx);
    profile.mark("services_ready");

    cx.open_window(
        WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), titlebar: Some(gpui::TitlebarOptions { title: Some(SharedString::from("Bokheim")), ..Default::default() }), focus: true, ..Default::default() },
        move |window, cx| {
            let application = cx.new(|cx| ApplicationRoot::new(services, browser_ui::WindowSession { initial: load_navigation_session(), save: Rc::new(save_navigation_session) }, application_backend, "Bokheim", None, window, cx));
            browser_back::install(window, cx);
            cx.new(|cx| Root::new(application, window, cx))
        },
    )
    .map_err(|error| error.to_string())?;
    profile.mark("window_opened");
    cx.activate(true);
    Ok(())
}

#[cfg(target_arch = "wasm32")]
mod backend_connection;

const NAVIGATION_STORAGE_KEY: &str = "bokheim.navigation.v1";

fn load_navigation_session() -> NavigationSession {
    let load = || -> Result<Option<NavigationSession>, String> {
        let storage = web_sys::window().ok_or("browser window is unavailable")?.local_storage().map_err(|error| format!("{error:?}"))?.ok_or("local storage is unavailable")?;
        storage.get_item(NAVIGATION_STORAGE_KEY).map_err(|error| format!("{error:?}"))?.map(|value| serde_json::from_str(&value).map_err(|error| error.to_string())).transpose()
    };
    match load() {
        Ok(session) => session.unwrap_or_default(),
        Err(error) => {
            log::warn!("could not restore navigation: {error}");
            NavigationSession::default()
        }
    }
}

fn save_navigation_session(session: NavigationSession, _: &mut App) {
    let save = || -> Result<(), String> {
        let storage = web_sys::window().ok_or("browser window is unavailable")?.local_storage().map_err(|error| format!("{error:?}"))?.ok_or("local storage is unavailable")?;
        let value = serde_json::to_string(&session).map_err(|error| error.to_string())?;
        storage.set_item(NAVIGATION_STORAGE_KEY, &value).map_err(|error| format!("{error:?}"))
    };
    if let Err(error) = save() {
        log::warn!("could not save navigation: {error}");
    }
}
