# Further literature period range consolidation

LCC selectors: 37,464 → 32,380; 5,084 fewer.

| Subject | Exact selectors replaced | Range |
|---|---:|---|
| 1700-ca. 1860/70 (4245) | 3,119 | `PT1799..PT2592@2` |
| Middle High German, ca. 1050–1450/1500 (12702) | 262 | `PT1501..PT1695@2` |
| 1400–1700 (12707) | 597 | `PQ4561..PQ4639@2` |
| 1701–1900 (12708) | 789 | `PQ4675..PQ4734@2` |
| 1700–ca. 1868 (12714) | 322 | `PQ6500..PQ6576@2` |
| 17th and 18th centuries (1640-1770) (3912) | 0 | `PR3291..PR3785@2` |
| 19th century, 1770/1800-1890/1900 (3915) | 0 | `PR3991..PR5990@2` |

The broader German 1700–ca.1860/70 interval retains the existing higher-priority PT1891–PT2239 range. Anonymous-work selectors remain intact. PT1526, PQ4708 and PQ4715 had already been removed from broad language parents by the concurrent cleanup; this pass preserved that state. The two English subjects were already range-only in the refreshed baseline; their range priorities were aligned with the other literature periods. No subjects or parent links changed.

Verified all 279,742 cached PR/PQ/PT codes with the production matcher before and after. No lost matches or unexpected routing changes. 1,493 code assignments changed; 141 previously unmatched codes gained routing (397 classification uses, not unique books).

The scoped baseline matched the live full cache; all out-of-scope cache rows were preserved. Applied migration only after a row-for-row live baseline check, then compared the result to the staged database. Foreign-key and integrity checks passed. Refreshed full usage counts and viewer.

Evidence: `client/.test-tmp/selector-range-consolidation-2/`.
