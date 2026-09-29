# Edition-level LCC coverage audit

Audited **36,113,684 editions** in the July 31, 2026 imported snapshot on metadata server **192.168.1.68**. The database was found at `/home/johan/data/openlibrary/current.sqlite`, resolving to `/home/johan/data/openlibrary/metadata-openlibrary-core-2026-07-31-isbn-reverse.sqlite`. No restoration or 19 GB local copy was needed.

| Direct edition classification status | Editions |
|---|---:|
| All direct LCC codes match | 10,721,696 |
| Both matching and failing direct LCC codes | 121,128 |
| Direct LCC present, none match | 178,248 |
| Direct Dewey codes, no direct LCC | 924,291 |
| No direct LCC or Dewey codes | 24,168,321 |

Among the 178,248 editions whose own LCC codes all fail, **2,249** have a matching LCC on their work record, and **19,005** carry direct Dewey data. These groups overlap. **157,268** have neither direct Dewey data nor a matching work-record LCC and form the stricter direct-record recovery queue.

## Interpretation

These are editions, not distinct works, ISBNs, or classification rows. A code is usable here when the frozen taxonomy matcher returns at least one subject. Invalid syntax, unknown coverage and unresolved ambiguity all fail that test. Dewey presence does not assert a verified taxonomy match.

Work alternatives are recovery signals, not newly installed assignments. Three of the 2,249 matching-work alternatives exceed the existing 12-classification work-import threshold. This audit does not simulate sibling consensus, external enrichment, ISBN-level edition grouping or classification-outlier filtering; do not describe the queue as proof that no possible enrichment source can classify these books.

The 24,168,321 editions with no direct LCC/DDC data are a separate missing-metadata population. Some have work-level alternatives. They are not parser failures.

## Original damaged example

`BL65.C8BL60HM621-HM6` occurs on 15 source editions. Twelve have another LCC that matches; three have no usable direct alternative: OL28058089M, OL28146941M, and OL28160240M. The shared damaged string cannot be replaced globally with the complete call number of a different book. Full edition-level evidence is saved in `20260907-damaged-lcc-original-editions.json`.

## Recovery priorities

Counts below are editions carrying each value within the stricter recovery queue; an edition can contribute to more than one row.

| Notation | Editions |
|---|---:|
| `CPB` | 15927 |
| `LAW` | 7043 |
| `IN PROCESS` | 5139 |
| `IN PROCESS (ONLINE)` | 1752 |
| `HD28-70HF4999.2-6182` | 1310 |
| `K7000-7720.22K7073-7` | 320 |
| `IN PROCESS (COPIED) (lcres)` | 294 |
| `IN PROCESS (COPIED)` | 278 |
| `LAW+` | 240 |
| `RC870-923.2RC875-899` | 181 |
| `QA273.A1-274.9QA274-` | 176 |
| `ACQUIRED FOR NLM` | 171 |

Catalog markers such as CPB, LAW and IN PROCESS need better source metadata, not new taxonomy selectors. The largest complete-looking joined-range candidate is `HD28-70HF4999.2-6182`; investigate its source/reference boundaries before extending normalization.

## Validation and artifacts

- Built and ran the optimized release `lcc_gaps` matcher against a frozen taxonomy snapshot; source hashes and taxonomy SHA-256 are in the JSON report.
- The remote source size, modification timestamp, snapshot metadata, and edition/work classification-row totals match the complete full-notation cache.
- A seven-edition fixture passed checks for mixed codes, failing-only codes, work alternatives, Dewey-only data and absent data.
- Summary categories sum to all 36,113,684 editions; the exported failure queue contains exactly 178,248 distinct edition IDs.
- Source database opened read-only; source modification timestamp unchanged. No production records, selectors, counts or service configuration were modified.

Full edition queue: `/home/johan/.cache/bokheim/edition-lcc-audit-20260907/unusable-lcc-editions.csv`.
Reproducible script, matcher output, provenance, logs and frozen taxonomy: `/home/johan/.cache/bokheim/edition-lcc-audit-20260907/`.
Remote audit tables: `192.168.1.68:/tmp/bokheim-edition-lcc-audit-20260907/edition-audit.sqlite3`.
