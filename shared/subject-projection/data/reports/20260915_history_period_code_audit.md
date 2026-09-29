# History period/code audit

Compared 1,186 History descendants containing digits with the bundled master
classification. This includes dated events and topical subjects, not just period
headings. The findings below record the initial audit; the CSV has been refreshed
after applying the fixes described here.

## Fixes applied

Applied `20260915_fix_history_period_code_support.sql` to the curated database:

- Corrected all seven concrete discrepancies below to match their code dates.
  Used neutral date labels for 10138, 10391, 10428, 10460 and 51992 so that an
  era name does not imply a narrower historical interval than the code supports.
- Extended parent 10199 to 1918–1993 to contain its corrected child period.
- Restored DG532–DG537.8 to 3569 and E351–E364.9 to 3693 from exact master ranges.
- Restored E839.5–E839.8, E840.4 and E840.6–E840.8 to 51005–51007 respectively.
  Renamed all three topical period leaves to 1961–2000, matching the master
  later-twentieth-century scope and avoiding repeated parent themes.
  The biography range is additionally supported by the
  [Library of Congress E–F outline](https://www.loc.gov/aba/publications/Archived-LCC2022/LCC_E-F2022OUT.pdf).
- Preserved subject IDs, parent links, existing classification assignments and
  original labels as searchable aliases.

Validated on a backup before application and reran the migration to check
idempotence: 11 label changes, five added ranges, no removed ranges, no missing
old-label aliases, no new sibling-name collisions, and clean integrity and
foreign-key checks. The refreshed audit has no uncoded dated leaf/subtree.
The other screening candidates remain unverified; this fix addresses the
confirmed discrepancies and five empty subjects, not every heuristic flag.

## Concrete discrepancies

| Subject ID | Curated label | Assigned LCC range | Master heading |
|---|---|---|---|
| 10138 | Union with Norway, 1814–1905 | DL807–DL859 | 1814–1907. 19th century |
| 10391 | First Republic, 1918–1938 | DB2195–DB2202 | 1918–1939 |
| 10428 | Federal Era, 1968–1989 | DB2835–DB2842 | 1968–1992 |
| 10460 | Democratic Federation, 1989–1992 | DB2235–DB2241 | 1989–1993 |
| 10361 | Post-Cold War Europe, 1989–2001 | D2001–D2009 | 1989– |
| 10604 | Postwar Europe, 1945–1989 | D1050–D2009 | 1945– |
| 51992 | Autonomous Kosovo, 1945–2008 | DR2086–DR2087.7 | 1945–. Autonomous region |

These labels impose boundaries different from their assigned classification
headings. This establishes a code/label discrepancy, not that the historical
dates themselves are wrong. Changing a date may also require changing its era
name; narrowing a code range requires examining the classification subdivisions.

## Subjects with no codes in their subtree

- 3569: Late Medieval, 1268–1492
- 3693: War of 1812
- 51005: U.S. Political, 1961–1969
- 51006: U.S. Military, 1961–1969
- 51007: U.S. Biography, 1961–1969

Another 54 uncoded dated subjects have coded descendants. Their lack of direct
codes alone does not demonstrate an error: they can serve as browse groups.

## Audit coverage and limitations

- 318 labels match an exact-code master heading after punctuation normalization.
- 194 additional labels have the same numeric tokens as an exact-code heading.
- 44 have differing numeric tokens in exact-code headings and require review.
- 571 have no exact dated heading match and require further code-scope review.

Numeric-token agreement is only a screening signal: it does not verify BCE/CE,
open-ended ranges, all codes assigned to a subject, or the coverage of child
periods. A different-token result is not automatically a mismatch; codes can
cover subdivisions of a period. Start-code matches are included separately as
leads, never treated as exact range evidence. Named periods without digits are
outside this audit. The bundled master, rather than the latest external LCC
schedule, is the reference used here.

Full evidence: `20260915_history_period_code_audit.csv`.
Reproduce with `python3 shared/subject-projection/tools/audit-history-period-codes.py`.
