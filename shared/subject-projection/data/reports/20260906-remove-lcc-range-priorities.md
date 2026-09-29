# Range priorities removed — 2026-09-06

LCC range priorities no longer participate in matching. Matching descendants win over ancestors; among unrelated subjects, exact keys, subclass specificity, and narrower ranges determine the winner. Equally specific unrelated candidates remain unresolved. Legacy numeric @N suffixes are accepted but discarded during loading, hashing, and persistence.

Both active databases now store bare range syntax, for example Vasa Era's `DL701..DL702`. Removed 9,249 curated and 49,541 master priority suffixes, collapsing 204 and 287 duplicate selectors respectively. The compiler preserves interval boundaries instead of merging overlapping ranges, because widths now determine specificity. Range collisions no longer fail validation merely because the subjects have the same label; the matcher exposes ambiguities. Structural and exact-selector validation remains in place.

Runtime revision 8 and matcher version 26 invalidate old projections. The production audit executable, complete match cache, subject usage TSV, and viewer were refreshed, with every taxonomy table guarded against the final audit snapshot.

## Validation

- All 47 package tests and compiler normalization checks passed, including ignored unequal priorities, canonical deduplication, overlay round trips, ambiguity handling, and embedded revision consistency.
- Old and new matchers compared the same initial taxonomy against 3,774,074 Open Library codes / 21,512,946 classification uses. The existing abbreviated `GR140..153@3` endpoint was expanded for the old matcher so it could load that snapshot.
- Before: 2,161,852 matched code variants / 10,688,667 uses. After: 2,159,107 / 10,671,910 uses.
- Exactly 2,745 variants / 16,757 uses became unresolved across 15 candidate groups. All remaining assignments were unchanged; no new matches or arbitrary reassignments. These are classification uses, not distinct books.
- Conflict details: `20260906-priority-removal-conflicts.tsv`. Largest groups: Saxony / Württemberg / Hanseatic League (8,480 uses), and Criminal Law & Procedure / General Criminal Law & Procedure (5,554 uses).
- Concurrent taxonomy edits were preserved. A temporary duplicate-selector validation error during a separate split affected both matchers and was resolved by that work. Final live tests passed and final cached counts come from the updated, guarded snapshot; they differ from the initial controlled comparison because of those independent edits.
- Both database integrity and foreign-key checks passed; no priority suffixes remain in either database.

Evidence: `client/.test-tmp/remove-range-priorities/`, including original snapshots, compressed controlled-comparison TSVs, comparison database, audit JSON, final snapshot/output, test logs and installation counts. Migration: `20260906_remove_lcc_range_priorities.sql` (applies to both databases).
