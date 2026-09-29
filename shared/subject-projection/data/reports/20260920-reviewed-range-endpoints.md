# Reviewed range endpoint corrections

Source: [Library of Congress D–DR 2025 schedule](https://www.loc.gov/aba/publications/FreeLCC/LCC_D-DR2025TEXT.pdf), printed pages 24 and 447.

| Concept | Original range | Corrected range |
|---|---|---|
| 10305, Russo-German Austrian | D551 .. D552.A-Z | D551 .. D552.Z |
| 54459, Political & Foreign Relations | DK908.67 .. DK908.68.2 | DK908.67 .. DK908.68 |

The first original spelling appears in retrospective record CF 95247892.
Its child CF 95247893 explicitly covers D552.A through D552.Z. The current
schedule confirms that D552.A-Z denotes an interval, not one boundary.
The old fallback silently truncated the upper boundary to D552.A.

The second original spelling appears in CF 95316093. Historical child
CF 95317106 has DK908.682 for a By period heading. The 2025 schedule instead
gives political-history sources at DK908.67 and general works at DK908.68,
with an unnumbered instruction to consult the specific period. The existing
exact DK908.682 selector is retained for historical compatibility. No new
interval between DK908.68 and DK908.682 is introduced.

The accompanying 20260920_correct_legacy_range_endpoints.sql migration is
qualified by concept and both original boundaries and is idempotent. It was
applied to the editable source taxonomy. Older read-only snapshots receive
these same two specific repairs during definition loading; writable caches
persist them through their upgrade path. New malformed inputs are rejected.
The archival MDSConnect reference records and prior migration history remain
unchanged as evidence of the original source spellings.

Stored-range compilation and writing now require complete endpoint parsing.
Book-call recovery is unchanged. Release regression tests cover complete
A–Z coverage, adjacent exclusions, Kazakhstan's preserved exact selector,
legacy snapshot upgrades, rejected malformed suffixes, and structured
round trips of every boundary in the bundled taxonomy (27,268 boundaries).

The taxonomy suite has 243 passing tests and the same four pre-existing
failures documented in the prior profiling report. The storage schema still
uses textual endpoints: this change fixes and validates the data needed for
the subsequent structured-storage migration.
