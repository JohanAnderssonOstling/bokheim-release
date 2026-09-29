# Streaming Wikidata extraction

`extract-wikidata.py` creates compressed projected evidence for the book and
author extraction plan. It does not activate a metadata database or download
portraits, websites, or book content.

The current run is on the metadata server, `192.168.1.68`, so laptop suspension
or disconnection does not interrupt it. It uses two bzip2 decoding threads,
normal Python execution with the orjson parser, and one Zstandard compression
thread. The job runs at nice level 10. No Rust binary is built or launched.

## Runtime

- Python 3.13 on the server.
- `indexed_bzip2` 1.7.0 in `/home/johan/lib/indexed-bzip2-1.7.0`.
- `orjson` 3.12.0 in `/home/johan/lib/wikidata-extraction`, installed from the
  published CPython 3.13 manylinux wheel after validating its PyPI SHA-256:
  `1c680706fc8396d95e7c4c1f9482563f552137aef91b57237a3ad5aaf64629df`.
- System `zstd`.
- Deployed scripts in `/home/johan/bin/wikidata-extraction/`.

## Source and output

Source directory: `/home/johan/data/metadata-rich-build-2026-08/`

- Dump: `wikidata-all-6a7c3024-17e7dca7e5.json.bz2`
- Index: `wikidata-all-6a7c3024-17e7dca7e5.seek-index.json`

Full extraction directory: `/home/johan/data/wikidata-extracted-2026-08-12/`

- `manifest.json`: exact source fingerprint, projection version, properties,
  excluded fields, requested range, and chunk size.
- `entities-START-END.jsonl.zst`: independent compressed JSONL chunks, with
  zero-based start-inclusive/end-exclusive record ranges.
- Corresponding `.meta.json`: committed chunk marker with SHA-256, counts,
  byte sizes, record boundaries, and elapsed time.
- `progress.json`: last committed position and run status. `full_dump` indicates
  the requested range, not completion; require `status=complete` as well.
- `heartbeat.json`: current uncommitted scan position, updated every 20,000
  records. After restarting, check its timestamp before using it for progress.
- `process.json`: launch PID, arguments, and launch time.
- `extraction.log`: progress messages and errors.

A 20,000-record pilot around record 29,800,000 is kept separately in
`/home/johan/data/wikidata-extraction-pilot-2026-08-12/`. Its manifest/progress
mark it as a partial range. Do not activate or concatenate it as a full dump.

## Resume

Run the same command with the same source, index, output and chunk size. Source
size, modification time and edge checksum must match the seek index. Committed
chunks are checksum-verified on resume; changed configuration is rejected.
Only one process may own an output directory at a time.

```sh
PYTHONPATH=/home/johan/lib/indexed-bzip2-1.7.0:/home/johan/lib/wikidata-extraction \
  nice -n 10 python3 -u /home/johan/bin/wikidata-extraction/extract-wikidata.py \
  /home/johan/data/metadata-rich-build-2026-08/wikidata-all-6a7c3024-17e7dca7e5.json.bz2 \
  /home/johan/data/metadata-rich-build-2026-08/wikidata-all-6a7c3024-17e7dca7e5.seek-index.json \
  /home/johan/data/wikidata-extracted-2026-08-12 \
  --chunk-records 200000 --threads 2
```

Interrupted `.part` files or data files without a commit marker are replaced
when their chunk is retried. Successfully committed chunks are never appended
twice. Every non-final checkpoint is checked against the seek index before
book. Completed data and metadata are fsynced before atomic book.

The default free-space reserve is 100 GiB, checked before each chunk. A failure
leaves prior committed chunks intact and does not publish a complete snapshot.

## Validation

Run `python3 -m unittest -v test_extract_wikidata` in the deployed script
folder with the same `PYTHONPATH`. Tests cover statement/identifier preservation,
Dewey exclusion, checksum failures, source/configuration changes, partial ranges,
interruption and resume, wrong seek boundaries, and the disk-space reserve.

The real pilot was decompressed again and checked for record count, expected
book fields, and Dewey exclusion. Zstandard integrity validation also passed.
The full run's completion and output totals must be checked before constructing
and activating lookup indexes.

## Classifier retention and recovery (projection v2, September 2026)

The v1 Dewey exclusion documented above is superseded. Projection v2 retains **all
string and external-ID statements**, plus the selected linked-entity properties.
This covers book and topic classifiers without depending on a fixed list
of property IDs. Full ranks, qualifiers, references and original property IDs
remain intact; deprecated statements are retained for fallback use.

Classifier-property definitions present in the dump are retained in chunk metadata.
`wikidata-classifiers.py` recognizes book/topic classification definitions,
with defaults for LCC, DDC, UDC, RVK, Basisklassifikation, Colon and BISAC. New
classifier properties retain their own property ID as their scheme identifier.
The helper selects preferred values, then normal values, then deprecated values
only when no active value exists. It returns direct and inferred evidence separately.
Inference follows an explicit book `P921` link to a topic carrying a topic-scope
classifier, preserving both the topic link and the original classification statement.
It does not traverse broader topics or treat a topic's book-specific code as
classification evidence for a different book.

The replacement output is:
`/home/johan/data/wikidata-extracted-2026-08-12-classifiers-v2/`.
Its `process.json` records the exact restart command and backfill boundary. The old
v1 output remains available and contains `replacement.json` pointing to this bundle.
The old process was stopped with SIGINT; only committed chunks define the boundary.

`run-wikidata-classifiers.py SOURCE INDEX OUTPUT --boundary RECORD` performs:

1. Extraction of the remaining tail using v2.
2. Replay of the previously processed prefix using v2, recovering omitted classifiers.
3. Construction of the staging `classifiers.sqlite` index.

The tail and prefix have independent manifests and resumable chunk checkpoints.
`extract-wikidata-complete.py` provides `completed_chunks()` to read the complete
logical dataset in original record order, validating version, source, continuity
and checksums. It rejects incomplete backfills. Do not concatenate the old v1 files
with this bundle. Root `progress.json` reports phase transitions; each part's
`progress.json` and `heartbeat.json` give live record counts during that phase.

The staging index retains original classifier evidence and validated ISBNs. Its
`isbn_classification_direct` and `isbn_classification_inferred` views keep direct
book assignments separate from `wikidata:topic:P921` inferences. Inferred rows retain
the source topic and linking statement, and must never be treated as exact-edition
identity evidence. Unknown schemes remain keyed by property ID. This index is not
automatically activated in the metadata server. Index construction resumes at
transactionally committed chunks; `state.status=complete` is required for consumption.

Run `python3 -m unittest -v test_extract_wikidata test_wikidata_classifiers` with
the documented server PYTHONPATH. Coverage includes retention of every known and
an unknown string classifier, full provenance, rank fallback, separate inference,
ISBN checksums, and interrupted tail/prefix recovery without duplicate records.
