//! The shape of one book's page.
//!
//! Compact pages keep the cover beside the title. Desktop pages place book
//! details in a sidebar; contents and description sit beside it when space
//! allows, or stack in one reading column.
//!
//! These functions style and lay out. What a button does, which entry the reader
//! is inside, and whether the file is on the device are the page's business.

use gpui::prelude::*;
use gpui::{Div, SharedString, div, px, rems};

use crate::{
    BOOK_COVER_WIDTH, BOOK_DETAIL_CONTENTS_WIDTH_REM, BOOK_DETAIL_COVER_FRACTION, BOOK_DETAIL_DESCRIPTION_LINE_HEIGHT_EM, BOOK_DETAIL_DESCRIPTION_SIZE, BOOK_DETAIL_DESCRIPTION_WIDTH_REM,
    BOOK_DETAIL_HERO_GAP, BOOK_DETAIL_PAGE_WIDTH_REM, BOOK_DETAIL_SECTION_GAP, BOOK_DETAIL_SIDEBAR_WIDTH_REM, BOOK_DETAIL_WIDE_PAGE_WIDTH_REM, BrowserTheme, SPACE_LG, SPACE_SM, SPACE_XS,
    TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XS, TITLE_LG, TITLE_MD,
};

/// The page centres whichever responsive measure the caller selects.
///
/// The caller makes it scroll — `overflow_y_scrollbar` wraps an element that
/// already has its children, so it cannot be applied from in here.
pub fn book_detail_page() -> Div {
    div().size_full().min_h_0().min_w_0().flex().flex_col().items_center().p(px(SPACE_LG))
}

/// The compact page's single measure.
pub fn book_detail_measure() -> Div {
    div().w_full().max_w(rems(BOOK_DETAIL_PAGE_WIDTH_REM)).min_w_0().flex().flex_col().gap(px(BOOK_DETAIL_SECTION_GAP))
}

/// The three-column desktop measure.
pub fn book_detail_wide_measure() -> Div {
    div().w_full().max_w(rems(BOOK_DETAIL_WIDE_PAGE_WIDTH_REM)).min_w_0().flex().flex_col()
}

/// At medium widths the sidebar stays beside one reading column.
pub fn book_detail_medium_measure() -> Div {
    div().w_full().max_w(rems(BOOK_DETAIL_SIDEBAR_WIDTH_REM + BOOK_DETAIL_DESCRIPTION_WIDTH_REM + 2.0)).min_w_0().flex().flex_col()
}

pub fn book_detail_sidebar() -> Div {
    div().w(rems(BOOK_DETAIL_SIDEBAR_WIDTH_REM)).flex_none().min_w_0().flex().flex_col().gap(px(SPACE_LG))
}

pub fn book_detail_sidebar_cover_slot() -> Div {
    div().relative().w(px(180.0)).max_w_full().flex_none()
}

pub fn book_detail_contents_column() -> Div {
    div().flex_1().min_w_0().max_w(rems(BOOK_DETAIL_CONTENTS_WIDTH_REM)).flex().flex_col().gap(px(BOOK_DETAIL_SECTION_GAP))
}

pub fn book_detail_description_column() -> Div {
    div().flex_1().min_w_0().max_w(rems(BOOK_DETAIL_DESCRIPTION_WIDTH_REM)).flex().flex_col().gap(px(BOOK_DETAIL_SECTION_GAP))
}

/// The cover and the words beside it. The two stand side by side at every width
/// — a cover above its own title was tried and rejected, because the title is
/// what the page is about and the cover is what identifies it.
pub fn book_detail_hero() -> Div {
    div().w_full().min_w_0().flex().flex_row().items_start().gap(px(BOOK_DETAIL_HERO_GAP))
}

/// The cover takes half the hero, capped at the width a cover is drawn at
/// everywhere else — so a narrow window shrinks it rather than pushing the title
/// into a column one word wide, and a wide one does not inflate it.
///
/// Positioned, because the reading position hangs under it the way it does
/// under a card in the grid.
pub fn book_detail_cover_slot() -> Div {
    div().relative().flex_none().w(gpui::relative(BOOK_DETAIL_COVER_FRACTION)).max_w(px(BOOK_COVER_WIDTH))
}

pub fn book_detail_headline() -> Div {
    div().flex_1().min_w_0().flex().flex_col().gap(px(SPACE_SM))
}

pub fn book_detail_title(title: impl Into<SharedString>, subtitle: Option<&str>, large: bool, theme: BrowserTheme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_XS))
        .child(div().font_family(crate::DISPLAY_FONT_FAMILY).text_size(rems(if large { TITLE_LG } else { TITLE_MD })).text_color(theme.text).child(title.into()))
        .children(subtitle.filter(|subtitle| !subtitle.trim().is_empty()).map(|subtitle| div().text_size(rems(TEXT_MD)).text_color(theme.text_muted).child(subtitle.to_owned())))
}

/// One credited line: "Narrated by", "Translated by", the authors themselves.
/// The role is stated rather than implied by position, because a page that lists
/// four names in a column says nothing about which of them wrote it.
pub fn book_detail_credit_line(role: Option<impl Into<SharedString>>, theme: BrowserTheme) -> Div {
    div().flex().flex_row().flex_wrap().items_baseline().gap(px(SPACE_XS)).text_size(rems(TEXT_SM)).text_color(theme.text_muted).children(role.map(|role| div().flex_none().child(role.into())))
}

/// A name that goes somewhere — an author's own page.
pub fn book_detail_name_link(id: impl Into<gpui::ElementId>, name: impl Into<SharedString>, theme: BrowserTheme) -> gpui::Stateful<Div> {
    div().id(id).cursor_pointer().text_color(theme.text_accent).hover(move |style| style.underline()).child(name.into())
}

/// The facts a reader checks rather than reads — how long an audiobook runs,
/// how many chapters it has. Separated by a middot on one line, because each is
/// two words long and a list of them down the page would take more room than the
/// cover beside it.
pub fn book_detail_facts(facts: impl IntoIterator<Item = SharedString>, theme: BrowserTheme) -> Div {
    let mut row = div().flex().flex_row().flex_wrap().items_baseline().gap(px(SPACE_SM)).text_size(rems(TEXT_XS)).text_color(theme.text_muted);
    for (index, fact) in facts.into_iter().enumerate() {
        if index > 0 {
            row = row.child(div().flex_none().text_color(theme.rule).child("·"));
        }
        row = row.child(div().flex_none().child(fact));
    }
    row
}

pub fn book_detail_subjects() -> Div {
    div().flex().flex_col().items_start().gap(px(SPACE_XS))
}

/// Where the reader is, in the book's own words rather than as a percentage:
/// "Against the Tide?" is a place, "34%" is a number about one. The percentage
/// stays beside it for the books whose navigation says nothing.
///
/// Said, not drawn. How far in they are is the bar under the cover, the same
/// one and in the same place as on a card in the grid — a book is one thing
/// wherever it is seen. Drawn here as well it ran the width of the hero, which
/// at nought per cent is a rule under the byline saying nothing.
pub fn book_detail_position(entry: Option<SharedString>, detail: Option<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_row()
        .items_baseline()
        .justify_between()
        .gap(px(SPACE_SM))
        .text_size(rems(TEXT_XS))
        .text_color(theme.text_muted)
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(entry.unwrap_or_else(|| "Not started".into())))
        .children(detail.map(|detail| div().flex_none().child(detail)))
}

pub fn book_detail_actions() -> Div {
    div().flex().flex_row().flex_wrap().items_center().gap(px(SPACE_SM))
}

/// The compact page's sections, in reading order.
pub fn book_detail_sections() -> Div {
    div().w_full().min_w_0().flex().flex_col().gap(px(SPACE_LG))
}

/// Desktop columns shrink with the window while retaining readable limits.
pub fn book_detail_columns() -> Div {
    div().w_full().min_w_0().flex().flex_row().items_start().gap(px(BOOK_DETAIL_SECTION_GAP))
}

pub fn book_detail_section() -> Div {
    div().w_full().min_w_0().flex().flex_col().gap(px(SPACE_SM))
}

pub fn book_detail_heading(heading: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().font_family(crate::DISPLAY_FONT_FAMILY).text_size(rems(TEXT_LG)).text_color(theme.text).child(heading.into())
}

/// The blurb, read as the markup it is. Publishers write descriptions in HTML
/// — `<p>`, `<i>` around titles, entities — and Standard Ebooks' long
/// descriptions are nothing but. Set as a raw string those tags are what the
/// reader sees, which is what [`crate::description::rich_text`] exists to
/// prevent; the card's popover has always gone through it.
///
/// `None` where the markup carries no words, so a page is not given a heading
/// over nothing.
pub fn book_detail_description(description: &str, theme: BrowserTheme) -> Option<Div> {
    let (text, highlights) = crate::description::rich_text(description);
    if text.is_empty() {
        return None;
    }
    Some(
        div()
            .w_full()
            .min_w_0()
            .text_size(rems(BOOK_DETAIL_DESCRIPTION_SIZE))
            .line_height(rems(BOOK_DETAIL_DESCRIPTION_SIZE * BOOK_DETAIL_DESCRIPTION_LINE_HEIGHT_EM))
            .text_color(theme.text)
            .child(gpui::StyledText::new(text).with_highlights(highlights)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_never_takes_more_than_half_the_hero() {
        assert!(BOOK_DETAIL_COVER_FRACTION <= 0.5);
        assert!(BOOK_COVER_WIDTH / (BOOK_DETAIL_PAGE_WIDTH_REM * 16.0) < BOOK_DETAIL_COVER_FRACTION, "at the page's full measure the cover is at its natural width, not the cap");
    }

}
