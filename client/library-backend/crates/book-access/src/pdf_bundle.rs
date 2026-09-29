//! The page warmer and seekable parser share one bounded block cache.
use crate::range_cache::RangeCache;
use pdf_view_common::{PdfByteRange, PdfDependencyIndex, PdfPageSource};
use std::{
    collections::BTreeSet,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};
const BLOCK: u64 = 64 * 1024;
const CAPACITY: usize = 256;
pub type Fetch = Box<dyn FnMut(Vec<(u64, usize)>) -> io::Result<Vec<u8>> + Send + Sync>;
struct State {
    cache: RangeCache,
    fetch: Fetch,
}
struct Source {
    index: PdfDependencyIndex,
    state: Mutex<State>,
}
struct Reader {
    source: Arc<Source>,
    position: u64,
}
pub fn open(index: PdfDependencyIndex, prefix: Vec<u8>, fetch: Fetch) -> Result<(crate::BoxedBookReader, Arc<dyn PdfPageSource>), String> {
    if !index.valid(index.pages.len()) || prefix.len() != index.length.min(BLOCK) as usize {
        return Err("invalid PDF page source".into());
    }
    let mut cache = RangeCache::new(CAPACITY);
    cache.insert(0, prefix);
    let source = Arc::new(Source { index, state: Mutex::new(State { cache, fetch }) });
    Ok((Box::new(Reader { source: source.clone(), position: 0 }), source))
}
impl Source {
    fn prepare(&self, ranges: &[PdfByteRange]) -> io::Result<()> {
        let mut blocks = BTreeSet::new();
        for r in ranges {
            for offset in (r.offset / BLOCK * BLOCK..r.offset + r.length).step_by(BLOCK as usize) {
                blocks.insert(offset);
                // Oversized page payloads stay demand-read: warming more than
                // the cache can retain would evict bytes before rendering.
                if blocks.len() > CAPACITY {
                    return Ok(());
                }
            }
        }
        let mut state = self.state.lock().map_err(|_| io::Error::other("PDF byte cache poisoned"))?;
        let missing = blocks.into_iter().filter(|offset| !state.cache.contains(*offset)).collect::<Vec<_>>();
        for batch in missing.chunks(64) {
            let mut spans: Vec<(u64, usize)> = Vec::new();
            for &offset in batch {
                let count = (self.index.length - offset).min(BLOCK) as usize;
                if let Some((start, length)) = spans.last_mut() {
                    if *start + *length as u64 == offset {
                        *length += count;
                        continue;
                    }
                }
                spans.push((offset, count));
            }
            let bytes = (state.fetch)(spans.clone())?;
            if bytes.len() != spans.iter().map(|(_, n)| n).sum::<usize>() {
                return Err(io::Error::other("incomplete PDF bundle"));
            }
            let mut consumed = 0;
            for (offset, length) in spans {
                for (i, chunk) in bytes[consumed..consumed + length].chunks(BLOCK as usize).enumerate() {
                    state.cache.insert(offset + i as u64 * BLOCK, chunk.to_vec());
                }
                consumed += length;
            }
        }
        Ok(())
    }
}
impl PdfPageSource for Source {
    fn prepare_startup(&self) -> Result<(), String> {
        self.prepare(&self.index.startup).map_err(|e| e.to_string())
    }
    fn prepare_page(&self, page: usize) -> Result<(), String> {
        self.prepare(self.index.pages.get(page).ok_or("PDF page outside index")?).map_err(|e| e.to_string())
    }
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() || self.position >= self.source.index.length {
            return Ok(0);
        }
        let mut state = self.source.state.lock().map_err(|_| io::Error::other("PDF byte cache poisoned"))?;
        if !state.cache.contains(self.position) {
            let offset = self.position / BLOCK * BLOCK;
            let length = (self.source.index.length - offset).min(BLOCK) as usize;
            let data = (state.fetch)(vec![(offset, length)])?;
            if data.len() != length {
                return Err(io::Error::other("incomplete PDF range"));
            }
            state.cache.insert(offset, data);
        }
        let count = state.cache.read(self.position, bytes);
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for Reader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let next = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(n) => self.source.index.length as i128 + n as i128,
            SeekFrom::Current(n) => self.position as i128 + n as i128,
        };
        self.position = u64::try_from(next).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid PDF seek"))?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_bundles_reuse_global_fonts_and_do_not_request_other_pages() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let range = |block| PdfByteRange { offset: block * BLOCK, length: BLOCK };
        let index = PdfDependencyIndex { length: 8 * BLOCK, startup: vec![range(1)], pages: vec![vec![range(1), range(3), range(5)], vec![range(1), range(7)]] };
        let (mut reader, hook) = open(
            index,
            vec![0; BLOCK as usize],
            Box::new(move |ranges| {
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
    fn oversized_page_stays_bounded_and_incomplete_batches_fail() {
        let index = PdfDependencyIndex { length: (CAPACITY as u64 + 2) * BLOCK, startup: vec![], pages: vec![vec![PdfByteRange { offset: BLOCK, length: (CAPACITY as u64 + 1) * BLOCK }]] };
        let (mut reader, hook) = open(
            index,
            vec![0; BLOCK as usize],
            Box::new(|ranges| {
                assert_eq!(ranges.len(), 1);
                Ok(vec![])
            }),
        )
        .unwrap();
        hook.prepare_page(0).unwrap();
        reader.seek(SeekFrom::Start(BLOCK)).unwrap();
        assert!(reader.read(&mut [0]).is_err());
    }
}
