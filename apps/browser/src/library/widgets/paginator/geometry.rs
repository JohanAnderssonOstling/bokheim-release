//! Pure paginator geometry. This module does not render or retain GPUI
//! elements; it turns explicit groups into stable canonical rows.
//!
//! Grid groups resolve a column count and shared width. Intrinsic groups pack
//! measured child widths in reading order with fixed gaps. Both use the same
//! row-height budget for pagination.

use super::slots::Slots;
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

pub(super) struct AssignedRow {
    pub(super) group_index: usize,
    pub(super) items: Vec<PageItem>,
    pub(super) child_width: Pixels,
    pub(super) column_gap: Pixels,
    pub(super) top: Option<Pixels>,
}

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
    let strip = data.groups.iter().any(|group| group.policy.column_fraction.is_some());
    data.groups
        .iter()
        .enumerate()
        .map(|(group_index, group)| {
            let mut geometry = group_geometry(group.policy, group_index, available_width, rem_size);
            if strip {
                geometry.columns = 1;
                geometry.leading = Pixels::ZERO;
                geometry.child_width = group.policy.column_fraction.map_or_else(|| group.policy.width.width(rem_size).min(available_width * 0.5), |fraction| available_width * fraction.clamp(0.01, 1.0)).max(px(1.0));
            }
            geometry
        })
        .collect()
}

pub(super) fn next_forward_row(data: &PaginatorData, widths: &Slots<Pixels>, geometries: &[GroupGeometry], next_child: &mut usize) -> Option<AssignedRow> {
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
        let width = if group.policy.intrinsic { widths.get(row_end).copied().unwrap_or_default().max(px(1.0)).min(geometry.child_width) } else { geometry.child_width };
        if !items.is_empty() && (if group.policy.intrinsic { x + width > geometry.leading + geometry.child_width } else { items.len() >= geometry.columns }) {
            break;
        }
        items.push(PageItem { child_index: row_end, x, width });
        x += width + geometry.column_gap;
        row_end += 1;
    }
    *next_child = row_end;
    Some(AssignedRow { group_index, items, child_width: geometry.child_width, column_gap: geometry.column_gap, top: None })
}

/// Normalizes an exact child range forward. The start remains stable and only
/// the final row of each group can be incomplete.
pub(super) fn forward_rows_in_range(data: &PaginatorData, widths: &Slots<Pixels>, geometries: &[GroupGeometry], start: usize, end: usize) -> Vec<AssignedRow> {
    let end = end.min(data.children.len());
    let mut next_child = start.min(end);
    let mut rows = Vec::new();
    while next_child < end {
        let Some(mut row) = next_forward_row(data, widths, geometries, &mut next_child) else {
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
pub(super) fn gap_between(data: &PaginatorData, previous_group: usize, next_group: usize) -> Pixels {
    px(f32::from(data.groups[previous_group].policy.row_gap).max(f32::from(data.groups[next_group].policy.row_gap)))
}

pub(super) fn page_rows(data: &PaginatorData, widths: &Slots<Pixels>, geometries: &[GroupGeometry], direction: AssignmentDirection, child_offset: usize, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    if data.groups.iter().any(|group| group.policy.column_fraction.is_some()) {
        let start = match direction {
            AssignmentDirection::Forward => child_offset,
            AssignmentDirection::Backward => strip_page_start(data, geometries, child_offset.saturating_add(1), available_height, rem_size),
        };
        let end = match direction {
            AssignmentDirection::Forward => data.children.len(),
            AssignmentDirection::Backward => child_offset.saturating_add(1).min(data.children.len()),
        };
        return strip_rows(data, geometries, start, end, available_height, rem_size);
    }
    match direction {
        AssignmentDirection::Forward => forward_page_rows(data, widths, geometries, child_offset, available_height, rem_size),
        AssignmentDirection::Backward => backward_page_rows(data, widths, geometries, child_offset, available_height, rem_size),
    }
}

/// Number of entry rows in a column; cover groups use one full-height cell.
fn strip_column_rows(policy: PaginatorGroupPolicy, width: Pixels, height: Pixels, rem_size: Pixels) -> usize {
    if policy.column_fraction.is_none() {
        return 1;
    }
    let row_height = policy.sizing.height(width, rem_size);
    ((f32::from(height + policy.row_gap) / f32::from(row_height + policy.row_gap)).floor() as usize).max(1)
}

fn strip_width(data: &PaginatorData, geometries: &[GroupGeometry]) -> Pixels {
    data.groups.iter().zip(geometries).find_map(|(group, geometry)| group.policy.column_fraction.map(|fraction| geometry.child_width / fraction.clamp(0.01, 1.0))).unwrap_or(px(1.0))
}

/// Packs entry columns followed by covers, retaining one contiguous child range
/// so paging, lazy loading and selection use the same identities as a grid.
pub(super) fn strip_rows(data: &PaginatorData, geometries: &[GroupGeometry], start: usize, end: usize, height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    let available = strip_width(data, geometries);
    let mut rows = Vec::new();
    let mut next = start;
    let mut x = Pixels::ZERO;
    while next < end.min(data.children.len()) {
        let Some(group_index) = data.group_index_for_child(next) else { break };
        let group = data.groups[group_index];
        let geometry = geometries[group_index];
        if !rows.is_empty() && x + geometry.child_width > available + px(0.01) {
            break;
        }
        let count = strip_column_rows(group.policy, geometry.child_width, height, rem_size).min(group.end.min(end) - next);
        let row_height = group.policy.sizing.height(geometry.child_width, rem_size).min(height.max(px(1.0)));
        for row in 0..count {
            rows.push(AssignedRow {
                group_index,
                items: vec![PageItem { child_index: next, x, width: geometry.child_width }],
                child_width: geometry.child_width,
                column_gap: geometry.column_gap,
                top: Some((row_height + group.policy.row_gap) * row as f32),
            });
            next += 1;
        }
        x += geometry.child_width + geometry.column_gap;
    }
    rows
}

fn strip_page_start(data: &PaginatorData, geometries: &[GroupGeometry], end: usize, height: Pixels, rem_size: Pixels) -> usize {
    let available = strip_width(data, geometries);
    let mut start = end.min(data.children.len());
    let mut used = Pixels::ZERO;
    while start > 0 {
        let Some(group_index) = data.group_index_for_child(start - 1) else { break };
        let group = data.groups[group_index];
        let geometry = geometries[group_index];
        if start < end && used + geometry.child_width > available + px(0.01) {
            break;
        }
        let count = strip_column_rows(group.policy, geometry.child_width, height, rem_size).min(start - group.start);
        start -= count;
        used += geometry.child_width + geometry.column_gap;
    }
    start
}

fn forward_page_rows(data: &PaginatorData, widths: &Slots<Pixels>, geometries: &[GroupGeometry], child_offset: usize, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    let mut next_child = child_offset.min(data.children.len());
    let mut rows = Vec::new();
    while let Some(row) = next_forward_row(data, widths, geometries, &mut next_child) {
        rows.push(row);
        if rows_height(data, &rows, rem_size) > available_height.max(px(1.0)) && rows.len() > 1 {
            rows.pop();
            break;
        }
    }
    rows
}

/// The first child of a page that ends just before `page_end`.
///
/// Walks backward one group at a time. Uniform row heights determine how
/// many rows fit; intrinsic widths determine which children those rows hold.
fn backward_page_start(data: &PaginatorData, widths: &Slots<Pixels>, geometries: &[GroupGeometry], page_end: usize, available_height: Pixels, rem_size: Pixels) -> usize {
    let budget = f32::from(available_height).max(1.0);
    let mut start = page_end;
    let mut used = 0.0;
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

        let available = budget - used - boundary_gap;
        let fit = if available < 0.0 { 0 } else { ((available + row_gap) / (row_height + row_gap)).floor() as usize };

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
                    let width = widths.get(left - 1).copied().unwrap_or_default().max(px(1.0)).min(geometry.child_width);
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
            if adjacent_group.is_some() {
                break;
            }
            take = 1;
        }

        start = if group.policy.intrinsic { starts[take - 1] } else { start - (take * columns).min(children_here) };
        used += take as f32 * row_height + take.saturating_sub(1) as f32 * row_gap + boundary_gap;
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
fn backward_page_rows(data: &PaginatorData, widths: &Slots<Pixels>, geometries: &[GroupGeometry], child_offset: usize, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    let page_end = child_offset.saturating_add(1).min(data.children.len());
    let start = backward_page_start(data, widths, geometries, page_end, available_height, rem_size);
    forward_rows_in_range(data, widths, geometries, start, page_end)
}

pub(super) fn rows_height(data: &PaginatorData, rows: &[AssignedRow], rem_size: Pixels) -> Pixels {
    let mut height = Pixels::ZERO;
    for (index, row) in rows.iter().enumerate() {
        height += data.groups[row.group_index].policy.sizing.height(row.child_width, rem_size);
        if let Some(next) = rows.get(index + 1) {
            height += gap_between(data, row.group_index, next.group_index);
        }
    }
    height
}

#[cfg(test)]
mod tests {
    use super::super::{PaginatorChild, PaginatorGroup, PaginatorSizing, PaginatorWidthPolicy};
    use super::*;

    fn intrinsic_data() -> PaginatorData {
        let mut policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(10.0)), px(0.0));
        policy.intrinsic = true;
        policy.column_gap = px(0.0);
        PaginatorData::new([PaginatorGroup::new(policy, (0..3).map(|_| PaginatorChild::new(|_, _, _| unreachable!())))])
    }

    fn child_indices(rows: Vec<AssignedRow>) -> Vec<Vec<usize>> {
        rows.into_iter().map(|row| row.items.into_iter().map(|item| item.child_index).collect()).collect()
    }

    fn mixed_strip(entries: usize, covers: usize) -> PaginatorData {
        let mut entry_policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(300.0)), PaginatorSizing::pixels(px(24.0)), px(0.0)).with_gap(px(0.0));
        entry_policy.column_fraction = Some(0.5);
        let cover_policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(166.0)), PaginatorSizing::pixels(px(220.0)), px(0.0)).with_gap(px(0.0));
        PaginatorData::new([PaginatorGroup::new(entry_policy, (0..entries).map(|_| PaginatorChild::new(|_, _, _| unreachable!()))), PaginatorGroup::new(cover_policy, (0..covers).map(|_| PaginatorChild::new(|_, _, _| unreachable!())))])
    }

    #[test]
    fn mixed_strips_place_entry_columns_in_half_the_width_with_covers_beside_them() {
        let data = mixed_strip(4, 20);
        for width in [160.0, 332.0, 664.0, 1200.0] {
            let geometries = group_geometries(&data, px(width), px(16.0));
            let rows = page_rows(&data, &Slots::default(), &geometries, AssignmentDirection::Forward, 0, px(220.0), px(16.0));
            assert!(rows.len() > 4, "the first page must include covers at width {width}");
            for (index, row) in rows[..4].iter().enumerate() {
                assert_eq!(row.items[0].x, px(0.0));
                assert_eq!(row.items[0].width, px(width * 0.5));
                assert_eq!(row.top, Some(px(index as f32 * 24.0)));
            }
            assert_eq!(rows[4].items[0].x, px(width * 0.5));
            assert_eq!(rows[4].top, Some(px(0.0)));
            for row in rows {
                assert!(row.items[0].x + row.items[0].width <= px(width));
            }
        }
    }

    #[test]
    fn mixed_strips_page_every_entry_and_cover_in_both_directions() {
        let data = mixed_strip(33, 20);
        for width in [160.0, 332.0, 664.0] {
            let geometries = group_geometries(&data, px(width), px(16.0));
            for direction in [AssignmentDirection::Forward, AssignmentDirection::Backward] {
                let mut offset = if direction == AssignmentDirection::Forward { 0 } else { data.children.len() - 1 };
                let mut visited = Vec::new();
                loop {
                    let rows = page_rows(&data, &Slots::default(), &geometries, direction, offset, px(220.0), px(16.0));
                    let indices: Vec<_> = rows.iter().flat_map(|row| row.items.iter().map(|item| item.child_index)).collect();
                    let first = *indices.first().unwrap();
                    let last = *indices.last().unwrap();
                    assert_eq!(indices, (first..=last).collect::<Vec<_>>());
                    visited.extend(indices);
                    match direction {
                        AssignmentDirection::Forward if last + 1 < data.children.len() => offset = last + 1,
                        AssignmentDirection::Backward if first > 0 => offset = first - 1,
                        _ => break,
                    }
                }
                visited.sort_unstable();
                assert_eq!(visited, (0..data.children.len()).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn independent_measurement_snapshots_pack_the_same_definitions_differently() {
        let data = intrinsic_data();
        let geometry = group_geometries(&data, px(100.0), px(16.0));
        let small = vec![px(60.0), px(30.0), px(60.0)].into();
        let large = vec![px(80.0), px(70.0), px(80.0)].into();
        assert_eq!(child_indices(forward_rows_in_range(&data, &small, &geometry, 0, 3)), vec![vec![0, 1], vec![2]]);
        assert_eq!(child_indices(forward_rows_in_range(&data, &large, &geometry, 0, 3)), vec![vec![0], vec![1], vec![2]]);
        assert_eq!(child_indices(forward_rows_in_range(&data, &small, &geometry, 0, 3)), vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn forward_pages_respect_group_gaps_exact_budgets_and_oversized_first_rows() {
        let group = |height, gap, count| {
            let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(100.0)), PaginatorSizing::pixels(px(height)), px(gap));
            PaginatorGroup::new(policy, (0..count).map(|_| PaginatorChild::new(|_, _, _| unreachable!())))
        };
        let data = PaginatorData::new([group(10.0, 3.0, 2), group(20.0, 7.0, 1)]);
        let geometries = group_geometries(&data, px(100.0), px(16.0));
        let widths = vec![Pixels::ZERO; 3].into();
        let rows = |budget| page_rows(&data, &widths, &geometries, AssignmentDirection::Forward, 0, px(budget), px(16.0));
        let exact = rows(23.0);
        assert_eq!(rows_height(&data, &exact, px(16.0)), px(23.0));
        assert_eq!(child_indices(exact), vec![vec![0], vec![1]]);
        assert_eq!(child_indices(rows(22.0)), vec![vec![0]]);
        assert_eq!(child_indices(rows(0.0)), vec![vec![0]]);
        assert_eq!(child_indices(rows(50.0)), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn backward_pagination_uses_the_supplied_measurements() {
        let data = intrinsic_data();
        let geometry = group_geometries(&data, px(100.0), px(16.0));
        let widths = vec![px(60.0), px(60.0), px(40.0)].into();
        let backward = page_rows(&data, &widths, &geometry, AssignmentDirection::Backward, 2, px(10.0), px(16.0));
        assert_eq!(child_indices(backward), vec![vec![1, 2]]);
        let forward = page_rows(&data, &widths, &geometry, AssignmentDirection::Forward, 0, px(10.0), px(16.0));
        assert_eq!(child_indices(forward), vec![vec![0]]);
    }
}
