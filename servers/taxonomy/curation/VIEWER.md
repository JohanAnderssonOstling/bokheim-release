# Taxonomy data and viewer

The immutable curated taxonomy is bundled locally by default. Its subjects use
positive `i64` IDs in Rust, JSON numbers in the taxonomy contract, and SQLite
`INTEGER` keys in both taxonomy artifacts and library assignments. Paths are
presentation routes; multiple paths can refer to the same subject ID.

Taxonomy format 5 requires integer IDs and stores selectors without ordinals.
Parent edges have no stored display order. The standalone viewer sorts roots and
children alphabetically for predictable browsing. Existing subjects retain their assigned
IDs when their labels, parents or selectors change. Allocate new IDs centrally
in the curated database with `INSERT INTO concept(preferred_label) VALUES (?)
RETURNING concept_id`; preserve the `AUTOINCREMENT` sequence and never renumber
subjects or reuse deleted IDs. Clients consume the published IDs and do not
allocate them. Shared master/curated subjects use the same IDs; importing a new
master revision must preserve those identities.

The `*-v2.sqlite3` artifact filenames remain, and their internal
`taxonomy_meta.format_version` is 1. Historical curation reports describe
earlier source data and are not input to the release build.

Generate the self-contained viewer with:

```sh
python3 servers/taxonomy/curation/generate-viewer.py
```

Open `servers/taxonomy/curation/taxonomy-viewer.html` in a browser. It reads
`data/unified-taxonomy-v2.sqlite3` and the OpenLibrary usage TSV. The complete
classification graph is stored separately in `data/master-taxonomy-v2.sqlite3`.
The viewer shows direct hits, descendant hits and combined LCC/BISAC selector
counts. Each subject displays its original source label when available and
human-readable LCC/BISAC descriptions alongside the selectors; search also
matches those names and codes.

Generate the complete original LCC viewer with:

```sh
python3 servers/taxonomy/curation/generate-lcc-viewer.py
```

Open `servers/taxonomy/curation/lcc-viewer.html` to browse the complete
non-obsolete LCC caption hierarchy independently of the curated taxonomy. It
is generated from the local MDSConnect reference database, which contains
fuller structural detail than the reduced 2024 outline extract.

Generate the original BISAC hierarchy viewer with:

```sh
python3 servers/taxonomy/curation/generate-bisac-viewer.py
```

Open `servers/taxonomy/curation/bisac-viewer.html` to browse BISAC headings
and their exact selectors independently of the curated taxonomy.
