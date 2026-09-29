# Open Library metadata server

This standalone server imports the Open Library editions, works, and authors
dumps into an immutable SQLite snapshot. It returns Library of Congress
Classification (LCC) and stable author
identifiers with edition/work provenance.

Snapshot schema v8 retains edition and work titles plus publisher and
book year for the same ISBN-bearing editions. `POST
/v2/edition-identities` can therefore recover an ISBN from bibliographic
metadata without importing unrelated editions. Edition and work titles are
both considered, but an ISBN is selected only from an edition whose normalized
author and every supplied edition discriminator agree. Ambiguous responses
contain a bounded candidate list for local manual review.

Schema v8 also aggregates the public Open Library ratings and reading-log
dumps at work level. Enrichment responses expose only aggregate counts and
rating sums; patron-level activity rows are not retained. Schema v8 deliberately
does not contain resolved Wikidata entities. Schema v9 adds those entities and
their retained geographic relationships.

## Rebuild title keys and backfill subtitles

`rebuild-titles SOURCE EDITIONS.gz WORKS.gz OUTPUT` copies an immutable snapshot,
rebuilds edition/work title keys with internal apostrophes removed, and imports
separate edition/work subtitles from the pinned Open Library dumps. It preserves
ISBNs, classifications, authors, and the original display titles. Full-title
indexes include subtitles, so a specific title/subtitle match precedes a broad
main-title search. Identity responses expose `subtitle` separately.

```sh
target/release/metadata-server rebuild-titles current.sqlite \
  ol_dump_editions_2026-07-31.txt.gz ol_dump_works_2026-07-31.txt.gz \
  metadata-titles.sqlite
```

The offline SQLite cache defaults to 128 MiB. Set `BOKHEIM_TITLE_REBUILD_CACHE_KIB`
(up to 1048576, or 1 GiB) when the build host has enough spare memory.

The command checkpoints into `OUTPUT` with extension `.titles-building.sqlite`;
rerunning with unchanged source and dump files resumes it. Completed output is
never overwritten. Activate the output with the matching server release after
lookup verification. Older snapshots remain readable, but their old title keys
need this rebuild for consistent apostrophe matching. Split snapshots retain the
subtitle tables in the identity store.

## Build a snapshot

The importer streams the gzip files and does not unpack them on disk:

```sh
cargo run --release -p metadata-server -- import \
  /home/johan/data/openlibrary/ol_dump_editions_latest.txt.gz \
  /home/johan/data/openlibrary/ol_dump_works_latest.txt.gz \
  /home/johan/data/openlibrary/ol_dump_authors_latest.txt.gz \
  /home/johan/data/openlibrary/classifications.sqlite
```

The output path must not already exist. The importer builds an adjacent
`.building` file, runs SQLite's integrity check, and renames it only after a
successful import. It retains only editions with a valid ISBN and the works and
authors reachable from those editions; unrelated Open Library records cannot be
queried by this service and are not stored. An optional final numeric argument
limits records from each dump and is intended only for sampling.

Use `import-popularity` with the editions, works, authors, ratings, reading-log,
and output paths to produce the production schema-v8 snapshot. The plain
`import` command remains useful for development snapshots without popularity.
To avoid rescanning Open Library, `add-popularity SOURCE.sqlite RATINGS.txt.gz
READING-LOG.txt.gz OUTPUT.sqlite` copies an immutable schema-v7/v8 bibliography
snapshot and streams only the aggregate activity dumps into the new output. It
also upgrades a completed legacy rich snapshot to schema v9 while preserving
its resolved entities.

### Rich place and person snapshot

The rich importer additionally consumes the Wikidata JSON dump:

```sh
cargo run --release -p metadata-server -- import-rich \
  ol_dump_editions_DATE.txt.gz \
  ol_dump_works_DATE.txt.gz \
  ol_dump_authors_DATE.txt.gz \
  wikidata-all.json.bz2 \
  ol_dump_ratings_DATE.txt.gz \
  ol_dump_reading-log_DATE.txt.gz \
  metadata-DATE.sqlite
```

`build-rich-snapshot.sh DATA_DIRECTORY OUTPUT.sqlite [MAX_RECORDS]` pins the
current dump URLs in `rich-snapshot.sources`, downloads every file with HTTP
range resumption, and starts the importer. Use a new data directory to select a
new monthly snapshot; an interrupted run continues using the pinned inputs.

The adjacent `.building` database is a durable checkpoint. Open Library phases
commit every 250,000 records; Wikidata commits every 20,000 and parses
5,000-record chunks. A bounded pipeline buffers compressed input, decompresses
ahead of the writer, and parses and filters records across the available CPU
cores. Wikidata's large typed records are immediately reduced to compact
retained rows before reaching SQLite. Dump rows first enter unindexed
append-only staging tables; each completed phase then sorts and deduplicates
them into the query schema in one separately checkpointed transaction. The
importer uses a 1 GiB SQLite page cache to reduce repeated B-tree writes.

Re-running the identical command validates each dump's canonical path, byte
size, modification time, record limit, and import mode, replays the compressed
stream to its last checkpoint, and resumes. Completed staging, resolution,
pruning, compaction, and integrity phases are also checkpointed. Do not replace
or touch dump files while a build is unfinished. A rich build produces schema
v9; its Open Library and popularity subset is schema v8. Because the popularity
aggregates add two pinned inputs, unfinished
older builds must be restarted from the source dumps.

Wikidata is decoded into typed records containing only labels, aliases, and the
selected claims used by the resolver. Place candidates must be unique and must
either have strong direct geographic evidence or belong through `P279` to a
retained geographic root. The finalized database contains only resolved
book-place/person entities and the `P131`/`P17`/`P30` ancestor closure. Build
coverage and retained graph counts are available in `metadata_metric`.

Wikidata/schema v9 is not part of the default production activation path. The rich build
may run to completion and is integrity-checked, but its activation supervisor
stops after validation unless `BOKHEIM_ACTIVATE_WIKIDATA_SNAPSHOT=true` is set
explicitly. The Open Library-only snapshot remains the production default.

Set `BOKHEIM_WIKIDATA_DECOMPRESSOR` to an `lbzip2`-compatible executable to
decode the bzip2 dump with two worker threads. The importer owns and validates
the child process and falls back to its built-in decoder when the variable is
unset.

`index-wikidata.py build DUMP.bz2 INDEX.json` creates a dump-specific bzip2
block map plus an entity-count-to-decoded-byte map at every 20,000-record
checkpoint. With `BOKHEIM_WIKIDATA_SEEKER` pointing to that executable and
`BOKHEIM_WIKIDATA_SEEK_INDEX` pointing to the completed index, a resumed scan
seeks directly to its committed record instead of replaying the compressed
stream. The helper validates the canonical path, size, modification time, and
edge checksum before emitting data. Missing configuration falls back to normal
sequential replay; a configured but invalid index fails closed.

The full Wikidata JSON dump is currently roughly 100 GB compressed. The helper
therefore requires 300 GB free by default to accommodate downloads, temporary
indexes, WAL, and `VACUUM`; override `BOKHEIM_METADATA_MIN_FREE_BYTES` only when
the filesystem has been sized separately.

## Run

```sh
BOKHEIM_METADATA_DATABASE=/home/johan/data/openlibrary/classifications.sqlite \
  target/release/metadata-server
```

For workload-isolated serving, split a completed snapshot once:

```sh
target/release/metadata-server split-snapshot \
  metadata-DATE.sqlite \
  metadata-subjects-DATE.sqlite \
  metadata-rich-DATE.sqlite \
  metadata-identities-DATE.sqlite

target/release/metadata-server build-subject-index \
  metadata-subjects-DATE.sqlite \
  metadata-subjects-DATE.idx

BOKHEIM_METADATA_SUBJECT_DATABASE=metadata-subjects-DATE.sqlite \
BOKHEIM_METADATA_SUBJECT_INDEX=metadata-subjects-DATE.idx \
BOKHEIM_METADATA_RICH_DATABASE=metadata-rich-DATE.sqlite \
BOKHEIM_METADATA_IDENTITY_DATABASE=metadata-identities-DATE.sqlite \
  target/release/metadata-server
```

The subject store contains only ISBN/edition/work classification lookup data.
The optional subject index precomputes those results into a compact sorted
binary file. It is memory-mapped and binary-searched, so subject lookups avoid
SQLite work and consume file-backed pages rather than a large heap table. The
server verifies that its dump date and import timestamp match the subject
database. Its builder derives classification evidence in bulk, streams ISBNs
in key order, and writes the fixed record table through a temporary file; heap
use therefore does not grow with the ISBN count. Once an index is active, the
server releases the subject SQLite pool. Without the variable, it falls back
to SQLite.
The rich store contains authors, entity evidence, resolved people and places,
and popularity/description data. The identity store contains the title,
author, publisher, year, ISBN, and classification indexes needed by
`/v2/edition-identities`. All three stores must come from the same dump date.
The legacy single `BOKHEIM_METADATA_DATABASE` configuration remains supported
and supplies all three workloads when the split variables are absent.

Configuration:

- `BOKHEIM_METADATA_DATABASE` is required.
- `BOKHEIM_METADATA_SUBJECT_DATABASE`, optional subject-store override. It can
  replace `BOKHEIM_METADATA_DATABASE` when all split-store variables are set.
- `BOKHEIM_METADATA_SUBJECT_INDEX`, optional mmap subject-index path generated
  by `build-subject-index`.
- `BOKHEIM_METADATA_RICH_DATABASE`, optional rich-store path.
- `BOKHEIM_METADATA_IDENTITY_DATABASE`, optional reverse-identity-store path.
- `BOKHEIM_METADATA_BIND` defaults to `127.0.0.1:8091`.
- Binding plain HTTP to a non-loopback address additionally requires
  `BOKHEIM_METADATA_ALLOW_PUBLIC_HTTP=true`. Prefer a TLS reverse proxy.

## Endpoints

The ISBN lookup endpoints accept 1–256 ISBN-10 or ISBN-13 values:

- `GET /health` returns `ok` for service supervision.
- `GET /internal/operations` returns aggregate snapshot coverage, process-lifetime
  request/error/latency counters, and importer metrics for the authenticated
  operations dashboard. It contains no queried ISBNs or book metadata.
- `POST /v2/classifications` returns LCC metadata plus aggregate
  work popularity without loading author or entity metadata.
- `POST /v2/enrichment` returns classifications, ordered Open Library author
  identities, resolved places and people, and aggregate work popularity.

JSON is supported for diagnostics:

```sh
curl --fail \
  --header 'Content-Type: application/json' \
  --data '{"isbns":["0821338277","9780821338278"]}' \
  http://127.0.0.1:8091/v2/classifications
```

Production clients should use the independent, versioned binary contract from
the `metadata-contract` crate:

```text
Content-Type: application/vnd.bokheim.metadata+protobuf; version=2
```

ISBN-10 and ISBN-13 representations are normalized to the same ISBN-13 key.
One input can return multiple matches when Open Library maps it to multiple
works. Classifications state whether they came from the exact edition or its
work; authors likewise state edition/work provenance. The server never silently
resolves authority ambiguity. Library clients only apply an author identity when
the ISBN resolves to exactly one work and the scanned credit matches exactly
one returned author after name normalization.

The SQLite database is opened read-only. Monthly refreshes should build a new,
dated snapshot and switch the configured path or symlink before restarting the
service.

## Production installation

Install the release binary and unit only after a complete snapshot has passed
the importer's integrity check:

```sh
cargo build --release -p metadata-server
sudo install -o root -g root -m 0755 target/release/metadata-server /usr/local/bin/metadata-server
sudo install -o root -g root -m 0644 servers/metadata/deploy/bokheim-metadata.service /etc/systemd/system/bokheim-metadata.service
ln -sfn /home/johan/data/openlibrary/metadata-YYYY-MM-DD.sqlite \
  /home/johan/data/openlibrary/current.sqlite.next
mv -Tf /home/johan/data/openlibrary/current.sqlite.next \
  /home/johan/data/openlibrary/current.sqlite
sudo systemctl daemon-reload
sudo systemctl enable --now bokheim-metadata.service
```

### Debian and musl deployment builds

The production host runs an older Debian glibc than some development machines.
A host-native release built against newer glibc can fail before startup with an
error such as `GLIBC_2.43 not found`. The host has musl installed, so build the
metadata binary for the portable target when building off host:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl -p metadata-server
```

Use the artifacts from
`target/x86_64-unknown-linux-musl/release/metadata-server`. Verify before
staging:

```sh
file target/x86_64-unknown-linux-musl/release/metadata-server
ldd target/x86_64-unknown-linux-musl/release/metadata-server || true
```

The passwordless metadata activation helper validates service health
after installation and rolls back on failure. A libc-loader error is a build
artifact problem, not a service health problem; rebuild for musl (or build
directly on the Debian host) before retrying activation.

Production Caddy exposes only the bounded client endpoints at
`https://meta.bokheim.se` and proxies them directly to this loopback service.

### Passwordless deployments

Provision the deployment user once from a trusted checkout:

```sh
sudo servers/metadata/deploy/provision-metadata-deploy.sh johan
```

The provisioner installs a root-owned deployment program and grants only that
exact program passwordless elevation. Routine activations can then run
unattended with:

```sh
sudo -n /usr/local/sbin/bokheim-deploy-metadata
```

Do not make systemd units or deployment programs writable by the deployment
user. Changes to those privileged templates require rerunning the one-time
provisioner; staged binaries and data continue to use the passwordless
installer.

## Extend the existing ISBN lookup with LC Books All

`import-loc-lcc.py` reads the free 2016 Library of Congress Books All UTF-8
MARC dump (plain `.utf8` or gzip `.utf8.gz`) into a new copy of the existing
metadata snapshot. It adds `isbn_lcc` inside that database; there is no second
lookup service, HTTP dependency, or extra runtime database setting.

Download the **UTF8** files from
<https://www.loc.gov/cds/products/MDSConnect-books_all.html>. An uncompressed
mirror is available at <https://archive.org/download/marc_loc_2016/>; verify
its file sizes and checksums using the archive manifest. The 43 uncompressed
parts total 10,055,185,164 bytes. This is a 2016 snapshot, not current coverage.

```sh
python3 servers/metadata/import-loc-lcc.py \
  /home/johan/data/openlibrary/current.sqlite \
  /home/johan/data/openlibrary/metadata-with-loc-2016.sqlite \
  /home/johan/data/loc-books-2016/BooksAll.2016.part*.utf8
```

The importer validates and normalizes `020$a` ISBNs (including ISBN-10),
excludes canceled/invalid `020$z`, and retains `050$a` LCC values with their LC
record IDs. It does not append the shelf item number in `050$b`. Multiple valid
ISBNs and classifications are retained. Existing Open Library rows are
unchanged. LC mappings are merged into unambiguous ISBN results, including
ISBNs absent from Open Library; existing ambiguous OL matches remain ambiguous.
No synthetic Open Library identifiers are generated for LC-only records.

The import also retains titles, authors, publishers, book dates,
languages, summaries and subject headings in `lc_record`, linked by
`lc_record_isbn`. Records without ISBNs are retained too, keyed by their MARC
control number. `lc_record_lcc` stores their classifications and `lc_title`
indexes full and main titles for classification-only title/author fallback.
Deleted records remove their metadata, title keys, ISBNs and classifications.
Rich responses fill missing subject headings and an absent
summary when the LC records supply one unique summary. Existing values are
preserved. When Open Library title/author lookup has no match, the edition-identity
response can contain an optional `authority_subjects` result. It requires a
compatible author and title, rejects conflicting subtitles/volume qualifiers,
and accepts multiple LC records only when their normalized LCC codes agree.
Truncated candidate lists cannot establish agreement. This result carries LC
control numbers and classifications; it never assigns an ISBN or OL edition.
Both books without ISBNs and ISBN lookup misses consume it in enrichment. The
conversion lives in `metadata-contract::authority_subjects` for reuse outside the
client. Existing Open Library matches and ambiguous identities are preserved.

The output is published only after a successful integrity check, and an
existing output is never overwritten. Reimport replaces the LC supplement in
an output copy. Supply the complete set of dump parts when replacing a prior
LC import. Allow disk space for a copy of the source snapshot plus the extracted
rows. Imports do not activate the new snapshot automatically.

For an interrupted process that left its staging SQLite file, rerun the same
command with `--resume-building /path/to/the/.building-directory/snapshot.sqlite`.
The source snapshot identity and LC importer version must still match. Version 2
retains records without ISBNs; older imports must be rebuilt from all 43 parts.
Completed dump parts are skipped;
the current partial part is replayed by LC record ID. Use the same immutable dump
parts and ordering. The importer uses a bounded 256 MiB SQLite page cache.

Rebuild any mmap subject index from the new snapshot before activating it:

```sh
cargo run --release -p metadata-server -- build-subject-index \
  /home/johan/data/openlibrary/metadata-with-loc-2016.sqlite \
  /home/johan/data/openlibrary/subjects-with-loc-2016.idx
```

The index includes LC-only ISBNs and the same merged classifications as SQLite.
The import advances the snapshot timestamp so an old index is rejected. When
using split stores, rerun `split-snapshot` on the augmented snapshot to retain
the LC mappings in the subject store and summaries/headings in the rich store.

Validation:

```sh
python3 -m unittest discover -s servers/metadata/tests -v
cargo test --release -p metadata-server --lib
```

### LCC recovery across duplicate Open Library works

When an ISBN resolves to exactly one work but has no LCC classification, the
service searches the existing title indexes for duplicate works. It compares
normalized primary titles (including leading articles and subtitle differences)
and complete author names or author IDs. Edition titles with adaptation,
abridgment, study-guide, selection, or volume markers are excluded; different
numbered titles remain distinct. Missing authors, excessive candidate sets, and
conflicting normalized LCC sets leave the original result unresolved.

The fallback preserves the requested ISBN, exact edition IDs, and work ID. It
adds only LCC codes, with `source: "work"` and an `evidence` array identifying the
`duplicate_work` method and each supporting ISBN, edition ID, and work ID. This
optional evidence is included in both JSON and protobuf responses. Existing LCC
classifications take precedence. Candidate classifications use the raw lookup,
so inferred classifications never recursively support other inferences.

Recovery runs through the service for SQLite and mmap configurations, including
both rich endpoints. Split snapshots keep the bibliographic search data and LC
ISBN mappings in the identity store. This does not modify Open Library records
or replace a book's ISBN, nor does it enable live LC API requests.

A work with exactly one edition carrying classification codes can now inherit that edition’s valid normalized codes. If multiple editions carry codes, the existing two-edition agreement rule applies. SQLite and newly built subject indexes use the same policy; subject index version 3 rejects older indexes so they must be rebuilt.

### Main-title search and client ISBN-miss recovery

Edition identity lookup first preserves an exact full-title match, then tries the
main title before expanding substring candidates from a long subtitle. Volume,
part, numbered-book, adaptation and activity-book qualifiers must agree with the
candidate; shortening a title must not silently select another volume.

The client retries books whose ISBN lookups all explicitly return `no_match`
using their local title, author, publisher and book-year evidence. Only a
unique result with agreeing full author names supplies classifications. The
client retains its original ISBNs and stores the supporting alternate ISBN/work
as classification evidence in the enrichment attempt. Ambiguous results remain
unassigned. Enrichment provider versions `openlibrary_rich_v17` and
`openlibrary_edition_identity_v5` permit existing negative results to be retried.


### LibraryThing classification fallback

Optionally set both `BOKHEIM_LIBRARYTHING_KEY_FILE` (a private file containing the API key)
and `BOKHEIM_LIBRARYTHING_CACHE` (a writable SQLite sidecar path). The service unit reads
these settings from `/home/johan/.config/bokheim/metadata.env` and provides
`/var/lib/bokheim-metadata` for persistent state. With neither setting, the provider is disabled.
The cache is independent of imported snapshots and is never stored in the repository.

After regular lookup and any configured LC API fallback, `/v2/classifications` and
`/v2/enrichment` consult LibraryThing for valid ISBNs without LCC and without conflicting
local matches. Existing metadata is preserved. The API method is `librarything.ck.getwork`;
non-archived `canonicallcc`, `canonicalddc`/`canonicaldewey`, and `canonicalbisac` facts are accepted when supplied. LCC, DDC and BISAC codes remain separate. Codes carry the LibraryThing work ID
in `librarything_isbn:<id>` evidence, without fabricating Open Library identities.
The API can omit LCC displayed on the website; no website scraping is performed.

Every completed lookup is permanently marked as tried: positive codes, a matched work
without LCC, and the verified API error 105 ("Could not determine data ID to retrieve").
These ISBNs are never automatically queried again, regardless of the stored check date. Network, authentication,
rate-limit, malformed-response and invalid-code errors are not cached as misses.
Requests are serialized and limited to 1,000 per UTC day, with at least one second between
requests; quota reservations persist across restarts. Each fallback batch has a 20-second
deadline and returns whatever enrichment has completed by then. Subsequent lookups reuse
completed entries. Keep one serving process per cache/key to avoid sharing the account quota
across independent caches.

Verified live using a private key: ISBN 9781538724736 returned work 22550208 and
`PS3608.O623`; 9781691706631 returned a work without LCC; 9798897248889 returned error 105.
An opt-in live test checks these cases and cache reuse after reopening:
`cargo test --release -p metadata-server librarything::tests::live_api_and_persistent_cache --lib -- --ignored`
(with `BOKHEIM_LIBRARYTHING_KEY_FILE` set).
API documentation: https://www.librarything.com/services/librarything.ck.getwork.php
and https://www.librarything.com/services/webservices.php.

Release validation (2026-09-07): 71 metadata-server tests passed; the opt-in live API
cache test passed; 13 backend enrichment tests passed; release server build succeeded.
A temporary server on the real metadata database confirmed LCC evidence through both
HTTP endpoints and persistent positive/negative cache entries. LibraryThing intermittently
returned HTTP 403; these attempts remained retryable and were not saved as misses.
The release is staged at `/home/johan/bin/metadata-server-librarything` on the metadata host;
the unit is staged at `/home/johan/data/librarything-fallback/bokheim-metadata.service`.
Production was activated on 2026-09-07 with release SHA-256
`9945ae61b60af9ac00eec369bb7e695e5cf6601fa2ee796e6ac5f660fc309c07`.
Both live endpoints returned LibraryThing provenance and reused positive/negative cache
entries without additional API requests. Persistent state is at
`/var/lib/bokheim-metadata/librarything.sqlite`. The LC snapshot deployment now preserves
the installed server release, so the pending snapshot activation will retain this provider.
The initial activation rolled back after an HTTP 400; its cause was not captured. The
subsequent activation and independent live verification succeeded.


#### Auditing ranked books without LCC

`rank-missing-lcc.py` writes popularity-ranked CSVs for all years and for a minimum
book year, excluding LCC already known in OL, the LC ISBN supplement, or the
LibraryThing cache. Popularity is rating count; year is the earliest retained edition year.
`audit-librarything-ranked.py` processes the unique works across both lists using
`metadata-server librarything-lookup ISBN`. This release CLI uses the same parser,
permanent cache and persistent quota as the HTTP fallback, but can explicitly inspect
an ISBN even when OL has ambiguous work identities. It does not resolve that ambiguity.

The audit tries another ISBN when LT has no work match, and stops once an LT work is
identified (with or without canonical LCC). It records codes, LT work ID, ISBN and request
failures separately; failures receive at most three passes. It never turns an HTTP failure
into a completed empty lookup. The report is resumable and written after each work.
An API result without canonical LCC does not establish that the LT website lacks LCC.

For subsequent popularity-only batches, pass `--popular-only --limit 200` to the
ranker and repeat `--exclude-audit PREVIOUS_RESULTS.json` for every completed batch.
This excludes completed works, including API matches without LCC, while allowing
previous request failures to be retried. ISBN cache entries remain permanent.

Before any LibraryThing call, the bulk auditor now runs the full local
`MetadataService::lookup` path for every ISBN associated with the candidate work,
through `metadata-server lookup-lcc DATABASE ISBN...`. This includes duplicate-work
recovery across different ISBNs as well as the LC ISBN supplement. Any existing LCC
marks the work `already_resolved_locally` and prevents a provider call. A failed local
precheck aborts instead of being interpreted as missing classification.

Verified on the real server database with Romeo and Juliet and A Great and Terrible
Beauty using a nonexistent LibraryThing key: both were skipped as locally classified,
with zero API requests. The optimized CLI and gated audit script are installed on the
server; the running HTTP service did not require replacement for this bulk-audit fix.


The LibraryThing cache retains `codes` (LCC, for compatibility), `ddc`, `bisac`, and
`all_schemes_checked`. Old entries default to unchecked for the additional schemes;
normal lookup still returns them without a network request. Explicit
`metadata-server librarything-refresh-schemes ISBN` upgrades an old matched no-LCC
entry once. Failures preserve the old entry and remain retryable; successful empty
upgrades are permanent too. No-work entries and existing LCC entries are not refreshed.
The command shares the production cache, request spacing and daily quota.

Use `audit-librarything-ranked.py --refresh-schemes` with a ranked list of previously
matched no-LCC results for the authorized one-time recheck. The complete normal local
LCC precheck still runs first. Reports distinguish `other_classification_found` from
`lcc_found`, with separate DDC/BISAC columns. API absence does not establish absence
on the LibraryThing website; availability of these additional fields still needs a
live check after the daily quota resets. DDC is retained as metadata; the current
unified taxonomy does not have a DDC crosswalk. BISAC uses its existing crosswalk.


### Continuous LibraryThing enrichment

When the LibraryThing key/cache are configured, the serving process runs one background
worker by default. `BOKHEIM_LIBRARYTHING_BACKGROUND=false` disables it.
`BOKHEIM_LIBRARYTHING_RATINGS_DUMP` optionally points to an Open Library ratings TSV gzip;
otherwise the worker uses the snapshot's `work_popularity` table if available. Rated
works run first (rating count descending), then retained works in work-ID order.
The queue, completion status, retry deadlines, ranking import checkpoint and scan cursor
live in the existing LibraryThing sidecar and survive restarts. A new snapshot resets
local-only skips; completed provider results still never expire. Legacy matched no-LCC
cache entries receive the previously authorized one-time DDC/BISAC upgrade.

All ISBNs of a work are checked first against raw classification rows (including legacy
DDC), all works sharing those ISBNs, the normal local lookup including LC and duplicate-work
recovery, local BISAC, classified siblings without ISBNs, and all cached provider classification schemes. Any known code prevents a
background API request. Failed local checks never fall through to the provider. Extremely
large records with over 512 ISBNs are recorded as `too_many_isbns` for review.

Both traffic types reserve requests in the same SQLite transaction: background work stops
when the day's combined request count reaches 950; interactive fallback can use the last
50 through the existing 1,000-request UTC-day limit. Errors count toward the quota too.
HTTP 429 pauses background work until the next UTC day; five consecutive other failures
pause it for 30 minutes. Individual failed works remain pending with a retry deadline;
negative API results remain permanent. The `api_without_codes` status describes only the
Common Knowledge API response, not classifications shown separately on the website.

`GET /internal/librarything` reports queue counts, worker state, resume time and remaining
request budgets. Do not run the old standalone refresh/audit jobs alongside this worker:
their explicit CLI requests are foreground requests and can consume the reserved budget.
The worker is started after HTTP binding and aborted on server shutdown.

### Ordered Library of Congress → LibraryThing enrichment

Set `BOKHEIM_LOC_CACHE` to a writable SQLite sidecar to enable exact-ISBN SRU
fallback and a continuous first-stage worker. If omitted, it defaults to the configured
LibraryThing cache, so both providers start together. Production uses the same file as
`BOKHEIM_LIBRARYTHING_CACHE`, allowing the LC worker to copy the existing popularity
ranking without consuming any LibraryThing quota. Without that ranking, it scans
retained work IDs. The LC queue and results have separate `loc_` tables.

All book years, including unknown years, are eligible. The earlier 2016 cutoff
has been removed. Existing skipped work is re-evaluated once without expiring API results.
Local classification checks include shared ISBNs, duplicate-work recovery, and cached
LibraryThing LCC/DDC/BISAC. LC uses the documented `http://lx2.loc.gov:210/LCDB`
SRU endpoint (the HTTPS gateway failed during verification), trying equivalent
ISBN-13/ISBN-10 identifiers and requiring an exact ISBN in the returned MARC record.
Multiple distinct matching records are left unresolved. Parsed data includes MARC
050 LCC, 082 DDC, and authority subject headings. Classification evidence identifies
LC and its record identifier using `loc_api_isbn:`.

Positive results, valid no-matches, and ambiguous responses are retained permanently
in `loc_api_result`. HTTP errors, malformed/truncated responses, and SRU diagnostics
are retryable failures, with a one-hour per-ISBN backoff. Requests are serialized, with at least six seconds between request starts.
The reservation is stored in `loc_api_rate`, shared by HTTP/background requests and
preserved across restarts. HTTP 429/503 Retry-After seconds extend the shared delay. Five consecutive worker failures pause scanning for 30 minutes.
`loc_background_work` records progress and errors across restarts. HTTP enrichment
merges LC data into an existing unambiguous match, preserving its book/author identity.
A foreground batch has a 35-second deadline. The worker continues independently of
LibraryThing's daily allowance and its 50-request foreground reserve.


LC backfill collects up to **50 ISBNs per batch**, one untried edition per work,
ordered by descending Open Library rating count (work ID breaks ties). ISBN-10
and ISBN-13 variants are combined in one OR query. Collection yields after 256
local checks or ten seconds, so batches may be smaller when eligible books are
sparse. A failed candidate precheck never authorizes a network request. Batch
candidates have two-minute durable leases and resume after interruption.

Results are fetched in pages of up to 100 MARC records, capped at 1,000 per batch
and 2 MiB per page. Changing totals, repeated record IDs, empty premature pages,
diagnostics, invalid XML, and oversized responses fail the batch. Neither positive
nor negative results are committed until the complete result set is verified.
Each returned record is assigned only to exact canonical ISBNs from MARC 020;
multiple records for an ISBN remain ambiguous. A negative edition result leaves
its work pending until remaining eligible editions are exhausted.

Shelf-control strings such as MLCM/MLCSE are rejected as LCC. Startup repairs older
cached values and archives original JSON in `loc_api_rejected`. Affected previously
successful work statuses are rechecked, reusing valid cached results. Operational
logs include ISBN/record counts for each completed batch; `loc_api_rate.requests`
counts actual network reservations, including pagination and failed attempts.

When LC is configured, LibraryThing takes only `ready_after_lc` work, durably handed
off after every ISBN on the work has a completed LC response without an LCC/DDC code.
Subject headings alone do not block a classification lookup. Ambiguous LC responses
are completed but supply no usable code; transport failures remain retryable and do
not authorize LibraryThing. A cached LC code on any sibling blocks the second provider.
The same gate protects new HTTP fallback requests; permanent cached LibraryThing data
can always be reused. Foreground requests check their requested ISBN at LC; if other
editions still need checking, the background worker completes those before LibraryThing.
Both queues preserve popularity ordering and retry state, even with separate cache files.
Completed LibraryThing attempts are never reset by an LC handoff. Its existing 50-request
daily foreground reserve remains unchanged.

## Wikidata authority ingestion

Wikidata is ingested offline directly into the existing relational authority store:
`edition`, `edition_isbn`, `edition_bibliography`, `edition_author`, `author`,
`author_identifier`, `edition_classification`, `isbn_classification`, and
`title_author_classification`. There is no separate runtime Wikidata lookup
store or serialized ISBN response table. `wikidata_ingestion` stores only the
resumable import checkpoint and completion status.

```sh
python3 index-wikidata-authors.py EXTRACTED_BUNDLE OL_AUTHORS.txt.gz INPUT.sqlite \
  --openlibrary-dump-date 2026-07-31
python3 extend-wikidata-bibliography.py EXTRACTED_BUNDLE INPUT.sqlite
cp --reflink=auto AUTHORITIES.sqlite STAGING.sqlite
metadata-server ingest-wikidata AUTHORITIES.sqlite INPUT.sqlite STAGING.sqlite
```

The input SQLite file is temporary ingestion input and is not needed for serving.
The importer reconciles identities against the existing authority store in bounded
batches. It preserves populated names and conflicting classifications. Direct
LCC and DDC assertions populate the existing classification tables; inferred
topic codes are not automatically applied. Ordinary ISBN and title/author
queries use the resulting edition and author rows. Existing lookup APIs continue
to expose LCC classifications.

New Wikidata editions and authors use negative internal IDs derived from their
QIDs. Public exact edition keys are `wikidata:Q…`; Open Library-specific fields
remain empty for these records. Wikidata-only authors expose a `wikidata`
identifier and an empty `open_library_author_id`. Use
`AuthorMetadata::identity_key()` for author deduplication.

Author merges require shared authority identifiers or compatible names
corroborated by at least two independent works. Conflicting known dates or
identifiers veto a merge. Names alone and author-list positions do not establish
identity. Uncertain authors remain separate identities. Full profiles are
persisted in the normal author table.

Startup refuses unfinished ingestion. Validate the completed staging snapshot
before activating it through the normal metadata deployment path. Split snapshots
retain the relational data and completion marker. Refresh cached misses after
activation. No book files are modified.

The bibliography extension checkpoints two passes over the existing extracted
bundle: book fields first, then names for referenced publisher QIDs. It
retains explicit titles/subtitles and unambiguous book years. Existing
populated authority fields win. It imports ISBN-less book/literary-work types,
editions, and directly classified books; P50 alone does not turn every
scientific article into a book.

ISBN-less books have ordinary edition/bibliography/author/classification
rows but no fabricated ISBN. Exact normalized title plus compatible author
lookup can return `authority_subjects` with provider `wikidata`; conflicting codes
must reach consensus. These are subject matches, not edition identity matches.
The shared metadata-contract validator supports this provider; clients need a
build containing that validator to apply these new subject-only results.

Wikidata's short descriptions fill missing book descriptions. The rich split
stores these in `edition_description` so requests need not open bibliography
storage. Existing Open Library work descriptions take priority, and conflicting
exact-edition descriptions are not arbitrarily selected.

### Core-first rollout

A consistent, separately checkpointed copy of the input can set
`state.ingestion_scope = 'isbn'`. The importer then reconciles ISBN-linked books
and their credited authors without waiting for `bibliography_status` completion.
It defers ISBN-less books and the sweep of other author profiles. The normal
input completion and snapshot compatibility checks still apply; the scope is
part of the ingestion signature, so a resume cannot silently change scope.

Take the copy only after stopping its writer and checkpointing SQLite WAL (or
use SQLite backup). Do not change the live input's bibliography completion flag.
Publish the verified core snapshot first. Optional bibliography work can resume
from its existing chunk checkpoints and build a later full snapshot.


## Audiobook enrichment

Clients send recording evidence to `POST /v2/audiobooks/audible` on
`meta.bokheim.se`. The server discovers candidate ASINs through Audible's US
and UK catalog APIs. Website fallback preserves the requested region with
`ipRedirectOverride=true`, avoiding geolocation redirects to another storefront's
homepage. It prefers Audnexus (`api.audnex.us/books/{ASIN}` and
`/books/{ASIN}/chapters`, with `region`) for recording metadata and precise
chapters, and falls back to Audible when those lookups fail validation or are
unavailable. Audnexus aggregates Audible data; agreement between these providers
is not treated as independent evidence.

Book and chapter responses must match the requested ASIN and region. Audnexus
chapter data must be marked accurate and pass the same complete, non-overlapping
timeline and declared-runtime checks as Audible. Title, recording, ambiguity, and chapter-alignment rules govern selection and
replacement. Explicit `(Unabridged)`/`(Abridged)` annotations constrain recording
type without becoming part of the title. Regional encodings count as the same
recording only when title, credits, every chapter label, and every chapter
boundary agree (at most 250 ms difference per boundary).
Rounded runtime alone cannot supply chapter boundaries. A supported recording
can fill missing ASIN/ISBN, publisher, and description fields without a chapter
replacement. Existing metadata is preserved.

Filename-style titles such as `Paul Bushkovitch - A Concise History of Russia`
produce an additional suffix-only discovery query. Matching discards the prefix
only if its normalized name tokens exactly match a returned catalogue author.
The local title and author metadata are not rewritten by this search heuristic.

Chapter headings that repeat the book title with a numeric track or part suffix
are treated as generic. A complete generated `Chapter 1…N` sequence can yield to
provider names while retaining local positions despite numbering conflicts only when all start and end times
agree within 250 ms, with at least five entries and three distinct durations.
This handles credits and interludes numbered as chapters by the file exporter;
names-only changes preserve every local timestamp. When names-only alignment is
not available, a supported recording with descriptive provider headings can
import its TOC if `abs(provider_ms - local_ms) / local_ms <= 0.01`. Matching,
differing, or absent ISBNs and different track counts do not veto this rule.
Provider starts are retained without scaling; entries starting beyond the local
audio end are omitted and the final end is clamped to the local duration. This
accepts recording-level duration agreement; it does not prove every internal
boundary matches. Policy `audible_v2` records the changed acceptance behavior.
Metadata success does not imply a TOC improvement: inspect
`chapter_plan.renamed` and `chapter_plan.method` separately.

Each candidate records `metadata_provider` and `chapter_provider`; the full
response, outcome, retry deadline, and diagnostic detail are stored in
`audible_enrichment_jobs`. `unavailable` is a service failure, not a negative
match. The library actor keeps failed requests scheduled, respecting the
five-minute retry deadline even after upload preparation completes. Ambiguous
and no-match results require an explicit refresh rather than a repeating loop.

Inspect or retry a local library with the optimized maintenance command:

```sh
cargo run --release -p library-backend --example audiobook_enrichment -- /absolute/library.db status
cargo run --release -p library-backend --example audiobook_enrichment -- /absolute/library.db retry
cargo run --release -p library-backend --example audiobook_enrichment -- /absolute/library.db refresh
cargo run --release -p library-backend --example audiobook_enrichment -- /absolute/library.db refresh https://meta.bokheim.se CONTENT_HASH
```

`retry` attempts due jobs once. `refresh` explicitly rechecks visible recordings, including previous matches
and ambiguous results. It claims each recording atomically without stealing
active claims or releasing a globally pending batch for another client to take. Both use `https://meta.bokheim.se` by default; an optional
URL selects a verification server, followed optionally by one visible content
hash to limit the refresh or retry. They use the same typed claim and atomic
application operations as the app, and never edit audiobook files.

Caddy must allow `/v2/audiobooks/audible`. Deployment verification posts `{}` to
the public endpoint and requires HTTP 422 from its typed request extractor;
this checks routing without contacting providers. A browser OPTIONS preflight
check also requires CORS support for the public, credential-free JSON POST. An HTTP 404 fails deployment
verification even if `/health` is healthy.

LC live ISBN lookup preserves MARC `020$z` values separately from exact edition
ISBNs (`020$a`). A `$z` hit supplies work-level classifications and subject headings
only when its title (including volume qualifiers) and at least one author agree
with the requested ISBN's bibliography in the identity snapshot. Missing,
conflicting, or truncated identity evidence is rejected; multiple matching LC
records remain ambiguous. These matches carry
`loc_api_related_isbn_verified_title_author` provenance and do not replace ISBNs.
The parser upgrade expires old cached misses once and requeues completed LC misses;
existing positive results and request throttling are preserved.
