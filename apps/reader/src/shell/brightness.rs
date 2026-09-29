use app::AppClient;
use gpui::prelude::*;
use gpui::{App, Context, Entity, FocusHandle, IntoElement, Pixels, Render, Window};
use gpui_component::{
    Disableable, IconName,
    popover::Popover,
    slider::{Slider, SliderEvent, SliderState},
};
use ui_components as components;

#[derive(Clone, Copy)]
enum LightKind {
    Brightness,
    Natural,
}
impl LightKind {
    fn label(self) -> &'static str {
        match self {
            Self::Brightness => "Brightness",
            Self::Natural => "Natural light",
        }
    }
}

/// Reopening reads both drivers again, including after wake.
pub struct BrightnessControls {
    brightness: Entity<LightControl>,
    natural: Entity<LightControl>,
}
impl BrightnessControls {
    pub fn new(backend: AppClient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let brightness = cx.new(|cx| LightControl::new(backend.clone(), LightKind::Brightness, window, cx));
        let natural = cx.new(|cx| LightControl::new(backend, LightKind::Natural, window, cx));
        // Read after the entity has been installed in the window. Starting the
        // request inside another entity constructor can race the first Kobo
        // frame and leave the slider at its default value until the popover is
        // opened.
        cx.spawn_in(window, async move |panel, cx| {
            let _ = panel.update_in(cx, |panel, window, cx| panel.refresh(window, cx));
        })
        .detach();
        Self { brightness, natural }
    }
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.brightness.update(cx, |control, cx| control.request(None, window, cx));
        self.natural.update(cx, |control, cx| control.request(None, window, cx));
    }
}
impl Render for BrightnessControls {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        components::reader_settings_dropdown(window, components::browser_theme(cx)).child(self.brightness.clone()).child(self.natural.clone())
    }
}

struct LightControl {
    kind: LightKind,
    unsupported: bool,
    backend: AppClient,
    percent: Option<u8>,
    slider: Entity<SliderState>,
    busy: bool,
    error: Option<String>,
}

impl LightControl {
    fn new(backend: AppClient, kind: LightKind, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let slider = cx.new(|_| SliderState::new().min(0.0).max(100.0).step(if matches!(kind, LightKind::Natural) { 10.0 } else { 1.0 }));
        cx.subscribe_in(&slider, window, |panel, _, event, window, cx| match event {
            SliderEvent::Change(_) => cx.notify(),
            SliderEvent::Release(value) => {
                // Commit once per gesture so dragging does not queue driver writes.
                panel.request(Some(value.start().round().clamp(0.0, 100.0) as u8), window, cx);
            }
        })
        .detach();
        Self { kind, unsupported: false, backend, percent: None, slider, busy: false, error: None }
    }

    fn request(&mut self, percent: Option<u8>, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let backend = self.backend.clone();
        let kind = self.kind;
        cx.spawn_in(window, async move |panel, cx| {
            let result = match (kind, percent) {
                (LightKind::Brightness, Some(percent)) => backend.set_kobo_brightness(percent).await.map(Some),
                (LightKind::Brightness, None) => backend.kobo_brightness().await.map(Some),
                (LightKind::Natural, Some(percent)) => backend.set_kobo_natural_light(percent).await.map(Some),
                (LightKind::Natural, None) => backend.kobo_natural_light().await,
            };
            let _ = panel.update_in(cx, |panel, window, cx| {
                panel.busy = false;
                match result {
                    Ok(Some(percent)) => {
                        panel.unsupported = false;
                        panel.percent = Some(percent);
                        panel.slider.update(cx, |slider, cx| slider.set_value(f32::from(percent), window, cx));
                    }
                    Ok(None) => {
                        panel.percent = None;
                        panel.unsupported = true;
                    }
                    Err(error) => {
                        panel.percent = None;
                        panel.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for LightControl {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let disabled = self.busy || self.percent.is_none();
        let label = if self.percent.is_some() {
            format!("{:.0}%", self.slider.read(cx).value().start())
        } else if self.busy {
            "Loading…".to_owned()
        } else {
            "Unavailable".to_owned()
        };
        let mut panel = components::reader_settings_section(self.kind.label(), theme).child(
            gpui::div()
                .flex()
                .items_center()
                .gap(gpui::px(components::SPACE_MD))
                .child(gpui::div().flex_1().min_w_0().py(gpui::px(12.0)).child(Slider::new(&self.slider).disabled(disabled)))
                .child(gpui::div().w(gpui::px(48.0)).text_align(gpui::TextAlign::Right).child(label)),
        );
        if self.unsupported {
            panel = panel.child(gpui::div().text_color(theme.text_muted).child("Not supported on this Kobo"));
        } else if matches!(self.kind, LightKind::Natural) {
            panel = panel.child(gpui::div().flex().justify_between().text_color(theme.text_muted).child("Cooler").child("Warmer"));
        }
        if let Some(error) = self.error.clone() {
            panel = panel
                .child(gpui::div().text_color(theme.text_muted).child(error))
                .child(components::topbar_action_button("brightness-retry", "Retry", IconName::Redo2, theme).disabled(self.busy).on_click(cx.listener(|panel, _, window, cx| panel.request(None, window, cx))));
        }
        panel
    }
}

pub fn popover(panel: Entity<BrightnessControls>, document_focus: Option<&FocusHandle>, theme: components::BrowserTheme, window: &Window) -> Popover {
    popover_with_height(panel, document_focus, theme, None, window)
}

pub fn popover_with_height(panel: Entity<BrightnessControls>, document_focus: Option<&FocusHandle>, theme: components::BrowserTheme, height: Option<Pixels>, window: &Window) -> Popover {
    let refresh = panel.clone();
    let mut popover = Popover::new("reader-brightness")
        .anchor(gpui::Anchor::TopRight)
        .top(gpui::px(components::reader_toolbar_size_px(window) + 4.0))
        .p_0()
        .when_some(document_focus, |popover, focus| popover.track_focus(focus))
        .trigger(components::reader_control_button_icon("reader-brightness-button", "Brightness", IconName::Sun, theme))
        .on_open_change(move |open, window, cx: &mut App| {
            if *open {
                refresh.update(cx, |panel, cx| panel.refresh(window, cx));
            }
        });
    popover = if let Some(height) = height { popover.child(gpui::div().h(height).child(panel)) } else { popover.child(panel) };
    popover
}
