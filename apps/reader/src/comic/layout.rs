//! Continuous rows of comic pages, anchored by page and row-relative position.

use book_content::comic::ComicPage;
use fixed_page::{PageLayout, PageZoom};
use std::ops::Range;

pub(super) const MAX_COLUMNS: usize = 6;

#[derive(Clone, Copy, Debug)]
pub(super) struct PageRow {
    pub first: usize,
    pub count: usize,
    pub top: f32,
    pub height: f32,
    pub cell_width: f32,
    pub gap: f32,
}

pub(super) struct ComicLayout {
    pub rows: Vec<PageRow>,
    pub viewport_height: f32,
    pub max_scroll: f32,
}

impl ComicLayout {
    pub fn new(pages: &[ComicPage], columns: usize, viewport: (f32, f32), zoom: PageZoom, page_gap: f32, size: impl Fn(usize) -> Option<(u32, u32)>) -> Self {
        let columns = columns.clamp(1, MAX_COLUMNS);
        let viewport = (viewport.0.max(1.0), viewport.1.max(1.0));
        let mut rows = Vec::new();
        let mut first = 0;
        let mut top = 0.0;
        while first < pages.len() {
            let count = if pages[first].double_page { 1 } else { pages[first..].iter().take(columns).take_while(|page| !page.double_page).count() };
            let gap = if count <= 1 { 0.0 } else { page_gap.max(0.0).min(((viewport.0 - 100.0) / (count - 1) as f32).max(0.0)) };
            let cell_width = (viewport.0 - gap * count.saturating_sub(1) as f32) / count as f32;
            let height = (first..first + count).map(|index| size(index).map(|size| PageLayout::new(size, (cell_width, viewport.1), zoom, 0.0, 0.0).height).unwrap_or(viewport.1)).fold(1.0, f32::max);
            let height = if zoom == PageZoom::FitPage { height.max(viewport.1) } else { height };
            rows.push(PageRow { first, count, top, height, cell_width, gap });
            top += height;
            first += count;
        }
        Self { rows, viewport_height: viewport.1, max_scroll: (top - viewport.1).max(0.0) }
    }

    pub fn row_for_page(&self, page: usize) -> usize {
        self.rows.partition_point(|row| row.first <= page).saturating_sub(1)
    }

    pub fn scroll_top(&self, page: usize, offset: f32) -> f32 {
        let row = self.rows[self.row_for_page(page)];
        (row.top + offset * row.height).clamp(0.0, self.max_scroll)
    }

    pub fn anchor(&self, scroll_top: f32) -> (usize, f32) {
        let top = scroll_top.clamp(0.0, self.max_scroll);
        let row = self.rows[self.rows.partition_point(|row| row.top <= top).saturating_sub(1)];
        (row.first, (top - row.top) / row.height)
    }

    pub fn visible_rows(&self, scroll_top: f32) -> Range<usize> {
        let first = self.rows.partition_point(|row| row.top + row.height <= scroll_top);
        let end = self.rows.partition_point(|row| row.top < scroll_top + self.viewport_height);
        first..end
    }

    pub fn visible_pages(&self, scroll_top: f32) -> Range<usize> {
        let visible = self.visible_rows(scroll_top);
        let first = self.rows[visible.start].first;
        let last = self.rows[visible.end - 1];
        first..last.first + last.count
    }

    pub fn requested_pages(&self, scroll_top: f32) -> Vec<usize> {
        let visible = self.visible_rows(scroll_top);
        let mut pages: Vec<_> = self.visible_pages(scroll_top).collect();
        if let Some(row) = self.rows.get(visible.end) {
            pages.extend(row.first..row.first + row.count);
        }
        if visible.start > 0 {
            let row = self.rows[visible.start - 1];
            pages.extend(row.first..row.first + row.count);
        }
        pages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(count: usize) -> Vec<ComicPage> {
        (0..count).map(|index| ComicPage { name: format!("{index}.jpg"), bookmark: None, double_page: false }).collect()
    }

    #[test]
    fn page_gap_resizes_spreads_and_does_not_shrink_double_pages() {
        let mut pages = pages(3);
        pages[2].double_page = true;
        let flow = ComicLayout::new(&pages, 2, (800.0, 600.0), PageZoom::FitWidth, 32.0, |_| Some((400, 1000)));
        assert_eq!(flow.rows[0].cell_width, 384.0);
        assert_eq!(flow.rows[0].gap, 32.0);
        assert_eq!(flow.rows[0].height, 960.0);
        assert_eq!(flow.rows[1].cell_width, 800.0);
        assert_eq!(flow.rows[1].gap, 0.0);
        let small = ComicLayout::new(&pages, 2, (120.0, 200.0), PageZoom::FitWidth, 200.0, |_| Some((400, 1000)));
        assert_eq!(small.rows[0].cell_width * 2.0 + small.rows[0].gap, 120.0);
        assert!(small.rows[0].cell_width >= 1.0);
    }

    #[test]
    fn viewport_steps_reveal_a_tall_page_before_advancing_and_reverse_exactly() {
        let flow = ComicLayout::new(&pages(3), 1, (800.0, 600.0), PageZoom::FitWidth, 0.0, |_| Some((800, 1600)));
        for top in [0.0, 600.0, 1200.0] {
            let (page, offset) = flow.anchor(top);
            assert_eq!(page, 0);
            assert!((flow.scroll_top(page, offset) - top).abs() < 0.001);
        }
        let (page, offset) = flow.anchor(1800.0);
        assert_eq!(page, 1);
        assert!((offset - 0.125).abs() < 0.001);
        let (page, offset) = flow.anchor(flow.scroll_top(page, offset) - 600.0);
        assert_eq!(page, 0);
        assert!((offset - 0.75).abs() < 0.001);
    }

    #[test]
    fn scrolling_across_a_boundary_keeps_the_remainder_and_both_pages_visible() {
        let flow = ComicLayout::new(&pages(3), 1, (800.0, 600.0), PageZoom::FitWidth, 0.0, |_| Some((800, 1000)));
        assert_eq!(flow.visible_pages(950.0), 0..2);
        let (page, offset) = flow.anchor(1050.0);
        assert_eq!(page, 1);
        assert!((offset - 0.05).abs() < 0.001);
        assert_eq!(flow.scroll_top(page, offset), 1050.0);
        assert_eq!(flow.anchor(950.0), (0, 0.95));
    }

    #[test]
    fn short_pages_fill_the_viewport_and_the_end_has_no_empty_tail() {
        let flow = ComicLayout::new(&pages(5), 1, (800.0, 600.0), PageZoom::FitWidth, 0.0, |_| Some((800, 200)));
        assert_eq!(flow.visible_pages(0.0), 0..3);
        assert_eq!(flow.max_scroll, 400.0);
        assert_eq!(flow.anchor(10_000.0), (2, 0.0));
        assert_eq!(flow.visible_pages(flow.max_scroll), 2..5);
        assert_eq!(flow.anchor(-10.0), (0, 0.0));
    }

    #[test]
    fn whole_page_mode_turns_one_row_at_a_time() {
        let flow = ComicLayout::new(&pages(3), 1, (800.0, 600.0), PageZoom::FitPage, 0.0, |_| Some((800, 200)));
        assert_eq!(flow.anchor(600.0), (1, 0.0));
        assert_eq!(flow.visible_pages(300.0), 0..2);
        assert_eq!(flow.rows[0].height, 600.0);
    }

    #[test]
    fn spreads_keep_double_pages_alone_and_use_the_tallest_page() {
        let mut pages = pages(9);
        pages[3].double_page = true;
        let flow = ComicLayout::new(&pages, 3, (900.0, 600.0), PageZoom::FitWidth, 0.0, |index| Some((300, if index == 1 { 1200 } else { 600 })));
        assert_eq!(flow.rows.iter().map(|row| (row.first, row.count)).collect::<Vec<_>>(), [(0, 3), (3, 1), (4, 3), (7, 2)]);
        assert_eq!(flow.rows[0].height, 1200.0);
        assert_eq!(flow.anchor(600.0), (0, 0.5));
        assert_eq!(flow.row_for_page(5), 2);
    }

    #[test]
    fn missing_images_reserve_space_and_prefetch_surrounding_rows() {
        let flow = ComicLayout::new(&pages(10), 2, (800.0, 600.0), PageZoom::FitWidth, 0.0, |_| None);
        assert_eq!(flow.requested_pages(750.0), [2, 3, 4, 5, 6, 7, 0, 1]);
    }
}
