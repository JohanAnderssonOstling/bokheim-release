# English Literature, 1961–2000: author-subject investigation

Investigation only; no English author subjects installed. Baseline after the approved 50 American author subjects: 118,577 direct classification uses, with one broad selector, PR6050..PR6076.

Observed first-author-Cutter groups account for 112,837 uses (95.2%), across 10,997 distinct groups. Another 5,740 uses lack a parsed author Cutter. The local named-author reference mapping identifies only 175 of these groups, representing 11,467 uses (9.7% of the subject). This is local mapping coverage, not evidence that the remaining authors cannot be identified.

## Concentration

These figures rank observed author-code groups, before comprehensive name verification and production-matcher extraction; they are estimates, not a ready-to-install named-author manifest. One identity can use multiple codes, and a shelf can include criticism, collaborations, or continuations.

| Largest code groups | Uses | Share of parent | Smallest group |
|---:|---:|---:|---:|
| 10 | 4,589 | 3.9% | 386 |
| 20 | 7,900 | 6.7% | 288 |
| 50 | 14,951 | 12.6% | 191 |
| 100 | 23,024 | 19.4% | 139 |
| 200 | 34,490 | 29.1% | 96 |

A threshold of 250 observed uses selects 29 candidate code groups and 10,322 uses (8.7%). A threshold of 200 selects 46 groups and 14,175 uses (12.0%). A threshold should guide review, not trigger automatic import.

## Verified ten-author pilot

The ten largest groups were named using the local reference plus primary catalog records below and tested as exact author selectors in a database copy. The production matcher assigns 4,587 uses to these subjects, leaving 113,990 direct uses on the era subject. Forty author-root, work/criticism, and neighboring-Cutter checks pass. This pilot has not undergone the full per-code coverage comparison required before installation.

| Author heading | Pilot direct hits | Selector | Name evidence |
|---|---:|---|---|
| Rowling, J. K. | 530 | `PR6068.O93` | [Catalog](https://ci.nii.ac.jp/ncid/BA78908628) |
| Chesney, Marion | 526 | `PR6053.H4535` | Local LCC record 197298 |
| Rendell, Ruth, 1930- | 522 | `PR6068.E63` | Local LCC record 197375 |
| McCall Smith, Alexander | 483 | `PR6063.C326` | [Catalog](https://ci.nii.ac.jp/ncid/BA74743399) |
| Perry, Anne | 480 | `PR6066.E693` | [Catalog](https://ci.nii.ac.jp/ncid/BA68800330) |
| Cornwell, Bernard | 452 | `PR6053.O75` | Local LCC record 197305 |
| Rushdie, Salman | 416 | `PR6068.U757` | [Catalog](https://ci.nii.ac.jp/ncid/BA6363771X) |
| Pinter, Harold | 404 | `PR6066.I53` | [Catalog](https://ci.nii.ac.jp/ncid/BA50983241) |
| Pratchett, Terry | 388 | `PR6066.R34` | [Catalog](https://ci.nii.ac.jp/ncid/BA79602088) |
| Le Carré, John, 1931- | 386 | `PR6062.E33` | Local LCC record 197355 |

## Recommended approach

Introduce reviewed, useful author browsing subjects across literature, prioritizing hit concentration and reliable identity/selector mappings. Start this branch with the candidates above 250 uses (29 groups), identify them all, deduplicate identities and pseudonyms, and run a complete extraction preview before applying them. Do not automatically import the thousands of author filing records. The 50 largest groups remove only 12.6% of this parent; the long tail remains useful under the existing period subject.

Keep one subject per author identity with all verified relevant selectors, and preserve the broad period selectors. Where a creator index exists, link the browsing subject to it; selectors alone do not implement that identity link. Present shelves as works by and about their authors, without treating all matched books as authored by the named person.

The existing policy currently permits only the approved 50 American subjects. Any English rollout should document its approved scope as a further exception. The present task investigates that rollout and makes no taxonomy or policy changes.

Counts are July 2026 Open Library classification uses, not unique books or readership/popularity. Working inputs and pilot outputs are in `/home/johan/.cache/bokheim/english-author-investigation/`.
