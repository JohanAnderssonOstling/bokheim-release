//! Library creation, selection, and switcher overlay rendering.

use app::LibraryEntry;
use ui_components::user_facing_error;

use gpui::prelude::*;
use gpui::{Context, Entity, KeyDownEvent, MouseButton, PathPromptOptions, SharedString, Window};
use gpui::{Focusable, Subscription};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::PopupMenu;
use gpui_component::{Disableable, WindowExt};
use sync_common::LibraryId;
use ui_components::{self as components, ScrollableElement};

use super::{Sidebar, SwitcherCursor};

pub(super) struct LibraryNameDialog {
    id: Option<LibraryId>,
    folders: Option<Vec<gpui::SelectedDirectory>>,
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
        let Some(library) = self.libraries.read(cx).entries().iter().find(|library| *library.library_id() == id) else { return };
        self.begin_library_name(Some(id), library.library_name().to_owned().into(), window, cx);
    }

    pub(crate) fn open_create_library_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_library_name(None, SharedString::default(), window, cx);
    }

    fn begin_library_name(&mut self, id: Option<LibraryId>, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Library name"));
        if id.is_some() {
            input.update(cx, |input, cx| input.set_value(name, window, cx));
        }
        let subscription = cx.subscribe_in(&input, window, |navigation, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                navigation.submit_library_name(cx);
            }
            cx.notify();
        });
        input.focus_handle(cx).focus(window, cx);
        self.library_name_dialog = Some(LibraryNameDialog { id, folders: None, estimate: Some((0, 0, 0)), asset_storage_enabled: true, input, busy: false, error: None, _subscription: subscription });
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
            self.libraries.update(cx, |libraries, cx| libraries.create_selected_libraries(folders, enabled, cx));
            self.library_name_dialog = None;
            self.switcher_open = false;
            cx.notify();
            return;
        }
        let name = dialog.input.read(cx).value().trim().to_owned();
        if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
            let message = if name.is_empty() {
                "Enter a library name."
            } else if name.len() > 255 {
                "This library name is too long. Enter a shorter name."
            } else {
                "Remove line breaks, tabs, and other invisible characters from the library name."
            };
            dialog.error = Some(message.into());
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
                            dialog.error = Some(user_facing_error(&error, "Could not save the library changes. Please try again.").into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_library_name_dialog(&self, emitter: Entity<Self>, window: &gpui::Window, cx: &gpui::App) -> Option<gpui::AnyElement> {
        let dialog = self.library_name_dialog.as_ref()?;
        let theme = components::browser_theme(cx);
        let cancel = emitter.clone();
        let submit = emitter.clone();
        let panel = components::modal_form_surface(theme)
            .w(gpui::px(360.0))
            .child(gpui::div().text_color(theme.text).child(if dialog.id.is_none() { "Create library" } else { "Rename library" }))
            .child(Input::new(&dialog.input).disabled(dialog.busy || dialog.folders.is_some()))
            .children(dialog.folders.as_ref().filter(|folders| folders.iter().any(|folder| folder.local_path.is_none())).map(|_| gpui::div().child("Books will be copied into Bokheim. The originals will stay in the selected folder.")))
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
                            // Folder-import review disables the input, so its
                            // submit button has no Enter shortcut to advertise.
                            .when(dialog.folders.is_none(), |button| button.tooltip(components::shortcut_tooltip(if dialog.id.is_none() { "Create" } else { "Rename" }, "enter")))
                            .disabled(dialog.busy || dialog.estimate.is_none() || dialog.input.read(cx).value().trim().is_empty())
                            .on_click(move |_, _, cx| submit.update(cx, |navigation, cx| navigation.submit_library_name(cx))),
                    ),
            );
        // Keep this in the normal content tree. KoboKeyboardRoot paints its
        // keyboard after application content; a deferred modal would be
        // painted above the keyboard and make its keys appear disabled.
        // Anchoring is not deferral: it places the layer, it does not reorder
        // painting.
        // Do not dismiss this dialog from the full-window scrim.
        // On Kobo, keyboard taps are outside the panel bounds.
        Some(components::centered_modal(components::modal_scrim("library-rename-scrim", theme), panel, window).into_any_element())
    }

    pub(crate) fn is_library_switcher_open(&self) -> bool {
        self.switcher_open || self.library_name_dialog.is_some() || self.cloud_library_dialog.is_some() || self.add_library_choices.is_some()
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
        self.switcher_cursor = match self.libraries(cx).iter().position(|library| Some(*library.library_id()) == active) {
            Some(index) => SwitcherCursor::Library { index, menu: false },
            None => SwitcherCursor::Add,
        };
    }

    /// Keeps the cursor inside the list after a row disappears.
    fn clamp_switcher_cursor(&mut self, cx: &gpui::App) {
        let count = self.libraries.read(cx).entries().len();
        self.switcher_cursor = match self.switcher_cursor {
            SwitcherCursor::Library { .. } if count == 0 => SwitcherCursor::Add,
            SwitcherCursor::Library { index, menu } if index >= count => SwitcherCursor::Library { index: count - 1, menu },
            other => other,
        };
    }

    fn move_switcher_cursor(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.library_name_dialog.is_some() || self.library_menu.is_some() || self.add_library_choices.is_some() {
            return;
        }
        let count = self.libraries.read(cx).entries().len();
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
                self.open_add_library_choices(window, cx);
            }
            SwitcherCursor::Library { index, menu } => {
                let Some(id) = self.libraries(cx).get(index).map(|library| *library.library_id()) else { return };
                if menu {
                    self.open_library_menu(id, window, cx);
                } else {
                    self.switcher_open = false;
                    cx.notify();
                    self.libraries.update(cx, |libraries, cx| libraries.select_library(id, cx));
                }
            }
        }
    }

    /// Both pointer and keyboard menu actions use the same confirmation. It is
    /// the one command in the library that asks first: everything else lands
    /// in Trash or on the undo stack, and a deleted library does neither.
    fn prompt_delete_library(&mut self, id: LibraryId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(library) = self.libraries.read(cx).entries().iter().find(|library| *library.library_id() == id) else { return };
        let question: SharedString = format!("Delete “{}”?", library.library_name()).into();
        let navigation = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, window, cx| {
            let theme = components::browser_theme(cx);
            let navigation = navigation.clone();
            let cancel = components::dialog_cancel_button("cancel-delete-library", "Cancel").on_click(|_, window, cx| window.close_dialog(cx));
            let delete = components::base_button("delete-library").label("Delete").danger().on_click(move |_, window, cx| {
                window.close_dialog(cx);
                // The switcher stays open: deleting is a management action,
                // and there is usually more than one to do. Closing it also
                // threw the reader back to a page whose library might be the
                // one just removed.
                let _ = navigation.update(cx, |navigation, cx| {
                    navigation.libraries.update(cx, |libraries, cx| libraries.delete_library(id, cx));
                    navigation.clamp_switcher_cursor(cx);
                    cx.notify();
                });
            });
            components::confirmation_dialog_row(components::confirmation_dialog(dialog, window), question.clone(), cancel, delete, theme)
        });
    }

    pub(crate) fn close_library_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cloud_library_dialog.is_some() { self.close_cloud_library(window, cx); return; }
        if self.add_library_choices.is_some() {
            self.close_add_library_choices(window, cx);
            return;
        }
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
        let switcher = if self.switcher_open { Some(self.render_library_switcher_overlay(emitter.clone(), window, cx)) } else { self.render_library_name_dialog(emitter.clone(), window, cx) };
        let choices = self.render_add_library_overlay(emitter.clone(), window, cx);
        let cloud = self.render_cloud_library(emitter, window, cx);
        if switcher.is_none() && choices.is_none() && cloud.is_none() {
            return None;
        }
        Some(components::absolute_full_size().children(switcher).children(choices).children(cloud).into_any_element())
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
        for directory in &mut directories {
            retain_eligible_files(directory);
        }
        let (books, bytes, unavailable) = estimate_selected_files(&directories);
        let locators: Vec<String> = directories.iter().filter_map(|directory| directory.local_path.as_ref().map(|path| path.to_string_lossy().into_owned())).collect();
        let title = if directories.len() == 1 { directories[0].name.clone() } else { format!("{} library folders", directories.len()) };
        self.open_create_library_dialog(window, cx);
        self.switcher_open = false;
        let Some(dialog) = self.library_name_dialog.as_mut() else {
            log::error!("Library creation dialog could not be opened");
            return;
        };
        dialog.input.update(cx, |input, cx| input.set_value(title, window, cx));
        dialog.folders = Some(directories);
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
                                dialog.error = Some(user_facing_error(&error, "Could not check the selected folders. Check that they are available, then try again.").into());
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

    fn libraries<'a>(&self, cx: &'a gpui::App) -> Vec<&'a LibraryEntry> {
        let mut choices = self.libraries.read(cx).entries().iter().collect::<Vec<_>>();
        choices.sort_by_cached_key(|library| library.library_name().to_lowercase());
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
        let keys = emitter.clone();
        let header = components::library_switcher_header("Libraries", theme)
            .flex_none()
            .child(self.render_add_library_control(emitter.clone(), components::library_switcher_add_button(matches!(cursor, SwitcherCursor::Add), theme), true));
        let panel = components::library_switcher_panel()
            .flex_1()
            .min_h_0()
            .track_focus(&self.switcher_focus)
            .on_key_down(move |event, window, cx| {
                keys.update(cx, |navigation, cx| navigation.move_switcher_cursor(event, window, cx));
            })
            .child(header);
        let mut list = components::library_switcher_panel().flex_1().min_h_0().overflow_y_scrollbar_with_id("library-switcher-list").hide_scrollbar();

        for (index, library) in libraries.into_iter().enumerate() {
            let library_id = *library.library_id();
            let name: SharedString = library.library_name().to_owned().into();
            let selected = active_library_id == Some(library_id);
            let select = emitter.clone();
            let select_id = library_id;
            let manage = emitter.clone();
            let menu_was_open = self.library_menu.as_ref().is_some_and(|(id, _, _)| *id == library_id);
            list = list.child(components::library_switcher_row(
                components::library_switcher_item(format!("library-switcher-{library_id}"), name.clone(), selected, cursor == SwitcherCursor::Library { index, menu: false }, theme).on_click(move |_, _window, cx| {
                    select.update(cx, |navigation, cx| {
                        navigation.switcher_open = false;
                        cx.notify();
                        navigation.libraries.update(cx, |libraries, cx| libraries.select_library(select_id, cx));
                    });
                }),
                gpui::div()
                    .flex_none()
                    .child(
                        components::library_switcher_menu_button(format!("library-switcher-menu-{library_id}"), name, cursor == SwitcherCursor::Library { index, menu: true }, theme)
                            .debug_selector(move || format!("library-switcher-menu-{library_id}"))
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                cx.stop_propagation();
                                manage.update(cx, |navigation, cx| {
                                    navigation.switcher_cursor = SwitcherCursor::Library { index, menu: true };
                                    // Outside-click dismissal runs during capture. Use the
                                    // state from before this press so the trigger cannot reopen it.
                                    if menu_was_open {
                                        navigation.library_menu = None;
                                        navigation.switcher_focus.focus(window, cx);
                                        cx.notify();
                                    } else {
                                        navigation.open_library_menu(library_id, window, cx);
                                    }
                                });
                            })
                            .on_click(|_, _, cx| cx.stop_propagation()),
                    )
                    .children(
                        self.library_menu
                            .as_ref()
                            .filter(|(id, _, _)| *id == library_id)
                            .map(|(_, menu, _)| {
                                if cx.supports_touch_input() {
                                    gpui::deferred(gpui_component::popover::centered_overlay(menu.clone(), window, cx)).with_priority(3)
                                } else {
                                    gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopRight).snap_to_window_with_margin(gpui::px(8.0)).child(menu.clone())).with_priority(3)
                                }
                            }),
                    ),
            ));
        }

        let library_name_dialog = self.render_library_name_dialog(emitter.clone(), window, cx);
        let dismiss = emitter;
        let scrim = components::modal_scrim("library-switcher-scrim", theme).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, window, cx| {
            cx.stop_propagation();
            dismiss.update(cx, |navigation, cx| navigation.close_library_switcher(window, cx));
        });
        let panel = components::navigation_modal_list(library_count, window.rem_size(), theme).p(gpui::px(components::SPACE_MD)).child(panel.child(list));
        components::absolute_full_size().child(gpui::deferred(components::centered_modal(scrim, panel, window)).with_priority(1)).children(library_name_dialog).into_any_element()
    }

    pub(super) fn render_library_switcher_trigger(&self, emitter: Entity<Self>, cx: &gpui::App) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        components::library_switcher_group().child(components::library_switcher_trigger(self.active_library_name(cx), theme).on_click(move |_, window, cx| {
            emitter.update(cx, |navigation, cx| navigation.open_library_switcher(window, cx));
        }))
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use gpui::{AppContext, Modifiers, Render, TestAppContext, VisualTestContext, div};
    use std::rc::Rc;

    struct SwitcherHarness {
        sidebar: Entity<Sidebar>,
        _changes: Subscription,
    }

    impl Render for SwitcherHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().children(self.sidebar.read(cx).library_switcher_overlay(self.sidebar.clone(), window, cx))
        }
    }

    #[gpui::test]
    async fn more_button_closes_its_open_menu_and_can_open_another_library_menu(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, _) = app::AppClient::start_native(launch).unwrap();
        let first = backend.create_library_with_asset_storage("First".into(), true).await.unwrap();
        let second = backend.create_library_with_asset_storage("Second".into(), true).await.unwrap();
        let first_id = *first.library_id();
        let second_id = *second.library_id();
        let entries = backend.libraries().await.unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            components::init_page_scrolling(cx);
        });
        let backend = Rc::new(backend);
        let window = cx.add_window(|window, cx| {
            let libraries = cx.new(|cx| crate::navigation::Libraries::new(backend, Rc::new(|_, _, _, _, _| {}), entries, None, cx));
            let sidebar = cx.new(|cx| Sidebar::new(libraries, Rc::new(|_, _, _| {}), cx));
            sidebar.update(cx, |sidebar, cx| sidebar.open_library_switcher(window, cx));
            let changes = cx.observe(&sidebar, |_, _, cx| cx.notify());
            SwitcherHarness { sidebar, _changes: changes }
        });
        let sidebar = window.read_with(cx, |view, _| view.sidebar.clone()).unwrap();
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        let first_selector = Box::leak(format!("library-switcher-menu-{first_id}").into_boxed_str());
        let second_selector = Box::leak(format!("library-switcher-menu-{second_id}").into_boxed_str());
        let first_position = visual.debug_bounds(first_selector).unwrap().center();
        let second_position = visual.debug_bounds(second_selector).unwrap().center();
        let open_menu = |cx: &VisualTestContext| sidebar.read_with(cx, |sidebar, _| sidebar.library_menu.as_ref().map(|(id, _, _)| *id));

        visual.simulate_click(first_position, Modifiers::default());
        assert_eq!(open_menu(&visual), Some(first_id));
        visual.simulate_click(first_position, Modifiers::default());
        assert_eq!(open_menu(&visual), None, "the same press must close the menu without reopening it on mouse-up");
        visual.update(|window, cx| assert!(sidebar.read(cx).switcher_focus.is_focused(window)));

        visual.simulate_click(first_position, Modifiers::default());
        assert_eq!(open_menu(&visual), Some(first_id));
        visual.simulate_click(second_position, Modifiers::default());
        assert_eq!(open_menu(&visual), Some(second_id));
        visual.simulate_click(second_position, Modifiers::default());
        assert_eq!(open_menu(&visual), None);
    }
}
