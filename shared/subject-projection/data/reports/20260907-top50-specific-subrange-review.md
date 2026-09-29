# Top 50 specific-subrange review

Reviewed the 50 high-hit broad-range candidates against the installed taxonomy
and local LCC reference. The governing rule was: move only named, specific
subjects down; retain introductory, reference, residual, and otherwise general
material at the current subject. Structural `lcc_range` rows were preferred;
no generated exact-code lists were added.

## Partitioned or reduced

Computer Networks; Politics & Government; Pediatrics; Arts & Media; Religion;
Geography; Education; Turkic Languages; School Administration; Agriculture;
Old & New Testaments; Building Construction; Tumors & Oncology; Biology; Child
Psychology; Pediatric Diseases; Physics; Artificial Intelligence; Arts in
General; Architecture Theory & Practice; Drugs & Their Actions; Political
Theory & the State; Geography, Places & Travel; Mechanical; Indians of North
America; Education Theory & Practice; General Corporate Finance; Arts & Crafts;
Microbiology; Industry & Organization; Money & Monetary Policy; Foreign
Exchange; Public & Social Customs; and Character & Virtue Ethics.

The only new concepts are four official Computer Networks divisions: Computer
Network Architecture, Computer Network Hardware, Network Protocols & Standards,
and Wide Area Networks. The existing Local Area Networks concept was reused.

## Reviewed and deliberately retained

- Kids' Fairy Tales, Folklore & Fables: its PZ8/PZ8.1/PZ8.2 values do not form
  safe subject ranges for Adaptations, Anthologies, or Country & Ethnic.
- Psychoanalysis: BF173-BF175.5 and RC500-RC510 are the subject itself; its only
  narrower development-theory child is an exact topic.
- Philosophy: the remaining B/BD coverage is general philosophy or residual
  coverage around already installed specific children.
- Special Elements & Subjects: PN46-PN57 topics are Cutter-defined and do not
  form safe contiguous numeric subject ranges.
- Christian Denominations and Catholic: existing denominational ranges already
  move the safe divisions down; the residual coverage is general or contains
  interleaved denominations.
- Other National Comics: Manga and South Korean comics already own their safe
  national Cutter ranges; remaining national comics stay at the parent.
- Probability & Statistics: its broad schedule was already partitioned; current
  parent ranges are the statistical residuals between specific children.
- Humor & Satire: the LCC block is organized mainly by collection language and
  country, not the taxonomy's Form and Topic children.
- Manga and Comics & Graphic Novels: their LCC ranges are author/title shelving
  ranges and do not encode the genre children.
- Cities & Towns: Bristol, Liverpool, Manchester, Oxford, York, Birmingham, and
  Bath already own safe Cutter subranges; other English towns remain here.
- Youth & Adolescence: the U.S. material is Cutter/table based rather than a
  safe contiguous numeric subject subrange.
- Technology & Engineering: current direct T-schedule coverage is general or
  residual around already ranged children.
- Religion, Society & Culture: its children are interleaved BL65 Cutter topics,
  not safe numeric subranges.
- Commerce: specific HF divisions already have child ranges; the remaining HF
  root coverage is general commerce.

## Validation

SQLite `integrity_check` is `ok`; `foreign_key_check` is empty. The focused
optimized-release structural range ownership test passed after the first batch.
The later full release suite could not start because unrelated concurrent root
workspace edits removed `workspace.package.edition`, preventing Cargo from
parsing the vendored `gpui-component` manifest. No viewer or metadata-server
refresh was performed.
