use gpui::prelude::*;
use gpui::{AnyElement, Div, ElementId, ImageSource, ObjectFit, SharedString, Stateful, StyledImage, div, img, px};
use gpui_component::breadcrumb::{Breadcrumb, BreadcrumbItem};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::spinner::Spinner;
use gpui_component::{Icon, IconName, Selectable, Sizable};
use unicode_normalization::UnicodeNormalization;

use crate::{
    BOOK_CARD_COVER_CONTENT_GAP, BOOK_CARD_TITLE_LINE_HEIGHT_EM, BOOK_CARD_TITLE_LINES, BOOK_CARD_TITLE_SIZE, BOOK_CARD_TITLE_WEIGHT, BOOK_COVER_ASPECT_RATIO, BrowserTheme, CAROUSEL_CONTENT_GAP, CAROUSEL_CONTROL_GAP, CAROUSEL_TITLE_SIZE,
    CAROUSEL_TITLE_WEIGHT, RADIUS_MD, RADIUS_SM, SHELF_SUBJECT_SIZE, SPACE_SM, SPACE_XS, SPACE_XXS, TEXT_SM, TEXT_XS,
};

/// Cover, then the text block. The two levels each have one gap: `SPACE_MD`
/// separates the art from the words, `SPACE_XS` separates the words from each
/// other. Previously the wider first gap was a margin on the title fighting the
/// column's own gap, which meant two spacing mechanisms in one component.
/// A focused card draws `accent_wash` as a rounded tile inside the grid's gap,
/// and leaves every foreground on the card alone.
///
/// It used to invert: `accent` with `accent_text` over it. An inversion has to
/// reach the edges of its cell or it reads as a hole in the fill, which is why
/// the grid ran at a zero gap and the card carried the spacing as its own
/// padding. A tint does not — so the gap came back (`BOOK_GRID_BOOK_GAP`) and
/// the padding became the tile's inset instead of the spacer between cards.
///
/// Hover depends on whether the card is already tinted, because gpui's hover
/// style *replaces* `background` rather than painting over it. On a plain card
/// the translucent `hover` composites against the page, which is what it is
/// for; on a tinted one it would composite against the page too and throw the
/// tile away, so the card would stop looking focused exactly when the pointer
/// reached it. A focused card hovers to `accent_wash_hover` instead — the same
/// tint, lifted.
///
/// `marquee` drops hover entirely: a rubber band drags the pointer over every
/// card it collects, and cards answering that individually report the pointer's
/// path instead of the selection.
pub fn book_card(id: impl Into<ElementId>, focused: bool, marquee: bool, theme: BrowserTheme) -> Stateful<Div> {
    let hovered = if focused { theme.accent_wash_hover } else { theme.hover };
    div()
        .id(id)
        .relative()
        .w_full()
        .min_w_0()
        .flex_none()
        .flex()
        .flex_col()
        .p(px(crate::BOOK_CARD_PADDING))
        .gap(px(BOOK_CARD_COVER_CONTENT_GAP))
        .rounded(px(RADIUS_MD))
        .when(focused, |card| card.bg(theme.accent_wash))
        .when(!marquee, |card| card.hover(move |style| style.bg(hovered)))
}

/// The grid cell a card sits in, at the row's uniform reserved height.
///
/// Carries no background of its own — a card's cover is boxed at its own
/// real proportions rather than the placeholder's, so `book_card` (which does
/// carry the focus tint) comes out shorter or taller than the cell. This cell
/// is plain space around it. The default `justify_end` pins grid and section
/// cards to the bottom edge; graph covers override it to align at the top.
pub fn book_card_cell() -> Div {
    div().relative().w_full().h_full().min_w_0().flex().flex_col().justify_end()
}

pub fn book_card_text() -> Div {
    div().w_full().min_w_0().flex_none().flex().flex_col().gap(px(crate::BOOK_CARD_TEXT_GAP))
}

/// The text block under a card's cover — see `book_card_title_row`. Reserves
/// the title budget the paginator's row height is computed from, so a title
/// without a subtitle carries its slack below itself rather than shortening
/// the card.
pub fn book_card_text_title_only() -> Div {
    div().w_full().min_w_0().flex_none().flex().flex_col().h(gpui::rems(crate::BOOK_CARD_TITLE_HEIGHT_REM))
}

/// `aspect_ratio` is the cover's own width-over-height once its art is
/// decoded, so the box takes the real cover's proportions instead of every
/// book being forced into the same frame. Callers without a decoded image yet
/// pass `BOOK_COVER_ASPECT_RATIO` as a placeholder.
pub fn book_cover(_theme: BrowserTheme, aspect_ratio: f32) -> Div {
    div().relative().w_full().min_w_0().aspect_ratio(aspect_ratio)
}

pub fn book_cover_frame(theme: BrowserTheme, aspect_ratio: f32) -> Div {
    crate::clickable_surface(theme).relative().w_full().min_w_0().aspect_ratio(aspect_ratio)
}

/// A compact length mark that also identifies an audiobook by its headphones.
pub fn book_cover_audiobook_length(length: Option<impl Into<SharedString>>) -> Div {
    div()
        .absolute()
        .left(px(8.0))
        .bottom(px(8.0))
        .flex()
        .items_center()
        .gap(px(5.0))
        .px(px(7.0))
        .py(px(5.0))
        .bg(gpui::black().opacity(0.55))
        .text_color(gpui::white())
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(Icon::new(crate::Headphones).size(px(11.0)))
        .children(length.map(|length| length.into()))
}

/// Cover art scaled to fit the standard 1:1.5 cover frame.
///
/// Scaled rather than cropped: a cover's own proportions are part of what it
/// is, and a square audiobook cover cropped to a paperback's 2:3 would lose
/// its edges rather than just sit in a taller frame than most books need.
///
/// A small neutral shadow lifts the jacket without making a larger coloured
/// rectangle around it. The outline itself stays invisible except when
/// focus or selection picks it out in the accent colour — a hover border
/// read as a stray black line rather than a lift.
/// A shaded strip down the binding edge gives the front board a little shape.
///
/// Only the outer edge rounds. A jacket's spine side is a flat fold against
/// the pages, not a curve, so rounding it along with the outer corners would
/// contradict the spine shade and hinge sitting right next to it.
pub fn book_cover_image(id: impl Into<SharedString>, source: impl Into<ImageSource>, opacity: f32, focused: bool, theme: BrowserTheme) -> AnyElement {
    let id = id.into();
    div()
        .id(ElementId::from(id.clone()))
        .relative()
        .size_full()
        .overflow_hidden()
        .rounded_r(px(crate::BOOK_COVER_RADIUS))
        .border_1()
        .border_color(if focused { theme.accent } else { gpui::transparent_black() })
        .shadow(book_cover_shadow(theme, false))
        .hover(move |style| style.shadow(book_cover_shadow(theme, true)))
        // Focus lifts the cover as a supporting cue to the selected card.
        .when(focused, |cover| cover.shadow(book_cover_shadow(theme, true)))
        // `overflow_hidden` on the wrapping div clips with a rectangular
        // mask regardless of its own corner radius — an image paints its
        // corners from its own style, so the rounding has to be set here too.
        .child(img(source).size_full().object_fit(ObjectFit::Contain).opacity(opacity).rounded_r(px(crate::BOOK_COVER_RADIUS)))
        .child(book_cover_spine())
        .child(book_cover_hinge())
        .into_any_element()
}

/// The palette's general overlay shadow can be gold in dark mode. Covers
/// instead cast a short neutral shadow; the alpha still follows the theme so
/// e-ink keeps its zero-shadow treatment.
fn book_cover_shadow(theme: BrowserTheme, lifted: bool) -> Vec<gpui::BoxShadow> {
    let mut color = gpui::black();
    color.a = theme.shadow.a * if lifted { 1.2 } else { 0.8 };
    let (offset, blur) = if lifted { (4.0, 12.0) } else { (2.0, 6.0) };
    vec![gpui_component::box_shadow(px(0.0), px(offset), px(blur), px(0.0), color)]
}

/// The gradient strip `book_cover_image` shades its leading edge with.
///
/// Painted as the image's last child so it lands above the art rather than
/// under it — an opaque cover would otherwise hide anything drawn first.
fn book_cover_spine() -> gpui::Div {
    div().absolute().left_0().top_0().bottom_0().w(px(crate::BOOK_COVER_SPINE_WIDTH)).bg(gpui::linear_gradient(90.0, gpui::linear_color_stop(gpui::black().opacity(0.35), 0.0), gpui::linear_color_stop(gpui::transparent_black(), 1.0)))
}

/// The shallow hinge groove just inside the spine shade. Its recessed line is
/// centered at 12 px; a faint catchlight on the board side keeps it legible on
/// dark jacket artwork without drawing a full bright outline.
fn book_cover_hinge() -> gpui::Div {
    // Four two-stop bands reconstruct the original five-point curve exactly —
    // `linear_gradient` here takes only one `from` and one `to`.
    let stops = [gpui::transparent_black(), gpui::black().opacity(0.12), gpui::black().opacity(0.28), gpui::white().opacity(0.14), gpui::transparent_black()];
    let band = |left: f32, from: gpui::Hsla, to: gpui::Hsla| div().absolute().left(px(left)).top_0().bottom_0().w(px(1.0)).bg(gpui::linear_gradient(90.0, gpui::linear_color_stop(from, 0.0), gpui::linear_color_stop(to, 1.0)));
    div()
        .absolute()
        .left(px(10.0))
        .top_0()
        .bottom_0()
        .w(px(4.0))
        .children((0..4).map(|index| band(index as f32, stops[index], stops[index + 1])))
}

/// The reader's place in the book, drawn on the cover's own bottom edge.
///
/// Laid over the artwork rather than in the gap below it, on a scrim dark
/// enough to keep the mark legible against art of any colour — without it,
/// an accent-coloured bar disappears into a cover already carrying warm or
/// saturated tones there. Length is the entire signal: nothing at zero, full
/// at the end. There is no separate finished treatment — a finished book is
/// one whose rule is full, and hiding those is the filter's business, not the
/// card's. Because length rather than colour carries it, the mark survives
/// greyscale.
pub fn book_cover_progress(progress: f32, theme: BrowserTheme) -> Option<AnyElement> {
    let fraction = (progress / 100.0).clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return None;
    }
    Some(
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(crate::BOOK_COVER_PROGRESS_SCRIM_HEIGHT))
            .flex()
            .flex_col()
            .justify_end()
            .child(div().absolute().inset_0().bg(gpui::linear_gradient(0.0, gpui::linear_color_stop(gpui::black().opacity(0.6), 0.0), gpui::linear_color_stop(gpui::transparent_black(), 1.0))))
            .child(div().relative().h(px(crate::BOOK_COVER_PROGRESS_HEIGHT)).flex().flex_row().child(div().w(gpui::relative(fraction)).h_full().bg(theme.accent)))
            .into_any_element(),
    )
}

/// Stand-in for a book with no cover art.
///
/// The label was previously an unstyled raw string at inherited body size,
/// which read as loud as a real title. Muted and small: it is an absence, not
/// a thing to look at.
pub fn missing_book_cover(label: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    book_cover(theme, BOOK_COVER_ASPECT_RATIO)
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded_r(px(crate::BOOK_COVER_RADIUS))
        .border_1()
        .border_color(theme.rule)
        .bg(theme.page_bg)
        .gap(px(SPACE_XS))
        .child(Icon::new(IconName::BookOpen).large().text_color(theme.text_muted))
        .child(div().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(label.into()))
}

/// Fixed book-cloth colours shared by generated covers and section headings.
/// They stay the same when the reader switches themes.
const LIBRARY_GRADIENTS: [(u32, u32); 12] = [
    (0x794e5c, 0x5b3b45), // burgundy
    (0x8b453f, 0x68342f), // brick
    (0x955341, 0x703e31), // terracotta
    (0x806335, 0x604a28), // amber
    (0x666b37, 0x4d5029), // olive
    (0x5c7045, 0x455434), // sage
    (0x48715a, 0x365544), // forest
    (0x3d7374, 0x2e5657), // teal
    (0x4b698e, 0x384f6b), // blue
    (0x566777, 0x414d59), // slate
    (0x62618a, 0x4a4968), // indigo
    (0x795278, 0x5b3e5a), // plum
];

/// A repeatable palette choice for a folder name or canonical subject path.
/// Keep the palette order and this hash fixed once colours are in use: changing
/// either would recolour existing folders and subjects.
pub fn section_card_band_index(kind: crate::NavigationIcon, key: &str) -> usize {
    let mut hash = 0xcbf29ce484222325_u64;
    let namespace = if kind == crate::NavigationIcon::Folder { b'F' } else { b'S' };
    hash = (hash ^ u64::from(namespace)).wrapping_mul(0x100000001b3);
    let normalized = key.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().nfc().collect::<String>();
    for byte in normalized.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    // Avalanche the FNV result so neighbouring names do not favour nearby
    // palette slots, including when the slot count has small factors.
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51afd7ed558ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ceb9fe1a85ec53);
    hash ^= hash >> 33;
    // Reduce before converting to usize so 32- and 64-bit devices agree.
    (hash % LIBRARY_GRADIENTS.len() as u64) as usize
}

#[cfg(test)]
mod section_band_tests {
    use super::section_card_band_index;
    use crate::NavigationIcon;

    #[test]
    fn assignment_is_stable_across_equivalent_names() {
        assert_eq!(section_card_band_index(NavigationIcon::Folder, "History"), 0);
        assert_eq!(section_card_band_index(NavigationIcon::Folder, "  HISTORY  "), 0);
        assert_eq!(section_card_band_index(NavigationIcon::Folder, "Café"), section_card_band_index(NavigationIcon::Folder, "Cafe\u{301}"));
        assert_eq!(section_card_band_index(NavigationIcon::Tags, "History / Europe"), 9);
    }
}

fn section_card_band_colors(index: usize) -> (gpui::Hsla, gpui::Hsla) {
    #[cfg(feature = "kobo")]
    let (from, to) = (0x000000, 0x333333);
    #[cfg(not(feature = "kobo"))]
    let (from, to) = LIBRARY_GRADIENTS[index % LIBRARY_GRADIENTS.len()];
    #[cfg(feature = "kobo")]
    let _ = index;
    (gpui::rgb(from).into(), gpui::rgb(to).into())
}

/// A cover for a book with no art of its own, used under the "cover text:
/// cover only" setting in place of [`missing_book_cover`]: that placeholder
/// says nothing without the title line the setting has just turned off, so a
/// book with no jacket would otherwise be the one anonymous tile in the grid.
///
/// `variant` picks the jacket colour — pass something stable per book (its
/// content hash, not a random draw) so the same book always generates the
/// same cover. Built from a gradient and text, nothing rasterized, so it
/// costs what any other card element costs.
pub fn book_cover_generated(title: impl Into<SharedString>, subtitle: Option<&str>, variant: usize, theme: BrowserTheme) -> Div {
    let (from, to) = LIBRARY_GRADIENTS[variant % LIBRARY_GRADIENTS.len()];
    let ink: gpui::Hsla = gpui::rgb(0xf2efe8).into();
    let subtitle = subtitle.map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned);
    book_cover(theme, BOOK_COVER_ASPECT_RATIO)
        .overflow_hidden()
        .rounded_r(px(crate::BOOK_COVER_RADIUS))
        .bg(gpui::linear_gradient(155.0, gpui::linear_color_stop(gpui::rgb(from), 0.0), gpui::linear_color_stop(gpui::rgb(to), 1.0)))
        .flex()
        .flex_col()
        .justify_end()
        .p(px(SPACE_SM * 2.0))
        .gap(px(SPACE_XXS))
        .text_color(ink)
        .child(div().w(px(28.0)).h(px(2.0)).bg(ink).opacity(0.6))
        .child(
            div()
                .w_full()
                .min_w_0()
                .mt(px(SPACE_XS))
                .font_weight(BOOK_CARD_TITLE_WEIGHT)
                .text_size(gpui::rems(BOOK_CARD_TITLE_SIZE))
                .line_height(gpui::relative(BOOK_CARD_TITLE_LINE_HEIGHT_EM))
                .text_ellipsis()
                .line_clamp(BOOK_CARD_TITLE_LINES)
                .child(title.into()),
        )
        .children(subtitle.map(|subtitle| div().w_full().min_w_0().opacity(0.85).text_size(gpui::rems(crate::BOOK_CARD_SUBTITLE_SIZE)).text_ellipsis().line_clamp(1).child(subtitle)))
}

/// No colour of its own: the title inherits from the card, which is what turns
/// it to `accent_text` when the cell fills with the accent.
///
/// Takes only the lines it needs rather than reserving the full
/// title-and-subtitle height itself — see `book_card_text_title_only`, which
/// reserves that budget as one box around the title.
pub fn book_card_title() -> Div {
    div().w_full().min_w_0().flex_none().text_size(gpui::rems(BOOK_CARD_TITLE_SIZE)).line_height(gpui::relative(BOOK_CARD_TITLE_LINE_HEIGHT_EM)).font_weight(BOOK_CARD_TITLE_WEIGHT)
}

/// Show the main title over up to two lines, with one smaller subtitle line
/// under it. The title takes the space first — it's the line a reader scans
/// for — so a long subtitle is what gives way to the ellipsis, never the title.
pub fn book_title_lines(title: impl Into<SharedString>, subtitle: Option<&str>) -> Div {
    let title = title.into();
    let Some(subtitle) = subtitle.map(str::trim).filter(|value| !value.is_empty()) else {
        return div().w_full().min_w_0().text_ellipsis().line_clamp(BOOK_CARD_TITLE_LINES).child(title);
    };
    div().w_full().min_w_0().flex().flex_col().child(div().w_full().min_w_0().text_ellipsis().line_clamp(BOOK_CARD_TITLE_LINES).child(title)).child(
        div()
            .w_full()
            .min_w_0()
            .text_size(gpui::rems(crate::BOOK_CARD_SUBTITLE_SIZE))
            .line_height(gpui::relative(BOOK_CARD_TITLE_LINE_HEIGHT_EM))
            .font_weight(gpui::FontWeight::NORMAL)
            .text_ellipsis()
            .line_clamp(crate::BOOK_CARD_SUBTITLE_LINES)
            .child(subtitle.to_owned()),
    )
}

/// A card's title row: title and subtitle across the card's whole width.
///
/// The download control used to sit beside them, which took width from only
/// the cards whose books were not on the device — so titles in one row wrapped
/// at different places. It is on the cover now; see
/// [`book_cover_download_badge`].
pub fn book_card_title_row(title: impl Into<SharedString>, subtitle: Option<&str>) -> Div {
    div().w_full().min_w_0().flex_none().child(book_card_title().child(book_title_lines(title, subtitle)))
}

/// A detailed card's two columns: the cover, then the words beside it.
///
/// The row's height is the cover's, and the text column stretches to it — which
/// is what gives the blurb a definite box to fill and be clipped by.
pub fn book_card_detail_body() -> Div {
    div().w_full().min_w_0().flex().flex_row().gap(px(crate::BOOK_CARD_DETAIL_GAP))
}

/// The left half of a detailed card, which is the ordinary card entire: cover,
/// then title.
///
/// The cover shrinks with narrow cards and stops at its preferred width.
/// Extra card width belongs to the description.
///
/// The words stay under the cover rather than moving in beside it, for
/// scanning: a title set directly above a paragraph of the same measure has a
/// weak boundary, and reading the titles across a row would mean passing over
/// the prose between them. Under the covers every title falls in the same place
/// on every card — the same place it holds in covers mode, so switching modes
/// no longer moves it — and the blurb is left as one uninterrupted block.
pub fn book_card_detail_cover() -> Div {
    div().w(gpui::relative(crate::BOOK_CARD_DETAIL_COVER_FRACTION)).max_w(px(crate::BOOK_CARD_CONTENT_WIDTH)).flex_none().flex().flex_col().gap(px(BOOK_CARD_COVER_CONTENT_GAP))
}

/// The right half: the blurb and nothing else, filling the card's full height.
pub fn book_card_detail_text() -> Div {
    div().flex_1().min_w_0().flex().flex_col()
}

/// The blurb, filling whatever the title leaves and clipped there.
///
/// Takes the description as stored — which is HTML — and sets what a card can
/// use of it: paragraphs as line breaks, emphasis as emphasis, entities as the
/// characters they name. See [`crate::description`] for why this is not the
/// document renderer.
///
/// Set in the author's colour rather than the title's: it is supporting text,
/// and a paragraph at full strength would outweigh the title it sits under.
pub fn book_card_description(id: impl Into<ElementId>, description: &str, theme: BrowserTheme) -> Option<Stateful<Div>> {
    let (text, highlights) = crate::description::rich_text(description);
    if text.is_empty() {
        return None;
    }
    Some(
        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .cursor_pointer()
            .text_size(gpui::rems(crate::TEXT_SM))
            .line_height(gpui::relative(crate::BOOK_CARD_DESCRIPTION_LINE_HEIGHT_EM))
            .text_color(theme.text_muted)
            .child(crate::description::BlurbText::new(text, highlights)),
    )
}

/// The full blurb, opened from the card that could only show the top of it.
///
/// A panel rather than a page: the card it came from stays where it was, so
/// closing it returns the reader to the same spot in the grid. It holds its own
/// wheel — see the card, which stops the event reaching the paginator, since
/// otherwise reading past the fold would turn the page underneath.
///
/// The panel is as tall as the blurb needs and no taller, up to a share of the
/// window. That bound is a *maximum*, so its own height is content-driven —
/// which is why the body inside it must keep an automatic flex basis. Given
/// `flex_1`, the body's basis is zero, and a zero basis inside an
/// auto-height column resolves to a zero-height box: the heading draws, the
/// blurb does not, and nothing about the panel looks wrong.
pub fn book_description_panel(theme: BrowserTheme) -> Div {
    crate::modal_surface(theme).w(gpui::rems(crate::BOOK_DESCRIPTION_OVERLAY_WIDTH_REM)).max_w_full().max_h(gpui::relative(crate::BOOK_DESCRIPTION_OVERLAY_HEIGHT_FRACTION)).flex().flex_col()
}

/// Holds the blurb between the heading and the bottom of the panel.
///
/// Shrinkable rather than growable — see [`book_description_panel`] — and
/// `min_h_0` so it may be shrunk below its content, which is what gives the
/// scroll area a bounded height to scroll inside.
pub fn book_description_scroll_area() -> Div {
    div().min_h_0().flex_shrink(1.0)
}

/// The panel's heading: the book the blurb belongs to, so an overlay opened
/// from a grid of covers still says which one it is about.
pub fn book_description_heading(title: impl Into<SharedString>, subtitle: Option<&str>, author: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    let author = author.into();
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SPACE_XXS))
        .p(px(crate::SPACE_MD))
        .border_b_1()
        .border_color(theme.rule)
        .child(div().text_size(gpui::rems(CAROUSEL_TITLE_SIZE)).font_weight(BOOK_CARD_TITLE_WEIGHT).child(title.into()))
        .children(subtitle.map(str::trim).filter(|value| !value.is_empty()).map(|subtitle| div().text_size(gpui::rems(BOOK_CARD_TITLE_SIZE)).font_weight(gpui::FontWeight::NORMAL).child(subtitle.to_owned())))
        .children((!author.is_empty()).then(|| div().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(author)))
}

/// The blurb in full: no clamp and no ellipsis, because whatever does not fit
/// is reached by scrolling rather than cut off.
pub fn book_description_body(description: &str, theme: BrowserTheme) -> Div {
    let (text, highlights) = crate::description::rich_text(description);
    div()
        .w_full()
        .p(px(crate::SPACE_MD))
        .text_size(gpui::rems(crate::TEXT_MD))
        .line_height(gpui::relative(crate::BOOK_DESCRIPTION_OVERLAY_LINE_HEIGHT_EM))
        .text_color(theme.text)
        .child(gpui::StyledText::new(text).with_highlights(highlights))
}

/// The mark of a book that is not on this device, in its cover's top-right
/// corner — the one the progress line, the audiobook tag and the spine leave
/// free — and the control that fetches it.
///
/// A dark plate under a light glyph, like the scrim the progress line sits on,
/// so it reads against any cover art in either theme. No border: a bordered
/// square beside a title read as a checkbox. `pending` swaps the glyph for a
/// spinner in the same place, so nothing moves when the mark is pressed; the
/// caller only makes the idle mark clickable.
pub fn book_cover_download_badge(id: impl Into<ElementId>, pending: bool) -> Stateful<Div> {
    let plate = gpui::black().opacity(0.45);
    let plate_hover = gpui::black().opacity(0.7);
    div()
        .id(id)
        .absolute()
        .top(px(crate::BOOK_COVER_BADGE_INSET))
        .right(px(crate::BOOK_COVER_BADGE_INSET))
        .size(gpui::rems(crate::DOWNLOAD_ACTION_SIZE_REM))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_SM))
        .bg(plate)
        .when(pending, |badge| badge.child(Spinner::new().icon(IconName::LoaderCircle).xsmall().color(gpui::white())))
        .when(!pending, |badge| badge.cursor_pointer().hover(move |style| style.bg(plate_hover)).child(download_glyph().text_color(gpui::white())))
}

/// The cloud-and-arrow glyph every download action carries, on a book's cover
/// and on a folder or subject chip alike.
pub fn download_glyph() -> Icon {
    Icon::empty().path("icons/cloud-download.svg").size(gpui::rems(crate::DOWNLOAD_ICON_SIZE_REM))
}

/// A page of shelves: rows of a paginator rather than a scrolling column. The
/// paginator supplies the vertical extent, so there is nothing here but the
/// page's own inset. The author index is laid out in the same container.
pub fn shelf_page() -> Div {
    crate::full_size_column().p(px(crate::SPACE_MD))
}

/// One row: the heading, then the shelf it names.
///
/// A whole row per shelf is what lets the width of the page go to books — the
/// heading takes a line rather than a column beside the covers.
pub fn shelf_row() -> Div {
    div().w_full().min_w_0().h_full().flex().flex_col().gap(px(CAROUSEL_CONTENT_GAP))
}

/// The line over a shelf: its title, then its controls at the far end.
///
/// Inset by `CONTENT_INSET` so the title begins on the same line as the first
/// cover beneath it. The covers carry that inset as card padding — the heading
/// has to match it rather than sit flush, or every title hangs a card's padding
/// to the left of its own row.
///
/// Its height is stated rather than left to the font because the row's height is
/// computed from it — see `shelf_rows_policy`.
pub fn shelf_heading() -> Div {
    div().w_full().min_w_0().flex_none().h(gpui::rems(crate::SHELF_HEADING_HEIGHT_REM)).flex().flex_row().items_center().justify_between().px(px(crate::CONTENT_INSET)).gap(px(CAROUSEL_CONTENT_GAP))
}

/// What the shelf is, at the head of the row, tinted when the keyboard cursor
/// is on this row. A title too long for the heading is elided rather than
/// pushing the controls off the row.
pub fn shelf_title(title: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .font_family(crate::DISPLAY_FONT_FAMILY)
        .text_size(gpui::rems(CAROUSEL_TITLE_SIZE))
        .font_weight(CAROUSEL_TITLE_WEIGHT)
        .when(selected, |title| title.text_color(theme.text_accent))
        .child(title.into())
}

/// The pill that follows the pointer while items are being dragged. It names
/// what is being carried rather than miniaturising it: a half-transparent copy
/// of three book covers reads as a rendering fault, and a count does not.
pub fn drag_preview(label: SharedString, theme: BrowserTheme) -> Div {
    div().px(px(SPACE_SM)).h(px(28.0)).flex().items_center().rounded(px(RADIUS_SM)).bg(theme.accent).text_color(theme.accent_text).text_size(gpui::rems(TEXT_XS)).child(label)
}

/// One subject chip: a quiet outline, because it states what a book is about
/// rather than offering somewhere to go.
///
/// A parent is stated ahead of the label and set a shade quieter, because many
/// leaves mean nothing alone — "Communities", "20th century", "Sources" — while
/// the eye should still land on the word the chip is about. A top-level subject
/// passes `None` and reads as the bare word it already is.
///
/// A chip is as wide as what it says. It is not elided and does not shrink: a
/// subject is a short phrase, and half of one is worse than the row it costs —
/// where chips no longer fit across, they wrap onto the next row.
///
/// Its height is stated rather than left to text plus padding, so a row of
/// chips is one known height.
pub fn subject_chip(parent: Option<SharedString>, label: SharedString, theme: BrowserTheme) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .h(gpui::rems(crate::SHELF_SUBJECT_CHIP_HEIGHT_REM))
        .flex()
        .flex_row()
        .items_center()
        .px(px(crate::CONTENT_INSET))
        .border_1()
        .border_color(theme.rule)
        .rounded(px(RADIUS_SM))
        .text_size(gpui::rems(SHELF_SUBJECT_SIZE))
        .text_color(theme.text)
        .children(parent.map(|parent| div().flex_none().text_color(theme.text_muted).child(format!("{parent} \u{203a} "))))
        .child(label)
}

/// The shelf under the heading, taking whatever the heading leaves.
pub fn shelf_carousel() -> Div {
    div().w_full().min_w_0().flex_1().min_h_0()
}

pub fn paginator_controls(previous: Option<AnyElement>, next: Option<AnyElement>) -> Div {
    div().flex_none().flex().flex_row().items_center().gap(px(CAROUSEL_CONTROL_GAP)).children(previous).children(next)
}

/// The author index: its paginated list, then the letter rail beside it.
pub fn author_index_body() -> Div {
    div().size_full().min_h_0().min_w_0().flex().flex_row().gap(px(crate::SPACE_MD)).p(px(crate::SPACE_MD))
}

/// The paginated list of names, taking whatever the rail leaves.
pub fn author_index_list() -> Div {
    div().flex_1().min_w_0().h_full()
}

/// The heading over one letter's names — or one book count's, when the index is
/// ordered by how many books a name has. Its padding and rule are what
/// `AUTHOR_INDEX_LETTER_FIXED_HEIGHT` reserves.
pub fn author_index_letter(letter: impl Into<SharedString>, count: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .size_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(px(SPACE_SM))
        .px(px(crate::CONTENT_INSET))
        .pt(px(crate::SPACE_MD))
        .pb(px(SPACE_XS))
        .border_b_1()
        .border_color(theme.rule)
        .child(div().flex_none().font_family(crate::DISPLAY_FONT_FAMILY).text_size(gpui::rems(crate::TEXT_LG)).line_height(gpui::relative(crate::SHELF_HEADING_LINE_HEIGHT_EM)).text_color(theme.text_accent).child(letter.into()))
        .child(div().min_w_0().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(count.into()))
}

/// One name in the index: the name and what it wrote, then a few of its covers.
///
/// Highlighted while the keyboard cursor is on it and while its books are open
/// beneath it, drawn like a focused book card — a tint, not an inversion.
pub fn author_index_entry(id: impl Into<ElementId>, highlighted: bool, theme: BrowserTheme) -> Stateful<Div> {
    let hovered = if highlighted { theme.accent_wash_hover } else { theme.hover };
    div()
        .id(id)
        .size_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(crate::SPACE_MD))
        .px(px(crate::CONTENT_INSET))
        .border_b_1()
        .border_color(theme.rule)
        .cursor_pointer()
        .when(highlighted, |entry| entry.bg(theme.accent_wash))
        .hover(move |style| style.bg(hovered))
}

/// The name over its detail line — how many books, and what they are about.
/// Both are elided rather than wrapped: an entry is one fixed height.
pub fn author_index_name(name: impl Into<SharedString>, detail: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SPACE_XS))
        .child(div().min_w_0().truncate().text_size(gpui::rems(crate::TEXT_MD)).line_height(gpui::relative(BOOK_CARD_TITLE_LINE_HEIGHT_EM)).child(name.into()))
        .child(div().min_w_0().truncate().text_size(gpui::rems(TEXT_XS)).line_height(gpui::relative(BOOK_CARD_TITLE_LINE_HEIGHT_EM)).text_color(theme.text_muted).child(detail.into()))
}

/// The covers at the end of an entry, and the count of any it leaves out.
pub fn author_index_covers() -> Div {
    div().flex_none().h_full().flex().flex_row().items_center()
}

/// One cover's slot. The card inside carries its own padding, so neighbouring
/// covers sit a card's padding apart without a gap of their own.
pub fn author_index_cover() -> Div {
    div().flex_none().h_full().w(gpui::rems(crate::AUTHOR_INDEX_COVER_SLOT_WIDTH_REM))
}

pub fn author_index_more(label: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().flex_none().pl(px(SPACE_XS)).text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(label.into())
}

/// The letters down the side of the index, one per heading, each a jump to it.
pub fn author_index_rail() -> Div {
    div().flex_none().h_full().flex().flex_col().overflow_hidden()
}

pub fn author_index_rail_letter(id: impl Into<ElementId>, letter: impl Into<SharedString>, theme: BrowserTheme) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(gpui::rems(1.5))
        .flex()
        .justify_center()
        .text_size(gpui::rems(TEXT_XS))
        .line_height(gpui::relative(1.5))
        .cursor_pointer()
        .hover(move |style| style.bg(theme.hover))
        .child(letter.into())
}

/// Breadcrumbs use small caps at the medium body size. Items own the vertical
/// padding so the trail stays aligned with the action buttons in the topbar.
///
/// The gap is zeroed against the toolkit's `gap_1p5`, which would otherwise add
/// 6px to the padding each crumb already carries — a chevron only 3.5 units
/// wide, floating in more air than it separates. Crumb padding is then the
/// single owner of the trail's rhythm: change it in [`folder_breadcrumb_item`]
/// and the label, its hit area and its distance from the separator all move
/// together.
pub fn folder_breadcrumb() -> Breadcrumb {
    Breadcrumb::new().text_size(gpui::rems(crate::TEXT_MD)).font_features(crate::small_caps()).gap_0()
}

/// Holds the trail in a topbar too narrow for it, and decides which end of the
/// trail survives that.
///
/// The current folder is what the trail is *for*, so it is the crumb that must
/// stay on screen; the root is the one the reader already knows. A left-anchored
/// row (which is what a horizontal scroll surface gives you, parked at offset 0)
/// keeps the wrong end and pushes the leaf out of sight, so overflow is taken
/// off the start instead: the row is clipped and justified to the end, and the
/// crumbs slide left under the topbar's edge as the path deepens.
///
/// The spacer is what keeps that from right-aligning a trail that fits. It
/// absorbs the free space while there is any, leaving justification nothing to
/// do; once the trail overflows it collapses and the end-justification takes
/// over. The trail itself never shrinks — crumbs are elided by character count
/// in [`folder_breadcrumb_item`], not squeezed.
pub fn folder_breadcrumb_trail(breadcrumb: Breadcrumb) -> Div {
    div().w_full().min_w_0().overflow_x_hidden().flex().items_center().justify_end().child(breadcrumb.flex_none()).child(div().flex_1().min_w_0())
}

/// The trail recedes and the current folder does not: ancestors are muted, the
/// last crumb keeps full ink. Weight used to carry this, but the interface has
/// no semibold face — see the type scale in `tokens.rs`.
///
/// The horizontal padding is `SPACE_XS` rather than the `CONTENT_INSET` the
/// rest of the browser is inset by, and it is the only thing between a label
/// and its chevron — see [`folder_breadcrumb`], which zeroes the toolkit's gap.
/// A chevron is a separator, not a neighbour: it belongs to the pair of crumbs
/// around it, and at the inset it reads as an item of its own.
pub fn folder_breadcrumb_item(label: impl Into<SharedString>, current: bool, theme: BrowserTheme) -> BreadcrumbItem {
    let label = label.into();
    let max_characters = 24;
    let mut characters = label.chars();
    let visible = characters.by_ref().take(max_characters).collect::<String>();
    let visible = if characters.next().is_some() { format!("{}…", visible.chars().take(max_characters.saturating_sub(1)).collect::<String>()) } else { visible };
    let item = BreadcrumbItem::new(visible).px(px(SPACE_XS)).py(px(SPACE_XS)).text_color(if current { theme.text } else { theme.text_muted });
    if current { item.font_family(crate::DISPLAY_FONT_FAMILY) } else { item }
}

/// The first crumb, set on the left edge of the covers below it.
///
/// Nothing above it carries a horizontal inset — the topbar and the column it
/// sits in are both unpadded — so this padding is the whole of the distance
/// between the rail and the trail, and `CONTENT_INSET` is where a book cover
/// starts: the card grid is flush with the column and the cover sits one
/// `BOOK_CARD_PADDING` inside its card. The two are the same token for exactly
/// this reason. Only the leading edge takes it; the trailing edge keeps the
/// tighter padding the chevrons are spaced on.
pub fn folder_breadcrumb_root_item(label: impl Into<SharedString>, current: bool, theme: BrowserTheme) -> BreadcrumbItem {
    folder_breadcrumb_item(label, current, theme).pl(px(crate::CONTENT_INSET))
}

/// Natural chip width, including the action slot only when it is shown.
/// Use the same text and icon layout as the rendered chip so font changes
/// affect wrapping without estimating label widths from character counts.
pub fn compact_hierarchy_item_width(icon: crate::NavigationIcon, label: SharedString, count: Option<usize>, show_download: bool, window: &mut gpui::Window, cx: &mut gpui::App) -> gpui::Pixels {
    let mut content = div()
        .flex()
        .items_center()
        .whitespace_nowrap()
        .text_size(gpui::rems(crate::COMPACT_HIERARCHY_ITEM_TEXT_SIZE_REM))
        .gap(px(crate::COMPACT_HIERARCHY_ITEM_CONTENT_GAP))
        .child(crate::navigation_icon(icon))
        .child(div().flex_none().child(label))
        .children(count.map(|count| div().flex_none().child(count.to_string())))
        .when(show_download, |content| content.child(div().w(gpui::rems(crate::DOWNLOAD_ACTION_SIZE_REM)).flex_none()))
        .into_any_element();
    let measured = content.layout_as_root(gpui::size(gpui::AvailableSpace::MaxContent, gpui::AvailableSpace::MaxContent), window, cx);
    measured.width + px(crate::COMPACT_HIERARCHY_ITEM_HORIZONTAL_PADDING * 2.0 + 2.0)
}

/// A compact hierarchy item for mixed paginator pages.
///
/// This function owns presentation only. Feature code remains responsible for
/// attaching click handlers and navigation behavior.
pub fn compact_hierarchy_item(
    id: impl Into<ElementId>, icon: crate::NavigationIcon, label: impl Into<SharedString>, path: Option<SharedString>, count: Option<usize>, download: Option<AnyElement>, focused: bool, marquee: bool, theme: BrowserTheme,
) -> Button {
    let label = label.into();
    let text =
        div().relative().flex_1().min_w_0().h_full().child(div().size_full().min_w_0().flex().items_center().child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))).children(path.map(|path| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(gpui::relative(0.5))
                .min_w_0()
                .flex()
                .items_center()
                .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_start().text_size(gpui::rems(0.5)).text_color(theme.text_muted).child(path))
        }));
    // A focused chip tints to `accent_wash` and keeps every foreground it had.
    //
    // `selected(true)` is what makes the hover reachable at all: a `Button`
    // writes its own hover slot unless it is selected, and gpui allows that slot
    // to be written once. Claiming it is the only way to say that a tinted chip
    // lifts rather than darkens, and the only way for `marquee` to say nothing
    // at all. See `selectable_button`, which takes the same state for the same
    // reason.
    //
    // Hover stays on the shared sidebar/list tone so chips and navigation rows
    // read as the same pointer state.
    let hovered = theme.hover;
    let base = if focused { theme.accent_wash } else { theme.page_bg };
    let button = crate::base_button(id).ghost().selected(true).bg(base).hover(move |style| if marquee { style.bg(base) } else { style.bg(hovered) });
    button.w_full().max_w_full().h_full().p_0().px(px(crate::COMPACT_HIERARCHY_ITEM_HORIZONTAL_PADDING)).rounded(px(0.0)).border_1().border_color(theme.rule).child(
        div()
            .size_full()
            .min_w_0()
            .text_size(gpui::rems(crate::COMPACT_HIERARCHY_ITEM_TEXT_SIZE_REM))
            .flex()
            .items_center()
            .gap(px(crate::COMPACT_HIERARCHY_ITEM_CONTENT_GAP))
            .child(crate::navigation_icon(icon).text_color(theme.text_accent))
            .child(text)
            .children(count.map(|count| div().flex_none().text_color(theme.text_muted).child(count.to_string())))
            .when_some(download, |content, download| content.child(div().size(gpui::rems(crate::DOWNLOAD_ACTION_SIZE_REM)).flex_none().flex().items_center().justify_center().child(download))),
    )
}

/// A section card's raised tile, with its own hairline edge.
pub fn section_card_frame(theme: BrowserTheme) -> Div {
    div()
        .absolute()
        .inset_0()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SPACE_SM))
        .p(px(crate::SPACE_MD))
        .bg(theme.raised_bg)
        .border_1()
        .border_color(theme.rule)
}

/// Full-width colour behind a section card's existing heading. Its negative
/// margins reach the card edge. Equal vertical padding centers the heading
/// in the band, while the contents below keep their position.
pub fn section_card_band(index: usize, heading: impl IntoElement) -> Div {
    let (from, to) = section_card_band_colors(index);
    div()
        .flex_none()
        .min_w_0()
        .mx(px(-crate::SPACE_MD))
        .mt(px(-crate::SPACE_MD))
        .py(px(crate::SPACE_MD / 2.0))
        .px(px(crate::SPACE_MD))
        .bg(gpui::linear_gradient(120.0, gpui::linear_color_stop(from, 0.0), gpui::linear_color_stop(to, 1.0)))
        .child(heading)
}

/// A section card's title, total and paging controls on a shared colour band.
/// The existing heading layout and pointer behavior stay the same.
///
/// Hover underlines carry an id on the text itself: the underline is shaped
/// with the text at layout, when only an element with its own state can know
/// that its group is hovered.
pub fn section_card_heading(id: impl Into<ElementId>, title: impl Into<SharedString>, count: usize, pager: AnyElement, focused: bool, marquee: bool) -> Stateful<Div> {
    div()
        .id(id)
        .group("section-card-heading")
        .flex_none()
        .h(gpui::rems(crate::SECTION_CARD_HEADING_HEIGHT_REM))
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(SPACE_SM))
        .text_color(gpui::rgb(0xf2efe8))
        .cursor_pointer()
        .child(
            div()
                .id("title")
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_family(crate::DISPLAY_FONT_FAMILY)
                .text_size(gpui::rems(crate::TEXT_LG))
                .when(focused, |title| title.text_color(gpui::white()))
                .when(!marquee, |title| title.group_hover("section-card-heading", |style| style.underline()))
                .child(title.into()),
        )
        .child(div().flex_none().child(count.to_string()))
        .child(pager)
}

/// One child in a section card's list, with its navigation icon and a quiet
/// count pill. The whole row is the hover and click target.
pub fn section_card_subject(id: impl Into<ElementId>, icon: crate::NavigationIcon, label: impl Into<SharedString>, count: usize, focused: bool, theme: BrowserTheme) -> Stateful<Div> {
    div()
        .id(id)
        .group("section-card-subject")
        .h(gpui::rems(crate::SECTION_CARD_SUBJECT_ROW_HEIGHT_REM))
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(SPACE_SM))
        .mx(px(-SPACE_XS))
        .px(px(SPACE_XS))
        .cursor_pointer()
        .text_size(gpui::rems(TEXT_SM))
        .hover(move |style| style.bg(theme.hover))
        .child(crate::navigation_icon(icon).size(gpui::rems(0.8)).text_color(theme.text_accent))
        .child(div().id("label").flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().when(focused, |label| label.text_color(theme.text_accent)).child(label.into()))
        .child(div().flex_none().min_w(gpui::rems(1.6)).px(px(SPACE_XS)).text_center().text_size(gpui::rems(0.8)).text_color(theme.text_muted).bg(theme.hover).child(count.to_string()))
}

/// A section card's contents, side by side: its child entries, then its covers.
pub fn section_card_body() -> Div {
    div().flex_1().min_h_0().min_w_0().flex().gap(px(crate::SECTION_CARD_COLUMN_GAP))
}

/// A section card's child entries fill one paginator cell.
pub fn section_card_subject_rows(rows: Vec<AnyElement>) -> Div {
    div()
        .flex_none()
        .h_full()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .children(rows)
}

/// Paging control for a mixed strip of entry columns and covers, in its heading.
pub fn section_card_cover_arrow(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: IconName, enabled: bool) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .size(gpui::rems(1.5))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |arrow| arrow.cursor_pointer())
        .text_color(gpui::rgb(0xf2efe8))
        .bg(gpui::white().opacity(0.12))
        .border_1()
        .border_color(gpui::white().opacity(0.28))
        .when(enabled, |arrow| arrow.hover(|style| style.bg(gpui::white().opacity(0.22))))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
        .child(Icon::new(icon).size(px(12.0)))
}

/// Marks the place a drag will land: an accent outline and a faint wash,
/// shown while a drag of `T`, or of files from the desktop, is over it and
/// `lands_here` says so. It is laid over the target's own content, so it shows
/// on a button as well as on a plain row, and it takes no clicks of its own.
/// The target it sits in has to be `relative`.
pub fn drop_highlight<T: 'static>(lands_here: impl Fn(&gpui::App) -> bool + 'static, theme: BrowserTheme) -> Div {
    let accent = theme.accent;
    let wash = theme.accent.opacity(0.08);
    let lands_here = std::rc::Rc::new(lands_here);
    let lands_here_files = lands_here.clone();
    div()
        .absolute()
        .inset_0()
        .drag_over::<T>(move |style, _, _, cx| if lands_here(cx) { style.border_2().border_color(accent).bg(wash) } else { style })
        .drag_over::<gpui::ExternalPaths>(move |style, _, _, cx| if lands_here_files(cx) { style.border_2().border_color(accent).bg(wash) } else { style })
}
