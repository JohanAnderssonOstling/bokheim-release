# Historical Library of Congress Classification 2016 snapshot

The active development reference has been upgraded across all subjects. See
[LCC-2025-REFERENCE-NOTICE.md](LCC-2025-REFERENCE-NOTICE.md) for its sources,
coverage, schema, and rebuild command. The instructions below build a separate
historical 2016 baseline.

The historical 2016 build can include the reviewed computing supplement in
[lcc-reference-updates/computing-2025-v1.json](lcc-reference-updates/computing-2025-v1.json):
89 named entries, three A–Z ranges, three corrected captions, four historical
code redirects, and three scope notes. This is a partial update through 2025,
not a replacement of the entire 2016 schedule with a complete 2025 release.
The original source metadata and filename continue to identify the baseline.

Apply the supplement to an existing reference database from the repository root:

```sh
python3 scripts/lcc_reference_updates.py
```

The command creates a SQLite backup before its first update. Updates run in a
transaction, verify existing captions, and are idempotent. The importer below
also applies the checked-in supplements when rebuilding the reference.
`metadata` records each applied manifest's checksum and scope. New records use
`local:` control numbers, rather than invented Library of Congress identifiers.
Their printed captions and caption hierarchies come from the cited schedules;
unavailable MARC number hierarchies and table sequences are not synthesized.

`reference_update_provenance` stores citations and the original values of revised
captions. `reference_redirect` records retired codes and their replacements;
historical records remain available for looking up older book classifications.
`reference_note` records reviewed scope clarifications. Reference consumers
should consult the redirect table when deciding whether an old code is current.

This process modifies only the development LCC reference. It does not modify
`unified-taxonomy-v2.sqlite3`, `master-taxonomy-v2.sqlite3`, taxonomy selectors,
runtime matching, the taxonomy viewer, or usage reports.

The detailed development-time LCC structure is derived from the Library of
Congress MDSConnect Classification 2016 retrospective snapshot. The Library
provides the snapshot as MARC 21 UTF-8 and MARCXML at:

https://www.loc.gov/cds/products/MDSConnect-classification.html

Run the repository importer from the workspace root:

```sh
python3 scripts/import-lcc-mdsconnect.py
```

The importer verifies the upstream archive by SHA-256 and creates
`shared/subject-projection/data/lcc-mdsconnect-2016-baseline.sqlite3`. The generated database
is intentionally ignored: the compressed upstream MARCXML is about 31 MiB and
expands to about 713 MiB. It can be recreated from the public source whenever
taxonomy curation or mapping needs the complete structural records.

The snapshot includes classification-number spans, captions, caption and
summary hierarchies, number hierarchies, and table references. It is a richer
curation source than `lcc-2024-outline.csv`, but it is older. The historical snapshot is retained as a reproducible source. Reference updates
do not automatically promote anything into the unified taxonomy.

MDSConnect describes these records as open-access and intended primarily for
research, education, and development. The Library of Congress should be
attributed as the source of the original records.
