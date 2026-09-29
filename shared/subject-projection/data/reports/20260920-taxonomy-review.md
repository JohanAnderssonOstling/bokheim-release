# Subject taxonomy review — 2026-09-20

Reviewed the local authoritative `unified-taxonomy-v2.sqlite3`, format 8, release metadata 18. This is a structural and semantic review of the current workspace snapshot, not verification of the deployed service or a classification-usage audit. No taxonomy data was changed. IDs below identify rows in `concept`; relationships come from `concept_parent`.

Snapshot SHA-256: `4fd7d971c6957dce316edfdbd5142c62947a463bb85f70b5d278ae84fd82b2cd`.

## Findings, in priority order

### 1. High: duplicate concepts preserve separate source-classification trees

Several subjects have separate IDs for BISAC and LCC despite describing the same topic. These are stronger duplication candidates than repeated labels alone: their subject context agrees and their selectors are divided by source system.

| Subject | IDs | Evidence |
|---|---|---|
| Beekeeping | 188 / 13194 | Under Animal Husbandry directly versus through Poultry, Birds & Insects. First has BISAC TEC003100 and DDC 638; second has LCC SF521–SF539.8. |
| Microelectronics | 270 / 8527 | Under two Electronics nodes. BISAC TEC008070 versus LCC TK7874–TK7874.9. |
| Optoelectronics | 271 / 6326 | Same parallel Electronics branches. BISAC TEC008080 versus LCC TK8300–TK8360. |
| Ethnomusicology | 731 / 8448 | Four-level BISAC route versus eight-level LCC route. BISAC MUS015000 versus LCC selector ML3797.6-3799. Only 8448 owns the three specialist children. |
| Educational Psychology | 2199 / 2320 | Directly under Teaching & Learning versus through Education Theory & Practice. BISAC EDU009000 versus LCC ranges LB1050–LB1099.99 and LB1050.9–LB1091. |

Electronics itself is split: 264 has BISAC TEC008000 and seven children; 259 has LCC coverage and 20 children, including the duplicate specializations above. Semiconductors (272) already uses both parents, illustrating inconsistent treatment within this pair of branches.

Consequence: equivalent classifications can reach different IDs and different descendant sets. A reader entering the shallower Ethnomusicology branch cannot navigate to the specialist subjects owned by the other ID.

Recommendation: merge confirmed equivalent concepts and combine selectors and children, retaining genuinely useful multiple parents on the shared ID. Review selector scope before migration; matching names alone are insufficient grounds for merging. Inspect the entire Electronics pair together rather than merging its leaves independently.

### 2. High: a narrow music subject owns unrelated general subjects

`Arts & Media > Music > Theory & Education > Instruction & Study > Music study abroad (806) > Vocal music (854)`.

This puts Lyrics, Sacred vocal music, Secular vocal music, Singing & vocal technique, and their descendants under studying abroad. American Music Before 1860 (8945) is another direct child of 806. There are 14 descendants beneath this narrow parent.

Node 806 also owns M4–M1480 and selectors M1490 and M2.5. Its label, children, and broad range warrant a joint review. This is not safely fixed by merely shortening the breadcrumb: reconstruct the intended subject and selector ownership, then reparent the vocal and historical branches appropriately.

### 3. Medium: colonial ancestry includes post-independence history

The seven ten-level routes all occur through colonial ancestry for Guyanese and Surinamese history. Examples:

- `History > By Region > European > West > British Isles > British Empire & Commonwealth > British America > Guyanese > By Period > 1966-` (11268).
- `History > By Region > European > West > Benelux Countries > Dutch > Dutch America > Surinamese > By Period > 1975-` (11231).

Both countries also have shorter routes through Latin American history, so ten levels are not unavoidable. The semantic problem is that the colonial parent receives the whole country subtree, including its independent period and local history. Existing multiple-parent work deliberately provided colonial entry points; this review recommends narrowing their scope, not removing all cross-regional navigation.

Recommendation: keep general country history under geographic parents and attach colonial periods or dedicated colonial subjects to empire branches. A historical association should not automatically make all later country history a descendant of the former colonial power.

### 4. Medium: repetitive wrappers produce unnecessarily deep paths

`Arts & Media > Music > History, Culture & Criticism > Music History & Criticism > Music Research & Reference > Research, History & Context > Musical research > Ethnomusicology > Ethnomusicology Foundations` (53172) has nine levels and a 205-character breadcrumb.

The longest breadcrumb by character count is 213 characters, at eight levels:

`Philosophy > History, Traditions & Regions > Historical Periods > Modern Philosophy > Regional Modern Philosophy > Latin American Modern Philosophy > South American Modern Philosophy > Venezuelan Modern Philosophy` (11018).

Music repeats research/history wrappers; philosophy repeats regional and modern qualifiers while mixing chronology and geography in one path. Preserve meaningful distinctions, but consider separate region and period navigation or fewer intermediate wrappers. A useful proposed review threshold is six levels, with longer paths justified individually; it is not an existing repository requirement.

### 5. Medium: root-level placement is inconsistent

- Galician Literature (51864) is a root with LCC PQ9450–PQ9469.3, alongside Literature, which already contains a Romance branch.
- Role Playing & Fantasy (4652, BISAC GAM010000) and Travel Games (4655, GAM011000) are roots despite Recreation already containing games branches.
- Philosophy, Psychology & Religion (53067, selector `(B)`) and Geography, Anthropology & Recreation (53068, `(G)`) are childless roots alongside separate topical roots. These look like broad classification fallback concepts rather than navigational groupings.

Recommendation: reattach clear specialist roots to their subject families. Decide explicitly how broad fallback concepts should appear to readers; preserve their classification coverage rather than deleting them as if they were empty wrappers.

### 6. Medium: direct and indirect parents inconsistently create shortcut routes

There are 70 direct child-parent edges for which another parent already supplies an indirect route to the same ancestor. Examples:

- Management Information Systems (134) belongs directly to IT Applications & Management (5577), and indirectly through Information Technology (100).
- Radar (422) belongs directly to Electrical & Electronic Engineering (5566), and indirectly through Electrical Engineering (250).
- Chemotherapy (2559) belongs directly to Health & Medicine and through Pharmacology & Therapeutics.

These are the same IDs exposed through multiple routes, not duplicate concepts. They may be intentional shortcuts, but the taxonomy currently encodes both immediate semantic parents and navigation shortcuts in the same relation. Decide whether to retain that policy consistently or represent shortcuts separately. Do not remove all multiple parents: valid cross-disciplinary placement is useful.

## Measured scope and checks

| Measure | Result |
|---|---:|
| Concepts | 17,005 |
| Parent edges | 17,330 |
| Roots | 26 |
| Concepts with multiple immediate parents | 325 |
| Root-to-concept routes, including intermediate subjects | 18,864 |
| Maximum path length, root counts as level 1 | 10 |
| Routes of at least eight levels | 810 |
| Distinct concepts with at least one such route | 796 |
| Concepts whose shortest route is at least eight levels | 632 |
| Case-insensitive repeated-label groups | 1,267 |
| Distinct concepts in those groups | 4,660 |
| Same-parent case-insensitive duplicate labels | 0 |
| Redundant direct parent edges | 70 |

Eight-or-more-level route counts by root: History 635; Arts & Media 70; Social Sciences 55; Philosophy 35; Health & Medicine 13; Language 2. History accounts for most long routes, but music provides especially clear avoidable wrapper chains.

SQLite quick_check passed, foreign_key_check returned no violations, and traversal of every concept found no cycles. There were no same-parent label collisions. These checks establish basic structural validity, not sound subject semantics.

Repeated-label counts are candidate inventories, not confirmed duplicate counts. Labels such as Other, Local, or By Period are expected under different countries. Audience-specific subjects, subject bibliographies, and disciplinary specializations can also legitimately repeat names. For example, Chemotherapy (13605) in the Psychiatry/Psychotherapy branch owns RC483–RC483.5 and needs a scope/label review before being treated as equivalent to general Chemotherapy (2559).

## Supporting inventories

- [All routes of eight or more levels](20260920-taxonomy-review-long-paths.csv), including shortest-route length for context.
- [All case-insensitive repeated-label candidates](20260920-taxonomy-review-repeated-labels.csv), with concept IDs and every path.
- [All redundant direct parent edges](20260920-taxonomy-review-redundant-parents.csv).

Suggested order of work: repair the music semantic mismatch; consolidate the clear cross-system duplicate branches; narrow colonial ancestry; reattach specialist roots; then simplify wrappers and settle shortcut policy. Each future migration should preserve selector coverage and compare classification destinations before and after. This review does not measure affected book counts or exhaustively adjudicate every repeated label.
