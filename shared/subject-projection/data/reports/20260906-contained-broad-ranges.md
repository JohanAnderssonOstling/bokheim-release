# Contained broad-range consolidation

Removed 488 LCC selectors. Total selectors: 35,996 → 35,508. LCC selectors: 30,878 → 30,390.

| Subject | Ranges in reviewed block before | After | Range retained |
|---|---:|---:|---|
| Jewish law. Halakah | 78 | 1 | KBM1..KBM4855 |
| Numismatics | 75 | 1 | CJ1..CJ6661 |
| History of canon law | 71 | 1 | KBR2..KBR4090 |
| Latin America | 71 | 1 | F1201..F3799 |
| France | 70 | 1 | DC1..DC947 |
| South Africa | 69 | 1 | DT1701..DT2405 |
| Catholic Canon Law | 61 | 1 | KBU2..KBU4820 |

Practical Theology was skipped: a concurrent edit had already replaced its numeric ranges with the selector BV before the audit snapshot.

All seven enclosing ranges already existed on the same subjects. Only wholly contained numeric ranges were removed. Exact selectors, other ranges, subject labels and parent links were unchanged. This does not fill gaps or expand numeric coverage.

Compared actual before/after production-matcher output for all 130,893 observed BV, KBM, CJ, KBR, F, DC, DT and KBU code variants, including concept IDs, paths and usage counts. Every assignment remained identical. No losses, new matches, moved hits or unresolved assignments were introduced. No narrower exception ranges needed to be restored. This validates observed codes; unknown future codes can still reveal specificity conflicts with unrelated subjects.

Applied after guarding all live taxonomy tables against the audited snapshot, then compared all installed tables with staged data. Integrity and foreign-key checks passed. Rebuilt the full match cache and usage counts and regenerated the viewer. Evidence: client/.test-tmp/contained-broad-ranges/. Migration: 20260906_consolidate_contained_broad_ranges.sql.
