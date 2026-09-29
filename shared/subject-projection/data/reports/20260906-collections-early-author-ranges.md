# Further collections and early-author range consolidation

LCC selectors: 32,380 → 28,321; 4,059 fewer.

| Subject | Exact selectors replaced | Range |
|---|---:|---|
| Collections (12701) | 392 | `PT1100..PT1374@2` |
| 1500–ca. 1700 (12703) | 319 | `PT1701..PT1797@2` |
| Collections (12706) | 173 | `PQ4201..PQ4263@2` |
| Ancient Greek Authors (4255) | 526 | `PA3818..PA4500@0` |
| Spanish (4470) | 778 | `PQ6271..PQ6498@2` |
| Spanish (4470) | 32 | `PQ6170..PQ6269@2` |
| American (3853) | 0 | `PS700..PS893@2` |
| American (3853) | 37 | `PS991..PS3390@2` |

Specific Greek subsubjects and the higher-priority PA4033 selector remain intact. General early American author coverage moved from criticism into the existing American literature subject. No subjects or parent links changed.

Verified all 371,155 cached PA/PQ/PT/PS codes with the production matcher before and after. No lost matches or unexpected routing changes. 5,283 code assignments changed; 3,067 previously unmatched codes gained routing (10,091 classification uses, not unique books).

The scoped baseline matched the live full cache; all out-of-scope cache rows were preserved. Applied migration only after a row-for-row live baseline check, then compared the result to the staged database. Foreign-key and integrity checks passed. Refreshed full usage counts and viewer.

Evidence: `client/.test-tmp/selector-range-consolidation-3/`.
