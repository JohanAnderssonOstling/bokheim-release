# Internal Medicine: consolidate remaining mental-health selectors

Curated selectors: 28,099 → 27,804, net reduction 295. Internal Medicine: 342 → 39 direct selectors and 12,157 remaining direct classification uses.

Removed 302 individual mental-health selectors from Internal Medicine and added seven priority-free ranges to existing subjects. Moved the atomic printed RC434.2-574 selector to Psychiatry unchanged. No subjects or parent links were created, deleted or changed.

| Existing destination | Added range |
|---|---|
| Psychiatry | RC434.2..RC574 |
| Psychotherapy | RC475..RC489.2 |
| Hypnotherapy | RC490..RC499 |
| Psychoanalysis | RC500..RC510 |
| Psychopathology | RC512..RC569.5 |
| Clinical Psychology | RC466.8..RC467.97 |
| Counseling | RC466..RC466.3 |

Clinical hypnotism codes were assigned to Hypnotherapy, rather than the historical/esoteric Hypnotism branch. Reviewed Clinical Psychology and mental-health counseling separately using their LCC captions. Existing disorder, treatment and therapy-specific selectors remain more specific and were preserved.

Audited all 42,279 observed RC code variants against frozen before/after snapshots with the production matcher. 6,268 variants / 34,443 uses moved from Internal Medicine or matching ancestors into the planned destinations and their specific children. 3,246 previously unmatched variants / 14,381 uses gained coverage within the reviewed ranges. No lost matches or unexpected routing changes. Counts represent classification uses, not unique books.

Guarded every live taxonomy table against the baseline and compared the installed result with staged data. Integrity and foreign-key checks passed. Rebuilt the full code-match cache and usage TSV, and regenerated the viewer.

Reference: local lcc-mdsconnect-2016.sqlite3 classification records. Evidence: client/.test-tmp/internal-medicine-mental-ranges/. Migration: 20260906_consolidate_internal_medicine_mental_health.sql.
