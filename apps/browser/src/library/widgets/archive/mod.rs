//! Superseded widgets, kept for reference while their replacements settle.
//!
//! Nothing here is wired into a page. [`paginator`] is the flexible-width
//! paginator that `widgets::paginator` replaced: it resolved a minimum,
//! preferred and maximum width per group and searched column-count candidates,
//! where the replacement fits one fixed width and derives spacing.
#[allow(dead_code, unused_imports)]
pub(crate) mod paginator;
