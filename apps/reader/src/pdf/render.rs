mod actions;
mod annotations;
mod init;
mod input;
mod navigation;
mod search;
mod settings;

use gpui::prelude::*;
use gpui::{App, ClickEvent, Context, Entity, FocusHandle, Focusable, Pixels, Point, Render, Subscription, Window};
use gpui_component::input::InputState;
use gpui_component::menu::ContextMenuExt;
use gpui_component::tree::TreeState;
use library_backend::LibraryClient;
use library_model::BookLocator;
use pdf_view_gpui::PdfView;
use ui_components as components;

use std::rc::Rc;

use crate::invalidation::Component;
use crate::pdf::state::{page_indicator, progress};
use crate::pdf::toc::PdfTocState;
use crate::shell::annotation_editor;
use crate::shell::chrome;
use crate::shell::context_sheet::{ContextAction, ContextActions};
use crate::shell::input::{self as shell_input, CommandTarget};
use crate::shell::marks::MarkSession;
use crate::shell::persistence::ReadingPositionWriter;
use crate::shell::state_panel::{SidebarState, SidebarTab};
use crate::shell::toolbar::{self as sidebar_chrome, SidebarContent, SidebarTarget};
use crate::{NextPage, PreviousPage};

/// One zoom step, in pixels per PDF point.
const ZOOM_STEP: f32 = 0.1;

pub(crate) struct PdfReaderView {
    #[cfg(feature = "kobo")]
    brightness: Entity<crate::shell::brightness::BrightnessControls>,
    locator: BookLocator,
    title: String,
    library: Rc<LibraryClient>,
    pdf: Entity<PdfView>,
    search_input: Entity<InputState>,
    /// Match position while search is open; `None` means the search bar is closed.
    search_match: Option<(usize, usize)>,
    toc_tree: Entity<TreeState>,
    toc: PdfTocState,
    sidebar: SidebarState,
    chrome: chrome::ChromeState,
    marks: MarkSession,
    annotation_note_input: Entity<InputState>,
    annotation_popup_anchor: Option<Point<Pixels>>,
    context_sheet: Option<ContextActions>,
    current_page: usize,
    page_count: usize,
    positions: ReadingPositionWriter,
    close_reader: crate::CloseReader,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for PdfReaderView {
    /// Focusing the reader means focusing the document.
    ///
    /// Navigation keys are bound in the `PdfReaderDocument` context, which only
    /// the page elements establish, so anything that focuses this view has to
    /// land on the document or the keyboard goes dead. Before a page has been
    /// laid out there is nothing to focus, so the shell stands in.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.page_count > 0 { self.pdf.read(cx).focus_handle() } else { self.focus.clone() }
    }
}

impl Render for PdfReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        components::sync_reader_rem_size(window, cx);
        crate::invalidation::record_render(Component::PdfShell);
        let theme = components::browser_theme(cx);
        let entity = cx.entity();
        let pdf_document_focus = self.pdf.read(cx).focus_handle();
        let outline_available = self.toc.has_entries();
        let search_visible = self.search_match.is_some();
        let mobile = components::uses_mobile_navigation(window);
        let chrome_visible = self.chrome.is_visible() || (!mobile && search_visible);
        let touch_input = cx.supports_touch_input() || mobile;
        // A persistent desktop split always shows a tab — there is no closed
        // state once the toolbar's toggle button is gone. The mobile/compact
        // overlay keeps the open-or-closed sheet behaviour instead.
        let overlay = chrome::sidebar_overlays_document(window);
        let active_tab = if overlay { self.sidebar.effective(outline_available) } else { Some(self.sidebar.effective_or_default(outline_available)) };
        let sidebar_visible = chrome_visible && active_tab.is_some();
        let mobile_search = if mobile && chrome_visible && !sidebar_visible { self.search_match.map(|position| crate::shell::search::search_bar(&entity, &self.search_input, position, theme)) } else { None };
        let toc_width = chrome::sidebar_width(sidebar_visible, window, cx);
        let available_width = (f32::from(window.viewport_size().width) - toc_width).max(0.0);
        self.pdf.update(cx, |pdf, cx| {
            pdf.set_available_width(available_width, cx);
        });
        let (visible_page_range, visible_column_count) = {
            let pdf = self.pdf.read(cx);
            (pdf.visible_page_range(), pdf.visible_page_count())
        };
        let page_indicator = page_indicator(visible_page_range, self.page_count);
        let progress = progress(visible_page_range, self.page_count);
        let section_title = self.active_outline_title().unwrap_or(&self.title).to_owned();

        let context_target = entity.clone();
        let pdf_document = components::reader_content().id("pdf-reader-document").child(self.pdf.clone()).on_click(cx.listener(|reader, event, window, cx| {
            if chrome::is_page_tap(event) {
                reader.toggle_mobile_controls(window, cx);
            }
        })).on_aux_click(cx.listener(|reader, event: &ClickEvent, window, cx| {
            if event.is_secondary() && components::uses_mobile_navigation(window) {
                reader.open_context_sheet(cx);
            }
        }));
        let pdf_document = if mobile {
            pdf_document.into_any_element()
        } else {
            pdf_document.context_menu(move |menu, _window, cx| context_target.read(cx).context_actions(&context_target, cx).popup(menu)).into_any_element()
        };
        let mut body = components::reader_document_body().id("pdf-reader-document-surface").child(pdf_document);
        if let Some(annotation) = self.marks.selected().cloned() {
            let viewport = window.viewport_size();
            // The passage as the page columns drew it, so the panel is placed
            // clear of the whole highlight rather than of the pointer that made
            // it — which is on its last line, and would leave the panel over
            // everything above. The pointer remains the fallback for a passage
            // that is not on screen.
            let passage = self.pdf.read(cx).passage_bounds().map(annotation_editor::Passage::from_bounds).or_else(|| self.annotation_popup_anchor.map(annotation_editor::Passage::from_point));
            let geometry = annotation_editor::geometry(passage, f32::from(viewport.width), f32::from(viewport.height), toc_width, components::reader_toolbar_size_px(window), visible_column_count);
            body = body.child(self.render_annotation_popup(annotation, geometry, cx));
        }
        self.chrome.sync_system_bars(chrome_visible, cx);
        let content = if sidebar_visible {
            let active = active_tab.expect("sidebar_visible implies an active tab");
            let tabs = self.render_sidebar_tabs(active, outline_available, theme, cx);
            let sidebar_body = match active {
                SidebarTab::Contents => self.render_toc_body(entity.clone(), theme, cx),
                SidebarTab::Annotations => self.render_annotations_body(theme, cx),
                SidebarTab::Settings => self.render_settings(cx),
            };
            let search_element = if mobile { None } else { self.search_match.map(|position| crate::shell::search::search_bar(&entity, &self.search_input, position, theme)) };
            let sidebar_content = SidebarContent::new(section_title, progress, page_indicator, Some(pdf_document_focus), tabs, sidebar_body);
            let sidebar = sidebar_chrome::reader_sidebar(self, &entity, sidebar_content, search_element, theme, window);
            chrome::with_sidebar(sidebar, body.into_any_element(), window, cx)
        } else {
            body.into_any_element()
        };

        // The page container is only focusable until the document has pages of
        // its own to hold the keyboard. A focusable container steals focus from
        // the document on any click that reaches it — the sidebar's background,
        // the margin around a page — and navigation is bound in the document's
        // key context, so the paging keys would go dead until the page was
        // clicked again. The container still sits on the dispatch path either
        // way, so `Reader` bindings keep firing.
        let document_ready = self.page_count > 0;
        let bottom_bar = (mobile && chrome_visible).then(|| {
            let tab_target = entity.clone();
            let on_select: std::rc::Rc<dyn Fn(SidebarTab, &mut Window, &mut App)> = std::rc::Rc::new(move |tab, window, cx| {
                tab_target.update(cx, |reader, cx| {
                crate::shell::state_panel::toggle_tab(&mut reader.sidebar, tab, outline_available, cx);
                reader.chrome.show(cx);
                if reader.sidebar.is_visible() {
                    reader.focus_document(window, cx);
                }
                crate::invalidation::notify(cx, Component::PdfShell, "mobile_sidebar_tab_changed");
                });
            });
            let search_target = entity.clone();
            let on_search: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = std::rc::Rc::new(move |window, cx| search_target.update(cx, |reader, cx| reader.toggle_search(window, cx)));
            sidebar_chrome::reader_bottom_bar("pdf-reader-bottom-bar", active_tab, outline_available, search_visible, theme, on_select, on_search)
        });
        let page = crate::shell::page::reader_page(window, cx, theme, document_ready, &self.focus, chrome_visible, bottom_bar, mobile_search, content)
            .id("pdf-reader-page")
            // The reader deliberately uses hover-to-focus: returning the
            // pointer here immediately gives page keys back to the document.
            .on_mouse_move(cx.listener(|reader: &mut Self, _, window, cx| reader.reclaim_hover_focus(window, cx)));
        let interactions = chrome::surface_interactions("pdf", chrome_visible, sidebar_visible, touch_input, cx, |reader: &mut Self, action, window, cx| match action {
            chrome::SurfaceAction::PreviousPage => {
                reader.execute(&PreviousPage, window, cx);
            }
            chrome::SurfaceAction::NextPage => {
                reader.execute(&NextPage, window, cx);
            }
            chrome::SurfaceAction::ToggleChrome => reader.chrome.toggle(cx),
            chrome::SurfaceAction::ShowChrome => {
                if !reader.chrome.show_from_pointer(cx) {
                    return;
                }
                crate::shell::state_panel::select_tab(&mut reader.sidebar, SidebarTab::Contents, cx);
                crate::invalidation::notify(cx, Component::PdfShell, "sidebar_shown");
            }
        });
        let mut page = page.children(interactions);
        if let Some(actions) = &self.context_sheet {
            let dismiss = entity.clone();
            let sheet = actions.sheet(theme, window.insets().safe_area.bottom, move |_, cx| dismiss.update(cx, |reader, cx| {
                reader.context_sheet = None;
                cx.notify();
            }));
            page = page.child(sheet);
        }
        page
    }
}

impl PdfReaderView {
    fn toggle_mobile_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !components::uses_mobile_navigation(window) || self.sidebar.is_visible() && self.chrome.is_visible() {
            return;
        }
        if self.chrome.is_visible() {
            self.chrome.hide(cx);
            self.focus_document(window, cx);
        } else {
            self.sidebar.close();
            self.chrome.show_from_touch(cx);
        }
    }
}

pub(crate) fn bind_pdf_keys(cx: &mut App) {
    shell_input::bind_document_keys(cx, "PdfReaderDocument");
}

impl PdfReaderView {
    fn context_actions(&self, entity: &Entity<Self>, cx: &App) -> ContextActions {
        let mut actions = ContextActions::default();
        if self.pdf.read(cx).selection_snapshot().is_some() {
            let highlight = entity.clone();
            let copy = self.pdf.clone();
            actions.add_section(vec![
                ContextAction::new("Highlight", move |window, cx| highlight.update(cx, |reader, cx| reader.create_annotation(window, cx))),
                ContextAction::new("Copy", move |_, cx| { copy.update(cx, |pdf, cx| pdf.copy_selection(cx)); }),
            ]);
        }
        actions
    }

    fn open_context_sheet(&mut self, cx: &mut Context<Self>) {
        let actions = self.context_actions(&cx.entity(), cx);
        if !actions.is_empty() {
            self.context_sheet = Some(actions);
            self.chrome.hide(cx);
            cx.notify();
        }
    }
}
