//! Logical slots with a shared value for unloaded ranges. Both child
//! descriptors and their measured widths keep the same sparse shape.

use std::collections::BTreeMap;
use std::ops::{Index, Range};

pub(crate) struct Slots<T> {
    len: usize,
    runs: Vec<Run<T>>,
}

struct Run<T> {
    start: usize,
    values: Values<T>,
}

enum Values<T> {
    Dense(Vec<T>),
    Sparse { len: usize, placeholder: T, loaded: BTreeMap<usize, T> },
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self { len: 0, runs: Vec::new() }
    }
}

impl<T> Slots<T> {
    pub(crate) fn sparse(len: usize, placeholder: T, loaded: BTreeMap<usize, T>) -> Self {
        assert!(loaded.keys().all(|index| *index < len));
        if len == 0 {
            return Self::default();
        }
        Self { len, runs: vec![Run { start: 0, values: Values::Sparse { len, placeholder, loaded } }] }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        let run = &self.runs[self.runs.partition_point(|run| run.start <= index) - 1];
        let offset = index - run.start;
        match &run.values {
            Values::Dense(values) => values.get(offset),
            Values::Sparse { placeholder, loaded, .. } => Some(loaded.get(&offset).unwrap_or(placeholder)),
        }
    }

    pub(crate) fn append(&mut self, mut other: Self) {
        for run in &mut other.runs {
            run.start += self.len;
        }
        self.len += other.len;
        self.runs.extend(other.runs);
    }

    /// Visit only materialized slots, e.g. when indexing stable item IDs.
    pub(super) fn stored(&self) -> impl Iterator<Item = (usize, &T)> {
        self.runs.iter().flat_map(|run| {
            let items: Box<dyn Iterator<Item = (usize, &T)> + '_> = match &run.values {
                Values::Dense(values) => Box::new(values.iter().enumerate()),
                Values::Sparse { loaded, .. } => Box::new(loaded.iter().map(|(&index, value)| (index, value))),
            };
            items.map(move |(index, value)| (run.start + index, value))
        })
    }

    pub(super) fn range(&self, range: Range<usize>) -> impl Iterator<Item = &T> {
        range.map(|index| &self[index])
    }

    #[cfg(test)]
    pub(super) fn iter(&self) -> impl Iterator<Item = &T> {
        self.range(0..self.len)
    }

    pub(crate) fn map<U>(&self, mut f: impl FnMut(&T) -> U) -> Slots<U> {
        self.filter_map(|value| Some(f(value)))
    }

    /// Partition groups without walking their repeated unloaded slots.
    pub(crate) fn filter_map<U>(&self, mut f: impl FnMut(&T) -> Option<U>) -> Slots<U> {
        let mut result = Slots::default();
        for run in &self.runs {
            match &run.values {
                Values::Dense(values) => result.append(values.iter().filter_map(&mut f).collect()),
                Values::Sparse { len, placeholder, loaded } => match f(placeholder) {
                    None => result.append(loaded.values().filter_map(&mut f).collect()),
                    Some(placeholder) => {
                        let mut removed = 0;
                        let kept = loaded
                            .iter()
                            .filter_map(|(&index, value)| match f(value) {
                                Some(value) => Some((index - removed, value)),
                                None => {
                                    removed += 1;
                                    None
                                }
                            })
                            .collect();
                        result.append(Slots::sparse(len - removed, placeholder, kept));
                    }
                },
            }
        }
        result
    }
}

impl<T> Index<usize> for Slots<T> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        self.get(index).expect("paginator slot out of bounds")
    }
}

impl<T> From<Vec<T>> for Slots<T> {
    fn from(values: Vec<T>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        Self { len: values.len(), runs: vec![Run { start: 0, values: Values::Dense(values) }] }
    }
}

impl<T> FromIterator<T> for Slots<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Vec::from_iter(iter).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_million_unloaded_slots_store_and_measure_one_placeholder() {
        let mut slots = Slots::from(vec![7, 8]);
        slots.append(Slots::sparse(1_000_000, 0, BTreeMap::from([(10, 42), (999_999, 99)])));
        slots.append(Slots::from(vec![100]));
        assert_eq!(slots.len(), 1_000_003);
        assert_eq!((slots[1], slots[2], slots[12], slots[1_000_001], slots[1_000_002]), (8, 0, 42, 99, 100));
        assert!(slots.get(1_000_003).is_none());
        let mut measurements = 0;
        let widths = slots.map(|value| {
            measurements += 1;
            value + 1
        });
        assert_eq!(measurements, 6, "measure the repeated placeholder once");
        assert_eq!(slots.stored().count(), 5);
        assert_eq!(widths[500_000], 1);
        assert_eq!(widths[1_000_001], 100);
    }

    #[test]
    fn filtering_sparse_groups_preserves_order_and_compacts_removed_slots() {
        let slots = Slots::sparse(1_000_000, 0, BTreeMap::from([(2, 3), (5, 4), (999_999, 8)]));
        let evens = slots.filter_map(|value| (value % 2 == 0).then_some(*value));
        assert_eq!(evens.len(), 999_999);
        assert_eq!((evens[2], evens[4], evens[999_998]), (0, 4, 8));
        let odds = slots.filter_map(|value| (value % 2 == 1).then_some(*value));
        assert_eq!(odds.len(), 1);
        assert_eq!(odds[0], 3);
    }

    #[test]
    fn filtering_mixed_runs_matches_dense_results_for_every_value_subset() {
        let mut slots = Slots::from(vec![1, 2, 3]);
        slots.append(Slots::sparse(8, 0, BTreeMap::from([(0, 4), (3, 5), (7, 6)])));
        slots.append(Slots::from(vec![7, 8]));
        let logical = [1, 2, 3, 4, 0, 0, 5, 0, 0, 0, 6, 7, 8];
        for selection in 0..1u16 << 9 {
            let keep = |value: &i32| selection & (1u16 << *value) != 0;
            let mut visits = 0;
            let filtered = slots.filter_map(|value| {
                visits += 1;
                keep(value).then_some(value * 10)
            });
            let expected: Vec<_> = logical.iter().filter(|value| keep(value)).map(|value| value * 10).collect();
            assert_eq!(filtered.iter().copied().collect::<Vec<_>>(), expected, "subset {selection:09b}");
            assert!(filtered.get(expected.len()).is_none());
            assert_eq!(visits, 9, "visit each dense value, loaded value and placeholder once");
        }
    }
}
