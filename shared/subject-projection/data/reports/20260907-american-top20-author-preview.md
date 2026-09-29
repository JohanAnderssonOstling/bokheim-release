# American Literature: top-20 author extraction preview

Tested in a separate database; not applied to the authoritative taxonomy. The ranking covers named author shelves in the American LCC PS author schedules, including nineteenth-century writers, rather than only the modern era or a general popularity ranking.

**23,026 classification uses across 3,672 distinct codes would move into 20 author subjects.** Complete before/after matching shows no coverage lost or gained. Eighty exact-selector, range-endpoint, work/criticism, and neighboring-Cutter checks pass; staged integrity and foreign keys pass.

| Rank | Author | Direct hits | Selector |
|---:|---|---:|---|
| 1 | Nora Roberts | 2,280 | `PS3568.O243` |
| 2 | Mark Twain | 2,121 | `PS1300..PS1348` |
| 3 | Henry James | 1,823 | `PS2110..PS2128` |
| 4 | William Faulkner | 1,528 | `PS3511.A86` |
| 5 | Ernest Hemingway | 1,301 | `PS3515.E37` |
| 6 | Herman Melville | 1,261 | `PS2380..PS2388` |
| 7 | Edgar Allan Poe | 1,193 | `PS2600..PS2648` |
| 8 | T. S. Eliot | 1,123 | `PS3509.L43` |
| 9 | Stephen King | 1,052 | `PS3561.I483` |
| 10 | F. Scott Fitzgerald | 984 | `PS3511.I9` |
| 11 | Nathaniel Hawthorne | 982 | `PS1850..PS1898` |
| 12 | Walt Whitman | 943 | `PS3200..PS3248` |
| 13 | James Patterson | 928 | `PS3566.A822` |
| 14 | Danielle Steel | 845 | `PS3569.T33828` |
| 15 | Henry David Thoreau | 809 | `PS3040..PS3058` |
| 16 | Ralph Waldo Emerson | 806 | `PS1600..PS1648` |
| 17 | Emily Dickinson | 796 | `PS1541` |
| 18 | Ezra Pound | 760 | `PS3531.O82` |
| 19 | Frederick Faust (Max Brand) | 757 | `PS3511.A87` |
| 20 | Edith Wharton | 734 | `PS3545.H16` |

| Era shelf | Before | After | Hits moved |
|---|---:|---:|---:|
| 19th century | 19,642 | 8,908 | 10,734 |
| 1900–1960 | 54,897 | 47,710 | 7,187 |
| 1961–2000 | 289,117 | 284,012 | 5,105 |
| 2001– | 149,965 | 149,965 | 0 |

This is a useful browsing pilot but reduces the largest 1961–2000 shelf by only 1.8%. Modern author mapping coverage remains the main constraint on a larger rollout. Author shelves include works by and books about their authors; these counts are classification uses, not unique books.

Selection was seeded from documented named-author records and observed code usage, then validated with the production matcher. High-use unmapped author Cutters were checked so that James Patterson and Danielle Steel were included. Most headings and older numeric author spans are documented in the repository LCC reference. Patterson’s mapping is independently evidenced by the [Wake County MARC record](https://catalog.wake.gov/Record/672271); Steel’s by the [University of North Texas catalog](https://discover.library.unt.edu/catalog/b2256653). No claim is made to complete author-identity coverage of the underlying collection.

Exact author Cutters are used for modern writers. Named numeric spans are used for older writers such as Mark Twain and Henry James. Author nodes are staged directly beneath their existing American era subject. No new alias/identity behavior or creator attribution is introduced.

The earlier [author feasibility investigation](20260907-american-author-subject-feasibility.md) discusses identity coverage, mixed works/criticism, and the current author-exclusion policy. This preview does not alter that policy or install author subjects. Full temporary databases, input manifests and comparison outputs are in `/tmp/american-top20/`.
