//! GPUI browser chrome around the Android webpage. Native code only owns the
//! WebView and file transfers; layout, controls and feedback belong to GPUI.
use app::host::browser_address::{AddressHistory, PAGE_METADATA_SCRIPT, PageMetadata, normalize};
use async_channel::{Receiver, Sender};
use gpui::prelude::*;
use gpui::{App, AppContext, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Subscription, Task, Window, canvas, deferred, div, px, rems};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::notification::Notification;
use gpui_component::progress::Progress;
use gpui_component::{ActiveTheme, IconName, Root};
#[cfg(not(test))]
use reqwest_client::ReqwestClient;
use serde::Deserialize;
use serde_json::{Value, from_str, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use ui_components::{self as components, BrowserDownloadPhase, BrowserDownloadUpdate, BrowserDownloads};

type Transport = dyn Fn(&str) -> Result<(), String> + Send + Sync;
type Feedback = dyn Fn(u64, &str) + Send + Sync;
static BRIDGE: OnceLock<Bridge> = OnceLock::new();

struct Bridge {
    native: Arc<Transport>,
    feedback: Arc<Feedback>,
    messages: Sender<Message>,
    receiver: Mutex<Option<Receiver<Message>>>,
}

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Command {
    Open { id: u64, request: Value },
    Close { id: u64 },
    Feedback { id: u64, feedback: BrowserDownloadUpdate },
}

enum Message {
    Command(Command),
    Native { id: u64, event: NativeEvent },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeEvent {
    Opened,
    PageVisited {
        #[serde(flatten)]
        page: PageMetadata,
    },
    Navigation {
        url: String,
        back: bool,
    },
    PageProgress {
        percent: u8,
    },
    DownloadProgress {
        download: String,
        percent: Option<u8>,
    },
    DownloadStarted {
        download: String,
        name: String,
    },
    DownloadWaiting {
        download: String,
    },
    CancelDownload {
        download: String,
    },
    DownloadFinished {
        download: String,
        name: String,
        error: Option<String>,
        #[serde(default)]
        torrent: bool,
    },
    Status {
        message: String,
    },
    Error {
        message: String,
    },
    Focused,
    BackPressed,
    RefreshRequested,
    Closed,
    #[serde(other)]
    Other,
}

pub(crate) fn initialize(native: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static, feedback: impl Fn(u64, &str) + Send + Sync + 'static) {
    let (messages, receiver) = async_channel::unbounded();
    let _ = BRIDGE.set(Bridge { native: Arc::new(native), feedback: Arc::new(feedback), messages, receiver: Mutex::new(Some(receiver)) });
}

/// Backend requests enter the GPUI thread before a native page is attached.
pub(crate) fn command(message: &str) -> Result<(), String> {
    let command = from_str(message).map_err(|error| format!("invalid browser command: {error}"))?;
    BRIDGE.get().ok_or("the browser UI is unavailable")?.messages.try_send(Message::Command(command)).map_err(|_| "the browser UI has closed".into())
}

/// JNI reports downloads to the backend and page state to the GPUI view.
pub(crate) fn receive(id: u64, message: &str) {
    let Some(bridge) = BRIDGE.get() else { return };
    (bridge.feedback)(id, message);
    if let Ok(event) = from_str(message) {
        let _ = bridge.messages.try_send(Message::Native { id, event });
    }
}

pub(crate) fn wrap<V: Render + 'static>(content: Entity<V>, window: &mut Window, cx: &mut App) -> Result<Entity<BrowserRoot<V>>, String> {
    let bridge = BRIDGE.get().ok_or("Android browser bridge was not initialized")?;
    let receiver = bridge.receiver.lock().map_err(|_| "Android browser bridge state is unavailable")?.take().ok_or("Android browser already has a GPUI host")?;
    Ok(cx.new(|cx| BrowserRoot::new(content, receiver, bridge.native.clone(), window, cx)))
}

pub(crate) struct BrowserRoot<V: Render + 'static> {
    content: Entity<V>,
    browser: Option<Entity<Browser>>,
    native: Arc<Transport>,
    downloads: BrowserDownloads,
    notifications: Option<Subscription>,
    _messages: Task<()>,
    _closed: Option<Subscription>,
}

fn download_toast(id: u64, download: &str, notification: Notification, window: &mut Window, cx: &mut App) {
    components::browser_download_notification(format!("{id}:{download}"), notification, window, cx);
}

impl<V: Render + 'static> BrowserRoot<V> {
    fn new(content: Entity<V>, messages: Receiver<Message>, native: Arc<Transport>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let task = cx.spawn_in(window, async move |root, cx| {
            while let Ok(message) = messages.recv().await {
                if root.update_in(cx, |root, window, cx| root.handle(message, window, cx)).is_err() {
                    break;
                }
            }
        });
        let root = cx.entity().downgrade();
        let downloads = BrowserDownloads::default().on_cancel(move |download, cx| {
            let _ = root.update(cx, |root, _| root.cancel_download(download));
        });
        Self { content, browser: None, native, downloads, notifications: None, _messages: task, _closed: None }
    }

    pub(crate) fn native_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(browser) = &self.browser else { return false };
        browser.update(cx, |browser, cx| browser.back(window, cx));
        true
    }

    fn cancel_download(&mut self, download: &str) {
        let Some((id, transfer)) = download.split_once(':') else { return };
        let Ok(id) = id.parse::<u64>() else { return };
        if self.downloads.cancel(download) {
            if let Err(error) = (self.native)(&json!({ "command": "cancel_download", "id": id, "download": transfer }).to_string()) {
                log::warn!("Could not cancel browser download: {error}");
            }
        }
    }

    fn handle(&mut self, message: Message, window: &mut Window, cx: &mut Context<Self>) {
        match message {
            Message::Command(Command::Open { id, request }) => {
                // Android has one GPUI window. Replacing its browser closes
                // the old page while its downloads can finish independently.
                self.close_browser(window, cx);
                let browser = cx.new(|cx| Browser::new(id, request, self.native.clone(), window, cx));
                self._closed = Some(cx.subscribe_in(&browser, window, |root, _, _: &Closed, window, cx| {
                    root.close_browser(window, cx);
                    cx.notify();
                }));
                self.browser = Some(browser);
            }
            Message::Command(Command::Close { id }) => {
                if self.browser.as_ref().is_some_and(|browser| browser.read(cx).id == id) {
                    self.close_browser(window, cx);
                }
            }
            Message::Command(Command::Feedback { id, feedback }) => self.download_update(id, feedback, window, cx),
            Message::Native { id, event } => match event {
                NativeEvent::DownloadStarted { download, name } => {
                    self.downloads.start(&format!("{id}:{download}"));
                    self.download_update(id, BrowserDownloadUpdate { replaces: None, download, name, phase: BrowserDownloadPhase::Downloading, percent: None, message: None }, window, cx);
                }
                NativeEvent::CancelDownload { download } => self.download_update(id, BrowserDownloadUpdate { replaces: None, download, name: String::new(), phase: BrowserDownloadPhase::Dismissed, percent: None, message: None }, window, cx),
                NativeEvent::DownloadProgress { download, percent } => self.downloads.progress(&format!("{id}:{download}"), percent, window, cx),
                NativeEvent::DownloadWaiting { download } => self.downloads.waiting(&format!("{id}:{download}"), window, cx),
                NativeEvent::DownloadFinished { download, name, error, torrent } => {
                    // Torrent metadata hands this same entry to the backend's
                    // progress publisher; it is not a completed payload yet.
                    // HTTP books wait for the backend's import result before
                    // reporting completion while this browser is open.
                    if (!torrent || error.is_some()) && self.downloads.get(&format!("{id}:{download}")).is_some() {
                        // The platform webview reports download failures only as text.
                        if let Some(error) = &error {
                            log::warn!("browser download {download} failed: {error}");
                        }
                        self.download_update(
                            id,
                            BrowserDownloadUpdate {
                                replaces: None,
                                download,
                                name,
                                phase: if error.is_some() { BrowserDownloadPhase::Failed(None) } else { BrowserDownloadPhase::Preparing },
                                percent: None,
                                message: error.is_none().then(|| "Adding to library".into()),
                            },
                            window,
                            cx,
                        );
                    }
                }
                NativeEvent::Status { message } => {
                    if self.browser.as_ref().is_some_and(|browser| browser.read(cx).id == id) {
                        download_toast(id, "browser-status", Notification::info(message), window, cx);
                    }
                }
                event => {
                    if let Some(browser) = self.browser.as_ref().filter(|browser| browser.read(cx).id == id) {
                        browser.update(cx, |browser, cx| browser.native_event(event, window, cx));
                    }
                }
            },
        }
        cx.notify();
    }

    fn download_update(&mut self, id: u64, mut update: BrowserDownloadUpdate, window: &mut Window, cx: &mut Context<Self>) {
        update.download = format!("{id}:{}", update.download);
        update.replaces = update.replaces.map(|previous| format!("{id}:{previous}"));
        if self.browser.as_ref().is_some_and(|browser| browser.read(cx).id == id) || self.downloads.get(&update.download).is_some() || update.replaces.as_ref().is_some_and(|previous| self.downloads.get(previous).is_some()) {
            self.downloads.apply(update, window, cx);
        }
    }

    fn close_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser = None;
        self._closed = None;
        self.downloads.clear(window, cx);
        self.notifications = None;
        window.blur();
    }
}

impl<V: Render + 'static> Render for BrowserRoot<V> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(browser) = &self.browser else { return div().size_full().child(self.content.clone()) };
        let notifications = Root::read(window, cx).notification.clone();
        if self.notifications.is_none() {
            self.notifications = Some(cx.observe(&notifications, |_, _, cx| cx.notify()));
        }
        let visible = !notifications.read(cx).notifications().is_empty();
        if !visible && browser.read(cx).notification_height != px(0.0) {
            browser.update(cx, |browser, cx| {
                browser.notification_height = px(0.0);
                cx.notify();
            });
        }
        let insets = window.insets();
        let measured_browser = browser.downgrade();
        let inset = cx.theme().notification.margins.top - px(8.0);
        div().relative().size_full().child(browser.clone()).when(visible, |view| {
            view.child(
                deferred(
                    div()
                        .absolute()
                        .bottom(insets.safe_area.bottom.max(insets.ime.bottom))
                        .right(insets.safe_area.right)
                        .debug_selector(|| "android-browser-download-toasts".into())
                        .capture_any_mouse_down(cx.listener(|root, _, _, cx| {
                            if let Some(browser) = &root.browser {
                                browser.update(cx, |browser, _| browser.send("focus_controls", json!({})));
                            }
                        }))
                        .child(notifications)
                        .child(
                            canvas(
                                move |bounds, window, cx| {
                                    let height = (bounds.size.height - inset).max(px(0.0)).min(window.viewport_size().height);
                                    let browser = measured_browser.clone();
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
                        ),
                )
                .with_priority(2),
            )
        })
    }
}

struct Closed;
impl EventEmitter<Closed> for Browser {}

struct Browser {
    id: u64,
    native: Arc<Transport>,
    address: Entity<InputState>,
    history: Option<AddressHistory>,
    suggestions: Vec<String>,
    selected: Option<usize>,
    editing: bool,
    focus: FocusHandle,
    current_url: String,
    page: PageMetadata,
    can_go_back: bool,
    page_progress: u8,
    notification_height: Pixels,
    error: Option<String>,
    opened: bool,
    bounds: Option<[i32; 4]>,
    _input: Subscription,
}

impl Browser {
    fn new(id: u64, mut request: Value, native: Arc<Transport>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        #[cfg(not(test))]
        cx.set_http_client(Arc::new(ReqwestClient::new()));
        let url = request["url"].as_str().unwrap_or("about:blank").to_owned();
        let history = request["history_file"].as_str().map(PathBuf::from).or_else(|| request["data_directory"].as_str().map(|path| PathBuf::from(path).join("address-history.json"))).map(AddressHistory::load);
        let page = history.as_ref().and_then(|history| history.page(&url)).cloned().unwrap_or_else(|| PageMetadata::for_url(&url));
        request["start_page_html"] =
            components::browser_start_page(history.iter().flat_map(|history| history.start_page_shortcuts()).map(|saved| (saved.page.url.as_str(), saved.page.title.as_str(), saved.page.icon.as_deref())), components::browser_theme(cx)).into();
        request["page_metadata_script"] = PAGE_METADATA_SCRIPT.into();
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Web address or search").default_value(if url == "about:blank" { "" } else { &url }));
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let subscription = cx.subscribe_in(&address, window, |browser, input, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => {
                    if !input.read(cx).focus_handle(cx).is_focused(window) {
                        return;
                    }
                    browser.editing = true;
                    browser.error = None;
                    browser.suggest(cx);
                }
                InputEvent::Focus => {
                    // Focus events can arrive after Enter has already moved
                    // focus back to the page. Do not reopen the suggestions.
                    if !input.read(cx).focus_handle(cx).is_focused(window) {
                        return;
                    }
                    browser.send("focus_controls", json!({}));
                    browser.editing = true;
                    browser.suggest(cx);
                    input.update(cx, |input, cx| input.set_selected_range(0..input.value().len(), cx));
                }
                InputEvent::Blur => {
                    if input.read(cx).focus_handle(cx).is_focused(window) {
                        return;
                    }
                    browser.editing = false;
                    browser.suggestions.clear();
                    browser.selected = None;
                    input.update(cx, |input, cx| input.set_value(if browser.current_url == "about:blank" { "" } else { &browser.current_url }, window, cx));
                }
                InputEvent::PressEnter { .. } => {
                    let value = browser.selected.and_then(|index| browser.suggestions.get(index).cloned()).unwrap_or_else(|| input.read(cx).value().to_string());
                    browser.navigate(&value, window, cx);
                }
            }
            cx.notify();
        });
        let mut browser = Self {
            id,
            native,
            address,
            history,
            suggestions: Vec::new(),
            selected: None,
            editing: false,
            focus,
            current_url: url,
            page,
            can_go_back: false,
            page_progress: 100,
            notification_height: px(0.0),
            error: None,
            opened: false,
            bounds: None,
            _input: subscription,
        };
        if let Err(error) = (browser.native)(&json!({ "command": "open", "id": id, "request": request }).to_string()) {
            receive(id, &json!({ "kind": "error", "message": error }).to_string());
            browser.error = Some(components::user_facing_error(&error, "Could not open the browser. Please try again.").to_owned());
        }
        browser
    }

    fn send(&mut self, command: &str, mut payload: Value) {
        payload["command"] = command.into();
        payload["id"] = self.id.into();
        if let Err(error) = (self.native)(&payload.to_string()) {
            self.error = Some(components::user_facing_error(&error, "Could not update the browser page. Please try again.").to_owned());
        }
    }

    fn suggest(&mut self, cx: &App) {
        self.suggestions = self.history.as_ref().map_or_else(Vec::new, |history| history.suggestions(&self.address.read(cx).value()));
        self.selected = None;
    }

    fn navigate(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        match normalize(value) {
            Ok(url) => {
                self.error = None;
                self.send("navigate", json!({ "url": url.as_str() }));
                if self.error.is_some() {
                    return;
                }
                if let Some(history) = &mut self.history {
                    if let Err(error) = history.remember(value) {
                        log::warn!("Could not save browser address history: {error}");
                    }
                }
                self.current_url = url.to_string();
                self.update_page();
                self.restore_address(window, cx);
                self.editing = false;
                self.suggestions.clear();
                self.selected = None;
                self.focus.focus(window, cx);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" | "up" if self.editing && !self.suggestions.is_empty() => {
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
            "back" | "escape" => {
                self.back(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn restore_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.address.update(cx, |input, cx| input.set_value(if self.current_url == "about:blank" { "" } else { &self.current_url }, window, cx));
    }

    fn update_page(&mut self) {
        if self.page.url != self.current_url {
            self.page = self.history.as_ref().and_then(|history| history.page(&self.current_url)).cloned().unwrap_or_else(|| PageMetadata::for_url(&self.current_url));
        }
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing {
            self.editing = false;
            self.suggestions.clear();
            self.selected = None;
            self.restore_address(window, cx);
            self.focus.focus(window, cx);
            self.send("focus_page", json!({}));
            cx.notify();
        } else if self.can_go_back {
            self.send("back", json!({}));
        } else {
            cx.emit(Closed);
        }
    }

    fn native_event(&mut self, event: NativeEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            NativeEvent::PageVisited { page } => {
                if page.url == self.current_url {
                    self.page = page.clone();
                }
                if let Some(history) = &mut self.history {
                    if let Err(error) = history.record_visit(page) {
                        log::warn!("Could not save browser page visit: {error}");
                    }
                }
            }
            NativeEvent::Opened => {
                self.opened = true;
                if self.current_url == "about:blank" {
                    self.editing = true;
                    self.suggest(cx);
                    self.address.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
            NativeEvent::Navigation { url, back } => {
                self.current_url = url.clone();
                self.update_page();
                self.can_go_back = back;
                if !self.address.read(cx).focus_handle(cx).is_focused(window) {
                    self.address.update(cx, |input, cx| input.set_value(if url == "about:blank" { "" } else { &url }, window, cx));
                }
            }
            NativeEvent::PageProgress { percent } => self.page_progress = percent,
            NativeEvent::Error { message } => {
                self.error = Some(components::user_facing_error(&message, "Could not open this page. Check the address and try again.").to_owned());
                if !self.opened {
                    cx.emit(Closed);
                }
            }
            NativeEvent::Focused => {
                self.editing = false;
                self.suggestions.clear();
                self.selected = None;
                self.restore_address(window, cx);
                // Restore the controls' keyboard target after leaving address editing.
                self.focus.focus(window, cx);
            }
            NativeEvent::BackPressed => self.back(window, cx),
            NativeEvent::RefreshRequested => {
                if self.opened && self.current_url != "about:blank" {
                    self.page_progress = 0;
                    self.send("reload", json!({}));
                }
            }
            NativeEvent::Closed => cx.emit(Closed),
            _ => {}
        }
        cx.notify();
    }

    fn layout(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        let bounds = [
            (f32::from(bounds.origin.x) * scale).round() as i32,
            (f32::from(bounds.origin.y) * scale).round() as i32,
            (f32::from(bounds.size.width) * scale).round().max(0.0) as i32,
            (f32::from(bounds.size.height) * scale).round().max(0.0) as i32,
        ];
        if self.bounds != Some(bounds) {
            self.bounds = Some(bounds);
            self.send("bounds", json!({ "x": bounds[0], "y": bounds[1], "width": bounds[2], "height": bounds[3] }));
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = (self.native)(&json!({ "command": "close", "id": self.id }).to_string());
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Reconcile with current focus as well as input events: focus may
        // change twice between frames without emitting a new Focus event.
        let focused = self.address.read(cx).focus_handle(cx).is_focused(window);
        if focused && !self.editing {
            self.editing = true;
            self.suggest(cx);
        } else if !focused && self.editing {
            self.editing = false;
            self.suggestions.clear();
            self.selected = None;
        }
        let theme = components::browser_theme(cx);
        let insets = window.insets();
        let browser = cx.entity().downgrade();
        let toolbar = div()
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(components::SPACE_SM))
            .p(px(components::SPACE_SM))
            .capture_any_mouse_down(cx.listener(|browser, _, _, _| browser.send("focus_controls", json!({}))))
            .child(components::browser_address_control(&self.address, self.editing, self.page.display_name().to_owned(), self.page.icon.as_deref(), theme).debug_selector(|| "android-browser-address".into()))
            .child(components::outlined_icon_button("browser-close", "Close browser", IconName::Close).debug_selector(|| "android-browser-close".into()).on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.page_bg)
            .text_color(theme.text)
            .text_size(rems(components::TEXT_SM))
            .pt(insets.safe_area.top)
            .pb(insets.safe_area.bottom.max(insets.ime.bottom))
            .pl(insets.safe_area.left)
            .pr(insets.safe_area.right)
            .track_focus(&self.focus)
            .key_context("BookBrowser")
            .capture_key_down(cx.listener(Self::key_down))
            .child(toolbar)
            .when(!self.suggestions.is_empty(), |view| {
                let available_height = (window.viewport_size().height - insets.safe_area.top - insets.safe_area.bottom.max(insets.ime.bottom)).max(px(0.0));
                let mut suggestions = div()
                    .id("android-browser-address-suggestions")
                    .debug_selector(|| "android-browser-address-suggestions".into())
                    .flex_none()
                    .mx(px(components::SPACE_SM))
                    .mb(px(components::SPACE_SM))
                    .max_h(available_height * 0.5)
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(theme.rule)
                    .bg(theme.page_bg);
                for (index, suggestion) in self.suggestions.iter().cloned().enumerate() {
                    let target = suggestion.clone();
                    suggestions = suggestions.child(
                        div()
                            .id(("android-address-suggestion", index))
                            .debug_selector(move || format!("android-address-suggestion-{index}"))
                            .flex_none()
                            .h(rems(components::topbar_action_height_rem()))
                            .px(px(components::SPACE_SM))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .bg(if self.selected == Some(index) { theme.accent_wash } else { theme.page_bg })
                            .hover(|style| style.bg(theme.hover))
                            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                            .on_click(cx.listener(move |browser, _, window, cx| browser.navigate(&target, window, cx)))
                            .child(div().flex_1().min_w_0().text_ellipsis().child(suggestion)),
                    );
                }
                view.child(suggestions)
            })
            .child(div().h(px(3.0)).w_full().when(self.page_progress < 100, |row| row.child(Progress::new("browser-page-progress").value(f32::from(self.page_progress)))))
            .child(
                div().flex_1().min_h_0().w_full().debug_selector(|| "android-browser-page".into()).child(
                    canvas(
                        move |bounds, window, cx| {
                            let scale = window.scale_factor();
                            cx.defer(move |cx| {
                                let _ = browser.update(cx, |browser, _| browser.layout(bounds, scale));
                            });
                        },
                        |_, _, _, _| {},
                    )
                    .size_full(),
                ),
            )
            .child(div().flex_none().h(self.notification_height))
            .when_some(self.error.clone(), |view, error| view.child(div().w_full().flex_none().px(px(components::SPACE_SM)).py(px(components::SPACE_XS)).child(error)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, point, size};
    use gpui_component::WindowExt;
    use std::time::Duration;

    struct Library;
    impl Render for Library {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().debug_selector(|| "browser-test-library".into()).child("Library")
        }
    }

    #[test]
    fn addresses_and_searches_keep_unsafe_schemes_out_of_the_page() {
        assert_eq!(normalize(" example.com/books ").unwrap().as_str(), "https://example.com/books");
        assert_eq!(normalize("C++ books").unwrap().as_str(), "https://www.google.com/search?q=C%2B%2B+books");
        for value in ["", "javascript:alert(1)", "file:///tmp/book.epub", "intent://book", "https://user:password@example.com"] {
            assert!(normalize(value).is_err());
        }
    }

    #[gpui::test]
    fn inactive_address_shows_site_identity_and_focus_restores_the_full_url(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
        });
        let url = "https://example.com/books?q=rust#chapter";
        let (root, cx) = cx.add_window_view(|window, cx| {
            let browser = cx.new(|cx| Browser::new(30, json!({"url":url}), Arc::new(|_| Ok(())), window, cx));
            Root::new(browser, window, cx)
        });
        let browser = cx.update(|_, cx| root.read(cx).view().clone().downcast::<Browser>().ok().unwrap());
        cx.simulate_resize(size(px(360.0), px(800.0)));
        browser
            .update_in(cx, |browser, window, cx| browser.native_event(NativeEvent::PageVisited { page: PageMetadata { url: url.into(), title: "A particular book".into(), site_name: Some("Example Books".into()), icon: None } }, window, cx));
        cx.run_until_parked();
        assert_eq!(browser.read_with(cx, |browser, _| browser.page.display_name().to_owned()), "Example Books");
        let identity = cx.debug_bounds("browser-site-identity").unwrap();
        let content = cx.debug_bounds("browser-site-identity-content").unwrap();
        assert!((content.center().x - identity.center().x).abs() < px(1.0));
        cx.simulate_click(identity.center(), Default::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("browser-site-identity").is_none());
        let address = browser.read_with(cx, |browser, _| browser.address.clone());
        assert_eq!(address.read_with(cx, |address, _| address.value()), url);
        assert_eq!(address.read_with(cx, |address, _| address.selected_value()), url);
        cx.simulate_input("an unsubmitted edit");
        browser.update_in(cx, |browser, window, cx| browser.native_event(NativeEvent::Focused, window, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("browser-site-identity").is_some());
        assert_eq!(address.read_with(cx, |address, _| address.value()), url);
        browser.update_in(cx, |browser, _, cx| {
            browser.page.site_name = Some("An unusually long site name ".repeat(20));
            cx.notify();
        });
        cx.run_until_parked();
        let name = cx.debug_bounds("browser-site-name").unwrap();
        let content = cx.debug_bounds("browser-site-identity-content").unwrap();
        assert!(name.left() >= identity.left() && name.right() <= identity.right());
        assert!((content.center().x - identity.center().x).abs() < px(1.0));
    }

    #[gpui::test]
    fn browser_toasts_dismiss_without_hover_and_clear_with_the_browser(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
        });
        let (messages, receiver) = async_channel::unbounded();
        let commands = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = commands.clone();
        let native: Arc<Transport> = Arc::new(move |command| {
            captured.lock().unwrap().push(from_str(command).unwrap());
            Ok(())
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let content = cx.new(|_| Library);
            let browser = cx.new(|cx| BrowserRoot::new(content, receiver, native, window, cx));
            Root::new(browser, window, cx)
        });
        let root = cx.update(|_, cx| root.read(cx).view().clone().downcast::<BrowserRoot<Library>>().ok().unwrap());
        cx.simulate_resize(size(px(360.0), px(800.0)));
        let send = |message: Message, cx: &mut VisualTestContext| {
            messages.try_send(message).unwrap();
            cx.run_until_parked();
        };
        let open = |id, cx: &mut VisualTestContext| send(Message::Command(Command::Open { id, request: json!({"url":"about:blank"}) }), cx);
        let fail = |id, cx: &mut VisualTestContext| {
            send(
                Message::Command(Command::Feedback {
                    id,
                    feedback: BrowserDownloadUpdate { download: "failed".into(), replaces: None, name: "Broken book".into(), phase: BrowserDownloadPhase::Failed(None), percent: None, message: None },
                }),
                cx,
            );
        };
        let count = |cx: &mut VisualTestContext| cx.update(|window, cx| Root::read(window, cx).notification.read(cx).notifications().len());
        cx.update(|window, cx| window.push_notification(Notification::info("Application notice").autohide(false), cx));
        open(7, cx);
        fail(7, cx);
        assert_eq!(count(cx), 2);
        cx.simulate_mouse_move(point(px(1.0), px(1.0)), None, Default::default());
        cx.run_until_parked();
        let close = cx.debug_bounds("notification-close").unwrap();
        cx.simulate_click(close.center(), Default::default());
        cx.run_until_parked();
        cx.background_executor.advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert_eq!(count(cx), 1);

        assert!(!commands.lock().unwrap().iter().any(|command| command["command"] == "cancel_download"));
        send(Message::Native { id: 7, event: NativeEvent::DownloadStarted { download: "cancelled".into(), name: "Cancel this book".into() } }, cx);
        let close = cx.debug_bounds("notification-close").unwrap();
        cx.simulate_click(close.center(), Default::default());
        cx.run_until_parked();
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "cancel_download" && command["download"] == "cancelled" && command["id"] == 7));
        send(Message::Native { id: 7, event: NativeEvent::CancelDownload { download: "cancelled".into() } }, cx);
        cx.background_executor.advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        send(
            Message::Command(Command::Feedback {
                id: 7,
                feedback: BrowserDownloadUpdate { download: "cancelled".into(), replaces: None, name: "Cancel this book".into(), phase: BrowserDownloadPhase::Completed, percent: None, message: None },
            }),
            cx,
        );
        assert_eq!(count(cx), 1, "late updates must not restore a cancelled toast");

        root.update(cx, |root, _| root.cancel_download("7:cancelled"));
        assert_eq!(commands.lock().unwrap().iter().filter(|command| command["command"] == "cancel_download").count(), 1, "duplicate close actions must cancel only once");
        send(
            Message::Command(Command::Feedback {
                id: 7,
                feedback: BrowserDownloadUpdate { download: "torrent:cancelled".into(), replaces: Some("cancelled".into()), name: "Cancelled torrent".into(), phase: BrowserDownloadPhase::Downloading, percent: Some(10), message: None },
            }),
            cx,
        );
        send(
            Message::Command(Command::Feedback {
                id: 7,
                feedback: BrowserDownloadUpdate { download: "torrent:cancelled".into(), replaces: None, name: "Cancelled torrent".into(), phase: BrowserDownloadPhase::Completed, percent: None, message: None },
            }),
            cx,
        );
        assert_eq!(count(cx), 1, "renaming a cancelled transfer must retain its cancellation");

        // Clear both terminal/status toasts and active download tracking.
        fail(7, cx);
        send(Message::Native { id: 7, event: NativeEvent::Status { message: "Browser status".into() } }, cx);
        send(Message::Native { id: 7, event: NativeEvent::DownloadStarted { download: "active".into(), name: "Active book".into() } }, cx);
        assert_eq!(count(cx), 4);
        let close = cx.debug_bounds("android-browser-close").unwrap();
        cx.simulate_click(close.center(), Default::default());
        cx.run_until_parked();
        assert!(root.read_with(cx, |root, _| root.browser.is_none() && root.downloads.is_empty()));
        assert_eq!(count(cx), 1);
        fail(7, cx);
        send(Message::Native { id: 7, event: NativeEvent::DownloadFinished { download: "active".into(), name: "Active book".into(), error: Some("HTTP 500".into()), torrent: false } }, cx);
        assert_eq!(count(cx), 1);
        assert_eq!(commands.lock().unwrap().iter().filter(|command| command["command"] == "cancel_download").count(), 1, "closing the browser must not cancel active transfers");

        open(8, cx);
        fail(8, cx);
        assert_eq!(count(cx), 2);
        open(9, cx);
        assert_eq!(count(cx), 1);
        fail(8, cx);
        assert_eq!(count(cx), 1);
        fail(9, cx);
        assert_eq!(count(cx), 2);
        send(Message::Command(Command::Close { id: 9 }), cx);
        assert!(root.read_with(cx, |root, _| root.browser.is_none()));
        assert_eq!(count(cx), 1);

        open(10, cx);
        fail(10, cx);
        assert_eq!(count(cx), 2);
        send(Message::Command(Command::Close { id: 9 }), cx);
        assert!(root.read_with(cx, |root, _| root.browser.is_some()));
        send(Message::Native { id: 10, event: NativeEvent::Closed }, cx);
        assert!(root.read_with(cx, |root, _| root.browser.is_none()));
        assert_eq!(count(cx), 1);
    }

    #[gpui::test]
    fn page_focus_keeps_hardware_back_in_the_browser(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
        });
        let commands = Arc::new(Mutex::new(Vec::<Value>::new()));
        let recorded = commands.clone();
        let native: Arc<Transport> = Arc::new(move |message| {
            recorded.lock().unwrap().push(from_str(message).unwrap());
            Ok(())
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let browser = cx.new(|cx| Browser::new(31, json!({"url":"https://example.com/second"}), native, window, cx));
            Root::new(browser, window, cx)
        });
        let browser = cx.update(|_, cx| root.read(cx).view().clone().downcast::<Browser>().ok().unwrap());
        browser.update_in(cx, |browser, window, cx| {
            browser.native_event(NativeEvent::Navigation { url: "https://example.com/second".into(), back: true }, window, cx);
            browser.native_event(NativeEvent::Focused, window, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(browser.read(cx).focus.is_focused(window));
            assert!(!browser.read(cx).address.read(cx).focus_handle(cx).is_focused(window));
        });
        cx.simulate_keystrokes("back");
        cx.run_until_parked();
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "back"));
    }

    #[gpui::test]
    fn gpui_controls_drive_the_page_and_back_restores_the_library(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
        });
        let commands = Arc::new(Mutex::new(Vec::<Value>::new()));
        let recorded = commands.clone();
        let feedback = Arc::new(Mutex::new(Vec::<(u64, Value)>::new()));
        let reported = feedback.clone();
        initialize(
            move |message| {
                let command: Value = from_str(message).unwrap();
                recorded.lock().unwrap().push(command.clone());
                if command["command"] == "open" {
                    receive(command["id"].as_u64().unwrap(), r#"{"kind":"opened"}"#);
                }
                Ok(())
            },
            move |id, message| reported.lock().unwrap().push((id, from_str(message).unwrap())),
        );
        let (root, cx) = cx.add_window_view(|window, cx| {
            let content = cx.new(|_| Library);
            let browser = wrap(content.clone(), window, cx).unwrap();
            assert!(wrap(content, window, cx).is_err(), "a second host must not steal the original host's event receiver");
            Root::new(browser, window, cx)
        });
        let root = cx.update(|_, cx| root.read(cx).view().clone().downcast::<BrowserRoot<Library>>().ok().unwrap());
        cx.simulate_resize(size(px(360.0), px(800.0)));
        let directory = tempfile::tempdir().unwrap();
        let history_file = directory.path().join("address-history.json");
        let mut history = AddressHistory::load(history_file.clone());
        history.remember("gutenberg.org").unwrap();
        history.remember("example.com/books").unwrap();
        history.record_visit(PageMetadata { url: "https://gutenberg.org/".into(), title: "Project Gutenberg".into(), icon: None, site_name: None }).unwrap();
        history.record_visit(PageMetadata { url: "https://example.com/books".into(), title: "Example books".into(), icon: None, site_name: None }).unwrap();
        let request = json!({"command":"open","id":7,"request":{"url":"about:blank","title":"Books","history_file":history_file}});
        command(&request.to_string()).unwrap();
        cx.run_until_parked();
        assert!(feedback.lock().unwrap().iter().any(|(id, event)| *id == 7 && event["kind"] == "opened"));
        {
            let commands = commands.lock().unwrap();
            let opened = commands.iter().find(|command| command["command"] == "open").unwrap();
            let html = opened["request"]["start_page_html"].as_str().unwrap();
            assert!(html.contains("Example books</span>"));
            assert!(!html.contains("gutenberg.org"));
            assert_eq!(opened["request"]["page_metadata_script"], PAGE_METADATA_SCRIPT);
        }
        receive(7, r#"{"kind":"page_visited","url":"https://gutenberg.org/","title":"Project Gutenberg","icon":null}"#);
        cx.run_until_parked();
        assert_eq!(AddressHistory::load(history_file.clone()).most_visited()[0].visits, 2);
        let suggestions = cx.debug_bounds("android-browser-address-suggestions").unwrap();
        assert!(suggestions.top() >= cx.debug_bounds("android-browser-address").unwrap().bottom());
        assert!(suggestions.bottom() <= cx.debug_bounds("android-browser-page").unwrap().top());
        cx.simulate_input("exam");
        assert_eq!(root.read_with(cx, |root, cx| root.browser.as_ref().unwrap().read(cx).suggestions.clone()), ["https://example.com/books"]);
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "navigate" && command["url"] == "https://example.com/books"));
        assert!(root.read_with(cx, |root, cx| root.browser.as_ref().unwrap().read(cx).suggestions.is_empty()));
        assert!(cx.debug_bounds("android-browser-address-suggestions").is_none());
        cx.update(|window, cx| {
            let browser = root.read(cx).browser.clone().unwrap();
            let input = browser.read(cx).address.clone();
            input.update(cx, |input, cx| input.set_value("guten", window, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
        });
        cx.run_until_parked();
        let suggestion = cx.debug_bounds("android-address-suggestion-0").unwrap();
        cx.simulate_click(suggestion.center(), Default::default());
        cx.run_until_parked();
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "navigate" && command["url"] == "https://gutenberg.org/"));
        assert!(cx.debug_bounds("android-browser-address-suggestions").is_none());
        assert_eq!(AddressHistory::load(history_file).addresses, ["https://gutenberg.org/", "https://example.com/books"]);
        cx.update(|window, cx| {
            let browser = root.read(cx).browser.clone().unwrap();
            let input = browser.read(cx).address.clone();
            input.update(cx, |input, cx| input.set_value("", window, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("android-browser-address-suggestions").is_some());
        receive(7, r#"{"kind":"back_pressed"}"#);
        cx.run_until_parked();
        assert!(root.read_with(cx, |root, _| root.browser.is_some()));
        assert!(cx.debug_bounds("android-browser-address-suggestions").is_none());
        let address = cx.debug_bounds("android-browser-address").unwrap();
        let page = cx.debug_bounds("android-browser-page").unwrap();
        assert!(page.top() >= address.bottom());
        assert!(page.size.height > px(0.0));
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "bounds" && command["height"].as_i64().unwrap() > 0));
        receive(7, r#"{"kind":"download_started","download":"one","name":"one.epub"}"#);
        receive(7, r#"{"kind":"download_started","download":"two","name":"two.pdf"}"#);
        receive(99, r#"{"kind":"download_progress","download":"one","percent":90}"#);
        receive(7, r#"{"kind":"download_progress","download":"one","percent":35}"#);
        cx.run_until_parked();
        assert_eq!(root.read_with(cx, |root, cx| root.downloads.get("7:one").unwrap().progress.read(cx).percent), Some(35));
        assert_eq!(root.read_with(cx, |root, cx| root.downloads.get("7:two").unwrap().progress.read(cx).percent), None);
        assert_eq!(cx.update(|window, cx| Root::read(window, cx).notification.read(cx).notifications().len()), 2);
        let toasts = cx.debug_bounds("android-browser-download-toasts").unwrap();
        assert!(toasts.left() >= px(0.0));
        assert!(toasts.size.width <= px(360.0));
        let smaller_page = cx.debug_bounds("android-browser-page").unwrap();
        assert!(smaller_page.size.height < page.size.height);
        cx.update(|_, cx| assert!(smaller_page.bottom() <= toasts.top() + cx.theme().notification.margins.top));
        receive(7, r#"{"kind":"download_waiting","download":"one"}"#);
        cx.run_until_parked();
        assert!(root.read_with(cx, |root, _| root.downloads.get("7:one").unwrap().phase == BrowserDownloadPhase::Waiting));
        receive(7, r#"{"kind":"download_progress","download":"one","percent":null}"#);
        cx.run_until_parked();
        assert!(!root.read_with(cx, |root, _| root.downloads.get("7:one").unwrap().phase == BrowserDownloadPhase::Waiting));
        assert_eq!(cx.update(|window, cx| Root::read(window, cx).notification.read(cx).notifications().len()), 2);
        // A .torrent HTTP download keeps its entry and progress entity when
        // the library takes over the payload transfer.
        let metadata_bar = root.read_with(cx, |root, _| root.downloads.get("7:two").unwrap().progress.entity_id());
        receive(7, r#"{"kind":"download_finished","download":"two","name":"two.torrent","torrent":true}"#);
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"two","name":"two.torrent","phase":"preparing"}}"#).unwrap();
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"torrent:metadata","replaces":"two","name":"Bundle","phase":"downloading","percent":12}}"#).unwrap();
        cx.run_until_parked();
        cx.background_executor.advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert_eq!(root.read_with(cx, |root, _| root.downloads.get("7:torrent:metadata").unwrap().progress.entity_id()), metadata_bar);
        assert_eq!(root.read_with(cx, |root, cx| root.downloads.get("7:torrent:metadata").unwrap().progress.read(cx).percent), Some(12));
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"torrent:first","name":"First torrent","phase":"downloading","percent":43}}"#).unwrap();
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"torrent:second","name":"Second torrent","phase":"preparing"}}"#).unwrap();
        command(r#"{"command":"feedback","id":99,"feedback":{"download":"torrent:stale","name":"Another session","phase":"downloading","percent":90}}"#).unwrap();
        cx.run_until_parked();
        assert_eq!(root.read_with(cx, |root, cx| root.downloads.get("7:torrent:first").unwrap().progress.read(cx).percent), Some(43));
        assert_eq!(root.read_with(cx, |root, cx| root.downloads.get("7:torrent:second").unwrap().progress.read(cx).percent), None);
        assert!(root.read_with(cx, |root, _| root.downloads.get("99:torrent:stale").is_none()));
        assert_eq!(cx.update(|window, cx| Root::read(window, cx).notification.read(cx).notifications().len()), 4);
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"torrent:first","name":"First torrent","phase":"downloading","percent":58}}"#).unwrap();
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"torrent:second","name":"Second torrent","phase":"waiting","message":"Waiting for an unmetered connection"}}"#).unwrap();
        cx.run_until_parked();
        assert_eq!(root.read_with(cx, |root, cx| root.downloads.get("7:torrent:first").unwrap().progress.read(cx).percent), Some(58));
        assert_eq!(root.read_with(cx, |root, cx| root.downloads.get("7:torrent:metadata").unwrap().progress.read(cx).percent), Some(12));
        assert_eq!(cx.update(|window, cx| Root::read(window, cx).notification.read(cx).notifications().len()), 4);
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"torrent:first","name":"First torrent","phase":"completed","message":"Added to library; seeding continues"}}"#).unwrap();
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"torrent:second","name":"Second torrent","phase":"failed","message":"Tracker unavailable"}}"#).unwrap();
        cx.run_until_parked();
        assert!(root.read_with(cx, |root, _| root.downloads.get("7:torrent:first").is_none() && root.downloads.get("7:torrent:second").is_none()));
        cx.update(|window, cx| {
            let notifications = Root::read(window, cx).notification.clone();
            notifications.update(cx, |list, cx| list.clear(window, cx));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("android-browser-download-toasts").is_none());
        assert_eq!(cx.debug_bounds("android-browser-page").unwrap().size.height, page.size.height);
        cx.update(|window, cx| {
            let browser = root.read(cx).browser.clone().unwrap();
            let input = browser.read(cx).address.clone();
            input.update(cx, |input, cx| input.set_value("example.com/books", window, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "navigate" && command["url"] == "https://example.com/books"));
        receive(7, r#"{"kind":"navigation","url":"https://example.com/books","back":true,"forward":false}"#);
        cx.run_until_parked();
        assert!(cx.debug_bounds("android-browser-back").is_none());
        receive(7, r#"{"kind":"refresh_requested"}"#);
        receive(7, r#"{"kind":"back_pressed"}"#);
        cx.run_until_parked();
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "reload" && command["id"] == 7));
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "back"));
        receive(99, r#"{"kind":"closed"}"#);
        cx.run_until_parked();
        assert!(root.read_with(cx, |root, _| root.browser.is_some()));
        receive(7, r#"{"kind":"navigation","url":"about:blank","back":false,"forward":false}"#);
        receive(7, r#"{"kind":"back_pressed"}"#);
        cx.run_until_parked();
        assert!(cx.debug_bounds("browser-test-library").is_some());
        assert!(cx.debug_bounds("android-browser-page").is_none());
        assert!(commands.lock().unwrap().iter().any(|command| command["command"] == "close" && command["id"] == 7));
        assert!(root.read_with(cx, |root, _| root.downloads.is_empty()));
        assert_eq!(cx.update(|window, cx| Root::read(window, cx).notification.read(cx).notifications().len()), 0);
        // Transfers can finish after closure without bringing their toasts
        // into the library or another browser session.
        receive(7, r#"{"kind":"download_finished","download":"one","name":"one.epub"}"#);
        cx.run_until_parked();
        assert!(root.read_with(cx, |root, _| root.downloads.is_empty()));
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"one","name":"one.epub","phase":"completed","message":"Added to library"}}"#).unwrap();
        receive(7, r#"{"kind":"download_finished","download":"two","name":"two.pdf","error":"HTTP 500"}"#);
        command(r#"{"command":"feedback","id":7,"feedback":{"download":"torrent:metadata","name":"Bundle","phase":"failed","message":"HTTP 500"}}"#).unwrap();
        cx.run_until_parked();
        assert!(root.read_with(cx, |root, _| root.downloads.is_empty()));
        assert_eq!(cx.update(|window, cx| Root::read(window, cx).notification.read(cx).notifications().len()), 0);
        cx.background_executor.advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        cx.background_executor.advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        assert_eq!(cx.update(|window, cx| Root::read(window, cx).notification.read(cx).notifications().len()), 0);
    }
}
