//! Leaf widgets used by selected-library pages.

mod archive;
mod book_card;
pub(crate) mod mobile_context_menu;
mod paginator;

pub(crate) use book_card::BookCardView;
pub(crate) use book_card::{BookPlacement, LoadedCover, OpenBookDetail, PlaceBook, RemoveBook, cover_aspect_ratio, loaded_cover_from_jpeg};
pub(crate) use paginator::{
    CursorMove, Paginator, PaginatorAxis, PaginatorChild, PaginatorGroup, PaginatorGroupPolicy, PaginatorMarqueeChanged, PaginatorSelectionChanged, PaginatorSizing, PaginatorWidthPolicy,
    author_index_entry_policy, author_index_letter_sizing, book_card_policy, browse_section_card_policy, browse_section_item_policy, graph_cover_policy, shelf_card_policy, shelf_rows_policy,
};
