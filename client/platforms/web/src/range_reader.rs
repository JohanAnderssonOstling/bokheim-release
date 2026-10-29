//! Capability-based byte-range access to browser-backed readers.

use crate::web_storage::FileReader;
use std::{
    cell::RefCell,
    collections::HashMap,
    io::{Seek, SeekFrom},
};

thread_local! {
    static READERS: RefCell<HashMap<String, FileReader>> = RefCell::new(HashMap::new());
}

/// The public description of a registered reader. This is safe to send over
/// the library protocol; the reader itself remains a thread-local resource.
pub struct ReaderSource {
    pub capability: String,
    pub length: u64,
}

/// Keeps a browser reader registered until the owning subscription is dropped.
pub struct ReaderLease {
    capability: String,
}

impl Drop for ReaderLease {
    fn drop(&mut self) {
        READERS.with(|readers| {
            readers.borrow_mut().remove(&self.capability);
        });
    }
}

pub fn register(mut reader: FileReader) -> Result<(ReaderSource, ReaderLease), String> {
    let length = reader.seek(SeekFrom::End(0)).map_err(|error| error.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|error| error.to_string())?;
    let capability = uuid::Uuid::new_v4().to_string();
    let lease = ReaderLease { capability: capability.clone() };
    READERS.with(|readers| readers.borrow_mut().insert(capability.clone(), reader));
    Ok((ReaderSource { capability, length }, lease))
}

pub fn read_range(capability: &str, offset: u64, length: usize) -> Result<Vec<u8>, String> {
    if length > 256 * 1024 {
        return Err("book range exceeds limit".into());
    }
    READERS.with(|readers| {
        let mut readers = readers.borrow_mut();
        let reader = readers.get_mut(capability).ok_or("book source closed; reopen the book")?;
        crate::reader::read_range(reader, offset, length).map_err(|error| error.to_string())
    })
}
