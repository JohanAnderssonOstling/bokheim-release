//! EPUB search controls and virtualized, chapter-grouped excerpts.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, Modifiers, Window, div, list, px};
use gpui_component::scroll::{Scrollbar, ScrollbarAxis};
use skrift_column_paginator::RendererCommand;
use ui_components as components;

use crate::epub::ReaderView;
use crate::epub::search::SearchRow;
use crate::shell::search;
use crate::shell::toolbar::SidebarTarget;

impl ReaderView {
    pub(super) fn render_search(&self, theme: components::BrowserTheme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let reader = cx.entity().downgrade();
        let results = list(self.search.list.clone(), move |row, _, cx| {
            let Some(reader) = reader.upgrade() else { return div().into_any_element() };
            // List callbacks run during layout. Only read the reader here;
            // input handlers perform updates after layout has finished.
            reader.read(cx).render_search_row(row, &reader, theme)
        })
        .size_full()
        .pr(px(components::READER_SCROLLBAR_GUTTER_PX));
        let scroll = self.search.list.clone();
        div()
            .id("epub-search-panel")
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .on_key_down(move |event, window, cx| {
                if event.keystroke.modifiers != Modifiers::default() {
                    return;
                }
                let direction = match event.keystroke.key.as_str() {
                    "left" | "pageup" => -1.0,
                    "right" | "pagedown" => 1.0,
                    _ => return,
                };
                if scroll.max_offset_for_scrollbar().y > px(0.0) {
                    scroll.scroll_by(scroll.viewport_bounds().size.height * direction);
                    window.refresh();
                    cx.stop_propagation();
                }
            })
            .child(div().flex_none().child(search::search_bar(&self.search_input, theme, window, cx)))
            .child(div().id("epub-search-results").relative().flex_1().min_h_0().min_w_0().bg(theme.page_bg).child(results).child(Scrollbar::new(&self.search.list).axis(ScrollbarAxis::Vertical)))
            .into_any_element()
    }

    fn render_search_row(&self, row: usize, reader: &Entity<Self>, theme: components::BrowserTheme) -> AnyElement {
        match self.search.rows.get(row) {
            Some(SearchRow::Heading(title)) => components::reader_annotation_group_heading(title.clone(), theme).into_any_element(),
            Some(SearchRow::Match(index)) => {
                let index = *index;
                let Some(result) = self.search.results.get(index) else { return div().into_any_element() };
                let selected = self.search.match_position.is_some_and(|(current, _)| current == index + 1);
                let request = self.search.current_request();
                let reader = reader.downgrade();
                search::result_row(("epub-search-hit", index), result.excerpt.clone(), self.search.query.as_deref().unwrap_or_default(), self.search.options, selected, theme)
                    .on_click(move |_, window, cx| {
                        let Some(reader) = reader.upgrade() else { return };
                        reader.update(cx, |reader, cx| {
                            if !reader.search.is_current(request) || index >= reader.search.results.len() {
                                return;
                            }
                            reader.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::NavigateSearchTo(index), cx));
                            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
                            reader.tts_navigation_changed(cx);
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

#[cfg(test)]
mod tests {
    use crate::epub::search::search_groups;
    use crate::epub::toc::TocState;
    use skrift_column_paginator::TocEntry;
    use skrift_column_paginator::{BookSearchMatch, BookSearchTarget};

    #[test]
    fn excerpts_group_nested_sections_by_chapter_and_keep_unmapped_documents_in_order() {
        let entries = vec![
            TocEntry { title: "First".into(), link: "first".into(), children: vec![TocEntry { title: "Section".into(), link: "section".into(), children: vec![] }] },
            TocEntry { title: "Second".into(), link: "second".into(), children: vec![] },
        ];
        let mut toc = TocState::default();
        let resolutions = TocState::resolve(&entries, |link| match link {
            "first" => Some((1, None)),
            "section" => Some((1, Some("section".into()))),
            "second" => Some((2, None)),
            _ => None,
        });
        toc.set(entries, resolutions);
        toc.install_source_positions([(1, vec![(vec![2, 4, 2], 1)])].into(), 3);
        let results = vec![
            BookSearchMatch { doc: 0, target: BookSearchTarget::LocalOccurrence(0), excerpt: "Preface".into() },
            BookSearchMatch { doc: 1, target: BookSearchTarget::SourceCfi("epubcfi(/6/4!/2/4/2/1:0)".into()), excerpt: "Nested section".into() },
            BookSearchMatch { doc: 1, target: BookSearchTarget::LocalOccurrence(0), excerpt: "Chapter".into() },
            BookSearchMatch { doc: 2, target: BookSearchTarget::SourceCfi("epubcfi(/6/6!/2/4/2/1:0)".into()), excerpt: "Second chapter".into() },
        ];
        assert_eq!(search_groups(&toc, &results), vec![(None, 0, vec![0]), (Some(0), 1, vec![1, 2]), (Some(2), 2, vec![3])]);
        assert_eq!(toc.title_for_ordinal(0).as_deref(), Some("First"));
    }
}
