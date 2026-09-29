# Literature and clinical selector range consolidation — 2026-09-06

Replaced observed exact-call enumeration with numeric LCC ranges in 16 subjects. Consolidated overlapping American period coverage and moved broad-parent English and Italian author-class coverage into the appropriate periods.

LCC selectors: **315,999 → 118,745** (net reduction **197,254**).

| Subject ID | Label | Numeric selector | Original exact selectors in interval |
|---|---|---|---:|
| 12719 | 1961–2000 | `PS3550..PS3576@2` | 69,930 |
| 12720 | 2001– | `PS3600..PS3626@2` | 64,205 |
| 12718 | 1900–1960 | `PS3500..PS3549@2` | 12,658 |
| 12705 | 1961–2000 | `PT2660..PT2688@2` | 8,428 |
| 12704 | 1860/70–1960 | `PT2600..PT2653@2` | 5,256 |
| 12716 | 1961–2000 | `PQ6650..PQ6676@2` | 8,157 |
| 12717 | 2001– | `PQ6700..PQ6726@2` | 6,135 |
| 12715 | 1868–1960 | `PQ6600..PQ6647@2` | 1,761 |
| 12710 | 1961–2000 | `PQ4860..PQ4886@2` | 7,160 |
| 12711 | 2001– | `PQ4900..PQ4926@2` | 7,753 |
| 12709 | 1900–1960 | `PQ4800..PQ4851@2` | 1,371 |
| 5431 | Psychoanalysis | `RC500..RC510@4` | 489 |
| 5651 | Clinical Psychology | `RC466.8..RC467.97@4` | 155 |
| 3913 | 1900-1960 | `PR6000..PR6049@2` | 2,497 |
| 3914 | 1961-2000 | `PR6050..PR6076@2` | 290 |
| 3916 | 2001- | `PR6100..PR6126@2` | 73 |

Clinical Psychology retains the exact printed span `RC466.8-467.97`; its 826 usage hits cannot route through a numeric range. Psychoanalysis retains its separate BF selectors.

Merged subject 3854 (2001-, formerly under Prose) into 12720 (American literature / 2001–), preserving its source labels and moving Anonymous works (6515) to that period. Other anonymous-work exceptions remain intact.

Removed 871 American criticism exact selectors inside PS3500–PS3549 and PS3550–PS3576: these identify authors and works generally, not exclusively criticism. Removed PQ4804 from the Italian parent and 11 PR period selectors from the English parent. Their numeric coverage now belongs to the corresponding period children.

Psychotherapy and geographic Cutter intervals were not changed. Numeric range endpoints do not model Cutter suffixes.

## Validation

Ran the production taxonomy_usage matcher over the complete cached Open Library usage database before and after the initial change. Rechecked all 157,325 PR/RC usage codes after resolving the English overlaps and retaining the clinical printed span, then merged those results and compared the complete outputs again.

Final comparison: 21,820 code assignments changed (83,726 classification uses); 10,672 previously unmatched codes gained routing (29,335 uses). No previously matched codes lost routing; no unexpected subject assignment changes. All changed destinations are within the reviewed numeric intervals. The 41 path-only changes belong to the preserved Anonymous works child.

Counts represent classification uses, not unique books. New coverage is expected from replacing observed exact selectors with schedule intervals.

The compact SQL migration was replayed against the baseline and compared row-for-row with the staged database. Installation required an unchanged live baseline; foreign-key and integrity checks passed. Usage counts, match cache, and taxonomy viewer were refreshed.

Audit artifacts: `client/.test-tmp/selector-range-consolidation/` (before/after database snapshots, matcher outputs, comparison database, routing-audit.json, completion.json).
