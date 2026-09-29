# Mathematics: selectors transferred to existing children

Transferred 61,284 direct hits across 6,216 distinct normalized code assignments into 20 existing subjects. No subjects were created and no parent links were changed. Mathematics retains 8,960 direct hits across 1,314 codes.

The current full-cache baseline was verified against the real matcher for all QA codes. Its Mathematics totals differ from the older overview. Counts are classification uses, not unique books.

| Existing branch receiving hits | Hits moved | Codes moved |
|---|---:|---:|
| History, Education & Reference | 35,278 | 2,312 |
| Algebra & Number Theory | 16,452 | 2,444 |
| Foundations, Logic & Discrete Mathematics | 8,345 | 1,257 |
| Probability, Statistics & Optimization | 762 | 130 |
| Applied & Computational Mathematics | 447 | 73 |

## Individual destinations

| Existing subject | Hits moved | Codes moved | Stored selectors transferred |
|---|---:|---:|---:|
| History, Education & Reference | 29,948 | 1,246 | 36 |
| Algebra | 3,957 | 518 | 24 |
| Discrete Mathematics | 3,501 | 449 | 31 |
| Number Theory | 3,351 | 524 | 21 |
| Abstract | 3,018 | 377 | 12 |
| Study & Teaching | 2,986 | 730 | 41 |
| Elementary | 2,207 | 253 | 3 |
| Logic | 2,058 | 334 | 28 |
| History & Philosophy | 2,054 | 310 | 9 |
| Linear | 1,494 | 245 | 13 |
| Group Theory | 1,472 | 343 | 20 |
| Combinatorics | 1,431 | 258 | 14 |
| Set Theory | 1,336 | 212 | 7 |
| Matrices | 842 | 161 | 7 |
| Game Theory | 762 | 130 | 4 |
| Numerical Analysis | 426 | 68 | 3 |
| Essays | 290 | 26 | 1 |
| Number Systems | 111 | 23 | 2 |
| Graphic Methods | 21 | 5 | 1 |
| Foundations, Logic & Discrete Mathematics | 19 | 4 | 1 |

## Overlap decisions

- QA8.9–QA10.35 goes to Logic. Mathematical philosophy uses the existing History & Philosophy subject; general information theory QA10.4 uses Foundations, Logic & Discrete Mathematics.
- QA10.92–QA20 goes to Study & Teaching. QA21–QA27 goes to History & Philosophy. Reference, textbooks, directories, biographies and collected works use History, Education & Reference; QA7 goes to Essays. General comprehensive mathematics QA36 stays at Mathematics.
- QA164–QA167.2 is combinatorics, with graph theory QA166–QA166.249 assigned to Discrete Mathematics. QA171.48–QA171.5 and QA248–QA248.5 go to Set Theory.
- Group theory, elementary/abstract/linear algebra, matrices and number theory use their existing subjects. QA221–QA224 goes to Numerical Analysis; QA267–QA268.5 to Discrete Mathematics; QA269–QA272 to Game Theory.
- Only Mathematics-owned selectors and observed code assignments were transferred. Existing assignments in other branches were preserved. There are no new broad ranges that could override unrelated subjects. Future imports may require additional exact-code coverage.

## Verification

All 43,303 QA input codes were evaluated before and after with the standalone taxonomy matcher. Every changed assignment moved from Mathematics to its intended existing descendant. All other cached groups and paths were preserved. Database integrity, foreign keys, unchanged hierarchy, usage output and viewer checks passed.

Source: local LCC reference classification_record/classification_span, QA1–QA43 and QA150–QA272, in lcc-mdsconnect-2016.sqlite3.
