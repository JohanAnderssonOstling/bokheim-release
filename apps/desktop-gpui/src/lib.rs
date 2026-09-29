#[cfg(all(target_os = "android", feature = "android"))]
mod android;
#[cfg(all(target_os = "android", feature = "android"))]
mod android_updates;
mod blocking_worker;
mod session;
#[cfg(feature = "desktop")]
mod single_instance;
#[cfg(all(feature = "desktop", target_os = "linux"))]
pub mod linux_updates;
#[cfg(all(feature = "desktop", target_os = "windows"))]
pub mod windows_updates;
#[cfg(all(feature = "desktop", any(target_os = "linux", target_os = "windows")))]
pub mod update_entry;
#[cfg(any(all(feature = "desktop", any(target_os = "linux", target_os = "windows")), all(feature = "android", target_os = "android")))]
mod update_metadata;
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;
#[cfg(feature = "desktop")]
use std::sync::{Arc, LazyLock};

use app::{AppClient, AppStartupState, host::BackendLaunch};
use application_shell::ApplicationRoot;
#[cfg(feature = "kobo")]
use browser_ui::SaveKoboAutoRotate;
use browser_ui::{AppServices, AppStartup};
use gpui::{App, AppContext, Bounds, Entity, SharedString, WeakEntity, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;
#[cfg(feature = "kobo")]
use gpui_kobo::KoboKeyboardRoot;
use reader_ui::{ReaderPreferences, SaveReaderSettings};

use blocking_worker::BlockingWorker;
use session::{SessionSaveScheduler, UiSession, WindowGeometry, session_path};
#[cfg(feature = "desktop")]
pub use single_instance::DesktopInstance;

#[cfg(feature = "desktop")]
const DESKTOP_APP_ID: &str = "se.bokheim.Bokheim";

#[cfg(feature = "desktop")]
static DESKTOP_ICON: LazyLock<Option<Arc<image::RgbaImage>>> =
    LazyLock::new(|| image::load_from_memory_with_format(include_bytes!("../../ui/design-tokens/assets/icon/bokheim-180.png"), image::ImageFormat::Png).ok().map(|image| Arc::new(image.into_rgba8())));

#[cfg(feature = "desktop")]
fn desktop_window_identity() -> (Option<String>, Option<Arc<image::RgbaImage>>) {
    (Some(DESKTOP_APP_ID.to_owned()), DESKTOP_ICON.clone())
}

/// Small host-specific policy boundary. The complete browser and reader UI is shared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiLaunchOptions {
    /// Persist desktop window positions and sizes. Kobo windows are always fullscreen.
    pub persist_window_geometry: bool,
    /// Override the persisted Kobo auto-rotation preference for this launch, if any.
    pub kobo_auto_rotate: Option<bool>,
}

impl UiLaunchOptions {
    pub const DESKTOP: Self = Self { persist_window_geometry: true, kobo_auto_rotate: None };
    pub const KOBO: Self = Self { persist_window_geometry: false, kobo_auto_rotate: None };
    pub const MOBILE: Self = Self { persist_window_geometry: false, kobo_auto_rotate: None };
}

#[cfg(feature = "kobo")]
fn platform_content<V: gpui::Render + 'static>(content: Entity<V>, cx: &mut App) -> Entity<KoboKeyboardRoot<V>> {
    cx.new(|_| KoboKeyboardRoot::new(content))
}

#[cfg(all(any(feature = "desktop", feature = "mobile"), not(feature = "kobo")))]
fn platform_content<V: gpui::Render + 'static>(content: Entity<V>, _: &mut App) -> Entity<V> {
    content
}

/// Install and open the existing Bokheim browser/reader UI in the selected GPUI platform.
pub struct IncomingBook {
    pub name: String,
    pub path: std::path::PathBuf,
}
pub type IncomingBookResult = Result<IncomingBook, String>;

#[cfg(any(target_os = "android", test))]
fn stage_incoming_book(mut source: impl std::io::Read, path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write as _;
    const RESERVED_BYTES: u64 = 128 * 1024 * 1024;
    let mut destination = std::fs::OpenOptions::new().create_new(true).write(true).open(path)?;
    let directory = path.parent().ok_or_else(|| std::io::Error::other("incoming file has no parent directory"))?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes_until_check = 0_u64;
    loop {
        if bytes_until_check == 0 {
            if fs2::available_space(directory)? <= RESERVED_BYTES {
                return Err(std::io::Error::other("not enough free storage to stage book"));
            }
            bytes_until_check = 8 * 1024 * 1024;
        }
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        destination.write_all(&buffer[..read])?;
        bytes_until_check = bytes_until_check.saturating_sub(read as u64);
    }
    destination.flush()?;
    destination.sync_all()
}

#[cfg(test)]
mod incoming_tests {
    #[test]
    fn stages_an_incoming_reader_without_buffering_the_book() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("incoming.part");
        super::stage_incoming_book(std::io::Cursor::new(b"incoming book"), &path).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"incoming book");
    }
}

pub fn open_ui(
    backend_context: BackendLaunch, options: UiLaunchOptions, incoming_files: Option<async_channel::Receiver<IncomingBookResult>>, #[cfg(feature = "desktop")] instance_activations: Option<async_channel::Receiver<()>>, cx: &mut App,
) -> Option<WeakEntity<ApplicationRoot>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    if let Err(error) = cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/LibertinusSerif-Regular.otf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/LibertinusSerif-Bold.otf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/LibertinusSerif-Italic.otf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/BodoniModa-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/BodoniModa-SemiBold.ttf")),
        #[cfg(feature = "kobo")]
        Cow::Borrowed(include_bytes!("../../../reference/plato/fonts/TerminusTTF-Regular.ttf")),
        #[cfg(feature = "kobo")]
        Cow::Borrowed(include_bytes!("../../../reference/plato/fonts/TerminusTTF-Bold.ttf")),
    ]) {
        log::warn!("failed to install application fonts: {error}");
    }
    #[cfg(all(feature = "desktop", target_os = "linux"))]
    let update_controls = linux_updates::bind(cx);
    #[cfg(all(feature = "desktop", target_os = "windows"))]
    let update_controls = windows_updates::bind(cx);
    #[cfg(all(feature = "android", target_os = "android"))]
    let update_controls = android_updates::bind(cx);
    let session_path = session_path(backend_context.app_data_dir());
    let playback_restore_path = Some(backend_context.app_data_dir().join("active-audiobook.json"));
    let saved_session = Rc::new(RefCell::new(UiSession::load(&session_path)));

    gpui_component::init(cx);
    ui_components::init_page_scrolling(cx);
    browser_ui::bind_keys(cx);
    #[cfg(feature = "mobile")]
    cx.bind_keys([gpui::KeyBinding::new("back", browser_ui::DismissSettings, Some("Application"))]);
    reader_ui::bind_keys(cx);
    let persistence_worker = BlockingWorker::new(cx.background_executor(), 1);
    let session_saves = SessionSaveScheduler::new(saved_session.clone(), session_path, persistence_worker.clone());
    #[cfg(feature = "kobo")]
    let kobo_auto_rotate = {
        let fallback = saved_session.borrow().kobo_auto_rotate;
        let value = options.kobo_auto_rotate.unwrap_or(fallback);
        if options.kobo_auto_rotate.is_some_and(|override_value| override_value != fallback) {
            saved_session.borrow_mut().kobo_auto_rotate = value;
            session_saves.schedule(cx);
        }
        if let Err(error) = gpui_kobo::request_auto_rotation(value) {
            log::warn!("failed to apply Kobo auto-rotation setting: {error}");
        }
        value
    };

    #[cfg(all(target_os = "android", feature = "android"))]
    let android_private_library_root = backend_context.app_data_dir().join("libraries");
    let (backend_worker, backend_startup) = AppClient::start_native(backend_context).expect("failed to initialize application backend");
    #[cfg(all(target_os = "android", feature = "android"))]
    android::watch_storage_access(backend_worker.clone(), android_private_library_root, cx);
    let AppStartupState { libraries, browsing_preferences, account_status, reader_preferences: shared_reader_preferences } = backend_startup;
    let startup = AppStartup { libraries, browsing_preferences, account_status };
    saved_session.borrow_mut().reader_preferences.apply_synchronized(&shared_reader_preferences);

    let reader_settings = saved_session.clone();
    let reader_session_saves = session_saves.clone();
    let reader_preferences_backend = backend_worker.clone();
    let save_reader_settings: SaveReaderSettings = Rc::new(move |preferences: ReaderPreferences, cx: &mut App| {
        let mut session = reader_settings.borrow_mut();
        let previous = session.reader_preferences.clone();
        let patches = previous.synchronized_patches(&preferences);
        session.reader_preferences = preferences;
        drop(session);
        reader_session_saves.schedule(cx);
        if !patches.is_empty() {
            let backend = reader_preferences_backend.clone();
            cx.spawn(async move |_| {
                if let Err(error) = backend.update_reader_preferences(patches).await {
                    log::warn!("failed to persist reader preference: {error}");
                }
            })
            .detach();
        }
    });

    #[cfg(feature = "kobo")]
    let save_kobo_auto_rotate: SaveKoboAutoRotate = {
        let session = saved_session.clone();
        let session_saves = session_saves.clone();
        Rc::new(move |enabled: bool, cx: &mut App| {
            let mut session = session.borrow_mut();
            if session.kobo_auto_rotate == enabled {
                return;
            }
            session.kobo_auto_rotate = enabled;
            drop(session);
            session_saves.schedule(cx);
            if let Err(error) = gpui_kobo::request_auto_rotation(enabled) {
                log::warn!("failed to apply Kobo auto-rotation setting: {error}");
            }
        })
    };
    reader_ui::configure(saved_session.borrow().reader_preferences.clone(), save_reader_settings, cx);

    let application_root: Rc<RefCell<Option<WeakEntity<ApplicationRoot>>>> = Rc::new(RefCell::new(None));
    let application_root_for_incoming = application_root.clone();
    let application_root_for_host = application_root.clone();
    let application_backend = backend_worker.clone();
    let playback_restore_path_for_root = playback_restore_path.clone();

    #[cfg(feature = "desktop")]
    if let Some(activations) = instance_activations {
        cx.spawn(async move |cx| {
            while activations.recv().await.is_ok() {
                let _ = cx.update(|cx| cx.activate(true));
            }
        })
        .detach();
    }

    let browser_bounds = if options.persist_window_geometry {
        let bounds = saved_session.borrow().window.map(WindowGeometry::bounds).unwrap_or_else(|| Bounds::centered(None, size(px(1240.0), px(820.0)), cx));
        WindowBounds::Windowed(bounds)
    } else {
        WindowBounds::Windowed(Bounds { origin: gpui::point(px(0.0), px(0.0)), size: size(px(300.0), px(400.0)) })
    };
    #[cfg(feature = "desktop")]
    let (app_id, icon) = desktop_window_identity();
    #[cfg(any(feature = "kobo", feature = "mobile"))]
    let (app_id, icon) = (None, None);
    let window_persistence = options.persist_window_geometry.then(|| (saved_session.clone(), session_saves.clone()));
    let browser_window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(browser_bounds),
                titlebar: options.persist_window_geometry.then(|| gpui::TitlebarOptions { title: Some(SharedString::from(app::APP_NAME)), ..Default::default() }),
                focus: true,
                app_id,
                icon,
                window_decorations: cfg!(all(target_os = "linux", feature = "desktop", not(feature = "mobile"))).then_some(gpui::WindowDecorations::Server),
                ..Default::default()
            },
            move |window, cx| {
                #[cfg(feature = "kobo")]
                let services = AppServices::new(backend_worker, startup, save_kobo_auto_rotate.clone());
                #[cfg(all(any(feature = "desktop", feature = "mobile"), not(feature = "kobo")))]
                let services = AppServices::new(backend_worker, startup);
                #[cfg(any(all(feature = "desktop", any(target_os = "linux", target_os = "windows")), all(feature = "android", target_os = "android")))]
                let services = match update_controls { Some(updates) => services.with_updates(updates), None => services };
                #[cfg(all(feature = "desktop", not(feature = "mobile")))]
                {
                    let services = services.clone();
                    let backend = application_backend.clone();
                    let playback_restore_path_for_windows = playback_restore_path_for_root.clone();
                    cx.on_action(move |request: &browser_ui::OpenInNewWindow, cx| {
                        let services = services.clone();
                        let backend = backend.clone();
                        let playback_restore_path = playback_restore_path_for_windows.clone();
                        let target = request.0.clone();
                        cx.spawn(async move |cx| match services.for_new_window().await {
                            Ok(services) => {
                                let _ = cx.update(|cx| open_extra_window(services, backend, playback_restore_path, target, cx));
                            }
                            Err(error) => log::error!("could not prepare a new library window: {error}"),
                        })
                        .detach();
                    });
                }
                let application = cx.new(|cx| {
                    if let Some((session, saves)) = window_persistence {
                        cx.observe_window_bounds(window, move |_, window, cx| {
                            let geometry = WindowGeometry::from_bounds(window.bounds());
                            let mut session = session.borrow_mut();
                            if session.window == Some(geometry) {
                                return;
                            }
                            session.window = Some(geometry);
                            drop(session);
                            saves.schedule(cx);
                        })
                        .detach();
                    }
                    #[cfg(feature = "kobo")]
                    return ApplicationRoot::new(services, application_backend, app::APP_NAME, playback_restore_path_for_root, kobo_auto_rotate, window, cx);
                    #[cfg(not(feature = "kobo"))]
                    ApplicationRoot::new(services, application_backend, app::APP_NAME, playback_restore_path_for_root, window, cx)
                });
                *application_root.borrow_mut() = Some(application.downgrade());
                let content = platform_content(application, cx);
                cx.new(|cx| Root::new(content, window, cx))
            },
        )
        .expect("failed to open browsing window");

    #[cfg(feature = "desktop")]
    #[cfg(any(feature = "kobo", feature = "mobile"))]
    let _ = browser_window;
    if let Some(incoming_files) = incoming_files {
        forward_incoming_files(incoming_files, application_root_for_incoming, cx);
    }

    let quit_session_saves = session_saves.clone();
    cx.on_app_quit(move |_| {
        quit_session_saves.flush();
        async {}
    })
    .detach();
    cx.activate(true);
    let root = application_root_for_host.borrow().clone();
    root
}

#[cfg(all(feature = "desktop", not(feature = "mobile")))]
fn open_extra_window(services: AppServices, backend: AppClient, playback_restore_path: Option<std::path::PathBuf>, target: browser_ui::NewWindowTarget, cx: &mut App) {
    let (app_id, icon) = desktop_window_identity();
    let bounds = Bounds::centered(None, size(px(1240.0), px(820.0)), cx);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions { title: Some(app::APP_NAME.into()), ..Default::default() }),
            focus: true,
            app_id,
            icon,
            window_decorations: cfg!(all(target_os = "linux", feature = "desktop", not(feature = "mobile"))).then_some(gpui::WindowDecorations::Server),
            ..Default::default()
        },
        move |window, cx| {
            let application = cx.new(|cx| ApplicationRoot::new(services, backend, app::APP_NAME, playback_restore_path, window, cx));
            let target_application = application.downgrade();
            window.defer(cx, move |window, cx| {
                let _ = target_application.update(cx, |application, cx| application.open_window_target(target, window, cx));
            });
            let content = platform_content(application, cx);
            cx.new(|cx| Root::new(content, window, cx))
        },
    );
    if let Err(error) = result {
        log::error!("could not open a new library window: {error}");
    }
}

fn forward_incoming_files(files: async_channel::Receiver<IncomingBookResult>, application_root: Rc<RefCell<Option<WeakEntity<ApplicationRoot>>>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(result) = files.recv().await {
            let Some(root) = application_root.borrow().clone() else {
                if let Ok(file) = result {
                    let _ = std::fs::remove_file(file.path);
                }
                continue;
            };
            let file = match result {
                Ok(file) => file,
                Err(error) => {
                    let _ = root.update_in(cx, |root, window, cx| root.incoming_import_failed(error, window, cx));
                    continue;
                }
            };
            let fallback_path = file.path.clone();
            if root.update_in(cx, |root, window, cx| root.import_and_open_path(file.name, file.path, window, cx)).is_err() {
                let _ = std::fs::remove_file(fallback_path);
            }
        }
    })
    .detach();
}
