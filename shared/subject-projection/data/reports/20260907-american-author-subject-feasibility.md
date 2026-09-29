# American Literature: author-level subject feasibility

Conclusion: author children are technically feasible with the existing exact LCC selector matcher. A curated initial set is supported by current evidence; a complete modern-author rollout needs additional identity mappings. No author subjects from this investigation were installed in the authoritative taxonomy.

## Separate-database pilot

Ten named-author children were added only to `/tmp/literature-next-extraction/author-pilot.sqlite3`, beneath their existing American author-era shelves. Each received its documented exact author Cutter selector. Broader period selectors were retained as residual coverage.

The complete Open Library classification cache moved **11,207 uses across 1,391 distinct codes** to these ten subjects. No matched coverage was lost. All changed destinations were pilot authors, and all displaced sources were the intended American era subjects. Forty positive, neighboring-Cutter, collected-work, and criticism probes passed.

| Author heading in classification reference | Exact selector | Verified direct hits |
|---|---|---:|
| Roberts, Nora | `PS3568.O243` | 2,280 |
| Faulkner, William, 1897-1962 | `PS3511.A86` | 1,528 |
| Hemingway, Ernest, 1899-1961 | `PS3515.E37` | 1,301 |
| Eliot, T. S. (Thomas Stearns), 1888-1965 | `PS3509.L43` | 1,123 |
| King, Stephen, 1947- | `PS3561.I483` | 1,052 |
| Fitzgerald, F. Scott (Francis Scott), 1896-1940 | `PS3511.I9` | 984 |
| Pound, Ezra, 1885-1972 | `PS3531.O82` | 760 |
| Faust, Frederick, 1892-1944 | `PS3511.A87` | 757 |
| Wharton, Edith, 1862-1937 | `PS3545.H16` | 734 |
| Steinbeck, John, 1902-1968 | `PS3537.T3234` | 688 |

The pilot tests exact author-root selectors, including additional book Cutters. It does not use a permissive textual prefix or append arbitrary numeric range endpoints. That avoids absorbing a different author whose Cutter begins with the same characters. Pilot counts differ slightly from simple string-group estimates because the production matcher parses and canonicalizes the notation.

## How much the current reference identifies

| American literature shelf | Current direct hits | Candidate uses with a documented author name | Share | Named first-Cutter groups |
|---|---:|---:|---:|---:|
| 1900–1960 | 54,897 | 48,982 | 89.2% | 2,248 |
| 1961–2000 | 289,117 | 19,331 | 6.7% | 307 |
| 2001– | 149,965 | 1,820 | 1.2% | 127 |

These coverage figures are conservative candidate estimates, not validated migrations. The analysis extracts the first author Cutter from codes currently assigned to each era and looks for exactly one named-author classification record with the appropriate individual-author/letter hierarchy. It excludes unnamed filing instructions and records about individual titles. It does not resolve alternate spellings, aliases, or missing author names, and does not prove that every unmatched code lacks a usable name elsewhere.

The code itself retains an author Cutter for most uses (53,827 of 54,897 in 1900–1960; 285,295 of 289,117 in 1961–2000; 137,135 of 149,965 in 2001–). For recent shelves, missing name/identity mappings are the main observed limitation, rather than absence of an author Cutter. The three shelves retain 17,722 uses without a captured author Cutter, which should remain on their era unless fuller metadata resolves them.

## Works by an author versus books about an author

The [official author table P-PZ40, page 122](https://www.loc.gov/aba/publications/FreeLCC/LCC_P_TABLES2025TEXT.pdf#page=122) groups collected and separate works, translations, biography, and criticism beneath one author Cutter. An author-root selector therefore describes an author-centered shelf rather than an authorship-only filter.

In the pilot, **9,156 of 11,207 uses (81.7%)** retained exactly the author code and no subsequent book Cutter. The usage cache normalizes notation to its first space-delimited component, as documented in `OPENLIBRARY-USAGE-NOTICE.md`. Distinguishing works by an author from books about them reliably requires fuller catalog metadata, contributor roles, and/or the full classification string; it cannot be promised from this aggregate cache alone.

## Suggested implementation

1. Start with the ten validated authors, or a similarly reviewed set, as children of their current era. Keep unmatched records on the era.
2. Link each author subject to one durable author identity; preserve pseudonyms and authority forms as identity data. Do not create duplicate people for alternate names or additional national/period routes.
3. Define the page as an author-centered shelf covering works and studies, or explicitly separate “Works by” and “About” using contributor and subject metadata. An author Cutter alone should not claim authorship.
4. Expand recent-author mappings using the source catalog’s contributor identities and authority data. Review joint pseudonyms, collaborations, homonyms, and nonstandard call numbers before attaching them.
5. Repeat the complete matcher comparison and neighboring-Cutter probes for each batch. Avoid generating thousands of unnamed or low-evidence children from alphabetic schedules.

## Existing policy and evidence

The [current visible-expansion policy](../LCC-VISIBLE-EXPANSION-POLICY.md) states: “Do not promote named-author children when extending an existing LCC-backed leaf by one visible level.” It currently places author identity in the author/creator index. Introducing author subjects would be an explicit product-policy change. This investigation and separate-database pilot do not change that policy or install author nodes.

The author hierarchy, documented headings, and alias examples are in the repository classification reference and the [official PR–PS–PZ schedule](https://www.loc.gov/aba/publications/FreeLCC/LCC_PR-PS_PZ2025TEXT.pdf). The schedule includes pseudonym redirects and joint-pseudonym exceptions, so name strings should not substitute for durable identities.

Full pilot manifests, coverage estimates, normalized assignment comparisons, logs, and probe inputs are retained in `/tmp/literature-next-extraction/` under `american-author-*` and `author-pilot-*`.
