//! Search controls shared by every reader surface.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, Window, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable, IconName};
use ui_components as components;

/// The two operations driven by the common search controls.
pub(crate) trait SearchBarTarget: Sized + 'static {
    fn search_navigate(&mut self, direction: i8, cx: &mut Context<Self>);
    fn search_close(&mut self, window: &mut Window, cx: &mut Context<Self>);
}

/// Search, match count, and navigation share one row at every width.
pub(crate) fn search_bar<C: SearchBarTarget>(target: &Entity<C>, input: &Entity<InputState>, position: (usize, usize), theme: components::BrowserTheme) -> AnyElement {
    let previous_target = target.clone();
    let next_target = target.clone();
    let (current, total) = position;
    let query = components::reader_search_box(theme).key_context("ReaderSearch").child(Input::new(input).appearance(false).cleanable(false).size_full());
    let controls = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(components::SPACE_SM))
        .child(components::reader_match_count(format!("{current} of {total}"), theme))
        .child(components::outlined_icon_button("reader-search-previous", "Previous match", IconName::ChevronLeft, theme).disabled(total == 0).tooltip("Previous match (Shift+Enter)").on_click(move |_, _, cx| {
            previous_target.update(cx, |this, cx| this.search_navigate(-1, cx));
        }))
        .child(components::outlined_icon_button("reader-search-next", "Next match", IconName::ChevronRight, theme).disabled(total == 0).tooltip("Next match (Enter)").on_click(move |_, _, cx| {
            next_target.update(cx, |this, cx| this.search_navigate(1, cx));
        }));
    components::reader_search_bar(theme).child(query).child(controls).into_any_element()
}
