# Contained short-range consolidation

Removed 151 LCC selectors. Total selectors: 36,703 → 36,552. LCC selectors: 31,585 → 31,434.

| Subject | Ranges in reviewed block before | After | Range retained |
|---|---:|---:|---|
| Venice | 26 | 1 | DG670..DG684.72 |
| Naples. Kingdom of the Two Sicilies | 24 | 1 | DG840..DG857.52 |
| Tuscany. Florence | 24 | 1 | DG731..DG759.3 |
| Rome (Modern city) | 23 | 1 | DG803..DG817.32 |
| Milan. Lombardy | 19 | 1 | DG651..DG664.5 |
| Sudan. Anglo-Egyptian Sudan | 15 | 1 | DT154.1..DT159.9 |
| Algeria | 14 | 1 | DT271..DT299 |
| Soils. Soil science | 14 | 1 | S590..S599.9 |

All eight enclosing ranges already existed on the same subjects. Only wholly contained numeric ranges were removed. Exact selectors, other ranges, subject labels and parent links were unchanged. This does not fill gaps or expand numeric coverage.

Compared actual before/after production-matcher output for all 72,205 observed DG, DT and S* code variants, including concept IDs, paths and usage counts. Every assignment remained identical. No losses, new matches, moved hits or unresolved assignments were introduced. No narrower exception ranges needed to be restored. This validates observed codes; unknown future codes can still reveal specificity conflicts with unrelated subjects.

Applied after guarding all live taxonomy tables against the audited snapshot, then compared all installed tables with staged data. Integrity and foreign-key checks passed. Rebuilt the full match cache and usage counts and regenerated the viewer. Evidence: client/.test-tmp/contained-short-ranges/. Migration: 20260906_consolidate_contained_short_ranges.sql.
