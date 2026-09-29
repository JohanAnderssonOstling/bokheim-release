# Useful remaining LCC repairs

Status: complete and verified against all 11,316,630 local LCC strings. Compared with the previously accepted audit, 1,506 additional strings match, covering 3,444 uses. No existing assignment was removed. Remaining unmatched: 119,825 strings / 220,520 uses.

## Changes

- Normalize the published comparative-law heading `K(520) 5582` (also the printed hyphenated form) to the full `K520-5582` range.
- Recover a common subject from two to four slash-separated, known bare subclasses. Every subclass must match independently. `KJ/KK` recovers Law and `PS/PZ` recovers Literature. Alternatives with no useful common ancestor, including `RC/BF`, remain unresolved. This does not assign both alternatives as established subjects.
- Treat a trailing numeric `+` as an unfinished range and a trailing numeric `*` as an unfinished numeric prefix. Preserve the original as unresolved evidence and assign only a shared ancestor. Starred prefixes include digit continuations and intersecting ranges, including same-subclass numeric ranges where neither endpoint starts with the prefix. Generic cross-subclass headings do not obscure represented numeric completions. Existing strict matches retain precedence.
- Expand an author's literary `Z5-Z999` criticism interval with the preceding author Cutters retained at both endpoints.
- Accept unmarked preliminary Cutter spans such as `.A-ZI` only when the local LC reference data verifies the complete outer span. The trailing initial cannot select a country or author. Ordinary complete `.A-Z` ranges retain their existing canonical form.

## Scope and remaining uncertainty

The focused inventory contained 125 previously unmatched canonical numeric range strings. The optimized probe confirms 22 now match, covering 25 uses. The other 103 remain unresolved. Their structural triage is 27 damaged numeric forms, 48 preliminary/table expressions, and 28 possible cross-class ranges or item separators; these categories are investigation aids, not source-verified diagnoses. No digits were invented, removed, or swapped to force coverage.

Suffix recovery is intentionally broad. A single represented completion yields a proper parent; it does not establish that this is the only possible completion. Bare subclass alternatives instead permit their agreed subject when every alternative independently selects it. Processing labels and unknown prefixes are still not classification evidence.

The taxonomy database was not edited or replaced. Verification uses the frozen schema-8 taxonomy because concurrent live taxonomy work has a different schema. Existing storage provenance carries these results without a schema change.

## Verification

- 237 optimized release projection tests pass.
- Optimized release server storage/backfill/enrichment-position integration test passes using the isolated schema-8 fixture.
- Evidence validation covers 212,632 inputs, including retained original strings, strict evidence, ambiguity readings, and shared ancestry.
- Independent numeric support reconstruction and subclass-alternative support checks pass.
- Full-inventory comparison agrees with all 212,632 evidence reports; there are no changed inputs outside that audit pool. All 31 source/data fingerprints match the verified snapshot. All previous assignments are retained.
- Independent support reconstruction passes for 3,236 numeric support sets and nine subclass-alternative rows.
- 286 previously matched strings gained additional assignments; none lost an existing assignment.
- Matcher versions: LCC 80, unified 130. Final accepted candidate: 65.

## Sources

- [Library of Congress law outline](https://www.loc.gov/catdir/cpso/lcco/lcco_k.pdf): comparative-law heading.
- [Library of Congress literature training, module 12.5](https://loc.gov/catworkshop/lcc/PDFs%20of%20slides/12-5%20handout.pdf): author-level Z5–Z999 criticism arrangement.
- Byte-verified public MARC records and local review artifacts are preserved under `/home/johan/.cache/bokheim/safe-lcc-repairs-20260907/`. They confirm `PK2166+` and `KJ/KK` appear in source 050 fields. Sample slash classifications also coexist with complete call numbers; the implementation retains this uncertainty rather than selecting an unsupported precise interpretation.
