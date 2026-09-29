# Broader taxonomy reconciliation implemented

Implemented the concrete branch and scope findings from
[the broader review](20260920-broader-pattern-review.md). The curated taxonomy
now has **16,910 subjects**, down from 16,961. The change includes 52 merges,
two code-free wrapper removals, three new scope-specific subjects, and revised
labels and parents. Original reference/master databases were not modified.

## Scope corrections

- The broad RC552.A–RC552.Z span now belongs to Psychopathology, not Eating
  Disorders. Documented phobia subdivisions resolve to Anxieties & Phobias;
  documented eating-disorder subdivisions resolve to Eating Disorders. Other
  subdivisions retain broader coverage. PTSD retains its specific assignment.
- Ancient, Medieval, and Modern Aesthetics are distinguished from general
  philosophy. BH83 now resolves to Origins of Aesthetics. DDC 189 and 190 remain
  general medieval and modern philosophy. Comparative aesthetics and the
  ancient-aesthetics children are back under aesthetics history.
- The B162.6 subject previously labeled Ancient Eastern Religious Thought is
  now correctly labeled Shinto Philosophy. Shinto, Sikh, Taoist, Jain, Jewish,
  and Buddhist philosophy are under Philosophy of Religion, without inherited
  ancient or modern chronological restrictions.
- Coalesced French/German 21st-century philosophy and Dutch/Belgian
  19th–20th-century philosophy ranges have separate, appropriately named
  subjects. No printed classification boundaries were discarded.

## Branch reconciliation

| Reviewed area | Implemented behavior |
|---|---|
| Biology and Life Sciences | One Biology branch; Life Sciences' codes and children transferred. The narrower Life branch is named Biological Processes & Functions. |
| Zoology, Botany, Microbiology | Canonical branches under Biology, combining source-system codes and children. Duplicate animal groups, virology, bacteriology, and animal behavior reconciled. |
| Ecology | The LCC/DDC, SCI, and NAT classifications share one Ecology concept. Popular versus academic publishing category is not used to create separate subject identities. |
| Other science duplicates | Molecular Biology, Biophysics, Evolution, Astrophysics, Electrochemistry, Relativity Physics, Geology, Mineralogy, and Paleontology reconciled. Molecular Biology and Biophysics retain meaningful alternative biological contexts. |
| Construction | Civil/Civil Engineering and Construction/Building construction combined; HVAC, Plumbing, and Industrial Engineering duplicates merged. |
| Mental health | Shared Mental Health and Psychopathology branches; matching disorders combined after range correction. Dementia, memory disorders, sleep disorders, and other distinct scopes remain separate. |
| Literary criticism | French, German, and Italian criticism each use one concept accessible through both criticism and literary-tradition routes. |
| Literary geography | French, Italian, and Portuguese literature headings clarify their subject; the former Spain heading is Spanish-Language Literature. Argentine and Mexican Spanish-language subjects explicitly retain their language scope. |
| Philosophy navigation | Historical Periods flattened; Regional Modern Philosophy combined into Modern Philosophy. Country-specific periods retain country context, while general modern centuries sit under Modern Philosophy by Era. |
| Religion | Practical Theology is no longer incorrectly subordinate to professional Christian Ministry. Missions, Evangelism/Revivals, and Christian Science source duplicates are consolidated. |
| Other wrappers | Cards & Invitations retains NC1860 directly instead of a General & Technique leaf. Computer Science absorbs its overlapping Foundations & Computer Science wrapper. The code-free science history/philosophy wrapper is flattened and its overlapping philosophy subjects combined. |
| Colonial ancestry | General Guyanese and Surinamese history stays geographic; only relevant colonial periods link to British/Dutch America. Independent-period history no longer appears under those colonial parents. |
| Vietnamese history | The formerly countryless reunification grouping is combined with the existing Vietnamese reunification period; the 1979 conflict inherits Vietnamese history. |

The two broad B/G fallback subjects are deliberately retained. Their codes span
multiple disciplines, so attaching them to one narrower discipline would create
a new scope error. This change does not invent a hidden-fallback UI mechanism.
Other useful geographic, chronological, and audience groupings are retained;
the review's mechanical candidate inventory was not treated as an automatic
merge/delete list.

## Verified outcomes

- Maximum taxonomy depth: **10 → 9 levels**.
- Routes with eight or more levels: **742 → 684**.
- Health & Medicine maximum: **8 → 7**; eight-level routes: **13 → 0**.
- Philosophy maximum: **8 → 7**; eight-level routes: **35 → 0**.
- All 11,977,153 observed DDC/LCC strings compared before and after: **84,267
  changed destinations; zero coverage losses**.
- 41,105 selector and range-endpoint probes: **120 changed destinations; zero
  coverage losses**. This includes selector expressions that are not individual
  call numbers, in addition to ordinary endpoints.
- Every selector/range row compared. Differences beyond canonical-ID merges are
  the documented RC552 specificity additions and broader-range correction,
  aesthetics/DDC transfers, and country-period splits.
- The observed non-merge differences comprise the intended scope corrections
  plus reviewed ancestor fallback or redundant-ancestor suppression changes.
  For example, RC552* now falls back to Psychiatry rather than Internal Medicine,
  and some composed zoology calls no longer retain a redundant Biology assignment.
- Six new release regression tests pass, covering corrected scopes, shared
  identities, regional context, religious philosophies, and colonial ancestry.
- Release library suite: **249 passed, four failed before and after**. The same
  pre-existing failures concern QA76. normalization, a root Humanities fixture,
  Russian geography route wording, and decorative-arts route wording. No new
  library-test failure was introduced.
- Migration replay, SQLite integrity, foreign keys, cycle checks, and duplicate
  sibling-label checks passed. The live database was checked against the frozen
  baseline before applying, then checked for exact agreement with the preview.
- Usage counts were regenerated from the full observed LCC audit, and the
  standalone viewer was rebuilt against the final graph.

## Reproducible artifacts

- Migration
- [Exact edits and ID mappings](20260920-broad-taxonomy-reconciliation.json)
- [Validation counts, before/after depths, and database hash](20260920-broad-reconciliation-validation.json)
- [Every changed observed classification](20260920-broad-classification-changes.tsv)
- [Completion checks](20260920-broad-completion-checks.json)
- Regression tests: `shared/subject-projection/tests/curated_scope_regressions.rs`.
  Run `cargo test --release -p subject-projection --test curated_scope_regressions`.

Changes are local. Deployment and release-number allocation were not performed.
