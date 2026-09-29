# High-hit technology selector extractions

Applied 30 LCC range assignments, transferring the construction-management range and a Japanese-architecture exact exception. Created 25 subjects and reused 5 existing destinations. Existing broader ranges retain general and residual coverage.

The complete July 31, 2026 Open Library classification cache was run through the current Rust matcher before and after the migration. Counts are classification uses, not unique books.

- 79,629 uses across 17,873 distinct scheme/code pairs moved to more specific subjects.
- No previously matched codes or uses became unmatched; no new coverage was introduced.
- All assignment changes lead to reviewed destinations; 31 selector/Cutter/boundary checks pass.
- Migration replay, database integrity, foreign keys, graph acyclicity, and reachability from Applied Sciences & Technology pass.
- The viewer now includes separately stored LCC ranges in selector totals and classification ordering.

| Source | Destination | Uses moved | Destination ID |
|---|---|---:|---:|
| Special automobiles. By power | Car Makes & Models | 8,790 | 51695 |
| Architectural History Overview | U.S. Architecture | 7,586 | 51701 |
| Artificial Intelligence | Computational Intelligence | 5,987 | 51697 |
| Architectural History Overview | Italian Architecture | 5,908 | 51711 |
| Dogs | Breeds | 5,362 | 3152 |
| Architectural History Overview | German Architecture | 5,162 | 51710 |
| Computer networks | Internet | 4,941 | 103 |
| Architectural History Overview | British Architecture | 3,731 | 51705 |
| Apparatus and materials | Semiconductors | 3,529 | 272 |
| Architectural History Overview | French Architecture | 3,328 | 51709 |
| Computer networks | Web Software | 3,107 | 51696 |
| Architecture | Architectural Theory & Philosophy | 2,528 | 51698 |
| Computer networks | Network Security | 2,053 | 165 |
| Technology & Engineering | Nanotechnology & MEMS | 2,007 | 409 |
| Architectural History Overview | Dutch Architecture | 1,524 | 51712 |
| Architectural History Overview | Swiss Architecture | 1,354 | 51717 |
| Architectural History Overview | Russian Architecture | 1,330 | 51714 |
| Architectural History Overview | Austrian Architecture | 1,168 | 51706 |
| Architecture | Architectural CAD | 1,136 | 51699 |
| Architectural History Overview | Mexican Architecture | 1,098 | 51703 |
| Building construction | Construction Site Management | 1,090 | 51700 |
| Architectural History Overview | Scandinavian Architecture | 1,065 | 51715 |
| Architecture | Japanese Architecture | 1,003 | 51719 |
| Architectural History Overview | Brazilian Architecture | 796 | 51704 |
| Architectural History Overview | Japanese Architecture | 778 | 51719 |
| Architectural History Overview | Portuguese Architecture | 684 | 51716 |
| Architectural History Overview | Indian Architecture | 673 | 51718 |
| Architectural History Overview | Belgian Architecture | 574 | 51713 |
| Architectural History Overview | Canadian Architecture | 550 | 51702 |
| Architectural History Overview | Czech & Czechoslovak Architecture | 444 | 51708 |
| Architectural History Overview | Hungarian Architecture | 343 | 51707 |

## Reviewed selectors

| Selector | Destination | Evidence |
|---|---|---|
| `TL215..TL215` | Car Makes & Models | Reference TL215 brand/model Cutter records |
| `TK5105.888..TK5105.8885` | Internet | Reference World Wide Web span |
| `TK5105.8885..TK5105.8885` | Web Software | Reference named Web software Cutter records |
| `Q342..Q342` | Computational Intelligence | Reference Q342 Computational intelligence |
| `SF429..SF429` | Breeds | Reference SF429 dog-breed Cutter records |
| `TK7871.85..TK7871.99` | Semiconductors | Reference Semiconductors span |
| `NA2500..NA2500` | Architectural Theory & Philosophy | Reference NA2500 Theory. Philosophy |
| `TK5105.59..TK5105.59` | Network Security | Reference TK5105.59 Computer network security |
| `T174.7..T174.7` | Nanotechnology & MEMS | Reference T174.7 Nanotechnology |
| `NA2728..NA2728` | Architectural CAD | Reference NA2728 Data processing. Computer-aided design |
| `TH438..TH438.4` | Construction Site Management | Reference Management of the construction site span |
| `NA705..NA738` | U.S. Architecture | Reference regional span: United States |
| `NA740..NA749.5` | Canadian Architecture | Reference regional span: Canada |
| `NA750..NA759` | Mexican Architecture | Reference regional span: Mexico |
| `NA850..NA859` | Brazilian Architecture | Reference regional span: Brazil |
| `NA961..NA997` | British Architecture | Reference regional span: Great Britain. England |
| `NA1001..NA1011.6` | Austrian Architecture | Reference regional span: Austria |
| `NA1012..NA1022.6` | Hungarian Architecture | Reference regional span: Hungary |
| `NA1023..NA1034.5` | Czech & Czechoslovak Architecture | Reference regional span: Czechoslovakia. Czech Republic |
| `NA1041..NA1053.3` | French Architecture | Reference regional span: France |
| `NA1061..NA1088.3` | German Architecture | Reference regional span: Germany |
| `NA1111..NA1123.3` | Italian Architecture | Reference regional span: Italy |
| `NA1141..NA1153.3` | Dutch Architecture | Reference regional span: Holland (Netherlands) |
| `NA1161..NA1173.3` | Belgian Architecture | Reference regional span: Belgium |
| `NA1181..NA1199` | Russian Architecture | Reference regional span: Russia |
| `NA1201..NA1293.3` | Scandinavian Architecture | Reference regional span: Scandinavia |
| `NA1321..NA1333.3` | Portuguese Architecture | Reference regional span: Portugal |
| `NA1341..NA1353.3` | Swiss Architecture | Reference regional span: Switzerland |
| `NA1501..NA1510.3` | Indian Architecture | Reference regional span: India |
| `NA1550..NA1559.6` | Japanese Architecture | Reference regional span: Japan |

Evidence: repository `lcc-mdsconnect-2016.sqlite3` classification records and the [Library of Congress architecture outline](https://www.loc.gov/catdir/cpso/lcco/lcco_n.pdf). The exact `NA1559.A5` exception is transferred to Japanese Architecture as well. The full Japanese span also moves 1,003 uses previously assigned to Architecture.

Validation commands and full comparison artifacts are retained in `/tmp/technology-extraction/`. The published usage TSV and viewer were regenerated against the installed database after preserving concurrent history edits.
