//! Bounded LRU fragments for one immutable file generation.
use std::collections::VecDeque;
use std::io::{self, SeekFrom};

pub(crate) const BLOCK_BYTES: usize = 64 * 1024;
pub(crate) const MAX_BUNDLE_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn seek_position(position: u64, length: u64, from: SeekFrom) -> io::Result<u64> {
    let next = match from {
        SeekFrom::Start(offset) => offset as i128,
        SeekFrom::Current(offset) => position as i128 + offset as i128,
        SeekFrom::End(offset) => length as i128 + offset as i128,
    };
    u64::try_from(next).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid book seek"))
}

pub(crate) struct RangeCache {
    entries: VecDeque<(u64, Vec<u8>)>,
    capacity: usize,
}
impl RangeCache {
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self { entries: VecDeque::new(), capacity }
    }
    pub(crate) fn contains(&self, offset: u64) -> bool {
        self.entries.iter().any(|(start, bytes)| offset >= *start && offset - start < bytes.len() as u64)
    }
    pub(crate) fn touch(&mut self, offset: u64) {
        if let Some(index) = self.entries.iter().position(|(start, bytes)| offset >= *start && offset - start < bytes.len() as u64) {
            if let Some(entry) = self.entries.remove(index) {
                self.entries.push_front(entry);
            }
        }
    }
    pub(crate) fn insert(&mut self, offset: u64, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.entries.retain(|(start, _)| *start != offset);
        if self.entries.len() == self.capacity {
            self.entries.pop_back();
        }
        self.entries.push_front((offset, bytes));
    }
    /// Validate the complete response before inserting any cache blocks.
    pub(crate) fn insert_range(&mut self, offset: u64, length: usize, bytes: Vec<u8>) -> io::Result<()> {
        validate_length(bytes.len(), length)?;
        if length <= BLOCK_BYTES {
            self.insert(offset, bytes);
            Ok(())
        } else {
            self.insert_ranges(&[(offset, length)], &bytes)
        }
    }

    pub(crate) fn insert_ranges(&mut self, ranges: &[(u64, usize)], bytes: &[u8]) -> io::Result<()> {
        let length = ranges.iter().try_fold(0usize, |total, (_, count)| total.checked_add(*count)).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "book bundle length overflow"))?;
        validate_length(bytes.len(), length)?;
        let mut consumed = 0;
        for &(offset, length) in ranges {
            // The first block will be read next; retain it as most recently used.
            for (index, chunk) in bytes[consumed..consumed + length].chunks(BLOCK_BYTES).enumerate().rev() {
                self.insert(offset + (index * BLOCK_BYTES) as u64, chunk.to_vec());
            }
            consumed += length;
        }
        Ok(())
    }
    pub(crate) fn read(&mut self, offset: u64, output: &mut [u8]) -> usize {
        let Some(index) = self.entries.iter().position(|(start, bytes)| offset >= *start && offset - start < bytes.len() as u64) else { return 0 };
        if index != 0 {
            if let Some(entry) = self.entries.remove(index) {
                self.entries.push_front(entry);
            }
        }
        let Some((start, bytes)) = self.entries.front() else { return 0 };
        let within = (offset - start) as usize;
        let count = output.len().min(bytes.len() - within);
        output[..count].copy_from_slice(&bytes[within..within + count]);
        count
    }
}

fn validate_length(actual: usize, expected: usize) -> io::Result<()> {
    if actual == expected {
        return Ok(());
    }
    let kind = if actual < expected { io::ErrorKind::UnexpectedEof } else { io::ErrorKind::InvalidData };
    Err(io::Error::new(kind, "incorrect book range length"))
}
