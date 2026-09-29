# Four low-hit child merges

Absorbed the four approved leaf subjects into existing parent ranges. No ranges were widened and no selectors were added.

| Child | Parent | Hits moved | Observed codes | Selectors removed |
|---|---|---:|---:|---:|
| Awards, prizes (6489) | History and criticism (4259) | 20 | 9 | 12 |
| Analog & Digital Computer Systems (12685) | Computer Science (62) | 72 | 16 | 7 |
| Advanced Computer Architectures (12686) | Computer Science (62) | 100 | 23 | 6 |
| Buildings (9065) | Archives (4700) | 56 | 9 | 3 |

Verified all 170,670 cached PQ/QA/CD codes with the production matcher before and after. Every changed assignment maps exactly from an approved child to its parent, including path checks. No sibling assignments changed, no matches were gained or lost, and total matched classification uses are unchanged. 57 codes representing 248 classification uses moved to their parents.

The scoped matcher baseline agreed with the full match cache. All rows outside the verified subclasses were preserved. Live rows were checked against the baseline before applying the migration, and the result was compared with the staged database. Foreign-key and integrity checks passed. Refreshed match cache, usage counts, and taxonomy viewer.

The other suggested literature and anesthetics merges were not applied. Audit artifacts: `client/.test-tmp/low-hit-child-merges/`.
