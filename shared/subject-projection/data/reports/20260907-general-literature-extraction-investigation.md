# General literature: remaining extraction candidates

The main eight assignments and four thematic candidates were subsequently [installed](20260907-general-literature-installation.md) on 2026-09-07.

Investigation only; no taxonomy edits. Counts use the local July 2026 Open Library matching cache after the American and English author extractions. Candidate totals below are observed-code estimates, not a production-matcher extraction preview.

| Current subject | Direct hits | Finding |
|---|---:|---|
| Literature (root) | 25,853 | All hits are bare class codes: PQ 16,623; PS 6,261; PT 1,337; PA 1,190; PL 442. These provide no thematic or author granularity. Some may be rehomed to appropriate broad existing branches after scope review. |
| English Literature (general) | 41,210 | Selectors PZ3, PZ4 and PR1119; dominated by old author filing codes. This is not primarily a thematic-studies bucket. |
| General Literary Studies | 22,799 | 15,647 hits are bare PN; about 4,105 are themed collections at PN6071, and 966 fall in literary-reference numbers PN41–PN44.5. |
| Special Elements & Subjects | 19,830 | Many documented thematic Cutters remain available for curated splits. |
| Prose. Prose fiction | 10,851 | No children; several substantial theory, writing, genre-study and history subdivisions are available. |

## Highest-value proposed assignments

| Proposed destination | Current source | Candidate selector(s) | Estimated direct hits |
|---|---|---|---:|
| Themed literary collections | General Literary Studies | PN6071 | 4,105 |
| Fiction Writing (reuse existing 3843) | Prose. Prose fiction | PN3355..PN3383 | 3,250 |
| History of Fiction | Prose. Prose fiction | PN3451..PN3503.2 | 1,592 |
| Fiction Theory | Prose. Prose fiction | PN3329..PN3352 | 1,385 |
| Science Fiction & Fantasy studies (reuse existing 4358 where equivalent) | Prose. Prose fiction | PN3433..PN3433.8 | 1,053 |
| Literary Reference | General Literary Studies | PN41..PN44.5 | 966 |
| Mystery & Detective Fiction Studies | Prose. Prose fiction | PN3448.D4 | 655 |
| Fantastic-fiction studies (consider existing 4358) | Prose. Prose fiction | PN3435 | 607 |

Themed collections belong beneath Literary Collections, rather than General Literary Studies. The largest exact code is PN6071.L7 (Love), with 1,081 uses; PN6071.C6 (Christmas) has 181. A contextual collections parent or carefully selected thematic children would preserve the distinction between anthologies and criticism.

## Literary themes worth reviewing

| Proposed topic | Selector | Estimated direct hits |
|---|---|---:|
| Modernism in Literature | PN56.M54 | 533 |
| Literature & Psychoanalysis | PN56.P92 | 439 |
| Travel in Literature | PN56.T7 | 418 |
| Women as Literary Characters | PN56.5.W64 | 406 |
| Literature & History | PN50 | 295 |
| Literature & Science | PN55 | 276 |

These topics belong under the corresponding literary-studies context. They must not classify criticism as ordinary genre fiction or equate women characters with women authors. Existing Literature & Cultural History (6197) is scoped to classical literature through PA6019 and should not be reused indiscriminately.

Captions and hierarchy were checked locally in lcc-mdsconnect-2016.sqlite3. The PN3433..PN3433.8 range is science fiction, PN3355..PN3383 is technique/authorship, and PN3451..PN3503.2 is fiction history. Existing concept 4358 has BISAC LIT004260; concept 3843 has LAN005050. Both are candidates for reuse subject to parent placement and a complete matcher audit.

Recommendation: prioritize Prose. Prose fiction and the misplaced themed collections, then extract the largest documented literary themes. Preserve bare PN and other underspecified class-code coverage. Before applying, test all proposed selectors together, compare complete before/after destinations and coverage, and refresh usage and viewer artifacts.
