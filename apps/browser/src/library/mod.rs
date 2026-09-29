//! Library-scoped pages and services.
//!
//! Each library session owns its current page, while folder and subject pages
//! own their nested navigation.

mod pages;
mod services;
mod widgets;

pub(crate) use self::pages::authors::AuthorsPage;
pub(crate) use self::pages::browse::BrowseCrumb;
pub(crate) use self::pages::browse::{BrowseKind, BrowsePage, BrowseSplitPlane, OpenTrash};
pub(crate) use self::pages::home::HomePage;
pub(crate) use self::pages::trash::TrashPage;
pub(crate) use self::services::{BookDetailClosed, LibraryContext, LibraryUpdateBridge, OpenBook};
pub(crate) use self::widgets::{LoadedCover, loaded_cover_from_jpeg};
