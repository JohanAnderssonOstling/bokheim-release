//! Browsing the library by folder or by subject.

/// The shelf pages reuse this module's search field, so the browse page and the
/// author index search alike rather than growing two of them.
pub(super) mod controls;
mod detail;
mod folder_actions;
mod folder_picker;
mod graph;
mod layout;
mod listing;
mod page;
mod search;
mod section_card;
mod split;

pub(crate) use folder_picker::{FolderPicker, FolderPickerEvent};
pub(crate) use listing::{BrowseCrumb, BrowseKind};
pub(crate) use page::{BrowsePage, OpenTrash};
pub(crate) use split::BrowseSplitPlane;
