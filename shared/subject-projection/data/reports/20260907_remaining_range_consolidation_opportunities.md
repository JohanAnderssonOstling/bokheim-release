# Remaining selector consolidation opportunities — 2026-09-07

Read-only review of a snapshot of curated taxonomy format 8, release 16, after the first four Law ranges. No changes were applied to the live taxonomy.

## Candidates with no existing-match changes in the audit

| Subject | Current entries | Proposed entries | Exact selectors replaced | New ranges | Net reduction |
|---|---:|---:|---:|---:|---:|
| Law | 2436 | 2314 | 125 | 3 | 122 |
| Latin American Indigenous Law | 312 | 299 | 16 | 3 | 13 |
| Comparative & Uniform Law | 96 | 91 | 7 | 2 | 5 |
| Courts, Organization & Procedure | 89 | 88 | 2 | 1 | 1 |

| Subject | Range | Reference caption | Exact selectors replaced | Newly matched audit inputs |
|---|---|---|---:|---:|
| Law | KNS17.8–KNS24.26 | Law reports and related materials | 46 | 0 |
| Law | KNX2049.2–KNX2677 | Constitutional law | 41 | 0 |
| Law | KKT9850–KKT9860 | Cities | 38 | 0 |
| Latin American Indigenous Law | KIM1–KIM30 | General | 8 | 14 |
| Latin American Indigenous Law | KIM40–KIM47 | Indigenous jurisprudence | 5 | 0 |
| Comparative & Uniform Law | K3620–K3624 | Animal protection. Animal welfare. Animal rights | 4 | 4 |
| Comparative & Uniform Law | K526–K526.5 | Statutes and administrative regulations | 3 | 0 |
| Latin American Indigenous Law | KIM35–KIM37 | History | 3 | 0 |
| Courts, Organization & Procedure | KDC842–KDC869 | Court organization and procedure | 2 | 0 |

Combined audit: 99,245 inputs, no changes to existing matched or ambiguous results, 18 previously unmatched inputs now matched. Total net reduction: 141 entries.

## Larger opportunity requiring a coverage decision

Art Subjects: 165 exact NX650 selectors could become NX650.A–NX650.Z, and eight NX653 selectors could become NX653.A–NX653.Z. This reduces 176 entries to 5, but redirects 67 audited inputs from Arts in general (980) to Art Subjects (986). These are documented reference spans, but applying them changes existing coverage. Each range was tested separately.

## Other subjects reviewed

- Numismatics: seven tested broad and narrow spans did not make any selected exact selectors safely removable under current matcher precedence. More targeted interval design may still offer savings.
- Author Biographies & Criticism: the automatically found Modern French literature span is semantically too broad; author-specific Cutter groupings need separate review.
- Recreation: 168 of 198 entries are BISAC; Modern English Bibles: all 96 entries are BISAC. The current schema provides ranges for LCC only.
- Societies: the candidate HS714–HS7718 looks suspicious and was excluded pending reference verification.
- Social Welfare & Equality Law, Civil Litigation, Other Specialized Law, and IP & Competition Law: tested ranges redirected existing classifications; excluded from the unchanged-match shortlist.
- European Government, European Comparative Law, Local & Municipal Government, Law of nations, and Constitutional Foundations & Rights: tested candidates offered no safe net savings with the conservative removal rule.
- Islamic law: no candidate emerged from this reference-span search.

## Method and limits

Candidate endpoints and captions came from the local LCC classification reference, excluding table-relative spans. The search covered the original 20 high-selector subjects, including remaining Law opportunities. It ranked reference spans containing at least two same-owner exact selectors, then individually audited a shortlist (plus separately reviewed Art spans with more specific subjects inside them). Nested candidates were not double-counted in the proposed totals.

The release matcher and its conservative redundant-selector check determined which exact selectors could be removed. Existing ranges were retained. Corpus: all curated exact selectors, current range boundaries and printed ranges, reference codes and spans in affected subclasses, and synthetic Cutter continuations. Main audit: 99,245 inputs; supplementary audit: 59,278. These are sampled validation results, not proof for every possible classification. Unmatched-to-matched changes are explicitly recorded. This is a shortlist, not an exhaustive optimum over all possible new intervals.

The JSON report includes every audited candidate, removable selectors, and examples of changed results.
