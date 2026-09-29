//! EPUB contents, annotation, and settings sidebar tabs.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, IntoElement, Window};
use library_backend::ReaderAnnotation;
use ui_components as components;

use crate::epub::ReaderView;
use crate::invalidation::Component;
use crate::shell::state_panel::{self, SidebarTab};

impl ReaderView {
    fn render_sidebar_tabs(&self, active: SidebarTab, theme: components::BrowserTheme, cx: &mut Context<Self>) -> AnyElement {
        state_panel::reader_sidebar_tabs("reader-sidebar-tabs", &cx.entity(), active, true, theme, |tab, this, cx| {
            state_panel::select_tab(&mut this.sidebar, tab, cx);
            crate::invalidation::notify(cx, Component::ReaderShell, "sidebar_tab_changed");
        })
    }

    fn render_toc_body(&self, entity: Entity<Self>, theme: components::BrowserTheme, cx: &App) -> AnyElement {
        let active_link = self.toc.active_link();
        let toc = state_panel::reader_toc_tree(&self.toc_tree, &entity, active_link.as_deref(), theme, cx, |link, window, this: &mut Self, cx| {
            this.navigate_to(link.to_owned(), cx);
            this.focus_document(window, cx);
            #[cfg(feature = "kobo")]
            {
                this.sidebar.close();
                crate::invalidation::notify(cx, Component::ReaderShell, "toc_navigated");
            }
        });
        components::reader_toc(theme).child(toc).into_any_element()
    }

    fn render_annotations_body(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> AnyElement {
        components::reader_state_panel(theme)
            .children(state_panel::reader_state_rows(
                &cx.entity(),
                &self.marks,
                "Select text, then use Highlight to create an annotation.",
                theme,
                |annotation: &ReaderAnnotation| {
                    annotation
                        .toc_ordinal
                        .and_then(|ordinal| usize::try_from(ordinal).ok())
                        .and_then(|ordinal| self.toc.title_at_ordinal(ordinal).map(|title| (format!("chapter:{ordinal}"), title.to_owned())))
                        .unwrap_or_else(|| ("other".to_owned(), "Other annotations".to_owned()))
                },
                |id, window, this: &mut Self, cx| {
                    this.annotation_popup_anchor = None;
                    this.select_annotation(id, window, cx);
                },
                |annotation: &ReaderAnnotation, this: &mut Self, cx| this.navigate_to_cfi(annotation.anchor.epub_cfi_value().unwrap_or_default().to_owned(), cx),
                |id, this: &mut Self, cx| this.delete_annotation(id, cx),
            ))
            .into_any_element()
    }

    /// The tab strip and the active tab's body — the two pieces
    /// `shell::toolbar::reader_sidebar` places under its own header.
    pub(super) fn render_sidebar(&self, active: SidebarTab, entity: Entity<Self>, theme: components::BrowserTheme, window: &Window, cx: &mut Context<Self>) -> (AnyElement, AnyElement) {
        let tabs = self.render_sidebar_tabs(active, theme, cx);
        let body = match active {
            SidebarTab::Contents => self.render_toc_body(entity, theme, cx),
            SidebarTab::Annotations => self.render_annotations_body(theme, cx),
            SidebarTab::Settings => self.render_settings_panel(cx),
        };
        (tabs, body)
    }
}
