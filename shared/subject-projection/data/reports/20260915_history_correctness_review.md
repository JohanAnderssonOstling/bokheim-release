# History assignment correctness review

## Resolution

The confirmed fixes below have now been applied through
`20260915_fix_reviewed_history_assignments.sql` and
`20260915_fix_reviewed_history_master_reference.sql`:

- Moved Egyptian ranges out of Lesotho and repaired the master Egyptian ancestry.
- Consolidated the six conflicting BISAC codes onto six inclusive subjects.
  Expanded the Revolutionary Period label to 1775–1800 to reflect BISAC scope.
- Narrowed the early Liberian period; retained later material on the Liberia
  parent and corrected the erroneous early-period range in the master.
- Moved the U.S. E838–E851.999 fallback to the later twentieth century and gave
  1961–1969 its narrower E841–E851.999 range, preserving continuous coverage.
- Applied the listed War of 1812, Central Asia, Turkish/Ottoman, Algeria,
  Arabian Peninsula and northern European specificity improvements.
- Added six regional History children under Canada / Local (IDs 54911–54916),
  moving their six BISAC codes and 22 existing LCC ranges to those children.
- Confirmed Tunisia's DT193 range belongs to Maghrib and moved it there.
  Repaired the master's mixed Tunisian ethnography assignments as well.
- Confirmed Vietnam's DS556–DS559.93 range is correct using the
  [LC outline](https://www.loc.gov/catdir/cpso/lcco/lcco_d.pdf).
  Kept the curated range and corrected the master reference destination.
- Preserved the user's previous DDC decisions, including 942 under England and
  962 under Egypt, and the four regional parent changes.

Validation: optimized release `taxonomy_probe` build; 49 production matcher
probes passed, with 33 corrected resolutions, repeated migration checks, and
clean SQLite integrity/foreign-key checks. Probe evidence is in
`20260915_history_assignment_fix_probes.csv`. Backups and the repeatable probe
runner are in `tmp/history-correctness-fix-20260915/`.

Subsequent validation update: `20260915_fix_duplicate_parent_child_labels.sql`
corrects all six case-insensitive parent/child duplicates (Classical, Film,
Archaeology, Chinese, Japanese and Korean). Full taxonomy loading now succeeds
with its actual labels, and all 49 history probes pass without `--code-only`.
The six renamed subjects also retain their original code destinations. Usage
counts and the viewer were refreshed from the full local classification cache:
15,124,751 of 15,676,165 LCC classification occurrences matched. Publication
verified that the live taxonomy still matched the scanned snapshot. Run evidence
and the detailed match export are in `tmp/taxonomy-label-fix-20260915/`.
The
following paragraph records the limitation during the earlier assignment pass.

Earlier limitation: strict full-taxonomy loading already failed before these changes
because of duplicate parent/child labels (first failure: Classical 3868/5588).
The code-resolution probes used `--code-only`, substituting unique display
labels in memory while preserving all IDs, edges and selectors. This verifies
code resolution, not normal display-route loading. The viewer was regenerated;
usage counts were not refreshed because their full-taxonomy matcher has the
same pre-existing load problem. Those unrelated label conflicts are not fixed
by those assignment migrations; they were corrected in the subsequent label pass.

The remainder of this document records the original findings. Automated
screening candidates outside the confirmed cases remain unverified.

## Scope and conclusion

Read-only review of 3,544 subjects reachable from History (3319), covering
5,643 assignments: 3,426 LCC ranges, 1,932 LCC selectors, 206 BISAC assignments,
and 79 DDC assignments. No taxonomy corrections were applied in this review.

The branch has substantive assignment errors. Priorities are wrong-country
LCC assignments, conflicting broad BISAC codes, and DDC codes whose geographic
scope exceeds the destination country. The bundled master is not independent
ground truth: it contains some of the same errors.

All assignments were inventoried. DDC and BISAC destinations were inspected;
LCC received exact-heading comparison, geographic screening and targeted manual
review. This is not a certification of all 5,358 LCC assignments. Exact master
headings were available for 2,716 of the 5,643 assignments. Lexical differences
and numeric outliers are screening candidates, not confirmed errors.

## High priority: confirmed errors

### 1. Ancient Egyptian ranges assigned to Lesotho

Subject 3346 (Lesotho) owns DT63–DT63.5, DT68–DT68.8 and DT68.2–DT68.3.
The reference headings describe pyramids, Egyptian religious antiquities and
cultus. These belong under Egypt/Ancient Egypt, not Lesotho.

Independent check: the Library of Congress records an Egyptian-pyramids book at
[DT63 .F5](https://www.loc.gov/resource/gdcmassbookdig.egyptianpyramids00fish/?sp=3).
The exact headings are also preserved in the assignment CSV.

The master has Ancient Egypt (33844) under By period (31803), itself under
Lesotho (3346). That broken ancestry explains why a master-versus-curated
country comparison fails to flag this. Repair needs to consider both the
destination and the source hierarchy; copying the master would reproduce it.

### 2. Six broad BISAC codes have competing narrow destinations

| Code | Assignment count | Current destinations |
|---|---:|---|
| HIS036020 | 5 | Individual colonial places and subperiods |
| HIS036030 | 14 | Revolutionary biography, politics, warfare and other themes |
| HIS036050 | 13 | Civil War biography, warfare, finance and other themes |
| HIS036060 | 11 | Individual administrations and twentieth-century themes |
| HIS036070 | 4 | Individual presidential administrations |
| HIS059000 | 2 | Earlier and later Byzantine periods |

These are 49 assignments of six codes. Each code supplies a broad period,
not evidence for the individual narrower subject. The
[BISG History list](https://www.bisg.org/history) confirms those broad scopes.
Use a shared subject covering the entire code's meaning, rather than copying
it onto the children.

Runtime consequence is established by code inspection: `runtime.rs`
`strict_matching_concepts` returns no resolved subject for multiple candidates;
`specificity_bisac_descendants_win_and_unrelated_ties_remain_diagnostic` explicitly
tests this policy. The review script checks ancestor shadowing for each conflict.
No new production matcher executable was built or run for this review.

### 3. Broad DDC geographic codes assigned to individual countries

| DDC | Destination | Scope lost by that destination |
|---|---|---|
| 942 | English (3566) | Wales |
| 943 | German (3524) | Other central European countries |
| 946 | Spanish (3613) | Portugal and the rest of Iberia |
| 947 | Russian (3595) | Other eastern European countries |
| 962 | Egyptian (3635) | Sudan |
| 972 | Mexican (3391) | Other parts of Middle America |

The [OCLC DDC summary, p. 19](https://www.oclc.org/content/dam/oclc/dewey/resources/summaries/deweysummaries.pdf)
supports these wider scopes. The reference is an older public summary; the
bundled master independently retains the same scope distinctions.

This affects detailed numbers too: `matching_candidates` normalizes DDC then
uses `code[..3]`. A narrower original number cannot recover the right country
after that truncation. Within the current three-digit schema, use an inclusive
regional destination. More precise routing requires retaining additional digits.

### 4. Liberia's early period absorbs twentieth-century codes

Subject 54301, Early to 1847, owns DT633–DT636.3. That interval includes DT634
and DT635, which describe later periods. Those ranges currently reside on the
Liberia parent; the early-period descendant can shadow that parent.
It also partially overlaps 54302 (1971–1980), DT636.2–DT636.4.

[Library of Congress subject headings](https://www.loc.gov/aba/publications/FreeLCSH/L.pdf)
identify DT633 with the early period, DT634 with 1847–1944, DT635 with 1944–1971,
and DT636.2 with 1971–1980. The early subject's range must be narrowed after
checking the printed subdivisions. The master repeats the faulty broad range,
so exact master agreement did not establish correctness here either.

### 5. The U.S. 1961–1969 range still includes later general material

Subject 50990 owns E838–E851.999. E838–E840.8 is general material for the later
twentieth century, as shown by the
[Library of Congress E–F outline](https://www.loc.gov/aba/publications/Archived-LCC2022/LCC_E-F2022OUT.pdf).
Those codes cannot establish a 1969 cutoff. The previous repair restored the
political, military and biography subranges, but did not remove this broader
incorrect fallback. Separate general later-century material from the two
administration ranges that actually support the narrower period.

## Medium priority: unnecessarily broad routing

These destinations generally include the subject, but discard an available
more precise meaning:

- HIS027210 is assigned to 3411 (early nineteenth century), despite the restored
  War of 1812 subject 3693.
- HIS050000 maps to Asia 3485 rather than its Central Asian subject 11573.
- HIS055000 maps to Middle East 3633 instead of the Turkish/Ottoman branch.
- DDC 965 maps to North Africa 3357 rather than Algerian 3321.
- DDC 953 maps to Middle East 3633 instead of Arabian Peninsula 3634.
- DDC 948 maps to Europe 3517 instead of the northern European branch.
- HIS006040–HIS006090 all map to Canada 3400, losing their regional meaning.

The BISAC meanings were checked against the official list above; the DDC
meanings against the OCLC summary and bundled headings. These are specificity
improvements, distinct from the wrong-country assignments above.

## Additional candidates requiring manual evidence

- Tunisia 3371 owns DT193–DT193.95, although the same master places Tunisia at
  DT241–DT269 and DT193 material in a broader Maghrib context.
- Vietnam 3513 owns DS556–DS559.93, whose exact master heading is French
  Indochina; the regional/country boundary needs closer inspection.
- The numeric screen reports 261 range outliers. Many are expected false
  positives because countries occupy multiple disjoint ranges or subclasses.
  Do not bulk-reassign these.
- The lexical screen reports 1,642 candidates, many explained by shortened
  labels or valid country-level fallback. Do not treat that count as errors.

## Reproduction and evidence

Run `python3 shared/subject-projection/tools/review-history-assignments.py`.

- `20260915_history_assignment_review.csv`: every assignment and exact master
  headings, including duplicated destinations.
- `20260915_history_range_outliers.csv`: numeric screening candidates.
- `20260915_history_geography_candidates.csv`: master-ancestry disagreement
  screen; empty in this snapshot, illustrating its limitation when both
  datasets share corrupted ancestry.

Recommended repair order: Egyptian assignments and master ancestry; the six
BISAC conflicts; six over-specific DDC country destinations; Liberia and U.S.
period ranges; then improve broad fallbacks. Validate production matcher
destinations on a frozen before/after snapshot before applying range repairs.
