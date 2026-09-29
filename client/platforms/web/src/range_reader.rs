//! Capability-based byte-range access to browser-backed readers.

use crate::web_storage::FileReader;
use std::{
    cell::RefCell,
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
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
        read_range_from(reader, offset, length)
    })
}

fn read_range_from<R: Read + Seek>(reader: &mut R, offset: u64, length: usize) -> Result<Vec<u8>, String> {
    reader.seek(SeekFrom::Start(offset)).map_err(|error| error.to_string())?;
    let mut bytes = vec![0; length];
    let mut count = 0;
    while count < length {
        let n = reader.read(&mut bytes[count..]).map_err(|error| error.to_string())?;
        if n == 0 {
            break;
        }
        count += n;
    }
    bytes.truncate(count);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::read_range_from;
    use std::io::Cursor;

    #[test]
    fn reads_requested_range() {
        let mut reader = Cursor::new(b"0123456789".to_vec());
        assert_eq!(read_range_from(&mut reader, 3, 4).unwrap(), b"3456");
    }

    #[test]
    fn truncates_at_end_of_reader() {
        let mut reader = Cursor::new(b"abc".to_vec());
        assert_eq!(read_range_from(&mut reader, 2, 8).unwrap(), b"c");
        assert_eq!(read_range_from(&mut reader, 9, 1).unwrap(), b"");
    }

    #[test]
    fn rejects_ranges_over_the_limit_before_lookup() {
        assert_eq!(super::read_range("missing", 0, 256 * 1024 + 1), Err("book range exceeds limit".into()));
    }

    #[test]
    fn rejects_closed_capability() {
        assert_eq!(super::read_range("missing", 0, 1), Err("book source closed; reopen the book".into()));
    }
}
