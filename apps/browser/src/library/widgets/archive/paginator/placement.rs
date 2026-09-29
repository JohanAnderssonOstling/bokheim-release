//! Phase-based paginator placement selection.
//!
//! This module owns the ordering of geometry changes. Measurement remains in
//! `element`, while canonical row construction remains in `geometry`.

use gpui::{Pixels, px};

use super::geometry::{self, AssignedRow, GroupGeometry};
use super::{AssignmentDirection, PaginatorData, VisiblePage};

const WIDTH_EPSILON: f32 = 0.25;
const PARTIAL_EXPANSION_STEPS: usize = 10;

pub(super) struct ProfileRow {
    pub(super) assignment: AssignedRow,
    pub(super) height: Pixels,
}

pub(super) struct Placement {
    pub(super) rows: Vec<ProfileRow>,
    pub(super) geometries: Vec<GroupGeometry>,
}

impl Placement {
    pub(super) fn visible_page(&self) -> Option<VisiblePage> {
        let first_child = self.rows.first()?.assignment.items.first()?.child_index;
        let last_child = self.rows.last()?.assignment.items.last()?.child_index;
        Some(VisiblePage { first_child, last_child })
    }

    pub(super) fn group_is_visible(&self, group_index: usize) -> bool {
        self.rows.iter().any(|row| row.assignment.group_index == group_index)
    }

    pub(super) fn constrain_single_row_height(mut self, available_height: Pixels) -> Self {
        if let [row] = self.rows.as_mut_slice() {
            row.height = row.height.min(available_height.max(px(1.0)));
        }
        self
    }

    pub(super) fn fits(&self, data: &PaginatorData, available_height: Pixels, maximum_rows: Option<usize>) -> bool {
        if maximum_rows.is_some_and(|maximum_rows| self.rows.len() > maximum_rows) {
            return false;
        }
        // A paginator never becomes empty solely because its first row is too
        // tall. Rendering caps each child at the paginator height.
        if self.rows.len() <= 1 {
            return true;
        }

        let heights = self.rows.iter().map(|row| f32::from(row.height)).sum::<f32>();
        let gaps = self.rows.windows(2).map(|pair| f32::from(geometry::minimum_gap_between(data, &pair[0].assignment, &pair[1].assignment))).sum::<f32>();
        heights + gaps <= f32::from(available_height).max(1.0)
    }
}

/// Extends a locked range by the next source row. Reflowing children already
/// in the range does not count as adding a row.
pub(super) fn extend_by_source_row(data: &PaginatorData, geometries: &[GroupGeometry], direction: AssignmentDirection, page: VisiblePage) -> Option<SourceRowExtension> {
    match direction {
        AssignmentDirection::Forward => {
            let mut next = page.last_child.saturating_add(1);
            let row = geometry::next_forward_row(data, geometries, &mut next)?;
            Some(SourceRowExtension { page: VisiblePage { first_child: page.first_child, last_child: row.items.last()?.child_index }, group_index: row.group_index })
        }
        AssignmentDirection::Backward => {
            let mut previous = page.first_child;
            let row = geometry::previous_row(data, geometries, &mut previous)?;
            Some(SourceRowExtension { page: VisiblePage { first_child: previous, last_child: page.last_child }, group_index: row.group_index })
        }
    }
}

pub(super) struct SourceRowExtension {
    pub(super) page: VisiblePage,
    pub(super) group_index: usize,
}

/// Expands a locked child range provisionally. Column counts and row membership
/// may change here; rank repair chooses the final row arrangement.
pub(super) fn expand_locked_range(
    data: &PaginatorData, rem_size: Pixels, available_height: Pixels, maximum_rows: Option<usize>, candidates: &[Vec<GroupGeometry>], mut current: Placement, mut evaluate: impl FnMut(&[GroupGeometry]) -> Placement,
) -> Placement {
    loop {
        let mut proposals = Vec::new();

        for (group_index, group_candidates) in candidates.iter().enumerate() {
            if !current.group_is_visible(group_index) {
                continue;
            }
            let current_geometry = current.geometries[group_index];
            let mut expanded = group_candidates.iter().copied().filter(|candidate| f32::from(candidate.child_width) > f32::from(current_geometry.child_width) + WIDTH_EPSILON).collect::<Vec<_>>();
            expanded.sort_by(|left, right| f32::from(left.child_width).total_cmp(&f32::from(right.child_width)).then_with(|| right.columns.cmp(&left.columns)));

            let mut first_same_column_failure = None;
            for geometry in expanded {
                let mut geometries = current.geometries.clone();
                geometries[group_index] = geometry;
                let trial = evaluate(&geometries);
                if trial.fits(data, available_height, maximum_rows) && trial.visible_page() == current.visible_page() {
                    proposals.push(ExpansionProposal { group_index, progress: normalized_progress(data, group_index, geometry.child_width, rem_size), placement: trial });
                    break;
                }
                if geometry.columns == current_geometry.columns && first_same_column_failure.is_none() {
                    first_same_column_failure = Some(geometry);
                }
            }

            // Aspect-ratio height can prevent the full same-column expansion.
            // Retain the largest measured partial expansion that still fits.
            if !proposals.iter().any(|proposal| proposal.group_index == group_index)
                && data.groups[group_index].policy.sizing.intrinsic_height_depends_on_width()
                && let Some(target) = first_same_column_failure
                && let Some(partial) = largest_fitting_partial(data, available_height, maximum_rows, current_geometry, target, &current, &mut evaluate)
            {
                let width = partial.geometries[group_index].child_width;
                proposals.push(ExpansionProposal { group_index, progress: normalized_progress(data, group_index, width, rem_size), placement: partial });
            }
        }

        let Some(proposal) = proposals.into_iter().min_by(|left, right| left.progress.total_cmp(&right.progress).then_with(|| left.group_index.cmp(&right.group_index))) else {
            break;
        };
        current = proposal.placement;
    }

    current
}

/// Shrinks each visible group when doing so reduces its row count. Of all
/// geometries with the smallest reachable row count, retains the widest one.
pub(super) fn repair_ranks(data: &PaginatorData, candidates: &[Vec<GroupGeometry>], current: Placement, mut evaluate: impl FnMut(&[GroupGeometry]) -> Placement) -> Placement {
    let Some(page) = current.visible_page() else {
        return current;
    };
    let range_end = page.last_child.saturating_add(1);
    let mut geometries = current.geometries.clone();

    for (group_index, group_candidates) in candidates.iter().enumerate() {
        if group_candidates.is_empty() || !current.group_is_visible(group_index) {
            continue;
        }

        let original = geometries[group_index];
        let mut best = original;
        let mut best_rows = group_row_count(data, &geometries, page.first_child, range_end, group_index);

        for candidate in group_candidates.iter().copied().filter(|candidate| candidate.columns >= original.columns && f32::from(candidate.child_width) <= f32::from(original.child_width) + WIDTH_EPSILON) {
            let mut trial = geometries.clone();
            trial[group_index] = candidate;
            let rows = group_row_count(data, &trial, page.first_child, range_end, group_index);
            if rows < best_rows || (rows == best_rows && f32::from(candidate.child_width) > f32::from(best.child_width)) {
                best = candidate;
                best_rows = rows;
            }
        }

        geometries[group_index] = best;
    }

    evaluate(&geometries)
}

fn group_row_count(data: &PaginatorData, geometries: &[GroupGeometry], range_start: usize, range_end: usize, group_index: usize) -> usize {
    geometry::forward_rows_in_range(data, geometries, range_start, range_end).iter().filter(|row| row.group_index == group_index).count()
}

struct ExpansionProposal {
    group_index: usize,
    progress: f32,
    placement: Placement,
}

fn largest_fitting_partial(
    data: &PaginatorData, available_height: Pixels, maximum_rows: Option<usize>, start: GroupGeometry, target: GroupGeometry, current: &Placement, evaluate: &mut impl FnMut(&[GroupGeometry]) -> Placement,
) -> Option<Placement> {
    let group_index = start.group_index;
    debug_assert_eq!(target.group_index, group_index);
    let mut low = 0.0_f32;
    let mut high = 1.0_f32;
    let mut best = None;
    for _ in 0..PARTIAL_EXPANSION_STEPS {
        let fraction = (low + high) * 0.5;
        let geometry = interpolate_geometry(start, target, fraction);
        let mut geometries = current.geometries.clone();
        geometries[group_index] = geometry;
        let trial = evaluate(&geometries);
        if trial.fits(data, available_height, maximum_rows) && trial.visible_page() == current.visible_page() {
            low = fraction;
            best = Some(trial);
        } else {
            high = fraction;
        }
    }

    best.filter(|placement| f32::from(placement.geometries[group_index].child_width) > f32::from(start.child_width) + WIDTH_EPSILON)
}

fn interpolate_geometry(start: GroupGeometry, target: GroupGeometry, fraction: f32) -> GroupGeometry {
    debug_assert_eq!(start.group_index, target.group_index);
    debug_assert_eq!(start.columns, target.columns);
    let width = f32::from(start.child_width) + (f32::from(target.child_width) - f32::from(start.child_width)) * fraction;
    let gap = f32::from(start.column_gap) + (f32::from(target.column_gap) - f32::from(start.column_gap)) * fraction;
    GroupGeometry { child_width: px(width), column_gap: px(gap), ..start }
}

fn normalized_progress(data: &PaginatorData, group_index: usize, width: Pixels, rem_size: Pixels) -> f32 {
    let policy = data.groups[group_index].policy.width;
    let minimum = f32::from(policy.minimum.to_pixels(rem_size));
    let preferred = f32::from(policy.preferred.to_pixels(rem_size));
    let maximum = f32::from(policy.maximum.to_pixels(rem_size));
    let width = f32::from(width);

    if width <= preferred {
        if preferred <= minimum { 0.0 } else { -((preferred - width) / (preferred - minimum)).clamp(0.0, 1.0) }
    } else if maximum <= preferred {
        0.0
    } else {
        ((width - preferred) / (maximum - preferred)).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PaginatorChild, PaginatorGapPolicy, PaginatorGroup, PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy};
    use super::*;
    use gpui::prelude::*;
    use gpui::{div, rems};

    fn data(length: usize, sizing: PaginatorSizing) -> PaginatorData {
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::new(rems(5.0), rems(10.0), rems(16.0)), sizing, PaginatorGapPolicy::new(px(2.0), px(8.0)), PaginatorGapPolicy::new(px(2.0), px(8.0)));
        PaginatorData::new([PaginatorGroup::new(policy, (0..length).map(|_| PaginatorChild::new(|_, _| div().into_any_element())))])
    }

    fn profile(geometries: &[GroupGeometry], visible: usize, row_height: f32) -> Placement {
        let geometry = geometries[0];
        let rows = (0..visible)
            .collect::<Vec<_>>()
            .chunks(geometry.columns)
            .map(|chunk| ProfileRow {
                assignment: AssignedRow {
                    group_index: 0,
                    items: chunk.iter().enumerate().map(|(column, &child_index)| geometry::PageItem { child_index, column }).collect(),
                    child_width: geometry.child_width,
                    column_gap: geometry.column_gap,
                },
                height: px(row_height),
            })
            .collect();
        Placement { rows, geometries: geometries.to_vec() }
    }

    #[test]
    fn locked_expansion_reflows_columns_but_rejects_intrinsic_overflow_without_dropping_children() {
        let data = data(12, PaginatorSizing::aspect_ratio(1.0));
        let compact = GroupGeometry { group_index: 0, columns: 4, child_width: px(80.0), column_gap: px(2.0) };
        let wider = GroupGeometry { group_index: 0, columns: 3, child_width: px(120.0), column_gap: px(2.0) };
        let current = profile(&[compact], 12, 50.0);

        let selected = expand_locked_range(&data, px(16.0), px(160.0), None, &[vec![compact, wider]], current, |geometries| {
            let height = if geometries[0].columns == 3 { 60.0 } else { 50.0 };
            profile(geometries, 12, height)
        });

        assert_eq!(selected.geometries, vec![compact]);
        assert_eq!(selected.visible_page(), Some(VisiblePage { first_child: 0, last_child: 11 }));
    }

    #[test]
    fn aspect_expansion_stops_at_the_largest_width_that_keeps_the_locked_range() {
        let data = data(12, PaginatorSizing::aspect_ratio(1.0));
        let compact = GroupGeometry { group_index: 0, columns: 4, child_width: px(80.0), column_gap: px(2.0) };
        let target = GroupGeometry { group_index: 0, columns: 4, child_width: px(120.0), column_gap: px(2.0) };
        let current = profile(&[compact], 12, 80.0);

        let selected = expand_locked_range(&data, px(16.0), px(280.0), None, &[vec![compact, target]], current, |geometries| profile(geometries, 12, f32::from(geometries[0].child_width)));

        let selected_width = f32::from(selected.geometries[0].child_width);
        assert!((91.8..=92.1).contains(&selected_width), "unexpected recovered width {selected_width}");
        assert_eq!(selected.visible_page(), Some(VisiblePage { first_child: 0, last_child: 11 }));
        assert!(selected.fits(&data, px(280.0), None));
    }

    #[test]
    fn locked_expansion_commits_a_column_change_when_reflowed_intrinsic_rows_fit() {
        let data = data(12, PaginatorSizing::pixels(px(40.0)));
        let compact = GroupGeometry { group_index: 0, columns: 4, child_width: px(80.0), column_gap: px(2.0) };
        let wider = GroupGeometry { group_index: 0, columns: 3, child_width: px(120.0), column_gap: px(2.0) };
        let current = profile(&[compact], 12, 30.0);

        let selected = expand_locked_range(&data, px(16.0), px(130.0), None, &[vec![compact, wider]], current, |geometries| profile(geometries, 12, 30.0));

        assert_eq!(selected.geometries, vec![wider]);
        assert_eq!(selected.rows.len(), 4);
        assert_eq!(selected.visible_page(), Some(VisiblePage { first_child: 0, last_child: 11 }));
    }

    #[test]
    fn rank_repair_rejects_a_provisional_column_change_that_adds_rows() {
        let data = data(10, PaginatorSizing::pixels(px(20.0)));
        let compact = GroupGeometry { group_index: 0, columns: 5, child_width: px(80.0), column_gap: px(2.0) };
        let wider = GroupGeometry { group_index: 0, columns: 3, child_width: px(120.0), column_gap: px(2.0) };
        let current = profile(&[compact], 10, 20.0);

        let candidates = [vec![compact, wider]];
        let expanded = expand_locked_range(&data, px(16.0), px(100.0), None, &candidates, current, |geometries| profile(geometries, 10, 20.0));
        assert_eq!(expanded.geometries, vec![wider], "expansion is allowed to create a temporary extra row");

        let selected = repair_ranks(&data, &candidates, expanded, |geometries| profile(geometries, 10, 20.0));

        assert_eq!(selected.geometries, vec![compact]);
        assert_eq!(selected.rows.iter().map(|row| row.assignment.items.len()).collect::<Vec<_>>(), vec![5, 5]);
    }

    #[test]
    fn rank_repair_shrinks_to_remove_an_orphan_already_in_the_baseline() {
        let data = data(14, PaginatorSizing::pixels(px(20.0)));
        let baseline_geometry = GroupGeometry { group_index: 0, columns: 8, child_width: px(80.0), column_gap: px(2.0) };
        let expanded_geometry = GroupGeometry { group_index: 0, columns: 6, child_width: px(120.0), column_gap: px(2.0) };
        let balanced_geometry = GroupGeometry { group_index: 0, columns: 7, child_width: px(100.0), column_gap: px(2.0) };
        let baseline = profile(&[expanded_geometry], 14, 20.0);

        let repaired = repair_ranks(&data, &[vec![expanded_geometry, balanced_geometry, baseline_geometry]], baseline, |geometries| profile(geometries, 14, 20.0));

        assert_eq!(repaired.geometries, vec![balanced_geometry]);
        assert_eq!(repaired.rows.iter().map(|row| row.assignment.items.len()).collect::<Vec<_>>(), vec![7, 7]);
    }

    #[test]
    fn rank_repair_is_stable_when_a_provisional_orphan_starts_fitting() {
        let data = data(14, PaginatorSizing::pixels(px(20.0)));
        let eight = GroupGeometry { group_index: 0, columns: 8, child_width: px(80.0), column_gap: px(2.0) };
        let seven = GroupGeometry { group_index: 0, columns: 7, child_width: px(100.0), column_gap: px(2.0) };
        let six = GroupGeometry { group_index: 0, columns: 6, child_width: px(120.0), column_gap: px(2.0) };
        let candidates = [vec![eight, seven, six]];
        let refine = |height: f32| {
            let baseline = profile(&[eight], 14, 20.0);
            let expanded = expand_locked_range(&data, px(16.0), px(height), None, &candidates, baseline, |geometries| profile(geometries, 14, 20.0));
            repair_ranks(&data, &candidates, expanded, |geometries| profile(geometries, 14, 20.0))
        };

        let before_orphan_fits = refine(63.9);
        let after_orphan_fits = refine(64.0);

        assert_eq!(before_orphan_fits.geometries, vec![seven]);
        assert_eq!(after_orphan_fits.geometries, before_orphan_fits.geometries);
        assert_eq!(after_orphan_fits.rows.iter().map(|row| row.assignment.items.len()).collect::<Vec<_>>(), vec![7, 7]);
    }

    #[test]
    fn source_row_extension_uses_one_canonical_row_in_each_navigation_direction() {
        let data = data(30, PaginatorSizing::pixels(px(40.0)));
        let geometry = GroupGeometry { group_index: 0, columns: 4, child_width: px(80.0), column_gap: px(2.0) };

        let forward = extend_by_source_row(&data, &[geometry], AssignmentDirection::Forward, VisiblePage { first_child: 0, last_child: 11 });
        let backward = extend_by_source_row(&data, &[geometry], AssignmentDirection::Backward, VisiblePage { first_child: 12, last_child: 23 });

        assert_eq!(forward.map(|extension| extension.page), Some(VisiblePage { first_child: 0, last_child: 15 }));
        assert_eq!(backward.map(|extension| extension.page), Some(VisiblePage { first_child: 8, last_child: 23 }));
    }

    #[test]
    fn source_row_extension_counts_only_children_outside_the_visible_range() {
        let data = PaginatorData::new([
            PaginatorGroup::new(
                PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(40.0)), PaginatorGapPolicy::fixed(px(2.0)), PaginatorGapPolicy::fixed(px(2.0))),
                (0..7).map(|_| PaginatorChild::new(|_, _| div().into_any_element())),
            ),
            PaginatorGroup::new(
                PaginatorGroupPolicy::new(PaginatorWidthPolicy::fixed(rems(5.0)), PaginatorSizing::pixels(px(40.0)), PaginatorGapPolicy::fixed(px(2.0)), PaginatorGapPolicy::fixed(px(2.0))),
                (0..8).map(|_| PaginatorChild::new(|_, _| div().into_any_element())),
            ),
        ]);
        let geometries = [GroupGeometry { group_index: 0, columns: 7, child_width: px(80.0), column_gap: px(2.0) }, GroupGeometry { group_index: 1, columns: 4, child_width: px(80.0), column_gap: px(2.0) }];

        // The provisional range ends one child before group 0 ends. That
        // unseen terminal row contains only child 6; reflow must not turn the
        // already-visible children into the source of another requested row.
        let extended = extend_by_source_row(&data, &geometries, AssignmentDirection::Forward, VisiblePage { first_child: 0, last_child: 5 });

        assert_eq!(extended.map(|extension| extension.page), Some(VisiblePage { first_child: 0, last_child: 6 }));
    }

    #[test]
    fn source_row_extension_retains_an_absorbed_tail_at_end_of_data() {
        let data = data(7, PaginatorSizing::pixels(px(40.0)));
        let geometry = GroupGeometry { group_index: 0, columns: 7, child_width: px(80.0), column_gap: px(2.0) };

        let extended = extend_by_source_row(&data, &[geometry], AssignmentDirection::Forward, VisiblePage { first_child: 0, last_child: 5 });

        assert_eq!(extended.map(|extension| extension.page), Some(VisiblePage { first_child: 0, last_child: 6 }));
    }
}
