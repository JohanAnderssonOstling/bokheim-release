//! Bounded LRU fragments for one immutable file generation.
use std::collections::VecDeque;

pub struct RangeCache {
    entries: VecDeque<(u64, Vec<u8>)>,
    capacity: usize,
}
impl RangeCache {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self { entries: VecDeque::new(), capacity }
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(any(test, feature = "web-runtime-tests"))]
    pub fn allocated_bytes(&self) -> usize {
        self.entries.iter().map(|(_, bytes)| bytes.capacity()).sum()
    }
    pub fn contains(&self, offset: u64) -> bool {
        self.entries.iter().any(|(start, bytes)| offset >= *start && offset - start < bytes.len() as u64)
    }
    pub fn insert(&mut self, offset: u64, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.entries.retain(|(start, _)| *start != offset);
        if self.entries.len() == self.capacity {
            self.entries.pop_back();
        }
        self.entries.push_front((offset, bytes));
    }
    pub fn read(&mut self, offset: u64, output: &mut [u8]) -> usize {
        let Some(index) = self.entries.iter().position(|(start, bytes)| offset >= *start && offset - start < bytes.len() as u64) else { return 0 };
        if index != 0 {
            let entry = self.entries.remove(index).unwrap();
            self.entries.push_front(entry);
        }
        let (start, bytes) = self.entries.front().unwrap();
        let within = (offset - start) as usize;
        let count = output.len().min(bytes.len() - within);
        output[..count].copy_from_slice(&bytes[within..within + count]);
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_fragments_and_reads_determine_eviction() {
        let mut cache = RangeCache::new(2);
        cache.insert(0, vec![1, 2]);
        cache.insert(10, vec![3]);
        let mut bytes = [0; 4];
        assert_eq!(cache.read(1, &mut bytes), 1);
        assert_eq!(bytes[0], 2);
        assert!(!cache.contains(2));
        cache.insert(20, vec![4]);
        assert!(cache.contains(0));
        assert!(!cache.contains(10));
        cache.insert(0, vec![5]);
        assert_eq!(cache.read(0, &mut bytes), 1);
        assert_eq!(bytes[0], 5);
        assert!(!cache.contains(1));
    }
}
