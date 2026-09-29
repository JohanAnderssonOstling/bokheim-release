# Music and parallel subject cleanup — 2026-09-20

Applied the reviewed cleanup to the local curated taxonomy and regenerated its
usage export and standalone viewer. The original master/reference databases are
unchanged. This is a local data change, not a server deployment.

## Result

- 38 merges: 14 in music and 24 elsewhere. Equivalent LCC/BISAC subjects now
  share IDs, selectors, and children. General-work leaves and overlapping
  wrappers were also consolidated where their broader surviving subject fits.
- Three code-free music wrappers removed: Music Manuscripts, Music Institutions
  & Careers, and Theory & Education. Their children remain reachable.
- Music shrank from 276 to 260 distinct subjects. Maximum route depth, counting
  Arts & Media as level one, fell from eight to six. Case-insensitive repeated
  labels within music fell from 16 groups to zero. This is not a claim that
  every remaining semantic overlap throughout the entire taxonomy is resolved.
- The complete curated taxonomy now contains 16,961 subjects.

Music merges include Ethnomusicology, Composition, Guitar, Conducting, Voice /
Singing, piano and keyboard, strings, brass, woodwinds, percussion, essays, and
the parallel philosophy/social-aspects heading. Instrumental techniques was
combined with Musical Instruments, preserving its broad MT coverage and its
more specific children. Scores, performance instruction, music analysis, and
historical criticism retain separate concepts with clearer labels.

Outside music, the reviewed merges cover Electronics and its Microelectronics,
Optoelectronics, and Electronic Circuits branches; Beekeeping; Educational
Psychology; Explosives & Pyrotechnics; Surveying; Tribology; Illustration;
Watercolor Painting; Adult & Continuing Education; Inclusive Education;
Special Education; Classroom Management; Home Schooling; Cybernetics;
Information Theory; Optical Data Processing; Street Photography;
Cinematography & Videography; Evidence-Based Medicine; Endodontics; and the
Architectural History wrapper/overview pair.

Galician Literature now belongs under Literature > Romance. Role Playing &
Fantasy and Travel Games now belong under the appropriate Recreation branches.
Psychiatric Chemotherapy was renamed Psychiatric Drug Therapy and placed under
Psychiatry; it was not merged with general Chemotherapy.

## Source-supported music corrections

The fuller local `lcc-mdsconnect-2016.sqlite3` reference establishes:

- `MT2.5`: Music study abroad, under Instruction and study. The reduced CSV
  incorrectly records `M2.5`. Removed that erroneous selector and moved the
  existing `MT2.5` selector from general Instruction & Study to Music Study Abroad.
- `M1490`: Music printed or copied in manuscript before 1700. Moved this from
  Music study abroad to the new Music Scores Before 1700 subject (54943).
- The existing `M4–M1480` instrumental span moved from Music study abroad to
  Instrumental Scores. Its boundaries were preserved.
- Vocal music is no longer a child of Music study abroad. American Music Before
  1860 is now American Scores Before 1860 under Printed Music.
- `MT3–MT5` belongs to music-instruction history. The former Music History node
  is now Music Education History under Instruction & Study, distinct from
  Music History & Criticism (`ML159–ML3785`).
- Children's instrumental music (`M1375–M1420`) and children's instrumental
  techniques (`MT740–MT810`) retain different subjects and explicit labels.

## Validation

The migration was prepared and replayed against copies before application. All
tables in the replay matched the preview exactly. Before applying, the live
taxonomy was checked against the frozen baseline to avoid overwriting concurrent
data changes. SQLite integrity, foreign keys, cycle detection, and duplicate
sibling-label checks passed. Every classification selector and range was
compared, permitting the recorded ID merges and the four explicit music repairs
above; no other classification coverage was removed.

- 41,087 selectors and range endpoints compared against the final taxonomy:
  66 changed destinations, zero losses. Range-selector strings are included as
  probes but are not all valid individual call numbers.
- All 11,977,153 observed DDC/LCC strings compared for the initial cleanup.
- Final music refinements checked against all observed DDC and all LCC strings
  containing `M`: 2,324,614 probes. All differences were the recorded merges.
- Final education/watercolor parent refinements checked against 249,848 observed
  LCC strings containing `LB` or `ND` after case, whitespace, and Unicode
  normalization. These restored useful original incomplete-code fallbacks.
- Combined final result: 37,385 observed strings changed destinations, zero
  coverage losses. All but 16 are explained by ID merges. The remaining changes
  are the corrected study-abroad/early-score classifications, two fallbacks
  from deleted music wrappers to surviving ancestors, and three composed codes
  whose redundant Computing assignment is suppressed after the Cybernetics merge.
- Release library tests: 245 passed and four failed both before and after.
  The unchanged failures concern damaged `QA76.` notation, a root Humanities
  fixture, an old Russian geography path, and an old decorative-arts path.

The LCC usage export was regenerated from full observed counts plus weighted
refinement deltas, rather than merely summing historical per-subject totals.
Every nonzero count references a surviving concept; viewer breadcrumbs reflect
the final graph.

## Artifacts

- Migration
- [Exact merge, label, parent, and selector changes](20260920-music-and-parallel-cleanup.json)
- [Validation summary and remaining non-merge differences](20260920-music-and-parallel-validation.json)
- [Every changed observed classification](20260920-music-and-parallel-classification-changes.tsv)

Existing reports dated earlier in this session describe their own frozen
snapshots and have intentionally not been rewritten as if they described this
new state. The on-disk release metadata is unchanged; deployment allocates the
next published release identifier.
