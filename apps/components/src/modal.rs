use gpui::prelude::*;
use gpui::{Div, ElementId, FontWeight, MouseButton, SharedString, Stateful, div, px, rems};

use crate::{BrowserTheme, RADIUS_MD, SPACE_MD, SPACE_SM, TEXT_MD};

/// The shared full-window layer for a centered modal. The caller supplies the
/// scrim and surface so it can retain its own identity and dismissal behavior.
pub fn modal_overlay() -> Div {
    crate::absolute_full_size().flex().items_center().justify_center().p(px(crate::SPACE_MD))
}

/// A modal's pointer-blocking backdrop.
pub fn modal_scrim(id: impl Into<ElementId>, theme: BrowserTheme) -> Stateful<Div> {
    crate::absolute_full_size().id(id).occlude().bg(theme.overlay_scrim).cursor_default()
}

/// The common elevated surface for dialogs, editors, and other modal content.
pub fn modal_surface(theme: BrowserTheme) -> Div {
    div().relative().min_h_0().min_w_0().occlude().rounded(px(RADIUS_MD)).border_1().border_color(theme.rule).bg(theme.page_bg).shadow(crate::overlay_shadow(theme))
}

/// The shared full-window layer for a bottom sheet: like `modal_overlay`, but
/// the surface it wraps sits flush against the bottom edge instead of
/// floating centered. The caller supplies the scrim and surface, same as
/// `modal_overlay`, so it keeps its own identity and dismissal behavior.
pub fn bottom_sheet_overlay() -> Div {
    crate::absolute_full_size().flex().flex_col().justify_end()
}

/// The sheet's own surface: full width, capped height with its own scroll,
/// and a click-swallowing guard so a tap inside doesn't fall through to the
/// scrim below it and dismiss the sheet it landed on.
pub fn bottom_sheet_surface(theme: BrowserTheme) -> Div {
    div().w_full().flex_none().flex().flex_col().max_h(gpui::relative(0.8)).bg(theme.page_bg).border_t_1().border_color(theme.border).shadow(crate::overlay_shadow(theme)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A bottom sheet's title row. Callers wanting a trailing close button (or
/// nothing at all) append it with `.child(...)`, since `justify_between`
/// leaves room for one without requiring every sheet to have one.
pub fn bottom_sheet_header(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px(px(SPACE_MD))
        .py(px(SPACE_SM + 4.0))
        .border_b_1()
        .border_color(theme.rule)
        .child(div().text_size(rems(TEXT_MD)).font_weight(FontWeight::SEMIBOLD).text_color(theme.text).child(title.into()))
}
