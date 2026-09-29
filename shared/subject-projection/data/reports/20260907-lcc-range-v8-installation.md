# Complete LCC range endpoints implemented

Taxonomy format 8 stores `start_code`, `end_code`, and `concept_id`. Migration preserved all 14,225 ranges present at the start of this task, including endpoint text and ownership. Other concurrent taxonomy additions and refinements were retained. Release 14 includes the two reviewed broad intervals `D1..DX301` (History, 3319) and `R..RZ` (Medicine, 2725).

The parser accepts cross-subclass, Cutter, abbreviated-endpoint and letter-only spans, including typographic dashes. Matching requires one stored interval to contain the complete incoming range. Upper endpoint filing families are respected. Exact printed-range assignments retain priority; disjoint selectors cannot bridge a gap. The legacy outline projection also checks the whole span rather than matching its start.

The reader remains compatible with format 7. Writable device caches migrate before server-overlay merge; new client seeds use format 8. Historical malformed stored bounds retain their old interpretation; new persisted ranges are strictly validated. Unified matcher version is 29 and outline matcher version is 3.

Validation: all 56 library tests pass for both authoritative and client-seed builds. Tests cover cross-subclass matching, intermediate subclasses, unknown subclasses, full containment, numeric and Cutter boundaries, reversed spans, letter-only bounds, explicit selectors, format-7 cache upgrades, endpoint round trips and unique range ownership. Seven installed-database probes pass, including both new broad mappings, typographic dashes, negative boundaries and the existing QA75–76 match. SQLite integrity and foreign keys pass. Counts, matching cache and viewer were regenerated from a frozen snapshot and the snapshot was compared with the current database before publication.

Two existing path-based tests were updated for concurrently renamed/subdivided subjects. A concurrent duplicate Marketing parent/child label prevented all runtime loading; the child label was restored to its previous distinct General Marketing label.

The first whole-cache comparison recovered 54,532 classification uses but also reflected concurrent coverage edits. These totals are not attributable solely to the range feature. Every newly unmatched code was checked using the new matcher against prior taxonomy snapshots and still matched there, identifying those losses as taxonomy-data differences. Confirmed newly supported examples include R–RZ, D1–DX301, QA273.A1–274.9, R850.A1–854 and TA455.P58–.P585. Remaining selector gaps and catalog markers are outside this schema/parser change.

Final observed LCC totals (classification uses, not unique books):

| Result | Distinct codes | Uses |
|---|---:|---:|
| ambiguous | 13 | 13 |
| matched | 10,279,126 | 14,188,495 |
| no_selector | 121,492 | 219,815 |
| rejected_format | 915,999 | 1,267,842 |

See [range format documentation](../LCC-RANGE-FORMAT.md). Working snapshots, full audit outputs and test logs: `/home/johan/.cache/bokheim/lcc-range-v8/`.
