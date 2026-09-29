use gpui::prelude::*;
use gpui::{AnyElement, App, Div, IntoElement, SharedString, Window, div, px};
use gpui_component::Sizable;
use gpui_component::Size;
use gpui_component::Theme;
use gpui_component::alert::Alert;

use crate::{BrowserTheme, SPACE_SM, SPACE_XS, Scrollable, ScrollableElement, TEXT_LG, TEXT_XS, TITLE_MD};

/// A full-size vertical flex boundary for page and viewport content.
///
/// Fills the parent's layout box and permits both axes to shrink inside flex
/// parents. GPUI's `size_full` alone retains the default minimum size, which can
/// make nested panes or virtual-list viewports overflow instead of shrinking.
pub fn full_size_column() -> Div {
    div().size_full().min_h_0().min_w_0().flex().flex_col()
}

/// Takes the remaining height in a vertical flex parent and establishes a new
/// vertical flex boundary for its content.
pub fn remaining_space_column() -> Div {
    div().w_full().flex_1().min_h_0().min_w_0().flex().flex_col()
}

/// Covers the nearest positioned ancestor.
pub fn absolute_full_size() -> Div {
    div().absolute().inset_0().size_full()
}

/// The window-sized page root: full-size column, clipped, dressed in the theme.
fn page_root(theme: BrowserTheme) -> Div {
    full_size_column().relative().overflow_hidden().bg(theme.page_bg).text_color(theme.text)
}

/// The window-sized application root shared by browser views.
pub fn app_page(font_family: impl Into<SharedString>, font_size_px: u8, theme: BrowserTheme) -> Div {
    page_root(theme).font_family(font_family).text_size(px(font_size_px as f32))
}

/// A reader-window root using the application-wide interface typography.
pub fn reader_app_page(window: &mut Window, cx: &App, theme: BrowserTheme) -> Div {
    let application_theme = Theme::global(cx);
    let font_size = application_theme.font_size;
    let font_family = application_theme.font_family.clone();
    sync_reader_rem_size(window, cx);
    page_root(theme).font_family(font_family).text_size(font_size)
}

/// Set the reader's rem before measuring toolbar-dependent overlay geometry.
pub fn sync_reader_rem_size(window: &mut Window, cx: &App) {
    window.set_rem_size(Theme::global(cx).font_size);
}

/// Error presentation for a browser section.
///
/// The caller owns error-state branching and the retry element's behavior;
/// this helper only supplies their shared layout and styling.
pub fn browser_error_banner(error: impl Into<SharedString>, retry: impl IntoElement, body: AnyElement) -> Div {
    full_size_column()
        .gap(px(SPACE_SM))
        .child(div().w_full().flex().items_center().justify_between().gap(px(SPACE_SM)).child(Alert::error("browser-error", error.into()).with_size(Size::Small)).child(retry))
        .child(div().flex_1().min_h_0().child(body))
}

/// The rail column. Carries no edge of its own — see [`nav_rail_destinations`].
pub fn sidebar(theme: BrowserTheme) -> Scrollable<Div> {
    div().w(gpui::rems(crate::NAV_RAIL_SIZE_REM)).h_full().min_h_0().flex_none().flex().flex_col().gap_0().bg(theme.page_bg).overflow_y_scroll_hidden()
}

/// Everything in the rail below the library switcher.
///
/// Draws no edge against the content. A rail edge and a topbar underline were
/// tried and removed: nothing scrolls beneath either region — they are flex
/// siblings of the content, and the grid paginates rather than scrolls — so the
/// line marked a boundary the eye already had from a column of tiles meeting a
/// grid of cards.
pub fn nav_rail_destinations(theme: BrowserTheme) -> Div {
    let _ = theme;
    div().w_full().flex_1().min_h_0().flex().flex_col().gap_0()
}

pub fn section_title(text: impl Into<SharedString>) -> Div {
    div().text_size(gpui::rems(TEXT_LG)).child(text.into())
}

pub fn section_header(title: impl Into<SharedString>, detail: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    let detail = detail.into();
    // A title and its detail line are one unit, but 2px put the detail into the
    // title's descenders. `SPACE_XS` still binds them and clears the type.
    let mut header = div().flex().flex_col().gap(px(SPACE_XS)).child(div().text_size(gpui::rems(TITLE_MD)).child(title.into()));
    if !detail.is_empty() {
        header = header.child(div().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(detail));
    }
    header
}

pub fn action_row() -> Div {
    div().flex().items_center().gap(px(SPACE_SM)).flex_wrap()
}
