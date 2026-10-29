//! One contents list, for every surface that shows one.
//!
//! A book's navigation is drawn in three places — the reader's sidebar, the
//! audiobook player's chapter table, and the library's detail page — and each
//! grew its own list: a tree widget, a `uniform_list` with two amount columns,
//! and nothing at all. The rows meant the same thing and looked different, and
//! a fix to one never reached the others.
//!
//! What varies between them is data, not presentation: an entry has children or
//! it has not, a number or not, an amount or not, and one row is where the
//! reader currently is. So the row takes those as fields and the differences
//! stop being different components.
//!
//! Interaction stays with the caller. A row hands back two hit areas because
//! they do different things — the disclosure opens the entry, the title goes to
//! it — and which of those means "seek the running audiobook" rather than "open
//! the book" is a decision only the surface can make.

use gpui::prelude::*;
use gpui::{Div, ElementId, MouseButton, SharedString, Stateful, div, px};
use crate::Button;
use gpui_component::{Icon, IconName};
use std::rc::Rc;

use crate::{BrowserTheme, CONTENTS_AMOUNT_WIDTH_REM, CONTENTS_DIGIT_WIDTH_EM, CONTENTS_DISCLOSURE_SIZE_REM, CONTENTS_ROW_GAP, CONTENTS_ROW_INDENT, CONTENTS_ROW_MIN_HEIGHT_REM, SPACE_SM, SPACE_XS, TEXT_SM, TEXT_XS};

pub type ContentsReadReset = Rc<dyn Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App)>;

pub fn contents_read_checkmark(id: impl Into<ElementId>, on_reset: ContentsReadReset) -> Button {
    crate::base_button(id)
        .ghost()
        .icon(Icon::new(IconName::Check).size(gpui::rems(TEXT_XS)))
        .tooltip("Mark chapter as unread")
        .debug_selector(|| "chapter-read-checkmark".into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_reset(event, window, cx);
        })
}

/// What one row of a contents list is.
///
/// Every field beyond the title answers a question the surface's data either
/// has or has not: an audiobook chapter knows how long it is, a nav document's
/// entry knows its children, a flat ordered list knows its number.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContentsRow {
    /// Nesting, from zero. Indent is capped so that a deep entry stays legible
    /// rather than disappearing off the right of a narrow sidebar.
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
    /// The entry the reader is inside. Exactly one row in a list has this, and
    /// its ancestors do not: a navigation document never says that a parent is
    /// partly read.
    pub marked: bool,
    /// A completed leaf, or a parent whose children are all complete.
    pub read: bool,
    /// The keyboard cursor, which is not the same thing as where the reader is.
    pub focused: bool,
    /// The entry's place in reading order. Every entry is counted — parents,
    /// and entries hidden inside closed parents — so a number never changes
    /// when a part is opened or closed.
    pub number: Option<usize>,
    /// Digits in the list's largest number, so every row's number column is
    /// the same width and every title starts on one line.
    pub number_digits: usize,
}

/// An open contents list, beginning with a quiet rule. Rows draw their own
/// hairlines so the same treatment works in detail views and reader sidebars.
pub fn contents_panel(theme: BrowserTheme) -> Div {
    div().w_full().min_w_0().flex().flex_col().border_t_1().border_color(theme.rule)
}

/// The title, as the row's own clickable region.
///
/// Whoever owns the row may already use a click for something else — the
/// reader's sidebar is inside a tree that toggles an entry when its row is
/// clicked — so the title takes the mouse down and stops it, and navigation
/// stays separable from expansion. A surface with nothing else on the row can
/// pass a bare string instead. Reader titles wrap to two lines at most.
pub fn contents_title(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Stateful<Div> {
    div().id(id).flex_1().min_w_0().whitespace_normal().line_height_auto().line_clamp(2).text_ellipsis().cursor_pointer().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(title.into())
}

/// The disclosure, for a surface whose row already opens the entry: drawn, and
/// transparent to the click that opens it.
///
/// The reader's tree toggles on mouse down over the whole row, so the chevron
/// there holds nothing of its own and lets the event through to the row behind
/// it. A chevron that swallowed the event without handling it would be the one
/// spot in the list that did nothing at all.
///
/// Rows without children keep the empty column so that every title in the list
/// starts on the same line — a ragged left edge reads as damage.
/// Callers can attach their own toggle handler when `row.has_children` is true.
///
/// The button is as wide as a one-line row is tall, and always the row's full
/// height: it stretches, and negative margins give back the row's padding on
/// three sides, so a wrapped title's taller row gets a taller button while
/// every title still starts on one line. On the current row the hover is
/// `accent_text` at a low alpha, since `hover` is the accent itself and would
/// vanish over it.
pub fn contents_disclosure(id: impl Into<ElementId>, row: ContentsRow, theme: BrowserTheme) -> Stateful<Div> {
    div().id(id).w(gpui::rems(CONTENTS_ROW_MIN_HEIGHT_REM)).self_stretch().my(px(-ROW_PADDING_Y)).ml(px(-SPACE_SM)).flex_none().when(row.has_children, |disclosure| {
        let hovered = if row.marked { theme.accent_text.opacity(0.2) } else { theme.hover };
        disclosure
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_color(if row.marked { theme.accent_text } else { theme.text })
            .hover(move |style| style.bg(hovered))
            .child(Icon::new(if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight }).size(gpui::rems(CONTENTS_DISCLOSURE_SIZE_REM)))
    })
}

/// One row, laid out and stated. The caller supplies the disclosure and hangs
/// its own click on the returned row.
///
/// A row reads caret, number, title, amount. The caret column is there only
/// in a list that has parents — `disclosure` is `None` in a flat one, where it
/// would be a column of nothing — and the number column only when the row has
/// a number.
///
/// The current entry — where the reader is — is the solid accent, the keyboard
/// cursor is `accent_wash` and the pointer is the light `hover`, as the ladder
/// in `tokens.rs` sets out. The cursor on the current entry steps it to
/// `accent_hover`. Finished entries use muted ink and a checkmark.
pub fn contents_row(id: impl Into<ElementId>, row: ContentsRow, disclosure: Option<impl IntoElement>, title: impl IntoElement, amount: Option<SharedString>, reset: Option<ContentsReadReset>, theme: BrowserTheme) -> Stateful<Div> {
    let (background, hovered, ink) = match (row.marked, row.focused) {
        (true, true) => (Some(theme.accent_hover), theme.accent_hover, theme.accent_text),
        (true, false) => (Some(theme.accent), theme.accent_hover, theme.accent_text),
        (false, true) => (Some(theme.accent_wash), theme.accent_wash_hover, if row.read { theme.text_muted } else { theme.text }),
        (false, false) => (None, theme.hover, if row.read { theme.text_muted } else { theme.text }),
    };
    let quiet = if row.marked { theme.accent_text } else { theme.text_muted };
    let depth = row.depth.min(MAXIMUM_DRAWN_DEPTH) as f32;
    let number_width = contents_number_width(row.number_digits);
    // A numbered list steps in one level by exactly the number column and its
    // gap, so a child's number stands under its parent's title. The column is
    // in rems and the gaps in pixels, so the step is a spacer of the one plus
    // padding of the other; the spacer brings its own row gap, taken back out.
    let (indent, spacer) = match row.number {
        Some(_) if depth > 0.0 => (SPACE_SM + depth * (SPACE_XS + CONTENTS_ROW_GAP) - CONTENTS_ROW_GAP, Some(div().flex_none().w(gpui::rems(depth * number_width)))),
        Some(_) => (SPACE_SM, None),
        None => (SPACE_SM + CONTENTS_ROW_INDENT * depth, None),
    };
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(CONTENTS_ROW_GAP))
        .min_h(gpui::rems(CONTENTS_ROW_MIN_HEIGHT_REM))
        .py(px(ROW_PADDING_Y))
        .px(px(SPACE_SM))
        .pl(px(indent))
        .cursor_pointer()
        .text_size(gpui::rems(TEXT_SM))
        .text_color(ink)
        .border_b_1()
        .border_color(theme.rule.opacity(0.45))
        .when_some(background, |element, colour| element.bg(colour))
        .hover(move |style| style.bg(hovered))
        .children(spacer)
        .children(disclosure)
        .children(row.number.map(|number| contents_number(number, number_width, quiet)))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
        .children(
            amount.map(|amount| div().debug_selector(|| "contents-row-amount".into()).w(gpui::rems(CONTENTS_AMOUNT_WIDTH_REM)).flex_none().whitespace_nowrap().text_right().text_size(gpui::rems(TEXT_XS)).text_color(quiet).child(amount)),
        )
        .when(row.read, |row| match reset {
            Some(reset) => row.child(contents_read_checkmark("chapter-read-checkmark", reset).text_color(quiet)),
            None => row.child(Icon::new(IconName::Check).size(gpui::rems(TEXT_XS)).text_color(quiet)),
        })
}

/// The number column's width in rems: as many figures as the list's largest
/// number has, at the number's own size.
fn contents_number_width(digits: usize) -> f32 {
    digits.max(1) as f32 * CONTENTS_DIGIT_WIDTH_EM * TEXT_XS
}

/// An entry's number, left-aligned in its column, which keeps a little air
/// before the title.
fn contents_number(number: usize, width: f32, colour: gpui::Hsla) -> Div {
    div().debug_selector(|| "contents-row-number".into()).flex_none().w(gpui::rems(width)).whitespace_nowrap().mr(px(SPACE_XS)).text_size(gpui::rems(TEXT_XS)).text_color(colour).child(number.to_string())
}

/// How many figures a number has, for [`ContentsRow::number_digits`].
pub fn contents_digits(largest: usize) -> usize {
    largest.max(1).ilog10() as usize + 1
}

/// Below this the indent stops growing. A fourth level is still drawn and still
/// navigable; it simply stops marching rightwards in a column that may be a
/// sidebar's width.
const MAXIMUM_DRAWN_DEPTH: usize = 3;

/// Above and below a row's title, the same whether it holds one line or two.
/// Owned here rather than by each surface because the disclosure's margins
/// cancel exactly this, to run the row's full height.
const ROW_PADDING_Y: f32 = SPACE_SM;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, Window};

    struct NumberedContents;

    impl Render for NumberedContents {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let row = ContentsRow { number: Some(1000), number_digits: 4, read: true, ..Default::default() };
            div().w(px(260.0)).child(contents_row("chapter", row, None::<Div>, "Chapter title", Some("123 min".into()), Some(Rc::new(|_, _, _| {})), BrowserTheme::default()))
        }
    }

    #[gpui::test]
    fn completed_contents_keep_large_numbers_on_one_line_and_checkmark_last(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, _| NumberedContents);
        cx.run_until_parked();
        let number = cx.debug_bounds("contents-row-number").unwrap();
        let amount = cx.debug_bounds("contents-row-amount").unwrap();
        let check = cx.debug_bounds("chapter-read-checkmark").unwrap();
        assert!(number.size.height <= px(24.0), "a four-digit number must remain one line");
        assert!(amount.size.height <= px(24.0), "a three-digit duration must remain one line");
        assert!(check.left() >= amount.right(), "the completion checkmark follows the duration at the far right");
    }
}
