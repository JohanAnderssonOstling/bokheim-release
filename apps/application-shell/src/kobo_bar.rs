//! Kobo device chrome sits above the browser, independently of browser routes.
mod battery;
use app::AppClient;
use app::{BluetoothRequest, BluetoothStatus, WifiNetwork, WifiPhase, WifiRequest, WifiSecurity, WifiSnapshot};
use gpui::prelude::*;
use gpui::{Context, Entity, Render, Window, px};
use gpui_component::{
    Disableable, Icon,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    popover::Popover,
    scroll::ScrollableElement,
    switch::Switch,
};
use std::time::Duration;
use ui_components as components;

// `.top()` is relative to the trigger's top edge. The shared Popover adds a
// 4px top inset for top anchors, so subtract that inset when placing the
// intrinsic panel at the bottom edge of the top bar. The bar and its buttons
// are rems, so the offset is worked out at the window's rem size.
fn popup_top(rem_size: gpui::Pixels) -> gpui::Pixels {
    rem_size * (components::TOPBAR_HEIGHT_REM - (components::TOPBAR_HEIGHT_REM - components::TOPBAR_ACTION_HEIGHT_REM) / 2.0) - px(2.0 * components::SPACE_XS)
}

fn device_button(id: &'static str, label: impl Into<gpui::SharedString>, icon: impl Into<Icon>) -> Button {
    components::base_button(id).icon(icon).tooltip(label).ghost().border_0().size(gpui::rems(components::TOPBAR_ACTION_HEIGHT_REM)).px_0()
}

pub(super) struct KoboDeviceBar {
    panel: Entity<WifiPanel>,
    bluetooth: Entity<BluetoothPanel>,
    brightness: Entity<reader_ui::BrightnessControls>,
    battery: Entity<battery::BatteryPanel>,
    services: browser_ui::AppServices,
    auto_rotate: bool,
    power_menu_open: bool,
    power_error: Option<String>,
}
impl KoboDeviceBar {
    pub(super) fn new(backend: AppClient, services: browser_ui::AppServices, auto_rotate: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let brightness = cx.new(|cx| reader_ui::BrightnessControls::new(backend.clone(), window, cx));
        let battery = cx.new(|cx| battery::BatteryPanel::new(backend.clone(), cx));
        cx.observe(&battery, |_, _, cx| cx.notify()).detach();
        let panel = cx.new(|cx| WifiPanel::new(backend.clone(), window, cx));
        cx.observe(&panel, |_, _, cx| cx.notify()).detach();
        let bluetooth = cx.new(|cx| BluetoothPanel::new(backend.clone(), cx));
        cx.observe(&bluetooth, |_, _, cx| cx.notify()).detach();
        cx.spawn(async move |bar, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                if bar
                    .update(cx, |bar, cx| {
                        if gpui_kobo::take_power_menu_request() {
                            bar.power_menu_open = !bar.power_menu_open;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self { panel, bluetooth, brightness, battery, services, auto_rotate, power_menu_open: false, power_error: None }
    }

    fn exit_kobo(&mut self, action: &str, cx: &mut Context<Self>) {
        let result = std::env::var_os("BOKHEIM_KOBO_EXIT_ACTION_FILE").ok_or_else(|| "Kobo launcher must be updated before using this action".to_owned()).and_then(|path| std::fs::write(path, action).map_err(|error| error.to_string()));
        match result {
            Ok(()) => cx.quit(),
            Err(error) => {
                self.power_error = Some(error);
                cx.notify();
            }
        }
    }
}
impl Render for KoboDeviceBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let popup_top = popup_top(window.rem_size());
        let state = self.panel.read(cx);
        let label = state.label();
        let icon = state.icon();
        let panel = self.panel.clone();
        let rotate_services = self.services.clone();
        let rotate_icon = if self.auto_rotate { "icons/lock-open.svg" } else { "icons/lock.svg" };
        let rotate_label = if self.auto_rotate { "Auto-rotate on" } else { "Auto-rotate off" };
        let bluetooth = self.bluetooth.clone();
        let bluetooth_state = bluetooth.read(cx).status.clone();
        let bluetooth_icon = if bluetooth_state.powered { "icons/bluetooth.svg" } else { "icons/bluetooth-off.svg" };
        let bluetooth_label = if bluetooth_state.available { if bluetooth_state.powered { "Bluetooth on" } else { "Bluetooth off" } } else { "Bluetooth unavailable" };
        gpui::div()
            .id("kobo-device-topbar")
            .h(gpui::rems(components::TOPBAR_HEIGHT_REM))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .px(px(components::CONTENT_INSET))
            .gap(px(components::TOPBAR_ACTION_GAP))
            .bg(theme.page_bg)
            .child(
                Popover::new("kobo-power-menu")
                    .anchor(gpui::Anchor::TopLeft)
                    .top(popup_top)
                    .open(self.power_menu_open)
                    .on_open_change(cx.listener(|bar, open, _, cx| {
                        bar.power_menu_open = *open;
                        cx.notify();
                    }))
                    .trigger(device_button("kobo-power-button", "Power", Icon::empty().path("icons/power.svg")))
                    .child(
                        gpui::div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .w(px(220.0))
                            .child(components::base_button("kobo-sleep").label("Sleep").ghost().on_click(cx.listener(|bar, _, _, cx| {
                                bar.power_menu_open = false;
                                cx.notify();
                                gpui_kobo::request_sleep();
                            })))
                            .child(components::base_button("kobo-return").label("Return to Kobo").ghost().on_click(|_, _, cx| cx.quit()))
                            .child(components::base_button("kobo-poweroff").label("Power off").ghost().on_click(cx.listener(|bar, _, _, cx| bar.exit_kobo("poweroff", cx))))
                            .child(components::base_button("kobo-restart").label("Restart").ghost().on_click(cx.listener(|bar, _, _, cx| bar.exit_kobo("reboot", cx))))
                            .children(self.power_error.clone()),
                    ),
            )
            .child(gpui::div().flex_1())
            .child(reader_ui::brightness_popover(self.brightness.clone(), None, theme).top(popup_top).trigger(device_button("kobo-brightness-button", "Brightness", gpui_component::IconName::Sun)))
            .child(device_button("kobo-auto-rotate-button", rotate_label, Icon::empty().path(rotate_icon)).on_click(cx.listener(move |bar, _, _, cx| {
                bar.auto_rotate = !bar.auto_rotate;
                rotate_services.save_kobo_auto_rotate(bar.auto_rotate, cx);
                cx.notify();
            })))
            .child(Popover::new("kobo-bluetooth-popup").anchor(gpui::Anchor::TopRight).top(popup_top).p_0().trigger(device_button("kobo-bluetooth-button", bluetooth_label, Icon::empty().path(bluetooth_icon))).child(bluetooth))
            .child(battery::popover(&self.battery, popup_top, cx))
            .child(
                Popover::new("kobo-wifi-popup")
                    .anchor(gpui::Anchor::TopRight)
                    // Offset from the centered square trigger to the bar's bottom.
                    .top(popup_top)
                    .p_0()
                    .trigger(device_button("kobo-wifi-button", label, Icon::empty().path(icon)))
                    .on_open_change(move |open, window, cx| {
                        panel.update(cx, |panel, cx| {
                            panel.open = *open;
                            if *open {
                                panel.run(WifiRequest::Status, cx);
                            } else {
                                panel.selected = None;
                                panel.password.update(cx, |input, cx| input.set_value("", window, cx));
                            }
                        });
                    })
                    .child(self.panel.clone()),
            )
    }
}

struct BluetoothPanel {
    backend: AppClient,
    status: BluetoothStatus,
    busy: bool,
    error: Option<String>,
}

impl BluetoothPanel {
    fn new(backend: AppClient, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |panel, cx| {
            loop {
                let Ok(()) = panel.update(cx, |panel, cx| panel.run(BluetoothRequest::Status, cx)) else { break };
                cx.background_executor().timer(Duration::from_secs(15)).await;
            }
        })
        .detach();
        Self { backend, status: BluetoothStatus::default(), busy: false, error: None }
    }

    fn run(&mut self, request: BluetoothRequest, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let backend = self.backend.clone();
        cx.spawn(async move |panel, cx| {
            let result = backend.kobo_bluetooth(request).await;
            let _ = panel.update(cx, |panel, cx| {
                panel.busy = false;
                match result {
                    Ok(status) => {
                        panel.status = status;
                        panel.error = None;
                    }
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for BluetoothPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let enabled = self.status.powered;
        let busy = self.busy;
        let mut panel = gpui::div()
            .w(px(300.0))
            .max_w(window.viewport_size().width - px(24.0))
            .max_h(window.viewport_size().height - px(72.0))
            .overflow_y_scrollbar()
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .bg(theme.page_bg)
            .text_color(theme.text)
            .child(
                gpui::div().flex().items_center().justify_between().child("Bluetooth").child(
                    Switch::new("bluetooth-power")
                        .label(if enabled { "On" } else { "Off" })
                        .checked(enabled)
                        .disabled(busy || !self.status.available)
                        .on_click(cx.listener(|panel, enabled, _, cx| panel.run(BluetoothRequest::Power(*enabled), cx))),
                ),
            );
        if !self.status.available {
            panel = panel.child(gpui::div().text_color(theme.text_muted).child("Bluetooth unavailable"));
        } else {
            panel = panel.child(
                gpui::div().flex().items_center().justify_between().child("Available devices").child(
                    components::base_button("bluetooth-refresh")
                        .icon(Icon::empty().path("icons/redo-2.svg"))
                        .ghost()
                        .tooltip(if busy { "Searching…" } else { "Search for devices" })
                        .disabled(busy || !enabled)
                        .on_click(cx.listener(|panel, _, _, cx| panel.run(BluetoothRequest::Scan, cx))),
                ),
            );
            if self.status.devices.is_empty() {
                panel = panel.child(gpui::div().text_color(theme.text_muted).child(if enabled { "No devices found. Press refresh to search." } else { "Turn Bluetooth on to search." }));
            } else {
                for (index, device) in self.status.devices.iter().enumerate() {
                    let address = device.address.clone();
                    let label = if device.connected { format!("{} · Connected", device.name) } else { device.name.clone() };
                    panel = panel.child(
                        components::base_button(format!("bluetooth-device-{index}"))
                            .ghost()
                            .w_full()
                            .justify_start()
                            .label(label)
                            .disabled(busy)
                            .on_click(cx.listener(move |panel, _, _, cx| panel.run(BluetoothRequest::Connect(address.clone()), cx))),
                    );
                }
            }
        }
        if let Some(error) = self.error.clone() {
            panel = panel.child(gpui::div().text_color(theme.text_muted).child(error));
        }
        panel
    }
}

struct WifiPanel {
    backend: AppClient,
    snapshot: WifiSnapshot,
    busy: bool,
    open: bool,
    error: Option<String>,
    selected: Option<WifiNetwork>,
    password: Entity<InputState>,
}
impl WifiPanel {
    fn new(backend: AppClient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("Network password").masked(true));
        cx.spawn(async move |panel, cx| {
            loop {
                let Ok(delay) = panel.update(cx, |panel, cx| {
                    panel.run(WifiRequest::Status, cx);
                    if panel.open || matches!(panel.snapshot.phase, WifiPhase::Connecting | WifiPhase::ObtainingAddress) { 2 } else { 30 }
                }) else {
                    break;
                };
                cx.background_executor().timer(Duration::from_secs(delay)).await;
            }
        })
        .detach();
        Self { backend, snapshot: WifiSnapshot::default(), busy: false, open: false, error: None, selected: None, password }
    }
    fn label(&self) -> &'static str {
        if self.snapshot.phase == WifiPhase::Off && self.error.is_some() && !self.snapshot.power_available {
            return "Wi-Fi unavailable";
        }
        match self.snapshot.phase {
            WifiPhase::Off => "Wi-Fi off",
            WifiPhase::Disconnected => "Wi-Fi",
            WifiPhase::Connecting => "Connecting…",
            WifiPhase::ObtainingAddress => "Getting address…",
            WifiPhase::Connected => "Wi-Fi connected",
        }
    }
    fn icon(&self) -> &'static str {
        match self.snapshot.phase {
            WifiPhase::Off => "icons/wifi-off.svg",
            WifiPhase::Connecting | WifiPhase::ObtainingAddress => "icons/loader-circle.svg",
            WifiPhase::Disconnected => "icons/wifi-off.svg",
            WifiPhase::Connected => "icons/wifi.svg",
        }
    }
    fn run(&mut self, request: WifiRequest, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let is_status = matches!(request, WifiRequest::Status);
        self.busy = true;
        if !is_status {
            self.error = None;
        }
        let backend = self.backend.clone();
        cx.spawn(async move |panel, cx| {
            let result = backend.kobo_wifi(request).await;
            let _ = panel.update(cx, |panel, cx| {
                panel.busy = false;
                let previous = (panel.snapshot.clone(), panel.error.clone());
                match result {
                    Ok(snapshot) => {
                        panel.error = snapshot.error.clone();
                        panel.snapshot = snapshot;
                    }
                    Err(error) => panel.error = Some(error),
                }
                if !is_status || previous != (panel.snapshot.clone(), panel.error.clone()) {
                    cx.notify();
                }
            });
        })
        .detach();
        if !is_status {
            cx.notify();
        }
    }
}
impl Render for WifiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let off = self.snapshot.phase == WifiPhase::Off;
        let busy = self.busy;
        let connecting = matches!(self.snapshot.phase, WifiPhase::Connecting | WifiPhase::ObtainingAddress);
        let mut panel = gpui::div()
            .w(px(340.0))
            .max_w(window.viewport_size().width - px(24.0))
            .max_h(window.viewport_size().height - px(72.0))
            .overflow_y_scrollbar()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(12.0))
            .bg(theme.page_bg)
            .text_color(theme.text)
            .child(
                gpui::div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(gpui::div().flex_1().child("Wi-Fi"))
                    .child(
                        components::base_button("wifi-refresh")
                            .icon(Icon::empty().path("icons/redo-2.svg"))
                            .ghost()
                            .tooltip(if self.snapshot.scanning { "Scanning…" } else { "Refresh networks" })
                            .disabled(busy || off || self.snapshot.scanning || connecting)
                            .on_click(cx.listener(|panel, _, _, cx| panel.run(WifiRequest::Scan, cx))),
                    )
                    .child(Switch::new("wifi-power").label(if !off { "On" } else { "Off" }).checked(!off).disabled(busy || !self.snapshot.power_available).on_click(cx.listener(|panel, enabled, window, cx| {
                        panel.selected = None;
                        panel.password.update(cx, |input, cx| input.set_value("", window, cx));
                        panel.run(WifiRequest::Power(*enabled), cx);
                    }))),
            );
        if let Some(error) = &self.error {
            panel = panel.child(gpui::div().text_color(theme.text_muted).child(error.clone()));
        }
        if let Some(network) = self.selected.clone() {
            panel = panel
                .child(gpui::div().child(format!("Connect to {}", network.name())))
                .when(network.security == WifiSecurity::WpaPersonal, |panel| panel.child(Input::new(&self.password).mask_toggle()))
                .child(
                    gpui::div()
                        .flex()
                        .gap(px(8.0))
                        .child(components::base_button("wifi-join").label("Connect").disabled(busy || connecting).on_click(cx.listener(move |panel, _, window, cx| {
                            let password = panel.password.read(cx).value().to_string();
                            panel.password.update(cx, |input, cx| input.set_value("", window, cx));
                            panel.selected = None;
                            panel.run(WifiRequest::Join { network: network.clone(), password }, cx);
                        })))
                        .child(components::base_button("wifi-cancel-entry").label("Cancel").on_click(cx.listener(|panel, _, window, cx| {
                            panel.selected = None;
                            panel.password.update(cx, |input, cx| input.set_value("", window, cx));
                            cx.notify();
                        }))),
                )
                .child(gpui::div().text_color(theme.text_muted).child("New connections are kept for this Wi-Fi session."));
        }
        let rows = network_rows(&self.snapshot);
        if rows.is_empty() {
            panel = panel.child(gpui::div().text_color(theme.text_muted).child(if off {
                "Turn Wi-Fi on to find networks."
            } else if self.snapshot.scanning {
                "Scanning…"
            } else {
                "Refresh to find networks."
            }));
        }
        for (index, row) in rows.into_iter().enumerate() {
            let disabled = busy || off || connecting || (!row.connected && row.saved_id.is_none() && row.network.as_ref().is_none_or(|network| network.security == WifiSecurity::Unsupported));
            panel = panel.child(
                components::base_button(format!("wifi-network-{index}"))
                    .ghost()
                    .w_full()
                    .justify_start()
                    .child(gpui::div().w_full().truncate().text_align(gpui::TextAlign::Left).when(row.connected, |label| label.font_weight(gpui::FontWeight::BOLD)).child(row.name.clone()))
                    .disabled(disabled)
                    .on_click(cx.listener(move |panel, _, window, cx| {
                        panel.password.update(cx, |input, cx| input.set_value("", window, cx));
                        if row.connected {
                            return;
                        }
                        if let Some(id) = row.saved_id {
                            panel.run(WifiRequest::ConnectSaved(id), cx);
                        } else if let Some(network) = &row.network {
                            panel.selected = Some(network.clone());
                            cx.notify();
                        }
                    })),
            );
        }
        panel
    }
}

struct NetworkRow {
    name: String,
    network: Option<WifiNetwork>,
    saved_id: Option<u32>,
    connected: bool,
}

fn network_rows(snapshot: &WifiSnapshot) -> Vec<NetworkRow> {
    let mut rows: Vec<NetworkRow> = Vec::new();
    for network in &snapshot.networks {
        if let Some(row) = rows.iter_mut().find(|row| row.network.as_ref().is_some_and(|other| other.ssid == network.ssid && other.security == network.security)) {
            if row.network.as_ref().unwrap().signal < network.signal {
                row.network = Some(network.clone());
            }
        } else {
            rows.push(NetworkRow { name: network.name(), network: Some(network.clone()), saved_id: None, connected: false });
        }
    }
    for saved in &snapshot.saved {
        if let Some(row) = rows.iter_mut().find(|row| row.name == saved.name && row.saved_id.is_none()) {
            row.saved_id = Some(saved.id);
        } else if !rows.iter().any(|row| row.name == saved.name) {
            rows.push(NetworkRow { name: saved.name.clone(), network: None, saved_id: Some(saved.id), connected: false });
        }
    }
    if snapshot.phase == WifiPhase::Connected {
        if let Some(name) = &snapshot.network {
            if let Some(row) = rows.iter_mut().find(|row| &row.name == name) {
                row.connected = true;
            } else {
                rows.push(NetworkRow { name: name.clone(), network: None, saved_id: None, connected: true });
            }
        }
    }
    rows.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| b.network.as_ref().map(|n| n.signal).cmp(&a.network.as_ref().map(|n| n.signal))).then_with(|| a.name.cmp(&b.name)));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(name: &str, signal: i32) -> WifiNetwork {
        WifiNetwork { ssid: name.as_bytes().to_vec(), signal, security: WifiSecurity::WpaPersonal }
    }

    #[test]
    fn connected_network_precedes_stronger_signals_and_duplicates_merge() {
        let snapshot = WifiSnapshot { phase: WifiPhase::Connected, network: Some("Home".into()), networks: vec![network("Weak", -85), network("Home", -90), network("Strong", -30), network("Weak", -50)], ..WifiSnapshot::default() };
        let rows = network_rows(&snapshot);
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Home", "Strong", "Weak"]);
        assert!(rows[0].connected);
        assert_eq!(rows[2].network.as_ref().unwrap().signal, -50);
    }

    #[test]
    fn connected_network_stays_visible_when_missing_from_scan() {
        let snapshot = WifiSnapshot { phase: WifiPhase::Connected, network: Some("Home".into()), networks: vec![network("Nearby", -30)], ..WifiSnapshot::default() };
        let rows = network_rows(&snapshot);
        assert_eq!(rows[0].name, "Home");
        assert!(rows[0].connected);
        let rows = network_rows(&WifiSnapshot { phase: WifiPhase::Disconnected, ..snapshot });
        assert!(rows.iter().all(|row| !row.connected));
    }
}
