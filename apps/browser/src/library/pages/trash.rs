//! Library Trash. Deleted items remain recoverable here until explicitly purged.

use gpui::prelude::*;
use gpui::{Context, FocusHandle, Focusable, IntoElement, Render, SharedString, Subscription, Window, div, px};
use gpui_component::IconName;
use library_model::{LibraryTrashView, TrashedBook, TrashedFolder};
use ui_components as components;

use crate::library::pages::section_empty_state;
use crate::library::services::{LatestRequest, LibraryContentsChanged, LibraryContext};

pub(crate) struct TrashPage {
    ui: LibraryContext,
    contents: Option<LibraryTrashView>,
    error: Option<SharedString>,
    request: LatestRequest,
    focus: FocusHandle,
    _updates: Subscription,
}

impl TrashPage {
    pub(crate) fn new(ui: LibraryContext, cx: &mut Context<Self>) -> Self {
        let updates = ui.updates();
        let update_subscription = cx.subscribe(&updates, |page, _, _: &LibraryContentsChanged, cx| page.refresh(cx));
        let mut page = Self { ui, contents: None, error: None, request: LatestRequest::default(), focus: cx.focus_handle().tab_index(0).tab_stop(true), _updates: update_subscription };
        page.refresh(cx);
        page
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let generation = self.request.begin();
        let library = self.ui.backend().clone();
        let load = async move { library.trash().await };
        let task = cx.spawn(async move |page, cx| {
            let result = load.await;
            cx.defer_update(page, move |page, cx| {
                if !page.request.is_current(generation) {
                    return;
                }
                match result {
                    Ok(contents) => {
                        page.contents = Some(contents);
                        page.error = None;
                    }
                    Err(error) => page.error = Some(error.into()),
                }
                cx.notify();
            });
        });
        self.request.install(task);
    }

    fn finish(&mut self, result: Result<(), String>, action: &str, cx: &mut Context<Self>) {
        match result {
            Ok(()) => self.refresh(cx),
            Err(error) => {
                self.error = Some(format!("Could not {action}: {error}").into());
                cx.notify();
            }
        }
    }

    fn restore_book(&mut self, book: TrashedBook, cx: &mut Context<Self>) {
        let content_hash = book.content_hash;
        let library = self.ui.backend().clone();
        let operation = async move { library.restore_book(content_hash).await };
        cx.spawn(async move |page, cx| {
            let result = operation.await;
            cx.defer_update(page, |page, cx| page.finish(result, "restore book", cx));
        })
        .detach();
    }

    fn restore_folder(&mut self, folder: TrashedFolder, cx: &mut Context<Self>) {
        let folder_id = folder.id;
        let library = self.ui.backend().clone();
        let operation = async move { library.restore_directory(folder_id, None).await };
        cx.spawn(async move |page, cx| {
            let result = operation.await.map(|_| ());
            cx.defer_update(page, |page, cx| page.finish(result, "restore folder", cx));
        })
        .detach();
    }

    fn purge_book(&mut self, book: TrashedBook, cx: &mut Context<Self>) {
        let content_hash = book.content_hash;
        let library = self.ui.backend().clone();
        let operation = async move { library.purge_book(content_hash).await };
        cx.spawn(async move |page, cx| {
            let result = operation.await;
            cx.defer_update(page, |page, cx| page.finish(result, "delete book permanently", cx));
        })
        .detach();
    }

    fn purge_folder(&mut self, folder: TrashedFolder, cx: &mut Context<Self>) {
        let folder_id = folder.id;
        let library = self.ui.backend().clone();
        let operation = async move { library.purge_directory(folder_id).await };
        cx.spawn(async move |page, cx| {
            let result = operation.await;
            cx.defer_update(page, |page, cx| page.finish(result, "delete folder permanently", cx));
        })
        .detach();
    }

    fn empty(&mut self, cx: &mut Context<Self>) {
        let library = self.ui.backend().clone();
        let operation = async move { library.empty_trash().await };
        cx.spawn(async move |page, cx| {
            let result = operation.await;
            cx.defer_update(page, |page, cx| page.finish(result, "empty Trash", cx));
        })
        .detach();
    }

    fn render_book(&self, book: &TrashedBook, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let restore_book = book.clone();
        let purge_book = book.clone();
        let restore = cx.entity();
        let purge = restore.clone();
        components::browser_settings_row(
            book.title.clone(),
            if book.format.is_empty() { "Book".to_owned() } else { book.format.to_uppercase() },
            div()
                .flex()
                .gap(px(8.0))
                .child(components::topbar_action_button(format!("restore-book-{}", book.content_hash), "Restore", IconName::Undo2, theme).on_click(move |_, _, cx| {
                    restore.update(cx, |page, cx| page.restore_book(restore_book.clone(), cx));
                }))
                .child(components::topbar_action_button(format!("purge-book-{}", book.content_hash), "Delete permanently", IconName::WindowClose, theme).on_click(move |_, _, cx| {
                    purge.update(cx, |page, cx| page.purge_book(purge_book.clone(), cx));
                })),
            theme,
        )
    }

    fn render_folder(&self, folder: &TrashedFolder, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::Div {
        let restore_folder = folder.clone();
        let purge_folder = folder.clone();
        let restore = cx.entity();
        let purge = restore.clone();
        components::browser_settings_row(
            folder.name.clone(),
            "Folder",
            div()
                .flex()
                .gap(px(8.0))
                .child(components::topbar_action_button(format!("restore-folder-{}", folder.id), "Restore", IconName::Undo2, theme).on_click(move |_, _, cx| {
                    restore.update(cx, |page, cx| page.restore_folder(restore_folder.clone(), cx));
                }))
                .child(components::topbar_action_button(format!("purge-folder-{}", folder.id), "Delete permanently", IconName::WindowClose, theme).on_click(move |_, _, cx| {
                    purge.update(cx, |page, cx| page.purge_folder(purge_folder.clone(), cx));
                })),
            theme,
        )
    }
}

impl Render for TrashPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let empty = self.contents.as_ref().is_some_and(|contents| contents.books.is_empty() && contents.folders.is_empty());
        let page = cx.entity();
        let heading = if components::uses_mobile_navigation(_window) {
            let back = (!components::has_native_back_button()).then(|| components::mobile_back_button(theme).on_click(|_, window, cx| window.dispatch_action(Box::new(crate::DismissSettings), cx)));
            components::mobile_navigation_heading("Trash", back)
        } else {
            components::browser_topbar_left().child(div().text_xl().child("Trash"))
        };
        let header = components::browser_topbar(theme).child(heading).child(components::browser_topbar_right().when(!empty, |actions| {
            actions.child(components::topbar_action_button("empty-trash", "Empty Trash", IconName::WindowClose, theme).on_click(move |_, _, cx| {
                page.update(cx, |page, cx| page.empty(cx));
            }))
        }));
        let mut body = components::browser_settings_page();
        match self.contents.as_ref() {
            None => body = body.child(components::browser_loading_message("Loading Trash…", theme)),
            Some(_) if empty => body = body.child(section_empty_state(theme)),
            Some(contents) => {
                let mut sections = components::browser_settings_sections();
                if !contents.folders.is_empty() {
                    sections = sections.child(components::browser_settings_fieldset("Folders", theme).children(contents.folders.iter().map(|folder| self.render_folder(folder, theme, cx))));
                }
                if !contents.books.is_empty() {
                    sections = sections.child(components::browser_settings_fieldset("Books", theme).children(contents.books.iter().map(|book| self.render_book(book, theme, cx))));
                }
                body = body.child(sections);
            }
        }
        let body = if let Some(error) = self.error.clone() {
            let retry = components::topbar_action_button("retry-section", "Retry", IconName::Redo2, theme).on_click({
                let page = cx.entity();
                move |_, _, cx| page.update(cx, |page, cx| page.refresh(cx))
            });
            components::browser_error_banner(error, retry, body.into_any_element()).into_any_element()
        } else {
            body.into_any_element()
        };
        components::library_section_content().track_focus(&self.focus).child(header).child(body).into_any_element()
    }
}

impl Focusable for TrashPage {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}
