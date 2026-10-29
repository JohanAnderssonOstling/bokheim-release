//! Connected account selection and the provider folder picker.

use super::Sidebar;
use app::{CloudEntry, CloudFolder, CloudLibraryRoot, CloudProvider, GoogleAccount, ProviderId, ProviderScope};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Focusable, SharedString, Subscription, Window, div, px};
use gpui_component::{
    Disableable,
    input::{Input, InputEvent, InputState},
};
use ui_components as components;
use uuid::Uuid;

pub(super) struct CloudLibraryDialog {
    id: Uuid,
    #[cfg(target_arch = "wasm32")]
    login: Option<app::host::web::GoogleCloudLogin>,
    #[cfg(target_arch = "wasm32")]
    login_attempt: Option<app::host::web::GoogleLoginCancellation>,
    accounts: Vec<GoogleAccount>,
    account: Option<GoogleAccount>,
    breadcrumbs: Vec<CloudFolder>,
    children: Vec<CloudFolder>,
    next_page: Option<String>,
    name: Entity<InputState>,
    folder_name: Entity<InputState>,
    suggested_name: String,
    new_folder: bool,
    busy: bool,
    connecting: bool,
    saving: bool,
    error: Option<SharedString>,
    _subscriptions: [Subscription; 2],
}

impl CloudLibraryDialog {
    fn scope(&self) -> Option<ProviderScope> {
        Some(ProviderScope { provider: CloudProvider::GoogleDrive, account_id: self.account.as_ref()?.id.clone(), container_id: ProviderId::new("my_drive").expect("static drive scope") })
    }
}

impl Sidebar {
    pub(super) fn open_cloud_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !app::google_drive_available() {
            return;
        }
        self.switcher_open = false;
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Library name"));
        let folder_name = cx.new(|cx| InputState::new(window, cx).placeholder("New folder name"));
        let name_subscription = cx.subscribe_in(&name, window, |sidebar, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                sidebar.submit_cloud_library(cx);
            }
            cx.notify();
        });
        let folder_subscription = cx.subscribe_in(&folder_name, window, |sidebar, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                sidebar.create_cloud_folder(cx);
            }
            cx.notify();
        });
        let id = Uuid::new_v4();
        self.cloud_library_dialog = Some(CloudLibraryDialog {
            id,
            #[cfg(target_arch = "wasm32")]
            login: None,
            #[cfg(target_arch = "wasm32")]
            login_attempt: None,
            accounts: Vec::new(),
            account: None,
            breadcrumbs: Vec::new(),
            children: Vec::new(),
            next_page: None,
            name,
            folder_name,
            suggested_name: String::new(),
            new_folder: false,
            busy: true,
            connecting: false,
            saving: false,
            error: None,
            _subscriptions: [name_subscription, folder_subscription],
        });
        self.switcher_focus.focus(window, cx);
        let backend = self.libraries.read(cx).folder_creation_backend();
        cx.spawn(async move |sidebar, cx| {
            let accounts = backend.google_cloud_accounts().await;
            #[cfg(target_arch = "wasm32")]
            let login = app::host::web::GoogleCloudLogin::prepare(&backend).await;
            let _ = sidebar.update_in(cx, |sidebar, _, cx| {
                let Some(dialog) = sidebar.cloud_library_dialog.as_mut().filter(|dialog| dialog.id == id) else { return };
                dialog.busy = false;
                match accounts {
                    Ok(accounts) => dialog.accounts = accounts,
                    Err(error) => dialog.error = Some(error.to_string().into()),
                }
                #[cfg(target_arch = "wasm32")]
                match login {
                    Ok(login) => dialog.login = Some(login),
                    Err(error) => dialog.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn close_cloud_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cloud_library_dialog.as_ref().is_some_and(|dialog| dialog.saving) {
            return;
        }
        self.cloud_library_dialog = None;
        self.switcher_focus.focus(window, cx);
        cx.notify();
    }

    fn connect_google_cloud(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.cloud_library_dialog else { return };
        if dialog.busy {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        let attempt = match dialog.login.as_ref().ok_or_else(|| app::BackendError::message("Google Drive browser sign-in is not configured")).and_then(|login| login.begin()) {
            Ok(attempt) => {
                dialog.login_attempt = Some(attempt.cancellation());
                attempt
            }
            Err(error) => {
                dialog.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        dialog.busy = true;
        dialog.connecting = true;
        dialog.error = None;
        let id = dialog.id;
        let backend = self.libraries.read(cx).folder_creation_backend();
        cx.spawn(async move |sidebar, cx| {
            #[cfg(target_arch = "wasm32")]
            let result = attempt.finish(&backend).await;
            #[cfg(not(target_arch = "wasm32"))]
            let result = async {
                let login = backend.begin_google_cloud_login().await?;
                let opened = sidebar
                    .update_in(cx, |sidebar, _, cx| {
                        if sidebar.cloud_library_dialog.as_ref().is_some_and(|dialog| dialog.id == id) {
                            if let Some(url) = &login.authorization_url {
                                cx.open_url(url);
                            }
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !opened {
                    return Err(app::BackendError::message("Google sign-in was closed"));
                }
                backend.wait_google_cloud_login(login.attempt_id).await
            }
            .await;
            let _ = sidebar.update_in(cx, |sidebar, _, cx| {
                let Some(dialog) = sidebar.cloud_library_dialog.as_mut().filter(|dialog| dialog.id == id) else { return };
                #[cfg(target_arch = "wasm32")]
                {
                    dialog.login_attempt = None;
                }
                dialog.busy = false;
                dialog.connecting = false;
                let connected = match result {
                    Ok(account) => {
                        dialog.accounts.retain(|current| current.id != account.id);
                        dialog.accounts.push(account.clone());
                        dialog.account = Some(account);
                        true
                    }
                    Err(error) => {
                        dialog.error = Some(error.to_string().into());
                        false
                    }
                };
                if connected {
                    sidebar.open_cloud_folder(None, false, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_cloud_folder(&mut self, folder: Option<ProviderId>, more: bool, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.cloud_library_dialog else { return };
        if dialog.busy {
            return;
        }
        let Some(scope) = dialog.scope() else { return };
        let page = if more { dialog.next_page.clone() } else { None };
        dialog.busy = true;
        dialog.error = None;
        let id = dialog.id;
        let backend = self.libraries.read(cx).folder_creation_backend();
        cx.spawn(async move |sidebar, cx| {
            let result = backend.cloud_folders(scope, folder, page).await;
            let _ = sidebar.update_in(cx, |sidebar, window, cx| {
                let Some(dialog) = sidebar.cloud_library_dialog.as_mut().filter(|dialog| dialog.id == id) else { return };
                dialog.busy = false;
                match result {
                    Ok(listing) => {
                        if !more {
                            dialog.children.clear();
                            if let Some(index) = dialog.breadcrumbs.iter().position(|folder| folder.id == listing.folder.id) {
                                dialog.breadcrumbs.truncate(index + 1);
                            } else {
                                dialog.breadcrumbs.push(listing.folder.clone());
                            }
                            let current_name = dialog.name.read(cx).value().to_string();
                            if current_name.trim().is_empty() || current_name == dialog.suggested_name {
                                dialog.name.update(cx, |input, cx| input.set_value(listing.folder.name.clone(), window, cx));
                            }
                            dialog.suggested_name = listing.folder.name.clone();
                        }
                        dialog.children.extend(listing.page.entries.into_iter().filter_map(|entry| match entry {
                            CloudEntry::Folder(folder) => Some(folder),
                            _ => None,
                        }));
                        dialog.children.sort_by_key(|folder| folder.name.to_lowercase());
                        dialog.next_page = listing.page.next_page;
                    }
                    Err(error) => dialog.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn create_cloud_folder(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.cloud_library_dialog else { return };
        if dialog.busy || !dialog.new_folder {
            return;
        }
        let Some((scope, parent)) = dialog.scope().zip(dialog.breadcrumbs.last().map(|folder| folder.id.clone())) else { return };
        let name = dialog.folder_name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        dialog.busy = true;
        dialog.error = None;
        let id = dialog.id;
        let backend = self.libraries.read(cx).folder_creation_backend();
        cx.spawn(async move |sidebar, cx| {
            let result = backend.create_cloud_folder(scope, parent, name).await;
            let _ = sidebar.update_in(cx, |sidebar, _, cx| {
                let Some(dialog) = sidebar.cloud_library_dialog.as_mut().filter(|dialog| dialog.id == id) else { return };
                dialog.busy = false;
                match result {
                    Ok(folder) => {
                        dialog.new_folder = false;
                        sidebar.open_cloud_folder(Some(folder.id), false, cx);
                    }
                    Err(error) => dialog.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn submit_cloud_library(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.cloud_library_dialog else { return };
        if dialog.busy {
            return;
        }
        let Some((scope, folder)) = dialog.scope().zip(dialog.breadcrumbs.last()) else { return };
        let name = dialog.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let root = CloudLibraryRoot { scope, folder_id: folder.id.clone() };
        dialog.busy = true;
        dialog.saving = true;
        dialog.error = None;
        let id = dialog.id;
        let backend = self.libraries.read(cx).folder_creation_backend();
        cx.spawn(async move |sidebar, cx| {
            let result = backend.create_cloud_library(name, root).await;
            let _ = sidebar.update_in(cx, |sidebar, window, cx| {
                if !sidebar.cloud_library_dialog.as_ref().is_some_and(|dialog| dialog.id == id) {
                    return;
                }
                match result {
                    Ok(entry) => {
                        sidebar.cloud_library_dialog = None;
                        sidebar.libraries.update(cx, |libraries, cx| libraries.select_library(*entry.library_id(), cx));
                        sidebar.switcher_focus.focus(window, cx);
                    }
                    Err(error) => {
                        if let Some(dialog) = &mut sidebar.cloud_library_dialog {
                            dialog.busy = false;
                            dialog.saving = false;
                            dialog.error = Some(error.to_string().into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_cloud_library(&self, emitter: Entity<Self>, window: &Window, cx: &App) -> Option<AnyElement> {
        let dialog = self.cloud_library_dialog.as_ref()?;
        let theme = components::browser_theme(cx);
        let cancel = emitter.clone();
        let connect = emitter.clone();
        let submit = emitter.clone();
        let mut panel = components::modal_form_surface(theme)
            .id("cloud-library-dialog")
            .w(px(520.0))
            .child(div().text_color(theme.text).child("Add Google Drive library"));
        if let Some(account) = &dialog.account {
            let change = emitter.clone();
            panel = panel.child(div().flex().items_center().justify_between().gap(px(components::SPACE_SM)).child(div().text_color(theme.text).child(account.email.clone())).child(
                components::base_button("cloud-change-account").label("Change account").disabled(dialog.busy).on_click(move |_, _, cx| {
                    change.update(cx, |sidebar, cx| {
                        if let Some(dialog) = &mut sidebar.cloud_library_dialog {
                            dialog.account = None;
                            dialog.breadcrumbs.clear();
                            dialog.children.clear();
                            dialog.error = None;
                        }
                        cx.notify();
                    })
                }),
            ));
            let mut breadcrumbs = Vec::with_capacity(dialog.breadcrumbs.len());
            let mut breadcrumb_labels = Vec::with_capacity(dialog.breadcrumbs.len());
            for (index, folder) in dialog.breadcrumbs.iter().enumerate() {
                let current = index + 1 == dialog.breadcrumbs.len();
                let label = SharedString::from(folder.name.clone());
                let mut crumb = components::folder_breadcrumb_item(index, label.clone(), current, theme).when(dialog.busy, |crumb| crumb.cursor_default());
                if !current && !dialog.busy {
                    let select = emitter.clone();
                    let folder_id = folder.id.clone();
                    crumb = crumb.on_click(move |_, _, cx| select.update(cx, |sidebar, cx| sidebar.open_cloud_folder(Some(folder_id.clone()), false, cx)));
                }
                breadcrumbs.push(crumb);
                breadcrumb_labels.push(label);
            }
            panel = panel.child(components::folder_breadcrumb(breadcrumbs, breadcrumb_labels, theme));
            let mut folders = div().id("cloud-folder-list").flex().flex_col().max_h(px(260.0)).overflow_y_scroll().gap(px(components::SPACE_XS));
            for (index, folder) in dialog.children.iter().enumerate() {
                let select = emitter.clone();
                let folder_id = folder.id.clone();
                folders = folders.child(
                    components::base_button(("cloud-folder", index))
                        .label(format!("{}  ›", folder.name))
                        .w_full()
                        .disabled(dialog.busy)
                        .on_click(move |_, _, cx| select.update(cx, |sidebar, cx| sidebar.open_cloud_folder(Some(folder_id.clone()), false, cx))),
                );
            }
            if dialog.children.is_empty() && !dialog.busy {
                folders = folders.child(div().text_color(theme.text_muted).child("No subfolders. You can use this folder or create one."));
            }
            if dialog.next_page.is_some() {
                let more = emitter.clone();
                let folder = dialog.breadcrumbs.last().map(|folder| folder.id.clone());
                folders = folders.child(components::base_button("cloud-more-folders").label("More folders").disabled(dialog.busy).on_click(move |_, _, cx| more.update(cx, |sidebar, cx| sidebar.open_cloud_folder(folder.clone(), true, cx))));
            }
            panel = panel.child(folders);
            if dialog.new_folder {
                let create = emitter.clone();
                panel = panel.child(Input::new(&dialog.folder_name).disabled(dialog.busy)).child(
                    components::base_button("cloud-create-folder")
                        .label("Create folder")
                        .disabled(dialog.busy || dialog.folder_name.read(cx).value().trim().is_empty())
                        .on_click(move |_, _, cx| create.update(cx, |sidebar, cx| sidebar.create_cloud_folder(cx))),
                );
            } else {
                let create = emitter.clone();
                panel = panel.child(components::base_button("cloud-new-folder").label("New folder").disabled(dialog.busy || dialog.breadcrumbs.is_empty()).on_click(move |_, window, cx| {
                    create.update(cx, |sidebar, cx| {
                        if let Some(dialog) = &mut sidebar.cloud_library_dialog {
                            dialog.new_folder = true;
                            dialog.folder_name.focus_handle(cx).focus(window, cx);
                        }
                        cx.notify();
                    })
                }));
            }
            panel = panel.child(div().text_color(theme.text_muted).child("Library name")).child(Input::new(&dialog.name).disabled(dialog.busy));
        } else {
            panel = panel.child(div().text_color(theme.text_muted).child("Connect your account, then choose the folder that contains your books."));
            for (index, account) in dialog.accounts.iter().enumerate() {
                let select = emitter.clone();
                let account = account.clone();
                panel = panel.child(components::base_button(("cloud-account", index)).label(account.email.clone()).disabled(dialog.busy).on_click(move |_, _, cx| {
                    select.update(cx, |sidebar, cx| {
                        if let Some(dialog) = &mut sidebar.cloud_library_dialog {
                            dialog.account = Some(account.clone());
                            dialog.error = None;
                        }
                        sidebar.open_cloud_folder(None, false, cx);
                    })
                }));
            }
            panel = panel.child(
                components::base_button("cloud-connect-google")
                    .label(if dialog.connecting { "Waiting for Google sign-in…" } else { "Connect Google Drive" })
                    .disabled(dialog.busy)
                    .on_click(move |_, _, cx| connect.update(cx, |sidebar, cx| sidebar.connect_google_cloud(cx))),
            );
        }
        if dialog.busy && !dialog.connecting {
            panel = panel.child(div().text_color(theme.text_muted).child(if dialog.saving { "Adding library…" } else { "Loading folders…" }));
        }
        panel = panel.children(dialog.error.clone().map(|error| components::error_text(theme).child(error))).child(
            div()
                .flex()
                .justify_end()
                .gap(px(components::SPACE_SM))
                .child(components::dialog_cancel_button("cloud-library-cancel", "Cancel").disabled(dialog.saving).on_click(move |_, window, cx| cancel.update(cx, |sidebar, cx| sidebar.close_cloud_library(window, cx))))
                .child(
                    components::base_button("cloud-library-add")
                        .primary()
                        .label("Use this folder")
                        .disabled(dialog.busy || dialog.breadcrumbs.is_empty() || dialog.name.read(cx).value().trim().is_empty())
                        .on_click(move |_, _, cx| submit.update(cx, |sidebar, cx| sidebar.submit_cloud_library(cx))),
                ),
        );
        Some(
            components::anchor_to_window(
                components::modal_overlay().child(components::modal_scrim("cloud-library-scrim", theme).on_click(move |_, window, cx| emitter.update(cx, |sidebar, cx| sidebar.close_cloud_library(window, cx)))).child(panel),
                window,
            )
            .into_any_element(),
        )
    }
}
