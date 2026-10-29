//! The page warmer and seekable parser share one bounded block cache.
use crate::range_cache::{seek_position, RangeCache, BLOCK_BYTES, MAX_BUNDLE_BYTES};
use crate::range_reader::validate_prefix;
use crate::BoxedBookReader;
use pdf_view_common::{PdfByteRange, PdfDependencyIndex, PdfPageSource, PdfPrefetchCancellation};
use std::{
    collections::BTreeSet,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};
const BLOCK: u64 = BLOCK_BYTES as u64;
const CAPACITY: usize = 512; // 32 MiB of raw PDF bytes.
pub use crate::BundleFetch as Fetch;
struct State {
    cache: RangeCache,
    fetch: Fetch,
}
struct Source {
    length: u64,
    index: Option<PdfDependencyIndex>,
    state: Mutex<State>,
}
struct Reader {
    source: Arc<Source>,
    position: u64,
    next_fetch_offset: Option<u64>,
    fetch_bytes: usize,
}
pub fn open(index: PdfDependencyIndex, prefix: Vec<u8>, fetch: Fetch) -> io::Result<(BoxedBookReader, Arc<dyn PdfPageSource>)> {
    if !index.valid(index.pages.len()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid PDF page source"));
    }
    let source = Source::new(index.length, prefix, Some(index), fetch)?;
    Ok((source.reader(), source))
}

/// The same bounded byte cache and adaptive demand reads without a page index.
pub fn open_unindexed(length: u64, prefix: Vec<u8>, fetch: Fetch) -> io::Result<BoxedBookReader> {
    Ok(Source::new(length, prefix, None, fetch)?.reader())
}
impl Source {
    fn new(length: u64, prefix: Vec<u8>, index: Option<PdfDependencyIndex>, fetch: Fetch) -> io::Result<Arc<Self>> {
        validate_prefix(length, &prefix)?;
        let mut cache = RangeCache::new(CAPACITY);
        cache.insert(0, prefix);
        Ok(Arc::new(Self { length, index, state: Mutex::new(State { cache, fetch }) }))
    }

    fn reader(self: &Arc<Self>) -> BoxedBookReader {
        Box::new(Reader { source: self.clone(), position: 0, next_fetch_offset: None, fetch_bytes: BLOCK_BYTES })
    }

    fn index(&self) -> Result<&PdfDependencyIndex, String> {
        self.index.as_ref().ok_or_else(|| "PDF dependency index unavailable".to_owned())
    }

    fn prepare(&self, ranges: &[PdfByteRange]) -> io::Result<()> {
        let (blocks, complete) = blocks_for(ranges, CAPACITY);
        if complete {
            self.prepare_blocks(blocks)
        } else {
            Ok(())
        }
    }

    fn prepare_blocks(&self, blocks: BTreeSet<u64>) -> io::Result<()> {
        self.prepare_blocks_cancellable(blocks, None)
    }

    fn prepare_blocks_cancellable(&self, blocks: BTreeSet<u64>, cancellation: Option<&PdfPrefetchCancellation>) -> io::Result<()> {
        let cancelled = || cancellation.is_some_and(PdfPrefetchCancellation::is_cancelled);
        if cancelled() {
            return Ok(());
        }
        let mut state = self.state.lock().map_err(|_| io::Error::other("PDF byte cache poisoned"))?;
        if cancelled() {
            return Ok(());
        }
        // Keep cached visible/window bytes ahead of unrelated old blocks, so
        // filling the rest of the window cannot evict them during prefetch.
        for &offset in &blocks {
            state.cache.touch(offset);
        }
        let missing = blocks.into_iter().filter(|offset| !state.cache.contains(*offset)).collect::<Vec<_>>();
        for batch in missing.chunks(MAX_BUNDLE_BYTES / BLOCK_BYTES) {
            if cancelled() {
                break;
            }
            let mut spans: Vec<(u64, usize)> = Vec::new();
            for &offset in batch {
                let count = (self.length - offset).min(BLOCK) as usize;
                if let Some((start, length)) = spans.last_mut() {
                    if *start + *length as u64 == offset {
                        *length += count;
                        continue;
                    }
                }
                spans.push((offset, count));
            }
            let response = match cancellation {
                Some(cancellation) => state.fetch.fetch_cancellable(spans.clone(), cancellation),
                None => state.fetch.fetch(spans.clone()),
            };
            let bytes = match response {
                Ok(bytes) => bytes,
                Err(_) if cancelled() => return Ok(()),
                Err(error) => return Err(error),
            };
            state.cache.insert_ranges(&spans, &bytes)?;
        }
        Ok(())
    }
}
/// Bound planning as well as fetched bytes. Pages are selected whole; a single
/// page larger than the cache retains the ordinary demand-read path.
fn blocks_for(ranges: &[PdfByteRange], limit: usize) -> (BTreeSet<u64>, bool) {
    let mut blocks = BTreeSet::new();
    for range in ranges {
        for offset in (range.offset / BLOCK * BLOCK..range.offset + range.length).step_by(BLOCK as usize) {
            if !blocks.contains(&offset) {
                if blocks.len() == limit {
                    return (blocks, false);
                }
                blocks.insert(offset);
            }
        }
    }
    (blocks, true)
}

impl PdfPageSource for Source {
    fn prepare_opening(&self, page: usize) -> Result<(), String> {
        let page = self.index()?.pages.get(page).ok_or("PDF page outside index")?;
        let ranges = page.iter().chain(&self.index()?.startup).copied().collect::<Vec<_>>();
        let (blocks, complete) = blocks_for(&ranges, CAPACITY);
        if complete { self.prepare_blocks(blocks) } else { self.prepare(page) }.map_err(|error| error.to_string())
    }
    fn prefetch_pages(&self, pages: &[usize]) -> Result<(), String> {
        self.prefetch_pages_cancellable(pages, &PdfPrefetchCancellation::default())
    }
    fn prefetch_pages_cancellable(&self, pages: &[usize], cancellation: &PdfPrefetchCancellation) -> Result<(), String> {
        let mut selected = BTreeSet::new();
        for (priority, &page) in pages.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            let ranges = self.index()?.pages.get(page).ok_or("PDF page outside index")?;
            let (blocks, complete) = blocks_for(ranges, CAPACITY);
            let extra = blocks.difference(&selected).count();
            if priority == 0 && !complete {
                // Preserve the visible page's current working bytes instead of
                // replacing them with neighbors when this page cannot fit.
                return Ok(());
            }
            if complete && selected.len() + extra <= CAPACITY {
                selected.extend(blocks);
            }
        }
        self.prepare_blocks_cancellable(selected, Some(cancellation)).map_err(|error| error.to_string())
    }
    fn prepare_startup(&self) -> Result<(), String> {
        self.prepare(&self.index()?.startup).map_err(|e| e.to_string())
    }
    fn prepare_page(&self, page: usize) -> Result<(), String> {
        self.prepare(self.index()?.pages.get(page).ok_or("PDF page outside index")?).map_err(|e| e.to_string())
    }
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() || self.position >= self.source.length {
            return Ok(0);
        }
        let mut state = self.source.state.lock().map_err(|_| io::Error::other("PDF byte cache poisoned"))?;
        if !state.cache.contains(self.position) {
            let offset = self.position / BLOCK * BLOCK;
            let needed = bytes.len().saturating_add((self.position - offset) as usize).min(MAX_BUNDLE_BYTES);
            let adaptive = if self.next_fetch_offset == Some(offset) { (self.fetch_bytes * 2).min(MAX_BUNDLE_BYTES) } else { BLOCK_BYTES };
            let blocks = adaptive.max(needed).div_ceil(BLOCK_BYTES);
            let mut length = (self.source.length - offset).min((blocks * BLOCK_BYTES) as u64) as usize;
            // A prewarmed interval ends speculative read-ahead. Reuse those
            // blocks instead of downloading them again in the larger request.
            for ahead in 1..blocks {
                if state.cache.contains(offset + (ahead * BLOCK_BYTES) as u64) {
                    length = length.min(ahead * BLOCK_BYTES);
                    break;
                }
            }
            let data = state.fetch.fetch(vec![(offset, length)])?;
            state.cache.insert_range(offset, length, data)?;
            self.next_fetch_offset = Some(offset + length as u64);
            self.fetch_bytes = length;
        }
        let count = state.cache.read(self.position, bytes);
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for Reader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = seek_position(self.position, self.source.length, from)?;
        if position != self.position {
            self.next_fetch_offset = None;
            self.fetch_bytes = BLOCK_BYTES;
        }
        self.position = position;
        Ok(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_prefetch_stops_after_current_batch_and_keeps_completed_bytes() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let cancelled = PdfPrefetchCancellation::default();
        let cancel_after_fetch = cancelled.clone();
        let index = PdfDependencyIndex { length: 1000 * BLOCK, startup: vec![], pages: vec![vec![PdfByteRange { offset: BLOCK, length: CAPACITY as u64 * BLOCK }], vec![PdfByteRange { offset: 900 * BLOCK, length: BLOCK }]] };
        let (mut reader, hook) = open(
            index,
            vec![0; BLOCK_BYTES],
            Box::new(move |ranges: Vec<(u64, usize)>| {
                recorded.lock().unwrap().push(ranges.clone());
                // Simulate navigation while this network request is in flight.
                cancel_after_fetch.cancel();
                Ok(vec![7; ranges.iter().map(|(_, count)| count).sum()])
            }),
        )
        .unwrap();
        let before_start = PdfPrefetchCancellation::default();
        before_start.cancel();
        hook.prefetch_pages_cancellable(&[0], &before_start).unwrap();
        assert!(calls.lock().unwrap().is_empty(), "cancel before starting must fetch nothing");
        hook.prefetch_pages_cancellable(&[0], &cancelled).unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![vec![(BLOCK, MAX_BUNDLE_BYTES)]], "cancel during the first request must skip the remaining seven batches");
        for block in [1, 64] {
            reader.seek(SeekFrom::Start(block * BLOCK)).unwrap();
            let mut byte = [0];
            reader.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [7]);
        }
        assert_eq!(calls.lock().unwrap().len(), 1, "the completed request must stay cached");
        // Current-page demand is still allowed after optional work is canceled.
        hook.prepare_page(1).unwrap();
        assert_eq!(calls.lock().unwrap()[1], vec![(900 * BLOCK, BLOCK_BYTES)]);
        hook.prefetch_pages(&[0]).unwrap();
        assert_eq!(calls.lock().unwrap().len(), 9, "a new prefetch must reuse the first completed batch");
    }

    #[test]
    fn page_bundles_reuse_global_fonts_and_do_not_request_other_pages() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let range = |block| PdfByteRange { offset: block * BLOCK, length: BLOCK };
        let index = PdfDependencyIndex { length: 8 * BLOCK, startup: vec![range(1)], pages: vec![vec![range(1), range(3), range(5)], vec![range(1), range(7)]] };
        let (mut reader, hook) = open(
            index,
            vec![0; BLOCK as usize],
            Box::new(move |ranges: Vec<(u64, usize)>| {
                recorded.lock().unwrap().push(ranges.clone());
                Ok(ranges.into_iter().flat_map(|(offset, length)| vec![(offset / BLOCK) as u8; length]).collect())
            }),
        )
        .unwrap();
        hook.prepare_startup().unwrap();
        hook.prepare_page(0).unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![vec![(BLOCK, BLOCK as usize)], vec![(3 * BLOCK, BLOCK as usize), (5 * BLOCK, BLOCK as usize)]]);
        reader.seek(SeekFrom::Start(5 * BLOCK + 13)).unwrap();
        let mut bytes = [0; 3];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, [5; 3]);
        hook.prepare_page(0).unwrap();
        assert_eq!(calls.lock().unwrap().len(), 2);
        hook.prepare_page(1).unwrap();
        assert_eq!(calls.lock().unwrap()[2], vec![(7 * BLOCK, BLOCK as usize)]);
        assert!(reader.seek(SeekFrom::Current(i64::MIN)).is_err());
    }
    #[test]
    fn oversized_page_stays_demand_read_and_incomplete_batches_fail() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let index = PdfDependencyIndex { length: (CAPACITY as u64 + 3) * BLOCK, startup: vec![], pages: vec![vec![PdfByteRange { offset: BLOCK, length: (CAPACITY as u64 + 2) * BLOCK }]] };
        let (mut reader, hook) = open(
            index,
            vec![0; BLOCK as usize],
            Box::new(move |ranges: Vec<(u64, usize)>| {
                recorded.lock().unwrap().push(ranges);
                Ok(vec![])
            }),
        )
        .unwrap();
        hook.prepare_page(0).unwrap();
        hook.prefetch_pages(&[0]).unwrap();
        assert!(calls.lock().unwrap().is_empty(), "one page larger than 32 MiB must retain demand reads");
        reader.seek(SeekFrom::Start(BLOCK)).unwrap();
        assert_eq!(reader.read(&mut [0]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn window_fits_fewer_pages_without_evicting_cached_visible_bytes() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let span = |block, count| vec![PdfByteRange { offset: block * BLOCK, length: count * BLOCK }];
        let index = PdfDependencyIndex { length: 1800 * BLOCK, startup: vec![], pages: vec![span(1100, 64), span(1, 320), span(400, 300), span(900, 128), span(1600, 192)] };
        let (mut reader, hook) = open(
            index,
            vec![0; BLOCK as usize],
            Box::new(move |ranges: Vec<(u64, usize)>| {
                recorded.lock().unwrap().push(ranges.clone());
                Ok(vec![7; ranges.iter().map(|(_, count)| count).sum()])
            }),
        )
        .unwrap();
        hook.prepare_page(1).unwrap(); // Visible page: 20 MiB.
        hook.prepare_page(4).unwrap(); // Unrelated newer data: fill the 32 MiB cache.
        calls.lock().unwrap().clear();
        hook.prefetch_pages(&[1, 2, 3, 0]).unwrap();
        let batches = calls.lock().unwrap().clone();
        assert_eq!(batches.iter().flatten().map(|(_, count)| count).sum::<usize>(), 12 * 1024 * 1024);
        assert!(batches.iter().flatten().all(|(offset, _)| *offset >= 900 * BLOCK && *offset < 1164 * BLOCK), "oversized next page must not consume the remaining window budget");
        let count = calls.lock().unwrap().len();
        for block in [1, 320, 900, 1027, 1100, 1163] {
            reader.seek(SeekFrom::Start(block * BLOCK)).unwrap();
            let mut byte = [0];
            reader.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [7]);
        }
        assert_eq!(calls.lock().unwrap().len(), count, "prefetch must preserve the visible page and all selected neighbors");
    }
    #[test]
    fn larger_configured_windows_are_not_truncated_to_four_pages() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let index = PdfDependencyIndex { length: 20 * BLOCK, startup: vec![], pages: (1..=8).map(|page| vec![PdfByteRange { offset: 2 * page * BLOCK, length: BLOCK }]).collect() };
        let (mut reader, hook) = open(
            index,
            vec![0; BLOCK as usize],
            Box::new(move |ranges: Vec<(u64, usize)>| {
                recorded.lock().unwrap().push(ranges.clone());
                Ok(vec![7; ranges.iter().map(|(_, count)| count).sum()])
            }),
        )
        .unwrap();
        hook.prefetch_pages(&(0..8).collect::<Vec<_>>()).unwrap();
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(calls.lock().unwrap()[0].len(), 8);
        for page in 1..=8 {
            reader.seek(SeekFrom::Start(2 * page * BLOCK)).unwrap();
            let mut byte = [0];
            reader.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [7]);
        }
        assert_eq!(calls.lock().unwrap().len(), 1, "every configured page must already be cached");
    }
    #[test]
    fn sequential_demand_reads_grow_to_four_mib_with_or_without_an_index() {
        let length = 16 * 1024 * 1024 + 13;
        for indexed in [false, true] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let recorded = calls.clone();
            let fetch: Fetch = Box::new(move |ranges: Vec<(u64, usize)>| {
                recorded.lock().unwrap().push(ranges.clone());
                Ok(ranges.into_iter().flat_map(|(offset, count)| (offset..offset + count as u64).map(|position| (position % 251) as u8)).collect())
            });
            let prefix = (0..BLOCK_BYTES).map(|position| (position % 251) as u8).collect();
            let mut reader = if indexed { open(PdfDependencyIndex { length, startup: vec![], pages: vec![vec![]] }, prefix, fetch).unwrap().0 } else { open_unindexed(length, prefix, fetch).unwrap() };
            reader.seek(SeekFrom::Start(BLOCK)).unwrap();
            let mut position = BLOCK;
            let mut buffer = vec![0; BLOCK_BYTES];
            while position < length {
                let count = (length - position).min(BLOCK) as usize;
                reader.read_exact(&mut buffer[..count]).unwrap();
                assert!(buffer[..count].iter().enumerate().all(|(index, byte)| *byte == ((position + index as u64) % 251) as u8));
                position += count as u64;
                assert_eq!(reader.stream_position().unwrap(), position, "no-op seeks must preserve sequential read-ahead");
            }
            assert_eq!(reader.read(&mut buffer).unwrap(), 0);
            let lengths = calls.lock().unwrap().iter().flatten().map(|(_, count)| *count).collect::<Vec<_>>();
            assert_eq!(lengths, vec![65536, 131072, 262144, 524288, 1048576, 2097152, 4194304, 4194304, 4194304, 13]);
            reader.seek(SeekFrom::Start(BLOCK)).unwrap();
            reader.read_exact(&mut buffer).unwrap();
            assert_eq!(calls.lock().unwrap().len(), 10, "rereading cached bytes must not fetch again");
        }
    }

    #[test]
    fn demand_reads_honor_large_buffers_cached_boundaries_and_random_seeks() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let index = PdfDependencyIndex { length: 40 * BLOCK, startup: vec![], pages: vec![vec![PdfByteRange { offset: 4 * BLOCK, length: BLOCK }, PdfByteRange { offset: 9 * BLOCK, length: BLOCK }]] };
        let (mut reader, hook) = open(
            index,
            vec![0; BLOCK_BYTES],
            Box::new(move |ranges: Vec<(u64, usize)>| {
                recorded.lock().unwrap().push(ranges.clone());
                Ok(vec![7; ranges.iter().map(|(_, count)| count).sum()])
            }),
        )
        .unwrap();
        hook.prepare_page(0).unwrap();
        calls.lock().unwrap().clear();
        reader.seek(SeekFrom::Start(BLOCK)).unwrap();
        reader.read_exact(&mut vec![0; 8 * BLOCK_BYTES]).unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![vec![(BLOCK, 3 * BLOCK_BYTES)], vec![(5 * BLOCK, 4 * BLOCK_BYTES)]], "read-ahead must stop before prewarmed blocks");
        reader.seek(SeekFrom::Start(20 * BLOCK)).unwrap();
        reader.read_exact(&mut [0]).unwrap();
        assert_eq!(calls.lock().unwrap().last().unwrap(), &vec![(20 * BLOCK, BLOCK_BYTES)]);
        reader.seek(SeekFrom::Current(0)).unwrap();
        reader.read_exact(&mut vec![0; BLOCK_BYTES - 1]).unwrap();
        reader.read_exact(&mut [0]).unwrap();
        assert_eq!(calls.lock().unwrap().last().unwrap(), &vec![(21 * BLOCK, 2 * BLOCK_BYTES)], "contiguous reads resume growth after a random seek");
    }

    #[test]
    fn failed_expanded_demand_read_preserves_position_cache_and_retry_size() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let mut reader = open_unindexed(
            8 * BLOCK,
            vec![0; BLOCK_BYTES],
            Box::new(move |ranges: Vec<(u64, usize)>| {
                let mut calls = recorded.lock().unwrap();
                calls.push(ranges.clone());
                let count = ranges[0].1;
                if calls.len() == 2 {
                    Ok(vec![9; count - 1])
                } else {
                    Ok(vec![7; count])
                }
            }),
        )
        .unwrap();
        reader.seek(SeekFrom::Start(BLOCK)).unwrap();
        reader.read_exact(&mut vec![0; BLOCK_BYTES]).unwrap();
        assert_eq!(reader.read(&mut [0]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(reader.stream_position().unwrap(), 2 * BLOCK);
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [7], "an incomplete response must not poison the cache");
        assert_eq!(*calls.lock().unwrap(), vec![vec![(BLOCK, BLOCK_BYTES)], vec![(2 * BLOCK, 2 * BLOCK_BYTES)], vec![(2 * BLOCK, 2 * BLOCK_BYTES)]]);
    }
}
