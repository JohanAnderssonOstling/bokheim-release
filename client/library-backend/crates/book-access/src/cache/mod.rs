//! Best-effort, device-local cache for immutable remote books. This
//! database never participates in sync or in the downloaded-asset inventory.
use super::storage::{CacheError as DatabaseError, CacheExecutor, CachePriority as Priority, CacheStorage};
use crate::{ContentHash, RemoteFile};
use rusqlite::{params, OptionalExtension};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

const BLOCK_BYTES: usize = 64 * 1024;
const MAX_RANGE_BYTES: usize = 4 * BLOCK_BYTES;
const BUDGET_BYTES: usize = 256 * 1024 * 1024;
const MAX_PENDING_WRITES: usize = 8;

#[derive(Clone, Debug)]
pub struct BookCache<S: CacheStorage> {
    database: Option<S>,
    budget: usize,
    pending: Arc<AtomicUsize>,
}

fn initialize(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    // Reclaim evicted pages at commit; no ever-growing freelist or WAL file.
    conn.execute_batch(include_str!("sql/initialize_pragma.sql"))?;
    // Cap physical database size too (payload budget plus 32 MiB of indexing
    // headroom), including pathological collections of tiny books.
    let page_size: i64 = conn.query_row(include_str!("sql/initialize_pragma_2.sql"), [], |r| r.get(0))?;
    conn.pragma_update(None, "max_page_count", (BUDGET_BYTES as i64 + 32 * 1024 * 1024) / page_size)
}

impl<S: CacheStorage> BookCache<S> {
    pub fn is_available(&self) -> bool {
        self.database.is_some()
    }
    pub fn pending_writes(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
    pub fn with_budget(mut self, bytes: usize) -> Self {
        self.budget = bytes;
        self
    }
    pub fn open(path: &Path) -> Self {
        let database = match S::open(path, initialize) {
            Ok(database) => Some(database),
            Err(error) => {
                log::warn!("Book cache unavailable: {error}");
                None
            }
        };
        Self { database, budget: BUDGET_BYTES, pending: Arc::new(AtomicUsize::new(0)) }
    }

    /// Opening a cached immutable book needs neither HEAD nor a prefix download.
    pub async fn describe(&self, hash: ContentHash) -> Option<(RemoteFile, Vec<u8>)> {
        let database = self.database.as_ref()?;
        database
            .command(Priority::Interactive, move |conn| {
                let source = conn.query_row(include_str!("sql/describe_select.sql"), [hash.as_str()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))).optional()?;
                let Some((checksum, length)) = source else { return Ok(None) };
                let Ok(checksum) = checksum.parse() else { return Ok(None) };
                let source = RemoteFile { hash, checksum, length };
                let blocks = read_blocks(conn, &source, 0, length.min(BLOCK_BYTES as u64) as usize)?;
                Ok(blocks.into_iter().next().flatten().map(|prefix| (source, prefix)))
            })
            .await
            .ok()
            .flatten()
    }

    /// Only an authoritative opening response may select a new revision.
    /// Delayed writes from older sessions still fail the checksum check in store().
    pub async fn activate(&self, source: RemoteFile) {
        let Some(database) = &self.database else { return };
        let result = database
            .command(Priority::Interactive, move |conn| {
                let tx = conn.transaction()?;
                tx.execute(include_str!("sql/activate_delete.sql"), params![source.hash.as_str(), source.checksum.as_str(), source.length as i64])?;
                tx.execute(include_str!("sql/store_insert.sql"), params![source.hash.as_str(), source.checksum.as_str(), source.length as i64])?;
                tx.commit()?;
                Ok(())
            })
            .await;
        if let Err(error) = result {
            log::debug!("Book cache revision update skipped: {error}");
        }
    }

    /// Called only after the session's memory cache misses. Only missing runs
    /// are downloaded, even when a read-ahead batch overlaps persisted blocks.
    pub async fn read<F, Fut>(&self, executor: &impl CacheExecutor, source: &RemoteFile, offset: u64, length: usize, mut fetch: F) -> Result<Vec<u8>, String>
    where
        F: FnMut(u64, usize) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
    {
        if source.length > i64::MAX as u64 || length == 0 || length > MAX_RANGE_BYTES || offset.checked_add(length as u64).is_none_or(|end| end > source.length) {
            return Err("invalid book cache range".into());
        }
        // Arbitrary small protocol reads also work, but fill complete blocks.
        let start = offset / BLOCK_BYTES as u64 * BLOCK_BYTES as u64;
        let end = (offset + length as u64).div_ceil(BLOCK_BYTES as u64).saturating_mul(BLOCK_BYTES as u64).min(source.length);
        let count = (end - start).div_ceil(BLOCK_BYTES as u64) as usize;
        let mut blocks = if let Some(database) = &self.database {
            let source = source.clone();
            database.command(Priority::Interactive, move |conn| read_blocks(conn, &source, start, (end - start) as usize)).await.unwrap_or_else(|_| vec![None; count])
        } else {
            vec![None; count]
        };
        let mut i = 0;
        while i < blocks.len() {
            if blocks[i].is_some() {
                i += 1;
                continue;
            }
            let first = i;
            while i < blocks.len() && blocks[i].is_none() && i - first < MAX_RANGE_BYTES / BLOCK_BYTES {
                i += 1;
            }
            let from = start + (first * BLOCK_BYTES) as u64;
            let size = (end - from).min(((i - first) * BLOCK_BYTES) as u64) as usize;
            let bytes = fetch(from, size).await?;
            if bytes.len() != size {
                return Err("incomplete remote book range".into());
            }
            self.remember(executor, source.clone(), from, bytes.clone());
            for (slot, chunk) in blocks[first..i].iter_mut().zip(bytes.chunks(BLOCK_BYTES)) {
                *slot = Some(chunk.to_vec());
            }
        }
        let bytes: Vec<u8> = blocks.into_iter().flatten().flatten().collect();
        let within = (offset - start) as usize;
        Ok(bytes[within..within + length].to_vec())
    }

    /// Bound queued copies as well as disk usage. Reading never waits for a
    /// cache write; quota, corruption, and shutdown losses are cache misses.
    pub fn remember(&self, executor: &impl CacheExecutor, source: RemoteFile, offset: u64, bytes: Vec<u8>) {
        if self.database.is_none() || !valid_write(&source, offset, &bytes) {
            return;
        }
        if self.pending.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_PENDING_WRITES).then_some(n + 1)).is_err() {
            return;
        }
        struct Pending(Arc<AtomicUsize>);
        impl Drop for Pending {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Release);
            }
        }
        let pending = Pending(self.pending.clone());
        let cache = self.clone();
        executor.spawn_detached(Box::pin(async move {
            let _pending = pending;
            if let Err(error) = cache.store(source, offset, bytes).await {
                log::debug!("Book cache write skipped: {error}");
            }
        }));
    }

    async fn store(&self, source: RemoteFile, offset: u64, bytes: Vec<u8>) -> Result<(), DatabaseError> {
        if !valid_write(&source, offset, &bytes) {
            return Err(DatabaseError::operation("invalid cache write"));
        }
        let Some(database) = &self.database else { return Ok(()) };
        let budget = self.budget;
        database
            .command(Priority::Background, move |conn| {
                let tx = conn.transaction()?;
                // Refuse delayed writes from a different revision. Only activate()
                // may replace a source following an authoritative remote response.
                tx.execute(include_str!("sql/store_insert.sql"), params![source.hash.as_str(), source.checksum.as_str(), source.length as i64])?;
                let matches: bool = tx.query_row(include_str!("sql/store_select.sql"), params![source.hash.as_str(), source.checksum.as_str(), source.length as i64], |r| r.get(0))?;
                if !matches {
                    return Err(DatabaseError::operation("inconsistent immutable book"));
                }
                let tick = next_tick(&tx)?;
                for (index, chunk) in bytes.chunks(BLOCK_BYTES).enumerate() {
                    tx.execute(include_str!("sql/store_insert_2.sql"), params![source.hash.as_str(), (offset + (index * BLOCK_BYTES) as u64) as i64, chunk, tick])?;
                }
                let mut total: i64 = tx.query_row(include_str!("sql/store_select_2.sql"), [], |r| r.get(0))?;
                while total > budget as i64 {
                    let (rowid, size): (i64, i64) = tx.query_row(include_str!("sql/store_select_3.sql"), [], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    tx.execute(include_str!("sql/store_delete.sql"), [rowid])?;
                    total -= size;
                }
                tx.execute(include_str!("sql/store_delete_2.sql"), [])?;
                tx.commit()?;
                Ok(())
            })
            .await
    }
}

fn valid_write(source: &RemoteFile, offset: u64, bytes: &[u8]) -> bool {
    source.length <= i64::MAX as u64
        && !bytes.is_empty()
        && bytes.len() <= MAX_RANGE_BYTES
        && offset % BLOCK_BYTES as u64 == 0
        && offset.checked_add(bytes.len() as u64).is_some_and(|end| end <= source.length && (bytes.len() % BLOCK_BYTES == 0 || end == source.length))
}

fn next_tick(conn: &rusqlite::Connection) -> rusqlite::Result<i64> {
    conn.query_row(include_str!("sql/next_tick_update.sql"), [], |r| r.get(0))
}

fn read_blocks(conn: &mut rusqlite::Connection, source: &RemoteFile, start: u64, length: usize) -> Result<Vec<Option<Vec<u8>>>, DatabaseError> {
    read_offsets(conn, source, &(start..start + length as u64).step_by(BLOCK_BYTES).collect::<Vec<_>>())
}

fn read_offsets(conn: &mut rusqlite::Connection, source: &RemoteFile, offsets: &[u64]) -> Result<Vec<Option<Vec<u8>>>, DatabaseError> {
    let tx = conn.transaction()?;
    let mut blocks = Vec::with_capacity(offsets.len());
    {
        let mut query = tx.prepare(include_str!("sql/read_offsets_select.sql"))?;
        for &offset in offsets {
            let expected = (source.length - offset).min(BLOCK_BYTES as u64) as usize;
            let bytes: Option<Vec<u8>> = query.query_row(params![source.hash.as_str(), offset as i64, source.checksum.as_str(), source.length as i64], |r| r.get(0)).optional()?;
            blocks.push(bytes.filter(|bytes| bytes.len() == expected));
        }
    }
    tx.commit()?;
    // Keep valid reads usable when the cache is full or read-only. All LRU
    // touches for a page share one best-effort transaction.
    let _ = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        let tick = next_tick(&tx)?;
        {
            let mut touch = tx.prepare_cached(include_str!("sql/touch_block.sql"))?;
            for &offset in offsets {
                touch.execute(params![tick, source.hash.as_str(), offset as i64])?;
            }
        }
        tx.commit()
    })();
    Ok(blocks)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

impl<S: CacheStorage> BookCache<S> {
    /// One database probe and one network batch for all missing aligned blocks.
    pub async fn read_bundle<F, Fut>(&self, executor: &impl CacheExecutor, source: &RemoteFile, ranges: Vec<(u64, usize)>, mut fetch: F) -> Result<Vec<u8>, String>
    where
        F: FnMut(Vec<(u64, usize)>) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
    {
        let request = sync_common::api::assets::BookRanges { checksum: source.checksum, ranges: ranges.clone() };
        let total = request.byte_count(source.length).ok_or("invalid book bundle")?;
        if ranges.iter().any(|(offset, length)| offset % BLOCK_BYTES as u64 != 0 || (*length % BLOCK_BYTES != 0 && *offset + *length as u64 != source.length)) {
            return Err("book bundles require aligned blocks".into());
        }
        let offsets = ranges.iter().flat_map(|&(offset, length)| (offset..offset + length as u64).step_by(BLOCK_BYTES)).collect::<Vec<_>>();
        let mut blocks = if let Some(database) = &self.database {
            let source = source.clone();
            let requested_offsets = offsets.clone();
            database.command(Priority::Interactive, move |conn| read_offsets(conn, &source, &requested_offsets)).await.unwrap_or_else(|_| vec![None; offsets.len()])
        } else {
            vec![None; offsets.len()]
        };
        let mut missing: Vec<(u64, usize)> = Vec::new();
        for (&offset, block) in offsets.iter().zip(&blocks) {
            if block.is_some() {
                continue;
            }
            let length = (source.length - offset).min(BLOCK_BYTES as u64) as usize;
            if let Some((start, size)) = missing.last_mut() {
                if *start + *size as u64 == offset {
                    *size += length;
                    continue;
                }
            }
            missing.push((offset, length));
        }
        if !missing.is_empty() {
            let bytes = fetch(missing.clone()).await?;
            if bytes.len() != missing.iter().map(|(_, n)| n).sum::<usize>() {
                return Err("incomplete book bundle".into());
            }
            let mut consumed = 0;
            let mut writes = Vec::new();
            for (offset, size) in missing {
                for (i, chunk) in bytes[consumed..consumed + size].chunks(MAX_RANGE_BYTES).enumerate() {
                    writes.push((offset + (i * MAX_RANGE_BYTES) as u64, chunk.to_vec()));
                }
                consumed += size;
            }
            self.remember_bundle(executor, source.clone(), writes);
            let mut chunks = bytes.chunks(BLOCK_BYTES);
            for block in &mut blocks {
                if block.is_none() {
                    *block = chunks.next().map(<[u8]>::to_vec);
                }
            }
        }
        let mut bytes = Vec::with_capacity(total);
        for block in blocks {
            bytes.extend(block.ok_or("missing book bundle block")?);
        }
        Ok(bytes)
    }
}

impl<S: CacheStorage> BookCache<S> {
    fn remember_bundle(&self, executor: &impl CacheExecutor, source: RemoteFile, writes: Vec<(u64, Vec<u8>)>) {
        if self.database.is_none() || writes.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() > sync_common::api::assets::MAX_BOOK_BUNDLE_BYTES {
            return;
        }
        if self.pending.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_PENDING_WRITES).then_some(n + 1)).is_err() {
            return;
        }
        struct Pending(Arc<AtomicUsize>);
        impl Drop for Pending {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Release);
            }
        }
        let pending = Pending(self.pending.clone());
        let cache = self.clone();
        executor.spawn_detached(Box::pin(async move {
            let _pending = pending;
            for (offset, bytes) in writes {
                if let Err(error) = cache.store(source.clone(), offset, bytes).await {
                    log::debug!("Book bundle cache write skipped: {error}");
                    break;
                }
            }
        }));
    }
}
