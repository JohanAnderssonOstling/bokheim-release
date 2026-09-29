# U.S. period review

Follow-up: see `20260916_us_period_completion.md` for remaining assignment fixes,
the full-schedule correction concerning E351, and the usage refresh.

Applied `20260916_flatten_us_administrations_and_repair_periods.sql`.

## Changes

- Removed 17 individual presidential-administration subjects: nine under the late nineteenth century, four under the twentieth century, four under the twenty-first century. Preserved their names as aliases and transferred classifications to parents.
- Consolidated late-nineteenth-century administration codes into the parent era range. Existing topical descendants remain more specific destinations.
- Reassigned the full constitutional-era range from U.S. to its period; restored early/mid-nineteenth-century coverage and widened the late-twentieth-century fallback to include its later children.
- Separated early-nineteenth-century codes from the century's general heading; moved late-eighteenth-century biography out of Confederation.
- Removed four redundant direct By Period links; nested the late-century subjects under their centuries. Added the appropriate period ancestry for affected topical subjects so ancestor ranges do not compete with them.
- Replaced vague early-twentieth-century and “Twenties” labels with supported date ranges. Renamed the overly broad Early Republic umbrella to describe its actual span.

## Period interpretation

The [Library of Congress E–F outline](https://www.loc.gov/aba/publications/Archived-LCC2022/LCC_E-F2022OUT.pdf) supports these corrections. Its nominal century boundaries are not strict calendar partitions: McKinley's first administration extends into 1901 and Clinton's into 2001. The 1919–1933 category is deliberately broader than the 1920s. Regional colonial histories can also continue after national independence.

[BISG History headings](https://www.bisg.org/history) define the Revolutionary period as 1775–1800 and Civil War period as 1850–1877, wider than the corresponding LCC war periods. Retained these broader curated subjects and their BISAC assignments rather than falsely narrowing their dates. Chronological overlap alone is not duplication.

The archived LOC outline is not a current authority for recent administrations. Existing twenty-first-century codes were preserved, not extrapolated into new allocations.

## Validation and recovery

2,208 production-matcher probes across E186–E919 and U.S. BISAC period codes passed: no previously resolved probe became unresolved, and every probe previously assigned to a removed administration moved to its intended parent. Full taxonomy loading, SQLite integrity, and foreign keys passed. Viewer regenerated.

Before/after snapshots, probe CSVs, and validation script are in `tmp/us-period-review-20260916/`. Usage-count aggregates were not recomputed in this pass; the code-assignment validation uses the revised database directly.
