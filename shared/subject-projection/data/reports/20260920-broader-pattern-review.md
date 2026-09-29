# Broader taxonomy pattern review — report only

Reviewed the current local curated taxonomy after the music cleanup: 16,961
subjects, 23 roots, and 18,815 root-to-subject routes. No taxonomy, selector,
viewer, or usage data was changed during this review. Only this report and its
two supporting inventories were written.

## Main finding

The recurring problem is larger than excessive depth: source-system structures
are often retained as separate subject trees. LCC, BISAC, and sometimes DDC
headings represent overlapping subjects using different IDs and different sets
of children. Added navigation groupings then sit around these trees. Elsewhere,
shortened source captions have lost the context needed to describe their codes.

This produces five distinct problems that need different remedies:

| Pattern | Appropriate response |
|---|---|
| Equivalent subjects split by source system | One canonical concept with combined selectors and children |
| Overlapping broad wrappers with divided children | Review and reconcile the entire branch, not just its leaves |
| Useful subject repeated under different perspectives | Reuse one ID with meaningful multiple parents where scope agrees |
| Same label for different material, discipline, audience, or scope | Keep separate concepts and clarify labels |
| A label narrower or broader than its actual codes | Repair scope and selector ownership before considering a merge |

Code-free does not automatically mean useless. A geographical, chronological,
or audience grouping can communicate essential context. Conversely, having a
classification code does not justify keeping a redundant layer in navigation.

## 1. Highest priority: label and selector scope disagree

### Eating Disorders currently includes phobias

Eating Disorders (12668) owns `RC552.A–RC552.Z`. In the bundled detailed LCC
reference, that span is **Other neuroses, A–Z**, including Acrophobia (`RC552.A43`)
and Agoraphobia (`RC552.A44`), as well as Anorexia nervosa and Bulimia.

Read-only production-matcher probes confirmed that both `RC552.A43` and
`RC552.A44` currently resolve to Eating Disorders (12668). This is a concrete
classification error, not merely an awkward breadcrumb.

There is also a BISAC Eating Disorders subject (5443, `PSY011000`). Its matching
name makes this look like a straightforward merge candidate; merging without
repairing the LCC scope would carry the error into the unified subject.

**Recommendation:** review the Psychopathology branch's range boundaries first,
retain appropriate coverage for the broader span, and route eating-disorder
subdivisions specifically. Do not infer that every present disorder label
correctly describes its full LCC range.

### Modern Philosophy mixes general philosophy with aesthetics

Modern Philosophy (7622) combines DDC `190` with LCC `BH151`. The detailed local
LCC reference describes `BH151` as **Aesthetics > History > Modern**. A matcher
probe confirms that it currently resolves to the general Modern Philosophy
subject. Another Modern Philosophy (4510) contains the main chronological and
regional branch.

Likewise, `BH83`, whose local source caption is **Aesthetics > History > Origins**,
is attached to the broad History, Traditions & Regions subject (4507).

**Recommendation:** separate the aesthetics-specific coverage before deciding
which general modern-philosophy headings to merge. Restoring lost source context
is more important than matching the two visible labels.

## 2. Science: parallel trees for entire disciplines

This is the strongest remaining structural duplication cluster, despite none
of its routes exceeding six levels. A depth-only review would miss it.

| Subject | Separate IDs and placements | Evidence |
|---|---|---|
| Zoology | 4863 under Biology > Life Sciences; 5048 directly under Science & Nature | BISAC SCI070000/DDC versus LCC QL. Eight children versus twelve, with different specializations. |
| Geology | 4827 under Earth & Environmental Sciences > Earth sciences; 4904 directly under Science & Nature | BISAC SCI031000/DDC versus LCC QE. First has no children; second has nine. |
| Microbiology | 4857 under Biology > Life Sciences; 4975 directly under Science & Nature | BISAC SCI045000/DDC versus LCC QR. Each owns different children. |
| Ecology | 4790 under Biology; 4851 under Life Sciences; 5001 under Nature & Natural History > Ecosystems & Habitats | LCC/DDC, BISAC SCI020000, and BISAC NAT010000 respectively. Only 4790 has the 23 specialist children. |
| Molecular Biology | 4858 under Life Sciences > Molecular & Cellular Biology; 6239 under Biology > Life | BISAC SCI049000 versus LCC QH506. |
| Virology | 4862 under the BISAC-side Microbiology; 4980 under the LCC-side Microbiology | BISAC SCI099000 versus LCC QR355–QR502 and subranges. Only 4980 has twelve children. |

Other strong review candidates include Astrophysics (4782/4881), Electrochemistry
(4818/7828), and Relativity Physics (4888/7526).

**Recommendation:** reconcile Biology, Life Sciences, Life, Zoology, Microbiology,
and their children as a connected branch. Do not merge all those broad names
as synonyms: their scopes differ. Decide which are disciplines, which are
subdisciplines, and which are source-derived organizational headings; then
consolidate equivalent topics inside that structure.

The SCI/NAT distinction in Ecology is partly a publishing-category distinction.
Decide whether popular natural history versus academic science belongs in
subject identity or in another browsing facet. Avoid accidentally baking that
distinction into every scientific topic.

## 3. Construction and engineering: nearly synonymous siblings

Under Civil, Structural & Construction:

- Civil (230, BISAC TEC009020) has seven children, including Bridges, Dams &
  Reservoirs, and Highway & Traffic.
- Civil Engineering (5932, DDC 624) has six different children, including
  Structural engineering, Surveying, and Tunnels.
- Building construction (197, LCC TH/DDC 690) has fourteen children.
- Construction (238, BISAC TEC005000) has eight children.
- HVAC is consequently split between 205 (LCC/DDC, three children) and 243
  (BISAC TEC005050, no children).

Industrial Engineering is similarly divided between 344 (BISAC TEC009060) and
446 (LCC-backed, eleven children) under different engineering groupings.

**Recommendation:** first reconcile Civil/Civil Engineering and the two
construction branches. Then review the resulting HVAC, plumbing, contracting,
and building-specialty children together. Leaf-by-leaf merging would leave the
main source-system split intact.

## 4. Mental health: parallel disorder trees beneath overlapping wrappers

Two Psychopathology nodes each have thirteen children:

- 5434, BISAC PSY022000, beneath Psychiatry > Clinical Psychology > Mental Health.
- 2715, LCC RC512–RC569.5 and additional ranges, beneath Internal Medicine.

Repeated or near-equivalent children include Schizophrenia (5447/12661),
Personality Disorders (5445/12670), Bipolar Disorder, anxiety/phobias, depression,
PTSD, and addiction. The Eating Disorders scope error above shows why the entire
range map needs checking before consolidation.

Some routes also pass through Internal Medicine > Neuroscience & Neuropsychiatry
> Psychiatry > Clinical Psychology > Mental Health > Psychopathology before
reaching a disorder. Other routes reach the same subjects much sooner.

**Recommendation:** separate the medical/psychological perspectives from the
identity of a disorder. A shared disorder concept can have appropriate access
routes without needing two IDs. Review the semantic value of Mental Health and
Psychopathology as successive intermediate headings; avoid assuming that all
psychiatric conditions fit exclusively inside clinical psychology.

## 5. Literature: region and form are represented as duplicate subjects

French Literary Criticism has two IDs:

- 4332: Literary Criticism > Regions & Literary Traditions > European Literary
  Criticism > French Literary Criticism; BISAC LIT004150.
- 9551: Romance > France > French Literary Studies > French Literary Criticism;
  LCC PQ69–PQ96.

The pattern repeats for German Literary Criticism (4333/8065) and Italian
Literary Criticism (4334/8956), with BISAC and LCC coverage split between them.

**Recommendation:** one canonical criticism subject per scope, reachable from
both the criticism branch and the relevant literary tradition where useful.
These are useful alternative browsing perspectives, not necessarily reasons
to duplicate subject identity.

There are also language-versus-country ambiguities: Argentine Spanish (11949)
is under Romance > Spain > Regional Spanish, while Argentine (4157) is under
American & Caribbean > South American. Those scopes need not be identical:
Spanish-language Argentine literature and all Argentine fiction can differ.
The parent label Spain nevertheless suggests a country where a language or
literary-tradition label appears to be intended. Review naming and scope before
merging these nodes.

## 6. Philosophy: chronology and geography are mixed inside one route

A representative eight-level route is:

`Philosophy > History, Traditions & Regions > Historical Periods > Modern Philosophy > Regional Modern Philosophy > Latin American Modern Philosophy > South American Modern Philosophy > Venezuelan Modern Philosophy`.

Historical Periods (5535) is code-free. Regional Modern Philosophy (9234) is
coded, so shortening this requires more than deleting empty nodes.

Regional Modern Philosophy also contains direct children named 21st century
(11102) and 19th & 20th centuries (11111). The latter owns B4051–B4095.2 and
B4157–B4175.2: source records associate those ranges with Netherlands and Belgium,
respectively. A generic century node beneath a regional grouping obscures both
the geography and the limits of its coverage.

Sikhism (11093) also sits under Ancient & Classical Philosophy > Orient > Ancient
Eastern Religious Thought. The bundled detailed LCC source itself places its
B162.65 record beneath an ancient-period heading, demonstrating that importing
source ancestry unchanged is not sufficient to produce a sound reader-facing
chronology. The Religion-side Sikhism subject is a different ID, 4623.

**Recommendation:** choose a consistent primary organization, with explicit
regional/period scope and appropriate secondary routes. Review geographically
scoped period ranges before consolidating their labels. Do not simply merge
everything named Modern Philosophy or a religion across these paths.

## 7. Religion: source wrappers repeat narrower subjects

Missions appears directly under Christian Ministry (4575, BISAC REL045000) and
under Christian Ministry > Practical Theology (11585, LCC BV2000–BV3705/DDC 266).
Christian Science appears directly under Christian Denominations (4585, BISAC
REL083000) and under Protestantism (13792, LCC BX6901–BX6997).

**Recommendation:** confirm equivalent scopes and use shared concepts. Review
the broader Christian Ministry/Practical Theology organization separately;
those are overlapping headings, but not automatically exact synonyms.

## 8. Code-free wrappers: useful candidates, not an automatic deletion list

Examples warranting examination:

- Foundations & Computer Science (5571) contains Computer Science (62) plus
  sibling computing foundations. Its name overlaps its child, but deleting it
  promotes eleven subjects, so assess the resulting Computing & IT branch.
- Science > History, Philosophy & Society (5725) contains Science, Philosophy
  & Society (13323) and Philosophy of Science Topics (13324). Reconcile their
  actual BISAC/LCC scopes before deciding whether to flatten or consolidate.
- Cards & Invitations (667) has one child, General & Technique (6464), which
  holds NC1860. Moving the code to the meaningful parent looks more useful
  than deleting the parent and exposing General & Technique.

There are 656 code-free internal subjects and 150 code-free, single-child,
non-root subjects. Many are country, period, or literature groupings. These
counts describe review opportunities, not 656 or 150 defects.

History still accounts for 635 of the 742 routes with eight or more levels.
Some paths combine region, subregion, empire, country, period, and event. The
previously reported Guyanese/Surinamese colonial routes remain examples where
the problem is inappropriate inheritance of independent-period history, not
just an extra click. Preserve useful geographic and chronological distinctions
when shortening these paths.

Two childless roots—Philosophy, Psychology & Religion (53067, `(B)`) and Geography,
Anthropology & Recreation (53068, `(G)`)—also expose broad classification fallback
concepts alongside topical roots. Decide how fallback assignments should appear
in navigation; their codes should not be discarded as if the nodes were empty.

## Scale and limitations

The current taxonomy has 1,231 case-insensitive repeated-label groups involving
4,580 subjects. Most cannot be treated as confirmed duplicates. A narrower
mechanical screen found 218 groups containing both a BISAC-only and an LCC-only
subject among non-audience/non-bibliography/non-biography routes. It considers
groups of two to six IDs; the selector distinction is not proof of equivalence.

| Area | Subjects reachable from root | Screened groups touching area | Maximum levels |
|---|---:|---:|---:|
| Science & Nature | 1,105 | 52 | 6 |
| Applied Sciences & Technology | 1,627 | 29 | 7 |
| Health & Medicine | 1,129 | 33 | 8 |
| Literature | 1,491 | 47 | 7 |
| Religion | 455 | 12 | 7 |
| Philosophy | 491 | 10 | 8 |
| Arts & Media | 1,217 | 28 | 8 |
| Social Sciences | 2,459 | 52 | 8 |
| History | 3,497 | 42 | 10 |

Counts overlap across roots because multiple parents are supported. The screen
misses differently named equivalents such as Civil/Civil Engineering and cannot
detect scope errors on its own. Detailed semantic review was targeted at the
examples above, not every candidate in the inventory. Evidence comes from the
current curated SQLite database, the bundled detailed LCC reference and outline,
and read-only probes using the existing optimized release matcher. No claim is
made about a newly downloaded classification edition or deployed service state.

Prioritize scope errors first, then Science, Construction, Psychopathology, and
Literary Criticism as whole-branch reviews. Assess Philosophy and Religion with
particular care around historical, geographic, and disciplinary context. Treat
code-free wrapper removal as a later navigation decision. Parent relationships
also affect the matcher's incomplete-code fallbacks, so structural changes need
classification comparisons even when every selector string is preserved.

- [Screened candidate inventory](20260920-broader-pattern-candidates.csv)
- [Counts and reviewed database fingerprint](20260920-broader-pattern-summary.json)
