use gpui::{FontWeight, Styled};

use crate::theme::BrowserTheme;

/// The interaction ladder, defined once.
///
/// A call site says what a thing *is* — current or not — and never names a
/// colour. That is the point: the treatment was previously spelled out at each
/// site, so the same state drifted into meaning `accent_wash` in some places
/// and a solid accent in others, and pages picked palette values directly.
///
///   rest      untouched
///   hover     `theme.hover` — translucent, correct over any surface
///   current   `accent` with `accent_text` on it — persistent "you are here"
///             state, not a transient pick; outranks hover
///
/// A transient pick (a row a reader clicked to look at, not a place the app
/// considers itself "in") is a third tier, `accent_wash`, that this trait does
/// not cover — see `contents_row`, which lays out all three together.
///
/// Do not call this on an element that already sets its own `hover`: gpui
/// asserts a hover style is only set once. A `gpui_component` `Button` is such
/// an element — use [`selectable_button`] for those.
pub trait Interactive: Styled + gpui::InteractiveElement + Sized {
    fn selectable(self, current: bool, theme: BrowserTheme) -> Self {
        // A current surface is already the accent, so a wash laid over it
        // would be invisible. It darkens instead — the same distinction
        // `accent_hover` exists for.
        let hovered = if current { theme.accent_hover } else { theme.hover };
        let element = self.hover(move |style| style.bg(hovered));
        if current { element.bg(theme.accent).text_color(theme.accent_text) } else { element }
    }
}

impl<T: Styled + gpui::InteractiveElement + Sized> Interactive for T {}

/// [`Interactive::selectable`] for a `gpui_component` `Button`.
///
/// A `Button` hands out its inner element's interactivity, so a caller's hover
/// and the button's own hover are the same slot — and gpui allows that slot to
/// be written exactly once. The button writes it only while it is neither
/// selected nor disabled, which leaves the selected state free; taking only
/// that state is what makes this safe where [`Interactive::selectable`] is not.
///
/// An unselected button therefore keeps its own hover treatment rather than the
/// `accent_wash` a plain surface gets. That is the cost of the button styling
/// itself, and it is a smaller one than a page that cannot be opened.
pub fn selectable_button(button: gpui_component::button::Button, selected: bool, theme: BrowserTheme) -> gpui_component::button::Button {
    use gpui::InteractiveElement;
    use gpui_component::Selectable;
    if !selected {
        return button;
    }
    button.selected(true).bg(theme.accent).text_color(theme.accent_text).hover(move |style| style.bg(theme.accent_hover))
}

/// Text that reports a failure. A page says the text is an error; the palette
/// decides what an error looks like.
pub fn error_text(theme: BrowserTheme) -> gpui::Div {
    gpui::div().text_size(gpui::rems(TEXT_XS)).text_color(theme.danger)
}

/// Text for state that needs a decision but is not a failure — see `warning`
/// on [`BrowserTheme`].
pub fn warning_text(theme: BrowserTheme) -> gpui::Div {
    gpui::div().text_size(gpui::rems(TEXT_XS)).text_color(theme.warning)
}

/// A determinate bar: `rule` for the track it has yet to cover, `accent` for
/// the part it has. Both the reader's download bar and the cover's reading
/// progress spelled this pairing out for themselves.
pub const PROGRESS_TRACK_HEIGHT: f32 = 6.0;

pub fn progress_track(theme: BrowserTheme) -> gpui::Div {
    gpui::div().w_full().h(gpui::px(PROGRESS_TRACK_HEIGHT)).overflow_hidden().bg(theme.rule)
}

pub fn progress_fill(fraction: f32, theme: BrowserTheme) -> gpui::Div {
    gpui::div().h_full().w(gpui::relative(fraction.clamp(0.0, 1.0))).bg(theme.accent)
}

/// The one elevation in the interface: something floating above the page —
/// a dialog, a popover, an editor. Everything else is defined by its border
/// and its surface, not by a shadow.
///
/// Book covers use a separate, tighter shadow that follows the artwork itself;
/// this larger shadow is reserved for floating interface surfaces.
pub fn overlay_shadow(theme: BrowserTheme) -> Vec<gpui::BoxShadow> {
    vec![gpui_component::box_shadow(gpui::px(0.0), gpui::px(14.0), gpui::px(34.0), gpui::px(0.0), theme.shadow)]
}

/// Spacing scale. Every gap, padding and margin picks from these; anything
/// that is a *component dimension* (a control height, a panel width) is a
/// named constant further down instead.
///
/// No two steps are closer than a third: at the small end a couple of pixels
/// is a visible difference, at the large end it is not, so the steps widen as
/// they climb. A value that lands between two of them is a value chosen by
/// eye — pick the neighbour rather than adding a step.
///
/// The scale runs to 96 because space is this system's only separator. One
/// surface and one shadow means a border is the alternative to a gap, and a
/// scale that stopped at 16 left every larger separation to a border.
///
/// Shared by every GPUI target, including the WebAssembly frontend.
pub const SPACE_XXS: f32 = 2.0;
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 16.0;
pub const SPACE_LG: f32 = 24.0;
pub const SPACE_XL: f32 = 32.0;
pub const SPACE_2XL: f32 = 48.0;
pub const SPACE_3XL: f32 = 64.0;
pub const SPACE_4XL: f32 = 96.0;

/// Native surfaces and controls use square corners. Semantic pills and
/// progress indicators may still opt into a fully rounded shape explicitly.
pub const RADIUS_SM: f32 = 0.0;
pub const RADIUS_MD: f32 = 0.0;

/// Type scale, in rem. Applied directly as `rems(TEXT_*)` so it tracks the
/// user's UI font size rather than being absolute. Regular text is 1rem.
///
/// Hierarchy is carried by size, colour and space — not by weight. That is the
/// book convention, and it is also forced: only Regular and Bold are bundled,
/// so the `FontWeight(600)` this scale used to lean on never existed and was
/// resolving to 700 or being synthesised.
///
/// The steps are therefore close to a 1.2 ratio, with regular text anchored at
/// 1rem. Captions remain readable at 0.875rem, while titles have enough room
/// above body text to establish hierarchy without relying on a different font.
///
/// The scale is tuned for Libertinus, whose x-height is smaller than Noto Sans;
/// the nominal rem values therefore keep the same comfortable visual density
/// when the interface font changes.
pub const TEXT_XS: f32 = 0.875;
pub const TEXT_SM: f32 = 1.0;
pub const TEXT_MD: f32 = 1.125;
pub const TEXT_LG: f32 = 1.375;
pub const TITLE_MD: f32 = 1.6875;
pub const TITLE_LG: f32 = 2.125;

/// Display face for prominent application headings. Reading text keeps the
/// interface's Libertinus Serif default.
pub const DISPLAY_FONT_FAMILY: &str = "Bodoni Moda";

/// Real small caps from the interface face, not uppercased text.
///
/// `smcp` maps lowercase onto the small-cap glyphs; `c2sc` does the same for
/// capitals, so a label reads uniformly whatever case it was written in and no
/// call site has to lowercase its strings. Libertinus carries both, covering
/// a-z plus å, ä and ö.
///
/// Small caps stand at 0.483em against the face's 0.429em x-height, so a label
/// set this way runs slightly larger than its nominal size suggests — see
/// `NAV_RAIL_LABEL_SIZE`, which is set from that measurement rather than from
/// the lowercase scale.
pub fn small_caps() -> gpui::FontFeatures {
    gpui::FontFeatures(std::sync::Arc::new(vec![("smcp".to_owned(), 1), ("c2sc".to_owned(), 1)]))
}

pub const BOOK_COVER_WIDTH: f32 = 200.0;
pub const BOOK_COVER_ASPECT_RATIO: f32 = 1.0 / 1.5;
/// However narrow the window, the book grid keeps at least this many columns.
/// One cover stretched across a phone screen is not a grid, and the covers are
/// what the reader scans.
pub const BOOK_GRID_MINIMUM_COLUMNS: usize = 2;
pub const BOOK_CARD_MINIMUM_WIDTH_REM: f32 = 10.0;
/// Cover width in logical pixels, independent of the UI font size.
pub const BOOK_CARD_CONTENT_WIDTH: f32 = 200.0;
pub const BOOK_CARD_WIDTH: f32 = BOOK_CARD_CONTENT_WIDTH + BOOK_CARD_PADDING * 2.0;
/// Regular cards grow by up to 25% before spare row width expands gaps.
pub const BOOK_CARD_MAXIMUM_WIDTH: f32 = BOOK_CARD_WIDTH * 1.25;
/// Air between one card's plate and the next, on both axes.
///
/// This was `SPACE_MD` and unused: the grid ran at a zero gap, because a
/// selected card drew a solid accent and a plate that stops short of its cell
/// reads as a gap in the fill rather than as a card. A selected card now draws
/// `accent_wash`, which is a tint rather than an inversion and so has no need
/// to reach the cell edge — the gap is real again, and the plate is a tile
/// sitting in it.
pub const BOOK_GRID_BOOK_GAP: f32 = SPACE_SM;
/// The plate's inset: how far the cover and its words sit inside the tinted
/// tile a selected or focused card draws.
///
/// With `BOOK_GRID_BOOK_GAP` between plates, two neighbouring covers are this
/// twice plus that gap apart.
///
/// It is also the clearance `COVER_FOCUS_GROWTH` grows into, and must stay
/// larger than it.
pub const BOOK_CARD_PADDING: f32 = CONTENT_INSET;
/// How far the first thing on a page sits from the content edge.
///
/// Shared by the breadcrumb, the folder chips and the book cards, because they
/// stack vertically down the left of the same column and any difference between
/// them reads as a misalignment. Each carries it as its own padding rather than
/// a container supplying it, so a selected card can still fill its whole cell.
pub const CONTENT_INSET: f32 = SPACE_SM;
/// Book-card text reserves two title lines and one subtitle line — the title
/// gets first claim on the space, so a long subtitle is what clips. Keep
/// those blocks in rems: the interface font can have different glyph widths,
/// and the user's UI font size must enlarge the card instead of being
/// clipped by the 16px-reference geometry in the application contract.
/// On the scale rather than beside it: this was 16.0, a value that appeared
/// nowhere else and sat between two steps.
pub const BOOK_CARD_TITLE_SIZE: f32 = TEXT_MD;
/// Main titles carry more weight than subtitles.
pub const BOOK_CARD_TITLE_WEIGHT: FontWeight = FontWeight::SEMIBOLD;
pub const BOOK_CARD_TITLE_LINE_HEIGHT_EM: f32 = 1.25;
pub const BOOK_CARD_TITLE_LINES: usize = 2;
pub const BOOK_CARD_SUBTITLE_SIZE: f32 = TEXT_SM;
pub const BOOK_CARD_SUBTITLE_LINES: usize = 1;
/// Author lines run at `TEXT_XS`.
pub const BOOK_CARD_AUTHOR_LINE_HEIGHT_EM: f32 = 1.0;
/// Compact card spacing for the GPUI browser surface.
pub const BOOK_CARD_COVER_CONTENT_GAP: f32 = SPACE_SM;
pub const BOOK_CARD_TEXT_GAP: f32 = SPACE_XS;
pub const BOOK_CARD_TITLE_HEIGHT_REM: f32 = {
    // The title always claims its full two lines, subtitle or not, so the
    // reserved box is that plus the subtitle's one line when there is one —
    // never a max() between two shapes, since the title no longer shrinks.
    let title_only = BOOK_CARD_TITLE_SIZE * BOOK_CARD_TITLE_LINE_HEIGHT_EM * BOOK_CARD_TITLE_LINES as f32;
    let with_subtitle = title_only + BOOK_CARD_SUBTITLE_SIZE * BOOK_CARD_TITLE_LINE_HEIGHT_EM * BOOK_CARD_SUBTITLE_LINES as f32;
    if title_only > with_subtitle { title_only } else { with_subtitle }
};
/// The reader's place in the book, drawn on the cover's own bottom edge.
pub const BOOK_COVER_PROGRESS_HEIGHT: f32 = 3.0;
/// How tall a stripe of the cover the progress mark's scrim darkens, so the
/// bar reads against art of any colour underneath it.
pub const BOOK_COVER_PROGRESS_SCRIM_HEIGHT: f32 = 28.0;
/// Width of the cover's binding-side spine shade, in logical pixels.
pub const BOOK_COVER_SPINE_WIDTH: f32 = 10.0;
/// A cover's own corner radius, apart from `RADIUS_SM`: every other square
/// corner in the interface stays sharp, so rounding this alone is a
/// deliberate departure rather than a change to the shared radius scale.
/// Applied to the outer (right) edge only — see `book_cover_image`.
pub const BOOK_COVER_RADIUS: f32 = 4.0;
/// A download action's hit area — the badge on a cover, the slot at the end of
/// a folder or subject chip — and the glyph inside it. Rems, so the glyph keeps
/// its size against the text beside a chip and the target keeps up with it.
pub const DOWNLOAD_ACTION_SIZE_REM: f32 = 1.75;
pub const DOWNLOAD_ICON_SIZE_REM: f32 = 0.875;
/// How far a cover badge sits in from the cover's corner: clear of its rounded
/// edge, and of the hinge shading on the spine side.
pub const BOOK_COVER_BADGE_INSET: f32 = SPACE_XS + 2.0;
/// A card under the "cover text: cover only" setting: nothing below the
/// cover at all, not even the gap — there is no second child to hold one
/// open against.
pub const BOOK_CARD_NO_TEXT_HEIGHT_REM: f32 = 0.0;
pub const BOOK_CARD_NO_TEXT_FIXED_HEIGHT: f32 = 0.0;

/// Detailed cards combine a fixed cover and padding with a scalable text column.
pub const BOOK_CARD_DETAIL_TEXT_WIDTH_REM: f32 = 24.0;
pub const BOOK_CARD_DETAIL_TEXT_MAXIMUM_WIDTH_REM: f32 = 36.0;
pub const BOOK_CARD_DETAIL_FIXED_WIDTH: f32 = BOOK_CARD_WIDTH + BOOK_CARD_DETAIL_GAP;
/// How much of a detailed card's content the cover takes. Half, so the cover is
/// drawn at the ordinary card's width whatever the row stretches to.
pub const BOOK_CARD_DETAIL_COVER_FRACTION: f32 = 0.5;
/// Between the cover and the words beside it. Wider than the gap under a cover
/// in the ordinary card: horizontally the two columns need a visible channel,
/// where stacked blocks read as one column without it.
pub const BOOK_CARD_DETAIL_GAP: f32 = SPACE_MD;
/// The ratio a detailed card is sized by.
///
/// The paginator resolves an aspect policy against the child's whole content
/// width, but only the cover's half of it carries the height — so the ordinary
/// cover ratio, widened by the fraction the cover is drawn at, is what makes the
/// card exactly as tall as its cover.
pub const BOOK_CARD_DETAIL_ASPECT_RATIO: f32 = BOOK_COVER_ASPECT_RATIO / BOOK_CARD_DETAIL_COVER_FRACTION;
/// The blurb on a detailed card, at `TEXT_SM`. A step above the author line:
/// it is read rather than scanned, and it is the reason the card is twice
/// as wide.
pub const BOOK_CARD_DESCRIPTION_LINE_HEIGHT_EM: f32 = 1.4;

/// The full blurb, opened from the card.
///
/// A measure rather than a share of the window: this is a column of prose, and
/// prose set across a wide window is harder to read, not easier. Around 70
/// characters at the overlay's size.
pub const BOOK_DESCRIPTION_OVERLAY_WIDTH_REM: f32 = 34.0;
/// How much of the window's height the overlay may take before it scrolls.
/// Short of the whole, so the page it opened over stays visible around it and
/// the overlay reads as a layer rather than a new screen.
pub const BOOK_DESCRIPTION_OVERLAY_HEIGHT_FRACTION: f32 = 0.75;
/// Set at `TEXT_MD`, larger than the card's blurb: on the card it is a sample
/// squeezed beside the cover, here it is the thing being read.
pub const BOOK_DESCRIPTION_OVERLAY_LINE_HEIGHT_EM: f32 = 1.5;

/// The compact detail page's single measure. Desktop reading columns use the
/// narrower limits below.
pub const BOOK_DETAIL_PAGE_WIDTH_REM: f32 = 38.5;
/// Desktop book detail widths approximate the 56ch contents and 65ch
/// description limits in the HTML mock. GPUI lengths do not support `ch`.
pub const BOOK_DETAIL_SIDEBAR_WIDTH_REM: f32 = 15.0;
pub const BOOK_DETAIL_CONTENTS_WIDTH_REM: f32 = 28.0;
pub const BOOK_DETAIL_DESCRIPTION_WIDTH_REM: f32 = 32.5;
pub const BOOK_DETAIL_WIDE_PAGE_WIDTH_REM: f32 = BOOK_DETAIL_SIDEBAR_WIDTH_REM + BOOK_DETAIL_CONTENTS_WIDTH_REM + BOOK_DETAIL_DESCRIPTION_WIDTH_REM + 4.0;
/// The cover takes at most half the hero, so a narrow window never leaves the
/// title in a column one word wide.
pub const BOOK_DETAIL_COVER_FRACTION: f32 = 0.5;
pub const BOOK_DETAIL_SECTION_GAP: f32 = SPACE_XL;
pub const BOOK_DETAIL_HERO_GAP: f32 = SPACE_LG;
/// Detail titles run at `TITLE_MD`, subtitles at `TEXT_MD`.
pub const BOOK_DETAIL_DESCRIPTION_SIZE: f32 = TEXT_SM;
pub const BOOK_DETAIL_DESCRIPTION_LINE_HEIGHT_EM: f32 = 1.5;

/// Aliased rather than restated: this was 21.0, duplicating `TITLE_MD`.
pub const CAROUSEL_TITLE_SIZE: f32 = TEXT_LG;
pub const CAROUSEL_TITLE_WEIGHT: FontWeight = FontWeight::NORMAL;
pub const CAROUSEL_CONTENT_GAP: f32 = SPACE_MD;
pub const CAROUSEL_CONTROL_GAP: f32 = SPACE_XS;

/// The line box of the title over a shelf. Stated rather than left to the font,
/// because the row's height is computed from it — see `shelf_rows_policy`.
pub const SHELF_HEADING_LINE_HEIGHT_EM: f32 = 1.25;
pub const SHELF_HEADING_HEIGHT_REM: f32 = CAROUSEL_TITLE_SIZE * SHELF_HEADING_LINE_HEIGHT_EM;
/// Subject chips share the heading line with the title, so they are set at the
/// smallest step: the title is what the row is filed under, the chips qualify it.
pub const SHELF_SUBJECT_SIZE: f32 = TEXT_XS;

/// A chip's own box. Stated rather than left to text plus padding, because the
/// heading's height is computed from it and a chip has to be exactly as tall as
/// the band it is stacked in.
pub const SHELF_SUBJECT_LINE_HEIGHT_EM: f32 = 1.5;
pub const SHELF_SUBJECT_CHIP_HEIGHT_REM: f32 = SHELF_SUBJECT_SIZE * SHELF_SUBJECT_LINE_HEIGHT_EM;
// A shelf row's height is not stated here. A shelf card is the ordinary book
// card at its ordinary width — a shelf that sized its covers differently would
// be a second visual language for the same object — and that card stretches
// with the window, so its height is only known once the row's width is. The row
// asks the cards instead of restating them; see `shelf_rows_policy`.

/// Between one shelf row and the next.
pub const SHELF_ROW_GAP: f32 = SPACE_MD;
/// The declared width of a shelf row, which is never the drawn one.
///
/// The paginator fits children across a row at their declared width, and a shelf
/// row is meant to have the row to itself. Any value wider than the window does
/// that: a declared width is clamped to the space available before the column
/// count is taken, so one column fits and it is exactly as wide as the content
/// area — for one row as well as for two hundred.
pub const SHELF_ROW_WIDTH_REM: f32 = 1000.0;

/// The author index is a list of names, not a shelf: most authors in a library
/// have one or two books, and a full-width shelf per name spent a screen on two
/// or three of them. A name is a line of text with a small stack of its covers.
///
/// An entry is at least this wide. The row's slack widens every entry, so a wide
/// window shows more columns rather than wider gaps.
pub const AUTHOR_INDEX_ENTRY_WIDTH_REM: f32 = 26.0;
/// Between one column of entries and the next.
pub const AUTHOR_INDEX_COLUMN_GAP: f32 = SPACE_LG;
/// A thumbnail's slot: the cover and the card padding around it. In rems rather
/// than the pixels a book card is measured in, because here the cover sits
/// beside a name and is sized against it — it has to grow with the text, or a
/// large UI font would outgrow the entry.
pub const AUTHOR_INDEX_COVER_SLOT_WIDTH_REM: f32 = 3.5;
/// How many covers an entry shows before stating the rest as a count.
pub const AUTHOR_INDEX_COVERS: usize = 3;
/// An entry is as tall as a slot's width would make a cover. The cover inside is
/// narrower by its padding, so it clears the entry with room to spare; the two
/// text lines beside it are shorter still at every font size, being in rems too.
pub const AUTHOR_INDEX_ENTRY_HEIGHT_REM: f32 = AUTHOR_INDEX_COVER_SLOT_WIDTH_REM / BOOK_COVER_ASPECT_RATIO;
/// The heading over each letter: its line box, and the padding and rule around it.
pub const AUTHOR_INDEX_LETTER_HEIGHT_REM: f32 = TEXT_LG * SHELF_HEADING_LINE_HEIGHT_EM;
pub const AUTHOR_INDEX_LETTER_FIXED_HEIGHT: f32 = SPACE_MD + SPACE_XS + 1.0;

/// The navigation rail and library switcher scale with the UI rem.
///
/// The rail's width has a floor set by its labels: the widest destination name
/// ("Settings") sets 48.5px in Libertinus small caps at `NAV_RAIL_LABEL_SIZE`
/// with the tracking below, so 5rem is the least this can be, and that floor is
/// what the rail is set to: the rail is navigation, not content, and every rem
/// it gives back goes to the list beside it. The destination tile is also the
/// unit the folder chips are cut from — see `COMPACT_HIERARCHY_ITEM_HEIGHT_REM`,
/// which is half of it. Because both the rail and the label are expressed in
/// rems, the label fit holds at every UI font size.
///
/// Measure again before adding a destination longer than "Settings", and note
/// that small caps are wider than the lowercase this was once sized against —
/// 4rem would leave under a SPACE_SM margin and is no longer enough.
pub const NAV_RAIL_SIZE_REM: f32 = 5.0;
/// Rail destinations are square: their height is the rail's width. Changing
/// the rail's width deliberately changes their height with it.
///
/// The square is still generous for what it holds: a 1.5rem icon, a SPACE_XXS
/// gap and one `NAV_RAIL_LABEL_SIZE` line come to 38px of the 80px box. That
/// slack is the point — it is what makes the item read as a tile rather than a
/// row.
///
/// Rail labels sit below their icon in a fixed-width column, so they run
/// smaller than ordinary caption text: the icon is the primary cue and the word
/// confirms it. Smaller here also buys margin against longer names before the
/// label has to ellipsize.
/// Set in small caps, whose letters stand at 0.483em against Libertinus'
/// 0.429em x-height. 12px therefore renders at the optical size 11px sans did,
/// and the label is not on the lowercase scale above.
pub const NAV_RAIL_LABEL_SIZE: f32 = 0.75;
pub const NAV_RAIL_ICON_SIZE_REM: f32 = 1.5;
pub const BOTTOM_NAV_HEIGHT_REM: f32 = 4.0;
/// The topbar is its actions plus a `CONTENT_INSET` above and below them, stated
/// as one rem height: its actions are rems, so a pixel bar would be outgrown by
/// its own buttons at a large UI font. The inset rides along at 16px to the rem
/// rather than staying fixed, because a height made of a rem part and a pixel
/// part cannot be given to a single element.
pub const TOPBAR_HEIGHT_REM: f32 = TOPBAR_ACTION_HEIGHT_REM + 2.0 * CONTENT_INSET / 16.0;
pub const TOPBAR_ACTION_HEIGHT_REM: f32 = 2.0;
pub const TOPBAR_ACTION_LABEL_WEIGHT: FontWeight = FontWeight::NORMAL;
pub const TOPBAR_ACTION_GAP: f32 = SPACE_SM;
pub const TOPBAR_ACTION_HORIZONTAL_PADDING: f32 = SPACE_SM;

/// The switcher and the compact "More" sheet are one row height throughout —
/// library rows, their delete buttons, the add button and the header all sit on
/// it. It was repeated as a bare `40.0` in five places.
pub const NAVIGATION_MODAL_ROW_HEIGHT_REM: f32 = 2.0;
/// The list stops growing here and scrolls instead.
pub const NAVIGATION_MODAL_MAX_HEIGHT_REM: f32 = 32.0;
/// Wide enough for a library name without being wider than a modal wants to be,
/// in rems because a library name is what it is wide for. The panel still
/// shrinks with the window through `max_w_full`.
pub const NAVIGATION_MODAL_WIDTH_REM: f32 = 20.5;

/// Compact hierarchy items used when folders and books share the same
/// paginator. These preserve the flat, borderless treatment of the original
/// hierarchy chips while keeping their geometry out of feature code.
///
/// Visible chip height; the paginator adds the row gap separately.
pub const COMPACT_HIERARCHY_ITEM_HEIGHT_REM: f32 = 3.0;
pub const COMPACT_HIERARCHY_ITEM_TEXT_SIZE_REM: f32 = TEXT_LG;
pub const COMPACT_HIERARCHY_ITEM_ROW_GAP: f32 = SPACE_SM;
pub const COMPACT_HIERARCHY_ITEM_HORIZONTAL_PADDING: f32 = CONTENT_INSET;
pub const COMPACT_HIERARCHY_ITEM_CONTENT_GAP: f32 = SPACE_SM;

/// Section cards: a title and total over columns of child entries, with a
/// single row of covers in the space left by the first column.
pub const SECTION_CARD_HEADING_HEIGHT_REM: f32 = 2.5;
pub const SECTION_CARD_SUBJECT_ROW_HEIGHT_REM: f32 = 1.5;
pub const SECTION_CARD_SUBJECT_ROWS_PER_COLUMN: usize = 10;
pub const SECTION_CARD_COLUMN_GAP: f32 = SPACE_MD;
/// The cover art in a section card's strip. The cell adds the book card's
/// padding around it.
pub const SECTION_CARD_COVER_WIDTH: f32 = 150.0;

/// The contents list, shared by the reader's sidebar, the audiobook player's
/// chapter table and the library's detail page.
///
/// The disclosure is small: it is a state marker beside a word, not a control
/// in its own right, and at the reader's old 22px a column of them competed
/// with the chapter titles. Its hit area is the row's full height, so the
/// smaller glyph costs nothing to aim at.
pub const CONTENTS_DISCLOSURE_SIZE_REM: f32 = 0.75;
/// One step of nesting. `SPACE_MD`, so a child is plainly indented from its
/// parent without a deep entry running out of column.
pub const CONTENTS_ROW_INDENT: f32 = SPACE_MD;
/// Between the parts of a row. Tighter than the page's ordinary gap: the
/// disclosure belongs to the title it opens, and `SPACE_MD` between them read
/// as two separate things.
pub const CONTENTS_ROW_GAP: f32 = SPACE_XS;
/// The right-hand column an entry states its amount in — a chapter's duration,
/// or a reading estimate where one is computed. Was `CHAPTER_COLUMN_WIDTH` in
/// the audiobook player, in pixels; in rems it keeps its proportion when the
/// interface font grows.
pub const CONTENTS_AMOUNT_WIDTH_REM: f32 = 5.125;

/// The rule a tab draws under its own label to say the panel below belongs to
/// it. Thicker than `rule`'s hairline on purpose: it is a state, not a divider.
pub const TAB_UNDERLINE_HEIGHT: f32 = 2.0;

/// Shared geometry for settings controls.
pub const SETTINGS_CONTROL_SIZE_REM: f32 = 2.0;
pub const SETTINGS_STEPPER_VALUE_WIDTH_REM: f32 = 4.5;
pub const SETTINGS_CONTROL_GAP: f32 = 1.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// The scale is only a scale if its steps are far enough apart to be told
    /// apart. Anything closer than a third invites picking by eye — which is
    /// how 5, 6, 7, 10 and 12 got into feature code in the first place.
    #[test]
    fn no_two_spacing_steps_are_closer_than_a_third() {
        let scale = [SPACE_XXS, SPACE_XS, SPACE_SM, SPACE_MD, SPACE_LG, SPACE_XL, SPACE_2XL, SPACE_3XL, SPACE_4XL];
        for pair in scale.windows(2) {
            let (smaller, larger) = (pair[0], pair[1]);
            assert!(larger > smaller, "the scale must climb: {smaller} is followed by {larger}");
            let step = (larger - smaller) / smaller;
            assert!(step >= 0.33, "{smaller} → {larger} is only {:.0}% — too close to be a decision", step * 100.0);
        }
    }
}
