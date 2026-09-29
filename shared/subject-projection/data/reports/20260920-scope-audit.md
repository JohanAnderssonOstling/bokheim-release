# Subject scope audit: medicine, psychology and philosophy

Report only, 20 September 2026. **Five confirmed label/scope mismatches and one overbroad fallback range. No taxonomy changes.**

Screened 989 stored LCC range rows in R and its subclasses, B, BC, BD, BF, BH and BJ. Compared suspicious labels with the locally stored 2025 LCC schedules and verified 30 classification probes using the existing optimized release matcher. This is a first-pass range-label audit with targeted verification, not a claim that every code or subject in these disciplines is correct. The inventory includes overlapping ranges and multiple ranges per subject; 989 is not a subject count.

## Highest priority: confirmed wrong subject destinations

| Subject | Owned range | Evidence from actual matcher results | Recommended correction |
|---|---|---|---|
| **Good & Evil** (4504) | BJ1296–BJ1517.9 | BJ1340 (existential ethics), BJ1451 (duty/obligation general works) and BJ1481 (happiness/joy general works) all resolve to Good & Evil. | Retain Good & Evil and its BISAC PHI008000 mapping, but restrict its LCC coverage to the corresponding topic. Give the broad fallback to Ethics (4494), preserving specific ethical-school and topic assignments. |
| **Psycholinguistics & Synesthesia** (12878) | BF455–BF499 | BF468 (time), BF482 (mental fatigue) and BF491 (normal illusions) all resolve here. These are distinct cognitive topics between psycholinguistics and synesthesia in LCC. | Separate psycholinguistics and synesthesia; attach intervening topics directly to Cognition or appropriate existing subjects. Correct both the broad range and explicit selectors such as BF468 and BF482. |
| **Habit, Adjustment & Nature–Nurture** (12875) | BF335–BF364.9 | BF355 (posture) and BF357 (imitation/mimicry) resolve here. Environmental Psychology (53300, BF353) resolves correctly but is also nested under this subject. | Restrict habit/adjustment and nature–nurture coverage to their own sections; place posture, imitation and Environmental Psychology under appropriate cognitive/psychological parents. Remove their inappropriate explicit selectors as well as correcting the range. |

The first case crosses particularly clear conceptual boundaries. The source assigns existential ethics its own heading at BJ1340; Good and Evil begins later. This is not merely a long path or an awkward synonym.

For Psycholinguistics & Synesthesia, a broader name alone would conceal an arbitrary grouping. Two specific subjects plus direct cognitive topics would be clearer without introducing another intermediate wrapper.

## Medicine: labels narrower than their classification coverage

| Subject | Owned range | Evidence from actual matcher results | Recommended correction |
|---|---|---|---|
| **Acute & Specialty Care** (2768) | RT89–RT120, also RT89–RT89.3 | RT89 (nursing administration), RT97 (public-health nursing) and RT120.F34 (family nursing) resolve here. LCC calls the larger section specialties in nursing; it includes teaching, community care and services. | Move the broad fallback to Nursing (2747), or use a genuinely broad nursing-specialties subject if it serves navigation. Route administration and community/public-health nursing to suitable existing subjects. Keep an acute-care label only for acute care. |
| **Mental Illness Prevention** (6195) | RA790–RA790.95 | RA790.55 (community psychology) and RA790.75 (mental health as a profession) resolve here. The schedule covers mental health as well as prevention. | Rename to **Public Mental Health**, preserving its Public Health parent, or split out community psychology and professional topics if existing subjects fit. A broader label is likely sufficient for the general fallback. |

These are taxonomy scope findings, not judgments about medical practice. Mental Illness Prevention is the milder mismatch: prevention is related, but it does not describe the whole assigned section.

## Overbroad range with specific mappings currently protecting examples

**Creative Thinking (12877)** owns BF408–BF454.9, although the creative-process section is BF408–BF426. The range runs across intelligence and general thinking.

However, tested BF431, BF431.3 and BF433.G45 resolve to Intelligence; BF441 resolves to Thinking; BF444 and BF449.5 resolve to Reasoning & Judgment. Therefore, this audit does **not** count those books as currently misclassified under Creative Thinking.

An artificial BF450 probe reaches Creative Thinking, demonstrating the fallback behavior, but no corresponding active schedule entry was found in the inspected reference. It is a diagnostic probe, not evidence of an assigned LCC topic or affected book.

Recommended correction: limit Creative Thinking to its actual scope and use Cognition as the fallback outside that scope, preserving the existing intelligence and thinking mappings. This prevents unrecognized or partial codes from inheriting an unjustifiably narrow label.

## Suspicious cases that should not be changed on this evidence alone

- **Depression & Mood Disorders, RC537–RC545:** the schedule includes alexithymia, anhedonia and seasonal affective disorder within this section. The broader current label is defensible; RC540 resolves there as expected.
- **Health Psychology, R726.5–R726.8:** terminal care at R726.8 already resolves to End-of-Life Care (6861). Its placement reflects the source section. A separate parent review may help, but this is not demonstrated blanket assignment to Health Psychology.
- **Drug Administration, RM147–RM180:** blood banks at RM172 already resolve to Infusion/Transfusion (10063). The source section includes administration of other therapeutic agents. Consider wording later; no narrow terminal misclassification demonstrated here.
- **Research & Assessment, BF39–BF80:** its broad range deserves hierarchy review, but practice at BF75 and ethics at BF76.4 reach their specific subjects. Do not infer erroneous final destinations solely from overlapping range ownership.
- **Earlier repairs still hold:** RC552.A44 resolves to Anxieties & Phobias (5436), BH151 to Modern Aesthetics (7622), and DDC190 to Modern Philosophy (4510).

## Why this pattern occurs

The inspected data shows two distinct mechanisms. Some ranges bridge several adjacent LCC headings but retain one narrow display label. Other concepts explicitly collect unrelated codes under a convenient label. For example, BF482 is both inside the psycholinguistics/synesthesia range and explicitly assigned to that subject; shortening its range alone would not fix the result.

LCC supplies ordered schedule sections, but adjacency does not make every intervening subject a subtype of the first named topic. More specific mappings sometimes mask an overbroad range, which is why runtime checks matter.

Recommended implementation order: Good & Evil; the two mixed psychology buckets; nursing; public mental health; then Creative Thinking fallback cleanup. Reuse existing broad subjects before creating wrappers. Any implementation should compare observed classifications and preserve coverage; this report does not estimate affected-book totals.

## Evidence and limits

- [Machine-readable evidence](20260920-scope-audit-evidence.json): subject IDs, ranges, selectors, source entry IDs, source document/page/line references, runtime results and database fingerprint.
- [All 30 runtime probes](20260920-scope-audit-probes.csv).
- [989-row range inventory](20260920-scope-audit-range-inventory.csv).

Source evidence comes from `lcc-mdsconnect-2016.sqlite3`, whose reference documents include `LCC_B-BJ2025TEXT.pdf` and `LCC_R2025TEXT.pdf`. The database filename does not describe the date of every imported reference. Captions and individual codes were checked; automatically extracted hierarchy arrays were not treated as authoritative on their own.

The authoritative taxonomy contains 16,910 concepts. Its SHA-256 remained `8a21331c74ec5e5aefe995a3f301aae7e7e6b4e9ce9dad556bb09391e683a93f` across report generation. No migration, viewer regeneration or usage-data change was made. No full observed-code comparison or new build was needed for this report-only audit.
