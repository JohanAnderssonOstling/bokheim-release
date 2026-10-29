//! Compact descriptions for a variable-height, grouped Trash list.

use std::cmp::Reverse;

use gpui::{Font, ListAlignment, ListState, Pixels, Window, px};
use library_model::LibraryTrashView;

use super::{DAY_MS, TrashItem, group_of};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TrashRow {
    Group(usize),
    Folder(usize),
    Book(usize),
}

impl TrashRow {
    pub(super) fn item(self, contents: &LibraryTrashView) -> Option<TrashItem<'_>> {
        match self {
            Self::Folder(index) => contents.folders.get(index).map(TrashItem::Folder),
            Self::Book(index) => contents.books.get(index).map(TrashItem::Book),
            Self::Group(_) => None,
        }
    }
}

pub(super) struct TrashList {
    pub(super) rows: Vec<TrashRow>,
    pub(super) state: ListState,
    dirty: bool,
    next_group_change: i64,
    typography: Option<(Pixels, Font, u32)>,
}

impl Default for TrashList {
    fn default() -> Self {
        Self { rows: Vec::new(), state: ListState::new(0, ListAlignment::Top, px(150.0)), dirty: true, next_group_change: i64::MAX, typography: None }
    }
}

impl TrashList {
    pub(super) fn sync_typography(&mut self, window: &Window) {
        let next = (window.rem_size(), window.text_style().font(), window.scale_factor().to_bits());
        if self.typography.as_ref().is_some_and(|previous| *previous != next) {
            self.state.remeasure();
        }
        self.typography = Some(next);
    }

    pub(super) fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Rebuild after content or input changes, or when an item ages into
    /// another group. Ordinary repaints retain the rows and scroll position.
    pub(super) fn update(&mut self, contents: &LibraryTrashView, query: &str, now: i64) {
        if !self.dirty && now < self.next_group_change {
            return;
        }
        self.dirty = false;
        let query = query.trim().to_lowercase();
        self.next_group_change = contents
            .folders
            .iter()
            .map(|folder| folder.deleted_at)
            .chain(contents.books.iter().map(|book| book.deleted_at))
            .flat_map(|deleted| [deleted.saturating_add(DAY_MS), deleted.saturating_add(7 * DAY_MS)])
            .filter(|boundary| *boundary > now)
            .min()
            .unwrap_or(i64::MAX);
        let mut items: Vec<_> = (0..contents.folders.len())
            .map(TrashRow::Folder)
            .chain((0..contents.books.len()).map(TrashRow::Book))
            .filter_map(|row| {
                let item = row.item(contents)?;
                (query.is_empty() || item.matches(&query)).then_some((row, item.deleted_at()))
            })
            .collect();
        items.sort_by_key(|(_, deleted_at)| Reverse(*deleted_at));
        let mut rows = Vec::with_capacity(items.len() + 3);
        let mut previous_group = None;
        for (row, deleted_at) in items {
            let group = group_of(deleted_at, now);
            if previous_group != Some(group) {
                rows.push(TrashRow::Group(group));
                previous_group = Some(group);
            }
            rows.push(row);
        }
        if rows != self.rows {
            self.state.reset_with_uniform_height(rows.len(), px(80.0));
            self.rows = rows;
        }
    }
}
