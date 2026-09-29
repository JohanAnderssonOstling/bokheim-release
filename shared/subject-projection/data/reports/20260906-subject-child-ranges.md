# Subject range consolidation and child subranges

Total curated selectors: 30,400 → 29,150 (1,250 fewer). These totals include the new child ranges and changes to existing child selectors.

| Subject | Direct selectors before | After |
|---|---:|---:|
| Medicine (General) | 242 | 80 |
| Internal Medicine | 357 | 342 |
| Southeast Asia | 305 | 220 |
| American | 511 | 201 |
| History | 281 | 210 |
| Poetry | 252 | 3 |
| Recreation | 423 | 219 |

Used existing medical and Southeast Asian country branches. Created children for American general criticism, literary biography, literary history by period, children’s literature studies, and sports. Poetry uses PN6099..PN6110. General medical and selected child selectors use schedule-backed numeric ranges. No priority suffixes were added.

The proposed American Poetry Studies PS301..PS326 range produced 670 unexpected assignments and was rejected in full. All other accepted changes passed the scoped routing audit. The Slavic language/literature mixture and French author criticism Cutter selectors remain unchanged, as identified in the proposal. Internal Medicine’s psychiatry overlaps and other unreviewed selector groups remain specific.

Audited 525,990 observed LCC codes across R*, DS, PS, PN and GV. 20,113 codes / 96,326 classification uses moved from general or ancestor subjects to planned destinations. 27,577 previously unmatched codes / 96,002 uses gained matches inside the accepted schedule ranges. No previously matched codes became unresolved and no unexpected subject assignments remained. Uses are not unique books.

The full cache disagreed with the independently computed baseline (11,039 cached rows differed and 11,678 baseline rows were missing). Therefore the entire cache and usage TSV were rebuilt against the staged taxonomy and full Open Library usage database, rather than merging stale cached rows. The controlled routing comparison used actual before/after matcher runs, not cached assignments. Guarded all live taxonomy tables against the snapshot, applied the migration, compared every table to staged output, and checked foreign keys and integrity. Refreshed usage counts and viewer.

Evidence: client/.test-tmp/range-subjects/, including specs.json, audit.json, before/after databases and matcher output. Migration: 20260906_consolidate_subject_child_ranges.sql.

The embedded-taxonomy revision validation test passed after installation.
