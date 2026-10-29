//! PDF search query policy and compact sidebar row descriptions.

use gpui::px;

use crate::shell::sidebar_list::{SidebarList, reveal_row};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SearchRow {
    Heading(usize),
    Match(usize),
}

#[derive(Default)]
pub(super) struct SearchList {
    pub(super) list: SidebarList<SearchRow>,
    pub(super) generation: usize,
    hit_count: usize,
    last_page: Option<usize>,
}

impl SearchList {
    pub(super) fn clear(&mut self) {
        self.list = SidebarList::default();
        self.generation = self.generation.wrapping_add(1);
        self.hit_count = 0;
        self.last_page = None;
    }

    /// PDF results stream in reading order. Append descriptions for new hits
    /// without constructing elements for the preceding result pages.
    pub(super) fn extend_pages(&mut self, pages: impl IntoIterator<Item = usize>) {
        let start = self.list.rows.len();
        for page in pages {
            if self.last_page != Some(page) {
                self.last_page = Some(page);
                self.list.rows.push(SearchRow::Heading(page));
            }
            self.list.rows.push(SearchRow::Match(self.hit_count));
            self.hit_count += 1;
        }
        if self.list.rows.len() != start {
            self.list.state.splice(start..start, self.list.rows.len() - start);
            self.list.state.clone().with_uniform_item_height(px(80.0));
        }
    }

    pub(super) fn hit_count(&self) -> usize {
        self.hit_count
    }

    pub(super) fn reveal_match(&self, current: usize) {
        if let Some(row) = current.checked_sub(1).and_then(|index| self.list.rows.iter().position(|row| *row == SearchRow::Match(index))) {
            reveal_row(&self.list.state, row);
        }
    }
}

/// Searching starts at two Unicode characters. Shorter input still produces
/// an empty query so old results and in-flight work are cleared.
pub(super) fn renderer_query(input: &str) -> String {
    if input.chars().count() >= 2 { input.to_owned() } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use gpui::prelude::*;
    use gpui::{Context, Render, TestAppContext, Window, div};
    use skrift_column_paginator::SearchOptions;
    use ui_components as components;

    use crate::shell::{search, sidebar_list};

    #[test]
    fn streamed_results_keep_original_indices_and_one_heading_per_page() {
        let mut search = SearchList::default();
        search.extend_pages([0, 0, 2]);
        search.extend_pages([2, 3]);
        assert_eq!(search.list.rows, [SearchRow::Heading(0), SearchRow::Match(0), SearchRow::Match(1), SearchRow::Heading(2), SearchRow::Match(2), SearchRow::Match(3), SearchRow::Heading(3), SearchRow::Match(4)]);
        let generation = search.generation;
        search.clear();
        search.extend_pages([7]);
        assert_ne!(search.generation, generation);
        assert_eq!(search.hit_count(), 1);
        assert_eq!(search.list.rows, [SearchRow::Heading(7), SearchRow::Match(0)]);
    }

    struct SearchTest {
        search: SearchList,
        rendered: Cell<usize>,
        clicked: Option<usize>,
    }

    impl Render for SearchTest {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(320.0)).h(px(300.0)).child(sidebar_list::panel("test-pdf-search", &cx.entity(), &self.search.list.state, components::browser_theme(cx), |this, row, target, theme, _| {
                this.rendered.set(this.rendered.get() + 1);
                match this.search.list.rows.get(row) {
                    Some(SearchRow::Heading(page)) => components::reader_annotation_group_heading(format!("Page {}", page + 1), theme).into_any_element(),
                    Some(SearchRow::Match(index)) => {
                        let index = *index;
                        let target = target.downgrade();
                        search::result_row(("test-search-hit", index), "An excerpt containing the search term, spanning several lines at sidebar width.".into(), "search", SearchOptions::default(), false, theme)
                            .debug_selector(move || format!("test-search-hit-{index}"))
                            .on_click(move |_, _, cx| {
                                let _ = target.update(cx, |this, _| this.clicked = Some(index));
                            })
                            .into_any_element()
                    }
                    None => div().into_any_element(),
                }
            }))
        }
    }

    #[gpui::test]
    fn large_search_renders_a_bounded_window_and_navigates_to_distant_hits(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| {
            let mut search = SearchList::default();
            search.extend_pages((0..10_000).map(|index| index / 10));
            SearchTest { search, rendered: Cell::new(0), clicked: None }
        });
        assert!(cx.debug_bounds("test-search-hit-0").is_some());
        assert!(cx.debug_bounds("test-search-hit-9999").is_none());
        view.read_with(cx, |view, _| assert!(view.rendered.get() < 100, "must not build the full result set"));
        view.read_with(cx, |view, _| assert!(view.search.list.state.max_offset_for_scrollbar().y > px(100_000.0), "the scrollbar must include unvisited results"));
        view.update(cx, |view, cx| {
            view.rendered.set(0);
            view.search.reveal_match(10_000);
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let last = cx.debug_bounds("test-search-hit-9999").unwrap();
        assert!(cx.debug_bounds("test-search-hit-0").is_none());
        view.read_with(cx, |view, _| assert!(view.rendered.get() < 100, "jumping must not measure every intervening result"));
        cx.simulate_click(last.center(), Default::default());
        view.read_with(cx, |view, _| assert_eq!(view.clicked, Some(9_999)));
        view.update(cx, |view, cx| {
            let before = view.search.list.state.logical_scroll_top();
            view.search.extend_pages([1_000, 1_000]);
            assert_eq!(view.search.list.state.logical_scroll_top().item_ix, before.item_ix);
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("test-search-hit-9999").is_some(), "streamed results must preserve the scroll position");
        view.update(cx, |view, cx| {
            view.search.clear();
            view.search.extend_pages([5]);
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("test-search-hit-0").is_some());
        assert!(cx.debug_bounds("test-search-hit-9999").is_none());
    }
}
