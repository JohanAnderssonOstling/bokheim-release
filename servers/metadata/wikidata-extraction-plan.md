# Wikidata book and author extraction

Status: streaming projection extractor implemented and tested; full extraction
activation is tracked by its output manifest and progress file. Lookup-index
construction and metadata-service integration remain separate steps.

Use one scan of the existing Wikidata JSON dump to retain author metadata and
bibliographic records. The resulting data supplements the existing title/author
to ISBN and ISBN to LCC lookups.

## Records and fields

Retain ISBN-bearing records regardless of their declared instance class. Retain
book works, editions, their linked parent works, and the agents credited as
authors. A work may have no ISBN and still supply authors and classifications to
its editions. Do not restrict authors to entities with a writer occupation.

| Field | Source | Use |
| --- | --- | --- |
| Entity identity | QID | Distinct source identity; never manufacture an OL ID |
| Title and subtitle | P1476, P1680 | Language-preserving bibliographic title search |
| Labels and aliases | labels, aliases | Additional search evidence and author display names |
| Short book/edition and author descriptions | descriptions | Preserve each available language; separate from summaries and biographies |
| ISBN-13 and ISBN-10 | P212, P957 | Validate checksums and normalize to canonical ISBN-13 |
| Authors | P50 | Agent identities; retain P1545 ordering when present |
| Author name strings | P2093 | Name evidence without inventing an author entity |
| Edition/version/translation parent | P629 | Resolve edition-to-work relationships after scanning |
| Book dates | P577 | Edition discrimination, preserving date precision |
| Book language | P407 | Distinguish language editions |
| Publisher | P123 | Supporting edition evidence |
| Genre and main subjects | P136, P921 | Typed genre/topic links, separate from LCC |
| Series and sequence | P179, P155, P156 | Series membership and predecessor/successor links |
| Volume, issue, edition and ordering | P478, P433, P393, P1545 | Preserve values and qualifiers on their original record |
| Translators, editors and illustrators | P655, P98, P110 | Contributor identities with distinct roles |
| Page count | P1104 | Edition-specific quantity, preserving bounds and units |
| Book place and format | P291, P437 | Edition-specific book context |
| Available text and scans | P953, P996, P724 | Links/references only; no content download in the scan |
| LCC | P8360 | Explicit book classification evidence |
| Open Library IDs | P648 | Crosswalk works, editions, and authors by ID type |
| Author dates | P569, P570 | Birth/death dates with precision |
| Occupations | P106 | Author roles and profile browsing |
| Writing languages | P6886 | Languages used in the author's writing |
| Awards received | P166 | Awards and recognitions with dates and qualifiers |
| Education | P69 | Educational institutions attended |
| Employers | P108 | Institutional affiliations with applicable dates |
| Influenced by | P737 | Directed links to people, ideas, or other influences |
| Movements | P135 | Literary, philosophical, artistic, or scientific movements |
| Birthplace | P19 | Linked birth location |
| Citizenship | P27 | Countries of citizenship with applicable dates |
| Official websites | P856 | Author website URLs, including temporal and language qualifiers |
| Field of work | P101 | Author specialization; linked discipline QIDs and localized labels |
| Notable works | P800 | Links to significant works; not a complete bibliography |
| Doctoral advisors | P184 | Student-to-advisor links |
| Doctoral students | P185 | Advisor-to-student links |
| Portrait reference | P18 | Commons filename; download and cache separately |
| Authority crosswalks | P214, P213, P244 | VIAF, ISNI, and LC authority identities |
| Goodreads author identity | P2963 | Author crosswalk and profile link |
| Goodreads edition and work identities | P2969, P8383 | Separate edition/work crosswalks |
| LibraryThing author identity | P7400 | Author crosswalk and profile link |
| StoryGraph author identity | P12430 | Author crosswalk and profile link |
| ORCID | P496 | Researcher/author crosswalk |
| Libris identity | P5587 | Preserve record identity and author/book scope |
| Additional external identifiers | external-id claims | Retain extensibly by property ID on selected entities |
| Wikipedia links | sitelinks | Book and author reference links; potential later article-extract lookup |

Preserve original statement values, rank, and source QID alongside normalized
lookup values. Use deprecated statements as lower-priority fallbacks under the
policy below. Preserve
multiple authors, ISBNs, and work parents as distinct evidence. Do not silently
select the first conflicting claim.

### Deprecated-claim fallback policy

Try usable preferred and normal claims before deprecated claims. For the
particular entity, property, and lookup context, consider deprecated claims only
when current evidence provides no usable result. They must not override usable
current evidence or resolve a conflict between current candidates by themselves.
Preserve the original rank and any deprecation reason, and mark derived results
that relied on fallback evidence.

Fallbacks still pass the same ISBN checksum, identifier format, bibliographic
identity, author compatibility, and work/edition scope checks. A deprecated ISBN
on a city record must not turn that city into a book candidate. Preserve multiple
fallback candidates and their ambiguity instead of taking the first value.
Carry fallback provenance through work/edition and author crosswalks so derived
results cannot silently become primary evidence.

Apply the same tiering to external IDs, classifications, profile fields,
portraits, and linked relationships. Missing usable values may be filled by
validated deprecated evidence. Historical or deprecated websites remain marked
as fallback/historical links rather than being presented as verified current
websites. This policy does not restore Dewey extraction or classification.

### External identifier storage

Retain main-value claims whose snak datatype is `external-id` on retained book,
edition, and author records, rather than restricting extraction to today's
named providers. Preserve QID, property ID, original value, statement ID, rank,
qualifiers, and references. Keep the explicit exclusion of Dewey properties
(including P1036 and P8359); generic identifier extraction must not restore
Dewey handling. Missing-value and unknown-value snaks are not usable IDs.

Normalize and index supported authorities separately, retaining unknown
authorities as raw evidence so a later integration need not rescan the dump.
External IDs are strings: preserve significant leading zeros, punctuation, and
case unless the particular authority defines canonicalization. Do not mix
Goodreads author, edition, and work identifiers or promote an edition ID to a
work ID. P648 and Libris IDs likewise retain the scope of the source record.

Build reverse authority/property-and-value to QID mappings. Multiple QIDs for
the same external ID are conflicting evidence, not permission to merge them
automatically. Index deprecated evidence in the fallback tier, keeping its rank
and provenance through crosswalk resolution. Compare Wikidata crosswalks with identifiers already supplied
by Open Library and preserve disagreements for review.

These identifiers enable direct profile links and later provider integrations.
They do not include those providers' reviews, ratings, biographies, or images;
fetching such content is separate work.

### Notable works and academic relationships

Retain P106, P6886, P166, P69, P108, P737, P135, P19, and P27 as
QID-linked author profile statements. Preserve multiple values, references,
statement identity, ranks, and all qualifiers, especially dates for awards,
employment, education, and citizenship. Resolve target labels through the
compact entity directory and retain unresolved links. Preserve date precision
and distinguish former affiliations from current ones when the evidence permits.
Do not assume an undated statement describes the present.

Keep P6886 writing language distinct from P1412 languages spoken, written, or
signed; do not substitute general language knowledge for writing-language
evidence. Keep citizenship distinct from birthplace. P737 influence targets may
be ideas or other entities, not just people, and its direction must be preserved.
Support reverse browsing without converting derived relationships into asserted
source statements. These profile fields do not automatically classify books.

Retain P856 website values with provenance, rank, and temporal/language
qualifiers. Allow historical or deprecated values as clearly identified fallback
links under the policy above. Validate HTTP(S) URLs for display and do
not crawl sites as part of extraction.

Retain P101 field-of-work statements as author-to-discipline QID links with
rank, qualifiers (including dates where provided), references, and statement
identity. Preserve multiple fields and resolve localized discipline labels
through the compact entity directory. Index both author-to-fields and
field-to-authors for profile display and browsing. Keep unresolved target QIDs.
Field of work is author-level evidence: it must not automatically assign LCC
codes or subjects to every book by that author. Keep specialization distinct
from occupation and from explicit book classifications.

Retain P800, P184, and P185 as directed QID-to-QID statements with rank,
qualifiers, references, and statement identity. Preserve multiple values. Build
forward and reverse lookup indexes for navigation, retaining the original
assertion separately from any derived reverse relationship. A statement on
either endpoint is enough to display the relationship; do not require both
endpoints to repeat it. Deduplicate reciprocal assertions for display without
discarding their provenance. Allow deprecated relationships as identified
fallbacks under the policy above.

Keep target QIDs even when their records are outside the bibliographic subset.
Resolve available labels, descriptions, and entity types from a compact entity
directory produced during the same scan; do not recursively import every
connected entity's full graph. A missing target record remains an unresolved
link, never a name-based guess or grounds for dropping the original statement.

P800 can point to scientific or artistic works as well as books. Display it as
notable works, not as a complete bibliography, and only attach ISBNs through
verified bibliographic relationships. An academic advisor or student need not
be an author in the user's library. Keep doctoral supervision distinct from
general education, influence, and non-doctoral teacher/student relationships.

## Derived lookups

1. Title plus author to candidate edition/work QIDs and their ISBNs. Reuse the
   current title and author normalization and ambiguity checks. A work-level
   match may identify several editions; do not claim an exact edition merely
   because their subjects agree.
2. ISBN to explicit LCC from the edition, or from an unambiguous linked work.
   Retain whether the code came from the edition or the work. Conflicting parent
   links must not result in an unconditional union of classifications.
3. ISBN to author QIDs, including work-derived author credits when edition
   credits are absent and the parent evidence is unambiguous.
4. Open Library author ID to Wikidata QID and portrait metadata. Combine this
   crosswalk with the existing OL `remote_ids.wikidata` evidence; surface
   conflicts rather than overwriting them.

Resolve cross-record links after the scan because records are not topologically
ordered. Stage enough compact bibliographic evidence to resolve referenced
works even when they occur before their editions. Guard recursive work traversal
against cycles and excessive depth.

The existing ISBN classification lookup remains useful when Wikidata has an
ISBN but no LCC: Open Library and LC dump data may already classify that ISBN.
Do not infer LCC from titles or topic labels, and do not extract Dewey codes.

## Book summaries and author biographies

Extract the language-keyed `descriptions` map for retained books, editions, and
authors. Wikidata descriptions are short identifying phrases, not full book
summaries or author biographies. Keep them in a separate short-description field
with language, source QID, and dump provenance. Preserve edition and work scope.
They must not overwrite richer descriptions from another source.

Reuse the existing Open Library work-description and author-biography sidecars
for longer text, joining only through verified identities. Prefer existing
user-supplied text; expose the short Wikidata text as a fallback when longer text
is unavailable. Apply the UI's language preference when selecting text for
display rather than discarding other languages during extraction.

Preserve book and author Wikipedia sitelinks. Article introductions could be
fetched and cached later through those exact links, with source, language,
revision, and required attribution retained. Wikipedia article bodies are not
contained in this Wikidata dump; this is a separate optional enrichment step,
not part of the dump scan.

## Streaming extraction implementation

`extract-wikidata.py` reads the existing bzip2 dump through its validated seek
index and writes projected JSONL in independent Zstandard-compressed chunks.
It never materializes the full raw JSON dump. See
`wikidata-streaming-extraction.md` for execution and recovery instructions.

The projection preserves the agreed properties, complete statements and all
external-ID properties (excluding Dewey) across entities. It also retains labels,
aliases, descriptions and Wikipedia sitelinks. This deliberately includes context
entities so an author, publisher, discipline or parent work encountered before
its references can be resolved without another raw-dump scan. The later index
builder selects bibliographic records and authors from this projected evidence;
projection alone is not a ready-to-query book/author database.

Genre and main-subject QIDs remain distinct from explicit LCC. Page counts,
formats, book places and contributor roles retain edition scope. Preserve
series order qualifiers and directed predecessor/successor relationships rather
than assuming the order of statements is the order of books. Digital-text and
scan references are metadata only.

## Integration and validation

The existing edition identity response has a required
`open_library_edition_id`. Add explicit provider-neutral identity support before
returning Wikidata-only matches through that API. Preserve compatibility with
existing Open Library clients and stored review candidates.

Build an independent, resumable output using the dump's existing seek index.
Validate the output before activating it. Record dump identity, schema version,
record checkpoints, and counts of retained and rejected evidence. A sampled or
interrupted build must not be published as a complete snapshot.

Test edition-before-work and work-before-edition order, inherited authors and
LCC, multiple authors, invalid ISBNs, deprecated claims, conflicting parents,
cycles, same-title unrelated authors, multiple editions, missing LCC, and exact
ISBN classification through existing OL/LC data. Measure net new ISBNs, LCC
assignments, author links, and disagreements before changing source preference.
Also test short-description language selection, work versus edition scope,
missing long text, and preservation of existing summaries and biographies.
Test external-ID round trips, unsupported authorities, significant leading
zeros, deprecated claims, conflicting reverse mappings, author/work/edition
scope separation, and exclusion of Dewey properties.
Test academic relationships recorded on either endpoint, reciprocal duplicate
assertions, multiple advisors, deprecated edges, unresolved targets, and notable
works that are not books.
Test multiple fields of work, localized and missing discipline labels,
deprecated statements, and that author fields do not become book subjects.
Test usable current claims winning over deprecated claims, fallback when all
current claims are unusable, deprecated-only ambiguity, invalid deprecated
ISBNs, non-book entities with ISBNs, and fallback provenance through linked
records.
Test temporal author affiliations and awards, multiple citizenships, writing
language versus general language knowledge, non-person influence targets,
influence direction, and historical or invalid website URLs.

Property documentation:

- https://www.wikidata.org/wiki/Wikidata:WikiProject_Books
- https://www.wikidata.org/wiki/Property:P629
- https://www.wikidata.org/wiki/Property:P8360
- https://www.wikidata.org/wiki/Help:Description
- https://www.wikidata.org/wiki/Property:P2963
- https://www.wikidata.org/wiki/Property:P2969
- https://www.wikidata.org/wiki/Property:P8383
- https://www.wikidata.org/wiki/Property:P7400
- https://www.wikidata.org/wiki/Property:P12430
- https://www.wikidata.org/wiki/Property:P496
- https://www.wikidata.org/wiki/Property:P5587
- https://www.wikidata.org/wiki/Property:P800
- https://www.wikidata.org/wiki/Property:P184
- https://www.wikidata.org/wiki/Property:P185
- https://www.wikidata.org/wiki/Property:P101
- https://www.wikidata.org/wiki/Property:P106
- https://www.wikidata.org/wiki/Property:P6886
- https://www.wikidata.org/wiki/Property:P166
- https://www.wikidata.org/wiki/Property:P69
- https://www.wikidata.org/wiki/Property:P108
- https://www.wikidata.org/wiki/Property:P737
- https://www.wikidata.org/wiki/Property:P135
- https://www.wikidata.org/wiki/Property:P19
- https://www.wikidata.org/wiki/Property:P27
- https://www.wikidata.org/wiki/Property:P856
