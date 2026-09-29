# American literary criticism topic extraction installed

Applied nine exact LCC selectors on 2026-09-07: seven new subjects and two existing subjects reused. **2,800 classification uses across 2,143 distinct codes move** from American Literary Criticism, reducing its direct hits from **8,345 to 5,545**. Counts are classification uses, not unique books.

Asian American & Pacific Islander and Hispanic & Latino reuse existing American criticism subjects. Mexican American Literature Studies is beneath Hispanic & Latino. New American mystery, science fiction, Indigenous and Jewish subjects retain routes through both American criticism and the corresponding broader literary criticism subjects. The residual PS153 and PS374 selectors remain on American Literary Criticism.

The local PS153 captions classify groups of authors; Minority Authors in American Literature makes this scope explicit. PS374.W6 is within fiction forms and topics and is labeled Women in American Fiction.

| Subject | Selector | Hits moved | Total direct hits | New subject |
|---|---|---:|---:|---|
| American Mystery Fiction Studies | `PS374.D4` | 389 | 389 | Yes |
| American Science Fiction Studies | `PS374.S35` | 376 | 376 | Yes |
| Indigenous American Literature Studies | `PS153.I52` | 365 | 365 | Yes |
| Mexican American Literature Studies | `PS153.M4` | 324 | 324 | Yes |
| Asian American & Pacific Islander | `PS153.A84` | 303 | 303 | Reused |
| Women in American Fiction | `PS374.W6` | 301 | 301 | Yes |
| Minority Authors in American Literature | `PS153.M56` | 278 | 278 | Yes |
| Hispanic & Latino | `PS153.H56` | 236 | 236 | Reused |
| Jewish American Literature Studies | `PS153.J4` | 228 | 228 | Yes |

Validation: 36 exact-selector, descendant-Cutter and neighboring-code probes; full before/after matching with no matched coverage lost or gained and no unrelated destination changes; migration replay; SQLite integrity and foreign keys; graph acyclicity and parent placement. Published usage counts, matching cache and viewer are refreshed. All nine displayed direct-hit counts, selector totals and parent routes are verified.
The installed database equals the audited staging database.

Caption and hierarchy evidence from the local LCC reference is retained in the [manifest](20260907-american-criticism-topics-manifest.json). Migration: 20260907_extract_american_criticism_topics.sql. Working files: `/home/johan/.cache/bokheim/american-criticism-topics-install/`.
