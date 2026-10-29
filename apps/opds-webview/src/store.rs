//! A native GPUI browser window with an embedded Wry webpage. Downloads
//! are reported to the app, which imports supported books into the library.
#[cfg(target_os = "linux")]
mod native_downloads;
#[cfg(target_os = "linux")]
mod native_events;
#[cfg(target_os = "linux")]
mod native_focus;
#[cfg(target_os = "linux")]
mod native_host;
#[cfg(target_os = "linux")]
mod native_navigation;
mod passwords;
use crate::address::{AddressHistory, normalize};
use anyhow::{Result, bail};
use app_preferences::ResolvedAppearance;
use gpui::prelude::*;
use gpui::{App, AppContext, Bounds, Context, Entity, Focusable, KeyDownEvent, MouseButton, Subscription, Window, WindowBounds, WindowOptions, canvas, deferred, div, px, rems, size};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Root, Sizable, WindowExt};
use library_opds::browser::address::{PAGE_METADATA_SCRIPT, PageMetadata};
use library_opds::browser::{BrowserFeedback, StoreEvent, StoreRequest, torrent_download};
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use reqwest_client::ReqwestClient;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    Arc, LazyLock, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use ui_components::{BrowserDownloadPhase, BrowserDownloads, Failure};
use ui_components::{SPACE_SM, SelectedPalette, TEXT_SM, apply_appearance, browser_address_control, browser_theme, error_text, outlined_icon_button, register_fonts, topbar_action_height_rem, user_facing_error};
use url::Url;
#[cfg(target_os = "linux")]
use webkit2gtk::{BackForwardListExt, BackForwardListItemExt, ContextMenuAction, ContextMenuExt, ContextMenuItemExt, WebViewExt};
#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS;
#[cfg(target_os = "linux")]
use wry::WebViewExtUnix;
#[cfg(target_os = "windows")]
use wry::WebViewExtWindows;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder};

const BOOKS: [&str; 9] = ["epub", "pdf", "mobi", "azw", "azw3", "m4b", "fb2", "fbz", "cbz"];
const STATUS_HEIGHT: f32 = 34.0;
static FEEDBACK: LazyLock<Mutex<Vec<Sender<Action>>>> = LazyLock::new(Mutex::default);

enum Action {
    PasswordResult {
        nonce: String,
        result: Result<crate::passwords::Reply, String>,
    },
    PasswordMessage(passwords::Message),
    PageLoaded(String),
    PageVisited(PageMetadata),
    Feedback(BrowserFeedback),
    Open(String),
    Refused(PathBuf),
    DownloadStarted(PathBuf),
    #[cfg(target_os = "linux")]
    DownloadProgress {
        path: PathBuf,
        percent: u8,
    },
    DownloadFinished {
        path: PathBuf,
        succeeded: bool,
    },
}

pub(crate) fn run() -> Result<()> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let request: StoreRequest = serde_json::from_str(&line)?;
    let start = Url::parse(&request.url)?;
    if !matches!(start.scheme(), "http" | "https") && start.as_str() != "about:blank" {
        bail!("a browser needs an HTTP address");
    }
    std::fs::create_dir_all(&request.downloads)?;
    #[cfg(target_os = "linux")]
    gtk::init()?;
    let web_context = crate::profile::context(&request.data_directory)?;
    let app_closed = Arc::new(AtomicBool::new(false));
    std::thread::Builder::new().name("browser-feedback".into()).spawn({
        let app_closed = app_closed.clone();
        move || {
            loop {
                let mut line = String::new();
                if std::io::stdin().read_line(&mut line).is_err() || line.is_empty() {
                    break;
                }
                if let Ok(feedback) = serde_json::from_str::<BrowserFeedback>(&line) {
                    FEEDBACK.lock().unwrap_or_else(PoisonError::into_inner).retain(|sender| sender.send(Action::Feedback(feedback.clone())).is_ok());
                }
            }
            app_closed.store(true, Ordering::Relaxed);
        }
    })?;
    gpui_platform::application().with_assets(ui_components::Assets).with_http_client(Arc::new(ReqwestClient::new())).run(move |cx: &mut App| {
        gpui_component::init(cx);
        if let Err(error) = register_fonts(cx) {
            eprintln!("Could not register browser fonts: {error}");
        }
        apply_appearance(&request.preferences, cx);
        #[cfg(target_os = "linux")]
        if let Some(settings) = gtk::Settings::default() {
            use gtk::prelude::GtkSettingsExt;
            settings.set_gtk_application_prefer_dark_theme(cx.global::<SelectedPalette>().1 == ResolvedAppearance::Dark);
        }
        let opened = open_browser_window(request, web_context, app_closed, cx);
        if let Err(error) = opened {
            eprintln!("Could not open the browser: {error}");
            cx.quit();
        }
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
    Ok(())
}

fn open_browser_window(request: StoreRequest, web_context: WebContext, app_closed: Arc<AtomicBool>, cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(1100.0), px(800.0)), cx);
    let (sender, receiver) = mpsc::channel();
    let profile_directory = web_context.data_directory().ok_or_else(|| anyhow::anyhow!("browser profile has no persistent storage directory"))?;
    let passwords = passwords::Autofill::new(crate::passwords::profile_directory(profile_directory), sender.clone());
    FEEDBACK.lock().unwrap_or_else(PoisonError::into_inner).push(sender.clone());
    cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, move |window, cx| {
        window.set_window_title(&request.title);
        let browser = cx.new(|cx| Browser::new(request, web_context, passwords, app_closed, sender, receiver, window, cx));
        let closing = browser.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            // Release the native surface before its GPUI parent disappears.
            let _ = closing.update(cx, |browser, cx| browser.close_native(window, cx));
            true
        });
        cx.new(|cx| Root::new(browser, window, cx))
    })?;
    cx.activate(true);
    Ok(())
}

struct Browser {
    passwords: passwords::Autofill,
    request: StoreRequest,
    app_closed: Arc<AtomicBool>,
    address: Entity<InputState>,
    history: AddressHistory,
    current_url: String,
    page: PageMetadata,
    suggestions: Vec<String>,
    selected: Option<usize>,
    editing: bool,
    error: Option<String>,
    navigation: (bool, bool),
    notification_subscription: Option<Subscription>,
    #[cfg(not(target_os = "linux"))]
    notification_height: f32,
    #[cfg(target_os = "linux")]
    native_focus: Option<native_focus::NativeFocus>,
    #[cfg(target_os = "linux")]
    native_navigation: Option<native_navigation::NativeNavigation>,
    webview: Option<WebView>,
    #[cfg(target_os = "linux")]
    native_host: Option<Rc<native_host::NativeHost>>,
    _web_context: WebContext,
    downloads: BrowserDownloads,
    receiver: Receiver<Action>,
    sender: Sender<Action>,
    _subscription: Subscription,
    page_layout: Option<(f32, f32, f32, f32, f32)>,
}

impl Browser {
    fn close_native(&mut self, window: &mut Window, cx: &mut App) {
        self.downloads.clear(window, cx);
        #[cfg(target_os = "linux")]
        {
            self.native_focus = None;
            self.native_navigation = None;
        }
        self.webview = None;
        #[cfg(target_os = "linux")]
        {
            self.native_host = None;
        }
    }

    fn new(request: StoreRequest, mut web_context: WebContext, passwords: passwords::Autofill, app_closed: Arc<AtomicBool>, sender: Sender<Action>, receiver: Receiver<Action>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_request = request.clone();
        let history_file = request.history_file.unwrap_or_else(|| request.data_directory.join("address-history.json"));
        let history = AddressHistory::load(history_file);
        let page = history.page(&request.url).cloned().unwrap_or_else(|| PageMetadata::for_url(&request.url));
        let downloads = request.downloads;
        let parent = NativeParent(window);
        #[cfg(target_os = "linux")]
        let native_host = native_host::NativeHost::new(&parent, page_bounds(window, controls_height(window)));
        let builder = WebViewBuilder::new_with_web_context(&mut web_context)
            .with_bounds(page_bounds(window, controls_height(window)))
            .with_url("about:blank")
            .with_focused(false)
            .with_ipc_handler(passwords.handler(sender.clone()))
            .with_on_page_load_handler({
                let sender = sender.clone();
                let guard = passwords.guard.clone();
                move |event, url| {
                    if matches!(event, PageLoadEvent::Started) {
                        guard.borrow_mut().take();
                    }
                    if matches!(event, PageLoadEvent::Finished) && matches!(Url::parse(&url).map(|url| url.scheme().to_owned()).as_deref(), Ok("http" | "https")) {
                        let _ = sender.send(Action::PageLoaded(url));
                    }
                }
            })
            .with_new_window_req_handler({
                let sender = sender.clone();
                move |url, _| {
                    if url.starts_with("magnet:") {
                        report(StoreEvent::Magnet { url });
                        return NewWindowResponse::Deny;
                    }
                    let _ = sender.send(Action::Open(url));
                    NewWindowResponse::Deny
                }
            })
            .with_navigation_handler({
                move |url| match Url::parse(&url) {
                    Ok(url) if url.scheme() == "magnet" => {
                        report(StoreEvent::Magnet { url: url.to_string() });
                        false
                    }
                    Ok(url) if !matches!(url.scheme(), "http" | "https" | "about" | "data" | "blob") => {
                        open_externally(&url);
                        false
                    }
                    _ => true,
                }
            });
        let builder = with_downloads(builder, downloads.clone(), sender.clone());
        #[cfg(target_os = "windows")]
        let builder = {
            use wry::{Theme, WebViewBuilderExtWindows};
            builder.with_theme(match cx.global::<SelectedPalette>().1 {
                ResolvedAppearance::Dark => Theme::Dark,
                ResolvedAppearance::Light => Theme::Light,
            })
        };
        #[cfg(target_os = "linux")]
        let (webview, native_host): (Result<WebView>, _) = {
            use wry::WebViewBuilderExtUnix;
            match native_host {
                Ok(host) => {
                    let page = builder.build_gtk(&host.container).map_err(Into::into);
                    if page.is_ok() {
                        host.show();
                    }
                    (page, Some(Rc::new(host)))
                }
                Err(error) => (Err(error), None),
            }
        };
        #[cfg(not(target_os = "linux"))]
        let webview = builder.build_as_child(&parent);
        let webview = webview.map_err(anyhow::Error::from).and_then(|page| {
            crate::profile::prepare(&page, &web_context)?;
            #[cfg(target_os = "linux")]
            page.webview().connect_context_menu(|_, menu, _, _| {
                for item in menu.items() {
                    if item.stock_action() == ContextMenuAction::DownloadLinkToDisk {
                        menu.remove(&item);
                    }
                }
                false
            });
            #[cfg(target_os = "linux")]
            native_downloads::observe(&page, sender.clone());
            if request.url == "about:blank" {
                let html = ui_components::browser_start_page(history.start_page_shortcuts().map(|saved| (saved.page.url.as_str(), saved.page.title.as_str(), saved.page.icon.as_deref())), browser_theme(cx));
                page.load_html(&html)?;
            } else {
                page.load_url(&request.url)?;
            }
            Ok(page)
        });
        let (webview, error) = match webview {
            Ok(webview) => {
                report(StoreEvent::Opened);
                (Some(webview), None)
            }
            Err(error) => {
                eprintln!("Could not embed the browser: {error}");
                (None, Some(user_facing_error(&error.to_string(), "Could not open the webpage. Try reopening the browser.").to_owned()))
            }
        };
        #[cfg(target_os = "linux")]
        let native_focus = webview.as_ref().and_then(|page| native_focus::NativeFocus::attach(page, native_host.as_ref()?.parent()));
        #[cfg(target_os = "linux")]
        let native_navigation = webview.as_ref().map(|page| native_navigation::NativeNavigation::attach(&page.webview()));
        let editing = request.url == "about:blank";
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Enter a web address or search Google"));
        address.update(cx, |address, cx| {
            address.set_value(display_url(&request.url), window, cx);
            if editing {
                address.focus(window, cx);
            }
        });
        if !editing {
            #[cfg(target_os = "linux")]
            if let Some(focus) = &native_focus {
                focus.page();
            }
            if let Some(page) = &webview {
                let _ = page.focus();
            }
        }
        let subscription = cx.subscribe_in(&address, window, |browser, _, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => {
                    browser.editing = true;
                    browser.error = None;
                    browser.suggest(cx);
                }
                InputEvent::Focus => {
                    #[cfg(target_os = "linux")]
                    if let Some(focus) = &browser.native_focus {
                        focus.toolbar();
                    }
                    browser.editing = true;
                    browser.suggest(cx);
                    browser.address.update(cx, |input, cx| input.set_selected_range(0..input.value().len(), cx));
                }
                InputEvent::Blur => {
                    browser.editing = false;
                    browser.suggestions.clear();
                    browser.selected = None;
                    browser.set_address(window, cx);
                }
                InputEvent::PressEnter { .. } => browser.submit(window, cx),
            }
            browser.layout(window);
            cx.notify();
        });
        cx.spawn_in(window, async move |browser, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                #[cfg(target_os = "linux")]
                native_events::pump(&gtk::glib::MainContext::default());
                if browser.update_in(cx, |browser, window, cx| browser.tick(window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            passwords,
            request: window_request,
            app_closed,
            address,
            history,
            current_url: request.url,
            page,
            suggestions: Vec::new(),
            selected: None,
            editing,
            error,
            navigation: (false, false),
            notification_subscription: None,
            webview,
            _web_context: web_context,
            downloads: BrowserDownloads::default(),
            receiver,
            sender,
            _subscription: subscription,
            page_layout: None,
            #[cfg(target_os = "linux")]
            native_focus,
            #[cfg(target_os = "linux")]
            native_navigation,
            #[cfg(target_os = "linux")]
            native_host,
            #[cfg(not(target_os = "linux"))]
            notification_height: 0.0,
        }
    }

    fn suggest(&mut self, cx: &App) {
        let input = self.address.read(cx).value();
        self.suggestions = self.history.suggestions(&input);
        self.selected = None;
    }

    fn set_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = display_url(&self.current_url).to_owned();
        self.address.update(cx, |input, cx| input.set_value(url, window, cx));
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let address = self.selected.and_then(|index| self.suggestions.get(index).cloned()).unwrap_or_else(|| self.address.read(cx).value().to_string());
        self.navigate(&address, window, cx);
    }

    fn navigate(&mut self, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.passwords.cancel();
        match normalize(address) {
            Ok(url) => {
                if let Err(error) = self.history.remember(address) {
                    eprintln!("Could not save browser address history: {error}");
                }
                if let Some(webview) = &self.webview {
                    #[cfg(target_os = "linux")]
                    let loaded: wry::Result<()> = {
                        if let Some(navigation) = &self.native_navigation {
                            navigation.load(&webview.webview(), url.as_str());
                        }
                        Ok(())
                    };
                    #[cfg(not(target_os = "linux"))]
                    let loaded = webview.load_url(url.as_str());
                    match loaded {
                        Ok(()) => {
                            self.current_url = url.to_string();
                            self.page = self.history.page(&self.current_url).cloned().unwrap_or_else(|| PageMetadata::for_url(&self.current_url));
                            self.error = None;
                            self.editing = false;
                            self.suggestions.clear();
                            self.selected = None;
                            window.blur();
                            #[cfg(target_os = "linux")]
                            if let Some(focus) = &self.native_focus {
                                focus.page();
                            }
                            let _ = webview.focus();
                            self.set_address(window, cx);
                        }
                        Err(error) => self.error = Some(user_facing_error(&error.to_string(), "Could not open this address. Check the address and try again.").to_owned()),
                    }
                }
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        self.layout(window);
        cx.notify();
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.passwords.expire();
        // Any remaining browser window must close when the main app exits,
        // including when the original browser window was closed first.
        if self.app_closed.load(Ordering::Relaxed) {
            self.close_native(window, cx);
            window.remove_window();
            return;
        }
        #[cfg(target_os = "linux")]
        if self.native_focus.as_mut().and_then(|focus| focus.sync()) == Some(true) {
            window.blur();
            self.editing = false;
            self.suggestions.clear();
            self.selected = None;
            self.set_address(window, cx);
            cx.notify();
        }
        while let Ok(action) = self.receiver.try_recv() {
            match action {
                Action::PasswordResult { nonce, result } => self.passwords.apply(self.webview.as_ref(), &nonce, result),
                Action::PasswordMessage(message) => self.passwords.receive(message),
                Action::PageLoaded(url) => {
                    if let Some(page) = &self.webview
                        && page.url().ok().as_deref() == Some(url.as_str())
                    {
                        self.passwords.install(page);
                        let sender = self.sender.clone();
                        let _ = page.evaluate_script_with_callback(PAGE_METADATA_SCRIPT, move |result| {
                            if let Ok(page) = serde_json::from_str::<PageMetadata>(&result)
                                && page.url == url
                            {
                                let _ = sender.send(Action::PageVisited(page));
                            }
                        });
                    }
                }
                Action::PageVisited(page) => {
                    if page.url == self.current_url || self.webview.as_ref().and_then(|view| view.url().ok()).as_deref() == Some(page.url.as_str()) {
                        self.page = page.clone();
                        cx.notify();
                    }
                    if let Err(error) = self.history.record_visit(page) {
                        eprintln!("Could not save browser page visit: {error}");
                    }
                }
                Action::Feedback(feedback) => self.downloads.apply(feedback, window, cx),
                Action::Open(url) => {
                    let mut request = self.request.clone();
                    request.url = url;
                    let opened = crate::profile::context(&request.data_directory).and_then(|context| open_browser_window(request, context, self.app_closed.clone(), cx));
                    if let Err(error) = opened {
                        window.push_notification(Notification::error(user_facing_error(&error.to_string(), "Could not open a new browser window. Please try again.")).autohide(false), cx);
                    }
                }
                Action::Refused(path) => {
                    self.downloads.apply(
                        BrowserFeedback {
                            replaces: None,
                            download: path.to_string_lossy().into_owned(),
                            name: download_name(&path),
                            phase: BrowserDownloadPhase::Failed(Some(Failure::DrmProtected)),
                            percent: None,
                            message: None,
                        },
                        window,
                        cx,
                    );
                }
                Action::DownloadStarted(path) => {
                    self.downloads.apply(BrowserFeedback { replaces: None, download: path.to_string_lossy().into_owned(), name: download_name(&path), phase: BrowserDownloadPhase::Downloading, percent: None, message: None }, window, cx);
                }
                #[cfg(target_os = "linux")]
                Action::DownloadProgress { path, percent } => {
                    self.downloads.progress(&path.to_string_lossy(), Some(percent), window, cx);
                }
                Action::DownloadFinished { path, succeeded } => {
                    let id = path.to_string_lossy().into_owned();
                    self.downloads.apply(
                        BrowserFeedback { replaces: None, download: id, name: download_name(&path), phase: if succeeded { BrowserDownloadPhase::Completed } else { BrowserDownloadPhase::Failed(None) }, percent: None, message: None },
                        window,
                        cx,
                    );
                }
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(page) = &self.webview
            && let Some(navigation) = &self.native_navigation
        {
            navigation.tick(&page.webview());
        }
        #[cfg(target_os = "linux")]
        let pending_navigation = self.native_navigation.as_ref().is_some_and(|navigation| navigation.pending());
        #[cfg(not(target_os = "linux"))]
        let pending_navigation = false;
        if !pending_navigation
            && let Some(page) = &self.webview
            && let Ok(url) = page.url()
            && url != self.current_url
        {
            let unchanged_input = self.address.read(cx).value().as_str() == display_url(&self.current_url);
            self.current_url = url;
            if self.page.url != self.current_url {
                self.page = self.history.page(&self.current_url).cloned().unwrap_or_else(|| PageMetadata::for_url(&self.current_url));
            }
            if !self.editing || unchanged_input {
                self.set_address(window, cx);
            }
            cx.notify();
        }
        let navigation = self.webview.as_ref().map(navigation_state).unwrap_or_default();
        if navigation != self.navigation {
            self.navigation = navigation;
            cx.notify();
        }
        self.layout(window);
    }

    fn toolbar_height(&self, window: &Window) -> f32 {
        let top = controls_height(window) + if self.error.is_some() { STATUS_HEIGHT } else { 0.0 };
        #[cfg(not(target_os = "linux"))]
        let top = top + self.suggestions.len() as f32 * suggestion_height(window);
        top
    }

    fn navigate_history(&mut self, forward: bool) {
        self.passwords.cancel();
        if let Some(page) = &self.webview {
            #[cfg(target_os = "linux")]
            if let Some(navigation) = &self.native_navigation {
                navigation.history(&page.webview(), forward);
            }
            #[cfg(not(target_os = "linux"))]
            navigate_history(page, forward);
        }
    }

    fn reload(&mut self) {
        self.passwords.cancel();
        if let Some(page) = &self.webview {
            #[cfg(target_os = "linux")]
            if let Some(navigation) = &self.native_navigation {
                navigation.reload(&page.webview());
            }
            #[cfg(not(target_os = "linux"))]
            let _ = page.reload();
        }
    }

    fn layout(&mut self, window: &Window) {
        let viewport = window.viewport_size();
        #[cfg(target_os = "linux")]
        let bottom = 0.0;
        #[cfg(not(target_os = "linux"))]
        let bottom = self.notification_height;
        let layout = (f32::from(viewport.width), f32::from(viewport.height), self.toolbar_height(window), window.scale_factor(), bottom);
        if self.page_layout != Some(layout) {
            let bounds = page_bounds(window, layout.2);
            #[cfg(not(target_os = "linux"))]
            let bounds = {
                let mut bounds = bounds;
                let size = bounds.size.to_physical::<f64>(1.0);
                bounds.size = PhysicalSize::new(size.width, (size.height - f64::from(bottom * layout.3)).max(1.0)).into();
                bounds
            };
            #[cfg(target_os = "linux")]
            if let Some(host) = &self.native_host {
                let _ = host.set_bounds(bounds);
            }
            #[cfg(not(target_os = "linux"))]
            if let Some(page) = &self.webview {
                let _ = page.set_bounds(bounds);
            }
            self.page_layout = Some(layout);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" | "up" if !self.suggestions.is_empty() => {
                let count = self.suggestions.len();
                self.selected = Some(match (event.keystroke.key.as_str(), self.selected) {
                    ("down", Some(index)) => (index + 1) % count,
                    ("down", None) => 0,
                    (_, Some(index)) => (index + count - 1) % count,
                    (_, None) => count - 1,
                });
                cx.stop_propagation();
                cx.notify();
            }
            "escape" => {
                self.suggestions.clear();
                self.selected = None;
                self.editing = false;
                self.set_address(window, cx);
                window.blur();
                #[cfg(target_os = "linux")]
                if let Some(focus) = &self.native_focus {
                    focus.page();
                }
                if let Some(page) = &self.webview {
                    let _ = page.focus();
                }
                self.layout(window);
                cx.stop_propagation();
                cx.notify();
            }
            _ => {}
        }
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.layout(window);
        let theme = browser_theme(cx);
        let row_height = suggestion_height(window);
        let active = self.editing || self.address.read(cx).focus_handle(cx).is_focused(window);
        let mut address = browser_address_control(&self.address, active, self.page.display_name().to_owned(), self.page.icon.as_deref(), theme);
        if !self.suggestions.is_empty() {
            let mut popup = div()
                .id("browser-address-suggestions")
                .absolute()
                .top(rems(topbar_action_height_rem()))
                .left_0()
                .w_full()
                .h(px(self.suggestions.len() as f32 * row_height + 2.0))
                .flex()
                .flex_col()
                .bg(theme.page_bg)
                .border_1()
                .border_color(theme.rule)
                .text_color(theme.text)
                .text_size(rems(TEXT_SM))
                .occlude();
            for (index, suggestion) in self.suggestions.iter().cloned().enumerate() {
                popup = popup.child(
                    div()
                        .id(("address-suggestion", index))
                        .h(px(row_height))
                        .px(px(SPACE_SM))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_SM))
                        .cursor_pointer()
                        .bg(if self.selected == Some(index) { theme.accent_wash } else { theme.page_bg })
                        .hover(|style| style.bg(theme.hover))
                        // Selecting a suggestion must not blur the address field first.
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(move |browser, _, window, cx| browser.navigate(&suggestion, window, cx)))
                        .child(Icon::new(IconName::Search).small().text_color(theme.text_muted))
                        .child(div().flex_1().min_w_0().text_ellipsis().child(self.suggestions[index].clone())),
                );
            }
            #[cfg(target_os = "linux")]
            if let Some(host) = self.native_host.clone() {
                let top = self.toolbar_height(window);
                popup = popup.child(
                    canvas(
                        move |bounds, window, _| {
                            host.set_overlay("suggestions", page_bounds(window, top), Some(bounds), window.scale_factor());
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                );
            }
            address = address.child(deferred(popup).with_priority(1));
        } else {
            #[cfg(target_os = "linux")]
            if let Some(host) = &self.native_host {
                host.set_overlay("suggestions", page_bounds(window, self.toolbar_height(window)), None, window.scale_factor());
            }
        }
        let mut toolbar = div()
            .id("browser-toolbar")
            .flex()
            .flex_col()
            .w_full()
            .bg(theme.page_bg)
            .text_color(theme.text)
            .capture_key_down(cx.listener(Self::key_down))
            .capture_any_mouse_down(cx.listener(|browser, _, _, _| {
                #[cfg(target_os = "linux")]
                if let Some(focus) = &browser.native_focus {
                    focus.toolbar();
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_SM))
                    .px(px(SPACE_SM))
                    .h(px(controls_height(window)))
                    .child(
                        outlined_icon_button("back", "Back", IconName::ArrowLeft)
                            .disabled(!self.navigation.0)
                            .when(!self.navigation.0, |button| button.text_color(theme.text_muted.opacity(0.5)).cursor_default())
                            .on_click(cx.listener(|browser, _, _, _| browser.navigate_history(false))),
                    )
                    .child(
                        outlined_icon_button("forward", "Forward", IconName::ArrowRight)
                            .disabled(!self.navigation.1)
                            .when(!self.navigation.1, |button| button.text_color(theme.text_muted.opacity(0.5)).cursor_default())
                            .on_click(cx.listener(|browser, _, _, _| browser.navigate_history(true))),
                    )
                    .child(
                        outlined_icon_button("reload", "Reload", Icon::empty().path("icons/arrow-clockwise.svg"))
                            .disabled(self.webview.is_none() || self.current_url == "about:blank")
                            .when(self.webview.is_none() || self.current_url == "about:blank", |button| button.text_color(theme.text_muted.opacity(0.5)).cursor_default())
                            .on_click(cx.listener(|browser, _, _, _| browser.reload())),
                    )
                    .child(address),
            );
        if let Some(error) = &self.error {
            toolbar = toolbar.child(error_text(theme).h(px(STATUS_HEIGHT)).px(px(SPACE_SM)).flex().items_center().child(error.clone()));
        }
        // Use the same notification list, visuals, and lifecycle as the main
        // app. Reveal its bounds through the native surface without resizing it.
        let notifications = Root::read(window, cx).notification.clone();
        if self.notification_subscription.is_none() {
            self.notification_subscription = Some(cx.observe(&notifications, |_, _, cx| cx.notify()));
        }
        let visible = !notifications.read(cx).notifications().is_empty();
        let mut content = div().relative().size_full().flex().flex_col().bg(theme.page_bg).child(toolbar).child(div().flex_1());
        if visible {
            let mut layer = div()
                .id("browser-download-toasts")
                .absolute()
                .bottom_0()
                .right_0()
                .capture_any_mouse_down(cx.listener(|browser, _, _, _| {
                    #[cfg(target_os = "linux")]
                    if let Some(focus) = &browser.native_focus {
                        focus.toolbar();
                    }
                }))
                .child(notifications);
            #[cfg(target_os = "linux")]
            if let Some(host) = self.native_host.clone() {
                let top = self.toolbar_height(window);
                let inset = cx.theme().notification.margins.top - px(8.0);
                layer = layer.child(
                    canvas(
                        move |mut bounds, window, _| {
                            // Exclude the list's empty top margin; allow its shadow and
                            // slide animation in the right and bottom edge margins.
                            bounds.origin.y += inset;
                            bounds.size.height -= inset;
                            bounds.origin.x -= px(8.0);
                            bounds.size.width += px(8.0);
                            host.set_overlay("notifications", page_bounds(window, top), Some(bounds), window.scale_factor());
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                );
            }
            #[cfg(not(target_os = "linux"))]
            {
                // These native child surfaces need space reserved for GPUI
                // notifications, just as they do for address suggestions.
                let browser = cx.entity().downgrade();
                let inset = cx.theme().notification.margins.top - px(8.0);
                layer = layer.child(
                    canvas(
                        move |bounds, window, cx| {
                            let height = f32::from(bounds.size.height - inset).clamp(0.0, (f32::from(window.viewport_size().height) - controls_height(window)).max(0.0));
                            cx.defer(move |cx| {
                                let _ = browser.update(cx, |browser, cx| {
                                    if browser.notification_height != height {
                                        browser.notification_height = height;
                                        cx.notify();
                                    }
                                });
                            });
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                );
            }
            content = content.child(deferred(layer).with_priority(2));
        } else {
            #[cfg(not(target_os = "linux"))]
            {
                self.notification_height = 0.0;
                self.layout(window);
            }
            #[cfg(target_os = "linux")]
            if let Some(host) = &self.native_host {
                host.set_overlay("notifications", page_bounds(window, self.toolbar_height(window)), None, window.scale_factor());
            }
        }
        content
    }
}

fn download_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

fn navigation_state(page: &WebView) -> (bool, bool) {
    #[cfg(target_os = "linux")]
    {
        let native = page.webview();
        // The blank page used to prepare cookies is not a useful Back target.
        let back = native.back_forward_list().and_then(|list| list.back_item()).is_some_and(|item| item.uri().is_some_and(|url| url != "about:blank"));
        (back && native.can_go_back(), native.can_go_forward())
    }
    #[cfg(target_os = "windows")]
    {
        let native = page.webview();
        let (mut back, mut forward) = (Default::default(), Default::default());
        unsafe {
            let _ = native.CanGoBack(&mut back);
            let _ = native.CanGoForward(&mut forward);
        }
        (back.as_bool(), forward.as_bool())
    }
    #[cfg(target_os = "macos")]
    {
        let native = page.webview();
        // SAFETY: GPUI calls this on the main thread, and Wry keeps the native
        // WKWebView alive for the lifetime of `page`.
        unsafe { (native.canGoBack(), native.canGoForward()) }
    }
}

#[cfg(not(target_os = "linux"))]
fn navigate_history(page: &WebView, forward: bool) {
    #[cfg(target_os = "windows")]
    {
        let native = page.webview();
        unsafe {
            let _ = if forward { native.GoForward() } else { native.GoBack() };
        }
    }
    #[cfg(target_os = "macos")]
    {
        let native = page.webview();
        // SAFETY: Navigation runs on the main thread while `page` owns the
        // live WKWebView and its navigation history.
        unsafe {
            if forward {
                let _ = native.goForward();
            } else {
                let _ = native.goBack();
            }
        }
    }
}

fn controls_height(window: &Window) -> f32 {
    f32::from(window.rem_size()) * topbar_action_height_rem() + 2.0 * SPACE_SM
}

fn suggestion_height(window: &Window) -> f32 {
    f32::from(window.rem_size()) * TEXT_SM + 2.0 * SPACE_SM
}

fn display_url(url: &str) -> &str {
    if url == "about:blank" { "" } else { url }
}

fn page_bounds(window: &Window, top: f32) -> Rect {
    let viewport = window.viewport_size();
    let scale = f64::from(window.scale_factor());
    // GPUI and GTK can use different logical scale factors under XWayland.
    Rect { position: PhysicalPosition::new(0.0, top as f64 * scale).into(), size: PhysicalSize::new((f64::from(viewport.width) * scale).max(1.0), ((f64::from(viewport.height) - top as f64) * scale).max(1.0)).into() }
}

/// GPUI exposes an XCB handle; Wry expects the same X11 window ID in an
/// Xlib handle and obtains its display connection from GTK itself.
struct NativeParent<'a>(&'a Window);

impl HasWindowHandle for NativeParent<'_> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = HasWindowHandle::window_handle(self.0)?.as_raw();
        #[cfg(target_os = "linux")]
        let raw = match raw {
            RawWindowHandle::Xcb(handle) => raw_window_handle::XlibWindowHandle::new(handle.window.get().into()).into(),
            raw => raw,
        };
        // The borrowed GPUI window owns this ID for the lifetime of self.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

fn with_downloads<'a>(builder: WebViewBuilder<'a>, downloads: PathBuf, sender: Sender<Action>) -> WebViewBuilder<'a> {
    let saving: Rc<RefCell<HashMap<String, VecDeque<PathBuf>>>> = Rc::default();
    let started = {
        let saving = saving.clone();
        let sender = sender.clone();
        move |url: String, path: &mut PathBuf| {
            let name = path.file_name().map_or_else(|| PathBuf::from("download"), PathBuf::from);
            let Some(destination) = unused_path_unless(&downloads, &name, |candidate| saving.borrow().values().any(|paths| paths.iter().any(|path| path == candidate))) else {
                eprintln!("Could not find an unused download filename");
                return false;
            };
            *path = destination;
            saving.borrow_mut().entry(url).or_default().push_back(path.clone());
            let _ = sender.send(Action::DownloadStarted(path.clone()));
            true
        }
    };
    let completed = move |url: String, path: Option<PathBuf>, succeeded: bool| {
        let mut saving = saving.borrow_mut();
        let pending = saving.get_mut(&url);
        let path = match (pending, path) {
            (Some(paths), Some(path)) => {
                if let Some(index) = paths.iter().position(|pending| *pending == path) {
                    paths.remove(index);
                }
                Some(path)
            }
            (Some(paths), None) => paths.pop_front(),
            (None, path) => path,
        };
        if saving.get(&url).is_some_and(VecDeque::is_empty) {
            saving.remove(&url);
        }
        let Some(path) = path else { return };
        if !succeeded {
            let _ = std::fs::remove_file(&path);
            let _ = sender.send(Action::DownloadFinished { path, succeeded: false });
            return;
        }
        if torrent_download(&path) {
            let download = path.to_string_lossy().into_owned();
            let _ = sender.send(Action::Feedback(BrowserFeedback {
                replaces: None,
                download: download.clone(),
                name: download_name(&path),
                phase: BrowserDownloadPhase::Preparing,
                percent: None,
                message: Some("Resolving torrent metadata".into()),
            }));
            report(StoreEvent::TorrentFile { path, download: Some(download) });
            return;
        }
        match book(&path) {
            Some(book) => {
                let download = path.to_string_lossy().into_owned();
                let _ =
                    sender.send(Action::Feedback(BrowserFeedback { replaces: None, download: download.clone(), name: download_name(&book), phase: BrowserDownloadPhase::Preparing, percent: None, message: Some("Adding to library".into()) }));
                report(StoreEvent::Downloaded { path: book, download: Some(download) });
            }
            None => {
                let _ = std::fs::remove_file(&path);
                let _ = sender.send(Action::Refused(path));
            }
        }
    };
    builder.with_download_started_handler(started).with_download_completed_handler(completed)
}

/// The downloaded file as a book the library imports, renamed to the
/// extension its contents call for; `None` for anything else.
fn book(path: &Path) -> Option<PathBuf> {
    let mut head = Vec::new();
    std::fs::File::open(path).ok()?.take(58).read_to_end(&mut head).ok()?;
    let sniffed = if head.starts_with(b"%PDF-") {
        Some("pdf")
    } else if head.starts_with(b"PK\x03\x04") && head.get(30..58) == Some(b"mimetypeapplication/epub+zip".as_slice()) {
        Some("epub")
    } else {
        None
    };
    let extension = path.extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase);
    match sniffed {
        Some(sniffed) if extension.as_deref() != Some(sniffed) => {
            let named = unused_path(path.parent()?, &path.with_extension(sniffed))?;
            std::fs::rename(path, &named).ok()?;
            Some(named)
        }
        Some(_) => Some(path.to_owned()),
        None => (extension.is_some_and(|extension| BOOKS.contains(&extension.as_str())) || path.file_name().is_some_and(|name| name.to_string_lossy().to_ascii_lowercase().ends_with(".fb2.zip"))).then(|| path.to_owned()),
    }
}

/// `name` in `directory`, numbered where that name is taken.
fn unused_path(directory: &Path, name: &Path) -> Option<PathBuf> {
    unused_path_unless(directory, name, |_| false)
}

fn unused_path_unless(directory: &Path, name: &Path, occupied: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let extension = name.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
    (1..=u64::MAX).map(|number| if number == 1 { directory.join(format!("{stem}{extension}")) } else { directory.join(format!("{stem} ({number}){extension}")) }).find(|path| !path.exists() && !occupied(path))
}

fn report(event: StoreEvent) {
    let mut stdout = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, &event);
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}

fn open_externally(url: &Url) {
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    let _ = command.arg(url.as_str()).spawn();
}
