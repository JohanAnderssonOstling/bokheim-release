# Three low-hit literature child merges

Absorbed the three approved literature leaves into their parents. Replaced nine child selectors with three parent ranges covering the same numeric intervals.

| Child | Parent | Hits moved | Observed codes | Old selectors |
|---|---|---:|---:|---:|
| Literary history and criticism (3932) | Faroese literature (3931) | 20 | 6 | 3 |
| Folk literature (4475) | Swedish literature (4473) | 10 | 6 | 3 |
| Folk literature (3902) | Dutch literature (3899) | 10 | 6 | 3 |

Verified all 40,871 cached PT codes with the production matcher before and after. Every changed assignment maps exactly from an approved child to its parent, including path checks. No sibling assignments changed, no matches were gained or lost, and total matched classification uses are unchanged. 18 codes representing 40 classification uses moved to their parents.

The scoped matcher baseline agreed with the full match cache. All rows outside the verified subclasses were preserved. Live rows were checked against the baseline before applying the migration, and the result was compared with the staged database. Foreign-key and integrity checks passed. Refreshed match cache, usage counts, and taxonomy viewer.

The three new parent ranges preserve the former child coverage. Parent hierarchy and all unrelated subjects/selectors remain unchanged. Audit artifacts: `client/.test-tmp/low-hit-literature-merges/`.
