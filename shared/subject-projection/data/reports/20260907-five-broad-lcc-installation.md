# Five broad LCC selectors added

Added five exact selectors to existing subjects, recovering **14,772 classification uses**. No subjects or ranges were added.

| Code | Subject | Uses recovered |
|---|---|---:|
| N | Visual Arts | 5,106 |
| K | Law | 3,432 |
| D | History | 3,413 |
| BF | Psychology | 1,475 |
| M | Music | 1,346 |

Examined 11316630 LCC codes; recovered 7 stored codes and 14772 uses. No existing matches changed.

A concurrent Child Psychology extraction duplicated an existing sibling and blocked runtime loading. Its range was moved to existing subject 10989 and the empty duplicate removed; see migration `20260907_reuse_existing_child_psychology.sql`.

Validation: 15 exact-code and normalized-variant probes; full comparison of all observed LCC codes; SQLite integrity and foreign keys; verified viewer selector totals. Usage counts, matching cache and viewer were refreshed. Changes were installed additively and concurrent taxonomy edits were preserved. Working files: `/home/johan/.cache/bokheim/five-broad-lcc/`.
