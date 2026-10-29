//! PDF search visibility and renderer navigation.

use crate::shell::search;
use crate::shell::toolbar::SidebarTarget;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Window, div};
use skrift_column_paginator::SearchOptions;
use ui_components as components;

use super::PdfReaderView;
use crate::pdf::search::SearchRow;
use crate::shell::search::SearchBarTarget;
use crate::shell::sidebar_list;

impl SearchBarTarget for PdfReaderView {
    fn search_position(&self) -> (usize, usize) {
        self.search_match.unwrap_or((0, 0))
    }

    fn search_navigate(&mut self, direction: i8, cx: &mut Context<Self>) {
        self.pdf.update(cx, |pdf, cx| pdf.navigate_search(direction, cx));
    }
}

impl PdfReaderView {
    pub(super) fn render_search(&self, theme: components::BrowserTheme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = sidebar_list::panel("pdf-search-results", &cx.entity(), &self.search_list.list.state, theme, Self::render_search_row);
        div().size_full().min_h_0().min_w_0().flex().flex_col().child(div().flex_none().child(search::search_bar(&self.search_input, theme, window, cx))).child(rows).into_any_element()
    }

    fn render_search_row(&self, row: usize, target: &Entity<Self>, theme: components::BrowserTheme, cx: &App) -> AnyElement {
        match self.search_list.list.rows.get(row) {
            Some(SearchRow::Heading(page)) => components::reader_annotation_group_heading(format!("Page {}", page + 1), theme).into_any_element(),
            Some(SearchRow::Match(index)) => {
                let index = *index;
                let Some(hit) = self.pdf.read(cx).search_hits().get(index) else { return div().into_any_element() };
                let generation = self.search_list.generation;
                let target = target.downgrade();
                let selected = self.search_match.is_some_and(|(current, _)| current == index + 1);
                search::result_row(("pdf-search-hit", index), hit.excerpt().to_owned(), &self.search_input.read(cx).value(), SearchOptions { match_diacritics: true, ..Default::default() }, selected, theme)
                    .debug_selector(move || format!("pdf-search-hit-{index}"))
                    .on_click(move |_, window, cx| {
                        let _ = target.update(cx, |reader, cx| {
                            if reader.search_list.generation != generation || index >= reader.search_list.hit_count() {
                                return;
                            }
                            reader.pdf.update(cx, |pdf, cx| pdf.go_to_search_hit(index, cx));
                            if components::uses_mobile_reader_controls(window) {
                                reader.sidebar.close(cx);
                                reader.focus_document(window, cx);
                            } else {
                                reader.focus_sidebar_body(window, cx);
                            }
                        });
                    })
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}
