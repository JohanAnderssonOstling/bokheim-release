//! Pages backed by the currently selected library.

use gpui::{AnyElement, IntoElement};
use ui_components::BrowserTheme;

pub(crate) mod authors;
pub(crate) mod browse;
pub(crate) mod home;
pub(crate) mod shelves;
pub(crate) mod subjects;
pub(crate) mod trash;

/// Fallback used while the root's authoritative library population is loading.
pub(crate) fn library_empty_state(theme: BrowserTheme) -> AnyElement {
    ui_components::browser_actionable_empty("No books in this library", "Add a book to get started.", gpui::div(), theme).into_any_element()
}

pub(crate) fn section_empty_state(theme: BrowserTheme) -> AnyElement {
    ui_components::browser_actionable_empty("Nothing here", "There is nothing to display in this section.", gpui::div(), theme).into_any_element()
}
