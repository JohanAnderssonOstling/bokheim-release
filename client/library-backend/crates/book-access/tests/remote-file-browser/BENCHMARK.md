# Remote EPUB range batching benchmark

The benchmark runs the production seekable remote file and `EpubProvider` inside
Chromium WASM parser workers. A local HTTP server adds 80 ms before every HEAD
or range response. These measurements cover file opening and archive-entry
reading, including ZIP decompression; they exclude GPUI rendering and the app’s
backend-worker RPC path. They are synthetic latency measurements, not end-to-end
app timings or predictions for every EPUB.

The deterministic fixture is 10,109,445 bytes: metadata, a small chapter, a
1 MiB stored chapter, 8 MiB of unused data, and the same large chapter deflated.
The two large chapters contain seeded random text to exercise multiple ranges.

## Recorded comparison

Chromium 152.0.7977.82, Rust 1.95.0. Times are medians of three trials. Each trial
opens a fresh reader; each stage then uses the cache left by the preceding stage.
The baseline fetched fixed 64 KiB blocks. The optimized reader grows contiguous
cache misses through 64, 128, and 256 KiB batches.

| Stage | Before (ms) | After (ms) | HTTP requests | Bytes fetched |
|---|---:|---:|---:|---:|
| Open EPUB | 268 | 274 | 3 → 3 | 82,437 → 82,437 |
| Small chapter | 13 | 19 | 0 → 0 | 0 → 0 |
| Large stored chapter | 1373 | 538 | 16 → 6 | 1,048,576 → 1,245,184 |
| Large deflated chapter | 873 | 458 | 10 → 5 | 655,360 → 655,360 |
| Revisit stored chapter | 15 | 19 | 0 → 0 | 0 → 0 |

In that batching comparison, opening used HEAD plus two ranges; small and cached chapter reads need no
network requests. The stored chapter trades 192 KiB of extra downloaded bytes
for ten fewer requests. The deflated chapter saves five requests without extra
bytes, because batching stops before the cached archive tail. Small timing
differences in cache-only stages include worker startup and scheduling noise.

The cache retains at most 4 MiB. A range response is at most 256 KiB, and a
discontinuous seek resets the next miss to 64 KiB. Batches are split into
independently evictable blocks so evicting a block actually releases its bytes.
Read-ahead is part of the requested batch; concurrent background prefetch has
not been added.

## Reproduce

From the Html-Prototype workspace root:

```sh
REMOTE_FILE_BENCHMARK=1 \
REMOTE_FILE_BENCHMARK_REPORT=/tmp/remote-epub-benchmark.json \
bash client/app/scripts/test-epub-range-reader.sh
```

Set `PLAYWRIGHT_MODULE` and `CHROMIUM_PATH` for non-default installations.
`REMOTE_FILE_BENCHMARK_LATENCY_MS` controls the artificial delay. The JSON report
includes all three trials, per-stage timings, request counts, byte counts, and
every requested range. Compare two code revisions using the same latency and
fixture settings. The command first runs the browser correctness tests.

## Opening prefix optimization

Replacing HEAD with a validated, cached first-range response reduces opening
from three HTTP requests to two for this fixture. With the same 80 ms latency
and three-trial methodology, median opening time fell from 274 ms to 184 ms
(about 33%). Bytes transferred remained 82,437. Stored and deflated chapter
medians were 536 ms and 452 ms, with unchanged request and byte counts.
The initial prefix is at most 64 KiB and counts toward the 4 MiB cache limit.

Single-block responses now move directly into the cache without a second byte
allocation and copy. Repeated reads from the most recently used block leave
its cache position unchanged. Larger batches still use independently owned
blocks to preserve the eviction memory bound. These CPU/allocation changes do
not change HTTP request counts or fetched byte counts.

A repeat run after these cache changes measured 188 ms median opening versus
184 ms before, with the same two requests and 82,437 bytes. This benchmark
shows no opening latency improvement from the allocation change.
