//! Pure paginator geometry. This module does not render or retain GPUI
//! elements; it turns explicit groups into stable canonical rows.

use gpui::{Pixels, px};

use super::{AssignmentDirection, PaginatorData, PaginatorGroupPolicy};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GroupGeometryCandidate {
    pub(super) columns: usize,
    pub(super) child_width: Pixels,
    pub(super) column_gap: Pixels,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GroupGeometry {
    pub(super) group_index: usize,
    pub(super) columns: usize,
    pub(super) child_width: Pixels,
    pub(super) column_gap: Pixels,
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
    pub(super) column: usize,
}

fn geometry_candidates_with_gap(policy: PaginatorGroupPolicy, child_count: usize, available_width: Pixels, rem_size: Pixels, minimum_gap_only: bool) -> Vec<GroupGeometryCandidate> {
    if child_count == 0 {
        return Vec::new();
    }

    let available_width = f32::from(available_width).max(1.0);
    let minimum_width = f32::from(policy.width.minimum_width(rem_size)).max(1.0);
    let preferred_width = f32::from(policy.width.preferred_width(rem_size)).max(minimum_width);
    let maximum_width = f32::from(policy.width.maximum_width(rem_size)).max(preferred_width);
    let minimum_gap = f32::from(policy.column_gap.minimum);
    let preferred_gap = f32::from(policy.column_gap.preferred);
    let maximum_columns = (((available_width + minimum_gap) / (minimum_width + minimum_gap)).floor() as usize).max(1).min(child_count).min(u16::MAX as usize);
    let mut candidates = Vec::with_capacity(maximum_columns);

    for columns in 1..=maximum_columns {
        let gaps = columns.saturating_sub(1) as f32;
        // When a geometry must contract, spend the gap range before reducing
        // item width. Once it has room to grow, keep the preferred gap and put
        // surplus width into the items.
        let column_gap = if minimum_gap_only {
            minimum_gap
        } else if gaps == 0.0 {
            preferred_gap
        } else {
            ((available_width - preferred_width * columns as f32) / gaps).clamp(minimum_gap, preferred_gap)
        };
        let width_limit = ((available_width - column_gap * gaps) / columns as f32).max(1.0).min(maximum_width);
        if width_limit + f32::EPSILON < minimum_width {
            continue;
        }
        let child_width = width_limit;
        candidates.push(GroupGeometryCandidate { columns, child_width: px(child_width), column_gap: px(column_gap) });
    }

    candidates
}

/// Every column-count geometry permitted by the width policy. The placement
/// phase uses this only after the child range has been locked, when expansion
/// is allowed to reflow those same children into a different number of rows.
pub(super) fn expansion_geometries(data: &PaginatorData, available_width: Pixels, rem_size: Pixels) -> Vec<Vec<GroupGeometry>> {
    data.groups
        .iter()
        .enumerate()
        .map(|(group_index, group)| {
            geometry_candidates_with_gap(group.policy, group.len(), available_width, rem_size, true)
                .into_iter()
                .map(|candidate| GroupGeometry { group_index, columns: candidate.columns, child_width: candidate.child_width, column_gap: candidate.column_gap })
                .collect()
        })
        .collect()
}

/// Smallest geometry used to probe whether the first row of a not-yet-visible
/// group can enter the page. The normal refinement phases may expand it after
/// that source row has been accepted.
pub(super) fn minimum_probe_geometry(data: &PaginatorData, group_index: usize, candidates: &[GroupGeometry], rem_size: Pixels) -> Option<GroupGeometry> {
    let candidate = candidates.iter().copied().max_by_key(|candidate| candidate.columns)?;
    let policy = data.groups[group_index].policy;
    Some(GroupGeometry { child_width: policy.width.minimum_width(rem_size), column_gap: policy.column_gap.minimum, ..candidate })
}

/// Preferred-width geometry after the column gaps have been reduced to their
/// minimum. This is the stable starting point for locked-range refinement.
pub(super) fn minimum_gap_preferred_geometries(data: &PaginatorData, available_width: Pixels, rem_size: Pixels) -> Vec<GroupGeometry> {
    let mut geometries = preferred_geometries(data, available_width, rem_size);
    for geometry in &mut geometries {
        geometry.column_gap = data.groups[geometry.group_index].policy.column_gap.minimum;
    }
    geometries
}

pub(super) fn preferred_geometries(data: &PaginatorData, available_width: Pixels, rem_size: Pixels) -> Vec<GroupGeometry> {
    data.groups
        .iter()
        .enumerate()
        .map(|(group_index, group)| {
            let preferred_width = f32::from(group.policy.width.preferred_width(rem_size)).max(1.0);
            let preferred_gap = f32::from(group.policy.column_gap.preferred);
            let available_width_f = f32::from(available_width).max(1.0);
            let preferred_columns = (((available_width_f + preferred_gap) / (preferred_width + preferred_gap)).floor() as usize).max(1).min(group.len());
            let gaps = preferred_columns.saturating_sub(1) as f32;
            let maximum_width = f32::from(group.policy.width.maximum_width(rem_size));
            let available_child_width = (available_width_f - preferred_gap * gaps) / preferred_columns as f32;
            // Baseline geometry starts at the declared preferred width. Free
            // horizontal space is consumed only by locked-range expansion;
            // otherwise widening a viewport would make aspect-ratio rows
            // taller and could remove already-visible children.
            let child_width = preferred_width.min(available_child_width).min(maximum_width).max(1.0);
            GroupGeometry { group_index, columns: preferred_columns, child_width: px(child_width), column_gap: px(preferred_gap) }
        })
        .collect()
}

pub(super) fn preferred_fallback_width(data: &PaginatorData, rem_size: Pixels) -> Pixels {
    px(data.groups.iter().map(|group| f32::from(group.policy.width.preferred_width(rem_size))).fold(1.0, f32::max))
}

/// Consumes harmless horizontal slack after child widths and the visible
/// range are locked. A group uses one gap across all of its rows, so incomplete
/// rows retain the same column positions as complete rows.
pub(super) fn expand_horizontal_gaps(data: &PaginatorData, geometries: &mut [GroupGeometry], available_width: Pixels) {
    let available_width = f32::from(available_width).max(1.0);
    for geometry in geometries {
        if geometry.columns < 2 {
            continue;
        }
        let gaps = geometry.columns.saturating_sub(1) as f32;
        let available_gap = (available_width - f32::from(geometry.child_width) * geometry.columns as f32) / gaps;
        let maximum = f32::from(data.groups[geometry.group_index].policy.column_gap.maximum);
        geometry.column_gap = px(f32::from(geometry.column_gap).max(available_gap.min(maximum)));
    }
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
    let row_end = (row_start + geometry.columns).min(group.end);
    let items = (row_start..row_end).enumerate().map(|(column, child_index)| PageItem { child_index, column }).collect();
    *next_child = row_end;
    Some(AssignedRow { group_index, items, child_width: geometry.child_width, column_gap: geometry.column_gap })
}

/// Pulls one source row from immediately before the exclusive boundary.
/// Unlike forward assignment, these source rows are not final geometry: the
/// complete preceding range is normalized forward before it is measured.
pub(super) fn previous_row(data: &PaginatorData, geometries: &[GroupGeometry], previous_child: &mut usize) -> Option<AssignedRow> {
    if *previous_child == 0 {
        return None;
    }
    let group_index = data.group_index_for_child(*previous_child - 1)?;
    let group = data.groups[group_index];
    let geometry = geometries[group_index];
    debug_assert_eq!(geometry.group_index, group_index);
    let row_end = *previous_child;
    let row_start = row_end.saturating_sub(geometry.columns).max(group.start);
    let items = (row_start..row_end).enumerate().map(|(column, child_index)| PageItem { child_index, column }).collect();
    *previous_child = row_start;
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

pub(super) fn preferred_gap_between(data: &PaginatorData, previous: &AssignedRow, next: &AssignedRow) -> Pixels {
    px(f32::from(data.groups[previous.group_index].policy.row_gap.preferred).max(f32::from(data.groups[next.group_index].policy.row_gap.preferred)))
}

pub(super) fn minimum_gap_between(data: &PaginatorData, previous: &AssignedRow, next: &AssignedRow) -> Pixels {
    px(f32::from(data.groups[previous.group_index].policy.row_gap.minimum).max(f32::from(data.groups[next.group_index].policy.row_gap.minimum)))
}

pub(super) fn maximum_gap_between(data: &PaginatorData, previous: &AssignedRow, next: &AssignedRow) -> Pixels {
    px(f32::from(data.groups[previous.group_index].policy.row_gap.maximum).max(f32::from(data.groups[next.group_index].policy.row_gap.maximum)))
}

pub(super) fn estimated_page_rows(data: &PaginatorData, geometries: &[GroupGeometry], direction: AssignmentDirection, child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    estimated_page_rows_with_gaps(data, geometries, direction, child_offset, maximum_rows, available_height, rem_size, false)
}

pub(super) fn estimated_page_rows_with_minimum_gaps(
    data: &PaginatorData, geometries: &[GroupGeometry], direction: AssignmentDirection, child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels,
) -> Vec<AssignedRow> {
    estimated_page_rows_with_gaps(data, geometries, direction, child_offset, maximum_rows, available_height, rem_size, true)
}

fn estimated_page_rows_with_gaps(
    data: &PaginatorData, geometries: &[GroupGeometry], direction: AssignmentDirection, child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels, minimum_gaps: bool,
) -> Vec<AssignedRow> {
    match direction {
        AssignmentDirection::Forward => estimated_forward_rows_with_gaps(data, geometries, child_offset, maximum_rows, available_height, rem_size, minimum_gaps),
        AssignmentDirection::Backward => estimated_backward_rows_with_gaps(data, geometries, child_offset, maximum_rows, available_height, rem_size, minimum_gaps),
    }
}

#[cfg(test)]
fn estimated_forward_rows(data: &PaginatorData, geometries: &[GroupGeometry], child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    estimated_forward_rows_with_gaps(data, geometries, child_offset, maximum_rows, available_height, rem_size, false)
}

fn estimated_forward_rows_with_gaps(data: &PaginatorData, geometries: &[GroupGeometry], child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels, minimum_gaps: bool) -> Vec<AssignedRow> {
    let mut next_child = child_offset.min(data.children.len());
    let mut rows = Vec::new();
    while maximum_rows.is_none_or(|maximum_rows| rows.len() < maximum_rows) {
        let Some(row) = next_forward_row(data, geometries, &mut next_child) else {
            break;
        };
        let mut candidate = rows.clone();
        candidate.push(row);
        if !rows.is_empty() && estimated_height_with_gaps(data, &candidate, rem_size, minimum_gaps) > available_height.max(px(1.0)) {
            break;
        }
        rows = candidate;
    }
    rows
}

#[cfg(test)]
fn estimated_backward_rows(data: &PaginatorData, geometries: &[GroupGeometry], child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels) -> Vec<AssignedRow> {
    estimated_backward_rows_with_gaps(data, geometries, child_offset, maximum_rows, available_height, rem_size, false)
}

fn estimated_backward_rows_with_gaps(data: &PaginatorData, geometries: &[GroupGeometry], child_offset: usize, maximum_rows: Option<usize>, available_height: Pixels, rem_size: Pixels, minimum_gaps: bool) -> Vec<AssignedRow> {
    let page_end = child_offset.saturating_add(1).min(data.children.len());
    let mut previous_child = page_end;
    let mut accepted = Vec::new();
    while previous_row(data, geometries, &mut previous_child).is_some() {
        let candidate = forward_rows_in_range(data, geometries, previous_child, page_end);
        if maximum_rows.is_some_and(|maximum_rows| candidate.len() > maximum_rows) {
            break;
        }
        if !accepted.is_empty() && estimated_height_with_gaps(data, &candidate, rem_size, minimum_gaps) > available_height.max(px(1.0)) {
            break;
        }
        accepted = candidate;
    }
    accepted
}

pub(super) fn estimated_height(data: &PaginatorData, rows: &[AssignedRow], rem_size: Pixels) -> Pixels {
    estimated_height_with_gaps(data, rows, rem_size, false)
}

fn estimated_height_with_gaps(data: &PaginatorData, rows: &[AssignedRow], rem_size: Pixels, minimum_gaps: bool) -> Pixels {
    let mut height = Pixels::ZERO;
    for (index, row) in rows.iter().enumerate() {
        height += data.groups[row.group_index].policy.sizing.height(row.child_width, rem_size);
        if let Some(next) = rows.get(index + 1) {
            height += if minimum_gaps { minimum_gap_between(data, row, next) } else { preferred_gap_between(data, row, next) };
        }
    }
    height
}

#[cfg(test)]
mod tests {
    use super::super::{PaginatorChild, PaginatorData, PaginatorGapPolicy, PaginatorGroup, PaginatorSizing, PaginatorWidthPolicy};
    use super::*;
    use gpui::prelude::*;
    use gpui::{div, rems};

    fn policy() -> PaginatorGroupPolicy {
        PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(10.0)), PaginatorSizing::aspect_ratio(1.0), PaginatorGapPolicy::fixed(px(4.0)), PaginatorGapPolicy::fixed(px(4.0)))
    }

    fn data_with_group_lengths(lengths: &[usize]) -> PaginatorData {
        PaginatorData::new(lengths.iter().copied().map(|length| {
            let children = (0..length).map(|_| PaginatorChild::new(|_, _| div().into_any_element()));
            PaginatorGroup::new(policy(), children)
        }))
    }

    fn group_with_policy(policy: PaginatorGroupPolicy, length: usize) -> PaginatorGroup {
        PaginatorGroup::new(policy, (0..length).map(|_| PaginatorChild::new(|_, _| div().into_any_element())))
    }

    fn child_indices(rows: &[AssignedRow]) -> Vec<usize> {
        rows.iter().flat_map(|row| row.items.iter().map(|item| item.child_index)).collect()
    }

    fn assert_only_group_final_rows_are_incomplete(rows: &[AssignedRow], geometries: &[GroupGeometry]) {
        for (index, row) in rows.iter().enumerate() {
            let followed_by_same_group = rows.get(index + 1).is_some_and(|next| next.group_index == row.group_index);
            if followed_by_same_group {
                assert_eq!(row.items.len(), geometries[row.group_index].columns, "non-final row {index} in group {} was incomplete", row.group_index);
            }
        }
    }

    #[test]
    fn candidates_preserve_canonical_full_rows_and_one_final_remainder() {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::new(rems(8.0), rems(10.0), rems(14.0)), PaginatorSizing::aspect_ratio(1.0), PaginatorGapPolicy::new(px(2.0), px(4.0)), PaginatorGapPolicy::new(px(4.0), px(8.0)));
        let candidates = geometry_candidates_with_gap(policy, 18, px(1_000.0), px(10.0), false);

        assert!(candidates.iter().any(|candidate| candidate.columns == 6));
        assert!(candidates.iter().all(|candidate| candidate.child_width >= policy.width.minimum_width(px(10.0))));
        assert!(candidates.windows(2).all(|pair| pair[0].columns < pair[1].columns));
    }

    #[test]
    fn minimum_gap_starting_geometry_preserves_columns_and_width_for_refinement() {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::new(rems(8.0), rems(10.0), rems(14.0)), PaginatorSizing::pixels(px(40.0)), PaginatorGapPolicy::new(px(2.0), px(8.0)), PaginatorGapPolicy::new(px(2.0), px(8.0)));
        let data = PaginatorData::new([group_with_policy(policy, 20)]);

        let preferred = preferred_geometries(&data, px(420.0), px(10.0));
        let compact = minimum_gap_preferred_geometries(&data, px(420.0), px(10.0));

        assert_eq!(preferred[0].columns, 3);
        assert_eq!(compact[0].columns, preferred[0].columns);
        assert_eq!(compact[0].child_width, px(100.0));
        assert_eq!(compact[0].column_gap, px(2.0));
    }

    #[test]
    fn widening_before_a_column_breakpoint_does_not_remove_aspect_rows() {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::new(rems(8.0), rems(10.0), rems(16.0)), PaginatorSizing::aspect_ratio(1.0), PaginatorGapPolicy::fixed(px(8.0)), PaginatorGapPolicy::fixed(px(8.0)));
        let data = PaginatorData::new([group_with_policy(policy, 30)]);
        let medium = preferred_geometries(&data, px(430.0), px(10.0));
        let wide = preferred_geometries(&data, px(499.0), px(10.0));

        assert_eq!(medium[0].columns, 4);
        assert_eq!(wide[0].columns, 4);
        assert_eq!(medium[0].child_width, px(100.0));
        assert_eq!(wide[0].child_width, px(100.0));

        let medium_rows = estimated_forward_rows(&data, &medium, 0, None, px(316.0), px(10.0));
        let wide_rows = estimated_forward_rows(&data, &wide, 0, None, px(316.0), px(10.0));
        assert_eq!(child_indices(&medium_rows), child_indices(&wide_rows));
        assert_eq!(child_indices(&wide_rows), (0..12).collect::<Vec<_>>());
    }

    #[test]
    fn widening_across_column_breakpoints_never_removes_visible_aspect_children() {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::new(rems(8.0), rems(10.0), rems(16.0)), PaginatorSizing::aspect_ratio(0.75), PaginatorGapPolicy::new(px(2.0), px(8.0)), PaginatorGapPolicy::new(px(2.0), px(8.0)));
        let data = PaginatorData::new([group_with_policy(policy, 80)]);
        let rem_size = px(10.0);
        let available_height = px(450.0);
        let mut previous_count = 0;

        // Start above the preferred item width so widening can only retain
        // preferred item size or cross a column breakpoint.
        for width in (320..=1_000).step_by(3) {
            let geometries = preferred_geometries(&data, px(width as f32), rem_size);
            let rows = estimated_forward_rows(&data, &geometries, 0, None, available_height, rem_size);
            let visible_count = child_indices(&rows).len();

            assert!(visible_count >= previous_count, "widening to {width}px removed visible children ({previous_count} -> {visible_count})");
            assert_only_group_final_rows_are_incomplete(&rows, &geometries);
            previous_count = visible_count;
        }
    }

    #[test]
    fn horizontal_gap_expansion_uses_one_bounded_gap_per_group() {
        let wide_gap = PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(40.0)), PaginatorGapPolicy::new(px(2.0), px(4.0)).with_maximum(px(20.0)), PaginatorGapPolicy::fixed(px(4.0)));
        let narrow_gap = PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(40.0)), PaginatorGapPolicy::new(px(2.0), px(4.0)).with_maximum(px(12.0)), PaginatorGapPolicy::fixed(px(4.0)));
        let data = PaginatorData::new([group_with_policy(wide_gap, 12), group_with_policy(narrow_gap, 7)]);
        let mut geometries = [GroupGeometry { group_index: 0, columns: 4, child_width: px(100.0), column_gap: px(4.0) }, GroupGeometry { group_index: 1, columns: 3, child_width: px(140.0), column_gap: px(4.0) }];

        expand_horizontal_gaps(&data, &mut geometries, px(500.0));

        assert_eq!(geometries[0].column_gap, px(20.0));
        assert_eq!(geometries[1].column_gap, px(12.0));
        assert_eq!(geometries[0].child_width, px(100.0));
        assert_eq!(geometries[1].child_width, px(140.0));
    }

    #[test]
    fn forward_anchor_stays_exact_after_column_count_changes() {
        let children = (0..20).map(|_| PaginatorChild::new(|_, _| div().into_any_element()));
        let data = PaginatorData::new([PaginatorGroup::new(policy(), children)]);
        let geometries = [GroupGeometry { group_index: 0, columns: 6, child_width: px(100.0), column_gap: px(4.0) }];
        // Child 8 was a boundary in a four-column layout, but lies inside the
        // second row after resizing to six columns.
        let mut anchor = 8;

        let row = next_forward_row(&data, &geometries, &mut anchor).expect("row starting at stable anchor");

        assert_eq!(row.items.iter().map(|item| item.child_index).collect::<Vec<_>>(), (8..14).collect::<Vec<_>>());
        assert_eq!(anchor, 14);
    }

    #[test]
    fn backward_range_normalizes_without_crossing_the_current_page_start() {
        let children = (0..20).map(|_| PaginatorChild::new(|_, _| div().into_any_element()));
        let data = PaginatorData::new([PaginatorGroup::new(policy(), children)]);
        let geometries = [GroupGeometry { group_index: 0, columns: 6, child_width: px(100.0), column_gap: px(4.0) }];
        let page_start = 8;
        let mut preceding_start = page_start;
        previous_row(&data, &geometries, &mut preceding_start).expect("first backward source row");
        previous_row(&data, &geometries, &mut preceding_start).expect("second backward source row");
        let rows = forward_rows_in_range(&data, &geometries, preceding_start, page_start);

        assert_eq!(rows.iter().map(|row| row.items.len()).collect::<Vec<_>>(), vec![6, 2]);
        assert_eq!(rows.iter().flat_map(|row| row.items.iter().map(|item| item.child_index)).collect::<Vec<_>>(), (0..page_start).collect::<Vec<_>>());
    }

    #[test]
    fn every_forward_anchor_remains_exact_and_contiguous_across_group_boundaries() {
        let data = data_with_group_lengths(&[7, 11, 2]);
        let geometries = [
            GroupGeometry { group_index: 0, columns: 3, child_width: px(120.0), column_gap: px(4.0) },
            GroupGeometry { group_index: 1, columns: 4, child_width: px(90.0), column_gap: px(6.0) },
            GroupGeometry { group_index: 2, columns: 5, child_width: px(70.0), column_gap: px(8.0) },
        ];

        for start in 0..data.children.len() {
            let mut next_child = start;
            let mut rows = Vec::new();
            while let Some(row) = next_forward_row(&data, &geometries, &mut next_child) {
                rows.push(row);
            }

            assert_eq!(child_indices(&rows), (start..data.children.len()).collect::<Vec<_>>(), "forward assignment changed stable start {start}");
            assert_only_group_final_rows_are_incomplete(&rows, &geometries);
        }
    }

    #[test]
    fn every_backward_candidate_stays_below_its_exclusive_boundary() {
        let data = data_with_group_lengths(&[7, 11, 2]);
        let geometries = [
            GroupGeometry { group_index: 0, columns: 3, child_width: px(120.0), column_gap: px(4.0) },
            GroupGeometry { group_index: 1, columns: 4, child_width: px(90.0), column_gap: px(6.0) },
            GroupGeometry { group_index: 2, columns: 5, child_width: px(70.0), column_gap: px(8.0) },
        ];

        for page_start in 1..=data.children.len() {
            let mut preceding_start = page_start;
            while previous_row(&data, &geometries, &mut preceding_start).is_some() {
                let rows = forward_rows_in_range(&data, &geometries, preceding_start, page_start);
                assert_eq!(child_indices(&rows), (preceding_start..page_start).collect::<Vec<_>>(), "backward normalization crossed boundary {page_start}");
                assert_only_group_final_rows_are_incomplete(&rows, &geometries);
            }
            assert_eq!(preceding_start, 0);
        }
    }

    #[test]
    fn geometry_changes_only_the_forward_page_tail() {
        let data = data_with_group_lengths(&[40]);
        let narrow = [GroupGeometry { group_index: 0, columns: 4, child_width: px(100.0), column_gap: px(4.0) }];
        let compact = [GroupGeometry { group_index: 0, columns: 6, child_width: px(70.0), column_gap: px(4.0) }];
        let stable_start = 7;
        let available_height = px(220.0);

        let narrow_rows = estimated_forward_rows(&data, &narrow, stable_start, None, available_height, px(16.0));
        let compact_rows = estimated_forward_rows(&data, &compact, stable_start, None, available_height, px(16.0));
        let narrow_children = child_indices(&narrow_rows);
        let compact_children = child_indices(&compact_rows);

        assert_eq!(narrow_children, (stable_start..15).collect::<Vec<_>>());
        assert_eq!(compact_children, (stable_start..25).collect::<Vec<_>>());
        assert_eq!(&compact_children[..narrow_children.len()], narrow_children.as_slice());
    }

    #[test]
    fn empty_groups_and_a_final_child_keep_valid_group_geometry() {
        let data = PaginatorData::new([group_with_policy(policy(), 0), group_with_policy(policy(), 1), group_with_policy(policy(), 0)]);
        let geometries = [GroupGeometry { group_index: 0, columns: 4, child_width: px(80.0), column_gap: px(4.0) }];
        let mut anchor = 0;

        let row = next_forward_row(&data, &geometries, &mut anchor).expect("single retained child");

        assert_eq!(data.groups.len(), 1);
        assert_eq!(child_indices(&[row]), vec![0]);
        assert_eq!(anchor, 1);
        assert!(next_forward_row(&data, &geometries, &mut anchor).is_none());
    }

    #[test]
    fn constrained_height_still_admits_one_row_and_honors_maximum_rows() {
        let data = data_with_group_lengths(&[8]);
        let geometries = [GroupGeometry { group_index: 0, columns: 2, child_width: px(100.0), column_gap: px(4.0) }];

        let clipped = estimated_forward_rows(&data, &geometries, 3, None, px(1.0), px(16.0));
        let capped = estimated_forward_rows(&data, &geometries, 0, Some(1), px(10_000.0), px(16.0));

        assert_eq!(child_indices(&clipped), vec![3, 4]);
        assert_eq!(child_indices(&capped), vec![0, 1]);
    }

    #[test]
    fn exact_fractional_height_boundary_is_inclusive() {
        let fixed_height = PaginatorSizing::pixels(px(10.25));
        let gap = PaginatorGapPolicy::fixed(px(0.5));
        let fixed_policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), fixed_height, gap, gap);
        let data = PaginatorData::new([group_with_policy(fixed_policy, 3)]);
        let geometries = [GroupGeometry { group_index: 0, columns: 1, child_width: px(80.0), column_gap: Pixels::ZERO }];

        let exact = estimated_forward_rows(&data, &geometries, 0, None, px(21.0), px(17.5));
        let below = estimated_forward_rows(&data, &geometries, 0, None, px(20.99), px(17.5));

        assert_eq!(child_indices(&exact), vec![0, 1]);
        assert_eq!(child_indices(&below), vec![0]);
    }

    #[test]
    fn mixed_group_row_gap_uses_the_larger_policy_once() {
        let first_policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(10.0)), PaginatorGapPolicy::fixed(Pixels::ZERO), PaginatorGapPolicy::new(px(2.0), px(4.0)).with_maximum(px(10.0)));
        let second_policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(15.0)), PaginatorGapPolicy::fixed(Pixels::ZERO), PaginatorGapPolicy::new(px(6.0), px(8.0)).with_maximum(px(12.0)));
        let data = PaginatorData::new([group_with_policy(first_policy, 1), group_with_policy(second_policy, 1)]);
        let geometries = [GroupGeometry { group_index: 0, columns: 1, child_width: px(80.0), column_gap: Pixels::ZERO }, GroupGeometry { group_index: 1, columns: 1, child_width: px(80.0), column_gap: Pixels::ZERO }];
        let rows = forward_rows_in_range(&data, &geometries, 0, 2);

        assert_eq!(minimum_gap_between(&data, &rows[0], &rows[1]), px(6.0));
        assert_eq!(preferred_gap_between(&data, &rows[0], &rows[1]), px(8.0));
        assert_eq!(maximum_gap_between(&data, &rows[0], &rows[1]), px(12.0));
        assert_eq!(estimated_height(&data, &rows, px(16.0)), px(33.0));
    }

    #[test]
    fn previous_then_next_restores_an_unchanged_page_boundary() {
        let data = data_with_group_lengths(&[20]);
        let geometries = [GroupGeometry { group_index: 0, columns: 3, child_width: px(50.0), column_gap: Pixels::ZERO }];
        let current_page_start = 7;
        let previous = estimated_backward_rows(&data, &geometries, current_page_start - 1, None, px(104.0), px(16.0));
        let restored_start = previous.last().and_then(|row| row.items.last()).map(|item| item.child_index + 1);

        assert_eq!(child_indices(&previous), (1..current_page_start).collect::<Vec<_>>());
        assert_eq!(restored_start, Some(current_page_start));
    }

    #[test]
    fn backward_row_limit_normalizes_across_group_boundaries_without_an_early_orphan() {
        let data = data_with_group_lengths(&[5, 7]);
        let geometries = [GroupGeometry { group_index: 0, columns: 3, child_width: px(50.0), column_gap: Pixels::ZERO }, GroupGeometry { group_index: 1, columns: 4, child_width: px(50.0), column_gap: Pixels::ZERO }];

        // The current page starts at child 7. Its preceding page may contain
        // only two canonical rows, even though the backward source rows cut
        // through both groups at partial boundaries.
        let previous = estimated_backward_rows(&data, &geometries, 6, Some(2), px(10_000.0), px(16.0));

        assert_eq!(child_indices(&previous), (2..7).collect::<Vec<_>>());
        assert_eq!(previous.iter().map(|row| row.items.len()).collect::<Vec<_>>(), vec![3, 2]);
        assert_only_group_final_rows_are_incomplete(&previous, &geometries);
    }
}
