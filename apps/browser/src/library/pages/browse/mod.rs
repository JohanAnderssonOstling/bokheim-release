//! Browsing the library's books, folders, subjects, authors and recent activity.

mod cards;
mod clipboard;
mod controls;
mod detail;
mod drag_drop;
mod feed_actions;
mod folder_actions;
mod folder_picker;
mod graph;
mod languages;
mod layout;
mod listing;
mod navigator;
mod page;
pub(crate) mod profile;
mod section_card;

pub(crate) use detail::spoken_duration;
pub(crate) use listing::BrowseKind;
pub(crate) use page::BrowsePage;
