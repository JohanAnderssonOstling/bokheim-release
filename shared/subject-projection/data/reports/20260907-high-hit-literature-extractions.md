# High-hit literature selector extractions

Extracted 33 LCC ranges into 22 new subjects and 5 existing destinations. The existing destinations are Brazilian Literature, Turkish Literature, Manga (the general comics subject, ID 614), Chinese Poetry, and Literature & the Arts. Broader selectors retain their general and residual coverage.

The complete July 31, 2026 Open Library classification cache was evaluated with the current Rust matcher before and after the migration. Counts describe classification uses, not unique books.

- 102,730 uses across 21,506 distinct scheme/code pairs move to the reviewed destinations.
- No previously matched code or use becomes unmatched; no additional coverage is introduced.
- 132 selector, Cutter, and boundary checks pass.
- Migration replay matches the staged database. Integrity, foreign keys, graph acyclicity, and reachability from Literature pass.
- Existing author-era shelves are retained. No named authors or alphabetical filing groups are added.
- Chinese genre subjects receive both studies and collections without making collections children of a studies-only subject.
- Galician and Bosnian literature receive appropriate European/Slavic routes rather than inheriting the narrower labels of their old catch-all subjects.

| Source | Destination | Uses moved | Destination ID |
|---|---|---:|---:|
| Portuguese Literature Abroad | Brazilian Literature | 20,824 | 4160 |
| Turkic languages | Turkish Literature | 12,900 | 4231 |
| Other National Comics | Manga | 9,892 | 614 |
| Serbian & Croatian Literature | Croatian Literature | 6,294 | 51866 |
| Chinese Literary Studies | Chinese Poetry | 6,233 | 4426 |
| Collections & Translations | Chinese Classics | 5,477 | 51871 |
| Other National Comics | South Korean Comics | 3,976 | 51865 |
| Other National Theater | Chinese Theater | 3,061 | 51873 |
| Other National Theater | Japanese Theater | 2,943 | 51874 |
| Chinese Literary Studies | Chinese Fiction | 2,846 | 51869 |
| Portuguese Literature Abroad | Galician Literature | 2,709 | 51864 |
| Collections & Translations | Chinese Poetry | 2,437 | 4426 |
| Other National Theater | Russian Theater | 2,344 | 51872 |
| Collections & Translations | Chinese Fiction | 2,067 | 51869 |
| Collections & Translations | Chinese Essays | 2,006 | 51870 |
| Special Elements & Subjects | Literature, Philosophy & Religion | 1,745 | 51884 |
| Other National Journalism | Chinese Journalism | 1,614 | 51880 |
| Serbian & Croatian Literature | Bosnian Literature | 1,385 | 51867 |
| Special Elements & Subjects | Literature & Society | 1,284 | 51885 |
| Other National Journalism | Spanish Journalism | 1,284 | 51883 |
| Other National Journalism | Italian Journalism | 1,237 | 51878 |
| Chinese Literary Studies | Chinese Drama | 1,141 | 51868 |
| Other National Journalism | Japanese Journalism | 1,123 | 51882 |
| Other National Journalism | Russian Journalism | 1,041 | 51879 |
| Other National Theater | Indian Theater | 968 | 51875 |
| Other National Theater | Spanish Theater | 928 | 51876 |
| Other National Theater | Scandinavian Theater | 754 | 51877 |
| Chinese Literary Studies | Chinese Essays | 732 | 51870 |
| Other National Journalism | Indian Journalism | 616 | 51881 |
| Special Elements & Subjects | Literature & the Arts | 461 | 4590 |
| Collections & Translations | Chinese Drama | 408 | 51868 |

## Extracted selectors

| Selector | Destination | Evidence |
|---|---|---|
| `PQ9500..PQ9698.436` | Brazilian Literature | Reference: Brazil |
| `PQ9450..PQ9469.3` | Galician Literature | Reference: Galicia |
| `PL201..PL271` | Turkish Literature | Reference: Literature |
| `PN6790.J3..PN6790.J349` | Manga | LOC PN 2025 p. 206 legacy country table (.x, .x2, .x3, .x4); LOC catalog PN6790.J3 Japanese manga |
| `PN6790.K6..PN6790.K649` | South Korean Comics | LOC December 2022 editorial decision: PN6790.K6 is South Korea; legacy country table (.x through .x4) |
| `PG1600..PG1696` | Croatian Literature | Reference: Croatian literature |
| `PG1700..PG1749` | Bosnian Literature | Reference: Bosnian literature |
| `PL2306..PL2333` | Chinese Poetry | Reference: Poetry |
| `PL2336..PL2353` | Chinese Poetry | Reference: Ci (Tzʻu) |
| `PL2517..PL2543` | Chinese Poetry | Reference: Poetry |
| `PL2548..PL2563` | Chinese Poetry | Reference: Ci (Tzʻu) |
| `PL2356..PL2393` | Chinese Drama | Reference: Drama |
| `PL2566..PL2603` | Chinese Drama | Reference: Drama |
| `PL2415..PL2443` | Chinese Fiction | Reference: Fiction |
| `PL2625..PL2653` | Chinese Fiction | Reference: Fiction |
| `PL2395..PL2413` | Chinese Essays | Reference: Essay |
| `PL2606..PL2623` | Chinese Essays | Reference: Essays |
| `PL2458..PL2489.6` | Chinese Classics | Reference: Confucian Canon. The Chinese Classics |
| `PN2720..PN2728` | Russian Theater | Reference: Russia. Soviet Union. Russia (Federation) |
| `PN2870..PN2878` | Chinese Theater | Reference: China |
| `PN2920..PN2928` | Japanese Theater | Reference: Japan |
| `PN2880..PN2888` | Indian Theater | Reference: India |
| `PN2780..PN2788` | Spanish Theater | Reference: Spain |
| `PN2730..PN2778` | Scandinavian Theater | Reference: Scandinavia |
| `PN5241..PN5250` | Italian Journalism | Reference: Italy |
| `PN5271..PN5280` | Russian Journalism | Reference: Russia |
| `PN5361..PN5370` | Chinese Journalism | Reference: China |
| `PN5371..PN5380` | Indian Journalism | Reference: India |
| `PN5401..PN5410` | Japanese Journalism | Reference: Japan |
| `PN5311..PN5320` | Spanish Journalism | Reference: Spain |
| `PN49..PN49` | Literature, Philosophy & Religion | Reference: Philosophy, ethics, religion, etc. |
| `PN51..PN51` | Literature & Society | Reference: Relation to sociology, economics, political science, etc. (social ideals, forces, etc. in literature) |
| `PN53..PN53` | Literature & the Arts | Reference: Relation to art |

## Sources and validation

Most spans were verified against the repository classification reference `lcc-mdsconnect-2016.sqlite3`. The historical comics country subdivisions are documented on page 206 of the [2025 Library of Congress PN schedule](https://www.loc.gov/aba/publications/FreeLCC/LCC_PN2025TEXT.pdf). That schedule retains the legacy structure for interpreting old catalog codes, although new records now use later numbers.

Japanese manga is confirmed by the [LOC catalog record at PN6790.J3](https://www.loc.gov/item/2024007247/). South Korea at PN6790.K6 is confirmed by the [December 2022 LOC editorial decision](https://loc.gov/aba/pcc/saco/cpsoed/ptcp-221216.pdf). The country ranges cover their legacy history, collections, author, and title subdivisions without creating individual-author subjects.

Full comparison artifacts, migration replay, and probe inputs are retained in `/tmp/literature-extraction/`. The usage TSV and taxonomy viewer are refreshed against the installed state. Installation remaps new IDs transactionally and preserves unrelated concurrent taxonomy changes.
