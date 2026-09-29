# Practical journalism topic extraction installed

Added eight journalism subjects with eight exact LCC selectors on 2026-09-07. **2,852 classification uses across 390 distinct codes move** from Practical Journalism, reducing its direct hits from **6,063 to 3,211**. The complete batch comparison finds no matched coverage lost or gained and no unrelated destination changes. Counts are classification uses, not unique books.

Television Journalism is beneath Broadcast Journalism. War Reporting and Sports Journalism are beneath Reporting & Correspondence. The other subjects are direct children of Practical Journalism. Each has its own selector; the parent PN4775..PN4784 range retains residual coverage.

Journalistic Editing is kept distinct from the broader Editing & Proofreading subject because PN4778 concerns editors and editing specifically within journalism. The other proposed subjects have no equivalent existing taxonomy subjects.

| Subject | Selector | Direct hits | ID |
|---|---|---:|---:|
| Reporting & Correspondence | `PN4781` | 719 | 52365 |
| Television Journalism | `PN4784.T4` | 509 | 52367 |
| Online Journalism | `PN4784.O62` | 377 | 52368 |
| Newspaper Style | `PN4783` | 326 | 52369 |
| Broadcast Journalism | `PN4784.B75` | 266 | 52366 |
| Journalistic Editing | `PN4778` | 234 | 52370 |
| War Reporting | `PN4784.W37` | 212 | 52371 |
| Sports Journalism | `PN4784.S6` | 209 | 52372 |

Validation: 32 exact-selector, descendant-Cutter and neighboring-code probes, full before/after matching, migration replay, SQLite integrity and foreign keys, graph acyclicity and parent placement. Published usage counts, matching cache and viewer are refreshed; all eight displayed hit totals, selector totals and parent routes are verified.
The installed database equals the audited staging database.

Caption evidence is retained from the local LCC reference in the [manifest](20260907-journalism-topics-manifest.json). Migration: 20260907_extract_practical_journalism_topics.sql. Working files: `/home/johan/.cache/bokheim/journalism-topics-install/`.
