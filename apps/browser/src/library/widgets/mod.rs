//! Leaf widgets used by selected-library pages.

mod book_card;
mod cover_cache;
mod cover_visibility;
#[cfg(test)]
mod cover_frame;
pub(crate) mod mobile_context_menu;
mod page_controls;
mod paginator;
mod selection_action;

pub(crate) use selection_action::SelectionAction;

pub(crate) use book_card::{BookCardEvent, BookCardEvents, LoadedCover, cover_aspect_ratio, generated_cover_variant, loaded_cover_from_jpeg, mark_feed_book_added};
pub(crate) use book_card::{BookCardInteraction, BookCardPresentation, BookCardView};
pub(crate) use book_card::{CoversEvicted, evict_covers};
pub(crate) use cover_visibility::CoverVisibility;
pub(crate) use paginator::{
    CursorMove, Paginator, PaginatorAxis, PaginatorChild, PaginatorChildren, PaginatorDragPreview, PaginatorGroup, PaginatorGroupPolicy, PaginatorPosition, PaginatorSizing, PaginatorSlots, PaginatorState, PaginatorWidthPolicy,
    book_card_policy, browse_folder_card_policy, browse_folder_item_policy, browse_section_card_policy, browse_section_item_policy, browse_shelf_card_policy, graph_cover_policy,
};

pub(crate) use page_controls::activate_paginator;
