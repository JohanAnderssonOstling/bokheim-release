# American Literature: 50 author subjects installed

Applied the approved 50-author preview on 2026-09-07. Added 50 subjects with 50 LCC selectors beneath the existing American literature era subjects, retaining the broad era selectors for residual coverage.

The complete July 2026 Open Library cache comparison moves **37,969 classification uses across 5,575 distinct codes** into the new subjects, with no matched coverage lost or gained. All changed destinations are the approved author subjects. These counts are classification uses, not unique books; the shelves include works by and material about authors.

| Era | Before | After | Direct hits moved |
|---|---:|---:|---:|
| 19th century | 19,642 | 8,492 | 11,150 |
| 1900–1960 | 54,897 | 42,458 | 12,439 |
| 1961–2000 | 289,117 | 274,737 | 14,380 |
| 2001– | 149,965 | 149,965 | 0 |

Validation: 200 selector and boundary probes, full before/after matching, migration replay, SQLite integrity and foreign keys, graph acyclicity, and parent placement. The installed database equals the audited staging database. Published usage counts, matching cache and viewer have been refreshed; all 50 viewer nodes have the expected hit count and one selector.

The [visible expansion policy](../LCC-VISIBLE-EXPANSION-POLICY.md#individual-authors) records this approved, bounded exception. The [preview](20260907-american-top50-author-preview.md) documents candidate selection, mapping evidence and limitations. Installation does not introduce creator identity, alias handling or automatic author expansion.

| Author heading | Subject ID | Direct hits | Selector |
|---|---:|---:|---|
| Roberts, Nora | 52111 | 2,280 | `PS3568.O243` |
| Clemens, Samuel Langhorne ("Mark Twain") | 52112 | 2,121 | `PS1300..PS1348` |
| James, Henry | 52113 | 1,823 | `PS2110..PS2128` |
| Faulkner, William, 1897-1962 | 52114 | 1,528 | `PS3511.A86` |
| Hemingway, Ernest, 1899-1961 | 52115 | 1,301 | `PS3515.E37` |
| Melville, Herman | 52116 | 1,261 | `PS2380..PS2388` |
| Poe, Edgar Allan | 52117 | 1,193 | `PS2600..PS2648` |
| Eliot, T. S. (Thomas Stearns), 1888-1965 | 52118 | 1,123 | `PS3509.L43` |
| King, Stephen, 1947- | 52119 | 1,052 | `PS3561.I483` |
| Fitzgerald, F. Scott (Francis Scott), 1896-1940 | 52120 | 984 | `PS3511.I9` |
| Hawthorne, Nathaniel | 52121 | 982 | `PS1850..PS1898` |
| Whitman, Walt | 52122 | 943 | `PS3200..PS3248` |
| Patterson, James | 52123 | 928 | `PS3566.A822` |
| Steel, Danielle | 52124 | 845 | `PS3569.T33828` |
| Thoreau, Henry David | 52125 | 809 | `PS3040..PS3058` |
| Emerson, Ralph Waldo | 52126 | 806 | `PS1600..PS1648` |
| Dickinson, Emily | 52127 | 796 | `PS1541` |
| Pound, Ezra, 1885-1972 | 52128 | 760 | `PS3531.O82` |
| Faust, Frederick, 1892-1944 | 52129 | 757 | `PS3511.A87` |
| Wharton, Edith, 1862-1937 | 52130 | 734 | `PS3545.H16` |
| Steinbeck, John, 1902-1968 | 52131 | 688 | `PS3537.T3234` |
| Updike, John | 52132 | 686 | `PS3571.P4` |
| Cather, Willa Sibert | 52133 | 673 | `PS3505.A87` |
| Morrison, Toni | 52134 | 625 | `PS3563.O8749` |
| London, Jack | 52135 | 608 | `PS3523.O46` |
| Oates, Joyce Carol, 1938- | 52136 | 584 | `PS3565.A8` |
| Krentz, Jayne Ann | 52137 | 550 | `PS3561.R44` |
| L'Amour, Louis, 1908- | 52138 | 536 | `PS3523.A446` |
| Miller, Arthur, 1915-2005 | 52139 | 536 | `PS3525.I5156` |
| Michaels, Fern | 52140 | 519 | `PS3563.I27` |
| Plath, Sylvia | 52141 | 517 | `PS3566.L27` |
| Macomber, Debbie | 52142 | 512 | `PS3563.A2364` |
| Williams, Tennessee, 1911-1983 | 52143 | 502 | `PS3545.I5365` |
| Cussler, Clive | 52144 | 497 | `PS3553.U75` |
| Koontz, Dean | 52145 | 478 | `PS3561.O55` |
| Morris, Gilbert | 52146 | 476 | `PS3563.O8742` |
| Dick, Philip K. | 52147 | 472 | `PS3554.I3` |
| Stein, Gertrude | 52148 | 461 | `PS3537.T323` |
| Kerouac, Jack, 1922-1969 | 52149 | 452 | `PS3521.E735` |
| Johnstone, William W. | 52150 | 443 | `PS3560.O415` |
| Roth, Philip | 52151 | 443 | `PS3568.O855` |
| Paine, Lauran | 52152 | 426 | `PS3566.A34` |
| Clark, Mary Higgins | 52153 | 423 | `PS3553.L287` |
| Brown, Sandra | 52154 | 422 | `PS3552.R718` |
| Alcott, Louisa May | 52155 | 416 | `PS1015..PS1018` |
| Grisham, John | 52156 | 412 | `PS3557.R5355` |
| Grey, Zane, 1872-1939 | 52157 | 401 | `PS3513.R6545` |
| Andrews, V. C. | 52158 | 395 | `PS3551.N454` |
| Nabokov, Vladimir Vladimirovich, 1899-1977 | 52159 | 395 | `PS3527.A15` |
| Parker, Robert B. | 52160 | 395 | `PS3566.A686` |

Migration: 20260907_extract_top50_american_authors.sql. The [installation manifest](20260907-american-top50-author-manifest.json) retains the selector mappings and reference evidence. Full working verification artifacts are in `/home/johan/.cache/bokheim/american-top50-install/`.
