# LCC visible expansion policy

The detailed MDSConnect classification snapshot is a curation source. Its
records are not automatically equivalent to visible unified-taxonomy concepts.

## Structural reconciliation

Every imported concept must extend the unified graph rather than reproduce an
existing branch. Concepts with the same normalized preferred label may not be
siblings, and a concept may not repeat the normalized preferred label of its
direct parent. Identical source selectors paired with the same normalized label
must resolve to one concept, with multiple parent routes retained on that
concept when the subject genuinely belongs in more than one place.

Repeated contextual navigation labels at different geographic or chronological
levels are not automatically equivalent. For example, a province may have its
own `Regions & Administrative Divisions` group beneath the national group.
Those nodes describe different scopes and remain distinct; only redundant
wrappers and equivalent source records are contracted.

## Individual authors

Do not promote named-author children when extending an existing LCC-backed
leaf by one visible level.

The deterministic initial rule excludes descendants of any current leaf whose
route contains `individual author` after case folding. This covers captions
such as:

- `Individual authors`
- `Individual authors or works`
- `Individual authors and works to 1400`
- `Individual authors, 1701-1900`

Keep the existing grouping or period node and all of its LCC selectors. Keep
the detailed MARC records in the development-time structural snapshot. Only
the proposed named-author concepts are excluded from the visible hierarchy.

Named authors belong in the author/creator index, where identity, aliases, and
works can be represented properly, rather than in the subject taxonomy.

An explicitly approved exception (2026-09-07) retains the 50 reviewed American
author shelves listed in `reports/20260907-american-top50-author-installation.md`.
These are LCC browsing subjects beneath their existing American literature era
nodes, covering works by and material about each author; they do not provide
creator identity or attribution. Keep the era selectors for residual coverage.
The subsequently approved 100 largest local author-code groups in English
Literature, 1961–2000 are also retained: 100 selectors form 99 subjects after
combining Marion Chesney and M. C. Beaton. The exact scope and evidence are in
`reports/20260907-english-top100-author-installation.md`.
These bounded exceptions do not enable automatic named-author expansion in
other schedules or for additional authors.

Against the 2016 MDSConnect snapshot and the current unified taxonomy, this
rule covers 31 existing LCC leaves and excludes 2,327 confirmed immediate-child
candidates. The explicit one-level candidate pool is reduced from 21,199 to
18,872 before applying other curation rules.

## Subject bibliographies

Subject-bibliography topics are document-function concepts, not alternate
routes to the corresponding ordinary subjects. For example, `Medicine` below
`Subject bibliography` means bibliographies about medicine and is distinct
from the ordinary `Medicine` concept.

Retain the 171 immediate LCC topics in `Z5051-Z7999`, but place them below
contextual navigation groups rather than as 171 direct siblings. Every topic
keeps its detailed LCC selector and exact historical source caption. The
navigation groups intentionally carry no classification selector: assigning
them a continuous range would falsely cover unrelated interleaved LCC topics.

Modernize harmful or obsolete visible terminology while preserving the source
wording in `source_label`. This applies, for example, to the historical labels
`Blind, The`, `Deaf-mutes`, and `Mental retardation. People with mental
disabilities`.

## Protestant denominations

Treat named denominations and religious movements as semantic subjects, but
exclude alphabetic filing-guide records such as `Baptists - Catholic Apostolic
Church`. The 2016 snapshot contains 162 immediate records beneath `Other
Protestant denominations`: 134 semantic subjects and 28 filing guides.

Place the semantic subjects below tradition-oriented navigation groups instead
of exposing a flat alphabetical list. When an equivalent unified concept
already exists elsewhere in the Protestant hierarchy, reuse it with an
additional route and detailed LCC selector. This currently applies to Baptist,
Lutheran, Mennonite, Methodist, Pentecostal, Presbyterian, Quaker, Shaker, and
United Church of Christ concepts.

## Decorative arts reconciliation

For `Other arts and art industries`, prefer existing unified concepts over
parallel LCC-derived concepts. Add the detailed NK selector and an additional
route only when the meanings are equivalent. Combined source captions may map
to an existing grouping, as with `Dolls and dollhouses` and `Toys, Dolls &
Miniatures`.

Do not force a match for a merely related concept. Narrow branded, geographic,
animal, food, and object collectible captions remain covered by the existing
parent selector until a suitable reusable grouping is available.

## Electricity and magnetism

Keep physical phenomena in `Science & Nature / Physics` distinct from their
engineering applications beneath `Applied Sciences & Technology`. For
example, Plasma Physics is not Plasma Engineering, and Electric Current is not
Power Transmission.

Retain the 54 semantic immediate QC subdivisions beneath Electricity and
Magnetism, grouped into scientific topics and contextual reference/history
areas. Exclude the two `Special topics, A-Z` filing guides. Reuse an existing
concept only when it is genuinely equivalent: the electrons-and-protons span
reuses `Nuclear & Particle Physics`, while the new `Quantum Electrodynamics`
concept also receives a route beneath existing `Quantum Theory`.

## Veterinary medicine

Represent the medical and agricultural views of Veterinary Medicine as one
multi-route concept, not separate BISAC and LCC concepts. Preserve both the
`Health & Medicine / Clinical Specialties` and `Applied Sciences & Technology /
Agriculture / Veterinary Health & Surgery` routes and their existing children.
Keep the legal schedule concept separate and label it `Veterinary Law &
Regulation`.

Retain 63 semantic immediate SF subdivisions beneath contextual veterinary
groups. Do not reuse superficially similar human-medicine concepts: Veterinary
Immunology and Veterinary Surgery are specifically about animal medicine. The
existing BISAC Veterinary Surgery concept is equivalent and is reused. Exclude
the two `A-Z` filing guides.

## Library and information science

Distinguish the discipline from library institutions and holdings. Place
`Library & Information Science` and `Libraries & Collections` as siblings
beneath `Libraries, Archives & Information Science`; do not retain the
redundant `Libraries / Library science. Information science` route.

Move existing DDC concepts for operations, personnel, physical plant,
library/archive relationships, and media use into the appropriate disciplinary
groups. Reuse existing LCC concepts for information services and library
collections. Retain 60 semantic immediate Z subdivisions beneath six groups
and exclude the two `A-Z` filing guides.

## Catholicism

Retain 60 semantic immediate BX subdivisions beneath contextual groups for
reference and history, doctrine and relations, governance, worship and
religious life, and institutions and biography. Exclude the two alphabetic
filing guides.

Do not reuse a broad Christian concept when doing so would incorrectly project
all of its books into Catholicism. Catholic sermons, sacraments, monasticism,
pilgrimages, and saints therefore remain contextual concepts. Reuse the
existing `Liturgy & Ritual` concept because its selector already represents the
same Catholic BX range, and reuse the specifically Catholic canon-law concept
with an additional religion route.

## Labor, work, and working class

Retain the 57 semantic immediate HD subdivisions beneath contextual groups for
reference and history, employment and labor markets, working conditions, labor
relations, unions, employment types, social protection, society, and politics.
Exclude the `By industry or trade, A-Z` filing guide.

Reuse the existing general concepts for `Unions`, `Wages & Compensation`, and
`Labor & Industrial Relations` with additional routes and detailed HD selectors.
Do not reuse similarly named labor-law or social-insurance concepts whose existing
classification context is specifically legal or jurisdictional.

## Public health

Merge the equivalent BISAC `Public Health` and LCC `Public health. Hygiene.
Preventive medicine` concepts into one multi-route concept. Retain the 57
semantic immediate RA subdivisions beneath contextual groups and exclude the
`Other subjects of public health, A-Z` filing guide.

Reuse existing general concepts for health risk assessment, environmental
health, epidemiology, and emergency medical services. Keep headings that are
specifically framed by public-health prevention or administration contextual;
for example, do not replace `Mental Health & Mental Illness Prevention` with a
broader generic mental-health concept.

## Management and industrial management

Retain the 53 semantic immediate HD subdivisions beneath contextual groups for
reference and study, leadership and organization, strategy and innovation,
operations and quality, and finance and risk. Exclude the `Other, A-Z` and `By
region or country, A-Z` filing guides.

Reuse the existing business concepts for leadership, organizational behavior,
public relations, quality control, total quality management, and valuation.
Keep related but contextually narrower engineering automation and software
business-intelligence concepts separate.

## Frozen remaining one-level expansion

The bulk expansion freezes the set of LCC-backed leaves present before the
migration and adds only their immediate structural children. It does not
recursively expand the newly created leaves.

Match snapshot parents using both their classification ranges and full caption
routes. Reject class-only or ambiguous caption matches. Jurisdiction-specific
law schedules remain eligible and receive contextual child concepts. Generic
cross-schedule law topics may aggregate exact or explicitly aliased captions,
but only records with concrete letter-prefixed LCC ranges become selectors;
table-relative numbers are never presented as call-number ranges.

Exclude individual-author routes, named-author records that appear without an
author umbrella, individual-institution authority routes, geographic subdivision
containers, local-history place schedules, and A-Z filing records. Regional
history retains periods, events, political, diplomatic, military, social,
economic, historiographic, antiquities, and chronological travel subjects while
also retaining ethnographic subjects such as national characteristics and
communities in foreign countries, and rejecting locality and bibliographic
apparatus. Geography retains semantic
cartography, environmental-science, oceanography, hydrology, and landform detail
while rejecting map/atlas place schedules. When collapsed selectors match both
a structural parent and its descendants, expand only the highest matched record
to preserve a single new visible level.

Existing concepts are normally reused only when both the detailed LCC range and
normalized source label agree and the added route cannot create a cycle. United
States presidential administrations are the deliberate exception: unique
caption matches reuse the existing administration concepts and route new or
reused administrations through the existing `By presidential administration`
branch rather than duplicating them beneath century nodes.

The Ukraine history and ethnography leaves use controlled structural alignments
because their imported outline selectors lost decimal points. Oceanographic
expeditions is aligned in the same way but remains non-expandable: its immediate
records are collective/form and named-expedition filing divisions. Nepal, Sri
Lanka, Bhutan, and Goa remain unexpanded because the retrospective snapshot has
no matching structural history schedules; no detail is inferred from unrelated
country occurrences elsewhere in LCC.

The frozen manifest accounts for 3,403 starting leaves whose strongest LCC
selector had priority 3 or lower: 2,420 expanded, 325 excluded with an explicit
reason, and 658 with no semantic immediate children after the subject and filing
filters. No starting leaf remains unresolved. The expansion contains 20,306
child placements: 20,092 new concepts and 214 existing-concept reuses, plus two
routes connecting the existing Bush and Obama concepts to the presidential
administration branch. Generated selectors use priority 4 so they extend the
broad outline while preserving narrower hand-curated priority-4/5 mappings.

Before records become visible, enforce structural span containment. A child
whose cross-class range remains inside its structural parent is retained, which
preserves legitimate schedules such as Indian law across KNT-KNU. When a range
escapes its parent, repair a mistaken start or end class prefix only if exactly
one candidate restores containment; reject the record when the correction is
ambiguous. This converts the malformed Organic substances and compounds span
`GC118-QC118.2` into `GC118-GC118.2` from its enclosing Chemical oceanography
span instead of relying on a record-specific override.

## France history reconciliation

Use one visible chronological route beneath France: `France / By period`, with
early/medieval and modern branches below it. Nest the 16th, 19th, 20th, and 21st
centuries and the Revolutionary and Napoleonic period beneath the appropriate
chronological parent rather than exposing them as siblings of `By period`.

Remove the redundant `France / History` and duplicate `By period` concepts after
transferring their useful DC selectors. Keep general French historiography
directly beneath France. Route `DC21-DC29.3 Description and travel` through the
existing `Geography, Places & Travel / Travel / Europe / France` concept and move
its chronological travel children with it. LCC places this literature in the DC
history schedule, but the unified taxonomy treats its primary purpose as travel
and place discovery rather than as eras in French history.

## Country-aware History continuation

Before extending the regional History leaves again, reconstruct collapsed
national chronology from the structural LCC paths. Shared schedules remain at
their regional node, but national ranges are routed through the existing place:
for example, `Northern Europe / Denmark / By period`, `Central Europe / Austria
/ By period`, and `History of Balkan Peninsula / Bulgaria / By period`.

Remove redundant `place / History` wrappers throughout `History by Region`.
Transfer their selectors to the place itself, lift unique children, and merge
same-labelled children such as duplicate `By period` branches. This leaves no
visible concept labelled exactly `History` below `History by Region` and no
duplicate sibling labels.

Freeze the 1,074 LCC-backed History leaves present before this continuation.
Of these, 416 contribute visible detail. Add 1,235 contextual or immediate
semantic nodes, then retire 105 empty LCC-only leaves whose ranges were split
among their correct national contexts. The umbrella reconciliation merges a
further 99 redundant concepts. The net result is 1,031 additional History
concepts over the pre-continuation taxonomy.

Continue to exclude bibliographic/form divisions, A-Z filing structures,
locality authorities, named-person/ruler authority records, and `Description
and travel`. The latter belongs in the unified Travel hierarchy even when LCC
files it inside a national history schedule.

## Civilization and Historical Eras

Expose the semantic CB chronology beneath the existing `Civilization / By
period` branches. Reuse the existing History `Renaissance` concept and attach
its three civilization subperiods there. Preserve Western, Eastern, East–West,
Buddhist, and Confucian civilization subjects, and modernize `Developing
countries. Third world` as `Developing Regions & Postcolonial Societies` while
retaining the original caption as a source label.

Do not reproduce LCC's obsolete racial taxonomy as visible peoples or
civilizations. Route its historical captions—including Caucasian/Aryan,
Nordic, Alpine/Celtic, Mediterranean/Latin, Black, and Semitic—through the
single contextual concept `Race and Civilization in Historical Thought`.

Add the substantive general-European era detail in D: the War of the Austrian
Succession and Seven Years' War, nineteenth-century historiography and
political, social, military, and naval history, the Eastern Question and major
periods, interwar Fascism and the Rome–Berlin Axis, and postwar NATO, Soviet
bloc, and Warsaw Pact structure. Reuse Fascism as a shared concept. Exclude
bibliographic forms, filing records, named-person records, generic catch-alls,
and military unit and formation schedules.

## Exhaustive History continuation

After the staged one-level expansions, recursively traverse every represented
semantic record in the LCC C-F history schedules. Schedules whose title exists
only as an unnumbered structural caption are anchored explicitly to the
equivalent existing unified concept—for example Italy, Czechoslovakia, British
America, and the auxiliary-science branches. This avoids treating the presence
or absence of a numbered schedule-root record as a curation decision.

Collapse unnumbered and generic structural captions rather than displaying
empty navigation concepts. When a structural child cannot classify a book more
narrowly than its parent, retain its wording only as provenance. Reuse an
equivalent direct child before creating a contextual concept, and retain every
accepted detailed C-F range as an LCC selector.

Exhaustion applies to semantic subjects, periods, events, conflicts,
historiography, political and diplomatic history, social and intellectual
history, ethnography, antiquities, archaeology, epigraphy, chronology, and
numismatics. It does not import alphabetic filing records, bibliographic or
document forms, named people or authors, institutions and organizations,
locality/place authority trees, description-and-travel schedules, military
units or formations, or obsolete racial-classification descendants. The latter
remain covered only by `Race and Civilization in Historical Thought`.

The exhaustive manifest accounts for all 51,898 eligible structural records:
2,137 were already represented, 4,253 additional records are assigned through
3,997 new concepts, 99 direct-child reuses, and transparent structural
collapses, and every other record has a recorded exclusion reason. No eligible
record remains unresolved.

## Universal History localities

After semantic History exhaustion, retain named regions, provinces, states,
counties, districts, islands, natural regions, cities, and towns from every LCC
local-history schedule. Place them beneath the country or regional History node
that owns the schedule. Present filing structures as useful contextual groups:
`Local History & Places`, `Regions & Administrative Divisions`, and `Cities &
Towns`; collapse bare `A-Z` and `Other, A-Z` guides without losing their named
descendants.

Include the separate `F1-F975 United States local history` schedule, which is
not structurally beneath the ordinary E-class United States schedule. Anchor it
to the existing `United States / Regions of the United States` concept, reusing
existing regional children before adding states, counties, natural regions,
cities, and their semantic local-history detail. Do not create a parallel U.S.
geography umbrella.

Locality inclusion does not admit people, authors, institutions, societies,
military formations, bibliographic forms, description-and-travel records, or
unrelated `By region or country` subject-filing trees. Named places retain their
specific LCC cutters as selectors; locality containers are contextual, never
visible merely because their caption says `A-Z`.

The locality manifest considers 58,655 C-F and U.S. local-history records. Of
these, 6,719 were already represented and 13,869 additional records are
assigned through 12,994 new concepts, 44 direct-child reuses, 829 transparent
structural collapses, and two same-span collapses. Every remaining record has
an explicit out-of-scope exclusion reason.

## Unified-taxonomy reconciliation

Treat successive LCC locality captions that normalize to the same visible
group as one structural concept. Collapse nested `Cities & Towns` and regional
wrappers, alphabetic locality buckets such as `Saint C` and `Le A-Le Z`, and
generic `Local` captions while lifting their named descendants. Reuse the
existing Alaska and Utah concepts beneath `West` rather than creating parallel
state branches at the United States root. Merge equivalent Corfu/Kerkyra
records; reject the snapshot's unresolved Chester/Chichester collision because
both records claim the same `DA690.C5` selector and cannot be classified
reliably.

Across the complete taxonomy, concepts with the same normalized label and the
same external selector are one concept with multiple parent routes, not
parallel copies. After their parents merge, recursively merge identically
labelled siblings so jurisdiction-specific selectors and routes remain on a
single canonical concept. Never contract an edge when doing so would create a
cycle or leave duplicate normalized siblings.

The locality reconciliation merges 104 redundant concepts and removes two
ambiguous snapshot records. The global reconciliation then unifies 643
pre-existing parallel-import concepts across 662 duplicate source groups while
preserving 645 distinct routes. The resulting taxonomy has 49,247 concepts,
50,198 edges, 56,847 selectors, and 40,345 source labels, with every concept
reachable from one of the twelve top-level roots.

## Exhaustive Philosophy continuation

Recursively traverse the semantic B-BJ schedules beneath the existing unified
Philosophy, Logic, Metaphysics, Psychology, Parapsychology, Occult Sciences,
Ethics, and Etiquette concepts. Preserve philosophical traditions, regional
philosophies, periods, schools and movements, logical systems, metaphysical and
epistemological subjects, psychological fields and processes, ethical topics,
and the substantive occult and parapsychological subjects represented by LCC.

Treat BH Aesthetics as a shared concept beneath both Philosophy and `Arts /
General, Theory & Institutions`; its detailed aesthetic subjects therefore have
one canonical visible home with two useful routes. Reuse equivalent existing
direct children where possible and retain each accepted B-BJ span as an LCC
selector.

Do not expose the large philosopher, psychologist, author, named-work, or
special-person authority trees. Also exclude A-Z filing schedules,
bibliographic and publication forms, institutions and organizations, and empty
structural captions. Criticism and commentary remain eligible when they are
substantive philosophical subjects rather than apparatus beneath a named work.

The exhaustive manifest accounts for all 9,542 eligible B-BJ records. Of these,
419 were already represented and 782 additional records are assigned through
731 new concepts, six direct-child reuses, and 46 transparent structural
collapses. The remaining 8,341 records have explicit exclusions: 3,316 filing
records, 1,755 form records, and 3,270 individual-person or named-work records.
No eligible record remains unresolved.
