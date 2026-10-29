use gpui::{FontWeight, Styled};
use crate::Button;

use crate::theme::BrowserTheme;

/// The interaction ladder, defined once.
///
/// A call site says what a thing *is* — current or not — and never names a
/// colour. That is the point: the treatment was previously spelled out at each
/// site, so the same state drifted into meaning `accent_wash` in some places
/// and a solid accent in others, and pages picked palette values directly.
///
///   rest      untouched
///   hover     `theme.hover` — the light wash under the pointer; translucent,
///             correct over any surface
///   keyboard  `accent_wash` — the keyboard cursor and the selection, which
///             is transient however it was made. The value chosen in a
///             settings control is marked separately with the current tier.
///   current   `accent` with `accent_text` on it — state the app holds: the
///             place you are, a mode that is on, a filter narrowing the
///             content. Menu and sheet choices share this solid accent and a
///             trailing checkmark through `gpui_component::menu::choice_row`.
///
/// Nothing is drawn as an outline for hover or the cursor. The pointer on a
/// keyboard item steps it to `accent_wash_hover`; the pointer or the cursor on
/// a current item steps it to `accent_hover`. `contents_row` lays out all of
/// them together. Book cards keep their own selection and cursor marks — see
/// `book_card`.
///
/// This trait covers rest, hover and current; the keyboard tier is the call
/// site's, because only it knows where the cursor is.
///
/// Do not call this on an element that already sets its own `hover`: gpui
/// asserts a hover style is only set once. Use [`selectable_button`] for the
/// application's `Button`, which owns its interaction styles.
pub trait Interactive: Styled + gpui::InteractiveElement + Sized {
    fn selectable(self, current: bool, theme: BrowserTheme) -> Self {
        // On Kobo nothing is filled: current is a black border on the page.
        if cfg!(feature = "kobo") {
            let element = self.hover(move |style| style.bg(theme.hover));
            return if current { element.border_2().border_color(theme.accent) } else { element };
        }
        // A current surface is already the accent, so a wash laid over it
        // would be invisible. It darkens instead — the same distinction
        // `accent_hover` exists for.
        let hovered = if current { theme.accent_hover } else { theme.hover };
        let element = self.hover(move |style| style.bg(hovered));
        if current { element.bg(theme.accent).text_color(theme.accent_text) } else { element }
    }
}

impl<T: Styled + gpui::InteractiveElement + Sized> Interactive for T {}

/// Shared current-state styling for the application's `Button`.
///
/// Selected buttons keep the heavy accent and contrasting text. Unselected
/// buttons take their light accent hover from the application button component.
pub fn selectable_button(button: Button, selected: bool, theme: BrowserTheme) -> Button {
    use gpui_component::Selectable;
    if !selected {
        return button;
    }
    // On Kobo nothing is filled: current is a black border on the page.
    if cfg!(feature = "kobo") {
        return button.selected(true).bg(theme.page_bg).text_color(theme.text).border_2().border_color(theme.accent).hover(move |style| style.bg(theme.hover));
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

/// Control density follows the primary input, independently of window width.
/// The live media query also follows changes to the browser's primary pointer.
pub fn touch_ui() -> bool {
    #[cfg(target_family = "wasm")]
    {
        use std::cell::OnceCell;
        thread_local! {
            static COARSE_POINTER: OnceCell<Option<web_sys::MediaQueryList>> = const { OnceCell::new() };
        }
        COARSE_POINTER.with(|query| query.get_or_init(|| web_sys::window()?.match_media("(pointer: coarse)").ok().flatten()).as_ref().is_some_and(web_sys::MediaQueryList::matches))
    }
    #[cfg(not(target_family = "wasm"))]
    {
        cfg!(all(feature = "mobile", not(feature = "kobo")))
    }
}
pub const TOUCH_TARGET_REM: f32 = 3.0;

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
/// Both interface faces share this scale; changing the family preserves the
/// user's chosen rem size.
pub const TEXT_XS: f32 = 0.875;
pub const TEXT_SM: f32 = 1.0;
pub const TEXT_MD: f32 = 1.125;
pub const TEXT_LG: f32 = 1.375;
pub const TITLE_MD: f32 = 1.6875;
pub const TITLE_LG: f32 = 2.125;

/// Display face for prominent headings in the serif UI. The sans-serif UI
/// uses its selected interface face for headings too.
pub const DISPLAY_FONT_FAMILY: &str = "Bodoni Moda";

/// Request small-cap glyphs from the selected interface face.
///
/// `smcp` maps lowercase onto the small-cap glyphs; `c2sc` does the same for
/// capitals, so no call site has to lowercase its strings. NV Garamond covers
/// a-z plus å, ä and ö; Raleway's small-cap coverage omits the Swedish letters.
///
/// Small caps stand at 0.483em against the face's 0.429em x-height, so text
/// set this way runs slightly larger than its nominal size suggests.
pub fn small_caps() -> gpui::FontFeatures {
    gpui::FontFeatures(std::sync::Arc::new(vec![("smcp".to_owned(), 1), ("c2sc".to_owned(), 1)]))
}

/// Placeholder proportions and the tallest cover frame reserved by book cards.
/// GPUI uses width over height; card JPEG generation enforces this limit.
pub const BOOK_COVER_ASPECT_RATIO: f32 = 1.0 / 1.6;
/// However narrow the window, the book grid keeps at least this many columns.
/// One cover stretched across a phone screen is not a grid, and the covers are
/// what the reader scans.
pub const BOOK_GRID_MINIMUM_COLUMNS: usize = 2;
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
/// Book-card text shares three lines between title and subtitle, reserving
/// one for each when both exist and giving the title first claim. Keep
/// those blocks in rems: the interface font can have different glyph widths,
/// and the user's UI font size must enlarge the card instead of being
/// clipped by the 16px-reference geometry in the application contract.
/// On the scale rather than beside it: this was 16.0, a value that appeared
/// nowhere else and sat between two steps.
pub const BOOK_CARD_TITLE_SIZE: f32 = TEXT_SM;
/// Main titles carry more weight than subtitles.
pub const BOOK_CARD_TITLE_WEIGHT: FontWeight = FontWeight::SEMIBOLD;
pub const BOOK_CARD_TITLE_LINE_HEIGHT_EM: f32 = 1.25;
pub const BOOK_CARD_TEXT_LINES: usize = 3;
pub const BOOK_CARD_SUBTITLE_SIZE: f32 = TEXT_XS;
/// Compact card spacing for the GPUI browser surface.
pub const BOOK_CARD_COVER_CONTENT_GAP: f32 = SPACE_SM;
// Three main-title lines are the tallest allocation of the shared budget.
pub const BOOK_CARD_TITLE_HEIGHT_REM: f32 = BOOK_CARD_TITLE_SIZE * BOOK_CARD_TITLE_LINE_HEIGHT_EM * BOOK_CARD_TEXT_LINES as f32;
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
/// A download action's size — the badge drawn on a cover, the slot at the end
/// of a folder or subject chip — and the glyph inside it. Rems, so the glyph keeps
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

/// The compact detail page's single measure.
pub const BOOK_DETAIL_PAGE_WIDTH_REM: f32 = 38.5;
/// The cover's width; on desktop, also the player column's width.
pub const BOOK_DETAIL_COVER_WIDTH: f32 = 360.0;
/// Desktop contents and description columns grow together to this cap;
/// the player column uses the cover's width. Spare width goes to margins and gaps.
pub const BOOK_DETAIL_COLUMN_MAX_WIDTH_REM: f32 = 30.0;
/// The least room between two desktop columns, and between a column and the
/// page's edge. Spare width is shared equally among all of them.
pub const BOOK_DETAIL_COLUMN_GAP: f32 = SPACE_XL;
pub const BOOK_DETAIL_PAGE_MARGIN: f32 = SPACE_LG;
/// The cover takes at most half the hero, so a narrow window never leaves the
/// title in a column one word wide.
pub const BOOK_DETAIL_COVER_FRACTION: f32 = 0.5;
pub const BOOK_DETAIL_SECTION_GAP: f32 = SPACE_XL;
pub const BOOK_DETAIL_HERO_GAP: f32 = SPACE_LG;
/// Detail titles run at `TITLE_MD`, subtitles at `TEXT_MD`.
pub const BOOK_DETAIL_DESCRIPTION_SIZE: f32 = TEXT_SM;
pub const BOOK_DETAIL_DESCRIPTION_LINE_HEIGHT_EM: f32 = 1.5;

/// The declared width of a shelf row, which is never the drawn one.
///
/// The paginator fits children across a row at their declared width, and a shelf
/// row is meant to have the row to itself. Any value wider than the window does
/// that: a declared width is clamped to the space available before the column
/// count is taken, so one column fits and it is exactly as wide as the content
/// area — for one row as well as for two hundred.
pub const SHELF_ROW_WIDTH_REM: f32 = 1000.0;

/// The navigation rail and library switcher scale with the UI rem.
///
/// The rail reserves 5rem for icons and destination labels. The
/// destination tile is also the unit the folder chips are cut from — see
/// `COMPACT_HIERARCHY_ITEM_HEIGHT_REM`, which is half of it. Both the rail
/// and its labels scale with the chosen UI size.
///
/// Measure both interface faces before adding a longer destination label.
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
/// Destination labels in their original case, sized to fit the rail.
pub const NAV_RAIL_LABEL_SIZE: f32 = 0.75;
pub const NAV_RAIL_ICON_SIZE_REM: f32 = 1.5;
pub const BOTTOM_NAV_HEIGHT_REM: f32 = 4.0;
/// The topbar is its actions plus a `CONTENT_INSET` above and below them, stated
/// as one rem height: its actions are rems, so a pixel bar would be outgrown by
/// its own buttons at a large UI font. The inset rides along at 16px to the rem
/// rather than staying fixed, because a height made of a rem part and a pixel
/// part cannot be given to a single element.
pub fn topbar_height_rem() -> f32 {
    topbar_action_height_rem() + 2.0 * CONTENT_INSET / 16.0
}
/// Also the size of every square icon button (`outlined_icon_button`).
pub fn topbar_action_height_rem() -> f32 {
    if touch_ui() { TOUCH_TARGET_REM } else { 2.0 }
}
pub const TOPBAR_ACTION_LABEL_WEIGHT: FontWeight = FontWeight::NORMAL;
/// Touch-sized icon buttons already carry their spacing as padding around the
/// glyph, and a row of them must still fit a phone's width beside the title.
pub fn topbar_action_gap() -> f32 {
    if touch_ui() { 0.0 } else { SPACE_SM }
}
pub const TOPBAR_ACTION_HORIZONTAL_PADDING: f32 = SPACE_SM;

/// The switcher and the compact "More" sheet are one row height throughout —
/// library rows, their delete buttons, the add button and the header all sit on
/// it. It was repeated as a bare `40.0` in five places.
pub fn navigation_modal_row_height_rem() -> f32 {
    if touch_ui() { TOUCH_TARGET_REM } else { 2.0 }
}
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

/// Container cards show one column of child entries beneath the title.
/// Match folder cards so their width follows the cover size.
pub const SECTION_CARD_WIDTH: f32 = FOLDER_CARD_WIDTH;
pub const SECTION_CARD_SUBJECT_COLUMNS: usize = 1;
pub const SECTION_CARD_HEADING_HEIGHT_REM: f32 = 2.5;
pub const SECTION_CARD_HEADING_PADDING_REM: f32 = 0.5;
pub const SECTION_CARD_SUBJECT_ROW_HEIGHT_REM: f32 = 1.5;
pub const SECTION_CARD_COLUMN_GAP: f32 = SPACE_MD;
/// The cover art in a section card's strip. The cell adds the book card's
/// padding around it.
pub const SECTION_CARD_COVER_WIDTH: f32 = 150.0;
/// Two cover cells plus the enclosing section's horizontal inset.
pub const FOLDER_CARD_WIDTH: f32 = (SECTION_CARD_COVER_WIDTH + BOOK_CARD_PADDING * 2.0) * 2.0 + (SPACE_MD - BOOK_CARD_PADDING) * 2.0;

/// The contents list, shared by the reader's sidebar, the audiobook player's
/// chapter table and the library's detail page.
///
/// The disclosure is small: it is a state marker beside a word, not a control
/// in its own right, and at the reader's old 22px a column of them competed
/// with the chapter titles. This is the glyph; the button around it is a
/// square as tall as a one-line row, [`CONTENTS_ROW_MIN_HEIGHT_REM`], because
/// on the detail page it is the only way to open a part.
pub const CONTENTS_DISCLOSURE_SIZE_REM: f32 = 0.625;
/// A one-line contents row, and so the side of its disclosure button.
pub const CONTENTS_ROW_MIN_HEIGHT_REM: f32 = 2.25;
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
/// One digit of an entry's number, in ems of the number's own size. GPUI has
/// no `ch`; this is a little over the interface face's figure width, so a
/// column sized by digit count never clips its widest number.
pub const CONTENTS_DIGIT_WIDTH_EM: f32 = 0.55;

/// Shared geometry for settings controls.
pub fn settings_control_size_rem() -> f32 {
    if touch_ui() { TOUCH_TARGET_REM } else { 2.0 }
}
pub const SETTINGS_STEPPER_VALUE_WIDTH_REM: f32 = 4.5;
pub const SETTINGS_CONTROL_GAP: f32 = 1.0;
