# Remaining supported LCC selectors

Installed **114 exact selectors and 37 inclusive fallback ranges** on existing subjects. The full comparison of 11,316,630 observed LCC strings, plus the final audited JQ correction, recovered **283,675 classification uses across 167,641 strings**, with no lost matches.

The final residual audit found that a concurrent edit had removed bare JQ (182 uses). Added JQ → Politics & Government after verifying the two normalized forms; it was already a recognized numeric subclass, so this exact selector does not change numeric matching. The full residual list then returns to the staged 5,160 strings (9,352 uses).

No existing assignments changed. The two Phenomenology spellings already reached that subject in the immediate pre-installation snapshot; this final comparison incorporates concurrent edits.

## Scope and evidence

The local 2024 LCC outline supports broad class coverage. The detailed local classification reference supplies regional law subclasses missing from that outline and the printed Phenomenology Cutter interval B829.5.A–B829.5.Z. The machine-readable manifest records exact selectors, targets, fallback ranges and reference captions.

Broad ranges cover recognized subclasses under existing subject umbrellas; existing narrower intervals retain precedence. Thirty additional documented law class selectors register previously missing regional prefixes. KZA has its own Law of the sea fallback because it sorts beyond KZ. This is classification routing, not validation that every possible numeric shelfmark is officially assigned.

Ten proposed exact selectors were already covered by concurrent edits and were preserved, including F → History of the Americas and L → Education. Two printed labels already handled by the updated parser, B808.5.A–Z and RG950.A–Z, required no extra selectors. The GN fallback uses Anthropology, preserving its existing matches.

## Validation and publication

- 62 Rust library tests passed.
- 122 selector, Cutter, and boundary checks passed, plus two JQ checks.
- Full before/after comparison uses snapshots immediately around the additive live transaction.
- SQLite integrity and foreign-key checks passed.
- Published a consistent taxonomy snapshot; verified selector totals for 41 affected subjects.
- Usage counts, shared match cache and taxonomy viewer refreshed while holding a consistent database snapshot.

The runtime rejects invented subclasses within broad spans and correctly treats R–R as a whole-class interval. Matcher version 32 also includes the concurrently added alphabetic Cutter-span normalization; final validation used that version.

## Remaining input work

The final installed residual totals are recorded below. During staged review, 5,160 syntax-accepted strings (9,352 uses) lacked a match; 915,946 strings (1,261,442 uses) were rejected by the format parser. These totals describe input strings, not missing subjects or unique books. No ambiguous resolved-candidate rows occurred in this audit.

Among the syntax-accepted residuals, 279 strings (393 uses) begin with locally documented prefixes. They include concatenated classifications, reversed or truncated intervals, unclear Cutter suffixes, and ranges with trailing book metadata such as `P121-143.3 .S66 2021`. Those need parsing/normalization review; adding literal selectors for individual book metadata would hide the underlying issue. Other residual prefixes include W/WB/WY, catalog markers, and prefixes absent from the local LCC reference. They were not assigned arbitrarily.

Next useful work: normalize legitimate range-plus-book-metadata forms, with boundary tests, then investigate concatenated classification fields separately. The selector additions are complete for the reviewed supported gaps; this does not claim universal support for every stored notation.

Migration: `data/migrations/20260907_add_remaining_supported_lcc_selectors.sql`.
Working snapshots, comparisons, scripts and residual audit: `/home/johan/.cache/bokheim/remaining-lcc-selectors`.

Installed residual audit before the final JQ correction (category, distinct strings, uses):

```text
matched	10395523	14405189
no_selector	5161	9534
rejected_format	915946	1261442
```
