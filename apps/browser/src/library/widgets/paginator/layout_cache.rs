//! Small cache of immutable page assignments. Moving a page does not change
//! its packing; data, typography and viewport changes do.

use super::slots::Slots;
use std::collections::VecDeque;
use std::rc::Rc;

use gpui::Pixels;

use super::geometry::{self, AssignedRow, GroupGeometry};
use super::{AssignmentDirection, PaginatorData, VisiblePage};

const CAPACITY: usize = 6;

#[derive(Clone)]
pub(super) struct LayoutKey {
    pub data: Rc<PaginatorData>,
    pub widths: Rc<Slots<Pixels>>,
    pub width: Pixels,
    pub height: Pixels,
    pub rem_size: Pixels,
    pub child_offset: usize,
    pub direction: AssignmentDirection,
    pub forced_page: Option<VisiblePage>,
}

impl LayoutKey {
    fn matches(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.data, &other.data)
            && Rc::ptr_eq(&self.widths, &other.widths)
            && self.width == other.width
            && self.height == other.height
            && self.rem_size == other.rem_size
            && self.child_offset == other.child_offset
            && self.direction == other.direction
            && self.forced_page == other.forced_page
    }
}

pub(super) struct PageLayout {
    pub key: LayoutKey,
    pub geometries: Vec<GroupGeometry>,
    pub rows: Vec<AssignedRow>,
    pub page: Option<VisiblePage>,
}

#[derive(Default)]
pub(super) struct LayoutCache {
    entries: VecDeque<Rc<PageLayout>>,
    #[cfg(test)]
    pub misses: usize,
}

impl LayoutCache {
    pub fn resolve(&mut self, key: LayoutKey) -> Rc<PageLayout> {
        // A replacement data/typography snapshot supersedes every old plan;
        // do not retain book-render closures from previous listings.
        if self.entries.front().is_some_and(|layout| !Rc::ptr_eq(&layout.key.data, &key.data) || !Rc::ptr_eq(&layout.key.widths, &key.widths)) {
            self.entries.clear();
        }
        if let Some(index) = self.entries.iter().position(|layout| layout.key.matches(&key))
            && let Some(entry) = self.entries.remove(index)
        {
            self.entries.push_front(entry.clone());
            return entry;
        }
        #[cfg(test)]
        {
            self.misses += 1;
        }
        let geometries = geometry::group_geometries(&key.data, key.width, key.rem_size);
        let rows = if let Some(page) = key.forced_page {
            if key.data.groups.iter().any(|group| group.policy.column_fraction.is_some()) {
                geometry::strip_rows(&key.data, &geometries, page.first_child, page.last_child.saturating_add(1), key.height, key.rem_size)
            } else {
                geometry::forward_rows_in_range(&key.data, &key.widths, &geometries, page.first_child, page.last_child.saturating_add(1))
            }
        } else {
            geometry::page_rows(&key.data, &key.widths, &geometries, key.direction, key.child_offset, key.height, key.rem_size)
        };
        let page = rows.first().zip(rows.last()).and_then(|(first, last)| Some(VisiblePage { first_child: first.items.first()?.child_index, last_child: last.items.last()?.child_index }));
        let entry = Rc::new(PageLayout { key, geometries, rows, page });
        self.entries.push_front(entry.clone());
        self.entries.truncate(CAPACITY);
        entry
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PaginatorChild, PaginatorGroup, PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy};
    use super::*;
    use gpui::px;

    fn key() -> LayoutKey {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(40.0)), px(0.0)).with_gap(px(0.0)).flush_edges();
        let data = Rc::new(PaginatorData::new([PaginatorGroup::new(policy, (0..100).map(|_| PaginatorChild::new(|_, _, _| unreachable!())))]));
        LayoutKey { data, widths: Rc::new(vec![Pixels::ZERO; 100].into()), width: px(400.0), height: px(80.0), rem_size: px(16.0), child_offset: 0, direction: AssignmentDirection::Forward, forced_page: None }
    }

    #[test]
    fn unchanged_drag_frames_reuse_packing_but_resize_and_typography_replace_it() {
        let mut cache = LayoutCache::default();
        let key = key();
        let original = cache.resolve(key.clone());
        assert_eq!(original.page, Some(VisiblePage { first_child: 0, last_child: 7 }));
        for _ in 0..60 {
            assert!(Rc::ptr_eq(&original, &cache.resolve(key.clone())));
        }
        assert_eq!(cache.misses, 1);
        let resized = cache.resolve(LayoutKey { width: px(200.0), ..key.clone() });
        assert_eq!(resized.page, Some(VisiblePage { first_child: 0, last_child: 3 }));
        let remeasured = cache.resolve(LayoutKey { widths: Rc::new(vec![Pixels::ZERO; 100].into()), ..key.clone() });
        assert!(!Rc::ptr_eq(&original, &remeasured));
        assert_eq!(cache.misses, 3);
        for child_offset in 0..100 {
            cache.resolve(LayoutKey { child_offset, ..key.clone() });
        }
        assert_eq!(cache.entries.len(), CAPACITY);
    }
}
