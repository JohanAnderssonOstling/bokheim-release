# Separate Slavic, Baltic and Albanian languages from literature

Curated selectors: 29,112 → 28,382, a net reduction of 730. The Slavic, Baltic & Albanian parent drops from 716 to 2 selectors (PG and PG799.2).

Reused Czech, Polish, Russian, Serbian & Croatian, Slavic Languages and Baltic Languages subjects. Added the missing language subjects under the appropriate language family. Distributed seven generic Language/Philology buckets and removed the empty nodes. Old Prussian retained its ID and moved from literature to Baltic Languages. Split mixed Lithuanian and Latvian ranges into language and literature, renaming the retained literature subjects accordingly. Existing Bulgarian, Czech, Slovak and Polish literature subjects were reused; Serbian & Croatian Literature was added.

| Destination | Type | Range |
|---|---|---|
| Slavic Languages (3742) | language | PG1..PG499 |
| Old Church Slavic (13645) | language | PG601..PG699 |
| Middle Bulgarian (13646) | language | PG771..PG799 |
| Bulgarian (13647) | language | PG801..PG999 |
| Macedonian (13648) | language | PG1151..PG1179 |
| Serbian & Croatian (3741) | language | PG1201..PG1399 |
| Slovenian (13649) | language | PG1801..PG1899 |
| Russian (3740) | language | PG2001..PG2827 |
| Belarusian (13650) | language | PG2830..PG2834.12 |
| Ukrainian (13651) | language | PG3801..PG3899.5 |
| Czech (3717) | language | PG4001..PG4840 |
| Slovak (13652) | language | PG5201..PG5393 |
| Sorbian (13653) | language | PG5631..PG5659.42 |
| Polish (3738) | language | PG6001..PG6840 |
| Baltic Languages (3714) | language | PG8001..PG8099 |
| Lithuanian (13654) | language | PG8501..PG8693 |
| Latvian (13655) | language | PG8801..PG8993 |
| Latvian (13655) | language | PG8995..PG8997 |
| Albanian (13656) | language | PG9501..PG9599 |
| Slavic (4468) | literature | PG500..PG585.2 |
| Serbian & Croatian Literature (13657) | literature | PG1400..PG1749 |
| Bulgarian Literature (12519) | literature | PG1000..PG1146 |
| Czech literature (6417) | literature | PG5000..PG5146 |
| Slovak Literature (12524) | literature | PG5400..PG5546 |
| Polish Literature (12525) | literature | PG7001..PG7446 |
| Lithuanian (6284) | literature | PG8700..PG8772 |
| Latvian (7609) | literature | PG8998..PG9146 |

Validated all 52,444 observed PG code variants using the same current matcher on before/after snapshots. 3,283 code variants / 19,033 classification uses moved to planned destinations, including 12,891 uses moving to language subjects. 6,788 previously unmatched variants / 22,239 uses gained coverage within the reviewed LCC ranges. No lost matches or unexpected destination changes. Classification uses are not distinct books.

The first audit found PG8996.Y68 would lose its match. The local LCC schedule identifies PG8995–PG8997 as Latvian slang and argot, so that separate language subrange was added before the successful final audit. The root PG799.2 selector remains for Middle Bulgarian literature.

Used current format-5 schema and rebuilt matcher; no schema or runtime changes were made by this task. Guarded all live tables against the snapshot, applied the migration, compared all tables against staged output, and checked integrity and foreign keys. Rebuilt the complete code-match cache and subject hit counts from the full Open Library usage database. Refreshed the taxonomy viewer.

Reference: local lcc-mdsconnect-2016.sqlite3 classification records. Evidence: client/.test-tmp/slavic-language-ranges/. Migration: 20260906_separate_slavic_languages_literature.sql.
