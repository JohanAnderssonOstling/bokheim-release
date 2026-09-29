# Music follow-up review

The requested merge is applied: Musical research (796) is merged into 782,
renamed Music Research & Reference. Its ML3797–ML3799.5 range and three children
are retained. The research route is now Music > Music Research & Reference >
Ethnomusicology. The usage export transfers 68 direct hits to 782 and updates
affected breadcrumbs; the viewer is regenerated.

Structural validation checks every selector row before/after (allowing only
796 -> 782), foreign keys, SQLite integrity, cycles, and sibling label collisions.
The release matcher comparison is recorded in 20260920-music-research-merge.json.
No additional changes below were applied.

## Remaining priorities

1. **Repair Music study abroad (806).** It still owns M4–M1480, M1490,
   and M2.5, and parents general Vocal music (854) and American Music Before
   1860 (8945). Vocal music also has an appropriate route through Creation &
   Performance. Review both selector ownership and the misleading parent edge;
   simply merging names would not repair this.
2. **Merge the two Ethnomusicology concepts after confirming scope.** 731 is
   directly under Music with BISAC MUS015000; 8448 is under Music Research &
   Reference with LCC ML3797.6-3799 and three specialist children. One ID should
   carry equivalent coverage and children, with chosen navigation parents.
3. **Review further source-system duplicates.** Composition (764, BISAC
   MUS007000) under Instruction & Study versus Composition (7778, LCC MT40)
   under Music theory > Music Composition is a strong candidate. Guitar (811,
   BISAC MUS023060) versus Guitar (8643, MT580–MT599) under Instrumental
   techniques > Plucked instruments is another; verify whether one means the
   instrument generally and the other specifically performance instruction.
4. **Disambiguate children's music and children's instruction.** Both 9392
   (M1375–M1420) and 774 (MT740–MT810) are named Kids’ Instrumental Music. The
   local LCC outline distinguishes Instrumental music for children from
   Instrumental techniques for children. Restore that distinction in labels,
   rather than automatically merging these subjects.
5. **Review the two history labels.** Music History (758, MT3–MT5) and Music
   Historical Studies (787, ML159–ML3785) are now siblings. Investigate source
   context: the MT and ML scopes should not be treated as equivalent merely
   because the current labels both suggest general music history.

The remaining branch has 276 distinct subjects. Sixteen case-insensitive
labels occur on multiple IDs; this is a candidate count, not sixteen proven
duplicates. Printed music versus instrument instruction, historical studies
versus analysis, and bibliography versus manuscripts can justify distinct
subjects. Long routes remain especially under Music study abroad and
Instrumental techniques. Prefer fixing semantic placement and duplicate
identities before indiscriminately flattening those branches.
