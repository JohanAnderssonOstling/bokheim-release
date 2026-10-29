//! A single interface-font override, with a lazy, virtualized system catalog.

use gpui::prelude::*;
use gpui::{Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render, ScrollStrategy, Task, UniformListScrollHandle, Window, div, px, rems, uniform_list};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::{Scrollbar, ScrollbarAxis, ScrollbarShow};
use gpui_component::tooltip::Tooltip;
use ui_components as components;

pub(super) struct FontSelected(pub String);

pub(super) struct FontPicker {
    input: Entity<InputState>,
    selected: Option<String>,
    active: bool,
    families: Vec<String>,
    filtered: Vec<usize>,
    catalog_loaded: bool,
    loading: bool,
    open: bool,
    trigger_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    _catalog_task: Option<Task<()>>,
}

impl EventEmitter<FontSelected> for FontPicker {}

impl FontPicker {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search fonts"));
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => this.filter(cx),
                InputEvent::PressEnter { .. } => {
                    if let Some(family) = this.filtered.first().map(|index| this.families[*index].clone()) {
                        this.choose(family, window, cx);
                    }
                }
                _ => {}
            }
            cx.notify();
        })
        .detach();
        // Android publishes its catalog after registering fonts in the background.
        cx.observe_global::<components::SystemFontFamilies>(|this, cx| {
            if this.loading
                && let Some(fonts) = cx.try_global::<components::SystemFontFamilies>()
            {
                this.set_families(fonts.0.clone(), cx);
            }
        })
        .detach();
        Self {
            input,
            selected: None,
            active: false,
            families: Vec::new(),
            filtered: Vec::new(),
            catalog_loaded: false,
            loading: false,
            open: false,
            trigger_focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
            _catalog_task: None,
        }
    }

    pub(super) fn set_selected(&mut self, family: Option<String>) {
        self.active = family.is_some();
        self.selected = family;
    }

    pub(super) fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    fn filter(&mut self, cx: &gpui::App) {
        let query = self.input.read(cx).value().to_lowercase();
        self.filtered = self.families.iter().enumerate().filter_map(|(index, family)| family.to_lowercase().contains(&query).then_some(index)).collect();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn set_families(&mut self, mut families: Vec<String>, cx: &mut Context<Self>) {
        families.retain(|family| !family.starts_with('.'));
        families.sort_by_cached_key(|family| family.to_lowercase());
        families.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        self.families = families;
        self.catalog_loaded = true;
        self.loading = false;
        self.filter(cx);
        cx.notify();
    }

    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        if self.catalog_loaded || self.loading {
            return;
        }
        self.loading = true;
        if let Some(fonts) = cx.try_global::<components::SystemFontFamilies>() {
            if !cfg!(target_os = "android") || !fonts.0.is_empty() {
                self.set_families(fonts.0.clone(), cx);
            }
            return;
        }
        let text_system = cx.text_system().clone();
        let fonts = cx.background_executor().spawn(async move { text_system.all_font_names() });
        self._catalog_task = Some(cx.spawn(async move |this, cx| {
            let families = fonts.await;
            let _ = this.update(cx, |this, cx| {
                this.set_families(families.clone(), cx);
                cx.set_global(components::SystemFontFamilies(families));
            });
        }));
    }

    fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.active
            && let Some(family) = self.selected.clone()
        {
            self.choose(family, window, cx);
        }
        self.open = true;
        self.input.update(cx, |input, cx| input.set_value("", window, cx));
        self.filter(cx);
        self.load_catalog(cx);
        self.input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.trigger_focus.focus(window, cx);
        cx.notify();
    }

    fn choose(&mut self, family: String, _: &mut Window, cx: &mut Context<Self>) {
        self.active = true;
        self.selected = Some(family.clone());
        cx.emit(FontSelected(family));
        cx.notify();
    }
}

impl Render for FontPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let preview_family = self.selected.clone().filter(|family| !cfg!(target_os = "android") || cx.try_global::<components::SystemFontFamilies>().is_some_and(|fonts| fonts.0.contains(family)));
        let sample = components::reader_typeface_sample(None).when_some(preview_family, |sample, family| sample.font_family(family));
        let tooltip = self.selected.clone().unwrap_or_else(|| "Choose an installed font".into());
        let label = self.selected.clone().unwrap_or_else(|| "Custom".into());
        let trigger = components::reader_picture_tile("ui-font-custom", sample, label, self.open || self.active, theme)
            .w_full()
            .track_focus(&self.trigger_focus)
            .debug_selector(|| "ui-font-custom-trigger".into())
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| this.show(window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    this.show(window, cx);
                    cx.stop_propagation();
                }
            }));
        let trigger = div().w_full().child(trigger);
        if !self.open {
            return trigger;
        }
        let rows = uniform_list(
            "ui-font-list",
            self.filtered.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let theme = components::browser_theme(cx);
                range
                    .map(|row| {
                        let index = this.filtered[row];
                        let family = this.families[index].clone();
                        components::font_picker_row(("ui-font", index), family.clone(), this.selected.as_deref() == Some(family.as_str()), theme)
                            .debug_selector(move || format!("ui-font-row-{row}"))
                            .on_click(cx.listener(move |this, _, window, cx| this.choose(family.clone(), window, cx)))
                    })
                    .collect()
            }),
        )
        .size_full()
        .track_scroll(&self.scroll);
        let list = div()
            .id("ui-font-scroll")
            .debug_selector(|| "ui-font-catalog".into())
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(rows)
            .child(Scrollbar::new(&self.scroll).axis(ScrollbarAxis::Vertical).scrollbar_show(ScrollbarShow::Always));
        let panel = components::font_picker_surface(window, theme)
            .id("ui-font-overlay")
            .debug_selector(|| "ui-font-overlay".into())
            .tab_group()
            .track_focus(&self.input.read(cx).focus_handle(cx))
            .when(self.catalog_loaded && self.selected.as_ref().is_some_and(|family| !self.families.contains(family)), |panel| panel.child(div().text_color(theme.text_muted).child("Saved font unavailable on this device")))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(components::SPACE_SM))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
                    .child(components::bottom_sheet_close_button("ui-font-overlay-close", theme).on_click(cx.listener(|this, _, window, cx| this.close(window, cx)))),
            )
            .when(self.loading || self.filtered.is_empty(), |panel| panel.child(div().text_color(theme.text_muted).child(if self.loading { "Loading fonts…" } else { "No matching fonts" })))
            .when(!self.loading && !self.filtered.is_empty(), |panel| panel.child(list));
        let picker = cx.entity();
        trigger.child(components::font_picker_overlay(panel, window, theme, move |window, cx| picker.update(cx, |picker, cx| picker.close(window, cx))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, point};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[gpui::test]
    fn interface_font_picker_is_lazy_scrolls_and_applies_the_actual_family(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
            let families = (0..1000).map(|index| format!("Font {index:04}")).chain(["NV Garamond".into()]).collect();
            cx.set_global(components::SystemFontFamilies(families));
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let picker = cx.new(|cx| FontPicker::new(window, cx));
            gpui_component::Root::new(picker, window, cx)
        });
        let picker = cx.update(|_, cx| root.read(cx).view().clone().downcast::<FontPicker>().ok().unwrap());
        picker.read_with(cx, |picker, _| {
            assert!(!picker.catalog_loaded);
            assert!(picker.families.is_empty());
        });
        let selected = Rc::new(RefCell::new(None));
        let target = selected.clone();
        let _subscription = cx.update(|_, cx| cx.subscribe(&picker, move |_, event: &FontSelected, _| *target.borrow_mut() = Some(event.0.clone())));
        cx.run_until_parked();
        let bounds = cx.debug_bounds("ui-font-custom-trigger").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        let desktop = cx.debug_bounds("ui-font-overlay").unwrap();
        cx.simulate_resize(gpui::size(px(390.0), px(640.0)));
        cx.run_until_parked();
        let phone = cx.debug_bounds("ui-font-overlay").unwrap();
        assert!(phone.size.width < desktop.size.width);
        assert!(phone.size.height < desktop.size.height);
        assert!(phone.left() >= px(0.0) && phone.right() <= px(390.0));
        assert!(phone.top() >= px(0.0) && phone.bottom() <= px(640.0));
        assert!(cx.debug_bounds("ui-font-row-0").is_some());
        assert!(cx.debug_bounds("ui-font-row-999").is_none());
        let bounds = cx.debug_bounds("ui-font-catalog").unwrap();
        cx.simulate_event(ScrollWheelEvent { position: bounds.center(), delta: ScrollDelta::Pixels(point(px(0.0), px(-132.0))), ..Default::default() });
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| assert!(picker.scroll.0.borrow().base_handle.offset().y < px(0.0)));
        cx.simulate_keystrokes("g a r enter");
        cx.run_until_parked();
        assert_eq!(*selected.borrow(), Some("NV Garamond".into()));
        picker.read_with(cx, |picker, _| {
            assert!(picker.open);
            assert_eq!(picker.selected.as_deref(), Some("NV Garamond"));
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| assert!(!picker.open));
        let bounds = cx.debug_bounds("ui-font-custom-trigger").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        picker.read_with(cx, |picker, cx| {
            assert!(picker.open);
            assert!(picker.input.read(cx).value().is_empty());
            assert_eq!(picker.filtered.len(), 1001);
        });
        cx.simulate_keystrokes("escape");
        picker.update(cx, |picker, cx| {
            picker.set_active(false);
            cx.notify();
        });
        *selected.borrow_mut() = None;
        cx.run_until_parked();
        let bounds = cx.debug_bounds("ui-font-custom-trigger").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(*selected.borrow(), Some("NV Garamond".into()), "clicking the saved font restores it immediately");
        picker.read_with(cx, |picker, _| {
            assert!(picker.active);
            assert!(picker.open);
            assert_eq!(picker.selected.as_deref(), Some("NV Garamond"));
        });
    }
}
