# Remaining literary themes, national journalism/theater, U.S. theater and humor

Applied the approved continuation on 2026-09-07: 42 selector assignments create 36 subjects and reuse five existing humor topics. The new subjects comprise 25 country-level theater/journalism subjects, eight literary themes and three U.S. theater subjects. Broad residual selectors remain in place.

**29,088 classification uses across 8,100 distinct codes move to the intended subjects.** Full comparison of the batch finds no matched coverage lost or gained and no unrelated destination changes. These are classification uses, not unique books.

| Source | Before | After | Direct hits moved |
|---|---:|---:|---:|
| Special Elements & Subjects | 17,406 | 15,400 | 2,006 |
| Other National Theater | 11,791 | 6,434 | 5,357 |
| Other National Journalism | 12,265 | 6,253 | 6,012 |
| U.S. Theater | 14,492 | 345 | 14,147 |
| Humor & Satire | 11,462 | 9,896 | 1,566 |

## Selection and placement

Country selection uses local residual hits and local documented LCC country spans, prioritizing at least 200 observed uses per group. The Czech structural reference record ends prematurely at PN2859.C93: the cached 2025 PN schedule, page 103, explicitly includes Czech collective biography at C95 and individual biography at C96A–C96Z. The country selector therefore includes C9..C96, and its endpoint descendants and neighboring Cutters were tested. No individual-person subjects are created.

U.S. theater is split into history (PN2221..PN2272.5), regional/local theater (PN2273..PN2279), and biography (PN2285..PN2287). The history selector ends before the separate local schedule begins. The biographies remain one group.

Cats move into the existing Animals humor topic; Jewish humor into Cultural, Ethnic & Regional; men and women into Men, Women & Relationships; politics into Politics; and sex into Adult. These retain their existing humor context and BISAC selectors. Literary-theme children remain beneath Special Elements & Subjects.

## Assignments

| Destination | Selector | Uses moved into subject | Subject ID |
|---|---|---:|---:|
| U.S. Theater Biography | `PN2285..PN2287` | 12,582 | 52362 |
| Polish Theater | `PN2859.P6..PN2859.P66` | 1,203 | 52327 |
| U.S. Theater History | `PN2221..PN2272.5` | 922 | 52360 |
| Brazilian Journalism | `PN5021..PN5030` | 834 | 52338 |
| Korean Journalism | `PN5411..PN5420` | 822 | 52339 |
| Brazilian Theater | `PN2470..PN2474` | 796 | 52328 |
| Canadian Journalism | `PN4901..PN4920` | 718 | 52340 |
| Korean Theater | `PN2930..PN2938` | 665 | 52329 |
| Regional U.S. Theater | `PN2273..PN2279` | 643 | 52361 |
| Polish Journalism | `PN5355.P6..PN5355.P64` | 572 | 52341 |
| Czech Theater | `PN2859.C9..PN2859.C96` | 522 | 52337 |
| Hungarian Theater | `PN2859.H8..PN2859.H86` | 466 | 52330 |
| Men, Women & Relationships | `PN6231.M45`, `PN6231.W6` | 444 | 713 |
| Argentine Journalism | `PN5001..PN5010` | 442 | 52342 |
| Australian Journalism | `PN5510.2..PN5590` | 427 | 52343 |
| Argentine Theater | `PN2450..PN2454` | 386 | 52331 |
| Mexican Journalism | `PN4961..PN4980` | 379 | 52344 |
| Cultural, Ethnic & Regional | `PN6231.J5` | 378 | 707 |
| Animals | `PN6231.C23` | 369 | 704 |
| Mexican Theater | `PN2310..PN2318` | 349 | 52332 |
| Dutch Journalism | `PN5251..PN5260` | 320 | 52345 |
| Canadian Theater | `PN2300..PN2306` | 312 | 52333 |
| Holocaust in Literature | `PN56.H55` | 308 | 52359 |
| Egyptian Journalism | `PN5461..PN5465` | 303 | 52346 |
| Literature & History | `PN50` | 295 | 52352 |
| Turkish Journalism | `PN5355.T8..PN5355.T84` | 294 | 52347 |
| Literature & Science | `PN55` | 277 | 52353 |
| Law in Literature | `PN56.L33` | 256 | 52354 |
| Swedish Journalism | `PN5301..PN5310` | 251 | 52348 |
| War in Literature | `PN56.W3` | 248 | 52355 |
| Ukrainian Journalism | `PN5355.U38..PN5355.U384` | 225 | 52349 |
| Turkish Theater | `PN2959..PN2959.8` | 223 | 52334 |
| Dutch Theater | `PN2710..PN2718` | 222 | 52335 |
| Love in Literature | `PN56.L6` | 220 | 52356 |
| Irish Journalism | `PN5141..PN5150` | 214 | 52350 |
| Egyptian Theater | `PN2970..PN2978` | 213 | 52336 |
| South African Journalism | `PN5471..PN5480` | 211 | 52351 |
| The Human Body in Literature | `PN56.B62` | 207 | 52357 |
| Politics | `PN6231.P6` | 199 | 714 |
| Utopias in Literature | `PN56.U8` | 195 | 52358 |
| Adult | `PN6231.S54` | 176 | 703 |

## Verification

196 selector, range-endpoint, descendant-Cutter and neighboring-code probes pass. Duplicate display labels in unrelated branches are disambiguated by explicit subject ID in the humor probes. Migration replay, database integrity, foreign keys, graph acyclicity and parent placement pass. All published destination counts, selector totals and routes are checked.
Unrelated concurrent taxonomy edits were preserved; matching was rerun against the installed snapshot before publishing the usage cache and viewer. Differences are recorded in the working concurrent-changes.json.

Migration: 20260907_extract_literary_themes_countries_and_humor.sql. [Manifest with local reference evidence](20260907-literature-themes-countries-manifest.json). Working files: `/home/johan/.cache/bokheim/literature-themes-countries/`.
