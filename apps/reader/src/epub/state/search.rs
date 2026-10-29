//! EPUB search visibility, debounce, and whole-book scanning.

use crate::invalidation::notify;
use std::time::Duration;

use gpui::Context;
use skrift_column_paginator::{RendererCommand, SearchScope};

use crate::epub::ReaderView;
use crate::invalidation::Component;
use crate::shell::search::SearchBarTarget;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

impl ReaderView {
    pub(in crate::epub) fn open_search(&mut self, cx: &mut Context<Self>) {
        if self.search.query.is_some() {
            return;
        }
        self.search.query = Some(self.search_input.read(cx).value().to_string());
        self.with_renderer(cx, |renderer, cx| {
            renderer.apply(RendererCommand::ActivateSearch, cx);
        });
        notify(cx, Component::ReaderShell, "search_visibility_changed");
    }

    pub(super) fn queue_search(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(request) = self.search.set_query(query) else { return };
        let timer = cx.background_executor().timer(SEARCH_DEBOUNCE);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.search.is_current(request) {
                    this.apply_pending_search(cx);
                }
            });
        })
        .detach();
    }

    /// A local result's occurrence index belongs to the old document. Drop it
    /// before queuing the current query for the newly loaded chapter.
    pub(super) fn refresh_chapter_search(&mut self, cx: &mut Context<Self>) {
        if self.search.options.scope != SearchScope::CurrentDocument {
            return;
        }
        let Some(query) = self.search.query.clone() else { return };
        let options = self.search.options;
        self.search.clear_results();
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetSearch { query: String::new(), options }, cx));
        self.queue_search(query, cx);
    }

    pub(super) fn apply_pending_search(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((renderer_query, options)) = self.search.take_pending() else {
            return false;
        };
        self.search.clear_results();
        if options.scope == SearchScope::CurrentDocument || renderer_query.is_empty() {
            self.with_renderer(cx, move |renderer, cx| renderer.apply(RendererCommand::SetSearch { query: renderer_query, options }, cx));
            return true;
        }
        let request = self.search.current_request();
        let provider = self.book.provider.clone();
        let documents = self.book.document_uris().to_vec();
        let cancellation = self.search.cancellation.clone();
        let executor = cx.background_executor().clone();
        self.with_renderer(cx, |renderer, cx| {
            renderer.apply(RendererCommand::SetSearchResults { query: renderer_query.clone(), options, results: Vec::new() }, cx);
        });
        cx.spawn(async move |this, cx| {
            let query = renderer_query.clone();
            let results = executor.spawn(async move { skrift_column_paginator::search_publication_streaming(provider.as_ref(), &documents, &query, options, || cancellation.load(std::sync::atomic::Ordering::Acquire) != request, |_| {}) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.search.still_wants(request, &renderer_query) {
                    this.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetSearchResults { query: renderer_query, options, results }, cx));
                }
            });
        })
        .detach();
        true
    }
}

impl SearchBarTarget for ReaderView {
    fn search_position(&self) -> (usize, usize) {
        self.search.match_position.unwrap_or((0, 0))
    }

    fn search_navigate(&mut self, direction: i8, cx: &mut Context<Self>) {
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::NavigateSearch(direction), cx));
    }
}
