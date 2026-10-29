use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render, ScrollStrategy, Task, UniformListScrollHandle, Window, div, px, rems, uniform_list};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::{Scrollbar, ScrollbarAxis, ScrollbarShow};
use html_view_gpui::is_private_font_family;
use ui_components as components;

use crate::settings::{ReaderFontRole, ReaderSettings};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FontSelected {
    pub role: ReaderFontRole,
    pub family: String,
}

pub(super) struct FontPicker {
    input: Entity<InputState>,
    families: Vec<String>,
    filtered: Vec<usize>,
    catalog_loaded: bool,
    loading: bool,
    catalog_task: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    open: bool,
    previous_focus: Option<FocusHandle>,
    active_role: Option<ReaderFontRole>,
    role_focus: [FocusHandle; 4],
    cjk: bool,
}

impl EventEmitter<FontSelected> for FontPicker {}

fn font_supports_cjk(text_system: &gpui::TextSystem, family: &str) -> bool {
    if family.starts_with('.') || is_private_font_family(family) || family == components::CJK_FONT_FAMILY {
        return false;
    }
    let descriptor = gpui::font(family.to_owned());
    let Ok(id) = text_system.font_id(&descriptor) else {
        return false;
    };
    ["漢字中文", "汉字中文", "あいうえ", "アイウエ", "한글한국"].iter().any(|sample| sample.chars().all(|character| text_system.advance(id, px(16.0), character).is_ok()))
}

impl FontPicker {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search fonts"));
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                if let Some(family) = this.filtered.first().and_then(|index| this.families.get(*index)).cloned() {
                    this.choose(family, window, cx);
                }
            } else if matches!(event, InputEvent::Change) {
                this.filter(cx);
            }
            cx.notify();
        })
        .detach();
        // Creating a reader must neither enumerate fonts nor shape previews.
        Self {
            input,
            families: Vec::new(),
            filtered: Vec::new(),
            catalog_loaded: false,
            loading: false,
            catalog_task: None,
            scroll: UniformListScrollHandle::new(),
            open: false,
            previous_focus: None,
            active_role: None,
            cjk: false,
            role_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
        }
    }

    pub(super) fn set_cjk(&mut self, cjk: bool) {
        self.cjk = cjk;
    }

    pub(super) fn cjk_focus(&self) -> &FocusHandle {
        &self.role_focus[ReaderFontRole::Cjk.index()]
    }

    pub(super) fn activate_cjk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(family) = ReaderSettings::preferences(cx).saved_cjk_font_family().map(str::to_owned) {
            self.hide(cx);
            cx.emit(FontSelected { role: ReaderFontRole::Cjk, family });
        } else {
            self.show_cjk(window, cx);
        }
    }

    pub(super) fn show_cjk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus = window.focused(cx);
        self.select_role(ReaderFontRole::Cjk, window, cx);
    }

    fn filter(&mut self, cx: &App) {
        let query = self.input.read(cx).value().to_lowercase();
        self.filtered = self.families.iter().enumerate().filter_map(|(index, family)| family.to_lowercase().contains(&query).then_some(index)).collect();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn set_families(&mut self, mut families: Vec<String>, cx: &mut Context<Self>) {
        families.retain(|family| !family.starts_with('.') && !is_private_font_family(family) && family != components::CJK_FONT_FAMILY);
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
        let cached = cx.try_global::<components::SystemFontFamilies>().map(|fonts| fonts.0.clone());
        if !self.cjk
            && let Some(families) = cached.as_ref()
        {
            self.set_families(families.clone(), cx);
            return;
        }
        self.loading = true;
        let text_system = cx.text_system().clone();
        let cjk = self.cjk;
        let fonts = cx.background_executor().spawn(async move {
            let all = cached.unwrap_or_else(|| text_system.all_font_names());
            let mut selected = all.clone();
            selected.retain(|family| family != components::CJK_FONT_FAMILY && (!cjk || font_supports_cjk(&text_system, family)));
            (all, selected)
        });
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let (all, selected) = fonts.await;
            let _ = this.update(cx, |this, cx| {
                cx.set_global(components::SystemFontFamilies(all));
                this.set_families(selected, cx);
            });
        }));
    }

    pub(super) fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.active_role = None;
        self.previous_focus = window.focused(cx);
        self.role_focus[0].focus(window, cx);
        cx.notify();
    }

    fn select_role(&mut self, role: ReaderFontRole, window: &mut Window, cx: &mut Context<Self>) {
        self.active_role = Some(role);
        self.input.update(cx, |input, cx| input.set_value("", window, cx));
        self.filter(cx);
        self.load_catalog(cx);
        self.input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(role) = self.active_role.take() {
            self.role_focus[role.index()].focus(window, cx);
        }
        cx.notify();
    }

    fn choose(&mut self, family: String, _: &mut Window, cx: &mut Context<Self>) {
        let Some(role) = self.active_role else { return };
        cx.emit(FontSelected { role, family });
        cx.notify();
    }
}

impl Render for FontPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let preferences = ReaderSettings::preferences(cx).clone();
        let choices = ReaderFontRole::ALL.into_iter().map(|role| {
            let family = preferences.custom_font_family(role).map(str::to_owned);
            let name = family.clone().unwrap_or_else(|| "Book default".into());
            let active = self.active_role == Some(role);
            components::reader_state_card(("custom-font-role", role.index()), active, theme)
                .flex_1()
                .w_auto()
                .border_1()
                .border_color(if active { theme.accent } else { theme.rule })
                .rounded(px(components::RADIUS_SM))
                .px(px(components::SPACE_XS))
                .py(px(components::SPACE_SM))
                .items_center()
                .justify_center()
                .text_color(if active { theme.text } else { theme.text_muted })
                .cursor_pointer()
                .child(div().text_size(rems(components::TEXT_XS)).child(role.label()))
                .debug_selector(move || format!("custom-font-role-{}", role.index()))
                .track_focus(&self.role_focus[role.index()])
                .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_center().text_size(rems(components::TEXT_XS)).when_some(family, |label, family| label.font_family(family)).child(name))
                .on_click(cx.listener(move |this, _, window, cx| this.select_role(role, window, cx)))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.select_role(role, window, cx);
                        cx.stop_propagation();
                    }
                }))
        });
        // The Typeface container already supplies SPACE_XXS between its children.
        let rows = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(components::SPACE_SM))
            .when(!self.cjk && (self.open || preferences.custom_fonts_active()), |rows| rows.mt(px(components::SPACE_SM - components::SPACE_XXS)).child(components::reader_tile_group("reader-custom-font-roles").children(choices)));
        rows.when(self.active_role.is_some(), |rows| {
            let picker = cx.entity();
            rows.child(components::font_picker_overlay(self.panel(window, cx), window, theme, move |window, cx| picker.update(cx, |picker, cx| picker.back(window, cx))))
        })
    }
}

impl FontPicker {
    pub(super) fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.active_role = None;
        cx.notify();
    }

    fn panel(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let Some(role) = self.active_role else { return div().into_any_element() };
        let selected = ReaderSettings::preferences(cx).custom_font_family(role);
        let rows = uniform_list(
            "font-list",
            self.filtered.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let theme = components::browser_theme(cx);
                let selected = this.active_role.and_then(|role| ReaderSettings::preferences(cx).custom_font_family(role));
                range
                    .map(|row| {
                        let index = this.filtered[row];
                        let family = this.families[index].clone();
                        components::font_picker_row(("system-font", index), family.clone(), selected == Some(family.as_str()), theme)
                            .debug_selector(move || format!("custom-font-row-{row}"))
                            .on_click(cx.listener(move |this, _, window, cx| this.choose(family.clone(), window, cx)))
                    })
                    .collect()
            }),
        )
        .size_full()
        .track_scroll(&self.scroll);
        let list = div()
            .id("custom-font-scroll")
            .debug_selector(|| "custom-font-list".into())
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(rows)
            .child(Scrollbar::new(&self.scroll).axis(ScrollbarAxis::Vertical).scrollbar_show(ScrollbarShow::Always));
        let body = if self.loading || self.filtered.is_empty() {
            div().flex_1().min_h_0().p(px(components::SPACE_SM)).text_color(theme.text_muted).child(if self.loading { "Loading fonts…" } else { "No matching fonts" }).into_any_element()
        } else {
            list.into_any_element()
        };
        components::font_picker_surface(window, theme)
            .id("reader-font-overlay")
            .debug_selector(|| "reader-font-overlay".into())
            .tab_group()
            .key_context("ReaderFontPicker")
            .track_focus(&self.input.read(cx).focus_handle(cx))
            .when(self.catalog_loaded && selected.is_some_and(|family| !self.families.iter().any(|available| available == family)), |panel| {
                panel.child(div().debug_selector(|| "reader-font-unavailable".into()).text_color(theme.text_muted).child("Saved font unavailable on this device"))
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(components::SPACE_SM))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
                    .child(components::bottom_sheet_close_button("font-overlay-close", theme).on_click(cx.listener(|this, _, window, cx| this.back(window, cx)))),
            )
            .child(body)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::settings::{CjkFontChoice, ReaderPreferences};
    use gpui::{AppContext, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, point};

    #[gpui::test]
    fn saved_cjk_font_switches_directly_and_editing_keeps_book_until_a_choice(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
            let preferences = ReaderPreferences {
                system_cjk_font_family: Some("Saved CJK".into()),
                system_font_family: Some("Latin font".into()),
                custom_fonts_enabled: Some(true),
                ..ReaderPreferences::default()
            };
            cx.set_global(ReaderSettings { preferences, save: Rc::new(|_, _, _| {}) });
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let picker = cx.new(|cx| {
                let mut picker = FontPicker::new(window, cx);
                picker.set_cjk(true);
                picker
            });
            gpui_component::Root::new(picker, window, cx)
        });
        let picker = cx.update(|_, cx| root.read(cx).view().clone().downcast::<FontPicker>().ok().unwrap());
        let _subscription = cx.update(|window, cx| {
            let subscription = cx.subscribe(&picker, |_, event: &FontSelected, cx| {
                ReaderSettings::update(cx, |preferences| preferences.set_custom_font_family(event.role, event.family.clone()));
            });
            picker.update(cx, |picker, cx| picker.activate_cjk(window, cx));
            subscription
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-font-overlay").is_none());
        picker.read_with(cx, |picker, cx| {
            assert!(!picker.catalog_loaded, "restoring a saved choice must not enumerate fonts");
            assert_eq!(ReaderSettings::preferences(cx).cjk_font_choice, CjkFontChoice::Custom);
        });

        cx.update(|window, cx| {
            ReaderSettings::update(cx, |preferences| preferences.cjk_font_choice = CjkFontChoice::Publisher);
            picker.update(cx, |picker, cx| {
                picker.set_families(vec!["Saved CJK".into(), "Replacement CJK".into()], cx);
                picker.show_cjk(window, cx);
            });
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-font-overlay").is_some());
        picker.read_with(cx, |_, cx| assert_eq!(ReaderSettings::preferences(cx).cjk_font_choice, CjkFontChoice::Publisher));
        cx.simulate_keystrokes("r e p enter");
        cx.run_until_parked();
        picker.read_with(cx, |_, cx| {
            let preferences = ReaderSettings::preferences(cx);
            assert_eq!(preferences.saved_cjk_font_family(), Some("Replacement CJK"));
            assert_eq!(preferences.cjk_font_choice, CjkFontChoice::Custom);
            assert_eq!(preferences.system_font_family.as_deref(), Some("Latin font"));
            assert!(preferences.custom_fonts_active());
        });
        cx.update(|window, cx| {
            ReaderSettings::update(cx, |preferences| preferences.cjk_font_choice = CjkFontChoice::Publisher);
            picker.update(cx, |picker, cx| picker.activate_cjk(window, cx));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-font-overlay").is_none());
        picker.read_with(cx, |_, cx| assert_eq!(ReaderSettings::preferences(cx).cjk_font_choice, CjkFontChoice::Custom));

        cx.update(|window, cx| {
            ReaderSettings::update(cx, |preferences| {
                preferences.system_cjk_font_family = None;
                preferences.cjk_font_choice = CjkFontChoice::Publisher;
            });
            picker.update(cx, |picker, cx| picker.activate_cjk(window, cx));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-font-overlay").is_some());
        picker.read_with(cx, |_, cx| assert_eq!(ReaderSettings::preferences(cx).cjk_font_choice, CjkFontChoice::Publisher));
    }

    #[gpui::test]
    fn cjk_picker_restores_local_choice_and_keeps_latin_roles_separate(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
            let preferences = ReaderPreferences {
                cjk_font_choice: CjkFontChoice::Custom,
                system_cjk_font_family: Some("Unavailable CJK font".into()),
                ..ReaderPreferences::default()
            };
            let preferences = serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
            cx.set_global(ReaderSettings { preferences, save: Rc::new(|_, _, _| {}) });
            cx.set_global(components::SystemFontFamilies(vec!["System CJK".into()]));
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let picker = cx.new(|cx| {
                let mut picker = FontPicker::new(window, cx);
                picker.set_cjk(true);
                picker
            });
            gpui_component::Root::new(picker, window, cx)
        });
        let picker = cx.update(|_, cx| root.read(cx).view().clone().downcast::<FontPicker>().ok().unwrap());
        picker.read_with(cx, |picker, _| assert!(!picker.catalog_loaded));
        let _subscription = cx.update(|window, cx| {
            let subscription = cx.subscribe(&picker, |_, event: &FontSelected, cx| {
                ReaderSettings::update(cx, |preferences| preferences.set_custom_font_family(event.role, event.family.clone()));
            });
            picker.update(cx, |picker, cx| { picker.set_families(vec!["System CJK".into()], cx); picker.show_cjk(window, cx) });
            subscription
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("custom-font-role-0").is_none());
        assert!(cx.debug_bounds("reader-font-overlay").is_some());
        assert!(cx.debug_bounds("reader-font-unavailable").is_some());
        picker.read_with(cx, |_, cx| {
            assert_eq!(ReaderSettings::preferences(cx).system_cjk_font_family.as_deref(), Some("Unavailable CJK font"));
        });
        cx.simulate_keystrokes("c j k enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("reader-font-unavailable").is_none());
        picker.read_with(cx, |picker, cx| {
            assert_eq!(picker.active_role, Some(ReaderFontRole::Cjk));
            let preferences = ReaderSettings::preferences(cx);
            assert_eq!(preferences.cjk_font_choice, CjkFontChoice::Custom);
            assert_eq!(preferences.system_cjk_font_family.as_deref(), Some("System CJK"));
            assert!(!preferences.custom_fonts_active());
        });
    }

    #[gpui::test]
    fn large_font_catalog_is_lazy_virtualized_and_scrolls_with_the_wheel(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
            cx.set_global(ReaderSettings { preferences: ReaderPreferences::default(), save: Rc::new(|_, _, _| {}) });
            cx.set_global(components::SystemFontFamilies((0..1000).map(|index| format!("Font {index:04}")).collect()));
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let picker = cx.new(|cx| FontPicker::new(window, cx));
            gpui_component::Root::new(picker, window, cx)
        });
        let picker = cx.update(|_, cx| root.read(cx).view().clone().downcast::<FontPicker>().ok().unwrap());
        picker.read_with(cx, |picker, _| {
            assert!(!picker.catalog_loaded);
            assert!(picker.families.is_empty(), "reader startup must not enumerate fonts");
        });
        cx.update(|window, cx| picker.update(cx, |picker, cx| picker.show(window, cx)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("custom-font-role-0").is_some());
        picker.read_with(cx, |picker, _| assert!(!picker.catalog_loaded, "opening the three role choices must not enumerate fonts"));
        let role = cx.debug_bounds("custom-font-role-0").unwrap();
        cx.simulate_click(role.center(), Modifiers::default());
        cx.run_until_parked();
        let desktop = cx.debug_bounds("reader-font-overlay").unwrap();
        cx.simulate_resize(gpui::size(px(390.0), px(640.0)));
        cx.run_until_parked();
        let phone = cx.debug_bounds("reader-font-overlay").unwrap();
        assert!(phone.size.width < desktop.size.width);
        assert!(phone.size.height < desktop.size.height);
        assert!(phone.left() >= px(0.0) && phone.right() <= px(390.0));
        assert!(phone.top() >= px(0.0) && phone.bottom() <= px(640.0));
        assert!(cx.debug_bounds("custom-font-row-0").is_some());
        assert!(cx.debug_bounds("custom-font-row-999").is_none(), "offscreen families must not be shaped");
        let bounds = cx.debug_bounds("custom-font-list").unwrap();
        cx.simulate_event(ScrollWheelEvent { position: bounds.center(), delta: ScrollDelta::Pixels(point(px(0.0), px(-132.0))), ..Default::default() });
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| assert!(picker.scroll.0.borrow().base_handle.offset().y < px(0.0)));
        cx.simulate_keystrokes("f o n t space 0 9 9 9");
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.filtered.len(), 1);
            assert_eq!(picker.families[picker.filtered[0]], "Font 0999");
            assert_eq!(picker.scroll.0.borrow().base_handle.offset().y, px(0.0));
        });
    }

    #[gpui::test]
    fn each_role_can_choose_any_font_without_closing_the_picker(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            components::register_fonts(cx).unwrap();
            cx.set_global(ReaderSettings { preferences: ReaderPreferences::default(), save: Rc::new(|_, _, _| {}) });
            cx.set_global(components::SystemFontFamilies(vec!["Raleway".into(), "NV Garamond".into()]));
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let picker = cx.new(|cx| FontPicker::new(window, cx));
            gpui_component::Root::new(picker, window, cx)
        });
        let picker = cx.update(|_, cx| root.read(cx).view().clone().downcast::<FontPicker>().ok().unwrap());
        let selected = Rc::new(RefCell::new(Vec::new()));
        let target = selected.clone();
        let _subscription = cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.show(window, cx));
            cx.subscribe(&picker, move |_, event: &FontSelected, cx| {
                ReaderSettings::update(cx, |preferences| preferences.set_custom_font_family(event.role, event.family.clone()));
                target.borrow_mut().push(event.clone());
            })
        });
        cx.run_until_parked();
        for role in ReaderFontRole::ALL {
            cx.update(|window, cx| picker.update(cx, |picker, cx| picker.select_role(role, window, cx)));
            cx.run_until_parked();
            cx.simulate_keystrokes("g a r enter");
            cx.run_until_parked();
            assert!(cx.debug_bounds("reader-font-overlay").is_some());
            picker.read_with(cx, |picker, cx| {
                assert!(picker.open);
                assert_eq!(picker.active_role, Some(role));
                assert_eq!(ReaderSettings::preferences(cx).custom_font_family(role), Some("NV Garamond"));
            });
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
        }
        assert_eq!(*selected.borrow(), ReaderFontRole::ALL.map(|role| FontSelected { role, family: "NV Garamond".into() }));
        cx.update(|window, cx| picker.update(cx, |picker, cx| picker.select_role(ReaderFontRole::Serif, window, cx)));
        picker.read_with(cx, |picker, cx| {
            assert!(picker.open);
            assert!(picker.input.read(cx).value().is_empty());
            assert_eq!(picker.filtered.len(), picker.families.len(), "reopening must restore the unfiltered catalog");
        });
        cx.update(|window, cx| picker.update(cx, |picker, cx| picker.show(window, cx)));
        cx.run_until_parked();
        cx.update(|window, cx| picker.update(cx, |picker, cx| picker.select_role(ReaderFontRole::Serif, window, cx)));
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        picker.read_with(cx, |picker, _| {
            assert!(picker.open);
            assert_eq!(picker.active_role, None);
        });
        assert_eq!(selected.borrow().len(), 3);
    }
}
