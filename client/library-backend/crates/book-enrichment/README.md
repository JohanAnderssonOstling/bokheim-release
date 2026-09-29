# Book enrichment

Portable, resumable policy shared by Bokheim clients and authority ingestion. The crate owns subject stage order, evidence types, printed/resource ISBN verification and classification consensus. It depends on no file parser, OCR engine, database or network runtime.

`SubjectPipeline` requests these stages, in order:

1. Resolve metadata LCC/BISAC.
2. Look up metadata ISBNs.
3. Inspect pages if there is still no resolved LCC, reusing cached evidence.
4. Resolve page LCC/BISAC, then look up newly discovered ISBNs.
5. Search title/authors and look up any newly supplied ISBNs.

Only resolved LCC completes subject enrichment; BISAC is retained as partial evidence. Author enrichment runs independently even when local LCC already completes subjects. Successful ISBN requests are deduplicated across equivalent ISBN-10/13 values. Hosts call `finish_step` only after definitive responses, never after timeouts or omitted results, and persist the serialized state.

`LookupPlan` and `defer_initial_page_inspection` support cheap initial import: metadata ISBNs defer inspection pending lookup, rather than permanently skipping the fallback.

Format parsing owns metadata extraction. `book-model::metadata_isbn` validates whole values regardless of the declared scheme, permitting ISBN whitespace/hyphens and explicit ISBN wrappers. EPUB parsing checks OPF property text/attributes and the bounded NCX head. UUID/date/URL/filename substrings are excluded. This crate re-exports the validator for compatibility.

The client adapter is `client/app/src/app/subject_pipeline.rs`. It batches network stages and short background database writes, runs page work through the native blocking/browser range-source worker, and caches ISBN responses for reuse by author enrichment. Progress and response caches are device-local SQLite data, not changes to source books. Native OCR remains bounded Tesseract; WASM has no OCR.

An ebook ASIN-to-ISBN network provider is not implemented here. The existing audiobook adapter remains ahead of general enrichment; Kindle ASINs must not be sent indiscriminately to Audible.

Descriptions use an independent client stage (`client/app/src/app/description_enrichment.rs`) across all formats, including books whose subjects are already complete. It reuses the ISBN response cache, fills only empty database descriptions, and applies no description when available records conflict. It neither reads nor rewrites source files.

## Bibliographic normalization

Catalogue discovery and subject verification share `metadata_contract::matching::NormalizedAuthor`
and `NormalizedTitle`. Author representations retain suffix evidence, normalize suffix spellings,
and expose broad candidate tokens separately from name compatibility. Verification requires at least one compatible author. `matching_author_count` ranks candidates
by distinct matching credits; extra narrators or missing coauthors do not veto a match, and one
catalogue credit cannot contribute multiple votes.

Title representations expose the cleaned full title, main title, subtitle and structural qualifiers.
Parenthetical production labels such as `(Unabridged)` do not block subject discovery. Volume and
adaptation evidence remains significant. Edition discovery, LC/Wikidata authority fallbacks and
client verification use these representations rather than independent cleanup routines.

Display values are unchanged. Persisted catalogue keys and author identity keys are unchanged:
lookup emits both apostrophe-normalized and legacy punctuation keys, including the original title.
No catalogue rebuild or author-identity migration is required. Strict stored identity and UI substring
search remain distinct policies; neither should be used to verify bibliographic compatibility.

The edition-attempt provider and subject-pipeline revision are advanced when these lookup rules
change, allowing previously exhausted books to be reconsidered through the ordinary pipeline.

Verified candidate editions may supply different classification codes. Subject recovery keeps the
union of their valid codes, with per-code provenance, without selecting an exact edition ISBN.
Missing or invalid classifications in one candidate do not discard usable codes from another.
Title, author, structural-qualifier and response-truncation checks still apply.

For records with no author credits, an `Author - Title` prefix can supply verification evidence
after a catalogue candidate corroborates both the author name and the remaining title. This
fallback does not overwrite known authors or assign an exact edition ISBN. It rejects truncated
responses, mismatched titles, and conflicting volume/adaptation qualifiers.

Filename lookup uses the same `normalize_with_filename` import parser on a temporary metadata
record. It tries parsed title/author evidence and parsed titles with stored author credits,
independently of display-field selection. Distinct queries are bounded and deduplicated.
The backend prefers supported stored metadata, then verified filename alternatives; those
alternatives may supply subjects but do not replace display metadata or attach an edition ISBN.
