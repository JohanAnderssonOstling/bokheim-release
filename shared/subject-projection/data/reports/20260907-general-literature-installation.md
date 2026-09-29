# General literary studies and themed collections: extraction installed

Applied the eight proposed assignments and four additional thematic candidates approved in the conversation. Twelve selector assignments create eight subjects and reuse three existing subjects: Fiction Writing, Mystery & Detective criticism, and Science Fiction & Fantasy criticism. The science-fiction range and fantastic-fiction selector share the existing combined criticism subject.

**15,823 classification uses across 2,604 distinct codes move to the intended destinations.** Full before/after matching finds no coverage lost or gained, and no unrelated destination changes. These are classification uses, not unique books.

| Source | Before | After | Hits moved |
|---|---:|---:|---:|
| General Literary Studies | 22,799 | 17,732 | 5,067 |
| Prose. Prose fiction | 10,851 | 2,208 | 8,643 |
| Special Elements & Subjects | 19,830 | 17,717 | 2,113 |

| Destination | Hits moved | Total direct hits after | Subject ID |
|---|---:|---:|---:|
| Themed Literary Collections | 4,101 | 4,101 | 52306 |
| Fiction Writing | 3,250 | 3,250 | 3843 |
| Science Fiction & Fantasy | 1,666 | 1,666 | 4358 |
| History of Fiction | 1,592 | 1,592 | 52307 |
| Fiction Theory | 1,385 | 1,385 | 52308 |
| Literary Reference | 966 | 966 | 52309 |
| Mystery & Detective | 750 | 750 | 4354 |
| Modernism in Literature | 685 | 685 | 52310 |
| Literature & Psychoanalysis | 510 | 510 | 52311 |
| Travel in Literature | 464 | 464 | 52312 |
| Women as Literary Characters | 454 | 454 | 52313 |

Themed Literary Collections is placed beneath Literary Collections. Fiction Writing, Mystery & Detective, and Science Fiction & Fantasy retain their existing routes and gain a route beneath Prose. Prose fiction. The four thematic subjects are children of Special Elements & Subjects. Bare class-code coverage remains unchanged.

Existing specific fiction-writing and fiction-theory ranges are transferred to their destinations. The fiction-history range is transferred and extended from PN3503 to PN3503.2 to match the locally documented schedule. The broad PN3311..PN3503 range retains residual coverage.

| Selector | Destination |
|---|---|
| `PN6071` | Themed Literary Collections |
| `PN3355..PN3383` | Fiction Writing |
| `PN3451..PN3503.2` | History of Fiction |
| `PN3329..PN3352` | Fiction Theory |
| `PN3433..PN3433.8` | Science Fiction & Fantasy |
| `PN41..PN44.5` | Literary Reference |
| `PN3448.D4` | Mystery & Detective |
| `PN3435` | Science Fiction & Fantasy |
| `PN56.M54` | Modernism in Literature |
| `PN56.P92` | Literature & Psychoanalysis |
| `PN56.T7` | Travel in Literature |
| `PN56.5.W64` | Women as Literary Characters |

Validation: 53 exact-selector, Cutter and numeric-range boundary probes; complete per-code before/after matching; migration replay; SQLite integrity and foreign keys; graph acyclicity and parent placement. Installation preserves two concurrent, unrelated parent links in Egyptian and North African history. Matching was rerun against the installed snapshot before publication. Published usage, matching cache and viewer are refreshed; all eleven destination hit totals, selector counts and new parent routes are checked.

Captions and range endpoints were checked against the local LCC reference during the [investigation](20260907-general-literature-extraction-investigation.md). Literature & History and Literature & Science were additional report suggestions, beyond the four thematic candidates presented for this batch, and remain unmodified.

Migration: 20260907_extract_general_literature_studies.sql. [Installed manifest](20260907-general-literature-manifest.json). Working artifacts: `/home/johan/.cache/bokheim/general-literature-install/`; large derived files may be regenerated from retained database snapshots.
