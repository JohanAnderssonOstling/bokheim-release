//! Pure paginator geometry. This module does not render or retain GPUI
//! elements; it turns explicit groups into stable canonical rows.
//!
//! Grid groups resolve a column count and shared width. Intrinsic groups pack
//! measured child widths in reading order with fixed gaps. Both use the same
//! row-height and group-header budget for pagination.

use gpui::{Pixels, px};

use super::{AssignmentDirection, PaginatorData, PaginatorGroupPolicy};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GroupGeometry {
    pub(super) group_index: usize,
    pub(super) columns: usize,
    pub(super) child_width: Pixels,
    /// The resolved gap between two children.
    pub(super) column_gap: Pixels,
    /// The margin before the first child: zero for a left-aligned group, one
    /// gap for a stretched one, whose children already fill the rest.
    pub(super) leading: Pixels,
}

#[derive(Clone)]
pub(super) struct AssignedRow {
    pub(super) group_index: usize,
    pub(super) items: Vec<PageItem>,
    pub(super) child_width: Pixels,
    pub(super) column_gap: Pixels,
}

#[derive(Clone)]
pub(super) struct PageItem {
    pub(super) child_index: usize,
    pub(super) x: Pixels,
    pub(super) width: Pixels,
}

/// Fits one group into `available_width`.
///
/// The column count always comes from the base width, never from the width a
/// child ends up drawn at: stretching must not change how many fit, or it
/// would feed back on itself. Every group resolves its grid across the full
/// available width, regardless of its child count, so short and wrapped groups
/// share the same child widths and gaps for a given policy.
///
/// The result is one width per group per viewport, which is what lets row
/// heights — including aspect-ratio ones, which follow the stretched width —
/// be known without measuring anything.
pub(super) fn group_geometry(policy: PaginatorGroupPolicy, group_index: usize, available_width: Pixels, rem_size: Pixels) -> GroupGeometry {
    // The group's own margin comes off the top, so everything below — the column
    // count, the stretch, the ceiling — divides the width the children actually
    // get. Adding it afterwards would push the last child past the edge.
    let inset = f32::from(policy.edge_inset).max(0.0);
    let available = (f32::from(available_width) - inset * 2.0).max(1.0);
    // A child never exceeds the row it sits in, so a window narrower than one
    // declared width degrades to a single shrunken column rather than overflow.
    if policy.intrinsic {
        return GroupGeometry { group_index, columns: 1, child_width: px(available), column_gap: policy.column_gap, leading: px(inset) };
    }
    let base_width = f32::from(policy.width.width(rem_size)).max(1.0).min(available);
    let column_gap = f32::from(policy.column_gap);
    let fit_width = if policy.maximum_width.is_some() && !policy.inset_edges { available + column_gap } else { available - column_gap };
    let columns_at = |width: f32| ((fit_width / (width + column_gap)).floor() as usize).max(1).min(u16::MAX as usize);
    // Respect the group's minimum column count on narrow windows by shrinking
    // children only when their preferred widths cannot fit.
    let fit = columns_at(base_width).max(policy.minimum_columns);

    let columns = fit;
    let gap_count = if policy.inset_edges { columns + 1 } else { columns - 1 };
    let fitted_width = ((available - column_gap * gap_count as f32) / columns as f32).max(1.0);
    let maximum_width = policy.maximum_width.map(|width| f32::from(width.width(rem_size)).max(base_width));
    let child_width = maximum_width.map_or(fitted_width, |maximum| maximum.min(fitted_width));
    let column_gap = if policy.maximum_width.is_some() && gap_count > 0 && !policy.pack_start { ((available - child_width * columns as f32) / gap_count as f32).max(column_gap) } else { column_gap };
    let occupied = columns as f32 * child_width + (columns.saturating_sub(1)) as f32 * column_gap;
    let leading = if policy.inset_edges { ((available - occupied) / 2.0).max(column_gap) } else { 0.0 };
    GroupGeometry { group_index, columns, child_width: px(child_width), column_gap: px(column_gap), leading: px(leading + inset) }
}

pub(super) fn group_geometries(data: &PaginatorData, available_width: Pixels, rem_size: Pixels) -> Vec<GroupGeometry> {
    data.groups.iter().enumerate().map(|(group_index, group)| group_geometry(group.policy, group_index, available_width, rem_size)).collect()
}

/// Width used when the paginator must report a size before it has been laid
/// out. The widest declared child is the only sensible guess.
pub(super) fn fallback_width(data: &PaginatorData, rem_size: Pixels) -> Pixels {
    px(data.groups.iter().map(|group| f32::from(group.policy.width.width(rem_size))).fold(1.0, f32::max))
}

pub(super) fn next_forward_row(data: &PaginatorData, geometries: &[GroupGeometry], next_child: &mut usize) -> Option<AssignedRow> {
    if *next_child >= data.children.len() {
        return None;
    }
    let group_index = data.group_index_for_child(*next_child)?;
    let group = data.groups[group_index];
    let geometry = geometries[group_index];
    debug_assert_eq!(geometry.group_index, group_index);
    let row_start = *next_child;
    let mut row_end = row_start;
    let mut items = Vec::new();
    let mut x = geometry.leading;
    while row_end < group.end {
        let width = if group.policy.intrinsic { data.children[row_end].measured_width.get().max(px(1.0)).min(geometry.child_width) } else { geometry.child_width };
        if !items.is_empty() && (if group.policy.intrinsic { x + width > geometry.leading + geometry.child_width } else { items.len() >= geometry.columns }) {
            break;
        }
        items.push(PageItem { child_index: row_end, x, width });
        x += width + geometry.column_gap;
        row_end += 1;
    }
    *next_child = row_end;
    Some(AssignedRow { group_index, items, child_width: geometry.child_width, column_gap: geometry.column_gap })
}

/// Normalizes an exact child range forward. The start remains stable and only
/// the final row of each group can be incomplete.
pub(super) fn forward_rows_in_range(data: &PaginatorData, geometries: &[GroupGeometry], start: usize, end: usize) -> Vec<AssignedRow> {
    let end = end.min(data.children.len());
    let mut next_child = start.min(end);
    let mut rows = Vec::new();
    while next_child < end {
        let Some(mut row) = next_forward_row(data, geometries, &mut next_child) else {
            break;
        };
        if let Some(first_after_range) = row.items.iter().position(|item| item.child_index >= end) {
            row.items.truncate(first_after_range);
            next_child = end;
        }
        if !row.items.is_empty() {
            rows.push(row);
        }
    }
    rows
}

/// The vertical gap separating two rows. Rows from different groups take the
/// larger of the two policies so neither group is crowded at the boundary.
pub(super) fn gap_between(data: &PaginatorData, previous: &AssignedRow, next: &AssignedRow) -> Pixels {
    px(f32::from(data.groups[previous.group_index].policy.row_gap).max(f32::from(data.groups[next.group_index].policy.row_gap)))
}

pub(super) fn page_rows(data: &PaginatorData, geometries: &[GroupGeometry], direction: AssignmentDirection, child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    match direction {
        AssignmentDirection::Forward => forward_page_rows(data, geometries, child_offset, maximum_rows, available_height, rem_size),
        AssignmentDirection::Backward => backward_page_rows(data, geometries, child_offset, maximum_rows, available_height, rem_size),
    }
}

fn forward_page_rows(data: &PaginatorData, geometries: &[GroupGeometry], child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    let mut next_child = child_offset.min(data.children.len());
    let mut rows = Vec::new();
    while maximum_rows.is_none_or(|maximum_rows| rows.len() < maximum_rows) {
        let Some(row) = next_forward_row(data, geometries, &mut next_child) else {
            break;
        };
        let mut candidate = rows.clone();
        candidate.push(row);
        if rows_height(data, &candidate, rem_size) > available_height.max(px(1.0)) && (!rows.is_empty() || data.groups[candidate[0].group_index].header_sizing.is_some()) {
            break;
        }
        rows = candidate;
    }
    rows
}

/// The first child of a page that ends just before `page_end`.
///
/// Walks backward one group at a time. Uniform row heights determine how
/// many rows fit; intrinsic widths determine which children those rows hold.
fn backward_page_start(data: &PaginatorData, geometries: &[GroupGeometry], page_end: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels) -> usize {
    let budget = f32::from(available_height).max(1.0);
    let mut start = page_end;
    let mut used = 0.0;
    let mut rows_taken = 0usize;
    let mut adjacent_group: Option<usize> = None;

    while start > 0 {
        let Some(group_index) = data.group_index_for_child(start - 1) else { break };
        let group = data.groups[group_index];
        let geometry = geometries[group_index];
        let columns = geometry.columns.max(1);
        let row_height = f32::from(group.policy.sizing.height(geometry.child_width, rem_size)).max(1.0);
        let row_gap = f32::from(group.policy.row_gap);
        // Joining this group to the rows already taken costs one gap, and rows
        // that straddle a group boundary take the larger of the two policies.
        let boundary_gap = adjacent_group.map_or(0.0, |next| f32::from(data.groups[next].policy.row_gap).max(row_gap));

        let header = f32::from(header_height(data, group_index, geometry.child_width, rem_size));
        let available = budget - used - boundary_gap - header;
        let mut fit = if available < 0.0 { 0 } else { ((available + row_gap) / (row_height + row_gap)).floor() as usize };
        if let Some(maximum_rows) = maximum_rows {
            fit = fit.min(maximum_rows.saturating_sub(rows_taken));
        }

        let children_here = start - group.start;
        // Intrinsic rows are counted from the requested end using the same
        // width budget. The chosen range is then drawn in reading order.
        let mut starts = Vec::new();
        if group.policy.intrinsic {
            let mut end = start;
            while end > group.start && starts.len() <= fit.max(1) {
                let mut left = end;
                let mut occupied = Pixels::ZERO;
                while left > group.start {
                    let width = data.children[left - 1].measured_width.get().max(px(1.0)).min(geometry.child_width);
                    let next = occupied + width + if left == end { Pixels::ZERO } else { geometry.column_gap };
                    if left < end && next > geometry.child_width {
                        break;
                    }
                    occupied = next;
                    left -= 1;
                }
                starts.push(left);
                end = left;
            }
        }
        let need = if group.policy.intrinsic { starts.len() } else { children_here.div_ceil(columns) };
        let mut take = fit.min(need);
        if take == 0 {
            // A page is never empty, even when its only row is too tall.
            if rows_taken > 0 || group.header_sizing.is_some() {
                break;
            }
            take = 1;
        }

        start = if group.policy.intrinsic { starts[take - 1] } else { start - (take * columns).min(children_here) };
        used += header + take as f32 * row_height + take.saturating_sub(1) as f32 * row_gap + boundary_gap;
        rows_taken += take;
        adjacent_group = Some(group_index);
        if take < need {
            break;
        }
    }

    start
}

/// Rows of the page that ends just before `child_offset`.
///
/// The start is counted backward, then the rows are packed forward from it.
/// Fixed columns use division; intrinsic groups greedily pack from the end.
/// Packing the resulting contiguous range forward needs no more rows, and
/// preserves reading order even when the first or last row is partial.
fn backward_page_rows(data: &PaginatorData, geometries: &[GroupGeometry], child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    let page_end = child_offset.saturating_add(1).min(data.children.len());
    let start = backward_page_start(data, geometries, page_end, maximum_rows, available_height, rem_size);
    forward_rows_in_range(data, geometries, start, page_end)
}

pub(super) fn header_height(data: &PaginatorData, group_index: usize, width: Pixels, rem_size: Pixels) -> Pixels {
    data.groups[group_index].header_sizing.map_or(Pixels::ZERO, |sizing| sizing.height(width, rem_size))
}

/// A header is charged once per visible group segment, including continuations.
pub(super) fn row_header_height(data: &PaginatorData, previous: Option<&AssignedRow>, row: &AssignedRow, rem_size: Pixels) -> Pixels {
    if previous.is_none_or(|previous| previous.group_index != row.group_index) { header_height(data, row.group_index, row.child_width, rem_size) } else { Pixels::ZERO }
}

pub(super) fn rows_height(data: &PaginatorData, rows: &[AssignedRow], rem_size: Pixels) -> Pixels {
    let mut height = Pixels::ZERO;
    for (index, row) in rows.iter().enumerate() {
        height += row_header_height(data, index.checked_sub(1).map(|i| &rows[i]), row, rem_size);
        height += data.groups[row.group_index].policy.sizing.height(row.child_width, rem_size);
        if let Some(next) = rows.get(index + 1) {
            height += gap_between(data, row, next);
        }
    }
    height
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::widgets::{PaginatorSizing, PaginatorWidthPolicy};
    use gpui::rems;

    fn header_fixture() -> PaginatorData {
        use crate::library::widgets::{PaginatorChild, PaginatorGroup};
        use gpui::{IntoElement, div};
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(40.0)), px(8.0)).flush_edges();
        let children = |count| (0..count).map(|_| PaginatorChild::new(|_, _, _| div().into_any_element())).collect::<Vec<_>>();
        PaginatorData::new([
            PaginatorGroup::new(policy, children(8)).with_header(PaginatorSizing::pixels(px(30.0)), |_, _| div().into_any_element()),
            PaginatorGroup::new(policy, children(6)).with_header(PaginatorSizing::pixels(px(30.0)), |_, _| div().into_any_element()),
            PaginatorGroup::new(policy, children(2)),
        ])
    }

    #[test]
    fn intrinsic_rows_pack_widths_and_cap_oversized_children() {
        use crate::library::widgets::{PaginatorChild, PaginatorGroup};
        use gpui::{IntoElement, div};
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(1.0)), PaginatorSizing::pixels(px(40.0)), px(8.0)).intrinsic().with_gap(px(8.0)).with_edge_inset(px(8.0));
        let children = [60.0, 80.0, 120.0, 40.0, 900.0, 70.0, 60.0].map(|width| {
            let child = PaginatorChild::new(|_, _, _| div().into_any_element());
            child.measured_width.set(px(width));
            child
        });
        let data = PaginatorData::new([PaginatorGroup::new(policy, children).with_header(PaginatorSizing::pixels(px(30.0)), |_, _| div().into_any_element())]);
        let mut paginator = super::super::Paginator::new("intrinsic-navigation", []);
        paginator.data = std::rc::Rc::new(data);
        paginator.prefetched = Some(super::super::PrefetchKey { children_revision: 0, page: super::super::VisiblePage { first_child: 0, last_child: 6 }, width: px(300.0), height: px(500.0), rem_size: px(16.0) });
        use super::super::CursorMove;
        assert_eq!(paginator.cursor_target(2, CursorMove::Down), Some(3));
        assert_eq!(paginator.cursor_target(3, CursorMove::Up), Some(0));
        assert_eq!(paginator.cursor_target(4, CursorMove::Down), Some(6));
        assert_eq!(paginator.cursor_target(0, CursorMove::Up), None);
        paginator.visible_page = Some(super::super::VisiblePage { first_child: 3, last_child: 6 });
        assert_eq!(paginator.cursor_target(3, CursorMove::Up), Some(0));
        let data = &paginator.data;
        let geometries = group_geometries(&data, px(300.0), px(16.0));
        let rows = forward_rows_in_range(&data, &geometries, 0, 7);
        assert_eq!(rows.iter().map(|r| r.items.len()).collect::<Vec<_>>(), [3, 1, 1, 2]);
        assert_eq!(rows[0].items.iter().map(|i| i.width).collect::<Vec<_>>(), [px(60.0), px(80.0), px(120.0)]);
        assert_eq!(rows[0].items[2].x, px(164.0));
        assert_eq!(rows[2].items[0].width, px(284.0));
        for width in [100.0, 180.0, 300.0, 600.0] {
            let geometries = group_geometries(&data, px(width), px(16.0));
            for end in 1..=7 {
                for height in [69.0, 70.0, 118.0, 166.0] {
                    let rows = page_rows(&data, &geometries, AssignmentDirection::Backward, end - 1, None, px(height), px(16.0));
                    if height < 70.0 {
                        assert!(rows.is_empty());
                        continue;
                    }
                    assert_eq!(rows.last().unwrap().items.last().unwrap().child_index, end - 1);
                    assert!(rows_height(&data, &rows, px(16.0)) <= px(height));
                    let start = rows[0].items[0].child_index;
                    let forward = page_rows(&data, &geometries, AssignmentDirection::Forward, start, None, px(height), px(16.0));
                    assert!(forward.last().unwrap().items.last().unwrap().child_index >= end - 1);
                    for row in rows {
                        assert!(row.items.last().unwrap().x + row.items.last().unwrap().width <= px(width - 8.0));
                    }
                }
            }
        }
    }

    #[test]
    fn cloning_groups_preserves_headers_and_empty_groups_stay_invisible() {
        use crate::library::widgets::{Paginator, PaginatorChild, PaginatorGroup};
        use gpui::{IntoElement, div};
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(40.0)), px(8.0));
        let groups = [
            PaginatorGroup::new(policy, []).with_header(PaginatorSizing::pixels(px(30.0)), |_, _| div().into_any_element()),
            PaginatorGroup::new(policy, [PaginatorChild::new(|_, _, _| div().into_any_element())]).with_header(PaginatorSizing::pixels(px(30.0)), |_, _| div().into_any_element()),
        ];
        let paginator = Paginator::new("clone-headers", groups);
        let data = PaginatorData::new(paginator.cloned_groups());
        assert_eq!(data.groups.len(), 1);
        assert_eq!(data.children.len(), 1);
        assert!(data.headers[0].is_some());
        assert_eq!(header_height(&data, 0, px(100.0), px(16.0)), px(30.0));
    }

    #[test]
    fn headers_repeat_on_continuations_without_taking_child_indices() {
        let data = header_fixture();
        let geometries = group_geometries(&data, px(240.0), px(16.0));
        let page = |start| page_rows(&data, &geometries, AssignmentDirection::Forward, start, None, px(118.0), px(16.0));
        let first = page(0);
        let second = page(4);
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        assert_eq!(first[0].items[0].child_index, 0);
        assert_eq!(second[0].items[0].child_index, 4);
        assert_eq!(rows_height(&data, &first, px(16.0)), px(118.0));
        assert_eq!(rows_height(&data, &second, px(16.0)), px(118.0));
        assert_eq!(row_header_height(&data, None, &second[0], px(16.0)), px(30.0));
        assert_eq!(row_header_height(&data, Some(&second[0]), &second[1], px(16.0)), Pixels::ZERO);
        assert_eq!(data.children.len(), 16);
    }

    #[test]
    fn headers_and_first_rows_move_together_at_page_boundaries() {
        let data = header_fixture();
        let geometries = group_geometries(&data, px(240.0), px(16.0));
        for direction in [AssignmentDirection::Forward, AssignmentDirection::Backward] {
            let offset = if matches!(direction, AssignmentDirection::Forward) { 0 } else { 7 };
            assert!(page_rows(&data, &geometries, direction, offset, None, px(69.0), px(16.0)).is_empty());
            let exact = page_rows(&data, &geometries, direction, offset, None, px(70.0), px(16.0));
            assert_eq!(exact.len(), 1);
            assert_eq!(rows_height(&data, &exact, px(16.0)), px(70.0));
        }
        // Last row of first group fits; the next group's header + row does not.
        let page = page_rows(&data, &geometries, AssignmentDirection::Forward, 6, None, px(147.0), px(16.0));
        assert_eq!(page.len(), 1);
        let page = page_rows(&data, &geometries, AssignmentDirection::Forward, 6, None, px(148.0), px(16.0));
        assert_eq!(page.len(), 2);
        assert_ne!(page[0].group_index, page[1].group_index);
        assert_eq!(rows_height(&data, &page, px(16.0)), px(148.0));
    }

    #[test]
    fn forward_and_backward_header_costs_agree_across_group_boundaries() {
        let data = header_fixture();
        for width in [120.0, 240.0, 450.0] {
            let geometries = group_geometries(&data, px(width), px(16.0));
            for height in [70.0, 118.0, 148.0, 200.0, 500.0] {
                for end in 1..=data.children.len() {
                    let backward = page_rows(&data, &geometries, AssignmentDirection::Backward, end - 1, None, px(height), px(16.0));
                    if backward.is_empty() {
                        continue;
                    }
                    let start = backward[0].items[0].child_index;
                    let forward = forward_rows_in_range(&data, &geometries, start, end);
                    assert_eq!(rows_height(&data, &backward, px(16.0)), rows_height(&data, &forward, px(16.0)));
                    assert!(rows_height(&data, &backward, px(16.0)) <= px(height));
                    let fitting = page_rows(&data, &geometries, AssignmentDirection::Forward, start, None, px(height), px(16.0));
                    assert!(fitting.last().unwrap().items.last().unwrap().child_index >= end - 1);
                }
            }
        }
    }

    #[test]
    fn book_slack_grows_cards_by_up_to_25_percent_then_expands_gaps() {
        for policy in [
            crate::library::widgets::book_card_policy(false, app_preferences::CoverText::Always),
            crate::library::widgets::shelf_card_policy(app_preferences::CoverText::Always),
            crate::library::widgets::shelf_card_policy(app_preferences::CoverText::CoverOnly),
        ] {
            for rem_size in [px(16.0), px(20.0)] {
                let width = policy.width.width(rem_size);
                assert_eq!(width, policy.width.width(px(16.0)), "book widths must not scale with text");
                let exact = width * 3.0 + policy.column_gap * 2.0;
                let first = group_geometry(policy, 0, exact, rem_size);
                let wider = group_geometry(policy, 0, exact + px(60.0), rem_size);
                assert_eq!(first.columns, 3);
                assert_eq!(wider.columns, 3);
                assert_eq!(first.child_width, width);
                assert_eq!(wider.child_width, width + px(20.0));
                assert_eq!(wider.column_gap, first.column_gap);
                let maximum = width * 1.25;
                let capped = group_geometry(policy, 0, maximum * 3.0 + policy.column_gap * 2.0 + px(12.0), rem_size);
                assert_eq!(capped.columns, 3);
                assert_eq!(capped.child_width, maximum);
                assert_eq!(capped.column_gap, policy.column_gap + px(6.0));
                assert_eq!(wider.leading, Pixels::ZERO);
                assert!(policy.sizing.height(wider.child_width, rem_size) > policy.sizing.height(first.child_width, rem_size));
                let fourth = group_geometry(policy, 0, width * 4.0 + policy.column_gap * 3.0, rem_size);
                assert_eq!(fourth.columns, 4);
                assert_eq!(fourth.child_width, width);
            }
        }
    }

    /// A shelf row holds a second paginator, and its cards stretch. A row that
    /// reserved their nominal height would clip the bottom of every stretched
    /// cover and the title under it, so the row is measured against what the
    /// cards actually resolve to — at every width, not just the nominal one.
    #[test]
    fn shelf_rows_are_as_tall_as_the_cards_they_hold() {
        let rem_size = px(16.0);
        for cover_text in [app_preferences::CoverText::Always, app_preferences::CoverText::CoverOnly] {
            let cards = crate::library::widgets::shelf_card_policy(cover_text);
            let row = crate::library::widgets::shelf_rows_policy(cover_text);
            let heading = px(16.0 * ui_components::SHELF_HEADING_HEIGHT_REM + ui_components::CAROUSEL_CONTENT_GAP);
            // Nominal, stretched, and capped: the three widths a card resolves to.
            let nominal = px(ui_components::BOOK_CARD_WIDTH) * 4.0 + cards.column_gap * 3.0;
            for width in [nominal, nominal + px(120.0), px(4000.0)] {
                let card = cards.sizing.height(group_geometry(cards, 0, width, rem_size).child_width, rem_size);
                assert_eq!(row.sizing.height(width, rem_size), heading + card, "a {width:?} row must be its heading over a {card:?} card");
            }
            let stretched = row.sizing.height(nominal + px(120.0), rem_size);
            assert!(stretched > row.sizing.height(nominal, rem_size), "a row whose cards stretched must grow with them");
        }
    }

    #[test]
    fn detailed_cards_grow_to_the_limit_then_expand_gaps() {
        let policy = crate::library::widgets::book_card_policy(true, app_preferences::CoverText::Always);
        let rem_size = px(16.0);
        let preferred = policy.width.width(rem_size);
        let maximum = policy.maximum_width.unwrap().width(rem_size);
        let larger_font = px(20.0);
        assert_eq!(policy.width.width(larger_font) - preferred, px(4.0 * ui_components::BOOK_CARD_DETAIL_TEXT_WIDTH_REM));
        assert_eq!(policy.maximum_width.unwrap().width(larger_font) - maximum, px(4.0 * ui_components::BOOK_CARD_DETAIL_TEXT_MAXIMUM_WIDTH_REM));
        assert_eq!(preferred, px(ui_components::BOOK_CARD_DETAIL_FIXED_WIDTH + 16.0 * ui_components::BOOK_CARD_DETAIL_TEXT_WIDTH_REM));
        let resolve = |width| group_geometry(policy, 0, width, rem_size);
        let base = resolve(preferred * 2.0 + policy.column_gap);
        let growing = resolve(preferred * 2.0 + policy.column_gap + px(100.0));
        assert_eq!(growing.columns, 2);
        assert_eq!(growing.child_width, preferred + px(50.0));
        assert_eq!(growing.column_gap, policy.column_gap);
        let capped = resolve(maximum * 2.0 + policy.column_gap + px(16.0));
        assert_eq!(capped.columns, 2);
        assert_eq!(capped.child_width, maximum);
        assert_eq!(capped.column_gap, policy.column_gap + px(16.0));
        assert_eq!(policy.sizing.height(base.child_width, rem_size), policy.sizing.height(capped.child_width, rem_size));
        let narrow = resolve(px(280.0));
        assert_eq!(narrow.child_width, px(280.0));
        assert!(policy.sizing.height(narrow.child_width, rem_size) < policy.sizing.height(base.child_width, rem_size));
    }

    #[test]
    fn mobile_book_carousels_keep_two_cards_visible() {
        for cover_text in [app_preferences::CoverText::Always, app_preferences::CoverText::CoverOnly] {
            let policy = crate::library::widgets::shelf_card_policy(cover_text);
            for width in [280.0, 320.0, 390.0] {
                let geometry = group_geometry(policy, 0, px(width), px(16.0));
                assert_eq!(geometry.columns, 2);
                assert!(geometry.child_width * 2.0 <= px(width));
            }
        }
    }

    /// The detailed card keeps the ordinary cover and its text block while
    /// giving the blurb a separate column.
    #[test]
    fn detailed_cards_keep_the_ordinary_cover_size() {
        let rem_size = px(16.0);
        let container = px(1600.0);
        let compact = crate::library::widgets::book_card_policy(false, app_preferences::CoverText::Always);
        let detailed = crate::library::widgets::book_card_policy(true, app_preferences::CoverText::Always);
        let compact_geometry = group_geometry(compact, 0, container, rem_size);
        let detailed_geometry = group_geometry(detailed, 0, container, rem_size);
        assert!(detailed_geometry.columns < compact_geometry.columns);

        // What each card holds back for the words under its cover, which is the
        // same block in both modes. Only the detailed policy caps its content
        // width (`with_aspect_width_limit`) — a compact cover is free to
        // stretch past `BOOK_CARD_CONTENT_WIDTH`, and this must not cap it
        // where the real geometry does not.
        let padding = px(ui_components::BOOK_CARD_PADDING);
        let text_block = |policy: PaginatorGroupPolicy, child_width: Pixels, cover_fraction: f32, capped: bool| {
            let cover_width = (child_width - padding * 2.0) * cover_fraction;
            let cover_width = if capped { cover_width.min(px(ui_components::BOOK_CARD_CONTENT_WIDTH)) } else { cover_width };
            f32::from(policy.sizing.height(child_width, rem_size)) - f32::from(cover_width) / ui_components::BOOK_COVER_ASPECT_RATIO
        };
        let compact_text = text_block(compact, compact_geometry.child_width, 1.0, false);
        let detailed_text = text_block(detailed, detailed_geometry.child_width, ui_components::BOOK_CARD_DETAIL_COVER_FRACTION, true);
        assert!((compact_text - detailed_text).abs() < 0.5, "detailed cards reserve {detailed_text} for their words where ordinary cards reserve {compact_text}");

        // And the cover itself stays about the size it is in the grid the
        // reader switched from.
        let compact_cover = compact_geometry.child_width - padding * 2.0;
        let detailed_cover = ((detailed_geometry.child_width - padding * 2.0) * ui_components::BOOK_CARD_DETAIL_COVER_FRACTION).min(px(ui_components::BOOK_CARD_CONTENT_WIDTH));
        assert!((f32::from(compact_cover) - f32::from(detailed_cover)).abs() <= f32::from(padding));
    }

    #[test]
    fn mobile_chips_fit_two_columns_for_single_and_multiple_rows() {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::new(rems(24.5)), PaginatorSizing::rems(rems(3.5)), px(4.0)).with_gap(px(4.0)).with_minimum_columns(2).flush_edges();
        for width in [280.0, 320.0, 390.0] {
            let geometry = group_geometry(policy, 0, px(width), px(16.0));
            assert_eq!(geometry.columns, 2);
            assert_eq!(geometry.child_width * 2.0 + geometry.column_gap, px(width));
        }
    }
}
