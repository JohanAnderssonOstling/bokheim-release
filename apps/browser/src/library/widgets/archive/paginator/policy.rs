//! Declarative sizing and spacing policies for paginator groups.

use gpui::{Pixels, Rems, px};

/// The nominal height assigned to a child. During intrinsic compaction this
/// also caps measurement, allowing a shorter child to reduce its row height.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PaginatorSizing {
    aspect_ratio: Option<f32>,
    rems: Rems,
    pixels: Pixels,
}

impl PaginatorSizing {
    pub(crate) fn rems(height: Rems) -> Self {
        Self { aspect_ratio: None, rems: height, pixels: Pixels::ZERO }
    }

    pub(crate) fn pixels(height: Pixels) -> Self {
        Self { aspect_ratio: None, rems: Rems::ZERO, pixels: height }
    }

    pub(crate) fn aspect_ratio(ratio: f32) -> Self {
        Self { aspect_ratio: Some(ratio), rems: Rems::ZERO, pixels: Pixels::ZERO }
    }

    pub(crate) fn with_rems(mut self, height: Rems) -> Self {
        self.rems = height;
        self
    }

    pub(crate) fn with_pixels(mut self, height: Pixels) -> Self {
        self.pixels = height;
        self
    }

    pub(super) fn height(self, width: Pixels, rem_size: Pixels) -> Pixels {
        let aspect_height = self.aspect_ratio.filter(|ratio| ratio.is_finite() && *ratio > 0.0).map(|ratio| f32::from(width) / ratio).unwrap_or_default();
        px((aspect_height + f32::from(self.rems.to_pixels(rem_size)) + f32::from(self.pixels)).max(1.0))
    }

    /// Whether intrinsic child height must be measured again when its width
    /// changes. Rem- and pixel-sized groups explicitly promise width-stable
    /// intrinsic height; aspect-ratio groups do not.
    pub(super) fn intrinsic_height_depends_on_width(self) -> bool {
        self.aspect_ratio.is_some()
    }
}

/// The permitted width range for every child in one group. Preferred chooses
/// the initial column count; minimum and maximum bound geometry candidates.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PaginatorWidthPolicy {
    pub(crate) minimum: Rems,
    pub(crate) preferred: Rems,
    pub(crate) maximum: Rems,
    fixed_pixels: Pixels,
}

impl PaginatorWidthPolicy {
    pub(crate) fn new(minimum: Rems, preferred: Rems, maximum: Rems) -> Self {
        let minimum = minimum.0.max(0.0625);
        let preferred = preferred.0.max(minimum);
        let maximum = maximum.0.max(preferred);
        Self { minimum: Rems(minimum), preferred: Rems(preferred), maximum: Rems(maximum), fixed_pixels: Pixels::ZERO }
    }

    pub(crate) fn fixed(width: Rems) -> Self {
        Self::new(width, width, width)
    }

    /// Adds fixed chrome, such as a cover and card padding, to the rem-based
    /// text measure declared by `new`.
    pub(crate) fn with_pixels(mut self, fixed: Pixels) -> Self {
        self.fixed_pixels = px(f32::from(fixed).max(0.0));
        self
    }

    pub(super) fn minimum_width(self, rem_size: Pixels) -> Pixels {
        self.minimum.to_pixels(rem_size) + self.fixed_pixels
    }

    pub(super) fn preferred_width(self, rem_size: Pixels) -> Pixels {
        self.preferred.to_pixels(rem_size) + self.fixed_pixels
    }

    pub(super) fn maximum_width(self, rem_size: Pixels) -> Pixels {
        self.maximum.to_pixels(rem_size) + self.fixed_pixels
    }
}

/// A flexible gap whose minimum is used only when it admits a complete row.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PaginatorGapPolicy {
    pub(crate) minimum: Pixels,
    pub(crate) preferred: Pixels,
    pub(crate) maximum: Pixels,
}

impl PaginatorGapPolicy {
    pub(crate) fn new(minimum: Pixels, preferred: Pixels) -> Self {
        let minimum = f32::from(minimum).max(0.0);
        let preferred = f32::from(preferred).max(minimum);
        Self { minimum: px(minimum), preferred: px(preferred), maximum: px(preferred) }
    }

    pub(crate) fn with_maximum(mut self, maximum: Pixels) -> Self {
        self.maximum = px(f32::from(maximum).max(f32::from(self.preferred)));
        self
    }

    pub(crate) fn fixed(gap: Pixels) -> Self {
        Self::new(gap, gap)
    }
}

/// Geometry shared by all children in a group. Groups never share rows, so
/// their widths, sizing policies, and gaps can be resolved independently.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PaginatorGroupPolicy {
    pub(crate) width: PaginatorWidthPolicy,
    pub(crate) sizing: PaginatorSizing,
    pub(crate) column_gap: PaginatorGapPolicy,
    pub(crate) row_gap: PaginatorGapPolicy,
}

impl PaginatorGroupPolicy {
    pub(crate) fn new(width: PaginatorWidthPolicy, sizing: PaginatorSizing, column_gap: PaginatorGapPolicy, row_gap: PaginatorGapPolicy) -> Self {
        Self { width, sizing, column_gap, row_gap }
    }
}
