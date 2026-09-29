//! EPUB search controls.

use gpui::{AnyElement, Context};
use ui_components as components;

use crate::epub::ReaderView;
use crate::shell;

impl ReaderView {
    pub(super) fn render_search(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.search.query.is_none() {
            return None;
        }
        let (current, total) = self.search.match_position.unwrap_or((0, 0));
        Some(shell::search::search_bar(&cx.entity(), &self.search_input, (current, total), theme))
    }
}
