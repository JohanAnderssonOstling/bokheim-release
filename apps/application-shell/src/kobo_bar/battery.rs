use app::{AppClient, BatteryStatus};
use gpui::{Context, Entity, Render, Window, prelude::*, px};
use gpui_component::{IconName, popover::Popover};
use std::time::Duration;

pub(super) struct BatteryPanel {
    backend: AppClient,
    status: Option<BatteryStatus>,
    error: Option<String>,
    busy: bool,
}
impl BatteryPanel {
    pub(super) fn new(backend: AppClient, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |panel, cx| {
            loop {
                if panel.update(cx, |panel, cx| panel.refresh(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(60)).await;
            }
        })
        .detach();
        Self { backend, status: None, error: None, busy: false }
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let backend = self.backend.clone();
        cx.spawn(async move |panel, cx| {
            let result = backend.kobo_battery().await;
            let _ = panel.update(cx, |panel, cx| {
                let previous = (panel.status.clone(), panel.error.clone());
                panel.busy = false;
                match result {
                    Ok(status) => {
                        panel.status = Some(status);
                        panel.error = None;
                    }
                    Err(error) => {
                        panel.status = None;
                        panel.error = Some(error);
                    }
                }
                if previous != (panel.status.clone(), panel.error.clone()) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
impl Render for BatteryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = ui_components::browser_theme(cx);
        gpui::div()
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .bg(theme.page_bg)
            .text_color(theme.text)
            .child("Battery")
            .child(self.status.as_ref().map(|status| format!("{}% · {}", status.percent, status.status)).unwrap_or_else(|| self.error.clone().unwrap_or_else(|| "Loading…".into())))
    }
}
pub(super) fn popover(panel: &Entity<BatteryPanel>, top: gpui::Pixels, cx: &gpui::App) -> Popover {
    let state = panel.read(cx);
    let icon = match &state.status {
        Some(status) if status.status == "Charging" => IconName::BatteryCharging,
        Some(status) if status.percent <= 20 => IconName::BatteryLow,
        Some(status) if status.percent <= 70 => IconName::BatteryMedium,
        Some(_) => IconName::BatteryFull,
        None => IconName::BatteryWarning,
    };
    let label = state.status.as_ref().map(|status| format!("{}%", status.percent)).unwrap_or_else(|| "—".into());
    let refresh = panel.clone();
    Popover::new("kobo-battery-popup")
        .anchor(gpui::Anchor::TopRight)
        .top(top)
        .p_0()
        .trigger(super::device_button("kobo-battery-button", format!("Battery: {label}"), icon))
        .on_open_change(move |open, _, cx| {
            if *open {
                refresh.update(cx, |panel, cx| panel.refresh(cx));
            }
        })
        .child(panel.clone())
}
