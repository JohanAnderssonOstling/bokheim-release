//! Browser book capabilities and their library-owned leases.

use crate::{book::BookSource, LibraryId};
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static SOURCES: RefCell<HashMap<String, (LibraryId, client_platform_web::range_reader::ReaderLease)>> = RefCell::new(HashMap::new());
}

pub(crate) enum BookLease {
    Local(String),
    Audiobook(crate::library::browser_audiobook::PlaybackLease),
    None,
}

/// Actor-local ownership of browser book resources. The actor owns the
/// leases, while the platform module owns the JavaScript-backed readers.
pub(crate) struct BookResources {
    leases: HashMap<u64, BookLease>,
    next: u64,
}
impl Default for BookResources {
    fn default() -> Self {
        Self { leases: HashMap::new(), next: 1 }
    }
}
impl BookResources {
    pub(crate) fn retain(&mut self, lease: BookLease) -> u64 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.leases.insert(id, lease);
        id
    }
    pub(crate) fn release(&mut self, id: u64) {
        self.leases.remove(&id);
    }
}

impl Drop for BookLease {
    fn drop(&mut self) {
        if let Self::Local(id) = self {
            SOURCES.with(|sources| sources.borrow_mut().remove(id));
        }
    }
}

pub(crate) fn register_local_reader(library: LibraryId, reader: client_platform_web::web_storage::FileReader) -> Result<(BookSource, BookLease), String> {
    let (source, lease) = client_platform_web::range_reader::register(reader)?;
    let capability = source.capability.clone();
    SOURCES.with(|sources| sources.borrow_mut().insert(capability.clone(), (library, lease)));
    Ok((BookSource::Local { capability: source.capability, length: source.length }, BookLease::Local(capability)))
}

pub fn read_local_range(library: LibraryId, capability: &str, offset: u64, length: usize) -> Result<Vec<u8>, String> {
    SOURCES.with(|sources| {
        let sources = sources.borrow();
        let (owner, _) = sources.get(capability).ok_or("book source closed; reopen the book")?;
        if *owner != library {
            return Err("book belongs to another library".into());
        }
        client_platform_web::range_reader::read_range(capability, offset, length)
    })
}
