# Low-hit etiquette and drawing child merges

Absorbed the three approved etiquette/drawing leaves into their parents. Replaced 24 child selectors with three parent ranges; removed the reversed BJ2139..BJ2137 selector. Reviewed coverage: travel etiquette BJ2137 with special topics BJ2139–BJ2156; medieval drawing NC70 and special works NC75; crayon/chalk techniques NC855–NC875 within graphic art materials.

| Child | Parent | Hits moved | Observed codes | Old selectors |
|---|---|---:|---:|---:|
| Etiquette of travel (4541) | Social usages. Etiquette (4539) | 36 | 10 | 10 |
| Medieval (6364) | History of drawing (668) | 16 | 5 | 8 |
| Crayon (8192) | Graphic art materials (666) | 70 | 18 | 6 |

Verified all 22,467 cached BJ/NC codes with the production matcher before and after. Every changed assignment maps exactly from an approved child to its parent, including path checks. No sibling assignments changed and no existing matches were lost. 33 existing codes representing 122 classification uses moved to their parents. The reviewed ranges also matched 4 previously unmatched codes representing 4 additional uses.

The scoped matcher baseline agreed with the full match cache. All rows outside the verified subclasses were preserved. Live rows were checked against the baseline before applying the migration, and the result was compared with the staged database. Foreign-key and integrity checks passed. Refreshed match cache, usage counts, and taxonomy viewer.

The three parent ranges preserve all observed former child coverage. The added matches are BJ2137M132g.Fb2008 (a travel call-number variation), BJ2156 and BJ2156.H66 (other travel topics), and NC75.I85 (a drawing work). These are classification uses, not necessarily unique books. Parent hierarchy and all unrelated subjects/selectors remain unchanged. Audit artifacts: `client/.test-tmp/low-hit-etiquette-drawing-merges/`.
