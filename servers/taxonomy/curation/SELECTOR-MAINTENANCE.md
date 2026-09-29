# Curated selector maintenance

The curated database owns selector destinations and range boundaries. Master
selectors are reference material, not an authoritative replacement for curated
assignments.

Preview syntax maintenance:

```sh
python3 servers/taxonomy/curation/compile-curated-selectors.py
```

`--apply` applies only syntax canonicalization of existing selectors (including
legacy priority suffixes). It preserves destination IDs, numeric range bounds,
exact exceptions, printed range labels and non-LCC systems. It checks that the
selector rows have not changed since planning. It does not import master rows,
infer wildcards from class headings, merge ranges or prune ancestor selectors.

Export master candidates separately:

```sh
python3 servers/taxonomy/curation/compile-curated-selectors.py \
  --master-proposals /tmp/master-selector-proposals.csv
```

This is read-only for both databases and cannot be combined with `--apply`.
Candidates exclude selectors already assigned in the curated taxonomy and
selectors contained by an existing numeric range on the same destination.
Independent master and curated IDs must also agree in label before the master
concept can be treated as retained. Renamed/unresolved subjects need review.
The CSV includes source and proposed destination IDs and labels.

Apply chosen additions through a separate reviewed migration after comparing
production matcher results against the same frozen before/after taxonomy and
usage dataset. Check coverage additions, losses, changed destinations and
ambiguities; numeric coverage alone does not establish matching equivalence.
Regenerate usage counts and the viewer after changing the taxonomy.

Open Library usage caches must use `normalization_version=2`: preserve complete
notations, trimming only outer ASCII spaces. Internal spaces, Cutters and dates
are classification evidence. Rebuild older caches with `rebuild-code-usage.py`
before running `refresh-curation-usage.sh`; the usage tool rejects versioned
caches with the obsolete first-space truncation. Counts represent classification
rows, not unique books. Match exports are quoted TSV; read them with a CSV reader
configured for a tab delimiter so embedded whitespace survives.

Incoming numeric, cross-subclass and Cutter-ended spans can match a curated
range containing the entire interval. Decimal-shortened upper endpoints such as
`T57.6-.97` inherit the lower endpoint’s whole-number part. A single-letter
upper endpoint after one Cutter inherits the class number (`B829.5.A3-Z`).
Explicit numbered class endpoints remain distinct (`B829.5.A-Z1`). Partial overlap and
matching endpoints across a gap do not establish coverage. Explicit printed-range
assignments keep precedence. Among otherwise tied ranges, a strictly contained
Cutter interval wins; equal or partially overlapping conflicting intervals remain
unresolved. These rules preserve Cutter decimal and upper-bound semantics.
Compare matcher versions on the same frozen taxonomy and cache; preserving full
notations changes the number of distinct strings, so old and rebuilt unmatched
string totals are not directly comparable.

Observed-selector proposals likewise respect the coverage audit. A stale
candidate file cannot turn an already matched or ambiguous code back into an
unmatched code. The coverage audit and candidate files should refer to the same
immutable taxonomy snapshot.

Regression checks:

```sh
python3 -m unittest discover -s servers/taxonomy/curation/tests -v
```

PZ title work marks follow LC Shelflisting Manual G350: after an author Cutter,
two or more title letters may stand alone or precede a book year. These
letters are retained in the full notation. The matcher preserves earlier class
and author keys, so explicit author assignments still win. This exception is
limited to PZ and does not accept arbitrary trailing holdings notes.

Matcher v34 accepts numbered item designations `vol.`, `v.`, `pt.`, `no.`,
`sect.`, and `sess.` after an otherwise valid call number. Compound numeric
parts and volume intervals remain item details, not LCC class intervals.
Full notation and exact item assignments are retained, while subject matching
uses the validated base. Arbitrary notes and box-location phrases remain
excluded. See LC Shelflisting Manual G610/G615 for these conventions.

Matcher v36 also recognizes documented custodial annotations after valid
notations: Orien/Asian/AMED with known language labels, optional Cage, and Hebr.
Angle brackets and the observed parenthesized form preserve the full annotation;
subject matching uses the preceding classification. Unknown tags remain rejected.
`Suppl.` (also `Suppl`) may stand alone or carry a supplement number, and composes
with volume suffixes and recognized custodial notes. These follow CSB 24 and
Shelflisting Manual G622/G155; no subject is inferred from the custody label.

Matcher v37 recognizes validated MLC and microform shelf numbers with an
explicit final one-letter classification assignment. Curated `(P)`-style
selectors refer to these broad assignments and are distinct from ordinary bare
subclass selectors such as `P`. The matcher derives this key only from a valid
complete shelf number; size/custody letters and unassigned shelf numbers are
not subject evidence. Full shelf numbers remain intact. Multi-letter terminal
assignments remain unsupported pending separate evidence.

A top-level subject with its own classification selectors is routable even
without children. Selector-free, childless orphan nodes remain hidden. This
supports explicitly broad subjects without adding artificial descendants or
placing cross-discipline assignments under narrower ancestors. Both runtime
routes and generated SQLite navigation follow this rule.

Matcher v38 recognizes a final space + `MLC` on a validated G-class map call
number, as documented in CSB 36 (Spring 1987), p. 43. Preserve the complete
notation and match using its explicit call number. The marker supplies no
subject, and unrelated or incomplete MLC entries are not accepted by this rule.

Matcher v39 recognizes imported apostrophe-separated call components. This is
a constrained data-normalization rule, not a claim that apostrophes are standard
LC punctuation. It requires complete class/Cutter tokens and an optional final
four-digit year (one-letter year suffix allowed). Numeric fragments, prose,
leading/trailing/repeated apostrophes are not repaired by this rule. Original
notation remains intact; matching uses the validated space-separated form,
including Cutter range boundaries. An explicit original-notation selector can
still win. No taxonomy selector changes are needed.

Matcher v40 extends numbered suffixes to established vernacular volume/part
captions from G615 and the PCC caption list: Bd., Bde., Bdchn., Nr., T., fasc.,
Abt., Lfg., livr., bk., Jahrg., jaarg., kn., knj., cz., d., sv., zesz., vyp.
Heft is documented in G800; H., Hft., Hbd., Halbbd. are documented historical
spine-label captions at Western Libraries. Complete numeric designations only;
multiple known captions can compose. Unknown trailing text still rejects the
notation. The full notation is retained and base call/Cutter matching applies.

Matcher v41 also recognizes the PZ-like title-letter shape following PR/PS
class + Cutter components. This is compatibility with observed historical MARC
imports: original LC records supply PZ $a, title $b, then alternate PR/PS $a;
Open Library attaches the PZ $b to the alternate number as well. MARC 050 says
alternate item data belongs inside that alternate $a, not the earlier $b.
Preserve the imported full notation and the explicit class/Cutter match keys;
do not describe these strings as standard PR/PS work-letter call numbers.
Source evidence: `tmp/lcc-legacy-literature-marks-20260907/source-evidence.json`.

Matcher v42 accepts a complete final four-digit year (optional one-letter suffix)
after a validated numbered volume/part suffix. Parenthesized chronology such as
`v.1 (1999)` is also accepted. The original year and enumeration remain in the
full notation; matching uses the preceding call. Unbalanced parentheses,
multiple bare years, short years and extra text remain rejected.

Matcher v43 accepts numeric combined-issue slashes inside a recognized
enumeration suffix (pt.19/2, no.69/70), colon-separated captioned levels
(v.1:no.2), and complete alternate captioned numbering (v.2:no.5=fasc.15).
The full notation remains intact; classification comes from the preceding valid
call. Incomplete slash tails, unknown captions and injected class numbers reject.
This does not allow splitting concatenated classification fields.

Matcher v44 accepts whitespace after a single Cutter separator period before
its alphabetic component. It preserves the original notation and existing
component keys; class-number decimals are unchanged. This also composes with
literature title marks and numbered suffixes. A second period, a numeric-only
fragment after the separator, or incomplete trailing content remains invalid.

Matcher v45/taxonomy correction removes the unsupported bare BG selector from
Sonambulism while retaining BF1068. Do not broaden BG into an LCC range: the
official B outline does not list it. A selector-derived subclass registry is a
matching mechanism, not independent authority that a prefix belongs to LCC.

Matcher v46 (outline matcher v6) treats the final MLC/microform assignment
parentheses as a boundary, allowing an omitted preceding space. All existing
prefix, year/sequence and one-letter assignment checks remain. Original notation
is preserved. Multiple assignments, malformed shelf numbers and trailing text
are not accepted by this spacing rule.

Matcher v47 / outline v7: accept validated whole-call MARC 050 display brackets.
Preserve full evidence and enclosed numeric/Cutter range matching. See
docs/reports/lcc-display-brackets-20260907.md; 82 release tests, 41 new matches.

Unified matcher v48: constrained interpretation of final period/year in imported complete class/Cutter calls, with full original evidence and exact-year keys retained. See docs/reports/lcc-period-year-20260907.md. 84 release tests; 3,873 new matches, no regressions.

Unified matcher v49 / outline v8 recognizes the literal LC Sociology heading HM(1)-1281 using the existing HM assignment. No generic parenthesis removal or numeric-range expansion. See docs/reports/lcc-obsolete-sociology-heading-20260907.md; 86 release tests, 327 newly covered occurrences.

Unified matcher v50: duplicated Cutter periods in complete imported calls, supported by LC MARC subfield evidence. Original evidence and numeric/Cutter boundaries retained. 88 release tests; 800 new matches. See docs/reports/lcc-duplicate-cutter-periods-20260907.md.

Unified matcher v51: imported PR/PS item marks can follow the class without a Cutter. No author Cutter inferred; full original evidence retained. 90 release tests, 427 new matches. See docs/reports/lcc-literature-without-cutter-20260907.md.

Unified matcher v52 / outline v9: registered terminal table notes on independently valid bases. Table IDs exported by export-lcc-table-identifiers.py from the local LC reference; no law table expansion. Preserve full-span containment and original evidence. 92 release tests, 2,345 new matches. See docs/reports/lcc-registered-table-notes-20260907.md.

Unified matcher v53: complete bracketed book years normalize for matching while original evidence and exact-year keys remain. G140 source guidance; 94 release tests, 120 new matches. See docs/reports/lcc-bracketed-book-years-20260907.md.

Unified matcher v54: validated Roman volume/part enumeration after existing captions. LC holdings standard documents Roman enumeration. Preserve full original evidence and base subject. 95 release tests, 143 new matches. See docs/reports/lcc-roman-volume-numbers-20260907.md.

Unified matcher v55 / outline v10: explicit one-letter class assignments on validated MLC X-placeholder forms. Preserve original evidence and broad main class; no subclass inference. 97 release tests, 20 new strings / 1,976 occurrences. See docs/reports/lcc-mlc-placeholder-assignments-20260907.md.

Unified matcher v56 / outline v11: explicit known non-law subclass assignments on completed MLC shelf numbers. Existing heading selectors only, original evidence retained. 99 release tests, 7,201 new matches. See docs/reports/lcc-mlc-subclass-assignments-20260907.md.

Unified matcher v57 / outline v12: whitespace at MLC year/sequence and assignment delimiters. Split digits and incomplete fields remain invalid; original evidence retained. 100 release tests, 92 new matches. See docs/reports/lcc-mlc-delimiter-spacing-20260907.md.

Unified matcher v58 / outline v13: full declared Cutter-span evidence in constrained preliminary .x constructions with registered table notes. No first-endpoint or invented-country selection. 102 release tests, 202 new matches. See docs/reports/lcc-variable-cutter-span-evidence-20260907.md.

Unified matcher v59 / outline v14: validate every item in terminal table lists, including numeric IDs with an explicit preceding family. Preserve the entire base span and original evidence. 104 release tests; 22 new matches / 71 occurrences. See docs/reports/lcc-multiple-table-references-20260907.md.

Unified matcher v60 / outline v15: preliminary initial forms after explicit Cutter spans; interpret before table-note removal. Full-span containment and original evidence retained. 106 release tests; 304 new matches / 695 occurrences. See docs/reports/lcc-preliminary-cutter-initials-20260907.md.

Unified matcher v61 / outline v16: terminal slash on constrained class/Cutter prefixes, interpreted as whole filing families. Preserve original evidence; do not match just a lower endpoint or let broad exact ancestors block containing ranges. 108 release tests; 9,723 new strings / 20,230 occurrences. See docs/reports/lcc-terminal-slash-prefixes-20260907.md.

Unified matcher v62 / outline v17: abbreviated upper Cutters in registered table-annotated spans inherit their class and preceding Cutters. Full containment and original evidence retained. 110 release tests; 60 new matches / 84 occurrences. See docs/reports/lcc-secondary-cutter-ranges-20260907.md.

Unified matcher v63 / outline v18: correct embedded BG1068–BG1073 to BF1068–BF1073, Sleep. Somnambulism under Parapsychology. BG must not enter the MLC known-subclass registry. 111 release tests; full cache has unchanged curated matches. See docs/reports/lcc-outline-bg-correction-20260907.md.

Unified matcher v64 / outline v19: exact repetitions of complete validated units with known subclasses. Preserve original evidence and whole-span containment; do not generalize to mixed or truncated concatenations. 113 release tests; 40 new matches / 566 occurrences. See docs/reports/lcc-exact-repeated-notation-20260907.md.

Unified matcher v65 / outline v20: qualified table intervals on complete Cutter prefixes. Only exact documented relative spans from the reproducible export-lcc-relative-table-spans.py registry; retain full interval and original evidence. 115 release tests; 227 new matches / 302 occurrences. See docs/reports/lcc-qualified-table-spans-20260907.md.

Unified matcher v66: numbered ser./series and new-series enumeration after complete calls. Preserve the base subject and original evidence; validate the entire suffix. 117 release tests; 3,647 new matches / 3,789 occurrences. See docs/reports/lcc-series-enumeration-20260907.md.

Unified matcher v67: historical one-letter PZ title work marks after complete author Cutters, documented by G350 section 8. Preserve author evidence and exact original selectors. 119 release tests; 870 new matches / 937 occurrences. See docs/reports/lcc-single-letter-title-marks-20260907.md.

Unified matcher v68 / outline v21: terminal plus on documented non-law reference starts uses the widest recorded complete scope, including the IN PROCESS bracket wrapper. Preserve original evidence; never reduce a range start to a point. Unsupported reference endpoints exclude the entire start. Reproduce data with export-lcc-reference-scopes.py. 121 release tests; 3,248 new matches / 9,750 occurrences, no regressions. See docs/reports/lcc-plus-reference-spans-20260907.md.

Unified matcher v69 / outline v22: inline relative intervals before a registered table name use the same documented interval registry and complete Cutter-prefix validation as qualified notes. Full-span containment and original evidence retained. 122 release tests; 14 new strings / 19 occurrences, no regressions. See docs/reports/lcc-inline-table-spans-20260907.md.

Unified matcher v70 / outline v23: preliminary Cutter spans inherit complete preceding Cutters on the upper endpoint and may end in a complete four-digit year before a registered table note. Preserve whole-span containment and original evidence; never infer a final language/title Cutter. 124 release tests; 96 new strings / 149 occurrences, no regressions. See docs/reports/lcc-dated-nested-spans-20260907.md.

Unified matcher v71 / outline v24: initial-only final prefixes with exact documented table intervals require coverage of their whole filing family. Do not append relative suffixes or infer author digits. Complete Cutter prefixes retain precise interval expansion. 126 release tests; 81 new strings / 113 occurrences, no regressions. See docs/reports/lcc-table-initial-families-20260907.md.

Unified matcher v72 / outline v25: 18 source-backed GB/GV reference upper-endpoint corrections applied only through the pinned lcc-reference-scope-corrections.json manifest. Exporter verifies original row and source identity; original reference rows are preserved. 127 release tests; 18 corrected reference scopes now match, with no Open Library match changes. See docs/reports/lcc-reference-gb-corrections-20260907.md.

Unified matcher v73 / outline v26: 23 additional record-specific endpoint corrections from exact printed ranges, with per-entry pinned source identity. Corrected intervals deduplicate against newer reference rows; no general imported-code rewriting. 128 release tests; reference residual 261, no Open Library match changes. See docs/reports/lcc-reference-printed-corrections-20260907.md.

Unified matcher v74 / outline v27: 18 reviewed Q→QE upper-endpoint corrections for descriptive mineralogy in the pinned correction manifest. Preserve original rows; no generic imported-code rewriting. 129 release tests; 16 distinct plus starts verified against widest scopes; reference residual 243, no Open Library match changes. See docs/reports/lcc-reference-qe-corrections-20260907.md.

Unified matcher v75 / outline v28: three source-backed endpoint corrections unblock BP65.A1+, GV1131+ and PN1991.77.A1+, preserving whole ranges. 130 release tests; 3 new strings / 6 occurrences, no regressions; reference residual 240. See docs/reports/lcc-reference-used-endpoints-20260907.md.

Unified matcher v76 / outline v29: complete repetitions of one numeric interval may alternate full and abbreviated upper-subclass notation. Require identical endpoints for every complete copy; reject mixed ranges and truncated tails. Preserve full containment and original evidence. 131 release tests; 6 new strings / 135 occurrences, no regressions. See docs/reports/lcc-equivalent-range-repetitions-20260907.md.

Unified matcher v77 / outline v30: literal MARC $a/$b display syntax retains classification and item portions in a validated call. Do not concatenate alternative $a fields or strip arbitrary subfields. 133 release tests; 398 new strings / 431 occurrences, no regressions. See docs/reports/lcc-marc-subfield-calls-20260907.md.

Unified matcher v78 / outline v31: explicit semicolon lists assign the union of independently unambiguous component subjects. Preserve the whole source code and evidence position; plural outline paths retain all components. Diagnostic multi-subject list results differ from single-code ambiguity; audit examples check production assignment. Every component must validate and resolve. 136 release tests; 1,243 new strings / 1,277 occurrences, no existing curated regressions. All new cache concept sets verified. See docs/reports/lcc-explicit-code-lists-20260907.md.

Unified matcher v79 / outline v32: reject unmarked single-letter integer components after semicolons because they can be item Cutters. Two linked MARC records confirm this risk. Withdraws 569 uncertain v78 gains / 571 occurrences; no other assignment changes. 137 release tests pass; all 674 retained and 569 withdrawn v78 cache assignments verified. Component-union equivalence alone does not prove a separator means multiple classes. See docs/reports/lcc-safe-list-boundaries-20260907.md.

Unified matcher v80 / outline v33: table ranges support repeated or omitted complete preceding Cutters, terminal alphabetic endpoints and complete years. Preserve whole-span containment and original evidence. Published B133.S5A6-.S5Z and BL4 .xA6-.xZ confirm the construction. 139 release tests; 8 new strings / 9 occurrences, no existing assignment changes. All expanded intervals and cache concept sets verified. See docs/reports/lcc-nested-table-endpoints-20260907.md.

Unified matcher v81 / outline v34: preliminary table spans allow exactly repeated preceding Cutters before the upper Z and final language/geographic initial. Preserve the full interval; do not invent a final Cutter or accept partial/different prefixes. 140 release tests; 128 new strings / 200 occurrences, no existing assignment changes. All expanded ranges and exported concept sets verified. See docs/reports/lcc-repeated-preliminary-prefixes-20260907.md.

Historical DS table review: DS7/DSVII/DS-DXVII biography imports must not alias current DS-DX7, which is the Jews outside Palestine table. G320 supplies a separate explicit biography construction. 36 candidate full-Cutter A6-Z expansions / 73 occurrences resolve, but are not yet production gains. See docs/reports/lcc-historical-ds-biography-review-20260907.md.

Unified matcher v82 / outline v35: historical DS biography labels support explicitly stated G320 operations only within 219 exported first-Cutter individual-biography scopes. Do not alias them to current DS-DX7. Complete Cutters retain full operations; missing digits retain the whole initial family. 142 release tests; 67 new strings / 113 occurrences, no existing assignment changes. All expanded intervals/families and cache assignments verified. Reproduce scopes with export-lcc-biography-scopes.py. See docs/reports/lcc-historical-biography-tables-20260907.md.

Unified matcher v83 / outline v36: exact registered relative operations after unresolved outer Cutter intervals preserve the whole outer interval. Do not invent a specific person/place Cutter or choose a partial-range subject. 144 release tests; 35 new strings / 78 occurrences, no existing assignment changes. All expanded outer ranges and cache assignments verified. See docs/reports/lcc-nested-variable-tables-20260907.md.

Unified matcher v84 / outline v37: a separate numeric-relative registry validates nested table operations and preliminary final initials while preserving the whole outer Cutter interval. Do not append digits to class numbers or infer completed place/person Cutters. 146 release tests; 127 new strings / 200 occurrences, no existing assignment changes. All outer ranges and cache assignments verified. Reproduce with export-lcc-numeric-relative-spans.py. See docs/reports/lcc-numeric-nested-tables-20260907.md.

Unified matcher v85 / outline v38: numeric qualified-table operations continue complete Cutter digits, preserving significant trailing zeroes; incomplete initials retain whole filing families. No class-number-only expansion. 148 release tests; 4 new strings / 5 occurrences, no existing assignment changes. All expanded ranges/families and cache assignments verified. See docs/reports/lcc-numeric-qualified-tables-20260907.md.

Unified matcher v86 / outline v39: unqualified .x positions require an exact published outer interval and an explicitly defined position, inline or in a named table. Preserve the full range and original evidence; never infer a completed country/person Cutter. 150 release tests; 80 new strings / 223 occurrences, no existing assignment changes. All range/position combinations and cache assignments verified. Reproduce with export-lcc-placeholder-positions.py. See docs/reports/lcc-documented-placeholders-20260907.md.

Unified matcher v87 / outline v40: preliminary plus ranges require the widest documented scope to end at the same-number Z boundary. Preserve the entire interval; never infer a completed Cutter from a preliminary initial. 152 release tests; 598 new strings / 1,636 occurrences, no existing assignment changes. All full-range equivalences and cache assignments verified. See docs/reports/lcc-documented-preliminary-plus-20260907.md.

Unified matcher v88 / outline v41: 105 bare-Z reference endpoints corrected only through pinned source-specific records with exact printed lines and captions. Export validates source lines; 412,107 scopes, 216 blocked starts. All 105 full-range comparisons and 152 release tests pass. One new string / two occurrences, no existing assignment changes; full cache verified. See docs/reports/lcc-bare-z-reference-endpoints-20260907.md.

Unified matcher v89 / outline v42: 52 dot-prefixed reference endpoints restored through exact pinned printed ranges and captions, allowing trailing table citations and NFC equivalence during review. 412,160 scopes / 164 blocked starts; reproducible export. All 52 scope comparisons and 152 release tests pass. Full usage audit finds zero assignment changes, so usage/viewer/cache remain valid and are retained. See docs/reports/lcc-dot-reference-endpoints-20260907.md.

Unified matcher v90 / outline v43: recognized Unicode hyphens and numeric references require a complete ordered non-law range; observed #8209; imports retain both endpoints. 154 release tests; one new string / 75 occurrences, no existing assignment changes. Independent full-range equivalence and exported assignment verified. See docs/reports/lcc-encoded-range-hyphens-20260907.md.

Unified matcher v91 / outline v44: unfinished terminal item captions preserve a valid preceding call and all original evidence. Never infer missing volume/part numbers; standalone bare V requires a preceding numbered level. Law unchanged. 156 release tests; 510 new strings / 807 occurrences, no existing assignment changes. All preceding-call equivalences and exported assignments verified. See docs/reports/lcc-unfinished-enumeration-20260907.md.

Unified matcher v92 / outline v45: explicit single-Cutter ranges ending at Z retain their exact documented interval even when a wider heading shares the start. Open-ended plus starts still use the widest scope. 158 release tests; four new strings / 33 occurrences, no existing assignment changes. All full-interval equivalences and cache assignments verified. See docs/reports/lcc-explicit-closed-plus-ranges-20260907.md.

Unified v95 / outline v48 adds numeric P-PZ scopes for 499 exact unmodified bindings without local exceptions; 19 new strings / 60 uses, 164 release tests. Full audit and cache verification use the recorded export snapshot; subsequent concurrent taxonomy edits require fresh current-state checks. See docs/reports/lcc-numbered-table-scopes-20260907.md.

Unified v98 / outline v51: BX870 and DC240 chronological call numbers retain date and item specificity; valid date/month forms and complete item suffixes only. Full v97 comparison: 16 gains / 18 uses, no changed existing assignments. See docs/reports/lcc-dated-calls-20260907.md. Cached usage/viewer exports need regeneration before current-version coverage claims.

Unified v99 / outline v52: three parent endpoint corrections (AS602.5, AS702.5, B2556), pinned printed hierarchy evidence; exact three-row registry diff; full interval equivalence verified. No observed plus-bearing usage strings for these starts. See docs/reports/lcc-reviewed-parent-endpoints-20260907.md.

Unified v100 / outline v53: 144 documented literal-date positions (138 unmodified BS2 bindings, six direct templates), preserve DATE and complete base; no inferred year. Reproduce with export-lcc-literal-date-scopes.py. Full audit: one gain / one use, no existing assignment changes. See docs/reports/lcc-literal-date-scopes-20260907.md.

Unified v103 / outline v56: exact source-reviewed call bases require preserved edition and linked MARC evidence; no inferred year or neighboring-code aliases. One gain / one use, no existing assignment changes. Reproduce with export-lcc-reviewed-call-bases.py. See docs/reports/lcc-reviewed-call-base-20260907.md.

Unified v105 / outline 58: six exact source-reviewed unmarked preliminary intervals retain both explicit endpoints and original evidence. Validate linked MARC sources with export-lcc-reviewed-preliminary-spans.py. Do not replace trailing initials with guessed language/author Cutters or generalize these reviewed forms to unverified inputs. See docs/reports/lcc-unmarked-range-review-20260907.md.
