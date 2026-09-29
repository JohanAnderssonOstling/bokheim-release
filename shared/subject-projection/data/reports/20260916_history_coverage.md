# History code coverage

## Resolved after the audit

Bare E/F now resolve to The Americas. All 20 missing BISAC codes below have
been attached by `20260916_attach_missing_history_bisac.sql`: eight codes use
existing subjects, and twelve use new subjects. A new global Indigenous Peoples
group contains the six Indigenous themes, avoiding a false Americas-only scope.
This adds thirteen subjects including that group. All twenty codes resolve to
one destination with the real-label production matcher. Repeated migration,
SQLite integrity and foreign-key checks passed; the viewer was regenerated.
The following sections preserve the original audit findings.

Read-only audit using the current release matcher and frozen curated/master
snapshots in `tmp/history-coverage-20260916/`. No assignments were changed.

## Confirmed gaps

### BISAC

Twenty current History codes were individually probed and returned no resolved
subject and no candidate. Their meanings were checked against the
[official BISG list](https://www.bisg.org/history).

| Codes | Missing coverage |
|---|---|
| HIS006000 | General Canadian history |
| HIS051000 | Exploration and discovery |
| HIS052000 | Geography in historical context |
| HIS067000 | Ukrainian history |
| HIS032010, HIS032020, HIS032030 | Imperial, Soviet and post-Soviet Russian periods |
| HIS026050 | Lebanese history |
| HIS028010–HIS028060 (six codes) | Indigenous origins, migration, archaeology/oral traditions, contact, colonial interactions and modern history |
| HIS068000 | Hispanic/Latino history |
| HIS069000 | Indigenous history of Turtle Island |
| HIS070000 | Native American history |
| HIS071000 | Asian American/Pacific Islander history |
| HIS072000 | Disability history |
| HIS073000 | Historical and collective memory |

The older bundled master does not contain most of these current codes. Its
missing wildcard patterns are not counted as missing individual book codes.

### LCC

Broad class-only classifications D, E and F have no destination. These are
confirmed by direct matcher probes and occur 3,412, 2,065 and 1,910 times
respectively in the local cache: 7,387 classification occurrences in total.
These are occurrences, not distinct books.

By contrast, broad DA, DB, DS and DT resolve successfully. A missing literal
selector is not itself a coverage gap: containing ranges and broader subjects
can resolve the input.

## Coverage scan

| Input set | Covered | No destination | Unsupported syntax |
|---|---:|---:|---:|
| Distinct LCC selectors/ranges from master History | 19,925 | 6 | 15 |
| Observed distinct D/E/F-prefixed LCC strings | 1,433,247 | 238 | 1,934 |
| Observed D/E/F classification occurrences | 1,858,591 | 7,807 | 2,721 |

No ambiguous matches were found in these input sets. The six unresolved master
entries are class-only C, D, E, F, KGV and KGZ. The latter two are law prefixes
present in the reference subtree, not evidence of missing historical periods.
Most of the observed unresolved strings beyond D/E/F have questionable prefixes
or malformed catalog text, so the 238 count must not be treated as 238 valid
missing subjects. Unsupported master ranges include reversed or malformed
endpoints and need source repair, not automatic new assignments.

All 78 valid individual DDC codes tested from the master History subtree
resolved. Summary ranges and unassigned 991/992 were excluded from that test.
Coverage does not establish correctness: a separate spot check found DDC 935
resolving to Assyria-Babylonia (10786) under Philosophy rather than History.
That is a routing issue, not an absent assignment.

## Reproduction

Build `history_coverage` with `cargo build --release -p subject-projection
--example history_coverage`, then pass curated, master and code-usage database
paths. The observed scan covers D/E/F prefixes; it does not exhaust auxiliary
History classes such as CC or military classes outside those prefixes.

Stored evidence: `tmp/history-coverage-20260916/gaps.csv` and `summary.tsv`.
Master LCC range syntax is converted from stored `..` notation to printed `-`
notation before matching. Coverage includes matches to broad fallback subjects
and to subjects outside History; it is not a correctness certification.
