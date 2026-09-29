//! EPUB table-of-contents, focus, and document navigation.

use gpui::{App, Context, FocusHandle, Window};
use gpui_component::tree::TreeItem;
use html_view_core::{RendererCommand, TocEntry};
use html_view_gpui::HtmlView;

use crate::epub::{LoadState, ReaderView};
use crate::invalidation::Component;

impl ReaderView {
    pub(super) fn load_database_toc(&self, cx: &mut Context<Self>) {
        let library = self.library.clone();
        let content_hash = self.locator.content_hash();
        cx.spawn(async move |this, cx| {
            match library.book_detail(content_hash).await {
                Ok(detail) => {
                    let _ = this.update(cx, |this, cx| {
                        let renderer = match &this.load_state {
                            LoadState::Ready(renderer) => renderer.clone(),
                            LoadState::Loading | LoadState::Error(_) => return,
                        };
                        let toc = to_epub_entries(&detail.toc);
                        let resolutions = {
                            let renderer = renderer.read(cx);
                            crate::epub::toc::TocState::resolve(&toc, |href| renderer.resolve_href(href))
                        };
                        let anchors = crate::epub::toc::TocState::anchor_strings_by_doc(&resolutions, this.book.document_uris.len());
                        this.set_toc(toc, resolutions, cx);
                        renderer.update(cx, |renderer, cx| renderer.set_toc_anchor_strings_by_doc(anchors, cx));
                    });
                }
                Err(error) => log::warn!("could not load EPUB table of contents from library database: {error}"),
            }
        })
        .detach();
    }

    pub(super) fn set_toc(&mut self, entries: Vec<TocEntry>, resolutions: Vec<crate::epub::toc::ResolvedTocEntry>, cx: &mut Context<Self>) {
        let tree_items = Self::toc_tree_items(&entries);
        self.toc_tree.update(cx, |tree, cx| tree.set_items(tree_items, cx));
        self.toc.set(entries, resolutions);
    }

    fn toc_tree_items(entries: &[TocEntry]) -> Vec<TreeItem> {
        entries.iter().map(|entry| TreeItem::new(entry.link.clone(), entry.title.clone()).children(Self::toc_tree_items(&entry.children))).collect()
    }

    /// The document's focus handle, once the book has loaded.
    pub(in crate::epub) fn document_focus(&self, cx: &App) -> Option<FocusHandle> {
        match &self.load_state {
            LoadState::Ready(renderer) => Some(renderer.read(cx).focus_handle()),
            LoadState::Loading | LoadState::Error(_) => None,
        }
    }

    /// Returns keyboard control to the document after a chrome interaction.
    pub(in crate::epub) fn focus_document(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = self.document_focus(cx) {
            focus.focus(window, cx);
        }
    }

    pub(in crate::epub) fn with_renderer(&self, cx: &mut Context<Self>, update: impl FnOnce(&mut HtmlView, &mut Context<HtmlView>)) {
        if let LoadState::Ready(renderer) = &self.load_state {
            renderer.update(cx, update);
        }
    }

    pub(in crate::epub) fn copy_preview_image(&self, cx: &mut Context<Self>) {
        if let Some((_, image)) = &self.image_preview {
            cx.write_to_clipboard(gpui::ClipboardItem::new_image(image));
        }
    }

    pub(in crate::epub) fn navigate_to(&self, href: String, cx: &mut Context<Self>) {
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::NavigateToHref(href), cx));
    }

    pub(in crate::epub) fn navigate_from_footnote(&mut self, cx: &mut Context<Self>) {
        let Some((preview, _)) = self.footnote.take() else { return };
        self.navigate_to(preview.href, cx);
        crate::invalidation::notify(cx, Component::ReaderShell, "footnote_navigated");
    }

    pub(in crate::epub) fn display_title(&self) -> String {
        let chapter = self.toc.active_title().or_else(|| (self.title != self.book_title).then(|| self.title.clone()));
        match chapter {
            Some(chapter) => format!("{} • {chapter}", self.book_title),
            None => self.book_title.clone(),
        }
    }

    pub(in crate::epub) fn spine_title(&self) -> String {
        self.toc.active_title().or_else(|| (self.title != self.book_title).then(|| self.title.clone())).unwrap_or_default()
    }
}

fn to_epub_entries(entries: &[book_model::BookTocEntry]) -> Vec<TocEntry> {
    entries
        .iter()
        .map(|entry| TocEntry { title: entry.title.clone(), link: entry.target.clone(), children: to_epub_entries(&entry.children) })
        .collect()
}
