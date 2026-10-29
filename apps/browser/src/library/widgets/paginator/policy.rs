//! Declarative sizing policies for paginator groups.
//!
//! A group declares how wide its children are, how tall a row is, and how
//! spare horizontal space is used. All grid groups expand either their children
//! or their gaps across the available width, regardless of their child count.

use app_preferences::CoverText;
use gpui::{Pixels, Rems, px};

/// The one horizontal gap, used between children and at either edge of a
/// expanded row. Groups that distribute slack into gaps use this as a minimum.
pub(super) const COLUMN_GAP: f32 = ui_components::SPACE_SM;

/// The nominal height assigned to a child. During intrinsic compaction this
/// also caps measurement, allowing a shorter child to reduce its row height.
#[derive(Clone, Copy)]
pub(crate) struct PaginatorSizing {
    aspect_ratio: Option<f32>,
    rems: Rems,
    pixels: Pixels,
    /// Padding the child carries on every side.
    ///
    /// Load-bearing for any group sized by aspect ratio: the ratio applies to
    /// the child's *content*, so padding narrows it and shortens it by more
    /// than the padding itself. Expressing that here keeps the arithmetic in
    /// one place instead of asking each group to pre-compensate its pixel term.
    padding: Pixels,
    /// How tall the children of a group nested inside this one resolve to, at
    /// this child's own width.
    ///
    /// Section and shelf cards contain a second paginator. Resolve its height
    /// with the same rem size so the outer row reserves space for its text.
    nested: Option<fn(Pixels, Pixels) -> Pixels>,
}

impl PaginatorSizing {
    pub(crate) fn rems(height: Rems) -> Self {
        Self { aspect_ratio: None, rems: height, pixels: Pixels::ZERO, padding: Pixels::ZERO, nested: None }
    }

    pub(crate) fn pixels(height: Pixels) -> Self {
        Self { aspect_ratio: None, rems: Rems::ZERO, pixels: height, padding: Pixels::ZERO, nested: None }
    }

    pub(crate) fn aspect_ratio(ratio: f32) -> Self {
        Self { aspect_ratio: Some(ratio), rems: Rems::ZERO, pixels: Pixels::ZERO, padding: Pixels::ZERO, nested: None }
    }

    pub(crate) fn with_rems(mut self, height: Rems) -> Self {
        self.rems = height;
        self
    }

    pub(crate) fn with_pixels(mut self, height: Pixels) -> Self {
        self.pixels = height;
        self
    }

    pub(crate) fn with_padding(mut self, padding: Pixels) -> Self {
        self.padding = padding;
        self
    }

    /// Adds the resolved height of a group laid out inside this child.
    pub(crate) fn with_nested(mut self, nested: fn(Pixels, Pixels) -> Pixels) -> Self {
        self.nested = Some(nested);
        self
    }

    pub(super) fn height(self, width: Pixels, rem_size: Pixels) -> Pixels {
        let padding = f32::from(self.padding).max(0.0);
        let content_width = (f32::from(width) - padding * 2.0).max(0.0);
        let aspect_height = self.aspect_ratio.filter(|ratio| ratio.is_finite() && *ratio > 0.0).map(|ratio| content_width / ratio).unwrap_or_default();
        let nested = self.nested.map_or(0.0, |nested| f32::from(nested(width, rem_size)));
        px((aspect_height + nested + f32::from(self.rems.to_pixels(rem_size)) + f32::from(self.pixels) + padding * 2.0).max(1.0))
    }
}

/// The width a child is measured at before any stretching. It decides how
/// many columns fit; minimum column counts can force narrower children.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct PaginatorWidthPolicy {
    rems: Rems,
    fixed_pixels: Pixels,
}

impl PaginatorWidthPolicy {
    pub(crate) fn new(width: Rems) -> Self {
        Self { rems: Rems(width.0.max(0.0625)), fixed_pixels: Pixels::ZERO }
    }

    pub(crate) fn pixels(width: Pixels) -> Self {
        Self { rems: Rems::ZERO, fixed_pixels: px(f32::from(width).max(1.0)) }
    }

    pub(super) fn width(self, rem_size: Pixels) -> Pixels {
        self.rems.to_pixels(rem_size) + self.fixed_pixels
    }
}

/// Geometry shared by all children in a group. Groups never share rows, so
/// their widths, sizing policies, and gaps resolve independently.
#[derive(Clone, Copy)]
pub(crate) struct PaginatorGroupPolicy {
    pub(crate) width: PaginatorWidthPolicy,
    pub(crate) intrinsic: bool,
    pub(crate) sizing: PaginatorSizing,
    pub(crate) row_gap: Pixels,
    /// Fewest columns the group will lay out, however narrow the window gets.
    pub(crate) minimum_columns: usize,
    /// Horizontal gap, defaulting to [`COLUMN_GAP`]. A group that wants its
    /// children packed tighter than the grid standard sets this; keeping it on
    /// the policy is what allows one group to change without dragging every
    /// other group along with it.
    pub(crate) column_gap: Pixels,
    /// Grow to this width, then put remaining slack into gaps. None is unbounded.
    pub(crate) maximum_width: Option<PaginatorWidthPolicy>,
    /// Whether a stretched row keeps its leftover slack at the edges. A group
    /// sitting under a heading turns this off, so the first child starts where
    /// the title does instead of a gap to the right of it.
    pub(crate) inset_edges: bool,
    /// A margin held at both edges of every row in the group, taken out of the
    /// width before the column count.
    ///
    /// Distinct from `inset_edges`, which only decides where a stretched row's
    /// leftover goes. This margin makes the row narrower rather than pushing
    /// children off the end.
    pub(crate) edge_inset: Pixels,
    /// Keep the gaps at their width and leave a row's slack after its last
    /// child, rather than spreading it into the gaps.
    pub(crate) pack_start: bool,
    /// Paint a shadow on the cell itself, outside its clipped child contents.
    pub(crate) item_shadow: bool,
    /// Stack this group's rows into columns of this fraction of the viewport.
    /// A strip containing such a group lays subsequent groups alongside it.
    pub(crate) column_fraction: Option<f32>,
}

impl PaginatorGroupPolicy {
    pub(crate) fn new(width: PaginatorWidthPolicy, sizing: PaginatorSizing, row_gap: Pixels) -> Self {
        Self {
            intrinsic: false,
            width,
            sizing,
            row_gap: px(f32::from(row_gap).max(0.0)),
            column_gap: px(COLUMN_GAP),
            maximum_width: None,
            minimum_columns: 1,
            inset_edges: true,
            edge_inset: Pixels::ZERO,
            pack_start: false,
            item_shadow: false,
            column_fraction: None,
        }
    }

    /// Let the outer cell cast a shadow into the gaps between its neighbors.
    pub(crate) fn with_item_shadow(mut self) -> Self {
        self.item_shadow = true;
        self
    }

    /// Wrap measured child widths with fixed gaps and no stretching.
    pub(crate) fn intrinsic(mut self) -> Self {
        self.intrinsic = true;
        self
    }

    /// Packs children against the row's start at their own width, with fixed
    /// gaps; whatever width is left over stays empty after the last child.
    pub(crate) fn pack_start(mut self) -> Self {
        self.pack_start = true;
        self.inset_edges = false;
        self
    }

    /// Limit card growth before expanding the gaps in wrapped groups.
    pub(crate) fn with_maximum_width(mut self, width: PaginatorWidthPolicy) -> Self {
        self.maximum_width = Some(width);
        self
    }

    /// Holds the group's rows `inset` off both edges of the container.
    ///
    /// For a group whose children are bordered boxes sitting above a card grid:
    /// a card is flush with the container and insets its cover by its own
    /// padding, so a flush group of boxes overhangs those covers by exactly that
    /// padding on each side. Giving the group the same inset puts the two on one
    /// vertical line.
    pub(crate) fn with_edge_inset(mut self, inset: Pixels) -> Self {
        self.edge_inset = px(f32::from(inset).max(0.0));
        self
    }

    /// Starts the row flush with the container, for a group whose heading sits
    /// directly above it.
    pub(crate) fn flush_edges(mut self) -> Self {
        self.inset_edges = false;
        self
    }

    /// Keeps at least `columns` children on a row even when the window is too
    /// narrow for them at their preferred width.
    pub(crate) fn with_minimum_columns(mut self, columns: usize) -> Self {
        self.minimum_columns = columns.max(1);
        self
    }

    /// Sets both gaps at once, for a group spaced equally in both directions.
    pub(crate) fn with_gap(mut self, gap: Pixels) -> Self {
        let gap = px(f32::from(gap).max(0.0));
        self.row_gap = gap;
        self.column_gap = gap;
        self
    }
}

/// The text block's contribution to a card's reserved height, under either
/// value of the "cover text" setting.
fn text_block_height(cover_text: CoverText) -> (f32, f32) {
    match cover_text {
        CoverText::Always => (ui_components::BOOK_CARD_TITLE_HEIGHT_REM, ui_components::BOOK_CARD_COVER_CONTENT_GAP),
        CoverText::CoverOnly => (ui_components::BOOK_CARD_NO_TEXT_HEIGHT_REM, ui_components::BOOK_CARD_NO_TEXT_FIXED_HEIGHT),
    }
}

/// The book grid on a browse page. Cover-only cards grow up to 25% before
/// expanding gaps.
pub(crate) fn book_card_policy(cover_text: CoverText) -> PaginatorGroupPolicy {
    let width = PaginatorWidthPolicy::pixels(px(ui_components::BOOK_CARD_WIDTH));
    let (extra_height, fixed_height) = text_block_height(cover_text);
    let sizing = PaginatorSizing::aspect_ratio(ui_components::BOOK_COVER_ASPECT_RATIO).with_rems(Rems(extra_height)).with_pixels(px(fixed_height)).with_padding(px(ui_components::BOOK_CARD_PADDING));
    let gap = px(ui_components::BOOK_GRID_BOOK_GAP);
    PaginatorGroupPolicy::new(width, sizing, gap).with_gap(gap).with_maximum_width(PaginatorWidthPolicy::pixels(px(ui_components::BOOK_CARD_MAXIMUM_WIDTH))).with_minimum_columns(ui_components::BOOK_GRID_MINIMUM_COLUMNS).flush_edges()
}

/// Cover-only cards in the graph sidebar. The preferred width is 200 px and
/// may stretch to 300 px; the paginator computes row height from the resolved
/// width and the cover aspect ratio. Phones keep two columns so the covers fit
/// in the bottom pane beside the graph.
pub(crate) fn graph_cover_policy() -> PaginatorGroupPolicy {
    let width = PaginatorWidthPolicy::pixels(px(200.0));
    let gap = px(ui_components::BOOK_GRID_BOOK_GAP);
    PaginatorGroupPolicy::new(width, PaginatorSizing::aspect_ratio(ui_components::BOOK_COVER_ASPECT_RATIO).with_padding(px(ui_components::BOOK_CARD_PADDING)), gap)
        .with_gap(gap)
        .with_maximum_width(PaginatorWidthPolicy::pixels(px(300.0)))
        .with_minimum_columns(if ui_components::has_native_back_button() { 2 } else { 1 })
        .with_edge_inset(px(12.0))
}

fn section_cover_cell_width() -> f32 {
    ui_components::SECTION_CARD_COVER_WIDTH + ui_components::BOOK_CARD_PADDING * 2.0
}

fn section_body_height(cover_text: CoverText, rem_size: Pixels) -> Pixels {
    browse_section_cover_policy(cover_text).sizing.height(px(section_cover_cell_width()), rem_size)
}

fn section_body_with_text(_width: Pixels, rem_size: Pixels) -> Pixels {
    section_body_height(CoverText::Always, rem_size)
}

fn section_body_cover_only(_width: Pixels, rem_size: Pixels) -> Pixels {
    section_body_height(CoverText::CoverOnly, rem_size)
}

/// Containers share the height of book previews, including optional cover text.
pub(crate) fn browse_section_card_policy(cover_text: CoverText) -> PaginatorGroupPolicy {
    let width = PaginatorWidthPolicy::pixels(px(ui_components::SECTION_CARD_WIDTH));
    // The body cells provide the full vertical padding below the heading.
    let heading_height = ui_components::SECTION_CARD_HEADING_HEIGHT_REM + ui_components::SECTION_CARD_HEADING_PADDING_REM * 2.0;
    let sizing = PaginatorSizing::rems(Rems(heading_height)).with_nested(match cover_text {
        CoverText::Always => section_body_with_text,
        CoverText::CoverOnly => section_body_cover_only,
    });
    // Keep the gap between tiles, while the first tile starts at the same
    // content inset as the breadcrumb and the first book cover.
    PaginatorGroupPolicy::new(width, sizing, px(ui_components::SPACE_MD)).with_gap(px(ui_components::SPACE_MD)).with_edge_inset(px(ui_components::CONTENT_INSET)).flush_edges().with_item_shadow()
}

/// Home's shelves: the same cards, each with the row to itself. Any width wider
/// than the window does that — see `SHELF_ROW_WIDTH_REM`.
pub(crate) fn browse_shelf_card_policy(cover_text: CoverText) -> PaginatorGroupPolicy {
    PaginatorGroupPolicy { width: PaginatorWidthPolicy::new(Rems(ui_components::SHELF_ROW_WIDTH_REM)), ..browse_section_card_policy(cover_text) }
}

/// Folder cards share a cover-height body, whether they contain subfolders
/// or books. The preferred width fits two covers side by side and can grow
/// to twice that width.
pub(crate) fn browse_folder_card_policy(cover_text: CoverText) -> PaginatorGroupPolicy {
    let width = PaginatorWidthPolicy::pixels(px(ui_components::FOLDER_CARD_WIDTH));
    PaginatorGroupPolicy { width, ..browse_section_card_policy(cover_text) }.with_maximum_width(PaginatorWidthPolicy::pixels(px(ui_components::FOLDER_CARD_WIDTH * 2.0)))
}

pub(crate) fn browse_folder_item_policy(cover_text: CoverText, container: bool) -> PaginatorGroupPolicy {
    if container {
        return browse_section_item_policy(cover_text, true);
    }
    browse_section_item_policy(cover_text, false).with_minimum_columns(2)
}

/// A book row inside a browse section card. Cover dimensions stay in pixels;
/// optional text adds its rem height and the cover-to-text gap.
pub(crate) fn browse_section_cover_policy(cover_text: CoverText) -> PaginatorGroupPolicy {
    let width = PaginatorWidthPolicy::pixels(px(section_cover_cell_width()));
    let gap = px(ui_components::BOOK_GRID_BOOK_GAP);
    let (text_height, text_gap) = text_block_height(cover_text);
    let sizing = PaginatorSizing::pixels(px(ui_components::SECTION_CARD_COVER_WIDTH / ui_components::BOOK_COVER_ASPECT_RATIO + text_gap)).with_rems(Rems(text_height)).with_padding(px(ui_components::BOOK_CARD_PADDING));
    PaginatorGroupPolicy::new(width, sizing, gap).with_gap(gap).with_maximum_width(width).pack_start()
}

/// Containers use one full-width column; shelves retain their cover cells.
pub(crate) fn browse_section_item_policy(cover_text: CoverText, container: bool) -> PaginatorGroupPolicy {
    if container {
        return PaginatorGroupPolicy::new(PaginatorWidthPolicy::new(Rems(ui_components::SHELF_ROW_WIDTH_REM)), browse_section_cover_policy(cover_text).sizing, Pixels::ZERO)
            .with_minimum_columns(ui_components::SECTION_CARD_SUBJECT_COLUMNS)
            .with_gap(Pixels::ZERO)
            .flush_edges();
    }
    let width = PaginatorWidthPolicy::pixels(px(section_cover_cell_width()));
    PaginatorGroupPolicy::new(width, browse_section_cover_policy(cover_text).sizing, Pixels::ZERO).with_gap(Pixels::ZERO).with_maximum_width(width).pack_start()
}

#[cfg(test)]
mod tests {
    use super::super::geometry::group_geometry;
    use super::{browse_folder_card_policy, browse_folder_item_policy, browse_section_card_policy, browse_section_item_policy, browse_shelf_card_policy};
    use app_preferences::CoverText;
    use gpui::px;
    use ui_components as components;

    #[test]
    fn folder_cards_fit_two_covers_and_use_the_same_height_for_subfolders() {
        for rem in [12.0, 16.0, 24.0, 32.0] {
            for setting in [CoverText::CoverOnly, CoverText::Always] {
                let card = browse_folder_card_policy(setting);
                assert_eq!(card.width.width(px(rem)), px(348.0));
                for width in [160.0, 200.0, 240.0, 348.0] {
                    let body_width = px(width - (components::SPACE_MD - components::BOOK_CARD_PADDING) * 2.0);
                    let books = browse_folder_item_policy(setting, false);
                    let folders = browse_folder_item_policy(setting, true);
                    let covers = group_geometry(books, 0, body_width, px(rem));
                    let children = group_geometry(folders, 0, body_width, px(rem));
                    assert_eq!(covers.columns, 2);
                    assert_eq!(children.columns, 1);
                    assert!(covers.child_width * 2.0 <= body_width);
                    assert_eq!(books.sizing.height(covers.child_width, px(rem)), folders.sizing.height(children.child_width, px(rem)));
                }
            }
        }
    }

    #[test]
    fn shelves_size_to_covers_and_optional_titles() {
        let cover_height = components::SECTION_CARD_COVER_WIDTH / components::BOOK_COVER_ASPECT_RATIO + components::BOOK_CARD_PADDING * 2.0;
        let heading_height = components::SECTION_CARD_HEADING_HEIGHT_REM + components::SECTION_CARD_HEADING_PADDING_REM * 2.0;
        for rem in [12.0, 16.0, 24.0, 32.0] {
            for setting in [CoverText::CoverOnly, CoverText::Always] {
                let text_height = if setting == CoverText::Always { components::BOOK_CARD_TITLE_HEIGHT_REM * rem + components::BOOK_CARD_COVER_CONTENT_GAP } else { 0.0 };
                let expected_body = px(cover_height + text_height);
                let shelf = browse_shelf_card_policy(setting);
                let items = browse_section_item_policy(setting, false);
                for width in [390.0, 780.0] {
                    assert_eq!(items.sizing.height(px(width), px(rem)), expected_body);
                    assert_eq!(shelf.sizing.height(px(width), px(rem)), expected_body + px(heading_height * rem));
                }
            }
        }
    }

    #[test]
    fn containers_keep_one_full_width_column_across_viewports_and_font_sizes() {
        for rem in [12.0, 16.0, 24.0, 32.0] {
            for setting in [CoverText::CoverOnly, CoverText::Always] {
                let card = browse_section_card_policy(setting);
                let items = browse_section_item_policy(setting, true);
                assert_eq!(card.width.width(px(rem)), px(components::SECTION_CARD_WIDTH));
                for width in [240.0, 320.0, 390.0, 600.0, 1200.0] {
                    let columns = group_geometry(items, 0, px(width), px(rem));
                    assert_eq!(columns.columns, 1);
                    assert_eq!(columns.child_width, px(width));
                }
            }
        }
    }

    #[test]
    fn containers_match_folder_and_book_preview_heights_across_platforms() {
        for rem in [12.0, 16.0, 24.0, 32.0] {
            for setting in [CoverText::CoverOnly, CoverText::Always] {
                let body = browse_section_item_policy(setting, false).sizing.height(px(166.0), px(rem));
                for width in [240.0, 390.0, 780.0] {
                    assert_eq!(browse_section_item_policy(setting, true).sizing.height(px(width), px(rem)), body);
                    let height = browse_section_card_policy(setting).sizing.height(px(width), px(rem));
                    assert_eq!(height, browse_folder_card_policy(setting).sizing.height(px(width), px(rem)));
                    assert_eq!(height, browse_shelf_card_policy(setting).sizing.height(px(width), px(rem)));
                }
            }
        }
    }
}
