//! The PDF outline, and which of its entries the current page falls under.
//!
//! The outline arrives as the tree the publisher authored, in the navigation
//! form every format shares, and that tree is the only copy this view keeps.
//! An entry is named by its position in document order, so the sidebar, the
//! page lookup and the active-entry search all agree on identity without
//! anything having to be stored twice.

use book_model::{BookTocEntry, pdf_toc_page};
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
    /// The entry the current page sits under, if any.
    active_id: Option<String>,
}

impl PdfTocState {
    pub(crate) fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    pub(crate) fn replace_entries(&mut self, entries: Vec<BookTocEntry>) {
        self.entries = entries;
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

    pub(crate) fn title_at_ordinal(&self, ordinal: usize) -> Option<&str> {
        Some(self.entry(&toc_id(ordinal))?.title.as_str())
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
mod tests {
    use super::*;

    fn entry(title: &str, page_index: usize, children: Vec<BookTocEntry>) -> BookTocEntry {
        BookTocEntry { title: title.to_owned(), target: book_model::pdf_toc_target(page_index), children }
    }

    fn sample() -> PdfTocState {
        let mut toc = PdfTocState::default();
        toc.replace_entries(vec![entry("Introduction", 0, Vec::new()), entry("Method", 12, vec![entry("Sampling", 12, Vec::new())]), entry("Results", 40, Vec::new())]);
        toc
    }

    #[test]
    fn document_order_names_every_entry_including_nested_ones() {
        let toc = sample();
        let titles = ["Introduction", "Method", "Sampling", "Results"];

        for (order, title) in titles.iter().enumerate() {
            assert_eq!(toc.entry(&toc_id(order)).map(|entry| entry.title.as_str()), Some(*title), "a parent is followed by its children, not by its sibling");
        }
        assert!(toc.entry("pdf-toc-4").is_none());
        assert!(toc.entry("epub-toc-0").is_none());
        assert_eq!(toc.title_at_ordinal(2), Some("Sampling"));
        assert_eq!(toc.title_at_ordinal(4), None);
    }

    #[test]
    fn the_sidebar_and_the_page_lookup_agree_on_identity() {
        let toc = sample();
        let items = pdf_toc_tree_items(&toc.entries);

        assert_eq!(items.len(), 3, "the tree keeps its shape rather than being flattened");
        assert_eq!(items[1].children.len(), 1);
        assert_eq!(toc.page_of(&items[1].children[0].id), Some(12), "the nested item's id resolves to the page it opens");
        assert_eq!(toc.page_of(&items[2].id), Some(40));
        assert_eq!(toc.page_of("missing"), None);
    }

    #[test]
    fn a_page_resolves_to_the_last_entry_at_or_before_it() {
        let toc = sample();
        assert_eq!(toc.entry_at(0).as_deref(), Some("pdf-toc-0"));
        assert_eq!(toc.entry_at(11).as_deref(), Some("pdf-toc-0"), "a page before the next entry stays in the previous section");
        assert_eq!(toc.entry_at(41).as_deref(), Some("pdf-toc-3"));
    }

    #[test]
    fn entries_sharing_a_page_resolve_to_the_innermost() {
        let toc = sample();
        assert_eq!(toc.entry_at(12).as_deref(), Some("pdf-toc-2"), "a section opening its chapter's page wins over the chapter");
    }

    #[test]
    fn ordinals_match_sidebar_identity() {
        let toc = sample();
        assert_eq!(toc.ordinal_at(0), Some(0));
        assert_eq!(toc.ordinal_at(12), Some(2));
        assert_eq!(toc.ordinal_at(41), Some(3));
        assert_eq!(toc.ordinal_at(11), toc.entry_at(11).and_then(|id| toc_order(&id)), "ordinal and id resolve through the same search");
    }

    #[test]
    fn an_outline_that_moves_backwards_still_resolves_to_the_nearest_page() {
        let mut toc = PdfTocState::default();
        toc.replace_entries(vec![entry("Chapter", 10, Vec::new()), entry("Frontispiece", 2, Vec::new())]);

        assert_eq!(toc.entry_at(5).as_deref(), Some("pdf-toc-1"), "only the frontispiece opens at or before page five");
        assert_eq!(toc.entry_at(12).as_deref(), Some("pdf-toc-0"), "the chapter opens later than the frontispiece, so it wins");
    }

    #[test]
    fn a_page_before_the_first_entry_belongs_to_nothing() {
        let mut toc = PdfTocState::default();
        toc.replace_entries(vec![entry("Later", 5, Vec::new())]);
        assert_eq!(toc.entry_at(0), None);
    }

    #[test]
    fn an_outline_free_pdf_resolves_nothing() {
        let toc = PdfTocState::default();
        assert_eq!(toc.entry_at(3), None);
        assert_eq!(toc.active_title(), None);
    }

    #[test]
    fn the_active_entry_exposes_its_title() {
        let mut toc = sample();
        toc.set_active(Some("pdf-toc-2".to_owned()));
        assert_eq!(toc.active_title(), Some("Sampling"));
    }
}
