# European subregions and local history periods

Implemented the two requested changes together, preserving classification codes.

## European geography

Moved all ten children of West and South directly under Europe: Benelux Countries, British Isles, French, Greek, Maltese, Greco-Roman, Italian, Iberia, Cypriot and Monaco. Western Europe remains a leaf carrying BISAC HIS010020. The empty, uncoded South grouping was removed.

Examples:

- `History → By Region → European → British Isles`
- `History → By Region → European → Benelux Countries`
- `History → By Region → European → Iberia`

East, Central, North, Balkan Peninsula and the existing chronological grouping remain in place. This request did not authorize a wider restructuring of those groups.

## Local time slices

Reviewed all 50 date-bearing subjects reachable through a History Local branch, plus a screen for undated period labels. Merged 49 time slices into their 15 geographic parents, transferring every selector and range. No national period groups were removed.

Places affected: New York City, Philadelphia, Pennsylvania, Great Plains, Boston, Massachusetts, Alabama, Florida, Louisiana, Texas, Virginia, Washington D.C., the U.S. South, California and Berlin.

Examples:

- Boston → 1775–1865, 1865–1950 and 1951– now all classify to Boston.
- New York City → its five date bands now classify to the city.
- Berlin → Berlin, 1945–1990 now classifies to Berlin.
- Massachusetts, Texas, Virginia and California retain their place subjects and absorb their period codes.
- Broad colonial Florida and Louisiana subjects were merged into those states. The states did **not** inherit the former period subjects' U.S. Colonial Period parent, which would incorrectly put all state history beneath a colonial era.

New Netherland, 1610–1664 remains a distinct historical territory with its Dutch America route. It is not merely a neutral date subdivision of a present-day place. No other named events were selected for removal.

For example, the Boston route now ends at:

`History → By Region → North American → U.S. → Local → New England → Massachusetts → Boston`

## Result and validation

| Measure | Before these two changes | After |
|---|---:|---:|
| Entire taxonomy subjects | 16,791 | 16,741 |
| History subjects | 3,378 | 3,328 |
| History root-to-subject routes | 3,729 | 3,677 |
| History routes at least eight levels deep | 624 | 378 |
| Maximum History depth | 9 | 9 |

Thirteen curated scope tests pass using an optimized release build. Selector/endpoint checks cover 54,700 probes; the European reparenting alone changed no destinations, and all combined boundary changes match the intended local-period merges. Full observed-code totals, integrity checks and viewer verification are recorded in the [validation report](20260920-europe-local-validation.json). The usage table and viewer reflect the final combined taxonomy.

Artifacts: European migration, local-period migration, [49-merge manifest](20260920-local-history-periods.json), [remaining deep paths](20260920-europe-local-deep-paths.json), [observed classification changes](20260920-europe-local-classification-changes.tsv).
