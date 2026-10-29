//! EPUB table-of-contents, focus, and document navigation.

use gpui::{App, Context, FocusHandle, Window};
use gpui_component::tree::TreeItem;
use skrift_column_paginator::{RendererCommand, TocEntry};
use html_view_gpui::HtmlView;

use crate::epub::{LoadState, ReaderView};
use crate::invalidation::Component;

impl ReaderView {
    pub(super) fn load_database_toc(&self, cx: &mut Context<Self>) {
        let library = self.library.clone();
        let book = self.book.clone();
        let content_hash = self.locator.content_hash();
        cx.spawn(async move |this, cx| {
            let (entries, read_status) = match library.book_detail(content_hash).await {
                Ok(detail) => (if detail.toc.is_empty() { to_content_entries(book.toc()) } else { to_epub_entries(&detail.toc) }, detail.chapter_read_status),
                Err(error) => {
                    log::warn!("could not load EPUB table of contents from library database: {error}");
                    (to_content_entries(book.toc()), Default::default())
                }
            };
            let (entries, resolutions, positions) = cx
                .background_executor()
                .spawn(async move {
                    let resolutions = crate::epub::toc::TocState::resolve(&entries, |href| crate::epub::toc::TocState::resolve_publication_href(href, book.document_uris(), book.provider.as_ref()));
                    let positions = crate::epub::toc::TocState::source_positions(&resolutions, book.document_uris(), book.provider.as_ref());
                    (entries, resolutions, positions)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let renderer = match &this.load_state {
                    LoadState::Ready(renderer) => renderer.clone(),
                    LoadState::Loading | LoadState::Error(_) => return,
                };
                let anchors = crate::epub::toc::TocState::anchor_strings_by_doc(&resolutions, this.book.document_uris().len());
                this.toc.read_status = read_status;
                this.set_toc(entries, resolutions, positions, cx);
                renderer.update(cx, |renderer, cx| renderer.set_toc_anchor_strings_by_doc(anchors, cx));
            });
        })
        .detach();
    }

    pub(super) fn set_toc(&mut self, entries: Vec<TocEntry>, resolutions: Vec<crate::epub::toc::ResolvedTocEntry>, positions: crate::epub::toc::SourcePositions, cx: &mut Context<Self>) {
        let tree_items = Self::toc_tree_items(&entries);
        self.toc_tree.update(cx, |tree, cx| tree.set_items(tree_items, cx));
        self.toc.set(entries, resolutions);
        self.toc.install_source_positions(positions, self.book.document_uris().len());
        self.toc.update_read_status(self.progress.fraction >= 1.0);
        self.search.rebuild_rows(&self.toc);
        self.sync_toc_position(cx);
        self.record_current_reading_position(cx);
        crate::invalidation::notify(cx, Component::ReaderShell, "toc_loaded");
    }

    pub(super) fn sync_toc_position(&self, cx: &mut Context<Self>) {
        let next = self.toc.active_link();
        self.toc_tree.update(cx, |tree, cx| {
            let selected = next.map(|link| TreeItem::new(link.clone(), link));
            tree.set_selected_item(selected.as_ref(), cx);
            if let Some(selected) = selected {
                tree.reveal_item(&selected.id, gpui::ScrollStrategy::Center, cx);
            }
        });
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

    pub(in crate::epub) fn navigate_to(&mut self, href: String, cx: &mut Context<Self>) {
        self.page_turn = None;
        self.swipe_preview.clear();
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::NavigateToHref(href), cx));
        #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
        self.tts_navigation_changed(cx);
    }

    pub(in crate::epub) fn navigate_from_footnote(&mut self, cx: &mut Context<Self>) {
        let Some((preview, _)) = self.footnote.take() else { return };
        self.navigate_to(preview.href, cx);
        crate::invalidation::notify(cx, Component::ReaderShell, "footnote_navigated");
    }

    pub(in crate::epub) fn spine_title(&self) -> String {
        self.toc.active_title().or_else(|| (self.title != self.book_title).then(|| self.title.clone())).unwrap_or_default()
    }
}

fn to_epub_entries(entries: &[book_model::BookTocEntry]) -> Vec<TocEntry> {
    entries.iter().map(|entry| TocEntry { title: entry.title.clone(), link: entry.target.clone(), children: to_epub_entries(&entry.children) }).collect()
}

fn to_content_entries(entries: &[book_content::TocEntry]) -> Vec<TocEntry> {
    entries.iter().map(|entry| TocEntry { title: entry.title.clone(), link: entry.link.clone(), children: to_content_entries(&entry.children) }).collect()
}
