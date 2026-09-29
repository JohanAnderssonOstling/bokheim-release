# Next 25: straightforward splits and selector audit

Implemented 48 direct children across eight subjects. Only proposed groups with at least 500 direct hits were retained; 20 smaller groups remain at their parents. No author/alphabet buckets or subdivisions below country level were created.

Counts use the complete supplied July 2026 Open Library classification-usage snapshot, rerun against a fixed current taxonomy snapshot. Hits are classification uses, not unique books. Codes are distinct normalized matched notations, including call-number variations. The refreshed baseline differs from the earlier overview, particularly for American literature.

Moved 680,540 hits across 204,675 code assignments. Full before/after matcher comparison found no unrelated routing changes. Database integrity, foreign keys, one-level depth and preservation of unrelated rows passed.

| Subject | New children | Hits moved | Codes moved | Remaining direct hits | Remaining codes |
|---|---:|---:|---:|---:|---:|
| German & Related Literatures | 5 | 75,975 | 14,344 | 483 | 88 |
| Spanish | 6 | 60,200 | 16,550 | 7,436 | 1,319 |
| Italian literature | 6 | 57,101 | 17,193 | 3,717 | 858 |
| American | 3 | 419,186 | 142,008 | 1 | 1 |
| French literature outside of France | 4 | 31,323 | 9,501 | 1,355 | 412 |
| Criminology | 7 | 12,138 | 1,946 | 19,526 | 3,317 |
| India (Bharat) | 4 | 7,489 | 1,104 | 23,747 | 4,030 |
| Other regions or countries | 13 | 17,128 | 2,029 | 13,397 | 2,180 |

## Implemented children

For parents using numeric ranges, children receive clipped ranges while the parent retains its fallback. For parents whose coverage consists of exact selectors, the existing selectors were transferred and supplemented with currently observed parent-owned codes; broad new ranges were avoided so unrelated branches retain their assignments. Future imports may require additional exact selector coverage.

### German & Related Literatures

Path: Literature / By Language / Germanic & Nordic / German & Related Literatures

| Child | LCC grouping | Hits moved | Codes moved | Existing selectors transferred |
|---|---|---:|---:|---:|
| Collections | PT1100..PT1374 | 1,747 | 331 | 387 |
| Middle High German, ca. 1050–1450/1500 | PT1501..PT1695 | 1,939 | 590 | 421 |
| 1500–ca. 1700 | PT1701..PT1797 | 925 | 261 | 332 |
| 1860/70–1960 | PT2600..PT2653 | 32,809 | 4,741 | 882 |
| 1961–2000 | PT2660..PT2688 | 38,555 | 8,421 | 54 |

### Spanish

Path: Literature / By Language / Spanish

| Child | LCC grouping | Hits moved | Codes moved | Existing selectors transferred |
|---|---|---:|---:|---:|
| Poetry Collections | PQ6174.95..PQ6215 | 1,122 | 129 | 160 |
| Prose Collections | PQ6247..PQ6269 | 923 | 101 | 46 |
| 1700–ca. 1868 | PQ6500..PQ6576 | 2,554 | 414 | 315 |
| 1868–1960 | PQ6600..PQ6647 | 13,984 | 1,625 | 309 |
| 1961–2000 | PQ6650..PQ6676 | 30,389 | 8,150 | 40 |
| 2001– | PQ6700..PQ6726 | 11,228 | 6,131 | 32 |

### Italian literature

Path: Literature / By Language / Romance / Italian, Romanian & related literatures / Italian literature

| Child | LCC grouping | Hits moved | Codes moved | Existing selectors transferred |
|---|---|---:|---:|---:|
| Collections | PQ4201..PQ4263 | 1,500 | 155 | 173 |
| 1400–1700 | PQ4561..PQ4639 | 2,089 | 481 | 678 |
| 1701–1900 | PQ4675..PQ4734 | 3,462 | 413 | 875 |
| 1900–1960 | PQ4800..PQ4851 | 10,992 | 1,248 | 270 |
| 1961–2000 | PQ4860..PQ4886 | 25,469 | 7,150 | 32 |
| 2001– | PQ4900..PQ4926 | 13,589 | 7,746 | 27 |

### American

Path: Literature / By Language / English / By Region / American

| Child | LCC grouping | Hits moved | Codes moved | Existing selectors transferred |
|---|---|---:|---:|---:|
| 1900–1960 | PS3500..PS3549 | 35,805 | 7,929 | 6,657 |
| 1961–2000 | PS3550..PS3576 | 237,127 | 69,913 | 248 |
| 2001– | PS3600..PS3626 | 146,254 | 64,166 | 123 |

### French literature outside of France

Path: Literature / By Language / Romance / French & Related Literatures / Provincial, local, colonial, etc. / French literature outside of France

| Child | LCC grouping | Hits moved | Codes moved | Existing selectors transferred |
|---|---|---:|---:|---:|
| Canada | PQ3900..PQ3919.3 | 15,531 | 4,077 | 0 |
| West Indies | PQ3940..PQ3949.3 | 2,519 | 651 | 0 |
| Asia | PQ3960..PQ3979.3 | 591 | 210 | 0 |
| Africa | PQ3980..PQ3989.3 | 12,682 | 4,563 | 0 |

### Criminology

Path: Social Sciences / Criminology

| Child | LCC grouping | Hits moved | Codes moved | Existing selectors transferred |
|---|---|---:|---:|---:|
| Criminal Psychology | HV6080..HV6113 | 1,118 | 128 | 17 |
| Causes of Crime | HV6115..HV6190 | 707 | 125 | 28 |
| Victims of Crime & Victimology | HV6250..HV6250.4 | 2,964 | 426 | 18 |
| Domestic Violence & Abuse | HV6625..HV6626.7 | 3,030 | 538 | 7 |
| Property Crime | HV6635..HV6700 | 1,166 | 210 | 21 |
| Financial Crime | HV6763..HV6771 | 1,739 | 190 | 4 |
| Computer Crime | HV6772..HV6773.3 | 1,414 | 329 | 10 |

### India (Bharat)

Path: Humanities / History / History by Region / Asia / South Asia / India (Bharat)

| Child | LCC grouping | Hits moved | Codes moved | Existing selectors transferred |
|---|---|---:|---:|---:|
| 997–1761 | DS452..DS461.8 | 880 | 145 | 0 |
| British Rule, 1761–1947 | DS463..DS480.83 | 3,202 | 479 | 0 |
| 1947– | DS480.832..DS480.859 | 1,866 | 200 | 0 |
| Political & Diplomatic History | DS444..DS450 | 1,541 | 280 | 0 |

### Other regions or countries

Path: Education / Policy, Philosophy & History / History of education / Other regions or countries

| Child | LCC grouping | Hits moved | Codes moved | Existing selectors transferred |
|---|---|---:|---:|---:|
| Canada | LA410..LA419 | 718 | 108 | 0 |
| Mexico | LA420..LA430 | 821 | 100 | 0 |
| Brazil | LA555..LA559 | 961 | 78 | 0 |
| United Kingdom | LA630..LA669.5 | 2,644 | 393 | 0 |
| France | LA690..LA718 | 1,059 | 108 | 0 |
| Germany | LA720..LA779.2 | 2,318 | 337 | 0 |
| Italy | LA790..LA799 | 647 | 97 | 0 |
| Poland | LA840..LA844 | 583 | 71 | 0 |
| Spain | LA910..LA919 | 818 | 125 | 0 |
| China | LA1130..LA1134 | 2,721 | 289 | 0 |
| India | LA1150..LA1154 | 809 | 132 | 0 |
| Japan | LA1310..LA1319 | 1,846 | 135 | 0 |
| Korea | LA1330..LA1339 | 1,183 | 56 | 0 |

## Audit of the other 13 subjects

This section is an audit, not an applied migration. Selector-coverage candidates are review leads: numeric bounds and normalized Cutter prefixes were compared with existing descendants and other branches. These counts are not guaranteed rerouting totals. Missing child coverage often means that a suitable child exists but lacks LCC selectors; it does not mean a new child is needed. Semantic review groups below can overlap and must not be summed.

### 4. American

Path: Literature / Studies / Criticism / Regions & Literary Traditions / American

Current direct hits: 57,615. Existing descendant selector coverage: 0 hits / 0 codes.

- Elsewhere: 1961–2000: 15,380 hits / 2,268 codes. Examples: PS3568.O243, PS3561.I483, PS3563.I27, PS3561.R44, PS3566.A34.
- Elsewhere: 1900–1960: 3,938 hits / 555 codes. Examples: PS3511.A87, PS3545.H16, PS3537.T323, PS3511.R94, PS3545.I342.
- Individual-author literature codes (PS700..PS3626): 48,990 hits / 9,383 codes. Review work versus criticism before moving. An author number alone does not establish that a book is criticism; biographical/critical Cutters must be preserved.

### 5. Mathematics

Path: Science & Nature / Mathematics

Current direct hits: 56,679. Existing descendant selector coverage: 0 hits / 0 codes.

- Algebra & Number Theory (QA150..QA163, QA171..QA272): 20,959 hits / 3,046 codes. Existing child: transfer after resolving overlap with discrete mathematics and checking narrower descendants.
- History, Education & Reference (QA1..QA43): 24,011 hits / 2,675 codes. Existing child: separate foundations/logic codes QA8–QA10.8 and teaching/reference material before moving.

### 6. Internal Medicine

Path: Health & Medicine / Internal Medicine

Current direct hits: 55,490. Existing descendant selector coverage: 0 hits / 0 codes.

- Psychotherapy (RC475..RC489.2): 20,782 hits / 3,939 codes. Psychotherapy already exists elsewhere; reuse it after reviewing the clinical-psychiatry placement.

### 7. Film

Path: Arts & Media / Performing Arts / Film

Current direct hits: 47,155. Existing descendant selector coverage: 5,137 hits / 656 codes.

- Existing descendant: History & Criticism: 5,137 hits / 656 codes. Examples: PN1993.5.U6, PN1993.5.A1, PN1993.5.I8, PN1993.5.U65, PN1993.5.I84.
- Elsewhere: Screenplays: 482 hits / 33 codes. Examples: PN1997.A1, PN1997A1, PN1997.A1R57, PN1997.A1T69, PN1997.A1A78.

### 9. France

Path: Humanities / History / History by Region / Europe / Western Europe / France

Current direct hits: 39,457. Existing descendant selector coverage: 0 hits / 0 codes.

- Regional and local French history (DC611..DC611, DC801..DC801): 16,233 hits / 4,105 codes. Keep at France under the country-level granularity preference; do not create province/city children.

### 10. China

Path: Humanities / History / History by Region / Asia / East Asia / China

Current direct hits: 39,407. Existing descendant selector coverage: 0 hits / 0 codes.

- Chinese social life, civilization and ethnography (DS721..DS727, DS730..DS731): 9,988 hits / 402 codes. Good topical candidates if further splits are desired; these are not missing historical periods.
- Chinese provinces and regions (DS793..DS793): 10,451 hits / 214 codes. Keep at China under the country-level granularity preference.

### 11. History

Path: Arts & Media / Visual Arts / History, Theory & Education / History

Current direct hits: 38,761. Existing descendant selector coverage: 0 hits / 0 codes.

- 20th-century art and movements (N6490..N6494): 7,901 hits / 605 codes. Check existing Modern and Contemporary branches; resolve their date boundaries before moving.

### 13. Latin America

Path: Humanities / History / History by Region / Americas / Latin America

Current direct hits: 34,044. Existing descendant selector coverage: 7,618 hits / 739 codes.

- Existing descendant: Argentina: 7,237 hits / 647 codes. Examples: F3031.5, F2849, F2936, F2810, F2848.
- Existing descendant: Jamaica: 378 hits / 91 codes. Examples: F1874, F1887, F1886, F1884, F1896.N4.
- Existing descendant: Bahamas: 3 hits / 1 codes. Examples: F1657.2.

### 14. Indians of North America

Path: Humanities / History / History by Region / Americas / Indians of North America

Current direct hits: 33,927. Existing descendant selector coverage: 0 hits / 0 codes.

- Individual Indigenous peoples (E99..E99): 19,291 hits / 4,555 codes. Many codes represent named peoples, not topical subdivisions. Do not automatically generate many small children.
- Regional Indigenous history (E78..E78): 7,964 hits / 1,605 codes. Do not split into states/provinces under the current geographic limit.
- Indigenous history: special topics (E98..E98): 4,507 hits / 1,138 codes. Potential topical extraction requires individual Cutter review; the whole base number is not a single topic.

### 15. World War II

Path: Humanities / History / Historical Eras / Modern History / 20th Century / World War II | Humanities / History / Military History, Wars & Conflicts / World Wars / World War II

Current direct hits: 33,836. Existing descendant selector coverage: 0 hits / 0 codes.

- World War II special topics (D810..D810): 7,627 hits / 781 codes. Audit Cutter-by-Cutter against the 35 existing children; never move the whole base number into one topic.

### 17. Spain

Path: Humanities / History / History by Region / Europe / Southern Europe / Iberia / Spain

Current direct hits: 32,339. Existing descendant selector coverage: 0 hits / 0 codes.

- Regional and local Spanish history (DP302..DP302, DP402..DP402): 22,090 hits / 3,238 codes. Keep at Spain under the country-level granularity preference.

### 24. Specialties of internal medicine

Path: Health & Medicine / Internal Medicine / Specialties of internal medicine

Current direct hits: 29,351. Existing descendant selector coverage: 0 hits / 0 codes.

- Cardiovascular diseases (RC666..RC701.2): 12,691 hits / 1,932 codes. Cardiology already exists elsewhere in Health & Medicine. Reuse it rather than creating a duplicate.
- Clinical endocrinology (RC648..RC665.2): 4,036 hits / 818 codes. Endocrinology & Metabolism already exists; review selector transfer and cross-branch placement.

### 25. Israel & Palestine

Path: Humanities / History / History by Region / Middle East / Israel & Palestine

Current direct hits: 28,990. Existing descendant selector coverage: 0 hits / 0 codes.

- Jewish history by country and antisemitism (DS135..DS135, DS145..DS145): 6,948 hits / 751 codes. Placement issue: these extend beyond Israel/Palestine. Review the broader Jewish-history structure before moving.

## Reference and verification

- Local LCC reference: `lcc-mdsconnect-2016.sqlite3`, `classification_record` and `classification_span`. French/Italian/Spanish literature: PQ; German literature: PT; American literature: PS; education: LA; criminology: HV.
- Indian history period boundaries checked against reference document 7, pages 83–87. Psychotherapy and cardiology/endocrinology audit groupings use LCC RC475–489.2, RC666–701.2 and RC648–665.2.
- Entire usage cache was evaluated before and after with the standalone production taxonomy matcher. Every changed code was checked to move solely from its intended source parent to its new child.
- The 13 audited subjects were left unchanged. Four lower-priority leaves (English 1900–1960, French 1900–1960, Chinese 1949–2000 and nursery rhymes) were not split.
