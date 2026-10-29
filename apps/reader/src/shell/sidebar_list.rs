//! Variable-height sidebar lists. Keep row descriptions, not off-screen elements.

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, ListAlignment, ListOffset, ListState, Modifiers, canvas, div, list, px};
use gpui_component::scroll::{Scrollbar, ScrollbarAxis};
use ui_components as components;

pub(crate) struct SidebarList<R> {
    pub(crate) rows: Vec<R>,
    pub(crate) state: ListState,
    selected_id: Option<String>,
}

impl<R> Default for SidebarList<R> {
    fn default() -> Self {
        Self { rows: Vec::new(), state: ListState::new(0, ListAlignment::Top, px(100.0)), selected_id: None }
    }
}

impl<R: PartialEq> SidebarList<R> {
    /// Preserve measured rows and the scroll anchor outside the changed range.
    pub(crate) fn set_rows(&mut self, rows: Vec<R>) {
        let prefix = self.rows.iter().zip(&rows).take_while(|(old, new)| old == new).count();
        let suffix = self.rows[prefix..].iter().rev().zip(rows[prefix..].iter().rev()).take_while(|(old, new)| old == new).count();
        if prefix + suffix != self.rows.len() || self.rows.len() != rows.len() {
            self.state.splice(prefix..self.rows.len() - suffix, rows.len() - prefix - suffix);
            // Estimate the scrollbar range without measuring off-screen cards.
            self.state.clone().with_uniform_item_height(px(80.0));
            self.rows = rows;
        }
    }

    pub(crate) fn reveal_selected(&mut self, id: Option<&str>, row: Option<usize>) {
        if self.selected_id.as_deref() != id {
            self.selected_id = id.map(str::to_owned);
            if let Some(row) = row {
                reveal_row(&self.state, row);
            }
        }
    }
}

/// Unknown heights cannot locate an off-screen row by pixel offset. Anchor
/// directly to its index so reaching it never measures the intervening rows.
pub(crate) fn reveal_row(state: &ListState, row: usize) {
    let viewport = state.viewport_bounds();
    if state.bounds_for_item(row).is_some_and(|bounds| bounds.top() >= viewport.top() && bounds.bottom() <= viewport.bottom()) {
        return;
    }
    state.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.0) });
}

/// The callback only reads the owner: GPUI invokes it during layout, when
/// updating the same entity would cause a reentrant entity borrow.
pub(crate) fn panel<C: 'static>(id: &'static str, target: &Entity<C>, state: &ListState, theme: components::BrowserTheme, render_row: impl Fn(&C, usize, &Entity<C>, components::BrowserTheme, &App) -> AnyElement + 'static) -> AnyElement {
    let owner = target.downgrade();
    let rows = list(state.clone(), move |row, _, cx| {
        let Some(owner) = owner.upgrade() else { return div().into_any_element() };
        render_row(owner.read(cx), row, &owner, theme, cx)
    })
    .size_full()
    .pr(px(components::READER_SCROLLBAR_GUTTER_PX));
    let scroll = state.clone();
    let previous_width = state.viewport_bounds().size.width;
    let sizing = state.clone();
    div()
        .id(id)
        .relative()
        .flex_1()
        .size_full()
        .min_h_0()
        .min_w_0()
        .bg(theme.page_bg)
        .on_key_down(move |event, window, cx| {
            if event.keystroke.modifiers != Modifiers::default() {
                return;
            }
            let direction = match event.keystroke.key.as_str() {
                "pageup" => -1.0,
                "pagedown" => 1.0,
                _ => return,
            };
            if scroll.max_offset_for_scrollbar().y > px(0.0) {
                scroll.scroll_by(scroll.viewport_bounds().size.height * direction);
                window.refresh();
                cx.stop_propagation();
            }
        })
        .child(rows)
        // List discards height hints when its width changes, including on its
        // first layout. Restore estimates after painting so the scrollbar can
        // reach unvisited rows without measuring the entire collection.
        .child(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    if sizing.viewport_bounds().size.width != previous_width {
                        sizing.clone().with_uniform_item_height(px(80.0));
                        window.refresh();
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .child(Scrollbar::new(state).axis(ScrollbarAxis::Vertical))
        .into_any_element()
}
