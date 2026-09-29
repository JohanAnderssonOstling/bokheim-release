# Safe LCC repairs — verification in progress

The goal is to repair clear supported formats while leaving catalog markers, truncated/ambiguous classifications, other classification systems, unknown text/prefixes, and ambiguous typos unrepaired. Original notation remains available as evidence. This goal is not complete.

## Current changes

- Complete joined numeric/Cutter calls and 304 reference-verified intervals omitted from the condensed outline.
- Physical `(set)` qualifiers and apostrophe-separated calls with recognized volume metadata.
- Semicolon publication-year separators, legacy `n.s.` captions, numbered volume continuations (`etc.`), commas before enumeration, and the documented `Index` designation.
- Complete single-letter decimal second classes such as `RC466.8R726.7`; existing period-separated publication-year interpretation takes precedence.
- Eleven exact joined strings excluded from splitting because original editions retain separate classifications showing that the joined field stops inside its final component.

## Source confirmation and limits

All 4,692 changed 20-character strings without spaces in the fourth intermediate comparison were checked against the original metadata database, read-only. There are 113 edition records proving an interior cut for 11 distinct strings. Their complete components are stored in `20260907-confirmed-clipped-joined-components.json`. The verification script is `/tmp/lcc-safe-repairs/verify_joined_clipping_remote.py`.

Fields ending at a complete component boundary were not counted as interior cuts. Neither matching string length alone nor absence of separate components proves that a classification is damaged or correct. Other ambiguous strings remain a review concern.

A broad 20-character guard was rejected: it removed 2,001 existing matches (4,146 uses) without individual evidence. That guard is no longer in the current source. The parser now excludes only the eleven source-confirmed strings. Intermediate fifth-comparison losses describe the abandoned broad guard and must not be reported as current results.

## Verification

The last completed suite passed 175 release tests (`tests-confirmed-clipping.log`); the suite including all eleven exclusions is `tests-confirmed-clipping-all.log`.

The fourth full intermediate comparison examined all 11,316,630 cache strings and recovered 11,311 strings / 25,226 uses, changed 1,361 assignments / 4,153 uses, and lost no matches. Those counts predate the final single-letter decimal, Index, and exact clipping-exclusion changes. They are not final claims.

The latest full candidate build is `build-after10.log`, with source hashes in `candidate-source-manifest.json`. Run its optimized `resolve_all` binary against the same frozen taxonomy into `after10.tsv`, then run `compare.py after10.tsv tenth` after completion. Audits include concurrent documented-date classification work from the shared source; aggregate gains are not attributable solely to this goal.

Outstanding: final full comparison and assignment-change review, remaining supported-format review, complete residual/exclusion evidence, final matcher-version invalidation and release verification. No live taxonomy rows are edited by these repairs.

## References

- [Harvard item-description guidance](https://harvardwiki.atlassian.net/wiki/spaces/LibraryBestPractice/pages/58595829/Best%2BPractices%2Bfor%2BItem%2BRecord%2BDescription%2BField) and [OCLC caption abbreviations](https://help.oclc.org/Library_Management/WorldShare_Acquisitions/Release_notes_and_known_issues/2023_release_notes/070WorldShare_Acquisitions_Release_Notes_September_2023) document `n.s.`.
- [LC G140](https://www.loc.gov/catdir/cpso/G140.pdf) and [LC G230](https://www.loc.gov/catdir/cpso/G230.pdf) document Index as a suffix to the original call number.

Audit workspace: `/tmp/lcc-safe-repairs`. Cache: `/home/johan/.cache/bokheim/openlibrary-code-usage.sqlite3`. Original metadata: `192.168.1.68:/home/johan/data/openlibrary/current.sqlite` (read-only). Frozen audit taxonomy: `/tmp/lcc-safe-repairs/isolated/shared/subject-projection/data/unified-taxonomy-v2.sqlite3`.

The all-eleven-exclusion release suite passed: 175 tests, zero failures. Full `after10.tsv` audit is running in tool session 85003. A release `annotate_unmatched` example is ready to annotate its unmatched rows after completion, distinguishing syntax rejection from taxonomy gaps. Reproducible scripts and completed small evidence artifacts are also saved under `/home/johan/.cache/bokheim/safe-lcc-repairs-20260907`.

Latest full comparison complete (`tenth-summary.json`): 11,955 recovered strings / 25,499 uses; zero lost matches; 1,356 changed assignments / 3,981 uses. Current residuals are annotated in `remaining10.csv`; 212 have both canonical syntax and a prefix present in the outline (`known-prefix-unmatched10.json`). Many are malformed/reversed ranges, ambiguous joined fields, and relative Cutter/table notations, so canonical syntax alone does not prove validity. Review these before declaring taxonomy coverage complete. Potentially interpretable relative ranges include `PK2659.P348Z5-Z999` and `PK2659.U32Z5-Z999`; they require reference/table evidence before changing interpretation.

Completed full outputs and evidence are persisted under `/home/johan/.cache/bokheim/safe-lcc-repairs-20260907`. Remaining work includes the 212-code review, the broader rejected-format review and changed-assignment audit, then final version invalidation and verification. No completion claim is made.

## Additional verified table and item formats

The numeric upper bound in preliminary `A61-Z458S (P-PZ40)` forms is now retained as Z458, using the exact `.XA61` to `.XZ458` interval in the existing local relative-table registry. Unknown numbered bounds, missing table references and unknown table names are rejected by this new interpretation.

Recognized item enumeration now supports a single letter or letter-plus-digits, such as `vol. B` and `no. M5`, and full English captions (`volume`, `part`, `number`, `issue`) plus unpunctuated `vol`/`no`. The complete preceding call still validates. Source: [LC series-numbering guidance](https://loc.gov/aba/rda/mgd/seriesSubseries/mg-m-numberingWithinSequence-Series-01.pdf).

Verified vernacular captions now also include `n.F.` / `neue Folge`, `deel`, `sz.`, and the observed `Heft.` spelling. Sources: [PCC caption list](https://www.loc.gov/aba/pcc/conser/conserhold/Captabbr.html) and [Syracuse Libraries abbreviation guidance](https://su-jsm.atlassian.net/wiki/spaces/library/pages/159811049/Abbreviations).

Latest release suite: 178 passed, zero failures (`tests-vernacular.log`). `after12.tsv` is complete and comparison `twelfth` is running (consult current tool handle); it predates the vernacular additions. Latest candidate build is `build-after13.log`; use it for the next full comparison. Other observed metadata still needing evidence/review includes Finnish `nide` and `osa`, guides/manuals, and more complex serial enumeration. These are not declared fixed or excluded merely because the parser currently rejects them.

The twelfth full comparison recovered 13,016 strings / 26,753 uses, with zero lost matches and 1,356 changed assignments / 3,981 uses. It includes the numbered-table and alphanumeric/English-caption additions, but predates the later vernacular and accompanying-material changes.

Finnish `nide` and `osa` are now supported as numbered item captions. Evidence: [Finnish National Library series-volume description](https://www.kansalliskirjasto.fi/fi/uutiset/sibeliuksen-teokset-viululle-ja-pianolle-julkaistu-kriittisena-editiona), [Fennica cataloging guidance](https://www.kiwi.fi/spaces/Kansallisbibliografiapalvelut/pages/141787366/Fennican%2Bkuvailuohje), and [NYPL catalog series enumeration](https://web.nypl.org/research/research-catalog/bib/cb844898).

Accompanying `Guide`, `User Guide`, `Teacher's Guide` and `Manual` descriptors now retain a complete dated call with a known LCC prefix. Undated/unknown free text and other classification prefixes are not accepted by that branch. Sources: [Yale accompanying-material guidance](https://web.library.yale.edu/cataloging/cms/special-processing/accompanying-material) and [LC MARC accompanying-material description](https://www.loc.gov/marc/umb/um07to10).

Latest release tests: 179 passed, zero failures (`tests-accompanying-material2.log`). `after15.tsv` is the new full comparison output using `build-after15.log`. Run its comparison and annotation after completion. Remaining metadata inventory is in `residual-metadata-review10.json`; it predates these repairs and requires refreshing. Known remaining candidates include additional documented caption spellings and compound serial enumeration; arbitrary markers/provenance remain excluded. Final matcher version invalidation, full residual review, and assignment review are still outstanding.

## Compound captions, MARC delimiters and assignment review

Further repairs include German Abh./Beiheft/Folge captions, spacing/punctuation variants of new-series captions, and complete chronology between enumeration levels. Sources: [ULB Münster abbreviations](https://www.ulb.uni-muenster.de/hans/abkuerzungen.html), [MARC holdings captions](https://www.loc.gov/marc/holdings/hd853855.html), and the previously linked PCC/Syracuse caption guidance.

Common accompanying-material compounds (Study Guide, Solutions Manual, Instructor's Manual, etc.) use the same validated dated-call rule as Guide and Manual.

MARC item fields now recognize `$`, `ǂ` and `‡` equivalently. Complete law calls are also allowed in the single-class `$a`/`$b` decoder, as MARC 050 structure applies to them too. Unknown/repeated subfields and class ranges in this decoder are still rejected. [OCLC's introduction](https://www.oclc.org/bibformats/en/about/introduction.html) documents the three display delimiters and implicit first `$a`.

All 1,356 changed assignments in candidate 15 passed a complete component audit (`changed15-review.csv`): concatenated parsed components reproduce the original string, each component resolves, and their union equals the assigned subject set. This proves literal component preservation and routing, not unrecorded source intent. Source-confirmed clipping exclusions remain in force. The audit-only wrapper was appended to the isolated crate, never the live source, and is overwritten by the next production-source snapshot.

Candidate 16 full comparison: 16,286 recovered strings / 30,622 uses; no lost matches; 1,356 changed assignments / 3,981 uses. It predates the final ordinal/plural captions. The latest live release suite passed 184 tests (`tests-ordinal-captions.log`), including `3rd ser.`, historical `2d ser.`, `38.Jahrg.`, `15th ed.`, plural vols./nos./pts., and observed PCC ar./zv. captions.

Candidate 17 snapshots both Rust source and text reference inputs (`candidate17-manifest.json`), preserving the frozen SQLite taxonomy. Its build is `build-after17.log`; run the full comparison and annotation on this candidate next. Current live matcher versions are LCC 53 / Unified 100 due to concurrent related work; final invalidation still needs a fresh audit once repairs are finished. No completion claim is made.

Candidate 17 full resolver is running in tool session 80565, writing `after17.tsv`. After it completes, compare against the original baseline with tag `seventeenth` and annotate remaining rows. Candidate 16 still leaves 178,092 strings / 316,083 uses unmatched; 147,050 of those uses are explicit excluded markers and 3,591 use NLM prefixes. These partial category counts do not establish that every residual is excluded; refresh and finish the residual review.


## Documented supplement and numbered-item labels

Candidate 17 finished its full 11,316,630-string comparison: 16,820 recovered strings / 31,279 recorded uses, zero lost matches, and 1,356 changed assignments / 3,981 uses. It leaves 177,558 strings / 315,426 uses unmatched. These residuals still need review; they are not all declared excluded.

Added complete `Supp.` / `Supl.` supplement suffixes and numbered `Reihe`, `Book`, and `Unit` captions. Their spellings are documented in [Western Libraries' spine-label table](https://www.lib.uwo.ca/collections/cataloguing_practices/abbreviations_for_use_on_spine_labels.html). Examples include `AC145 .I93 1982 supp.2 v.14`, `B2967 .B6 1975 Reihe 2A Bd. 12`, and `D387 .R425 pt. 1, unit 1`. The parser validates the preceding call and the complete suffix and retains original match evidence. Unknown prose, bare unfinished Unit, and catalog-processing phrases remain rejected. `Sup.` was not inferred from these other spellings.

Release tests: 185 passed, zero failures (`tests-supplement-labels.log`). Candidate 18 source/text reference manifest is `candidate18-manifest.json`, and its release build log is `build-after18.log`. Frozen taxonomy stays unchanged. Full candidate 18 comparison and continued residual review remain pending, along with final matcher-version invalidation and final completion audit.

Candidate 18 release build succeeded. Its full resolver is running in tool session 92831, writing `after18.tsv`; last live poll confirmed it running. Once terminal, run `compare.py after18.tsv eighteenth` and annotate the completed output. Do not read a partially written resolver output as a completed comparison.


## Compound supplementary volumes

Candidate 18 full comparison completed: 17,131 recovered strings / 31,624 uses, zero lost matches, and 1,356 changed assignments / 3,981 uses. The changed-assignment set is identical to candidate 17. This adds 311 recovered strings / 345 uses beyond candidate 17. `remaining18.csv` is complete; `residual-metadata-review18.json` inventories remaining caption tokens but includes excluded markers and must not be treated as a validity classifier.

Added compound supplementary volume enumeration, including `ML5.A63 Suppl.Bd.14`, `ML1700 .A63 Suppl. Bd. 20`, and `B11 .A7 Suppl., v.62, 1988`. A supplement prefix is detached only when followed by a recognized caption with a number; the full remaining suffix must parse. Truncated `Suppl. V.`, unknown text, and doubled punctuation are rejected. Source context: [LC G155](https://www.loc.gov/aba/publications/FreeCSM/G155.pdf) specifies supplementary designation on the original call; Western Libraries' previously linked table documents Suppl./Supp./Supl. and Bd./volume labels. Composition is supported by the observed local records.

All 186 release tests passed (`tests-compound-supplement2.log`). Candidate 19 copied source and all root text reference data, preserving frozen SQLite (`candidate19-manifest.json`); release build succeeded (`build-after19.log`). Its full resolver is running in tool session 50217, writing `after19.tsv`. Revalidate its handle before comparing with tag `nineteenth` and annotating the completed output. Final version invalidation, residual review, and completion audit are still pending.


## Dutch/German series and matcher invalidation

Candidate 19 completed all 11,316,630 rows: 17,475 recovered strings / 32,038 uses, zero lost matches, 1,356 changed assignments / 3,981 uses. This adds 344 recovered strings / 414 uses beyond candidate 18.

Added Dutch n.r./n. r. new-series captions, dl. volume numbering, joined `deel140`, and German Neue Reihe. Examples include `AS244 .A512 n.r. dl.70 no.2` and `H31 .W5 Neue Reihe, Bd. 1`. Unknown trailing text and `deel-100` remain rejected. Primary evidence: [Rijksmuseum catalog record](https://www.rijksmuseum.nl/en/collection/publication/Oudheidkundige-mededeelingen-van-het-Rijksmuseum-van-Oudheden-te-Leiden-Nuntii-ex-Museo-Antiquario-Leidensi--1e04e02103a73c5249e04bf8221011b0) uses N.R./nieuwe reeks and dl.; [Heidelberg-hosted bibliography abbreviation list](https://fid4sa-repository.ub.uni-heidelberg.de/254/1/01_Intr_etc_.pdf) expands N.R. to German/Dutch new series.

All 187 release tests pass (`tests-dutch-series2.log`). Candidate 20 source/text manifest and release build are complete. Full resolver is running in tool session 51183, writing `after20.tsv`; after termination compare using tag `twentieth`, then annotate the completed output with the candidate20 binary. Candidate19 annotation was not run after replacing the binary with candidate20, avoiding mixed-version annotation.

Live matcher versions advanced from LCC53/Unified100 to LCC54/Unified101 after candidate20 snapshot so consumers can invalidate cached projections. This is a version-only live lib.rs difference; matching logic remains the candidate20 snapshot. Fresh release tests for the version bump are in session60929/log `tests-version54.log`; verify result. Further parser edits require reassessing invalidation before final completion. Full residual review, final source consistency checks, and completion audit remain unfinished.

Version-bump release test session60929 completed successfully: 187 passed, zero failures (`tests-version54.log`).


## Composition of separators and volume ranges

Candidate 20 completed its full comparison: 17,568 recovered strings / 32,147 uses, zero lost matches, 1,356 changed assignments / 3,981 uses. The changed-assignment set equals candidate19. `remaining20.csv` is complete. `known-prefix-unmatched20.json` contains 208 canonical-looking known-prefix residuals; this is an investigation queue, not a declaration of valid classifications.

Investigating `B'3279'H9'BD. 18-19` exposed a runtime composition bug: apostrophe decoding validated the volume suffix, but the matcher subsequently attempted to interpret the volume range as a classification range. A new regression with numeric and bounded-Cutter selectors failed before the fix (`tests-composed-before.log`, expected subject2, got no subject). The runtime now applies validated item-suffix removal after display-separator normalization. Unknown text and incomplete ranges still fail. All 188 release tests pass after the fix (`tests-composed-after.log`).

Matcher versions advanced to LCC55/Unified102 for this runtime change. Candidate21 snapshots the current source and text inputs including those versions (`candidate21-manifest.json`), with frozen SQLite preserved. Its optimized release build succeeded (`build-after21.log`). The full resolver is running in session75506, writing `after21.tsv`. After successful termination, compare against baseline using tag `twentyfirst` and annotate with the same candidate21 binary. Residual review and final completion audit are still outstanding.


## Comma-separated enumeration and temporary storage recovery

Added complete comma-separated item enumeration, including `vol.5,6,7`, `no.88-89,94`, and `v.389-390, 398-399`. These lists are confined to an already recognized caption; unknown text and unfinished lists/ranges still fail. Annual-volume captions (Jahrg./Jaarg./ar.) retain the complete-year rule, so `Jahrg.44,76,Heft1` is not reinterpreted as an item list. This distinction was caught by the existing regression suite; the first implementation failed that test and was corrected. Final release tests: 189 passed, zero failures (`tests-enumeration-lists3.log`). Primary examples/guidance: [Smithsonian holdings](https://siris-libraries.si.edu/ipac20/ipac.jsp?aspect=subtab103&index=PSUBJ&menu=search&profile=liball&ri=41&session=176I28F5X9939.155193&source=~%21silibraries&term=Ethnoarchaeology+--+Periodicals.&uri=link%3D3100009~%21855670~%213100002), [UMass holdings procedures](https://library.umass.edu/wikis/acp/doku.php?id=five_college_annex_processing_procedures).

Candidate21 full scan failed terminally with ENOSPC (session75506 exit1). The first comma-list test build also failed for storage exhaustion, not a code diagnostic. Its partial `after21.tsv` was deleted without comparison. Older completed resolver TSVs were compressed to `/home/johan/.cache/bokheim/safe-lcc-repairs-20260907/archived-scans`; baseline `before.tsv` and latest completed `after20.tsv` remain locally. Archive session57968 completed; /tmp has 3.4GiB free after recovery. Old comparison JSONs and source manifests remain available. Do not restart failed candidate21: candidate22 supersedes it and includes both the runtime composition fix and comma-list repair.

Candidate22 manifest is updated to the final passing source (`candidate22-manifest.json`), and its final release build succeeded (`build-after22-final.log`). Full resolver is running in session40882, writing `after22.tsv`. After successful completion, run `compare.py after22.tsv twentysecond` and annotate with the same binary. Latest completed full comparison remains candidate20 (17,568 recovered strings / 32,147 uses, zero loss). Final version invalidation review, residual review, and completion audit remain outstanding.


## Explicit copies and an intermediate regression

Candidate22 full scan and annotation completed: 17,796 strings / 32,399 uses recovered versus original baseline, with zero original matches lost. However, comparison of the gained sets against candidate20 found four previously recovered strings lost (not visible as baseline losses). They are listed in `candidate22-regressions-versus20.json`: ordinal edition captions after a comma, such as `no.1, 2nd ed.` and `Heft44, 3rd ed.`. The comma-list parser now gives complete ordinal captions precedence over bare numeric continuation. All four examples are regression tests. Future full comparisons must check retention against candidate20/candidate22 as well as the original baseline.

Added explicit Copy/Cop. numbered labels after a validated complete call; unknown text and incomplete copy labels remain rejected. The shorter C. form remains ambiguous and was not added. Primary evidence: [LC MARC holdings examples](https://loc.gov/marc/holdings/examples.html) uses Cop.1; [Folger catalog record](https://catalog.folger.edu/record/253159) demonstrates copy 1/copy 2 and Cop.2; [Yale marking guidance](https://web.library.yale.edu/printpdf/book/export/html/3513) distinguishes copy information from the call number.

An exclusion audit of all 17,568 candidate20 gains found none of the exact excluded examples, catalog-marker prefixes, or NLM prefixes checked (`candidate20-exclusion-audit.json`). This is a bounded check, not proof that all remaining strings are excluded.

Latest live release suite: 192 passed, zero failures (`tests-copy-and-ordinal.log`; count includes concurrent tests). Candidate23 copies current source/text inputs (`candidate23-manifest.json`); optimized release build succeeded (`build-after23.log`). Full resolver is running in session1224 writing `after23.tsv`. Once terminal, compare using tag `twentythird`, annotate, and explicitly confirm all candidate20 gains plus candidate22 gains still match. Final matcher invalidation review, remaining-format review and completion audit remain pending.


## Candidate23 completed verification and table review

Candidate23 full resolver, comparison and annotation are complete. Recovery versus the original baseline is 17,815 strings / 32,419 uses, with zero lost original matches and 1,356 changed assignments / 3,981 uses. The new `check_retention.py` also checked all 19,156 distinct repaired/reassigned strings from candidate20 and candidate22: zero missing rows, zero lost matches, and zero changed assignments (`candidate23-retention.json`). This confirms the four ordinal-caption regressions are restored. `remaining23.csv` is complete.

The current live source and root text reference inputs match `candidate23-manifest.json` exactly. Current matcher versions are LCC56 / Unified103, included in this snapshot. No build or scan remains running for this candidate. Latest live release test evidence remains 192 passed, zero failures.

Refreshed lexical residual triage is saved as `residual-triage22.json`. Its review categories are not assertions of invalidity. Investigated unresolved H8/H15/H50 expressions against the local reference database; complete queried rows and missing outer-scope lookups are saved in `residual-table-review23.json`. H8 supplies numeric geographic divisions, and H50 supplies relative .x operations; their names alone cannot establish the unresolved outer intervals in the imported strings. No guessed interval or parser change was made from that evidence. Further remaining-format review and final completion audit are still required; no completion claim is made.


## Part and chapter captions

Added numbered Band, Teil, Teilbd., Ch., Sec., and Cahier labels after complete calls. Examples include `BS1154 .A6 Teilbd. 22/2 2001`, `BS1235.52 .H473 ch.1-11 2018`, and `BJ1661 .S25 1984, Sec.3, no.4`. Original evidence is preserved; unknown prose, unfinished suffixes, and malformed caption punctuation still fail. Sources: [Western Libraries spine-label vocabulary](https://www.lib.uwo.ca/collections/cataloguing_practices/abbreviations_for_use_on_spine_labels.html) for Band/Teil/ch./cahier and sec.; [DNB Teilband/Teilbd. usage](https://katalog.dnb.de/DE/resource.html?id=1101612215&v=plist); [UW–Madison section abbreviation guidance](https://writing.wisc.edu/handbook/citation/docmla/citation_abbrev/).

All 193 release tests pass (`tests-part-chapter.log`). Candidate24 source/text snapshot (`candidate24-manifest.json`) and release build (`build-after24.log`) are complete. Tool session35228 runs a sequential checked script: write `after24.tsv`, run baseline comparison tag `twentyfourth`, run retention check against `twentieth`, `twentysecond`, and `twentythird`, then write `remaining24.csv`. Revalidate this handle and let its script finish; do not launch duplicate comparisons or replace its binary during the sequence. Latest fully verified result remains candidate23. Final invalidation review, residual review, and completion audit are outstanding.


## Ordinal volume/series captions

Candidate24 completed resolver, comparison, retention audit, and annotation successfully. Recovery is 17,872 strings / 32,495 uses, with zero baseline losses and 1,356 changed assignments / 3,981 uses. All 19,171 distinct prior repaired/reassigned strings checked from candidates20/22/23 retain their assignments. `remaining24.csv` is complete.

Added Roman and Arabic ordinal captions, including `294. Bd.`, `II. Reihe, 15. Hft.`, and `II. Reihe, 21./22. Hft.`. Ordinal ranges require both complete endpoints; invalid Roman numerals, missing endpoints, zero ordinals, and unknown captions are rejected. The existing ordinary/English ordinal handling remains. Source context: [Syracuse Library ordinal/abbreviation guidance](https://su-jsm.atlassian.net/wiki/spaces/library/pages/159811049/Abbreviations), previously used for numeric-period ordinals, combined with the complete locally observed labels. All 194 release tests pass (`tests-ordinal-volumes.log`).

Candidate25 source/text snapshot is `candidate25-manifest.json`. Tool session67220 runs a checked sequential script: release build to `build-after25.log`, resolver to `after25.tsv`, baseline comparison tag `twentyfifth`, retention check against candidates20/22/23/24, and annotation to `remaining25.csv`. Revalidate this handle; do not overwrite its binary or run duplicate comparisons while it is active. Final invalidation review, remaining-format review and completion audit are still outstanding.


## French/Scandinavian series and candidate25 verification

Candidate25 full verification completed successfully: 17,937 recovered strings / 32,566 uses, zero baseline losses, 1,356 changed assignments / 3,981 uses. Retention check against candidates20/22/23/24 checked 19,228 distinct strings with no missing rows, lost matches, or changed assignments. `remaining25.csv` is complete.

Added `nouv. ser.`, NFC/NFD-accented `nouv. sér.`, and `ny ser.` as complete new-series labels. Original accented evidence is retained. Fixed comma/colon before numbered series (e.g. `nouv. sér., 2` or `n.s., 2`); unknown text and empty/doubled separators remain rejected. Sources: [LC CONSER training manual](https://www.loc.gov/aba/pcc/conser/scctp/documents/pdf/CSRTrainingManual2009.pdf) and [Folger catalog](https://catalog.folger.edu/record/33072) document nouvelle série/nouv. sér.; [NOAA journal abbreviation list](https://spo.nmfs.noaa.gov/sites/default/files/List_of_Journal_Abbreviations_final.pdf) documents Ny Ser./Ny Serie, alongside the previously cited Western multilingual new-series table.

All 195 release tests pass (`tests-french-series.log`). No scan/build is currently running. These latest changes still need a fresh isolated source/text snapshot and full comparison (candidate26), including retention against candidate25 and earlier repaired sets. `post-french-series-live-manifest.json` records the tested live state. Candidate25 isolated binary remains the last fully compared binary. Final matcher invalidation review, remaining-format review and completion audit are outstanding.


## Valid-prefix audit and explicit shelving suffix

Candidate26 full verification completed: 18,008 recovered strings / 32,660 uses, zero baseline losses, 1,356 changed assignments / 3,981 uses. Retention against candidates20/22/23/24/25 checked 19,293 strings with zero missing rows, lost matches, or changed assignments. `remaining26.csv` is complete.

A new prefix audit evaluated 116,684 distinct candidate prefixes from 42,062 remaining candidate25 records using the candidate26 release matcher. It found 36,365 still-unmatched records with a matching prefix and unsupported remainder (`item-prefix-audit26-review.json`). This does not authorize dropping those remainders. The largest include unexplained single letters, x, REF, year-plus forms, and shelving labels. Full audit input and prefix matching output are retained.

Primary evidence resolves one suffix: [Yale's local modifications policy](https://web.library.yale.edu/cataloging/local-modification-lc-call-numbers) explicitly adds `(LC)` to call numbers in certain shelving runs. Added this exact suffix only after a validated complete call with a known LCC class, optionally followed by supported item enumeration. Standalone `(LC)`, CPB/LAW, NLM, unknown prose, and unfinished volume ranges remain rejected. All 196 release tests pass (`tests-lc-shelving-suffix.log`). Original evidence is preserved.

No process remains running. The latest suffix addition needs a fresh candidate27 source/text snapshot and full comparison/retention audit. `post-lc-suffix-live-manifest.json` records the tested live state. Candidate26 remains the last fully compared isolated binary. Final invalidation review, residual review and completion audit remain outstanding.


## Dated oversize signs and candidate27 verification

Candidate27 full verification completed: 18,307 recovered strings / 32,982 uses, zero baseline losses, 1,356 changed assignments / 3,981 uses. Retention against candidates20/22/23/24/25/26 checked 19,364 strings with no missing rows, lost matches or changed assignments. `remaining27.csv` is complete. The (LC) suffix change added 299 strings / 322 uses.

The prefix audit identified 1,256 still-unmatched strings / 1,369 uses ending in a complete year plus one or two plus signs. [Yale's local modifications policy](https://web.library.yale.edu/cataloging/local-modification-lc-call-numbers) documents historical oversize plus signs; [Yale score call-number guidance](https://web.library.yale.edu/printpdf/book/export/html/3367) documents +/++. Added interpretation only after a complete dated call with a known LCC class and numeric, single-letter Cutters. A plus after a bare class or classification range remains governed by existing scope rules. Unknown prefixes, incomplete years, excess plus signs, and trailing prose are rejected. All 197 release tests pass (`tests-dated-oversize.log`); original evidence is retained. The count above is a candidate inventory, not a claimed recovery count for the new rule.

No process is running. `post-dated-oversize-live-manifest.json` captures the tested live state. Next required full comparison is candidate28, including prior-retention checking. Candidate27 is the last fully compared isolated binary. /tmp has about 1.7GiB free; archive older task-created scans before exhausting it again, keeping baseline and latest completed output. Final invalidation review, remaining-format review and completion audit remain outstanding.


## Explicit folio-size labels

Added `Folio` and `fol.` after a complete dated call with a known LCC class, using the existing validated item-descriptor branch. Examples: `B74 P4414 2004 fol.` and `CN1178.S5 F8 1997 Folio`. The ambiguous single-letter F and undated/unexplained trailing labels are not interpreted by this branch. Primary evidence: [Yale folio marking policy](https://web.library.yale.edu/cataloging/cms/special-processing/folio) and [Western's spine-label vocabulary](https://www.lib.uwo.ca/collections/cataloguing_practices/abbreviations_for_use_on_spine_labels.html). All 198 release tests pass (`tests-folio-labels.log`); original evidence remains intact.

Candidate28 source/text snapshot and release verification sequence were started before this addition. Tool session46968 remains running as of the latest live poll: resolver to `after28.tsv`, baseline comparison tag `twentyeighth`, retention against candidates20/22/23/24/25/26/27, then annotation to `remaining28.csv`. Let this sequence complete without replacing its binary. `post-folio-live-manifest.json` captures the newer tested source; it will need candidate29 full verification after candidate28.

Archive session45741 completed: older task-created scans after22 through after26 were gzip archived under the durable cache's `archived-scans` directory and removed from /tmp. Baseline and latest completed after27 remain local. /tmp currently has about 2.7GiB free. Final matcher invalidation review, remaining-format review and completion audit are still outstanding.


## Oversize assignment equivalence and supplied enumeration

Candidate28 is fully verified: 19,513 recovered strings / 34,292 uses, zero baseline losses, 1,356 changed assignments / 3,981 uses. Retention checked 19,663 prior repaired/reassigned strings with no missing rows, lost matches or changed assignments. All 1,206 newly recovered oversize-sign strings independently resolve to exactly the same assignment as the underlying dated call (`oversize-assignment-audit28-result.json`, zero mismatches).

Candidate29 snapshots the folio-label addition and is running in session64485. Its full resolver completed; the sequential script is comparing with tag `twentyninth`, checking retention against candidates20/22/23/24/25/26/27/28, then annotating `remaining29.csv`. Do not replace its binary or launch duplicate comparison steps while active.

Live matcher versions advanced to LCC57/Unified104; release suite passed198 (`tests-version57.log`). Subsequent remaining-record review found269 numeric supplied enumeration forms such as `v.[16]`. [OCLC holdings guidance](https://help.oclc.org/Metadata_Services/Local_Holdings_Maintenance/A_holdings_primer/10Z3971) documents square brackets around supplied enumeration. Added only complete numeric bracketed values after recognized item captions; empty, nested, unclosed, uncertain and arbitrary-text brackets remain rejected. Original evidence is preserved. All199 release tests pass (`tests-supplied-enumeration.log`). The269 count is a candidate inventory, not yet a full recovery count.

`post-supplied-enumeration-live-manifest.json` captures the current tested source. These changes need candidate30 full verification after candidate29's sequence finishes. Overall remaining-format review and final completion audit remain unfinished.


## Candidate29 complete; ordinal session captions

Candidate29 completed: 19,689 recovered strings / 34,512 uses, zero baseline losses, 1,356 changed assignments / 3,981 uses. Prior retention checked 20,869 strings with no missing rows, losses or changed assignments. Remaining: 174,689 strings / 312,193 uses. The folio addition recovered 176 strings / 220 uses.

Candidate30 includes bracketed supplied enumeration and versions57/104. Its release build passed; full resolver/comparison/retention/annotation is running in session96819. Do not replace its isolated binary until terminal.

Remaining-record review found91 strings /102 uses containing sess.; this is an inventory, not an impact count. Added SESS. to existing validated ordinal captions for observed 67th sess., no.4 and similar complete item numbering. UCLA call-number marking guidance documents sess. as session: https://unitproj.library.ucla.edu/cataloging/procedures/CallNumberPlacement.pdf . Malformed ordinals, unknown suffixes and incomplete ranges remain rejected. Original evidence is preserved. Release test session1424 was started; verify its terminal result. This later change is not in candidate30 and needs a subsequent snapshot. Overall residual review and completion audit remain unfinished.

Ordinal-session release tests completed successfully: test result: ok. 200 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.39s


## Hungarian volume captions

Added documented köt. volume captions, including observed unpunctuated forms, ASCII-uppercased composed/decomposed accents, caption-before-number and number-before-caption placement, and combination with new-series labels. The LC-PCC Hungarian abbreviation table is primary evidence: https://www.loc.gov/aba/rda/mgd/mg-ses-abbreviations-capital-languages.pdf . Complete enumeration and the underlying call remain required; arbitrary trailing text and incomplete ranges remain rejected. Original evidence is retained. All201 release tests pass in tests-hungarian-volumes.log. The prior 91-record inventory counted only composed accents, so is not a total impact count.

Candidate30 session96819 remains live, confirmed by polling; it verifies the earlier bracketed-number snapshot. Do not replace its isolated binary while running. Session87698 (live release tests) is terminal success. post-hungarian-volumes-live-manifest.json captures the newer source. Candidate31 will need both ordinal-session and Hungarian-volume additions, plus full comparison and prior-retention checks. Residual review, matcher invalidation review and completion audit remain unfinished.


## Candidate30 completed; Hungarian part and issue labels

Candidate30 completed: 19,925 recovered strings /34,750 uses, zero baseline losses, 1,356 changed assignments /3,981 uses. Retention checked21,045 strings with zero missing/lost/changed. Bracketed supplied enumeration added236 strings /238 uses. Remaining174,453 strings /311,955 uses.

Current source had concurrent reviewed_call_base changes and versions58/105. Inspected exact one-row registry PA85.G 646A3 DATE -> PA85.G646.A3; preserved it. Candidate31 snapshots all current Rust/text data including ordinal-session and Hungarian-volume additions and that concurrent registry, keeping frozen taxonomy. Release build and live203 tests passed. Full resolver/comparison/retention/annotation session23271 is running; do not replace its binary until terminal.

Added Hungarian rész (part) and füzet (issue/fascicle) captions, supported by University of Szeged bibliography glossary: https://acta.bibl.u-szeged.hu/37997/1/biblio_001.pdf . Remaining30 inventory contains13 such strings /13 uses including accent variants. This is not a verified gain count. Complete values and suffix consumption remain required. Expanded evidence-preservation and malformed-tail regressions; all203 release tests pass in tests-hungarian-parts.log. This later change needs candidate32 full verification. post-hungarian-parts-live-manifest.json records it.

Archiving older task scans after27/28 is running in session94661, with decompression-size validation before local deletion. Overall residual review, invalidation review, and completion audit remain unfinished.


## Restored verification workspace and workbook composition

The temporary workspace was cleared externally; old candidate31 handle was missing. Saved durable baseline, source edits and completed candidate30 reports remain intact. Candidate31's resolver previously reported20188 gains/35068 uses but retention/annotation were not saved, so it is not treated as fully verified. Reconstructed isolated workspace from live source with saved frozen taxonomy; SHA256 matches original baseline provenance fa4d7bc0445ae06fe0d4e66dc863c9f6ca0316f1d1cca3711e3f65751d04fc7c. Required included migration was restored after initial isolated build failure; retry release build passed.

Added dated Workbook, complete numbered enumeration OF four-digit year, and French NOUV. PÉRIODE including accent variants. All204 release tests passed. Sources: UBC standard suffixes https://techserv.library.ubc.ca/files/2011/10/SUFFIX-July2006.pdf ; BnF series record https://catalogue.bnf.fr/ark%3A/12148/cb327013645 ; publisher volume-year usage https://www.history.co.zw/downloads-2/ . Candidate32 session77914 is running resolver/comparison/retention against saved prior deltas through28. Later29/30 deltas were lost; their saved summaries alone cannot prove per-code retention. Full reconstruction/audit remains required.

Subsequent workbook inventory reviewed all79 strings/80 uses. Added exact undated Workbook and Student Workbook suffixes and composition after complete numbered volumes, requiring known class and numeric single-letter Cutters. Tests running session29388; this newer source is not in candidate32. post-workbook-composition-live-manifest.json saved durably. Table review of B133.M184, BL1225.M863, BR60.F3 found no exact reference source starts; no table meaning was guessed. Remaining workbook variants, table/range review, final invalidation and completion audit remain outstanding.

Workbook-composition release tests completed: 204 passed, zero failed. Session29388 terminal success. Candidate32 remains running in session77914; preserve its isolated binary until terminal.


## Cache invalidation and Finnish numbered items

Advanced LCC matcher59 /Unified106 for accumulated caption and workbook repairs. Version-only release tests passed204. Revisited the broad saved prefix audit: N:O numbered-item suffix group accounts for155 uses in that older audit; not a current gain count. LC CONSER caption abbreviations explicitly document Finnish numero -> n:o: https://www.loc.gov/aba/pcc/conser/conserhold/Captabbr.html . Added N:O to numbered captions with complete number/range and full suffix validation; evidence retained. New release suite session21182 is running. post-finnish-number-live-manifest.json saved durably.

Candidate32 full scan remains running in session77914; preserve its binary. Live workbook-composition, version59/106 and Finnish N:O changes postdate candidate32 and need a subsequent source snapshot/full scan. No completion claim; remaining formats, range/table review, and final audit are outstanding.

Finnish-caption release tests completed:205 passed,0 failed; session21182 terminal success. Candidate32 output reached275MiB and session77914 remains live for sequential comparison/retention.


## Candidate32 taxonomy mismatch identified and recovery started

Initial candidate32 comparison completed with20302 gains/35188 uses, zero lost matches but249394 changed strings and425 prior-retention assignment differences. This is NOT a valid stability result: the restored edition-audit snapshot ends at concept53204, while the actual saved before.tsv uses all concepts through53255. The hash check verified the older edition audit, not the parser baseline; prior wording claiming the baseline was restored is superseded.

Searched preserved repository tmp databases. Four snapshots with16252 concepts/max53255 have identical concept/parent/LCC selector/range content: lcc-dated-calls, lcc-reviewed-parent-endpoints, lcc-reference-coverage-v96, lcc-card-navigation-repair. All baseline concept IDs exist in them. Copied lcc-dated-calls snapshot to recovered-parser-baseline-taxonomy.sqlite3 in /tmp and durable cache, with source/hash in taxonomy-recovery.json. Full candidate32 binary is rerunning against this recovered snapshot, then comparing and checking saved prior-retention. This remains a candidate recovery until full assignments verify it; do not claim completion. Live source still includes newer workbook-composition, Finnish N:O and versions59/106, all205 release tests passed; these require next snapshot.


## Remaining-record diagnostic audit

Corrected candidate32 comparison remains live session50423. Reviewed PK2903/DS588/PN5449/LC910 plus B133.M184/BL1225.M863/BR60.F3 against local reference spans. Exact base spans do not establish meaning of appended numeric/name/table notes; no guessed parser change added. Evidence saved table-note-review32.json.

Prepared174076 remaining strings from initial candidate32 as a diagnostic subset. Because that scan used an older taxonomy this is a queue, not proof of complete current residual coverage. Built current live lcc_gaps example in optimized release successfully (build-live-gaps.log) and launched it against the recovered parser-baseline taxonomy and this subset. It distinguishes parser rejection, unmatched valid syntax, and ambiguous matches with outline evidence; results remain advisory and require semantic review. Source is live versions59/106 including Workbook composition and Finnish N:O. Overall range/table and remaining-format reviews remain incomplete.

Live diagnostic session36059 completed:160 subset strings/182 uses now match;168700 strings/301961 uses parser-rejected;5216 strings/9374 uses canonical but unmatched. This is not an exclusion certification or final full-dataset impact. Diagnostic CSV/summary saved durably. Corrected full comparison remains session50423.


## Corrected candidate32 verified; candidate33 underway

Corrected candidate32 completed all11316630 rows:20302 recovered strings/35188 uses, zero baseline losses,1356 changed strings/3981 uses, restoring the historical assignment-change count. Prior retention checked20869 strings with zero missing/lost/changed assignments. This directly supports the recovered taxonomy; the initial older-taxonomy candidate32 is superseded and must not be used for stability totals. Corrected summary/delta/retention saved durably.

Reviewed all199 canonical-but-unmatched diagnostic entries with known initial classes (262 uses). All contain a hyphen or semicolon:79 alphabetic/table expressions,60 joined/cross-class expressions,57 other hyphenated expressions,3 literary item marks after semicolon. known-prefix-review32.json records each; these categories are unresolved interpretations, not a claim that every row is invalid. No simple plain point coverage gap appears in this bounded set. Parser-rejected records still need review.

Candidate33 snapshots current source/text data, required migration, and the recovered taxonomy; includes Workbook composition, Finnish N:O, versions59/106. Sequential release build/full resolver/baseline comparison/prior retention is running session57784. Preserve its binary while active. On completion results are copied to durable cache automatically. Overall remaining-format and source interpretation review plus final audit remain outstanding.


## Brackets enclosing complete item captions

Diagnostic residual review found153 strings/154 uses ending in bracketed volume/part/number captions. Added one enclosing square-bracket pair around a complete numbered caption, e.g. CT3990.Y83 A2 2009 [v.2] and BS741 .A3 1982, [v.11d]. Canadian Mennonite University call-number guidance shows [v.1]: https://www.cmu.ca/library/cat/classification.shtml . Require a recognized caption and number, complete suffix validation, and no nested brackets; unknown notes, missing values, incomplete ranges and trailing text remain rejected. Original evidence retained. Expanded supplied-enumeration regressions, release test session43185 running. post-bracketed-captions-live-manifest.json saved durably.

Candidate33 session57784 remains live and predates this addition; do not replace its isolated binary. Inventory count is not yet verified gain count. Remaining review and final audit stay open.

Bracketed-caption release suite completed: test result: ok. 205 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.15s Session43185 terminal success.


## Candidate33 complete and additional CONSER captions

Candidate33 completed11316630 rows:20462 recovered strings/35370 uses, zero baseline losses,1356 changed strings/3981 uses. Prior retention checked21658 strings:zero missing/lost/changed assignments. Results automatically copied durably; session57784 terminal success.

Reviewed remaining labels against LC CONSER caption list https://www.loc.gov/aba/pcc/conser/conserhold/Captabbr.html . Added observed sér., afl., ptie/pties, roč., seš. to existing numbered-caption parsing with composed/decomposed accents; preserve evidence and reject incomplete values/tails. GD. music-item examples were not interpreted as Latvian-year abbreviation merely because the letters match. Release tests running session8592. post-additional-conser-live-manifest.json saved durably. Bracketed-caption and these later additions need candidate34 snapshot/full comparison. Review annual ROČ comma continuation consistency before finalizing; overall residual audit incomplete.

Annual ROČ captions now use the same shortened-chronology rejection as JAHRG/JAARG/AR; added regression. Final release suite206 passed,0 failed (tests-additional-conser-final.log), session29065 terminal success. Candidate34 snapshots bracketed-caption and additional CONSER changes; sequential full release build/comparison/retention launched, automatic durable summary/delta/retention copies. No completion claim.


## Serial year:issue notation

Residual diagnostic inventory has1862 strings/1910 uses ending in four-digit year:digits. Publisher issue archive documents this numbering: https://www.sis-group.org.uk/sis-review/ . Added only complete year:issue after a valid known non-law call with numeric single-letter Cutters; the dated prefix is retained as match base and original notation as evidence. Missing/truncated values, extra colons/text, unknown classes, bare classes and law calls are not interpreted by this branch. Inventory is not an actual gain count. Release tests running session21875. post-serial-year-issue-live-manifest.json saved durably.

Spacing inventory separately found9 strings with no .56, vol .76 or v .25; these remain queued. Candidate34 session41309 remains active and predates the year:issue addition. Keep its binary unchanged. Overall residual audit remains incomplete.

Year:issue release suite completed: test result: ok. 207 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.75s Session21875 terminal success.


## Candidate34 verified; spaced caption periods

Candidate34 completed:20628 recovered strings/35539 uses, zero baseline losses,1356 changed strings/3981 uses. Prior retention checked21818 strings, zero missing/lost/changed assignments. Session41309 terminal success; durable artifacts copied automatically.

Added exact observed NO ., VOL ., V . caption spellings before their shorter alternatives so whitespace before the period does not prevent numbered-item interpretation. Reviewed9 such diagnostic records. Expanded evidence-preservation and incomplete/unknown-tail regressions; all208 release tests pass (tests-spaced-caption-period.log), session25820 terminal success.

Saved reusable verify_candidate.py plus workspace Cargo configuration and resolve_all.rs in durable cache. It snapshots sources/reference data and required migration, restores baseline and preserved taxonomy, uses release build only, performs full comparison/prior-retention, and copies outputs durably. Candidate35 session18105 is running that procedure, including serial year:issue and spaced captions. Preserve its binary while active. Remaining interpretation review and completion audit stay open.


## Exclusion and underlying-call assignment audits

Candidate34's20628 gained strings were checked for exact excluded marker/prefix families CPB, LAW, IN PROCESS, ACQUIRED FOR NLM, JP, WEB, AEU and NLM WA-WZ/QS-QZ patterns:zero violations. This is a bounded lexical exclusion audit, not complete semantic certification. exclusion-audit34.json saved durably.

All326 gains since corrected candidate32 had a validated matching prefix in the preserved prefix audit. Independently resolved those prefixes with the release matcher against recovered parser-baseline taxonomy:all326 original repaired assignments equal their underlying-call assignments, zero mismatches, no missing audit rows. assignment-prefix-audit34.json/tsv saved durably. Current candidate35 remains running session18105; no source changes this audit turn. Broader residual interpretation review and completion audit remain open.


## Textbook grade suffixes

Residual diagnostic search found703 strings/734 uses containing numeric gr./grade forms, including other classification systems and incomplete calls; this is not a projected recovery count. Tarleton Library documents GR. grade suffixes in LCC textbook call numbers: https://dslreference.blogspot.com/2018/05/ . Added exact GR./GRADE after complete dated known-LCC call with numeric single-letter Cutters, accepting K or1..12 and complete ascending ranges. Reversed/unfinished/out-of-range grades, arbitrary tails, undated/unknown calls and NLM examples remain rejected. Original evidence retained. Release tests session33832 running. post-textbook-grade-live-manifest.json saved durably. Candidate35 session18105 remains running and predates this addition; preserve binary. Full residual review and final audit remain unfinished.


Candidate35 completed:22317 recovered strings/37270 uses, zero baseline losses,1356 changed strings/3981 uses. Prior retention checked21984 strings:zero missing/lost/changed assignments. All1689 gains since34 independently match their prior validated underlying-call prefixes (assignment-prefix-audit35.json, zero mismatches/missing). Grade-label release suite209 passed,0 failed; session33832 terminal success. Candidate36 full build/comparison/retention launched with current grade-label source and candidate35 included in prior-retention inputs. Results auto-save durably. Remaining-format review and final audit open.


## Remaining workbook details and reading levels

Candidate35 residuals contain five Workbook strings. Added complete teacher-edition, PART-number, and publication-year-after-Workbook combinations, retaining the original notation and validated underlying call. Existing library accompanying-material and curriculum-label sources support the forms. First workbook-detail release suite passed (session72801). Then added observed LVL./LVL/LEVEL numbered reading-level captions, including composition in PE1128.A2 R43 1994 lvl1, workbook. Library examples: https://library.fue.edu.eg/cgi-bin/koha/opac-detail.pl?biblionumber=1534 and https://hpl.bibliocommons.com/v2/search?origin=core-catalog-explore&query=Warner%2C+Gertrude+Chandler&searchType=author . Inventory33 strings/35 uses, not an impact count. Full suffix validation retained, expanded malformed-tail regressions.

Main-workspace release test command failed before compilation because concurrent Cargo changes made --locked unsatisfiable; did not alter shared Cargo.lock. Created independent isolated-tests workspace using current sources/text references and a read-only SQLite backup of live taxonomy; release --offline tests running session21336 with its own lockfile. This testing database is separate from the preserved parser-baseline taxonomy. Candidate36 full comparison remains running session62663 and predates these additions. post-workbook-level-live-manifest.json saved durably. Broader review remains open.

Isolated current-source release suite passed: test result: ok. 209 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.80s Session21336 terminal success. Candidate36 resolver reported22698 gains/37666 uses; retention still running in session62663, so not yet fully verified.


## Candidate36 complete and documented ebook suffix

Candidate36 completed22698 recovered strings/37666 uses, zero baseline losses,1356 changed strings/3981 uses. Retention checked23673 strings:zero missing/lost/changed. All381 grade-label gains independently equal their validated underlying-call assignments, zero mismatches or missing (assignment-prefix-audit36.json). Session62663 terminal success.

Primary documentation confirms eb appended to ebook call numbers: University of Maine https://tdx.maine.edu/TDClient/3094/Portal/KB/ArticleDet?ID=173348 ; Oregon State catalogers' published workflow https://journals.ala.org/lrts/article/view/5335/6515 ; Nebraska library newsletter https://govdocs.nebraska.gov/epubs/C2800/N004-2001fal.pdf . Added EB/EBOOK only when removal leaves an independently valid known LCC call, optionally with validated enumeration. Valid literary item marks and bare numeric class numbers retain their underlying meaning. No range, unknown prose, incomplete suffix or other classification system is inferred. Original evidence is preserved. Inventory1510 strings/1541 uses ending year-eb/ebook/space-eb, not a recovery count. Release tests running session11957 in isolated current-source test workspace. post-ebook-suffix-live-manifest.json saved durably. This and the prior workbook-detail/reading-level changes need candidate37 full verification. Overall review remains incomplete.

Ebook release suite210 passed,0 failed; session11957 terminal success. Candidate37 sequential full verification launched with candidate36 in prior-retention list. No process from earlier scans remains running.


## Ebook complete-call validation tightened

Code review found valid_lcc_remainder accepts alphabetic filing-family fragments as well as complete item calls, so using it alone in the new ebook branch could strip eb from an incomplete or unexplained remainder. Added per-part validation for numeric single-letter Cutters and complete terminal date words, retaining already supported literary work marks and bare numeric classes. Regressions now reject .UNKNOWN, .A, malformed date words and unknown tokens following dates. This changes only new ebook-suffix eligibility. Release tests session67800 running in isolated-tests; post-ebook-completeness-live-manifest.json saved durably.

Candidate37 session42062 remains live and predates tightening. Its result must be followed by a newer candidate, with explicit review of any removed candidate37 matches so legitimate complete calls are not accidentally lost. Overall review and completion audit remain open.

Ebook completeness release suite: test result: ok. 210 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.41s Session67800 terminal success.


## Ebook tightening impact:17 records require review

Built an independent current strict release resolver in isolated-tests (session61027 terminal success). Compared all144569 cache strings ending eb/ebook with candidate37's broader ebook rule using the same recovered taxonomy. Targeted session90815 completed with17 changed assignments; exact rows and both resolver outputs saved durably in ebook-tightening-impact.json and ebook-before/after-tightening.tsv.

These17 are not automatically accepted losses. Cases include malformed dates (201; 2 013), possible0/O typo044 (excluded), repeated eb/ebeb markers, eb before and after volume enumeration, date-before-Cutter1977.M5217, old literary Lo5 mark, and H236s/W67a workmarks. Investigate and repair documented complete forms before finalizing. No generic unknown-text or numeric correction authorized. Candidate37 session42062 remains live for retention after resolver/comparison printed23844 gains/38839 uses and15926 changed strings/18796 uses. That much larger changed-assignment set also requires an underlying-call equivalence audit; no stability claim yet. Overall goal incomplete.

Candidate37 session42062 is terminal: prior-retention checked24054 strings, zero missing/lost but16 changed assignments. Samples include MARC-delimited eb calls and eb preceding previously supported incomplete enumeration. These16 need explicit underlying-call comparison in addition to the total15926 changed-string audit. Candidate37 is not accepted as a stable verified repair batch yet. No process remains running at turn end.


## Ebook assignment equivalence and repeated-marker repair

Of14570 new changed assignments in candidate37, all14166 direct eb/ebook suffix cases independently match their call number after removing that suffix. The other404 have eb followed by supported item metadata; all404 independently match the call before eb. Both audits have zero mismatches (ebook-assignment-audit37.json and ebook-indirect-audit37.json, inputs/outputs saved durably). Thus the larger changed-assignment set reflects underlying-call matching rather than arbitrary reassignment; this does not certify every previously permissive call as complete.

Addressed a subset of17 stricter-validation differences: repeated EB markers and EB before numbered volumes. After removing the outer marker and validated enumeration, collapse further EB markers only if the resulting base ends in a digit, then apply strict completeness. Preserve a literary title mark ending EB (PZ7.A1 DEBEB -> PZ7.A1 DEB); do not treat arbitrary trailing letters as redundant markers. Added repeated-marker and literary-title regression cases. All210 release tests pass in tests-repeated-ebook.log, session13983 terminal success. post-repeated-ebook-live-manifest.json saved durably. Remaining17-case audit needs rerun with this source; date-before-Cutter and legacy workmark cases still need review. No full candidate38 yet; candidate37 binary remains available for targeted comparison. Overall goal incomplete.


## Repeated ebook audit and year-before-Cutter

Recompiled strict repeated-marker resolver (session98598 terminal). Repeated audit144569 rows completed session41068:9 differences from original broad ebook rule remain, down from17. Exact rows in ebook-repeat-impact.json and output saved durably. Thus8 repeated-marker cases restored.

Added complete four-digit year followed by period and complete numeric Cutter within a remainder token, preserving QA76.5 1977.M5217 before EB removal. Missing-year digits and arbitrary .UNKNOWN still rejected. All210 release tests pass (tests-ebook-year-cutter.log), session89559 terminal. post-ebook-year-cutter-live-manifest.json saved durably. Audit needs rerun to confirm expected8 remaining differences.

Source search found corroboration for TX553.A3 W67a in Open Library's LC-sourced edition OL5242465M, LCCN75313994: https://openlibrary.org/works/OL2353113W/Toxicological_evaluation_of_some_food_additives_including_anticaking_agents_antimicrobials_antioxida . Record lists TX553.A3 W67a 1974 no.5 and LC MARC source. Do not dismiss that workmark as invalid; inspect linked MARC to support a scoped legacy-base allowance if needed. QP905.H236s and PZ3.M8346 Lo5 remain source-review candidates. G350 direct PDF fetch failed403; alternate source evidence remains available, so no blocker. All other malformed/ambiguous cases must retain original evidence without inferred typo corrections. No full candidate38 yet.


## Candidate38: confirmed ebook repairs verified

Completed the repeated ebook marker and year-before-Cutter repairs and added a narrowly scoped allowance for TX553.A3 W67A. The LC MARC 050 in LCCN 75313994 explicitly records TX553.A3 $b W67a 1974 no. 5; saved source HTML and edition JSON in the durable audit directory. Adjacent unverified marks and unknown trailing words remain rejected. Original call evidence is retained. Bumped LCC matcher 59→60 and unified matcher 106→107 for cache invalidation.

Current source matches candidate38 manifest. All 210 release library tests pass, including positive source-backed legacy form and negative adjacent-mark, malformed-date and unknown-text cases (tests-ebook-final.log). Candidate38 full scan completed: 11,316,630 strings examined; cumulative baseline gains 23,842 strings / 38,837 uses; zero baseline lost matches; 15,921 changed assignments / 18,791 uses. Prior retention checked 24,054 strings, zero missing/lost matches, 16 changed assignments already covered by the candidate37 underlying-call equivalence audits.

Full candidate37→38 comparison found exactly seven changes, all reverting to the pre-ebook candidate36 assignments. These are incomplete/ambiguous strings DT159.944 S69 201 eb; HD58.7.R4343 2 013eb; QA76.73.R3DbL43 2016eb; QA76.76.C672 044 2000eb; Z6265 .B53 2000 AZ221eb; and still-unverified legacy marks PZ3.M8346 Lo5eb and QP905 .H236s v. 175eb. No guessed correction was added. Full results saved in ebook-final-impact38.json. This restores ten of the initial seventeen strictness differences using validated complete forms; seven unsupported interpretations remain disabled. The verified batch adds 1,144 matching strings / 1,171 uses over candidate36.

This batch is complete. Broader remaining-format review is still open: 170,536 unmatched strings / 307,868 uses are not collectively certified invalid. Investigate the two legacy work marks and other documented residual formats separately; preserve excluded processing markers, other classification systems, ambiguous typos and clipped ranges. No taxonomy rows were changed or replaced. All scan and test processes completed successfully.


## Candidate40: final period on complete dated calls

Added interpretation of one terminal period after a complete four-digit publication year (optional single year suffix). Preserve the entire dated call and original evidence. The preceding call must have a known LCC subclass and fully consumed numeric single-letter Cutters, or the existing validated PR/PS/PZ literary work-mark form. A class with only a complete date is also supported. Truncated class/decimal/Cutter fragments, incomplete dates, double periods, arbitrary text, NLM and unknown prefixes remain unchanged. This is punctuation normalization after a complete date; no missing digits or subject codes are inferred.

Candidate39 numeric-Cutter release scan recovered477 strings/533 uses. Extended the same rule to existing validated literary title marks, then completed candidate40 full verification of current live sources. Candidate40 adds485 strings/543 uses versus candidate38, all independently verified to match the raw call after removing its final period. Zero lost matches, zero existing assignment changes versus candidate38; terminal-period-audit40.json includes all485 rows and zero mismatches. Cumulative baseline totals:24327 gained strings/39380 uses;15921 changed strings/18791 uses;zero baseline losses. Prior retention40240 checked,zero missing/lost,16 older ebook assignment refinements already audited. All211 release tests pass (tests-terminal-period-literature.log). Current source matches candidate40 manifest; matcher versions LCC61/unified108. All candidate39/40 tests, scans and audits are terminal successes. No live taxonomy rows changed.

Remaining work is open. Reviewed residual families against candidate38's unmatched set: terminal-year punctuation; year/issue slash; numeric Cutter followed by x. Year/slash counts contain many excluded MLCS/MLCM, microfilm and microfiche identifiers, so do not globally strip slash tails. Saved year-slash-residual38.json; actual LCC examples AS141.A52 belong to Acta Universitatis Carolinae, per OCLC050-099 and Finna institutional serial records. Source research supports numbering usage but no slash repair added yet.

Next documented candidate is Cutter-final x. UCLA's Exceptional Call Numbers for Rapid Cataloging (July1999), section2, explicitly instructs removing x at the end of a Cutter and retaining the remainder: https://unitproj.library.ucla.edu/cataloging/procedures/Exceptional%20Call%20Numbers%20for%20Rapid%20Cataloging.pdf . Residual inventory2947 strings/3108 uses;2509 candidate bases produce taxonomy matches after hypothetical removal. This is review-only, not a validated repair: require complete numeric Cutters, known LCC prefix, complete optional date and full-string consumption; do not remove arbitrary X letters, title marks, fractional Cutter letters or unknown text. Exact rows and candidate assignments saved durably in cutter-x-residual38.json and cutter-x-base-review.json. Broader goal remains incomplete; excluded processing markers, truncated/ambiguous codes, NLM/other systems and unknown prefixes remain excluded.


## Candidate42: documented numeric Cutter-final x

Implemented local_cutter_x_base and integrated it into canonical validation, subject paths, match-key evidence and runtime normalization. UCLA Exceptional Call Numbers for Rapid Cataloging (July1999), section2, explicitly documents removing x at the end of a Cutter while retaining the call. The rule requires a known LCC subclass, fully consumed single-letter numeric Cutters, a final numeric Cutter immediately before the optional spaced x, and an optional complete publication year (comma accepted). Internal four-digit dates preceding Cutters remain intact. Reject arbitrary text, bare-class x, x after dates, repeated x, incomplete years, X-initial numeric Cutters interpreted as suffixes, and ambiguous separated single-Cutter PR/PS/PZ title marks. Two-Cutter literary forms are supported. No original evidence is discarded and no taxonomy rows are edited.

Initial release suite failed one old assertion that HD69.C6X must be unresolved. Updated that assertion to the documented Consultants topic and retained HD69.C6Z as a rejection case. All212 release tests now pass, including added boundary, chronology and literary cases. Candidate41 scanned the initial numeric-only form; its production sources matched live except the test-only correction. Reviewing181 residual candidates showed valid internal-date and two-Cutter literary forms, added before final candidate42. Final source exactly matches candidate42 manifest (lcc.rs/lib.rs/runtime.rs); tests-local-cutter-x-chronology.log saved durably. Matcher versions LCC62/unified109.

Candidate42 full scan11,316,630 strings: cumulative baseline gains26805 strings/42008 uses;18253 changed assignments/21251 uses;zero baseline losses. Against candidate40,2478 newly matching strings/2628 uses and2340 assignment refinements,4818 total differences. All4778 direct suffix differences independently equal the source call without the suffix. The40 other differences have already-supported enumeration after x; removing only x while keeping their metadata independently gives the same assignments. Both audits have zero mismatches (cutter-x-audit42.json and cutter-x-indirect-audit42.json). Prior retention44908 checked,zero missing/lost matches,24 changed assignments:16 prior ebook refinements plus8 x/metadata refinements included in the40-case audit. Candidate41/42 and all audit/test sessions are terminal; first failed test run superseded by passing final suite.

Of2509 candidate-base matches in the initial x inventory,31 remain unresolved: x is attached to a bare class, follows a date, or could be a separated literary title mark. Exact rows in cutter-x-residual42-review.json. These are not silently declared invalid; the current source proves Cutter-final x only. Broader format review remains open.

Next documented group: Columbia CPM-605 explicitly describes F,FF,FFF as oversize suffixes on LCC calls, including NA123.A72 1999 FFF and N6853.P5 D83213 2008 F; it also places size designations after volume numbering. https://www1.columbia.edu/sec/cu/libraries/inside/clio/docs/bcd/cpm/cpmspe/cpm605.html . Candidate41 residual dated-call inventory:978 F strings/998 uses,1 FF,6 FFF. Saved folio-f-residual41.json. No F/FF/FFF repair added yet. Existing FOLIO/FOL. rule and original evidence must be retained. Year/issue slashes and two unverified ebook legacy marks also remain on the review queue. Preserve all previously excluded processing identifiers, other systems, ambiguous typos and clipped ranges. Overall goal remains incomplete.


## Candidate43: documented F / FF / FFF oversize suffixes

Added exact separated F,FF,FFF suffix interpretation in lcc_item_base, based on Columbia CPM-605's MARC852 $m instructions and explicit LCC examples. Require a complete dated call or recognized enumeration containing a number; validate the full remaining numeric Cutters/dates or the existing literary work-mark form and known LCC subclass. Preserve the underlying dated call or enumeration base and original evidence. Reject arbitrary suffix letters, FFFF, undated bare Cutters/title letters, truncated ranges, incomplete dates and unknown/other-system prefixes. Existing FOLIO/FOL. behavior retained. No taxonomy row changes.

An initial regression test incorrectly expected QA76.A1 V.F to be rejected. Code inspection confirmed existing lettered enumeration treats it as volume F, independently of the new oversize branch. Preserved that behavior explicitly in the test; the new branch still requires numeric enumeration for undated calls. Initial failed suites are superseded by tests-oversize-letters-verified.log:213 release tests passed. LCC matcher63/unified110. Current lcc.rs/lib.rs/runtime.rs exactly match candidate43-manifest.json.

Full candidate43 verification completed11,316,630 strings. Cumulative baseline gains27752 strings/42972 uses,18253 changed assignments/21251 uses,zero baseline losses. Versus candidate42:947 new matches/964 uses,zero lost matches,zero existing assignment changes. All947 independently equal the raw call with only its F/FF/FFF suffix removed, including existing volume metadata (oversize-audit43.json,zero mismatches). Prior retention has zero missing/lost matches; its24 older changed assignments were already audited in ebook and Cutter-x batches. All current scan/test/audit sessions are terminal. Completed records and source manifests saved durably.

Of978 dated F strings in the initial residual inventory,76 remain unresolved: unknown prefixes/other systems, damaged numeric punctuation and unsupported attached work letters, not merely the size suffix. Exact rows saved in folio-f-residual43-review.json. Do not silently classify these as invalid or strip their remaining unexplained text. No unmatched Roman-volume-plus-F examples were found in a bounded residual search.

Next evidence-backed review candidate is REF:411 residual strings/444 uses with leading or trailing REF (reference-label-residual42.json). UGA documents R/Ref as a reference-collection location code: https://libraries.uga.edu/user-services/locating/callnumbers . OCLC documents placing location stamps above or below call numbers: https://help.oclc.org/Metadata_Services/Connexion/Connexion_client/Cataloging/Print_labels/Get_started/50Add_an_input_stamp . This is still review-only; no REF repair added. Known-label handling must require a complete underlying LCC call and retain original evidence; do not strip composite unknown location text (e.g. So Asia Ref) automatically. The general residual-format review, year/issue slashes, legacy work marks and final completion audit remain open. Goal remains in progress.


## Candidate44: exact REF collection labels

Added reference_collection_base for exact leading REF or trailing REF around a fully consumed known LCC call, including existing volume enumeration. Integrated canonical validation, subject paths, match keys and runtime normalization; original evidence remains intact. Require complete numeric single-letter Cutters/dates or an existing valid literary work mark. Reject missing/incomplete calls, unknown/other-system prefixes, classification ranges, repeated labels and additional unexplained location text such as China Ref. Source: UGA documents Ref as a reference-collection location code; OCLC documents placing location stamps above or below calls (URLs in prior section). No taxonomy selectors contained REF in either live or frozen databases; no taxonomy rows changed.

All214 release tests pass (tests-reference-labels.log), including evidence and negative boundaries. LCC matcher64/unified111. Current production sources exactly match candidate44-manifest.json. Full scan11,316,630 strings completed: cumulative baseline gains28115 strings/43363 uses;18253 changed assignments/21251 uses;zero baseline lost matches. Candidate43→44 adds363 matching strings/391 uses with zero lost matches and zero existing assignment changes. All363 independently equal the raw call after removing only REF (reference-audit44.json,zero mismatches). Prior retention reports zero missing/lost matches; older24 audited refinements remain. All scan/test/audit processes terminal success.

48 of411 inventoried REF strings remain unresolved due to underlying unknown FC codes, literal YEAR placeholders, unsupported legacy work letters (e.g. BS491.2.A52b), or composite location text. Saved reference-label-residual44-review.json; do not misreport these as safely repaired or certified invalid. Next residual review includes year spans and indexes. Bounded inventory found29 Index+four-digit-year strings/33 uses;274 apparent terminal year spans after a numeric Cutter/292 uses (terminal-year-span-review43.json). The larger numeric-span set also contains volume ranges and Italian legal article references; do not conflate those with years or remove legal subdivisions. Year/issue slash and legacy work-mark reviews remain open. Attempts to retrieve G140/G350 through old LC URLs and the ClassWeb index still did not yield the documents; the index alone is not source proof. This does not block other source-record review. Overall goal remains incomplete.


## Candidate46: source-confirmed index years and remaining ebook legacy marks

Read-only source database lookup recovered edition IDs for all29 Index+year strings, both unverified ebook legacy marks, and two year-span examples (residual-source-editions45.json). Fetched Open Library edition JSON and linked original MARC records. Z1215.S5 Index2000 is explicit in LC update MARC05000 for OL3543081M. Although G140 prescribes no date after Index, this actual imported record proves the observed variant. OL13050638M also lists AC149.S73 H7921996 index1999, but its linked LC record instead has AC149.S73 X8 1999; do not cite that second MARC as proof of its imported call. The first record independently supports the normalization.

Extended existing Index enumeration handling to Index followed by exactly four digits, with full suffix consumption. Preserve any date already in the underlying call; never reconstruct one or treat the index year as a subject. Complete-source examples and incomplete/date-range/unknown-tail rejection cases added to existing tests. All29 inventoried Index+year strings recovered.

Columbia MARC0504 for OL12773809M explicitly records QP905.H236s v.175; LC MARC05010 for OL4886228M explicitly records PZ3.M8346 Lo5. Added exact legacy-base eligibility to the ebook branch alongside previously documented TX553.A3 W67a. Neighboring H236t/Lo6 and unexplained tails remain unsupported. Saved linked MARC HTML and edition JSON durably (legacy-source-12773809.html,legacy-source-4886228.html,source-OL*.json). QP905 ebook gains a match; PZ3 ebook now equals its underlying Lo5 call. This resolves both remaining unverified legacy marks from the earlier ebook strictness audit; the five malformed/ambiguous interpretations remain unchanged.

Candidate45 verified index years plus QP905:30 gains/34 uses,zero other changes versus44. Candidate46 adds PZ3 refinement:full11,316,630-string scan; cumulative baseline gains28145 strings/43397 uses,18254 changed assignments/21252 uses,zero baseline losses. Against44,30 gains/34 uses and one refinement,31 differences total. All31 independently equal calls with index-year metadata or eb removed; index-legacy-audit46.json has zero mismatches. Retention46398 checked,zero missing/lost,24 older audited refinements. Matcher versions65/112.

A concurrent edit changed only the joined-code regression test to use fixed selectors alongside live-taxonomy equivalence. Preserved it. Current production code matches candidate46 exactly before cfg(test); full source hashes recorded in post-candidate46-live-manifest.json. Re-ran current live-source release suite:214 tests pass (tests-index-years-current-live.log). All candidate45/46 scan/test/audit processes terminal success. No taxonomy rows changed.

Archived six task-owned full TSVs (after32,after32-corrected,after33..36) as gzip in durable directory, verified decompressed SHA256 against originals before removing temporary copies. Manifest archived-scans32-36.json. The wrong-taxonomy after32 remains superseded even though archived; recovered-parser-baseline-taxonomy.sqlite3 remains the correct verification baseline. Current44..46 TSVs remain in tmp.

Year-span investigation remains open. OL37903794M explicitly lists BX7715.B3 1833-1838, but linked GTU MARC fetch returned HTTP429; no range repair added and no immediate retry performed. Other available work remains, so this is not an overall blocker. Original source edition JSON and prior274-string year-span inventory are durable. Next work: verify year-span semantics with source records, remaining year/issue slashes, unsupported local work marks/location labels and final exclusion/completion audit. Overall goal is still incomplete.


## Candidate48: complete coverage-year spans

Verified original LC MARC05000 for OL3222788M (Gokstadfunnet): AM101.S2494 $b A33 1979-80 $a DL596.G6. Publication date1981 confirms that the call's1979-80 is not simply the imprint year. Saved source-OL3222788M.json and year-span-source-3222788.html durably. Added complete_year_span plus item-base handling for complete ascending four-digit coverage years and abbreviated closing years within the same century, after known non-law class plus fully numeric single-letter Cutters. Existing numbered enumeration can also end with such chronology. Match the common call base without selecting either year, retain all original evidence, and reject incomplete/reversed/unexplained tails and non-LCC prefixes. Uncaptioned law ranges remain excluded from this new rule.

Initial candidate47 treated a shortened lower closing suffix as a century rollover. Residual inspection exposed potentially different year/issue forms (1985-02,1993-41). Tightened live source before final candidate48: never reconstruct a century. Candidate47 is provisional/superseded, not a stable retention baseline. All215 release tests pass in tests-coverage-years-strict.log, including coverage, enumeration, malformed range and no-rollover boundaries. Current source exactly matches candidate48-manifest.json. Matcher versions LCC66/unified113.

Candidate48 full11,316,630-string scan: cumulative baseline gains28399 strings/43669 uses,18254 changed assignments/21252 uses,zero baseline losses. Candidate46→48:254 gains/272 uses,zero lost matches,zero existing assignment changes. All254 independently equal raw calls after removing only coverage chronology; coverage-audit48.json has zero mismatches. Retention46399 checked,zero missing/lost,24 older audited refinements. Provisional47→48 removes exactly8 proposed gains, all preserving the stable46 unresolved state; coverage-strictness-impact48.json lists them. All candidate47/48 scans, tests and audits are terminal successes.

Read-only source DB query located editions for the10 potential rollover/year-issue strings. OL2329047M identifies QC1.M23 1985-02 and QB981.M23 1985-02 as KFKI series1985-02, published1985. This supports year/serial-number semantics rather than expanding to1985-2002. Its linked original MARC fetch failed with connection reset; no immediate repeated fetch or guessed century added. OL1153600M lookup also reset. Source edition JSON, year-or-issue-source-editions48.json and year-issue-source-review48.json are durable. Next step: verify the original MARC then support documented year/serial-number syntax separately (without assigning a year endpoint), alongside remaining slash notation and source-reviewed local work marks. The overall exclusion and completion audit remains open; goal is not complete.


## Candidate49: source-confirmed year/serial identifiers

Original LC MARC for OL2329047M was retrieved directly using the linked archive byte range and validated against its MARC leader, length and terminator. Fields050,490 and830 explicitly identify QC1.M23 1985-02 as KFKI series1985-02. Saved raw MARC and provenance/SHA256 in year-issue-primary-proof49.json and year-issue-source-2329047.mrc. This supersedes the earlier pending-source note.

Renamed the helper to complete_item_year_suffix. A complete four-digit year followed by a two-digit identifier now keeps the common call base without reconstructing any century or year endpoint. Existing complete ascending four-digit coverage years remain supported. Unknown tails, incomplete suffixes, other classification systems and uncaptioned law ranges remain excluded. Original evidence is preserved. Matcher versions67/114;215 release tests pass. Live lcc.rs/lib.rs/runtime.rs hashes match candidate49 snapshot.

Full scan11,316,630 strings completed: cumulative baseline gains28407 strings/43677 uses;18254 changed assignments/21252 uses;zero baseline losses. Versus48:8 gains/8 uses,zero existing assignment changes or losses. Independent year-serial-audit49.json confirms all8 equal their underlying call assignments,zero mismatches. Retention46653 checked,zero missing/lost; the same24 older audited refinements remain (the retention JSON adds the48 comparison key, with identical final assignments). All verification and audit processes completed successfully.

Further source review confirmed slash-form series notation in original LC050 for OL1648155M: AS613.S8 A13 1990/1, with matching490/830 series numbering. Saved source edition JSON and slash-primary-proof50.json/.mrc durably. This is evidence for the next repair, not an implemented slash repair. Remaining format/exclusion review and final completion audit remain open; overall goal remains in progress.


## Candidate50: slash-form series chronology

Extended complete_item_year_suffix for complete four-digit year plus slash and1–5 numeric issue digits, within the existing fully consumed known non-law numeric-Cutter guard. Original LC050/490/830 for OL1648155M confirms AS613.S8 A13 1990/1. Preserve the common call and original evidence; reject incomplete, alphabetic, repeated-slash and unexplained trailing material. No taxonomy rows changed. Versions68/115.

Initial215-test release suite had214 passes and one unrelated decorative-arts assertion: current runtime test expected the renamed ampersand label while the isolated test taxonomy still held its old name. Refreshed only the isolated copy with a read-only SQLite backup of current live taxonomy, then215 release tests passed. No live taxonomy overwrite. Current lcc.rs/lib.rs/runtime.rs hashes match candidate50-manifest.json.

Full11,316,630-string scan: cumulative baseline gains28603 strings/43893 uses,18254 changed assignments/21252 uses,zero baseline losses. Against49:196 gains/216 uses,zero lost matches or existing assignment changes. Independent slash-series-audit50.json confirms all196 equal the underlying call after removing only slash chronology,zero mismatches. Prior retention46661 checked,zero missing/lost; the same24 previously audited refinements remain, all equal candidate49 final assignments. All scans/tests/audits terminal success.

Read-only edition lookup plus original MARC confirms AS284.A1 S37 1978/1979:5 (OL4232369M, LC050) and AS182.H4 1979/80 abh.5 (OL4156375M, LC050/490/830). Both remain unmatched in candidate50 although their bases resolve to4673. Saved raw MARC and provenance in combined-series-primary-proof51 and coverage-enumeration-primary-proof51 files, edition JSON, and combined-series-next-review51.json. These are confirmed next implementation candidates. OL18215084M source fetch reset; RAND report syntax is still unverified.

Residual-queue-inventory50.json is a syntactic work queue over candidate49 remaining rows, not an invalidity audit:87713 processing/minimal-cataloging strings,19258 other nonnumeric-prefix strings,24912 numeric-prefix join/range punctuation,31904 other numeric-prefix strings,1818 labeled strings,366 year/slash strings after excluding processing labels. These buckets are not certified exclusions. Overall format/exclusion review and final completion audit remain open.


## Candidate51: combined series chronology and numbered captions

Extended slash chronology to complete coverage-year plus colon/issue (e.g. AS284.A1 S37 1978/1979:5), with ascending complete four-digit closing years and full numeric consumption. Added recognition of chronology before an independently validated numbered caption (e.g. AS182.H4 1979/80 abh.5). Both shapes have original LC MARC proof recorded in the preceding section. Reuse the existing complete known non-law numeric-Cutter guard and preserve original evidence. Reject unknown trailing words, missing issues/caption numbers, reversed complete coverage years, repeated separators and other classification systems. Check the year token before constructing the temporary enumeration-validation string. No taxonomy rows changed.

215 release tests pass in tests-combined-series-final.log. Current lcc.rs/lib.rs/runtime.rs hashes match candidate51-manifest.json; versions69/116. Full11,316,630-string scan: cumulative baseline gains28647 strings/43942 uses,18254 changed assignments/21252 uses,zero baseline losses. Against50:44 gains/49 uses,zero existing assignment changes or losses. Independent combined-series-audit51.json confirms all44 equal underlying calls after removing only the coverage/issue token,zero mismatches. Retention46857 checked,zero missing/lost; same24 older audited refinements, identical to50 final assignments. All scans/tests/audits terminal success.

Source review identified the next caption variants: original MARC050 confirms Fasz. (OL4683326M), Fas. (OL22424244M), Abh without period (OL4818026M), Bd without period (OL316030M), and physical <fol> (OL3795881M). Saved raw records, provenance hashes, edition JSON and caption-source-review52.json durably. Linked LC050 omits <fol.> for OL889514M and bare fol for OL3524202M; their JSON labels alone are not proof of these exact source forms. These additional variants are not implemented in51. Caption-inventory52.json counts77 Fasz.,4 Fas.,6 Abh-space,49 Bd-space candidates plus fol variants; underlying unrelated problems may keep some unresolved.

Archived task-owned after37..41.tsv to durable gzip, verified decompressed SHA256 before deleting temporary copies; archived-scans37-41.json records hashes and original lengths. Overall remaining supported-format review, residual/exclusion audit and final completion audit stay open. Goal remains in progress.


## Candidate52: caption spellings and folio display variants

Added Fasz., Fas., Abh-space and Bd-space to the existing complete numbered-caption parser, supported by the original MARC proofs recorded in51. Added exact FOL, <FOL> and <FOL.> suffixes under the existing dated known-call folio guard. LC050 directly proves <fol>; fol and <fol.> are observed edition display variants composed from the existing FOL. spelling and confirmed angle-bracket convention. Their linked LC050 records omit those exact suffixes, so no claim of direct primary proof for those two variants is made. Original evidence preserved. Tests cover confirmed forms and rejection of unknown tails, malformed brackets, undated calls and other systems.

215 release tests pass; current lcc.rs/lib.rs/runtime.rs hashes match candidate52-manifest.json; versions70/117. Full11,316,630-string scan: cumulative baseline gains28815 strings/44159 uses,18254 changed assignments/21252 uses,zero baseline losses. Against51:168 gains/217 uses,zero existing assignment changes or losses. Independent caption-folio-audit52.json confirms all168 equal calls using the already-supported caption spellings or with the physical suffix removed,zero mismatches. Retention46901 checked,zero missing/lost; same24 older audited refinements equal51 final assignments. All tests/scans/audits terminal success.

200 distinct strings were inventoried for this batch;33 remain unmatched due to additional attached size labels, unsupported legacy work letters, punctuation, joined codes and undated enumeration. Saved exact remaining rows in caption-residual-review52.json.167 inventory rows gained matches plus one additional case outside the initial inventory. Do not label the33 collectively invalid or excluded. Next work includes attached folio markers/composition with literary work marks and numbered metadata, plus enumeration punctuation (enumeration-punctuation-inventory53.json).

User requested the genuine remaining count. At candidate51 there were165731 unmatched strings:87713 excluded processing/minimal-cataloging marker strings,78018 other strings. Among those,52927 begin with a known LCC numeric subclass,5833 with unknown numeric prefixes,19258 have other formats (residual-prefix-triage52.json). These are review buckets, not confirmed repair counts or proof of invalidity. The exact number of remaining fixable strings is still unverified; do not present52927 or78018 as the repair count. Pattern-level source review is recorded in residual-pattern-groups52.json. Overall goal and final residual/exclusion audit remain open.


## Candidate53: folio labels composed with complete calls

Support angle-bracket folio labels attached directly to a year, folio following existing numbered enumeration, and folio after recognized literary work letters. Non-literary complete numeric-Cutter calls may be undated: folio is a physical descriptor independent of imprint chronology. Literary calls retain the date-or-numbered-enumeration guard against ambiguous undated title marks. The former B74 P4414 FOLIO rejection now intentionally resolves to B74 P4414; PZ7.V266 FOLIO remains rejected by the new branch. Attached dates such as .C431990 are not split or reconstructed; only the exact physical label is removed. Existing strict dated behavior retained. Original evidence preserved; no taxonomy rows changed.

215 release tests pass. Current lcc.rs/lib.rs/runtime.rs hashes match candidate53-manifest.json; versions71/118. Full11,316,630-string scan: cumulative baseline gains28855 strings/44209 uses,18254 changed assignments/21252 uses,zero baseline losses. Against52:40 gains/50 uses,zero existing assignment changes or losses. Independent folio-composition-audit53.json confirms all40 equal their raw underlying calls after removing only folio labels,zero mismatches. Retention47069 checked,zero missing/lost; same24 older audited refinements equal52 final assignments. All scans/tests/audits terminal success. Residuals from the prior33-candidate review saved in caption-residual-review53.json.

Read-only source lookup and primary MARC review confirm AI (OL5370894M 050/490/830, series A volume I), BIV plus supplementary parts (OL491237M 050), numbered commentary (OL439736M LC050), period between volume levels (OL4202742M 050), and parenthesized numeric subpart5a(1) (OL27425628M Columbia050/490/830). Saved raw MARC/provenance, edition JSON, and enumeration-source-review54.json durably. OL4818351M JSON lists a terminal volume period, but inspected LC050 omits it; no direct-source claim for that exact spelling. OL4122774M date-period-before-enumeration JSON reset; no guessed interpretation added. These next enumeration changes are not yet implemented. Overall residual/exclusion and completion audits remain open; exact remaining fixable-string count is still unverified.


## Policy superseded by authorized best-effort matching

The user explicitly relaxed the earlier exclusion of entire damaged fields and authorized recovering complete components, then defensible broad fallbacks, while retaining unresolved fragments and evidence methods. The new active plan and verified candidate56 results are in20260907-best-effort-lcc-matching.md. Candidate54 was superseded due to four Bd-space regressions; these are fixed and retention verified in56. Earlier exclusions still inform fragment uncertainty but no longer require discarding complete neighboring classifications. Do not apply the earlier all-or-nothing policy to the new goal.
