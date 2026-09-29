# History hierarchy cleanup and remaining deep paths

Implemented locally on 20 September 2026. **Preserved every existing By Region, By Period, Eras and Local grouping ID and label.** Depth alone did not trigger a removal.

## Changes

- Consolidated seven equivalent subject pairs: Crusades, Holocaust, Study & Teaching, Victorian Era, Côte d’Ivoire, Kenyan history and Libyan history. The surviving subjects carry the combined codes. Crusades retains medieval and military browsing routes; Libya retains North Africa and Maghrib routes.
- Consolidated the redundant `19th Century → Eastern Question → 19th Century` subject into Eastern Question, preserving its codes and the Conference of Berlin child.
- Consolidated 111 `Local → Other` branches into their existing Local subjects. Each Other node was a leaf, its only parent was Local, and it was Local's sole child. The Local node had no codes before the merge. All leaf codes now belong to Local. This does not remove the spatial grouping or merge named localities.
- Removed U.S. Colonial Period from British America. It remains beneath U.S. → By Period. New Netherland retains its Dutch America route and no longer inherits British America through the U.S. colonial grouping.
- Kept broad colonial Louisiana and Florida subjects beneath their states and the U.S. Colonial Period, removing empire-specific routes that implied one empire covered the whole interval. Their codes describe broad early periods: F372 is Louisiana before 1803; F314 is Florida before 1821.
- Moved four Tunisian period subjects from Maghrib directly into a new Tunisia → By Period group: 19th Century, Bourguiba Era, Ben Ali Era and the existing 2011– subject. Their classification codes and labels are unchanged.
- Changed the Dutch period umbrella from `19th–20th centuries` to `19th Century–Present`, so it can contain the existing Willem-Alexander, 2013–Present subject.

Total: 119 consolidations (including the 111 Local/Other pairs), one new useful period group, and two label changes. The other label change is Education → Study & Teaching, matching BISAC HIS035000 and the LCC D16.2–D16.5 subject it absorbs.

## Before and after

| Measure | Before | After |
|---|---:|---:|
| Entire taxonomy subjects | 16,909 | 16,791 |
| Subjects reachable from History | 3,496 | 3,378 |
| History root-to-subject routes | 3,855 | 3,729 |
| History routes of eight or more levels | 625 | 624 |
| Maximum History depth, including History itself | 9 | 9 |

The deep-route count barely changes because many redundant Local/Other paths were shorter than eight levels, while restoring a useful country/period group also adds levels. This is a semantic cleanup, not an attempt to minimize depth.

## Remaining deep paths for review

All examples below are actual post-cleanup paths, each nine levels deep.

1. **Geography plus a nested period**  
   `History → By Region → European → West → British Isles → English → By Period → 17th century → Early Stuarts, 1603–1642`

   Review question: do West and British Isles both help navigation? By Period and the century group have distinct purposes and were preserved.

2. **Geography down to a city and period**  
   `History → By Region → North American → U.S. → Local → New England → Massachusetts → Boston → 1775–1865`

   Review question: is this geographic detail useful at every step, or should the interface offer a quicker city entry point while keeping the taxonomy intact?

3. **A locality-type grouping**  
   `History → By Region → European → West → British Isles → English → Local → Counties & Regions → Yorkshire`

   Review question: does Counties & Regions help distinguish those subjects from cities, or should they appear together inside Local?

4. **Regional and dynastic grouping**  
   `History → By Region → European → South → Iberia → Portuguese → By Period → Medieval Era, 1095–1580 → House of Aviz, 1385–1580`

   Review question: are South and Iberia both useful? The country, period and dynasty express separate scopes.

5. **Several chronological subdivisions**  
   `History → By Region → European → South → Greek → Modern → By Period → Greece since 1913 → Greece, 1913–1967`

   Review question: should Modern be a period inside the country's By Period group, and should repeated “Greece” wording be removed? Modern currently owns other thematic and local children too, so restructuring it requires a branch-level decision.

6. **A century umbrella and a reign**  
   `History → By Region → European → West → Benelux Countries → Dutch → By Period → 19th Century–Present → Willem-Alexander, 2013–Present`

   Review question: is the century umbrella useful, or should reigns be immediate children of By Period? Its date scope is now consistent with its children.

No additional shortening of these examples was applied. The complete [remaining deep-path inventory](20260920-history-remaining-deep-paths.json) includes subject IDs and depths.

## Retained distinctions and follow-up

Identical names alone were not sufficient for merging. National centuries, WWI versus WWII operations, military versus general Canadian/U.S. history, and ancient civilization versus general ancient history remain distinct. The two Armenian headings need a separate scope review: the local reference distinguishes DK680 Armenia (Republic)/Armenian SSR from the broader DS161 Armenia section, while the current subjects have overlapping coverage.

The Greco-Roman branch's modern-period children also remain a scope-review candidate. The local source itself places DE100 “1945–” inside an ancient/Greco-Roman hierarchy, so importing that ancestry again would not settle the appropriate display structure.

## Evidence and validation

Evidence uses the local LCC reference database and BISAC CSV. LCC entries checked include D151/D173 (Crusades), D804.177/D804.348 (Holocaust), D16.2 (Study and teaching), D374/D375 (the repeated Eastern Question century), F122.1 (New Netherland), F372/F314 (broad colonial periods), DT545 (Côte d’Ivoire), DT433.5 (Kenya), DT211 (Libya), and DT263/DT264.35/DT266.5/DT266.8 (Tunisian context). Existing selectors and ranges were preserved through transfers; no new geographic or chronological code coverage was invented.

Validation includes a frozen before/after comparison, migration replay equivalence, SQLite integrity and foreign keys, cycles, sibling-label collisions, preservation of grouping IDs, 54,700 selector/endpoint probes, and the full observed-code audit. The audit checked 11,977,153 strings: 82,047 changed destinations, zero losses and zero gains. Apart from ID consolidations, 190 Libyan locality strings now reach Libya's Local subject instead of the duplicate country heading. All original selector strings and range boundaries remain present. Twelve release scope-regression tests pass; the library suite has 249 passes and the same four existing failures before and after. Exact totals, the database fingerprint and reviewed destination changes are in the [validation results](20260920-history-hierarchy-validation.json).

Artifacts: migration, [change manifest](20260920-history-hierarchy-cleanup.json), [observed code changes](20260920-history-classification-changes.tsv). The classification usage table and viewer are regenerated after validation.
