//! How far through the book the reader is.
//!
//! The renderer re-reports progress on every relayout, usually with identical
//! numbers. Folding that here keeps repaints and database writes tied to real
//! movement rather than to layout churn.

use crate::epub::{ProgressState, ProgressSummary};

impl Default for ProgressState {
    fn default() -> Self {
        // A book always has at least one location and one section, and
        // positions are one-based, so zero is never a valid starting value.
        Self { fraction: 0.0, doc_fraction: 0.0, location: 1, total_locations: 1, section: 1, section_count: 1 }
    }
}

impl Default for ProgressSummary {
    fn default() -> Self {
        ProgressState::default().summary()
    }
}

impl ProgressSummary {
    /// What the toolbar shows: the one number worth a permanent place.
    pub(crate) fn label(self) -> String {
        format!("{}%", self.percent)
    }

    /// Progress is deliberately presented only as a percentage. ZIP byte
    /// weights are useful for relative progress, not as synthetic page or
    /// location numbers.
    pub(crate) fn tooltip(self) -> String {
        format!("{}% through book", self.percent)
    }
}

impl ProgressState {
    pub(in crate::epub) fn summary(&self) -> ProgressSummary {
        // Clamped rather than trusted: the renderer reports a fraction derived
        // from layout, and a rounding overshoot must not print "101%".
        let percent = (self.fraction.clamp(0.0, 1.0) * 100.0).round() as u8;
        ProgressSummary { percent, location: self.location, total_locations: self.total_locations, section: self.section, section_count: self.section_count }
    }

    /// Records a progress report, returning whether it actually moved.
    ///
    /// `doc` is the renderer's zero-based document index; sections are shown
    /// one-based.
    pub(in crate::epub) fn update(&mut self, fraction: f32, doc_fraction: f32, location: u64, total_locations: u64, doc: usize, doc_count: usize) -> bool {
        let next = Self { fraction, doc_fraction, location, total_locations, section: doc + 1, section_count: doc_count };
        if *self == next {
            return false;
        }
        *self = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_report_is_not_movement() {
        let mut progress = ProgressState::default();
        assert!(progress.update(0.25, 0.1, 40, 160, 2, 12), "the first report moves off the default");
        assert!(!progress.update(0.25, 0.1, 40, 160, 2, 12), "an identical report must not repaint or persist");
    }

    #[test]
    fn any_changed_component_counts_as_movement() {
        for (fraction, doc_fraction, location, total, doc, count) in
            [(0.26, 0.1, 40, 160, 2, 12), (0.25, 0.1, 41, 160, 2, 12), (0.25, 0.1, 40, 161, 2, 12), (0.25, 0.1, 40, 160, 3, 12), (0.25, 0.1, 40, 160, 2, 13), (0.25, 0.2, 40, 160, 2, 12)]
        {
            let mut progress = ProgressState::default();
            progress.update(0.25, 0.1, 40, 160, 2, 12);
            assert!(progress.update(fraction, doc_fraction, location, total, doc, count), "a change in any component must be reported");
        }
    }

    #[test]
    fn the_summary_rounds_to_whole_percent_and_cannot_exceed_one_hundred() {
        let mut progress = ProgressState::default();
        progress.update(0.4449, 0.1, 40, 160, 2, 12);
        assert_eq!(progress.summary().percent, 44);
        progress.update(1.004, 0.2, 160, 160, 11, 12);
        assert_eq!(progress.summary().percent, 100, "a rounding overshoot must not print 101%");
    }

    #[test]
    fn the_toolbar_label_is_just_the_percentage() {
        let mut progress = ProgressState::default();
        progress.update(0.42, 0.1, 40, 160, 2, 12);
        assert_eq!(progress.summary().label(), "42%");
        assert_eq!(progress.summary().tooltip(), "42% through book");
    }

    #[test]
    fn sections_are_reported_one_based() {
        let mut progress = ProgressState::default();
        progress.update(0.0, 0.1, 1, 160, 0, 12);
        assert_eq!(progress.section, 1, "the renderer's document 0 is section 1 to the reader");
    }
}
