# LCC development reference: complete 2025 source coverage

The development reference at `lcc-mdsconnect-2016.sqlite3` now uses the complete
2019 MDSConnect MARCXML release as its structural baseline, supplemented across
all subjects by the 45 complete 2025 text schedules and supplementary tables
(20,953 PDF pages), plus all 20 published 2025 approved lists linked from the
Library of Congress approved-list index at the time of this update.

The old SQLite filename is retained for compatibility with existing curation
scripts. Read `metadata.source_release` and `metadata.reference_edition` for
the actual baseline and update coverage. This database is a locally assembled
reference, not an official MARC 2025 release. The 2025 PDFs do not themselves
contain all changes approved in 2025; the pinned monthly lists extend them.
The indexed lists extend through November 2025; no unlisted releases are assumed.

## Rebuild

The exact source URLs and checksums are committed in
[lcc-2025-sources.json](lcc-2025-sources.json). Place the files in a local source
directory, with the monthly lists in its `approved-2025` subdirectory. Run from
the repository root:

```sh
python3 scripts/update-lcc-reference.py --sources /path/to/lcc-schedules-2025
```

Dependencies: Python 3, Beautiful Soup (`beautifulsoup4`), and Poppler's
`pdftotext`. The builder extracts text from the pinned PDFs itself, verifies
all input checksums, builds in a separate file, checks page coverage and SQLite
integrity, then replaces the reference. An existing reference is backed up as
`lcc-mdsconnect-2016.before-complete-2025.sqlite3` before its first replacement.
`--output` can select a separate staged database for review.

An unmodified 2019 baseline can optionally be reused with `--baseline-cache`;
the default rebuild imports the verified compressed MARCXML itself. The old
`import-lcc-mdsconnect.py` and `lcc_reference_updates.py` commands now default
to the separate `lcc-mdsconnect-2016-baseline.sqlite3` historical file, so they
cannot silently replace the updated reference with a 2016 build.

## Representation and provenance

- `classification_record` / `classification_span`: retained MARC structure,
  updated captions, and additional concrete schedule codes and ranges.
  Existing MARC identifiers are preserved. PDF-derived additions use `local:`
  identifiers, rather than fabricated LoC control numbers.
- `reference_document`: every input schedule or approved list, its source URL,
  and checksum.
- `reference_page`: every PDF page, including indexes and cross-references.
  `reference_page_fts` provides full-text searching of the schedule pages.
- `reference_entry`: every detected printed notation and caption, its page and
  line, scope, status, PDF hierarchy and notes, and linked structural record.
  Table rows are scoped by `table_id`; a geographic Cutter is not mistaken for
  a standalone global LCC code.
- `reference_caption_history`: prior captions for records changed by the
  supplement, linked to the source entry that changed them.
- `reference_record_state`: current/optional/obsolete state for explicitly
  printed records. `reference_current_record` excludes explicitly obsolete
  records. Obsolete records remain in the main tables for historical lookup.
- `reference_redirect` and `reference_note`: extracted replacement-code
  references and approved-list notes. Full reference wording remains in the
  source pages and entry notes, including complex references not reducible to
  one replacement code.

Relative templates such as `.x2A-Z` and `<date>` instructions remain searchable
in `reference_entry` and the source pages. They are not invented as ordinary
codes or expanded into hypothetical call numbers. Their existing MARC table
structure is retained. Records absent from the PDFs are not deleted: the PDFs
do not print every record in the MARC dataset. Existing MARC number hierarchies
and table sequences remain intact; newly parsed PDF hierarchies are separately
identified and are not presented as newly supplied MARC data.

This full-source build supersedes the earlier computing-only supplement in
`lcc-reference-updates/computing-2025-v1.json`. That supplement remains available
for historical 2016 rebuilds; it is not reapplied to the new reference.

## Separation from the curated taxonomy

This updates only the development LCC reference. It does not modify curated
concepts, parent/child relationships, selectors, runtime matching, the taxonomy
viewer, or Open Library usage counts. The importer rejects a destination
database containing the curated `concept` table.

Sources: [MDSConnect dataset](https://www.loc.gov/item/2020445552/),
[LCC schedule distribution](https://www.loc.gov/aba/publications/FreeLCC/freelcc.html),
[approved-list index](https://classweb.org/approved/). The manifest pins the
actual 2025 PDF URLs rather than assuming the distribution index's older
2024 links represent the 2025 files.
