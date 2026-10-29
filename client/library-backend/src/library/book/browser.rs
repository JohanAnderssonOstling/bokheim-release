//! Browser book capabilities and their library-owned leases.

pub(crate) use crate::{book::BookSource, LibraryId};
pub(crate) use std::cell::RefCell;
pub(crate) use std::collections::HashMap;

thread_local! {
    pub(crate) static SOURCES: RefCell<HashMap<String, (LibraryId, client_platform_web::range_reader::ReaderLease)>> = RefCell::new(HashMap::new());
}

pub(crate) enum BookLease {
    Local(String),
    Audiobook(crate::library::browser_audiobook::PlaybackLease),
    None,
}

/// Actor-local ownership of browser book resources. The actor owns the
/// leases, while the platform module owns the JavaScript-backed readers.
pub(crate) type BookResources = client_platform_runtime::pending::Pending<BookLease>;

impl Drop for BookLease {
    fn drop(&mut self) {
        if let Self::Local(id) = self {
            SOURCES.with(|sources| sources.borrow_mut().remove(id));
        }
    }
}
