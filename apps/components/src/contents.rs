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

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Div, ElementId, MouseButton, SharedString, Stateful, Window, div, px};
use gpui_component::{Icon, IconName};

use crate::{BrowserTheme, CONTENTS_AMOUNT_WIDTH_REM, CONTENTS_DISCLOSURE_SIZE_REM, CONTENTS_ROW_GAP, CONTENTS_ROW_INDENT, SPACE_SM, SPACE_XS, TEXT_SM, TEXT_XS};

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
    /// A leaf entry before the current position in reading order. Parent
    /// entries stay neutral because they can contain both read and unread text.
    pub read: bool,
    /// The keyboard cursor, which is not the same thing as where the reader is.
    pub focused: bool,
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
pub fn contents_title(id: impl Into<ElementId>, title: impl Into<SharedString>, theme: BrowserTheme) -> Stateful<Div> {
    let _ = theme;
    div().id(id).flex_1().min_w_0().whitespace_normal().line_height(gpui::relative(1.2)).line_clamp(2).text_ellipsis().cursor_pointer().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(title.into())
}

/// The disclosure, for a surface whose row already opens the entry: drawn, and
/// transparent to the click that opens it.
///
/// The reader's tree toggles on mouse down over the whole row, so the chevron
/// there holds nothing of its own and lets the event through to the row behind
/// it. A chevron that swallowed the event without handling it would be the one
/// spot in the list that did nothing at all.
pub fn contents_disclosure(id: impl Into<ElementId>, row: ContentsRow, theme: BrowserTheme) -> AnyElement {
    disclosure(id, row, theme, None)
}

/// The disclosure with the opening of the entry as its own, for a list whose row
/// means something else — the book's page, where the row goes to the entry.
///
/// The toggle arrives here rather than being wrapped around the outside: keeping
/// the click off the row means swallowing the mouse down, and a listener on an
/// ancestor never sees an event a descendant has consumed. Both belong to this
/// element, whose own click is registered after the swallow and so still runs.
pub fn contents_toggle_disclosure(id: impl Into<ElementId>, row: ContentsRow, theme: BrowserTheme, toggle: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    disclosure(id, row, theme, Some(Rc::new(toggle)))
}

/// Rows without children keep the empty column so that every title in the list
/// starts on the same line — a ragged left edge reads as damage.
fn disclosure(id: impl Into<ElementId>, row: ContentsRow, theme: BrowserTheme, toggle: Option<Rc<dyn Fn(&mut Window, &mut App)>>) -> AnyElement {
    let size = gpui::rems(CONTENTS_DISCLOSURE_SIZE_REM);
    if !row.has_children {
        return div().w(size).h(size).flex_none().into_any_element();
    }
    div()
        .id(id)
        .w(size)
        .h(size)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if row.marked { theme.accent_text } else { theme.text_muted })
        .hover(move |style| style.bg(theme.hover))
        .when_some(toggle, |element, toggle| {
            // The title beside it navigates; opening an entry must not also go to it.
            element.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, window, cx| toggle(window, cx))
        })
        .child(Icon::new(if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight }).size(gpui::rems(CONTENTS_DISCLOSURE_SIZE_REM * 0.7)))
        .into_any_element()
}

/// One row, laid out and stated. The caller supplies the disclosure and hangs
/// its own click on the returned row.
///
/// Hover is the neutral overlay, the keyboard cursor is `accent_wash`, and
/// the current entry is solid accent. Earlier leaf entries use muted ink.
pub fn contents_row(id: impl Into<ElementId>, row: ContentsRow, disclosure: AnyElement, title: impl IntoElement, number: Option<usize>, amount: Option<SharedString>, theme: BrowserTheme) -> Stateful<Div> {
    let (background, hovered, ink) = match (row.marked, row.focused) {
        (true, _) => (Some(theme.accent), theme.accent_hover, theme.accent_text),
        (false, true) => (Some(theme.accent_wash), theme.accent_wash_hover, if row.read { theme.text_muted } else { theme.text }),
        (false, false) => (None, theme.hover, if row.read { theme.text_muted } else { theme.text }),
    };
    let quiet = if row.marked { theme.accent_wash } else { theme.text_muted };
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(CONTENTS_ROW_GAP))
        .min_h(gpui::rems(2.25))
        .py(px(SPACE_XS))
        .px(px(SPACE_SM))
        .pl(px(SPACE_SM + CONTENTS_ROW_INDENT * row.depth.min(MAXIMUM_DRAWN_DEPTH) as f32))
        .cursor_pointer()
        .text_size(gpui::rems(TEXT_SM))
        .text_color(ink)
        .border_b_1()
        .border_color(theme.rule.opacity(0.45))
        .when_some(background, |element, colour| element.bg(colour))
        .hover(move |style| style.bg(hovered))
        .child(disclosure)
        .children(number.map(|number| div().flex_none().min_w(gpui::rems(1.6)).text_right().text_size(gpui::rems(TEXT_XS)).text_color(quiet).child(number.to_string())))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
        .children(amount.map(|amount| div().w(gpui::rems(CONTENTS_AMOUNT_WIDTH_REM)).flex_none().text_right().text_size(gpui::rems(TEXT_XS)).text_color(quiet).child(amount)))
}

/// Below this the indent stops growing. A fourth level is still drawn and still
/// navigable; it simply stops marching rightwards in a column that may be a
/// sidebar's width.
const MAXIMUM_DRAWN_DEPTH: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indent_stops_growing_but_the_entry_is_never_dropped() {
        let indent = |depth: usize| SPACE_SM + CONTENTS_ROW_INDENT * depth.min(MAXIMUM_DRAWN_DEPTH) as f32;

        assert!(indent(1) > indent(0), "a child steps in from its parent");
        assert!(indent(3) > indent(2));
        assert_eq!(indent(4), indent(3), "and a fourth level stops stepping rather than vanishing rightwards");
        assert_eq!(indent(9), indent(3));
    }
}
