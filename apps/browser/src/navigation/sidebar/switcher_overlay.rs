//! Library creation, selection, and switcher overlay rendering.

use gpui::prelude::*;
use gpui::{Context, Entity, KeyDownEvent, MouseButton, PathPromptOptions, PromptLevel, SharedString, Window};
use gpui::{Focusable, Subscription};
use gpui_component::Disableable;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::PopupMenu;
use sync_common::LibraryId;
use ui_components as components;

use super::{Sidebar, SwitcherCursor};

#[derive(Clone)]
struct LibraryChoice {
    id: LibraryId,
    name: SharedString,
}

pub(super) struct LibraryNameDialog {
    id: Option<LibraryId>,
    folders: Option<Vec<gpui::SelectedDirectory>>,
    trace: crate::services::library_add_trace::Trace,
    estimate: Option<(u64, u64, u64)>,
    asset_storage_enabled: bool,
    input: Entity<InputState>,
    pub(super) busy: bool,
    error: Option<SharedString>,
    _subscription: Subscription,
}

impl Sidebar {
    fn open_library_menu(&mut self, id: LibraryId, window: &mut Window, cx: &mut Context<Self>) {
        let rename = cx.entity();
        let delete = cx.entity();
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            menu.item(components::menu_item("Rename", move |_, window, cx| {
                rename.update(cx, |navigation, cx| navigation.open_library_name_dialog(id, window, cx));
            }))
            .item(components::menu_item("Delete", move |_, window, cx| {
                delete.update(cx, |navigation, cx| navigation.prompt_delete_library(id, window, cx));
            }))
        });
        let subscription = cx.subscribe_in(&menu, window, |navigation, _, _: &gpui::DismissEvent, window, cx| {
            navigation.library_menu = None;
            if navigation.library_name_dialog.is_none() {
                navigation.switcher_focus.focus(window, cx);
            }
            cx.notify();
        });
        menu.focus_handle(cx).focus(window, cx);
        self.library_menu = Some((id, menu, subscription));
        cx.notify();
    }

    fn open_library_name_dialog(&mut self, id: LibraryId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(library) = self.libraries(cx).into_iter().find(|library| library.id == id) else { return };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Library name"));
        input.update(cx, |input, cx| input.set_value(library.name, window, cx));
        let subscription = cx.subscribe_in(&input, window, |navigation, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                navigation.submit_library_name(cx);
            }
            cx.notify();
        });
        input.focus_handle(cx).focus(window, cx);
        self.library_name_dialog = Some(LibraryNameDialog { id: Some(id), folders: None, trace: Default::default(), estimate: Some((0, 0, 0)), asset_storage_enabled: true, input, busy: false, error: None, _subscription: subscription });
        cx.notify();
    }

    pub(crate) fn open_create_library_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Library name"));
        let subscription = cx.subscribe_in(&input, window, |navigation, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                navigation.submit_library_name(cx);
            }
            cx.notify();
        });
        input.focus_handle(cx).focus(window, cx);
        self.library_name_dialog = Some(LibraryNameDialog { id: None, folders: None, trace: Default::default(), estimate: Some((0, 0, 0)), asset_storage_enabled: true, input, busy: false, error: None, _subscription: subscription });
        cx.notify();
    }

    fn cancel_library_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library_name_dialog.as_ref().is_some_and(|dialog| dialog.busy) {
            return;
        }
        self.library_name_dialog = None;
        self.switcher_focus.focus(window, cx);
        cx.notify();
    }

    fn submit_library_name(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.library_name_dialog.as_mut() else { return };
        if dialog.busy || dialog.estimate.is_none() {
            return;
        }
        if let Some(folders) = dialog.folders.take() {
            let enabled = dialog.asset_storage_enabled;
            let trace = dialog.trace.clone();
            trace.mark("review_confirmed");
            self.libraries.update(cx, |libraries, cx| libraries.create_selected_libraries(folders, enabled, trace, cx));
            self.library_name_dialog = None;
            self.switcher_open = false;
            cx.notify();
            return;
        }
        let name = dialog.input.read(cx).value().trim().to_owned();
        if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
            dialog.error = Some("Enter a shorter, nonempty library name without control characters.".into());
            cx.notify();
            return;
        }
        dialog.busy = true;
        dialog.error = None;
        let asset_storage_enabled = dialog.asset_storage_enabled;
        let operation = self.libraries.read(cx).save_library_name(dialog.id, name, asset_storage_enabled);
        cx.spawn(async move |navigation, cx| {
            let result = operation.await;
            let _ = navigation.update_in(cx, |navigation, window, cx| {
                match result {
                    Ok(created) => {
                        if let Some(id) = created {
                            navigation.libraries.update(cx, |libraries, cx| libraries.select_library(id, cx));
                        }
                        navigation.library_name_dialog = None;
                        navigation.reset_switcher_cursor(cx);
                        navigation.switcher_focus.focus(window, cx);
                    }
                    Err(error) => {
                        if let Some(dialog) = navigation.library_name_dialog.as_mut() {
                            dialog.busy = false;
                            dialog.error = Some(error.into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_library_name_dialog(&self, emitter: Entity<Self>, cx: &gpui::App) -> Option<gpui::AnyElement> {
        let dialog = self.library_name_dialog.as_ref()?;
        let theme = components::browser_theme(cx);
        let cancel = emitter.clone();
        let submit = emitter.clone();
        let panel = components::modal_surface(theme)
            .w(gpui::px(360.0))
            .max_w_full()
            .p(gpui::px(components::SPACE_MD))
            .flex()
            .flex_col()
            .gap(gpui::px(components::SPACE_MD))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(gpui::div().text_color(theme.text).child(if dialog.id.is_none() { "Create library" } else { "Rename library" }))
            .child(Input::new(&dialog.input).disabled(dialog.busy || dialog.folders.is_some()))
            .children((dialog.id.is_none()).then(|| {
                let toggle = emitter.clone();
                let estimate = match dialog.estimate {
                    Some(estimate) => crate::navigation::folder_creation::estimate_label(estimate),
                    None => "Estimating book size…".to_owned(),
                };
                gpui::div().flex().flex_col().gap(gpui::px(components::SPACE_SM)).child(estimate).child(Checkbox::new("library-upload-choice").label("Cloud storage").checked(dialog.asset_storage_enabled).disabled(dialog.busy).on_click(
                    move |checked, _, cx| {
                        toggle.update(cx, |navigation, cx| {
                            if let Some(dialog) = &mut navigation.library_name_dialog {
                                dialog.asset_storage_enabled = *checked;
                            }
                            cx.notify();
                        })
                    },
                ))
            }))
            .children(dialog.error.clone().map(|error| components::error_text(theme).child(error)))
            .child(
                gpui::div()
                    .flex()
                    .justify_end()
                    .gap(gpui::px(components::SPACE_SM))
                    .child(components::dialog_cancel_button("cancel-library-rename", "Cancel").disabled(dialog.busy).on_click(move |_, window, cx| {
                        cancel.update(cx, |navigation, cx| navigation.cancel_library_name(window, cx));
                    }))
                    .child(
                        components::base_button("save-library-rename")
                            .primary()
                            .label(if dialog.id.is_none() {
                                if dialog.busy { "Creating…" } else { "Create" }
                            } else if dialog.busy {
                                "Renaming…"
                            } else {
                                "Rename"
                            })
                            .disabled(dialog.busy || dialog.estimate.is_none() || dialog.input.read(cx).value().trim().is_empty())
                            .on_click(move |_, _, cx| submit.update(cx, |navigation, cx| navigation.submit_library_name(cx))),
                    ),
            );
        // Keep this in the normal content tree. KoboKeyboardRoot paints its
        // keyboard after application content; a deferred modal would be
        // painted above the keyboard and make its keys appear disabled.
        Some(
            components::modal_overlay()
                .occlude()
                // Do not dismiss this dialog from the full-window scrim.
                // On Kobo, keyboard taps are outside the panel bounds.
                .child(components::modal_scrim("library-rename-scrim", theme))
                .child(panel)
                .into_any_element(),
        )
    }

    pub(crate) fn is_library_switcher_open(&self) -> bool {
        self.switcher_open || self.library_name_dialog.is_some()
    }

    /// Opens the switcher and hands it the keyboard. Every way in — the rail
    /// trigger, the compact More sheet, and running off the top of Home — is
    /// the same four steps, and an opener that forgot the focus left the panel
    /// on screen with no way to drive it.
    pub(crate) fn open_library_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher_open {
            return;
        }
        self.switcher_open = true;
        self.reset_switcher_cursor(cx);
        window.focus(&self.switcher_focus, cx);
        cx.notify();
    }

    /// Places the cursor on the library currently in use, so the first key
    /// press moves from where the reader already is rather than from the top.
    pub(super) fn reset_switcher_cursor(&mut self, cx: &gpui::App) {
        let active = self.libraries.read(cx).selected_id();
        self.switcher_cursor = match self.libraries(cx).iter().position(|library| Some(library.id) == active) {
            Some(index) => SwitcherCursor::Library { index, menu: false },
            None => SwitcherCursor::Add,
        };
    }

    /// Keeps the cursor inside the list after a row disappears.
    fn clamp_switcher_cursor(&mut self, cx: &gpui::App) {
        let count = self.libraries(cx).len();
        self.switcher_cursor = match self.switcher_cursor {
            SwitcherCursor::Library { .. } if count == 0 => SwitcherCursor::Add,
            SwitcherCursor::Library { index, menu } if index >= count => SwitcherCursor::Library { index: count - 1, menu },
            other => other,
        };
    }

    fn move_switcher_cursor(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.library_name_dialog.is_some() || self.library_menu.is_some() {
            return;
        }
        let count = self.libraries(cx).len();
        let cursor = self.switcher_cursor;
        let moved = match event.keystroke.key.as_str() {
            "up" => Some(match cursor {
                SwitcherCursor::Add => SwitcherCursor::Add,
                SwitcherCursor::Library { index: 0, .. } => SwitcherCursor::Add,
                SwitcherCursor::Library { index, menu } => SwitcherCursor::Library { index: index - 1, menu },
            }),
            "down" => Some(match cursor {
                // Leaving the header lands on a library, never on its menu
                // button: the column is a deliberate step sideways.
                SwitcherCursor::Add if count > 0 => SwitcherCursor::Library { index: 0, menu: false },
                SwitcherCursor::Add => SwitcherCursor::Add,
                SwitcherCursor::Library { index, menu } if index + 1 < count => SwitcherCursor::Library { index: index + 1, menu },
                other => other,
            }),
            "left" => Some(match cursor {
                SwitcherCursor::Library { index, menu: true } => SwitcherCursor::Library { index, menu: false },
                other => other,
            }),
            "right" => Some(match cursor {
                SwitcherCursor::Library { index, menu: false } => SwitcherCursor::Library { index, menu: true },
                other => other,
            }),
            "enter" | "space" => {
                self.activate_switcher_cursor(window, cx);
                None
            }
            _ => return,
        };
        if let Some(moved) = moved {
            self.switcher_cursor = moved;
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn activate_switcher_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.switcher_cursor {
            SwitcherCursor::Add => {
                self.switcher_open = false;
                cx.notify();
                self.choose_library_folders(cx);
            }
            SwitcherCursor::Library { index, menu } => {
                let Some(library) = self.libraries(cx).get(index).cloned() else { return };
                if menu {
                    self.open_library_menu(library.id, window, cx);
                } else {
                    self.switcher_open = false;
                    cx.notify();
                    self.libraries.update(cx, |libraries, cx| libraries.select_library(library.id, cx));
                }
            }
        }
    }

    /// Both pointer and keyboard menu actions use the same confirmation.
    fn prompt_delete_library(&mut self, id: LibraryId, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            "Delete this library?",
            Some("Local database and state are removed immediately. Server deletion runs in the background, or automatically after the next sign-in. Local files and stored server blobs are kept."),
            &["Delete library", "Cancel"],
            cx,
        );
        cx.spawn(async move |navigation, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            // The switcher stays open: deleting is a management action, and
            // there is usually more than one to do. Closing it also threw the
            // reader back to a page whose library might be the one just removed.
            let _ = navigation.update(cx, |navigation, cx| {
                navigation.libraries.update(cx, |libraries, cx| libraries.delete_library(id, cx));
                navigation.clamp_switcher_cursor(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn close_library_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.switcher_open && self.library_name_dialog.is_none() {
            return;
        }
        if self.library_name_dialog.is_some() {
            self.cancel_library_name(window, cx);
        } else if self.library_menu.is_some() {
            self.library_menu = None;
            self.switcher_focus.focus(window, cx);
        } else {
            self.switcher_open = false;
        }
        cx.notify();
    }

    pub(crate) fn library_switcher_overlay(&self, emitter: Entity<Self>, window: &gpui::Window, cx: &gpui::App) -> Option<gpui::AnyElement> {
        if self.switcher_open { Some(self.render_library_switcher_overlay(emitter, window, cx)) } else { self.render_library_name_dialog(emitter, cx) }
    }

    pub(crate) fn choose_library_folders(&mut self, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_directories(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("Choose library folders".into()) });
        cx.spawn(async move |navigation, cx| {
            let Ok(Ok(Some(directories))) = selection.await else { return };
            if directories.is_empty() {
                return;
            }
            let _ = navigation.update_in(cx, |navigation, window, cx| navigation.review_library_folders(directories, window, cx));
        })
        .detach();
    }

    fn review_library_folders(&mut self, mut directories: Vec<gpui::SelectedDirectory>, window: &mut Window, cx: &mut Context<Self>) {
        use crate::navigation::folder_creation::{estimate_selected_files, retain_eligible_files};
        let trace = crate::services::library_add_trace::Trace::start();
        for directory in &mut directories {
            retain_eligible_files(directory);
        }
        let (books, bytes, unavailable) = estimate_selected_files(&directories);
        let locators: Vec<String> = directories.iter().filter_map(|directory| directory.local_path.as_ref().map(|path| path.to_string_lossy().into_owned())).collect();
        let title = if directories.len() == 1 { directories[0].name.clone() } else { format!("{} library folders", directories.len()) };
        self.open_create_library_dialog(window, cx);
        self.switcher_open = false;
        let dialog = self.library_name_dialog.as_mut().unwrap();
        dialog.input.update(cx, |input, cx| input.set_value(title, window, cx));
        dialog.folders = Some(directories);
        dialog.trace = trace;
        dialog.estimate = locators.is_empty().then_some((books, bytes, unavailable));
        let input_id = dialog.input.entity_id();
        if !locators.is_empty() {
            let backend = self.libraries.read(cx).folder_creation_backend();
            cx.spawn(async move |navigation, cx| {
                let estimate = backend.estimate_library_folders(locators).await;
                let _ = navigation.update(cx, |navigation, cx| {
                    if let Some(dialog) = navigation.library_name_dialog.as_mut().filter(|dialog| dialog.input.entity_id() == input_id) {
                        match estimate {
                            Ok((local_books, local_bytes, local_unavailable)) => dialog.estimate = Some((books + local_books, bytes.saturating_add(local_bytes), unavailable + local_unavailable)),
                            Err(error) => {
                                dialog.error = Some(error.into());
                                dialog.estimate = Some((books, bytes, unavailable + 1));
                            }
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn libraries(&self, cx: &gpui::App) -> Vec<LibraryChoice> {
        let mut choices = self.libraries.read(cx).entries().iter().map(|library| LibraryChoice { id: *library.library_id(), name: library.library_name().to_owned().into() }).collect::<Vec<_>>();
        choices.sort_by_cached_key(|library| library.name.to_lowercase());
        choices
    }

    pub(super) fn active_library_name(&self, cx: &gpui::App) -> SharedString {
        let libraries = self.libraries.read(cx);
        libraries.entries().iter().find(|library| Some(*library.library_id()) == libraries.selected_id()).map(|library| library.library_name().to_owned().into()).unwrap_or_else(|| "Libraries".into())
    }

    fn render_library_switcher_overlay(&self, emitter: Entity<Self>, window: &gpui::Window, cx: &gpui::App) -> gpui::AnyElement {
        let theme = components::browser_theme(cx);
        let active_library_id = self.libraries.read(cx).selected_id();
        let libraries = self.libraries(cx);
        let library_count = libraries.len();
        let cursor = self.switcher_cursor;
        let add = emitter.clone();
        let keys = emitter.clone();
        let mut header = components::library_switcher_header("Libraries", theme);
        #[cfg(not(target_arch = "arm"))]
        {
            header = header.child(components::library_switcher_add_button(matches!(cursor, SwitcherCursor::Add), theme).on_click(move |_, _, cx| {
                add.update(cx, |navigation, cx| {
                    navigation.switcher_open = false;
                    cx.notify();
                    navigation.choose_library_folders(cx);
                });
            }));
        }
        let mut panel = components::library_switcher_panel()
            .track_focus(&self.switcher_focus)
            .on_key_down(move |event, window, cx| {
                keys.update(cx, |navigation, cx| navigation.move_switcher_cursor(event, window, cx));
            })
            .child(header);

        for (index, library) in libraries.into_iter().enumerate() {
            let library_id = library.id;
            let selected = active_library_id == Some(library_id);
            let select = emitter.clone();
            let select_id = library_id;
            let manage = emitter.clone();
            panel = panel.child(components::library_switcher_row(
                components::library_switcher_item(format!("library-switcher-{library_id}"), library.name.clone(), selected, cursor == SwitcherCursor::Library { index, menu: false }, theme).on_click(move |_, _window, cx| {
                    select.update(cx, |navigation, cx| {
                        navigation.switcher_open = false;
                        cx.notify();
                        navigation.libraries.update(cx, |libraries, cx| libraries.select_library(select_id, cx));
                    });
                }),
                gpui::div()
                    .flex_none()
                    .child(
                        components::library_switcher_menu_button(format!("library-switcher-menu-{library_id}"), library.name, cursor == SwitcherCursor::Library { index, menu: true }, theme)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                manage.update(cx, |navigation, cx| {
                                    navigation.switcher_cursor = SwitcherCursor::Library { index, menu: true };
                                    navigation.open_library_menu(library_id, window, cx);
                                });
                            }),
                    )
                    .children(
                        self.library_menu
                            .as_ref()
                            .filter(|(id, _, _)| *id == library_id)
                            .map(|(_, menu, _)| gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(gpui::px(8.0)).child(menu.clone())).with_priority(3)),
                    ),
            ));
        }

        let library_name_dialog = self.render_library_name_dialog(emitter.clone(), cx);
        let dismiss = emitter;
        components::absolute_full_size()
            .child(
                gpui::deferred(
                    components::modal_overlay()
                        .occlude()
                        .child(components::modal_scrim("library-switcher-scrim", theme).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            dismiss.update(cx, |navigation, cx| navigation.close_library_switcher(window, cx));
                        }))
                        .child(
                            components::navigation_modal_list(library_count, window.rem_size(), theme)
                                .pt(gpui::px(components::SPACE_MD))
                                .pr(gpui::px(components::SPACE_MD))
                                .pb(gpui::px(components::SPACE_MD))
                                .pl(gpui::px(components::SPACE_MD - components::SPACE_SM))
                                .child(panel),
                        ),
                )
                .with_priority(1),
            )
            .children(library_name_dialog)
            .into_any_element()
    }

    pub(super) fn render_library_switcher_trigger(&self, emitter: Entity<Self>, cx: &gpui::App) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        components::library_switcher_group(theme).child(components::library_switcher_trigger(self.active_library_name(cx), theme).on_click(move |_, window, cx| {
            emitter.update(cx, |navigation, cx| navigation.open_library_switcher(window, cx));
        }))
    }
}
