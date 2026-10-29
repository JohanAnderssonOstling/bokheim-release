//! Bounded reads shared by browser capabilities and CPU range sources.

use std::io::{self, Read, Seek, SeekFrom};

pub(crate) fn read_range(reader: &mut (impl Read + Seek), offset: u64, length: usize) -> io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(length);
    reader.take(length as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}
