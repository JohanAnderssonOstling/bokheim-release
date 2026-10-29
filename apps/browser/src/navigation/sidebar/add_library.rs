//! Desktop dropdown and centered mobile overlay for adding a library.

use gpui::prelude::*;
use gpui::{Anchor, AnyElement, App, Context, DismissEvent, Entity, FocusHandle, Focusable, KeyDownEvent, MouseButton, Subscription, Window, anchored, deferred, div, px};
use gpui_component::IconName;
use gpui_component::menu::PopupMenu;
use ui_components as components;
use ui_components::Button;

use super::Sidebar;

#[derive(Clone, Copy)]
enum AddLibraryChoice {
    Create,
    #[cfg(feature = "google-drive")]
    CloudStorage,
    #[cfg(not(target_arch = "arm"))]
    OpenFolder,
}

impl AddLibraryChoice {
    const ALL: &[Self] = &[
        Self::Create,
        #[cfg(feature = "google-drive")]
        Self::CloudStorage,
        #[cfg(not(target_arch = "arm"))]
        Self::OpenFolder,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Create => "Create new library",
            #[cfg(feature = "google-drive")]
            Self::CloudStorage => "Connect cloud storage",
            #[cfg(not(target_arch = "arm"))]
            Self::OpenFolder => "Open library from folder",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Create => IconName::Plus,
            #[cfg(feature = "google-drive")]
            Self::CloudStorage => IconName::FolderOpen,
            #[cfg(not(target_arch = "arm"))]
            Self::OpenFolder => IconName::FolderOpen,
        }
    }

    fn available(self) -> bool {
        match self {
            #[cfg(feature = "google-drive")]
            Self::CloudStorage => app::google_drive_available(),
            _ => true,
        }
    }
}

pub(super) struct AddLibraryChoices {
    menu: Option<(Entity<PopupMenu>, Subscription)>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    selected: usize,
    choices: Vec<AddLibraryChoice>,
}

impl Sidebar {
    pub(super) fn open_add_library_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choices: Vec<_> = AddLibraryChoice::ALL.iter().copied().filter(|choice| choice.available()).collect();
        let previous_focus = window.focused(cx);
        let focus = cx.focus_handle();
        let menu = if cx.supports_touch_input() || components::uses_mobile_navigation(window) {
            focus.focus(window, cx);
            None
        } else {
            let sidebar = cx.entity();
            let menu_choices = choices.clone();
            let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
                for &choice in &menu_choices {
                    let sidebar = sidebar.clone();
                    menu = menu.item(components::menu_item_with_icon(choice.label(), choice.icon(), move |_, window, cx| {
                        sidebar.update(cx, |sidebar, cx| sidebar.add_library(choice, window, cx));
                    }));
                }
                menu
            });
            let subscription = cx.subscribe_in(&menu, window, |sidebar, _, _: &DismissEvent, _, cx| {
                sidebar.add_library_choices = None;
                cx.notify();
            });
            menu.focus_handle(cx).focus(window, cx);
            Some((menu, subscription))
        };
        self.add_library_choices = Some(AddLibraryChoices { menu, focus, previous_focus, selected: 0, choices });
        cx.notify();
    }

    pub(super) fn close_add_library_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(choices) = self.add_library_choices.take() {
            if let Some(focus) = choices.previous_focus {
                focus.focus(window, cx);
            }
            cx.notify();
        }
    }

    fn add_library(&mut self, choice: AddLibraryChoice, window: &mut Window, cx: &mut Context<Self>) {
        self.close_add_library_choices(window, cx);
        if !choice.available() {
            return;
        }
        self.switcher_open = false;
        match choice {
            AddLibraryChoice::Create => self.open_create_library_dialog(window, cx),
            #[cfg(feature = "google-drive")]
            AddLibraryChoice::CloudStorage => self.open_cloud_library(window, cx),
            #[cfg(not(target_arch = "arm"))]
            AddLibraryChoice::OpenFolder => self.choose_library_folders(cx),
        }
        cx.notify();
    }

    pub(crate) fn render_add_library_control(&self, emitter: Entity<Self>, button: Button, in_switcher: bool) -> AnyElement {
        let was_open = self.add_library_choices.is_some();
        div()
            .flex_none()
            .child(
                button
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        emitter.update(cx, |sidebar, cx| {
                            if was_open {
                                sidebar.close_add_library_choices(window, cx);
                            } else {
                                sidebar.open_add_library_choices(window, cx);
                            }
                        });
                    })
                    .on_click(|_, _, cx| cx.stop_propagation()),
            )
            .children(
                self.add_library_choices
                    .as_ref()
                    .filter(|_| self.switcher_open == in_switcher)
                    .and_then(|choices| choices.menu.as_ref())
                    .map(|(menu, _)| deferred(anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.0)).child(menu.clone())).with_priority(3)),
            )
            .into_any_element()
    }

    fn add_library_overlay_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choices) = self.add_library_choices.as_mut() else { return };
        match event.keystroke.key.as_str() {
            "up" => choices.selected = choices.selected.saturating_sub(1),
            "down" => choices.selected = (choices.selected + 1).min(choices.choices.len() - 1),
            "enter" | "space" => {
                let choice = choices.choices[choices.selected];
                self.add_library(choice, window, cx);
            }
            "escape" => self.close_add_library_choices(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_add_library_overlay(&self, emitter: Entity<Self>, window: &Window, cx: &App) -> Option<AnyElement> {
        let choices = self.add_library_choices.as_ref().filter(|choices| choices.menu.is_none())?;
        let theme = components::browser_theme(cx);
        let close = emitter.clone();
        let header = components::library_switcher_header("Add library", theme).child(components::outlined_icon_button("add-library-overlay-close", "Close", IconName::Close).on_click(move |_, window, cx| {
            close.update(cx, |sidebar, cx| sidebar.close_add_library_choices(window, cx));
        }));
        let keys = emitter.clone();
        let mut panel = components::modal_form_surface(theme)
            .id("add-library-overlay")
            .debug_selector(|| "add-library-overlay".into())
            .w(px(360.0))
            .gap(px(components::SPACE_XS))
            .track_focus(&choices.focus)
            .on_key_down(move |event, window, cx| keys.update(cx, |sidebar, cx| sidebar.add_library_overlay_key(event, window, cx)))
            .child(header);
        for (index, &choice) in choices.choices.iter().enumerate() {
            let select = emitter.clone();
            panel = panel.child(components::library_switcher_item(("add-library-choice", index), choice.label(), false, index == choices.selected, theme).flex_none().w_full().min_h(px(48.0)).on_click(move |_, window, cx| {
                cx.stop_propagation();
                select.update(cx, |sidebar, cx| sidebar.add_library(choice, window, cx));
            }));
        }
        Some(
            deferred(components::anchor_to_window(
                components::modal_overlay()
                    .child(components::modal_scrim("add-library-overlay-scrim", theme).on_click(move |_, window, cx| {
                        emitter.update(cx, |sidebar, cx| sidebar.close_add_library_choices(window, cx));
                    }))
                    .child(panel),
                window,
            ))
            .with_priority(4)
            .into_any_element(),
        )
    }
}
