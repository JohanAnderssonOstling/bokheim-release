# Open Library classification usage cache

The current cache includes **all classification rows in the completed July 31,
2026 metadata snapshot**, without a top-N cutoff or minimum frequency threshold.
It replaces the earlier 100,000-entry sample.

- Source: `metadata-openlibrary-core-2026-07-31-isbn-reverse.sqlite` (schema 7).
- 19,341,787 edition classification rows and 2,171,159 work classification rows.
- 3,774,074 distinct normalized scheme/code pairs: 3,135,306 LCC and 638,768 DDC.
- Total uses: 21,512,946 (15,676,165 LCC and 5,836,781 DDC).
- Minimum frequency: one. Rare codes are retained.

## Meaning and coverage

A use is a row in `edition_classification` or `work_classification`. Counts are
not distinct titles, editions, works, or books; a book can contribute multiple
classifications, and edition/work records are combined. The derived
`edition_work_classification` table is deliberately excluded to avoid counting
its propagated records again.

Normalization preserves the previous audit: trim ASCII spaces and take the
notation before its first ASCII space. A call number such as
`QA76.73.R87 M38 2019` contributes to `QA76.73.R87`. This is not a complete repair
of malformed call numbers or every spacing convention. The runtime matcher
subsequently validates notations using its own matching rules.

The cache is comprehensive **for the supplied metadata snapshot**, not for all
records currently in live Open Library. The snapshot importer retains editions
with valid ISBNs and their linked works. Editions without valid ISBNs and books
added after the snapshot are outside its coverage. Zero recorded uses is therefore
not proof that Open Library has no books on a subject.

## Location and rebuild

The durable local cache is `$XDG_CACHE_HOME/bokheim/openlibrary-code-usage.sqlite3`,
using `$HOME/.cache` when `XDG_CACHE_HOME` is unset. The previous sample is retained
beside it as `openlibrary-code-usage.before-complete.sqlite3`.
`/tmp/openlibrary-code-usage.sqlite3` is a compatibility symlink to the durable cache.

From the repository root, build to a fresh output filename:

```sh
python3 shared/subject-projection/tools/rebuild-code-usage.py \
  /path/to/openlibrary/current.sqlite /path/to/new-cache.sqlite3
```

The builder opens the source read-only, counts both source tables, aggregates
without a limit, checks that every use is accounted for, records source identity
and coverage in `cache_metadata`, checks SQLite integrity, and publishes only the
completed file. It refuses to overwrite an existing output. The source snapshot
must remain immutable throughout the build.

After installing the new cache, refresh taxonomy usage and the viewer:

```sh
bash shared/subject-projection/tools/refresh-curation-usage.sh
```

`BOKHEIM_CODE_USAGE_DATABASE` can select another cache, and
`BOKHEIM_CODE_MATCHES_OUTPUT` can select a match-export path. Match exports default
to the durable cache directory; staging files prevent a failed audit from replacing
the previous outputs. The Rust audit streams match rows so the complete cache does
not require a second in-memory copy of every matched path. Audit logs also live
in the durable cache directory. For taxonomies whose LCC wildcards contain only
letters, the audit reuses fallback routes by subclass and numeric class after
checking exact selectors. Cutter-specific wildcard taxonomies disable this
optimization. This changes only the audit implementation, not the runtime matcher.

The cache builder's regression test exceeds 100,000 distinct codes and verifies
that single-use codes and edition/work contributions survive. The Rust audit's
uncached source query also no longer has a 100,000-row limit.

The fallback optimization produced byte-identical usage and match exports on
the previous 100,000-entry cache against a pinned taxonomy. Boundary tests cover
decimals, cutters, case normalization, invalid markers, and unhandled spacing.
