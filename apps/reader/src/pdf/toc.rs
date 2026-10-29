//! The PDF outline, and which of its entries the current page falls under.
//!
//! The outline arrives as the tree the publisher authored, in the navigation
//! form every format shares, and that tree is the only copy this view keeps.
//! An entry is named by its position in document order, so the sidebar, the
//! page lookup and the active-entry search all agree on identity without
//! anything having to be stored twice.

use std::collections::HashSet;

use book_model::{BookTocEntry, ChapterReadStatus, ReadingPosition, pdf_toc_page};
use gpui_component::tree::TreeItem;

const TOC_ID_PREFIX: &str = "pdf-toc-";

fn toc_id(order: usize) -> String {
    format!("{TOC_ID_PREFIX}{order}")
}

fn toc_order(id: &str) -> Option<usize> {
    id.strip_prefix(TOC_ID_PREFIX)?.parse().ok()
}

#[derive(Default)]
pub(crate) struct PdfTocState {
    entries: Vec<BookTocEntry>,
    pub(crate) read_status: ChapterReadStatus,
    /// The entry the current page sits under, if any.
    active_id: Option<String>,
}

impl PdfTocState {
    pub(crate) fn reset_chapter_read_status(&mut self, id: &str) -> Option<String> {
        let target = self.entry(id)?.target.clone();
        self.read_status.reset_chapter(&self.entries, &target);
        Some(target)
    }
    pub(crate) fn update_read_status(&mut self, page: usize, at_end: bool) {
        self.read_status.update_position(&self.entries, &ReadingPosition::pdf_page(page as u32), None, at_end);
    }

    pub(crate) fn read_ids(&self) -> HashSet<String> {
        fn visit(entries: &[BookTocEntry], status: &ChapterReadStatus, next: &mut usize, ids: &mut HashSet<String>) {
            for entry in entries {
                if status.completed.contains(&entry.target) {
                    ids.insert(toc_id(*next));
                }
                *next += 1;
                visit(&entry.children, status, next, ids);
            }
        }
        let mut ids = HashSet::new();
        visit(&self.entries, &self.read_status, &mut 0, &mut ids);
        ids
    }
    pub(crate) fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    pub(crate) fn replace_entries(&mut self, entries: Vec<BookTocEntry>) {
        self.entries = entries;
    }

    fn chapter_pages(&self) -> Vec<usize> {
        fn visit(entries: &[BookTocEntry], result: &mut Vec<usize>) {
            for entry in entries {
                if let Some(page) = pdf_toc_page(&entry.target) {
                    result.push(page);
                } else {
                    visit(&entry.children, result);
                }
            }
        }
        let mut pages = Vec::new();
        visit(&self.entries, &mut pages);
        pages.sort_unstable();
        pages.dedup();
        pages
    }

    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    pub(crate) fn narration_chapter_label(&self, page: usize) -> String {
        let start = self.chapter_at(page);
        let number = self.chapter_pages().iter().position(|page| *page == start).unwrap_or(0) + 1;
        fn chapter_title(entries: &[BookTocEntry], start: usize) -> Option<&str> {
            for entry in entries {
                if let Some(page) = pdf_toc_page(&entry.target) {
                    if page == start {
                        return Some(entry.title.as_str());
                    }
                } else if let Some(title) = chapter_title(&entry.children, start) {
                    return Some(title);
                }
            }
            None
        }
        let title = chapter_title(&self.entries, start).filter(|title| !title.trim().is_empty());
        title.map_or_else(|| format!("Chapter {number}"), |title| format!("Chapter {number} · {title}"))
    }

    pub(crate) fn chapter_at(&self, page: usize) -> usize {
        self.chapter_pages().into_iter().rev().find(|start| *start <= page).unwrap_or(0)
    }

    #[cfg(any(test, all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo"))))]
    pub(crate) fn narration_start_page(&self, page: usize, page_count: usize) -> Option<usize> {
        (page..page_count).find(|page| {
            let entry = self.ordinal_at(*page).and_then(|ordinal| entry_in_order(&self.entries, ordinal, &mut 0));
            entry.is_none_or(|entry| !html::layout::is_navigation_section_title(&entry.title))
        })
    }

    /// Changes the active outline entry and reports whether it moved.
    pub(crate) fn set_active(&mut self, active_id: Option<String>) -> bool {
        if self.active_id == active_id {
            return false;
        }
        self.active_id = active_id;
        true
    }

    /// The entry covering `page_index`: the one opening at the highest page at
    /// or before it.
    ///
    /// Ties break towards the deepest entry, then the latest in document
    /// order, so a chapter and the section opening it both starting on one
    /// page resolve to the section.
    pub(crate) fn entry_at(&self, page_index: usize) -> Option<String> {
        self.ordinal_at(page_index).map(toc_id)
    }

    /// Depth-first ordinal of the entry covering `page_index`, for reading
    /// position. Matches the sidebar identity, so stored ordinals agree with
    /// what the contents panel shows.
    pub(crate) fn ordinal_at(&self, page_index: usize) -> Option<usize> {
        let mut best = None;
        best_entry_at(&self.entries, page_index, 0, &mut 0, &mut best);
        best.map(|(_, _, order)| order)
    }

    pub(crate) fn page_of(&self, id: &str) -> Option<usize> {
        pdf_toc_page(&self.entry(id)?.target)
    }

    /// The title of the active entry, shown in the reader toolbar.
    pub(crate) fn active_title(&self) -> Option<&str> {
        Some(self.entry(self.active_id.as_ref()?)?.title.as_str())
    }

    pub(crate) fn active_id(&self) -> Option<&str> {
        self.active_id.as_deref()
    }

    fn entry(&self, id: &str) -> Option<&BookTocEntry> {
        entry_in_order(&self.entries, toc_order(id)?, &mut 0)
    }
}

/// The entry at one position in document order.
fn entry_in_order<'a>(entries: &'a [BookTocEntry], order: usize, next: &mut usize) -> Option<&'a BookTocEntry> {
    for entry in entries {
        let current = *next;
        *next += 1;
        if current == order {
            return Some(entry);
        }
        if let Some(found) = entry_in_order(&entry.children, order, next) {
            return Some(found);
        }
    }
    None
}

/// Searches for the entry a page belongs to, carrying the best candidate so
/// far as `(page, depth, order)` — the ordering that decides ties.
fn best_entry_at(entries: &[BookTocEntry], page_index: usize, depth: usize, next: &mut usize, best: &mut Option<(usize, usize, usize)>) {
    for entry in entries {
        let order = *next;
        *next += 1;
        if let Some(page) = pdf_toc_page(&entry.target).filter(|page| *page <= page_index) {
            let candidate = (page, depth, order);
            if best.is_none_or(|current| candidate > current) {
                *best = Some(candidate);
            }
        }
        best_entry_at(&entry.children, page_index, depth + 1, next, best);
    }
}

/// Builds the sidebar tree, naming each entry by its position in document
/// order so [`PdfTocState`] can find it again.
pub(crate) fn pdf_toc_tree_items(entries: &[BookTocEntry]) -> Vec<TreeItem> {
    tree_items(entries, &mut 0)
}

fn tree_items(entries: &[BookTocEntry], next: &mut usize) -> Vec<TreeItem> {
    entries
        .iter()
        .map(|entry| {
            let id = toc_id(*next);
            *next += 1;
            TreeItem::new(id, entry.title.clone()).children(tree_items(&entry.children, next))
        })
        .collect()
}

#[cfg(test)]
mod tts_tests {
    use super::*;

    #[test]
    fn narration_skips_contents_pages_and_resumes_at_the_next_section() {
        let mut toc = PdfTocState::default();
        toc.replace_entries(vec![
            BookTocEntry { title: "Contents".into(), target: "pdf-page:1".into(), children: vec![] },
            BookTocEntry { title: "List of Figures".into(), target: "pdf-page:3".into(), children: vec![] },
            BookTocEntry { title: "Chapter One".into(), target: "pdf-page:4".into(), children: vec![] },
        ]);
        assert_eq!(toc.narration_start_page(0, 8), Some(0));
        assert_eq!(toc.narration_start_page(1, 8), Some(4));
        assert_eq!(toc.narration_start_page(2, 8), Some(4));
        assert_eq!(toc.narration_start_page(4, 8), Some(4));
        assert_eq!(toc.narration_start_page(1, 4), None);
    }

    #[test]
    fn sleep_chapters_group_outline_subsections() {
        let mut toc = PdfTocState::default();
        toc.replace_entries(vec![
            BookTocEntry { title: "One".into(), target: "pdf-page:0".into(), children: vec![BookTocEntry { title: "Section".into(), target: "pdf-page:3".into(), children: vec![] }] },
            BookTocEntry { title: "Two".into(), target: "pdf-page:10".into(), children: vec![] },
        ]);
        assert_eq!(toc.chapter_at(8), 0);
        assert_eq!(toc.chapter_at(12), 10);
        assert_eq!(PdfTocState::default().chapter_at(3), 0);
    }
}
