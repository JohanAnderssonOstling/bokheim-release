use super::compact_whitespace;
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

#[derive(Default)]
struct LccOutline {
    ranges_by_main: HashMap<char, Vec<LccRange>>,
}

struct LccRange {
    start_letters: String,
    start_number: f64,
    end_letters: String,
    end_number: f64,
    path: String,
}

pub(super) struct LccCall<'a> {
    pub(super) letters: String,
    pub(super) number: f64,
    display: &'a str,
    pub(super) remainder: &'a str,
}

fn outline() -> &'static LccOutline {
    static OUTLINE: OnceLock<LccOutline> = OnceLock::new();
    OUTLINE.get_or_init(|| {
        let mut outline = LccOutline::default();
        let mut reader = csv::Reader::from_reader(include_str!("../data/lcc-2024-outline.csv").as_bytes());
        for record in reader.records() {
            let record = record.expect("the embedded LCC outline CSV must be valid");
            let start_letters = record.get(0).unwrap_or_default().to_owned();
            let range = LccRange {
                start_number: record.get(1).unwrap_or_default().parse().expect("numeric LCC range start"),
                end_letters: record.get(2).unwrap_or_default().to_owned(),
                end_number: record.get(3).unwrap_or_default().parse().expect("numeric LCC range end"),
                path: record.get(4).unwrap_or_default().to_owned(),
                start_letters: start_letters.clone(),
            };
            outline.ranges_by_main.entry(start_letters.chars().next().expect("LCC class letter")).or_default().push(range);
        }
        outline
    })
}

pub(super) fn outline_subclasses() -> BTreeSet<String> {
    outline().ranges_by_main.values().flatten().flat_map(|r| [r.start_letters.clone(), r.end_letters.clone()]).collect()
}

/// Outline path for a single classification.
pub fn lcc_subject_paths(value: &str) -> Vec<String> {
    lcc_subject_path(value).into_iter().collect()
}

pub fn lcc_subject_path(notation: &str) -> Option<String> {
    if notation.contains(';') {
        return None;
    }
    let normalized = compact_whitespace(notation).to_ascii_uppercase().replace(['–', '—'], "-");
    if let Some(assignment) = minimal_class_assignment(&normalized) {
        let main = outline().ranges_by_main.get(&assignment.chars().next()?)?.first()?.path.split(" / ").next()?;
        return Some(build_subject_path(main, &normalized));
    }
    if is_holdings_or_storage_marker(&normalized) {
        return None;
    }
    if normalized.contains('-') {
        return None;
    }
    let call = parse_lcc_call(notation)?;
    let ranges = outline().ranges_by_main.get(&call.letters.chars().next()?)?;
    let best = ranges.iter().filter(|range| range_contains(range, &call)).max_by(|left, right| compare_matches(left, right, &call))?;
    Some(build_subject_path(&best.path, call.display))
}

/// Normalizes a syntactically valid LCC class, call number, or shelf number
/// containing an explicit LC classification assignment, without
/// requiring that the currently installed outline already contains it.
/// This keeps collection of external identifiers independent from local
/// taxonomy coverage so finer server-side schedules can be requested later.
pub fn canonical_lcc_notation(notation: &str) -> Option<String> {
    let notation = compact_whitespace(notation).to_ascii_uppercase().replace(['–', '—'], "-");
    // The published LC outline prints K(520)-5582 for comparative law.
    // Imports also lose the separator after the parenthesized lower bound.
    // https://www.loc.gov/catdir/cpso/lcco/lcco_k.pdf
    if matches!(notation.as_str(), "K(520) 5582" | "K(520)-5582" | "K(520) - 5582") {
        return Some("K520-5582".to_owned());
    }
    if minimal_class_assignment(&notation).is_some() {
        return Some(notation);
    }
    if is_holdings_or_storage_marker(&notation) {
        return None;
    }
    if letters_only_syntax(&notation)
        || apostrophe_call_base(&notation).is_some()
        || lcc_item_base(&notation).is_some()
        || parse_lcc_call(&notation).is_some_and(|call| valid_lcc_remainder(call.remainder) || valid_literature_work_mark(&call))
    {
        Some(notation)
    } else {
        None
    }
}

// Printed spans stay distinct from individual call numbers and can also be
// queried as complete numeric intervals by the taxonomy matcher.
#[derive(Clone, Debug)]
pub(super) struct LccEndpoint {
    pub letters: String,
    pub number: f64,
    pub remainder: String,
    pub code: String,
}

pub(super) fn lcc_endpoint(value: &str, upper: bool) -> Option<LccEndpoint> {
    let value = value.trim().to_ascii_uppercase();
    if letters_only_syntax(&value) {
        return Some(LccEndpoint { letters: value.clone(), number: if upper { f64::INFINITY } else { 0.0 }, remainder: String::new(), code: value });
    }
    let call = parse_lcc_call(&value)?;
    let mut rest = call.remainder.trim();
    let mut remainder = String::new();
    while !rest.is_empty() {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '.');
        let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        let digits = rest[letters..].bytes().take_while(u8::is_ascii_digit).count();
        if letters == 0 || (digits == 0 && (letters != 1 || letters != rest.len())) {
            return None;
        }
        remainder.push('.');
        remainder.push_str(&rest[..letters + digits]);
        rest = &rest[letters + digits..];
    }
    let code = format!("{}{}{}", call.letters, call.number, remainder);
    Some(LccEndpoint { letters: call.letters, number: call.number, remainder, code })
}

// Imported records sometimes use apostrophes between complete call-number
// components. Keep the original notation, and interpret only unambiguous
// class + Cutter components with an optional final publication year.
pub(super) fn apostrophe_call_base(value: &str) -> Option<String> {
    if !value.contains('\'') {
        return None;
    }
    for (at, _) in value.match_indices('\'') {
        if at == 0 || !value.as_bytes()[at - 1].is_ascii_alphanumeric() || !value.as_bytes().get(at + 1).is_some_and(u8::is_ascii_alphanumeric) {
            return None;
        }
    }
    let base = value.replace('\'', " ");
    // Compose two already supported display forms: apostrophe separators
    // and item enumeration. The enumeration never changes classification.
    let call = parse_lcc_call(lcc_enumeration_base(&base).unwrap_or(&base))?;
    let mut cutters = 0;
    let mut parts = call.remainder.split_whitespace().peekable();
    while let Some(part) = parts.next() {
        let part = part.strip_prefix('.').unwrap_or(part);
        let bytes = part.as_bytes();
        if bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.len() > 1 && bytes[1..].iter().all(u8::is_ascii_digit) {
            cutters += 1;
        } else if cutters > 0 && parts.peek().is_none() && (bytes.len() == 4 || (bytes.len() == 5 && bytes[4].is_ascii_alphabetic())) && bytes[..4].iter().all(u8::is_ascii_digit) {
            break;
        } else {
            return None;
        }
    }
    Some(base)
}

fn is_holdings_or_storage_marker(value: &str) -> bool {
    value.starts_with("CPB") || value.starts_with("MLC") || value.starts_with("LAW") || matches!(value, "LCC" | "COMIC" | "MICROFICHE" | "MICROFILM" | "UNCLASSIFIED" | "ACQUIRED" | "ACQUISITION" | "NONE" | "N/A")
}

// Later LC MLC records also carry explicit subclass assignments, e.g.
// MLCF 2013/40001 (PQ). Validate the completed shelf number with the same
// rules as main-class assignments, and accept only known non-law subclasses.

// DCM B11 and CSB 36: size/custody letters identify shelving, while the
// optional final parenthesized letter carries a broad LC classification.
fn minimal_class_assignment(value: &str) -> Option<&str> {
    // Parentheses already delimit the assignment; imported shelf numbers
    // sometimes omit the space before it. Keep the entire original notation.
    let at = value.rfind('(')?;
    let assignment = &value[at..];
    let class = assignment.strip_prefix('(')?.strip_suffix(')')?.trim();
    if class.len() != 1 || !b"ABCDEFGHJKLMNPQRSTUVZ".contains(&class.as_bytes()[0]) {
        return None;
    }
    let placeholder = |s: &str| (3..=5).contains(&s.len()) && s.bytes().all(|b| b == b'X');
    // LC preliminary records can use MLCxxx (P) before a shelf sequence is
    // assigned. Only the explicit parenthesized class supplies a subject.
    let shelving_prefix = |s: &str| {
        let Some(suffix) = s.strip_prefix("MLC") else {
            return false;
        };
        let suffix = suffix.as_bytes();
        matches!(suffix.len(), 1 | 2) && b"SMLF".contains(&suffix[0]) && (suffix.len() == 1 || b"ACEHJKMNRT".contains(&suffix[1]))
    };
    let shelf = value[..at].trim();
    if let Some(x) = shelf.find('X') {
        let prefix = shelf[..x].trim();
        if (prefix == "MLC" || shelving_prefix(prefix)) && placeholder(&shelf[x..]) {
            return Some(class);
        }
    }
    let mut parts = value[..at].split_whitespace().peekable();
    let prefix = parts.next()?;
    if matches!(prefix, "MICROFILM" | "MICROFICHE" | "MICROOPAQUE") {
        if parts.peek().is_some_and(|part| matches!(*part, "(O)" | "(W)")) {
            parts.next();
        }
    } else if prefix == "MLC" {
        if parts.next()? != "R" {
            return None;
        }
    } else if !shelving_prefix(prefix) {
        return None;
    }
    // Whitespace around the delimiter is harmless; whitespace within either
    // digit component remains invalid. Do not concatenate split digits.
    let number = parts.collect::<Vec<_>>().join(" ");
    if prefix.starts_with("MLC") && placeholder(&number) {
        return Some(class);
    }
    let (year, sequence) = number.split_once('/')?;
    let (year, sequence) = (year.trim(), sequence.trim());
    if !matches!(year.len(), 2 | 4) || !year.bytes().all(|b| b.is_ascii_digit()) || sequence.is_empty() || sequence.len() > 5 || !sequence.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(class)
}

// G610/G615 volume, part and number designations describe an item, not a
// different subject. Only detach a complete numbered suffix from a valid call.
// Complete coverage chronology or a year plus a two-digit serial number.
// LC MARC 050 for OL3222788M records A33 1979-80; OL2329047M records
// .M23 1985-02, also identified as a series number in MARC 490/830.
// Both forms retain the common call base; no century or endpoint is inferred.
fn complete_item_year_suffix(value: &str) -> bool {
    // LC MARC 050/490/830 for OL1648155M records A13 1990/1.
    // Slash chronology identifies a numbered series issue, not a class range.
    if let Some((year, issue)) = value.split_once('/') {
        if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) || !(1000..=2999).contains(&year.parse::<u32>().unwrap()) {
            return false;
        }
        // LC 050 for OL4232369M: S37 1978/1979:5. The coverage
        // chronology and issue number jointly describe the series item.
        let issue = if let Some((end, number)) = issue.split_once(':') {
            if !matches!(end.len(), 2 | 4) || !end.bytes().all(|b| b.is_ascii_digit()) || (end.len() == 4 && !(year.parse::<u32>().unwrap()..=2999).contains(&end.parse::<u32>().unwrap())) {
                return false;
            }
            number
        } else {
            issue
        };
        return !issue.is_empty() && issue.len() <= 5 && issue.bytes().all(|b| b.is_ascii_digit());
    }
    let Some((start, end)) = value.split_once('-') else {
        return false;
    };
    if start.len() != 4 || !start.bytes().all(|b| b.is_ascii_digit()) || !matches!(end.len(), 2 | 4) || !end.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let start = start.parse::<u32>().unwrap();
    if !(1000..=2999).contains(&start) {
        return false;
    }
    if end.len() == 2 {
        return true;
    }
    (start..=2999).contains(&end.parse::<u32>().unwrap())
}

pub(super) fn lcc_enumeration_base(value: &str) -> Option<&str> {
    fn number(rest: &str) -> Option<&str> {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let end = digits + usize::from(rest.as_bytes().get(digits).is_some_and(u8::is_ascii_alphabetic));
        Some(&rest[end..])
    }
    fn numbered_caption_tail(rest: &str) -> Option<&str> {
        // MARC 050 source records confirm Fasz. (OL4683326M), Fas.
        // (OL22424244M), Abh (OL4818026M) and Bd (OL316030M).
        [
            "SER.",
            "SERIES ",
            "VOLUME ",
            "VOLS.",
            "VOL .",
            "VOL ",
            "VOL.",
            "V .",
            "V.",
            "PART ",
            "PTS.",
            "PT ",
            "PT.",
            "NUMBER ",
            "NOS.",
            "NO .",
            "NO ",
            "NO.",
            "N:O",
            "ISSUE ",
            "ISS.",
            "SECT.",
            "SESS.",
            "SUPPL.",
            "SUPPL ",
            "SUPP.",
            "SUPL.",
            "BD.",
            "BD ",
            "BDE.",
            "BDCHN.",
            "NR.",
            "T.",
            "FASC.",
            "FASZ.",
            "FAS.",
            "ABT.",
            "LFG.",
            "LIVR.",
            "BK.",
            "JAHRG.",
            "JAARG.",
            "KN.",
            "KNJ.",
            "CZ.",
            "D.",
            "SV.",
            "ZESZ.",
            "VYP.",
            "HEFT.",
            "HEFT ",
            "H.",
            "HFT.",
            "HBD.",
            "HALBBD.",
            "DEEL",
            "DL.",
            "SZ.",
            "NIDE ",
            "OSA ",
            "ABH.",
            "ABH ",
            "BEIHEFT ",
            "BEIH.",
            "FOLGE ",
            "AR.",
            "ZV.",
            "REIHE ",
            "BOOK ",
            "UNIT ",
            "COPY ",
            "COP.",
            "BAND ",
            "TEIL ",
            "TEILBD.",
            "CH.",
            "SEC.",
            "CAHIER ",
            "RéSZ ",
            "RÉSZ ",
            "RE\u{301}SZ ",
            "FüZET ",
            "FÜZET ",
            "FU\u{308}ZET ",
            "SéR.",
            "SÉR.",
            "SE\u{301}R.",
            "AFL.",
            "PTIES ",
            "PTIE ",
            "ROč.",
            "ROČ.",
            "ROC\u{30c}.",
            "SEš.",
            "SEŠ.",
            "SES\u{30c}.",
            "LVL.",
            "LVL ",
            "LEVEL ",
        ]
        .iter()
        .find_map(|label| rest.strip_prefix(label))
    }
    fn suffix(mut rest: &str) -> bool {
        loop {
            // Numbered volume, series, and part levels carry the item
            // enumeration. Every other tail shape is rejected.
            let Some(tail) = numbered_caption_tail(rest) else {
                return false;
            };
            let Some(tail) = number(tail.trim_start()) else {
                return false;
            };
            rest = tail;
            if rest.is_empty() {
                return true;
            }
            if !rest.starts_with(|c: char| c.is_ascii_whitespace() || matches!(c, ',' | ':' | '=')) {
                return false;
            }
            rest = rest.trim_start();
            if rest.starts_with([',', ':', '=']) {
                rest = rest[1..].trim_start();
            }
            if rest.is_empty() {
                return false;
            }
        }
    }
    for (at, _) in value.match_indices(' ') {
        if !suffix(value[at..].trim_start()) {
            continue;
        }
        let base = value[..at].trim_end().strip_suffix(',').unwrap_or(value[..at].trim_end()).trim_end();
        // A storage phrase must not become a PZ title mark plus issue number.
        if base.ends_with(" BOX") && value[at..].trim_start().starts_with("NO") {
            continue;
        }
        let call = parse_lcc_call(base)?;
        if !base.contains('-') && (valid_lcc_remainder(call.remainder) || valid_literature_work_mark(&call)) {
            return Some(base);
        }
    }
    None
}

// CSB 24 and G622 distinguish custodial assignments from classification.
// Preserve the full notation; only known assignment forms yield a match base.
pub(super) fn lcc_item_base(value: &str) -> Option<&str> {
    let mut base = value;
    // Coverage years and year/serial identifiers follow numeric Cutters.
    // Match their common call base without inventing a year or selecting
    // an endpoint. The original suffix remains in the evidence.
    // https://openlibrary.org/show-records/marc_loc_2016/BooksAll.2016.part15.utf8:18301670:981
    for (at, _) in value.match_indices(' ').rev() {
        let prefix = &value[..at];
        let (years, tail) = value[at + 1..].split_once(' ').unwrap_or((&value[at + 1..], ""));
        if !complete_item_year_suffix(years) {
            continue;
        }
        // LC 050/490/830 for OL4156375M: .H4 1979/80 abh. 5.
        // Validate a following numbered caption independently of chronology.
        let complete_tail = tail.is_empty() || (tail.bytes().any(|b| b.is_ascii_digit()) && lcc_enumeration_base(&format!("{prefix} {tail}")) == Some(prefix));
        if complete_tail
            && !prefix.contains('-')
            && parse_lcc_call(prefix).is_some_and(|call| {
                if call.letters.starts_with('K') {
                    return false;
                }
                let mut rest = call.remainder;
                let mut count = 0;
                while !rest.is_empty() {
                    rest = rest.trim_start();
                    rest = rest.strip_prefix('.').unwrap_or(rest).trim_start();
                    let bytes = rest.as_bytes();
                    if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
                        return false;
                    }
                    let digits = bytes[1..].iter().take_while(|b| b.is_ascii_digit()).count();
                    if digits == 0 {
                        return false;
                    }
                    count += 1;
                    rest = &rest[1 + digits..];
                }
                count > 0 && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
            })
        {
            base = prefix;
            break;
        }
    }
    // Columbia CPM-605 records F, FF and FFF in MARC 852 $m as
    // oversize suffixes on LC calls, also after volume enumeration.
    // Require a complete dated call or explicit enumeration so an
    // unfinished Cutter or literary title letter cannot become a size mark.
    // https://www1.columbia.edu/sec/cu/libraries/inside/clio/docs/bcd/cpm/cpmspe/cpm605.html
    if let Some(prefix) = [" FFF", " FF", " F"].iter().find_map(|suffix| value.strip_suffix(suffix)) {
        let enumerated = lcc_enumeration_base(prefix);
        let candidate = enumerated.unwrap_or(prefix);
        let year = candidate.split_whitespace().last().unwrap_or_default().as_bytes();
        let dated = (year.len() == 4 || (year.len() == 5 && year[4].is_ascii_alphabetic())) && year[..4].iter().all(u8::is_ascii_digit);
        let numbered = enumerated.is_some() && prefix[candidate.len()..].bytes().any(|b| b.is_ascii_digit());
        if (dated || numbered)
            && !candidate.contains('-')
            && parse_lcc_call(candidate).is_some_and(|call| {
                let complete_parts = call.remainder.split_whitespace().all(|part| {
                    let part = part.trim_start_matches('.');
                    let bytes = part.as_bytes();
                    if bytes.first().is_some_and(u8::is_ascii_digit) {
                        return (bytes.len() == 4 || (bytes.len() == 5 && bytes[4].is_ascii_alphabetic())) && bytes[..4].iter().all(u8::is_ascii_digit);
                    }
                    let cutters = super::runtime::cutter_components(part);
                    !cutters.is_empty() && cutters.iter().all(|(letters, digits)| letters.len() == 1 && !digits.is_empty())
                });
                ((valid_lcc_remainder(call.remainder) && complete_parts) || valid_literature_work_mark(&call))
                    && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
            })
        {
            base = candidate;
        }
    }
    // A single terminal period after a complete publication year is export
    // punctuation, not a decimal or an unfinished Cutter. Keep the dated
    // call intact and preserve the original notation in the match keys.
    if let Some(prefix) = value.strip_suffix('.') {
        if let Some((undated, year)) = prefix.rsplit_once(' ') {
            let year = year.as_bytes();
            if (year.len() == 4 || (year.len() == 5 && year[4].is_ascii_alphabetic()))
                && year[..4].iter().all(u8::is_ascii_digit)
                && !prefix.contains('-')
                && parse_lcc_call(undated).is_some_and(|call| {
                    let known_class = outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters));
                    if !known_class {
                        return false;
                    }
                    if valid_literature_work_mark(&call) {
                        return true;
                    }
                    let mut rest = call.remainder;
                    while !rest.is_empty() {
                        rest = rest.trim_start();
                        rest = rest.strip_prefix('.').unwrap_or(rest).trim_start();
                        let bytes = rest.as_bytes();
                        if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
                            return false;
                        }
                        let digits = bytes[1..].iter().take_while(|b| b.is_ascii_digit()).count();
                        if digits == 0 {
                            return false;
                        }
                        rest = &rest[1 + digits..];
                        if !rest.is_empty() && !rest.starts_with(|c: char| c.is_ascii_whitespace() || c == '.') {
                            return false;
                        }
                    }
                    true
                })
            {
                base = prefix;
            }
        }
    }
    // Libraries append eb (often directly to the date) or ebook to identify
    // electronic copies. The remaining call must independently be complete.
    if let Some(prefix) = value.strip_suffix(" EBOOK").or_else(|| value.strip_suffix("EB")) {
        let prefix = prefix.trim_end();
        let mut candidate = lcc_enumeration_base(prefix).unwrap_or(prefix);
        // Imported ebook records sometimes repeat the marker, or attach it
        // both to the call and to its volume. Only collapse repeated markers
        // after a digit; a literary title mark ending in EB stays intact.
        let mut repeated_base = candidate;
        while let Some(rest) = repeated_base.strip_suffix("EB") {
            repeated_base = rest.trim_end();
        }
        if repeated_base.as_bytes().last().is_some_and(u8::is_ascii_digit) {
            candidate = repeated_base;
        }
        if !candidate.contains('-')
            && parse_lcc_call(candidate).is_some_and(|call| {
                let complete_parts = call.remainder.split_whitespace().all(|part| {
                    let part = part.trim_start_matches('.');
                    if part.is_empty() {
                        return true;
                    }
                    if part.as_bytes()[0].is_ascii_digit() {
                        let (year, cutter) = part.split_once('.').map_or((part, None), |(year, cutter)| (year, Some(cutter)));
                        let year = year.as_bytes();
                        return (year.len() == 4 || (year.len() == 5 && year[4].is_ascii_alphabetic()))
                            && year[..4].iter().all(u8::is_ascii_digit)
                            && cutter.is_none_or(|part| {
                                let cutters = super::runtime::cutter_components(part);
                                !cutters.is_empty() && cutters.iter().all(|(letters, digits)| letters.len() == 1 && !digits.is_empty())
                            });
                    }
                    let cutters = super::runtime::cutter_components(part);
                    !cutters.is_empty() && cutters.iter().all(|(letters, digits)| letters.len() == 1 && !digits.is_empty())
                });
                // LC's MARC 050 for LCCN 75313994 records TX553.A3 W67a.
                // This verified legacy work mark is not a general license to
                // accept arbitrary trailing letters on numeric Cutters.
                // https://openlibrary.org/show-records/marc_loc_2016/BooksAll.2016.part09.utf8:65764277:1235
                // Columbia's MARC 050 4 for OL12773809M independently records
                // QP905 .H236s v.175, corroborating this second legacy base.
                // https://openlibrary.org/show-records/marc_columbia/Columbia-extract-20221130-012.mrc:204912902:1888
                // LC's MARC 050 10 for OL4886228M records PZ3.M8346 Lo5.
                // https://openlibrary.org/show-records/marc_loc_2016/BooksAll.2016.part09.utf8:176896691:989
                let reviewed_legacy_work_mark = matches!(candidate, "TX553.A3 W67A" | "QP905 .H236S" | "PZ3.M8346 LO5");
                ((valid_lcc_remainder(call.remainder) && complete_parts) || valid_literature_work_mark(&call) || reviewed_legacy_work_mark)
                    && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
            })
        {
            base = candidate;
        }
    }
    // Textbook call numbers can append a K–12 grade designation (Tarleton
    // Library). Interpret it only after a complete dated call, so GR as an
    // LCC class or a label inside an unfinished call is not discarded.
    if let Some((prefix, grade)) = value.rsplit_once(" GR.").or_else(|| value.rsplit_once(" GRADE ")) {
        let grade = grade.trim_start();
        let grade_number = |part: &str| -> Option<u8> {
            if part == "K" {
                return Some(0);
            }
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse::<u8>().ok().filter(|n| (1..=12).contains(n))
        };
        let valid_grade = if let Some((start, end)) = grade.split_once('-') { grade_number(start).zip(grade_number(end)).is_some_and(|(a, b)| a <= b) } else { grade_number(grade).is_some() };
        let year = prefix.split_whitespace().last().unwrap_or_default().as_bytes();
        if valid_grade
            && (year.len() == 4 || (year.len() == 5 && year[4].is_ascii_alphabetic()))
            && year[..4].iter().all(u8::is_ascii_digit)
            && !prefix.contains('-')
            && parse_lcc_call(prefix).is_some_and(|call| {
                let cutters = super::runtime::cutter_components(call.remainder);
                valid_lcc_remainder(call.remainder)
                    && !cutters.is_empty()
                    && cutters.iter().all(|(letters, digits)| letters.len() == 1 && !digits.is_empty())
                    && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
            })
        {
            base = prefix;
        }
    }
    // Serial enumeration can use year:issue. Keep the complete dated call
    // as the match base; never turn a bare number or truncated issue into LCC.
    if let Some((prefix, issue)) = value.rsplit_once(':') {
        let year = prefix.split_whitespace().last().unwrap_or_default();
        if year.len() == 4
            && year.bytes().all(|b| b.is_ascii_digit())
            && !issue.is_empty()
            && issue.bytes().all(|b| b.is_ascii_digit())
            && !prefix.contains(['-', ':'])
            && parse_lcc_call(prefix).is_some_and(|call| {
                let cutters = super::runtime::cutter_components(call.remainder);
                !call.letters.starts_with('K')
                    && valid_lcc_remainder(call.remainder)
                    && !cutters.is_empty()
                    && cutters.iter().all(|(letters, digits)| letters.len() == 1 && !digits.is_empty())
                    && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
            })
        {
            base = prefix;
        }
    }
    // UBC's standard print suffix list includes workbook. A year is not
    // required when the preceding classification and Cutter are complete.
    let workbook_prefix = [" STUDENT WORKBOOK", " WORKBOOK"].iter().find_map(|suffix| value.strip_suffix(suffix)).or_else(|| {
        let (prefix, tail) = value.rsplit_once(" WORKBOOK ")?;
        let year = tail.as_bytes();
        let complete_year = (year.len() == 4 || (year.len() == 5 && year[4].is_ascii_alphabetic())) && year[..4].iter().all(u8::is_ascii_digit);
        let numbered_part = tail.starts_with("PART ") && lcc_enumeration_base(&format!("{prefix} {tail}")) == Some(prefix);
        (complete_year || tail == "TEACHER'S EDITION" || numbered_part).then_some(prefix)
    });
    if let Some(prefix) = workbook_prefix {
        let prefix = prefix.trim_end().strip_suffix(',').unwrap_or(prefix.trim_end()).trim_end();
        let candidate = lcc_enumeration_base(prefix).unwrap_or(prefix);
        if parse_lcc_call(candidate).is_some_and(|call| {
            let cutters = super::runtime::cutter_components(call.remainder);
            !candidate.contains('-')
                && valid_lcc_remainder(call.remainder)
                && !cutters.is_empty()
                && cutters.iter().all(|(letters, digits)| letters.len() == 1 && !digits.is_empty())
                && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
        }) {
            base = candidate;
        }
    }
    // Accompanying print material and explicit folio-size labels keep the
    // original dated call (Yale Accompanying Material and folio guidance).
    // LC 050 for OL3795881M records <fol>. Imported edition records also
    // display fol and <fol.>; recognize only these exact folio spellings.
    // Recognize only these documented descriptors, not arbitrary prose.
    if let Some(prefix) = [
        " INSTRUCTOR'S RESOURCE MANUAL",
        " INSTRUCTOR'S MANUAL",
        " INSTRUCTOR MANUAL",
        " INSTRUCTOR'S GUIDE",
        " TEACHER'S MANUAL",
        " TEACHERS GUIDE",
        " TEACHER GUIDE",
        " TEACHER'S GUIDE",
        " STUDY GUIDE",
        " STUDENT GUIDE",
        " STUDENT MANUAL",
        " SOLUTIONS MANUAL",
        " SOLUTION MANUAL",
        " LAB MANUAL",
        " USER GUIDE",
        " GUIDE",
        " MANUAL",
        " WORKBOOK",
        " FOLIO",
        " FOL.",
        " FOL",
        " <FOL>",
        " <FOL.>",
    ]
    .iter()
    .find_map(|suffix| value.strip_suffix(suffix))
    {
        let prefix = prefix.trim_end().strip_suffix(',').unwrap_or(prefix.trim_end()).trim_end();
        let year = prefix.split_whitespace().last().unwrap_or_default().as_bytes();
        if (year.len() == 4 || (year.len() == 5 && year[4].is_ascii_alphabetic()))
            && year[..4].iter().all(u8::is_ascii_digit)
            && parse_lcc_call(prefix)
                .is_some_and(|call| valid_lcc_remainder(call.remainder) && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters)))
        {
            base = prefix;
        }
    }
    // Folio markers describe physical size, including after existing volume
    // enumeration. Angle brackets supply their own token boundary. Retain
    // literary work letters only when a date or numbered caption disambiguates
    // the size label from an undated title mark.
    if let Some(prefix) = ["<FOL.>", "<FOL>", " FOLIO", " FOL.", " FOL"].iter().find_map(|suffix| value.strip_suffix(suffix)) {
        let prefix = prefix.trim_end().strip_suffix(',').unwrap_or(prefix.trim_end()).trim_end();
        let enumerated = lcc_enumeration_base(prefix);
        let candidate = enumerated.unwrap_or(prefix);
        let year = candidate.split_whitespace().last().unwrap_or_default().as_bytes();
        let dated = (year.len() == 4 || (year.len() == 5 && year[4].is_ascii_alphabetic())) && year[..4].iter().all(u8::is_ascii_digit);
        let numbered = enumerated.is_some() && prefix[candidate.len()..].bytes().any(|b| b.is_ascii_digit());
        if !candidate.contains('-')
            && parse_lcc_call(candidate).is_some_and(|call| {
                let literary = matches!(call.letters.as_str(), "PR" | "PS" | "PZ");
                let cutters = super::runtime::cutter_components(call.remainder);
                let numeric_cutters = !cutters.is_empty() && cutters.iter().all(|(letters, digits)| letters.len() == 1 && !digits.is_empty());
                (dated || numbered || (!literary && numeric_cutters))
                    && (valid_lcc_remainder(call.remainder) || valid_literature_work_mark(&call))
                    && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
            })
        {
            base = candidate;
        }
    }
    // Some imported calls separate the publication year with a semicolon.
    // Only a complete four-digit year can be detached; a second call or an
    // unfinished classification list must still use the list parser.
    if let Some((prefix, year)) = value.split_once(';') {
        let year = year.trim();
        let prefix = prefix.trim_end();
        if year.len() == 4
            && year.bytes().all(|b| b.is_ascii_digit())
            && !prefix.contains('-')
            && parse_lcc_call(prefix).is_some_and(|call| {
                valid_lcc_remainder(call.remainder)
                    && call.remainder.split(|c: char| c.is_ascii_whitespace() || c == '.').filter(|part| !part.is_empty()).all(|part| {
                        let bytes = part.as_bytes();
                        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1..].iter().all(u8::is_ascii_digit)
                    })
                    && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
            })
        {
            base = prefix;
        }
    }
    // A complete call followed by the physical-item qualifier (set) keeps
    // its classification. Do not interpret other parenthesized text here.
    if let Some(prefix) = value.strip_suffix("(SET)") {
        let prefix = prefix.trim_end();
        if parse_lcc_call(prefix).is_some_and(|call| outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))) {
            base = prefix;
        }
    }
    // CSB 36 (Spring 1987), p. 43: minimally cataloged maps retain a
    // regular LC call number followed by a space and MLC. This marker
    // supplies no subject of its own; the complete map call must validate.
    if let Some(prefix) = value.strip_suffix(" MLC") {
        let prefix = prefix.trim_end();
        if parse_lcc_call(prefix).is_some_and(|call| call.letters == "G" && valid_lcc_remainder(call.remainder)) {
            base = prefix;
        }
    }
    let opener = match value.as_bytes().last() {
        Some(b'>') => '<',
        Some(b')') => '(',
        _ => '\0',
    };
    if opener != '\0' {
        if let Some((prefix, note)) = value[..value.len() - 1].rsplit_once(opener) {
            let parts = note.split_whitespace().collect::<Vec<_>>();
            let language = |s: &str| matches!(s, "CHINA" | "JAPAN" | "KOREA" | "ARAB" | "HEBR" | "HIND" | "PERS" | "AMHAR" | "TIGR" | "CEBUANO");
            let recognized = match parts.as_slice() {
                ["HEBR"] => true,
                ["ORIEN" | "ASIAN" | "AMED", lang] => language(lang),
                ["ORIEN" | "ASIAN" | "AMED", lang, "CAGE"] => language(lang),
                _ => false,
            };
            if recognized && prefix.ends_with(char::is_whitespace) {
                base = prefix.trim_end();
            }
        }
    }
    // Yale appends (LC) to call numbers in designated shelving runs.
    // It carries no additional subject information. Require a complete call
    // with a known LCC class; the marker alone never creates a match.
    if let Some(prefix) = base.strip_suffix(" (LC)") {
        let call_base = lcc_enumeration_base(prefix).unwrap_or(prefix);
        if parse_lcc_call(call_base).is_some_and(|call| {
            (valid_lcc_remainder(call.remainder) || valid_literature_work_mark(&call))
                && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
        }) {
            base = prefix;
        }
    }
    // Yale's historical oversize notation appends + (or ++ for scores).
    // Recognize it only after a complete dated call with numeric Cutters;
    // a plus on a class/range remains a classification-scope expression.
    if let Some(prefix) = base.strip_suffix("++").or_else(|| base.strip_suffix('+')) {
        if let Some((call_base, year)) = prefix.rsplit_once(' ') {
            let year = year.as_bytes();
            if (year.len() == 4 || (year.len() == 5 && year[4].is_ascii_alphabetic()))
                && year[..4].iter().all(u8::is_ascii_digit)
                && !call_base.contains('-')
                && parse_lcc_call(call_base).is_some_and(|call| {
                    let cutters = super::runtime::cutter_components(call.remainder);
                    valid_lcc_remainder(call.remainder)
                        && !cutters.is_empty()
                        && cutters.iter().all(|(letters, digits)| letters.len() == 1 && !digits.is_empty())
                        && outline().ranges_by_main.get(&call.letters.chars().next().unwrap()).is_some_and(|ranges| ranges.iter().any(|range| range.start_letters == call.letters))
                })
            {
                base = prefix;
            }
        }
    }
    base = lcc_enumeration_base(base).unwrap_or(base);
    if base == value {
        return None;
    }
    let valid = parse_lcc_call(base).is_some_and(|call| valid_lcc_remainder(call.remainder) || valid_literature_work_mark(&call));
    valid.then_some(base)
}

// G350 documents PZ title work letters. Historical MARC imports also append
// the PZ item subfield to alternate PR/PS classifications (e.g. LC 35014886).
// Recognize that imported shape without treating it as standard PR/PS shelving.
fn valid_literature_work_mark(call: &LccCall<'_>) -> bool {
    if !matches!(call.letters.as_str(), "PZ" | "PR" | "PS") {
        return false;
    }
    let remainder = call.remainder.trim_start();
    let remainder = remainder.strip_prefix('.').unwrap_or(remainder).trim_start();
    let mut parts = remainder.split_whitespace();
    let Some(first) = parts.next() else {
        return false;
    };
    let author = first.as_bytes();
    let has_cutter = author.len() >= 2 && author[0].is_ascii_alphabetic() && author[1..].iter().all(u8::is_ascii_digit);
    let title = if has_cutter {
        let Some(title) = parts.next() else {
            return false;
        };
        title
    } else if matches!(call.letters.as_str(), "PR" | "PS") {
        // Alternate PR/PS numbers may have no Cutter at all (LC 78316959:
        // $a PZ3.D55 $b Su 1978 $a PR4552). The imported item mark adds
        // no author precision to the explicit alternate class.
        first
    } else {
        return false;
    };
    // G350 section 8 preserves the historical single-letter PZ practice
    // (PZ7.S268 E). Alternate PR/PS imports retain their existing rule.
    let minimum_title_letters = if call.letters == "PZ" { 1 } else { 2 };
    if title.len() < minimum_title_letters || !title.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    if let Some(date) = parts.next() {
        let date = date.as_bytes();
        if !(date.len() == 4 || (date.len() == 5 && date[4].is_ascii_alphabetic())) || !date[..4].iter().all(u8::is_ascii_digit) {
            return false;
        }
    }
    parts.next().is_none()
}

fn valid_lcc_remainder(mut remainder: &str) -> bool {
    while !remainder.is_empty() {
        remainder = remainder.trim_start();
        if remainder.is_empty() {
            return true;
        }
        let bytes = remainder.as_bytes();
        let mut cursor;
        match bytes[0] {
            b'.' => {
                cursor = 1;
                // Spacing after the Cutter separator does not change its
                // letter/number components. Do not consume a second period.
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                if !bytes.get(cursor).is_some_and(u8::is_ascii_alphabetic) {
                    return false;
                }
                while bytes.get(cursor).is_some_and(|byte| byte.is_ascii_alphanumeric()) {
                    cursor += 1;
                }
            }
            b'-' => {
                cursor = 1;
                if !bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                    return false;
                }
                while bytes.get(cursor).is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.') {
                    cursor += 1;
                }
            }
            byte if byte.is_ascii_alphabetic() => {
                cursor = 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_alphabetic) {
                    cursor += 1;
                }
                if !bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                    return false;
                }
                while bytes.get(cursor).is_some_and(u8::is_ascii_alphanumeric) {
                    cursor += 1;
                }
            }
            byte if byte.is_ascii_digit() => {
                cursor = 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_alphanumeric) {
                    cursor += 1;
                }
            }
            _ => return false,
        }
        if bytes.get(cursor).is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'.') {
            return false;
        }
        remainder = &remainder[cursor..];
    }
    true
}

fn letters_only_syntax(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4 && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn build_subject_path(outline_path: &str, display: &str) -> String {
    let labels = outline_path.split(" / ").collect::<Vec<_>>();
    let selected = if labels.len() <= 4 { labels } else { vec![labels[0], labels[1], labels[labels.len() - 2], labels[labels.len() - 1]] };
    let mut components = vec!["LCC".to_owned()];
    for label in selected {
        if components.last().is_none_or(|existing| !existing.eq_ignore_ascii_case(label)) {
            components.push(label.to_owned());
        }
    }
    let display = compact_whitespace(display);
    if components.last().is_none_or(|existing| !existing.eq_ignore_ascii_case(&display)) {
        components.push(display);
    }
    components.join(" / ")
}

pub(super) fn parse_lcc_call(value: &str) -> Option<LccCall<'_>> {
    let value = value.trim();
    let bytes = value.as_bytes();
    let mut cursor = 0;
    // Regional law schedules can use a four-letter subclass (for example
    // KJCX), even though the common LCC classes use one to three letters.
    while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() && cursor < 4 {
        cursor += 1;
    }
    if cursor == 0 || bytes.get(cursor).is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    let letters = value[..cursor].to_ascii_uppercase();
    while bytes.get(cursor).is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'\'') {
        cursor += 1;
    }
    let number_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    if cursor == number_start {
        return None;
    }
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let decimal_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == decimal_start {
            cursor -= 1;
        }
    }
    let number = value[number_start..cursor].parse::<f64>().ok()?;
    Some(LccCall { letters, number, display: value, remainder: &value[cursor..] })
}

/// Component boundaries, not textual prefixes: C6 and C65 are distinct.
pub(super) fn lcc_match_keys(value: &str) -> Option<Vec<String>> {
    let canonical = canonical_lcc_notation(value)?;
    if let Some(base) = apostrophe_call_base(&canonical) {
        let mut keys = lcc_match_keys(&base)?;
        keys.push(canonical);
        return Some(keys);
    }
    if let Some(assignment) = minimal_class_assignment(&canonical) {
        return Some(vec![format!("({assignment})"), canonical]);
    }
    if let Some(base) = lcc_item_base(&canonical) {
        let mut keys = lcc_match_keys(base)?;
        keys.push(canonical);
        return Some(keys);
    }
    let Some(call) = parse_lcc_call(&canonical) else { return Some(vec![canonical]) };
    // Printed schedule spans are atomic selectors, not individual call numbers.
    if call.remainder.contains('-') {
        return Some(vec![canonical]);
    }
    let mut key = canonical[..canonical.len() - call.remainder.len()].replace([' ', '\''], "");
    let mut keys = vec![key.clone()];
    let mut rest = call.remainder;
    while !rest.is_empty() {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '.');
        if rest.is_empty() {
            break;
        }
        let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        let digits = rest[letters..].bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            // Preserve earlier topic keys for a single-letter year suffix
            // (1999a), without broadening matching of arbitrary Cutter suffixes.
            let year_suffix = rest.len() == 1 && rest.as_bytes()[0].is_ascii_alphabetic() && keys.last().and_then(|key| key.rsplit_once(' ')).is_some_and(|(_, year)| year.len() == 4 && year.bytes().all(|byte| byte.is_ascii_digit()));
            if year_suffix || valid_literature_work_mark(&call) {
                if keys.last() != Some(&canonical) {
                    keys.push(canonical);
                }
                return Some(keys);
            }
            return Some(vec![canonical]);
        }
        let end = letters + digits;
        key.push(if letters == 0 { ' ' } else { '.' });
        key.push_str(&rest[..end]);
        keys.push(key.clone());
        rest = &rest[end..];
    }
    Some(keys)
}

pub(super) fn compare_lcc_key(left_letters: &str, left_number: f64, right_letters: &str, right_number: f64) -> std::cmp::Ordering {
    left_letters.cmp(right_letters).then_with(|| left_number.total_cmp(&right_number))
}

fn range_contains(range: &LccRange, call: &LccCall<'_>) -> bool {
    compare_lcc_key(&range.start_letters, range.start_number, &call.letters, call.number).is_le() && compare_lcc_key(&call.letters, call.number, &range.end_letters, range.end_number).is_le()
}

fn compare_matches(left: &LccRange, right: &LccRange, call: &LccCall<'_>) -> std::cmp::Ordering {
    let left_exact = left.start_letters == call.letters && left.end_letters == call.letters;
    let right_exact = right.start_letters == call.letters && right.end_letters == call.letters;
    left.path.matches(" / ").count().cmp(&right.path.matches(" / ").count()).then_with(|| left_exact.cmp(&right_exact)).then_with(|| {
        let left_span = if left_exact { left.end_number - left.start_number } else { f64::INFINITY };
        let right_span = if right_exact { right.end_number - right.start_number } else { f64::INFINITY };
        right_span.total_cmp(&left_span)
    })
}

#[cfg(test)]
mod tests {
    use super::canonical_lcc_notation;

    #[test]
    fn canonical_lcc_rejects_storage_markers_and_malformed_trailing_text() {
        for invalid in ["CPB", "CPB Box no. 1574", "MLCS", "MLCS 2021/45060", "LAW+", "LCC", "Comic", "Microfiche 82/7", "QA76 BOX NO. 4", "QA76, PN1995"] {
            assert_eq!(canonical_lcc_notation(invalid), None, "accepted invalid LCC value {invalid}");
        }
    }

    #[test]
    fn canonical_lcc_accepts_complete_call_numbers() {
        for valid in ["ML", "QA76.73.R87", "PS3558.O3447 D68 2015", "KJCX 12.4 .A2"] {
            assert!(canonical_lcc_notation(valid).is_some(), "rejected valid LCC value {valid}");
        }
    }

    #[test]
    fn pz_title_work_letters_preserve_author_keys() {
        for code in ["PZ7.R79835 Ham 1999", "PZ7.R79835 Halm 2003", "PZ7.T47 Wh", "PZ7.B1387 Lo 1988a"] {
            assert!(canonical_lcc_notation(code).is_some(), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(keys[0], "PZ7");
            assert!(keys[1].starts_with("PZ7."));
            assert_eq!(keys.last(), Some(&code.to_ascii_uppercase()));
        }
        for code in ["PZ7 Wh 1999", "PZ7.R79835 BOX NO. 4", "PZ7.R79835 Wh 12", "PZ7.R79835 Wh 1999 extra", "QA76.R79835 Ham 1999"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn numbered_item_suffixes_preserve_complete_notations_and_base_keys() {
        assert!(canonical_lcc_notation("PZ7.T47 Wh vol. 2").is_some());
        for suffix in ["VOL. 2", "V.2", "VOL. 4, PT. 1", "NO. 1744", "SECT. 2 SESS. 1"] {
            let code = format!("QA3 .L28 {suffix}");
            assert_eq!(canonical_lcc_notation(&code), Some(code.clone()));
            assert_eq!(super::lcc_match_keys(&code).unwrap(), vec!["QA3".to_owned(), "QA3.L28".to_owned(), code]);
        }
        for code in ["QA3 .L28 VOL. XBOX", "QA3 .L28 VOL. 1,", "QA3 .L28 VOL. 1 BOX 2", "QA3 .L28 VOL. 1-QB2", "QA3 BAD TEXT VOL. 1", "CPB VOL. 2", "QA3 .L28 SUPPLEMENT NOTES", "QA3 .L28 SUPPL. UNKNOWN"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn custodial_notes_preserve_valid_classification_keys() {
        for note in ["<Orien China>", "<Asian Japan>", "<HEBR>", "(Orien China)", "<AMED Arab>", "<Asian China Cage>"] {
            let code = format!("PL2780.F4 1981b {note}");
            let canonical = canonical_lcc_notation(&code).unwrap();
            assert_eq!(canonical, code.to_ascii_uppercase());
            let keys = super::lcc_match_keys(&code).unwrap();
            assert!(keys.contains(&"PL2780.F4".to_owned()));
            assert_eq!(keys.last(), Some(&canonical));
        }
        assert_eq!(super::lcc_item_base("QA3 .L28 VOL. 2 <ASIAN CHINA>"), Some("QA3 .L28"));
        for code in ["PL2780.F4 <UNKNOWN>", "PL2780.F4 <ASIAN UNKNOWN>", "PL2780.F4 <ASIAN CHINA> EXTRA", "BAD TEXT <ORIEN CHINA>", "CPB <ORIEN CHINA>", "PL2780.F4 <ASIAN CHINA)"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn minimal_class_assignment_parentheses_delimit_the_final_letter() {
        for (code, assignment) in [("MLCL 2006/00304(B)", "(B)"), ("MLCM 2006/00248(R)", "(R)"), ("MICROFILM (O) 85/4003(P)", "(P)")] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            assert_eq!(super::lcc_match_keys(code).unwrap(), vec![assignment.to_owned(), code.to_owned()]);
            let spaced = code.replace(assignment, &format!(" {assignment}"));
            let a = super::lcc_subject_path(code).unwrap();
            let b = super::lcc_subject_path(&spaced).unwrap();
            assert_eq!(a.rsplit_once(" / ").unwrap().0, b.rsplit_once(" / ").unwrap().0);
        }
        for code in ["MLCL 2006/00304(ZZ)", "MLCL 2006/00304(B) NOTE", "MLCL 2006/00304(B)(P)", "MLCL 2006/00304X(B)", "MLCL 2006/00304 (O)(B)"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn cutter_separator_spacing_keeps_the_same_components() {
        for (code, base) in [("TJ217.2. C48 2000", "TJ217.2.C48 2000"), ("QA76 . A1 . B2 1999", "QA76.A1.B2 1999"), ("AC106 . A24 2013", "AC106.A24 2013")] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            assert_eq!(super::lcc_match_keys(code), super::lcc_match_keys(base));
        }
        for code in ["PZ7 . R79835 HAM 1999", "PR6045 . R35 DC"] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            assert!(super::lcc_match_keys(code).unwrap().len() >= 3);
        }
        for code in ["QA76 . 123", "QA76 . ", "QA76 . A1 UNKNOWN"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn historical_pz_single_title_letters_keep_author_evidence() {
        for (code, class, cutter) in [("PZ7.S268 E", "PZ7", "PZ7.S268"), ("PZ7.C54 A", "PZ7", "PZ7.C54"), ("PZ10.831.G586 E 1998", "PZ10.831", "PZ10.831.G586")] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            assert_eq!(super::lcc_match_keys(code).unwrap(), vec![class.to_owned(), cutter.to_owned(), code.to_owned()]);
        }
        for code in ["PZ7 E", "PZ7.S E", "PZ7.S268 E EXTRA", "PZ7.S268 E 19", "PR4552 S", "PS648.S3 B", "QA76.S268 E"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn series_enumeration_preserves_complete_call_and_evidence() {
        let base = "QA3 .A572";
        for suffix in ["SER. 2, VOL. 130", "SER.2 VOL.130"] {
            let code = format!("{base} {suffix}");
            assert_eq!(super::lcc_item_base(&code), Some(base), "{code}");
            let mut keys = super::lcc_match_keys(base).unwrap();
            keys.push(code.clone());
            assert_eq!(super::lcc_match_keys(&code), Some(keys));
        }
        for suffix in ["SER.", "SER. 2,", "SERIES UNKNOWN", "SER. 2 EXTRA", "NEW SER. UNKNOWN", "NEW SERIES,", "SER. 2 QA76", "SER. IIV", "SERIAL 2"] {
            let code = format!("{base} {suffix}");
            assert!(super::lcc_item_base(&code).is_none(), "{code}");
        }
    }

    #[test]
    fn sleep_outline_uses_bf_and_does_not_register_bg() {
        let subclasses = super::outline_subclasses();
        assert!(subclasses.contains("BF"));
        assert!(!subclasses.contains("BG"));
        for code in ["BF1068", "BF1071", "BF1073.S58"] {
            assert!(super::lcc_subject_path(code).unwrap().contains("Parapsychology / Sleep. Somnambulism"), "{code}");
        }
        for code in ["BF1067", "BF1074"] {
            assert!(!super::lcc_subject_path(code).unwrap().contains("Sleep. Somnambulism"), "{code}");
        }
        assert!(super::lcc_subject_path("BG1068").is_none());
        assert!(canonical_lcc_notation("MLCS 2000/1234 (BG)").is_none());
    }

    #[test]
    fn mlc_delimiter_spacing_retains_digit_and_assignment_boundaries() {
        for code in ["MLCM 2006/05417 ( H)", "MLCM 2006/05417 (H )", "MLCM 2006 / 05417 ( H )", "MLCM 2006/ 05417(H)", "MLCM 2006 /05417 (H)"] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            assert_eq!(super::lcc_match_keys(code).unwrap(), vec!["(H)", code]);
            assert!(super::lcc_subject_path(code).is_some());
        }

        for code in ["MLCM 20 06/05417 (H)", "MLCM 2006/05 417 (H)", "MLCM 2006//05417 (H)", "MLCM 2006/ (H)", "MLCM 2006/05417 EXTRA (H)", "MLCM 2006/05417 (P Q)"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn mlc_placeholders_require_an_explicit_main_class() {
        for code in ["MLCXXX (P)", "MLCXXX(P)", "MLC XXXX (P)", "MLCMA XXXXX (P)", "MLCSA XXX(P)", "MLCSAXXX(P)", "MLCSAXXXX(P)"] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            assert_eq!(super::lcc_match_keys(code).unwrap(), vec!["(P)", code]);
            assert!(super::lcc_subject_path(code).is_some());
        }
        for code in ["MLCXXX", "MLCMA XXXXX", "MLCXXX (PQ)", "MLCXXX (X)", "MLCXXX (P) EXTRA", "MLCXYX (P)", "MLCB XXXXX (P)", "MLCMA XXXXX OTHER (P)", "MLCMA XXXXXX (P)"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn alternate_literature_classes_without_cutters_keep_class_evidence() {
        for code in ["PR4552 SU 1978", "PS2112 ST", "PR1285 TE", "PS3566 PIC"] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            let class = code.split_whitespace().next().unwrap();
            assert_eq!(super::lcc_match_keys(code).unwrap(), vec![class, code]);
        }
        for code in ["PR4552 S", "PR4552 SU EXTRA", "PR4552 SU 19", "PR4552 SU 1978 1979", "PZ3 SU 1978", "QA76 SU 1978"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn imported_alternate_literature_classes_retain_class_and_cutter_keys() {
        for (code, class, cutter) in [("PR6045.R35 Dc", "PR6045", "PR6045.R35"), ("PS648.S3 Be", "PS648", "PS648.S3"), ("PS3566.I4 Hi 1978", "PS3566", "PS3566.I4")] {
            let canonical = code.to_ascii_uppercase();
            assert_eq!(canonical_lcc_notation(code), Some(canonical.clone()));
            assert_eq!(super::lcc_match_keys(code).unwrap(), vec![class.to_owned(), cutter.to_owned(), canonical]);
        }
        for code in ["PR6045 D", "PS648.S3 B", "PS648.S3 BE EXTRA", "PS648.S3 BE 12", "QA76.R35 DC"] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
        assert_eq!(super::lcc_item_base("PS648.S3 BE VOL. 2"), Some("PS648.S3 BE"));
    }

    #[test]
    fn established_vernacular_volume_labels_preserve_call_number_keys() {
        for suffix in [
            "BD. 12",
            "BDCHN. 2",
            "NR. 45",
            "T. 6",
            "FASC. 129",
            "ABT. 12, T. 5",
            "LFG. 2",
            "LIVR. 3",
            "BK. 1",
            "JAHRG. 1978, NR. 3",
            "JAARG. 4",
            "KN. 2",
            "KNJ. 3",
            "CZ. 2",
            "D. 5",
            "SV. 2",
            "ZESZ. 8",
            "VYP. 15",
            "HEFT 1",
            "H. 2",
            "HFT. 3",
            "HBD. 1",
            "HALBBD. 2",
            "BD. 10, FASZ. 2",
            "BD. 293, ABH 1",
            "DL. 10",
            "BD 24",
        ] {
            let code = format!("QA3 .L28 {suffix}");
            assert_eq!(canonical_lcc_notation(&code), Some(code.clone()));
            assert_eq!(super::lcc_match_keys(&code).unwrap(), vec!["QA3".to_owned(), "QA3.L28".to_owned(), code]);
        }
        for code in [
            "QA3 .L28 BD.",
            "QA3 .L28 BD. UNKNOWN",
            "QA3 .L28 NR. 2 EXTRA",
            "QA3 .L28 FASZ. UNKNOWN",
            "QA3 .L28 FAS. 1 UNKNOWN",
            "QA3 .L28 ABH EXTRA",
            "QA3 .L28 BD 24 UNKNOWN",
            "QA3 .L28 BDX 24",
            "QA3 .L28 T. 1-QB2",
            "BAD TEXT BD. 1",
            "QA3 .L28 FASC. 2,",
            "QA3 .L28 NR. 2 99",
            "QA3 .L28 HEFT EXTRA",
        ] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn apostrophes_separate_complete_components_without_erasing_evidence() {
        for (code, base) in [("TN'291'C35'1977", "TN291.C35 1977"), ("HF5415.12'G7'D37'1976", "HF5415.12.G7.D37 1976"), ("BR'60'A64", "BR60.A64")] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            let mut expected = super::lcc_match_keys(base).unwrap();
            expected.push(code.to_owned());
            assert_eq!(super::lcc_match_keys(code).unwrap(), expected);
        }
        for code in ["QA76'73", "QA76'BAD'2000", "TN'291'C35'", "TN''291'C35", "'TN291'C35", "TN'291'C35'1977'NOTE", "516.1'5", "QA76'C35'QB2"] {
            assert!(super::apostrophe_call_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn map_mlc_marker_preserves_the_explicit_classification() {
        let code = "G6513.M6E635 1993 .G4 MLC";
        assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
        assert_eq!(super::lcc_item_base(code), Some("G6513.M6E635 1993 .G4"));
        let keys = super::lcc_match_keys(code).unwrap();
        assert!(keys.contains(&"G6513".to_owned()));
        assert_eq!(keys.last().map(String::as_str), Some(code));
        assert_eq!(super::lcc_item_base("G6513.M6E635MLC"), None);
        for invalid in ["G6513 BAD TEXT MLC", "MLC", "ENHANCED MLC", "428.24 .R777P 1999 OISE/UT MLC", "QA76.A1 MLC"] {
            assert!(canonical_lcc_notation(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn mlc_shelf_numbers_require_an_explicit_class_assignment() {
        for (code, key) in [
            ("MLCM 98/02114 (P)", "(P)"),
            ("MLCMJ 2003/00135 (S)", "(S)"),
            ("MLC R 85/7 (D)", "(D)"),
            ("MLCS 2005/04905 (Z)", "(Z)"),
            ("MICROFILM 85/4003 (P)", "(P)"),
            ("MICROFICHE (O) 84/61398 (B)", "(B)"),
            ("MICROOPAQUE (W) 85/42 (D)", "(D)"),
        ] {
            assert_eq!(canonical_lcc_notation(code).as_deref(), Some(code));
            assert_eq!(super::lcc_match_keys(code).unwrap(), vec![key.to_owned(), code.to_owned()]);
        }
        for code in [
            "MLCS",
            "MLCMJ 2003/00135",
            "MLCS 2010/40000 (I)",
            "MLCS 2010/40000 (ZZ)",
            "MLCX 2010/40000 (P)",
            "MLCS 201/40000 (P)",
            "MLCS 2010/40000X (P)",
            "MLCS 2010/40000 (P) NOTE",
            "MICROFILM 85/4003",
            "MICROFICHE (X) 84/61398 (B)",
            "MICROFILM 85/4003 (PQ)",
        ] {
            assert!(canonical_lcc_notation(code).is_none(), "{code}");
        }
    }

    #[test]
    fn minimal_class_outline_paths_use_the_assignment_not_shelf_prefix() {
        assert!(super::lcc_subject_path("MLCS 2005/00456 (H)").unwrap().contains("Social Sciences"));
        assert!(super::lcc_subject_path("MICROFILM 85/4003 (P)").unwrap().contains("Language and Literature"));
        let broad_b = super::lcc_subject_path("MLCM 98/02114 (B)").unwrap();
        assert!(broad_b.contains("Psychology") && broad_b.contains("Religion"));
        assert!(super::lcc_subject_path("MLCM 98/02114").is_none());
    }

    #[test]
    fn fully_written_ranges_reject_incomplete_or_reversed_endpoints() {
        for invalid in ["PN1-PN6790-PN7000", "PN6790-PN1", "QA76.C5-QA76.A1", "RZ-R"] {
            assert_eq!(canonical_lcc_notation(invalid), None, "{invalid}");
        }
    }
}

#[cfg(test)]
mod physical_set_tests {
    #[test]
    fn set_qualifiers_preserve_complete_call_and_evidence() {
        for (code, base) in [("N7340 .K816 (set)", "N7340 .K816"), ("GR335 .L44 2007(set)", "GR335 .L44 2007")] {
            let canonical = super::canonical_lcc_notation(code).unwrap();
            assert_eq!(canonical, code.to_ascii_uppercase());
            assert_eq!(super::lcc_item_base(&canonical), Some(base));
            let path = super::lcc_subject_path(code).unwrap();
            let base_path = super::lcc_subject_path(base).unwrap();
            assert_eq!(path.rsplit_once(" / ").unwrap().0, base_path.rsplit_once(" / ").unwrap().0);
            assert_eq!(super::lcc_match_keys(&canonical).unwrap().last(), Some(&canonical));
        }
        for code in ["N7340 ??? (SET)", "N7340 .K816 (SETS)", "WB18.2 (SET)", "LAW (SET)", "N7340 .K816 (SET) EXTRA"] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }
}

#[cfg(test)]
mod composed_item_metadata_tests {
    #[test]
    fn ordinal_and_plural_item_captions_preserve_classification() {
        for (code, base) in [("DF10 .A825 AR.197", "DF10 .A825"), ("CD6151.S57 S37 ZV. 31", "CD6151.S57 S37")] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base));
            assert!(super::lcc_match_keys(code).is_some());
        }
        for code in ["AC9 .A6 13RD SER.", "AC9 .A6 3RD UNKNOWN", "AC9 .A6 3RD SER.,", "AC9 .A6 3..SER.", "AC9 .A6 3RD SER. UNKNOWN"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn compound_serial_captions_keep_complete_chronology() {
        for (code, base) in [("AS182 .H4 1971 ABH. 3", "AS182 .H4 1971"), ("AS182 .G811 FOLGE 3, NR. 31", "AS182 .G811")] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base));
            assert!(super::lcc_match_keys(code).is_some());
        }
        for code in ["AM101 .M9743 N.F.UNKNOWN", "AM101 .M9743 JAHRG. 44, 1976, UNKNOWN", "AM101 .M9743 JAHRG. 44, 1976,", "AM101 .M9743 JAHRG. 44, 76, HEFT 1"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn accompanying_print_descriptors_follow_complete_dated_calls() {
        for (code, base) in [
            ("QD33 .Z843 1998 GUIDE", "QD33 .Z843 1998"),
            ("BC71 .C49 1990 MANUAL", "BC71 .C49 1990"),
            ("BF121 .B47 1991, GUIDE", "BF121 .B47 1991"),
            ("BF121 .C522 2010 STUDY GUIDE", "BF121 .C522 2010"),
            ("BF637 C45 G35 2005 INSTRUCTOR'S RESOURCE MANUAL", "BF637 C45 G35 2005"),
            ("QA303 .A1 2010 SOLUTIONS MANUAL", "QA303 .A1 2010"),
        ] {
            assert_eq!(super::lcc_item_base(code), Some(base));
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(keys.last().unwrap(), code);
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
        }
        for code in ["GUIDE RECORD", "QD33 UNKNOWN 1998 GUIDE", "QD33 .Z843 1998 GUIDE EXTRA", "QD33 .Z843 98 GUIDE", "QD33 .Z843 1998 UNKNOWN", "WB18.2 .A1 1998 GUIDE"] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn documented_vernacular_captions_keep_the_complete_call() {
        for (code, base) in [("AM101 .P295845 SZ. 18", "AM101 .P295845"), ("AM49 F67 HEFT. 16", "AM49 F67"), ("B56 .H53 NIDE 360", "B56 .H53"), ("AS262.T84 A3 OSA 139", "AS262.T84 A3")] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base));
            assert!(super::lcc_match_keys(code).is_some());
        }
        for code in ["AM101 .M9743 N.F.UNKNOWN", "AS244 .A512 DEEL ?", "AM101 .P295845 SZ.UNKNOWN"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn alphanumeric_item_numbers_follow_recognized_captions() {
        for (code, base) in [("PJ2463 .M66 VOL. 7, ISSUE 1", "PJ2463 .M66"), ("QA1 .A1 NO 12", "QA1 .A1"), ("QA1 .A1 VOL 2", "QA1 .A1")] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base));
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
        }
        for code in ["HD4456 .A49 NO. M5UNKNOWN", "HD4456 .A49 NO. M?", "HD4456 .A49 NO. M5-", "HD4456 .A49 M5"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn coverage_year_spans_keep_the_common_call_and_evidence() {
        for (code, base) in [
            ("AM101.S2494 A33 1979-80", "AM101.S2494 A33"),
            ("BX7715 .B3 1833-1838", "BX7715 .B3"),
            ("QA1 .S3575 2000-2001", "QA1 .S3575"),
            ("QA76 .A1 1998-99", "QA76 .A1"),
            ("QA76 .A1 1999-00", "QA76 .A1"),
            ("AS613.S8 A13 1990/1", "AS613.S8 A13"),
            ("PF861 A13 1990/1", "PF861 A13"),
            ("AS284.A1 S37 1978/1979:5", "AS284.A1 S37"),
            ("AS182 .H4 1979/80 ABH. 5", "AS182 .H4"),
            ("QD255 .H4 1979/80 ABH. 5", "QD255 .H4"),
            ("QA3 .L28 1999-2000 V.2", "QA3 .L28"),
            ("QC1 .M23 1985-02", "QC1 .M23"),
            ("QB981 .M23 1985-02", "QB981 .M23"),
        ] {
            assert_eq!(super::lcc_item_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in [
            "QA76 .A1 2001-2000",
            "QA76 .A1 2001-",
            "QA76 .A1 2001-2",
            "QA76 .A1 2001-2002 UNKNOWN",
            "QA76 .A1 5000-5001",
            "QA76 1999-2000",
            "QA76 .A 1999-2000",
            "QA75-76 1999-2000",
            "KF1 .A1 1999-2000",
            "WB18.2 .A1 1999-2000",
            "QA3 .L28 V.1 1999-2000-2001",
            "QA76 .A1 1985-02 UNKNOWN",
            "AS613.S8 A13 1990/",
            "AS613.S8 A13 1990/1 UNKNOWN",
            "AS613.S8 A13 1990/1/2",
            "AS613.S8 A13 90/1",
            "AS613.S8 A13 1990/A",
            "AS613.S8 A13 1990/123456",
            "KF1 .A1 1990/1",
            "WB18.2 .A1 1990/1",
            "AS284.A1 S37 1978/1979:",
            "AS284.A1 S37 1978/1977:5",
            "AS284.A1 S37 1978/1979:5 UNKNOWN",
            "AS284.A1 S37 1978/1979:5:6",
            "AS182 .H4 1979/80 ABH.",
            "AS182 .H4 1979/80 ABH. 5 UNKNOWN",
            "AS182 .H4 1979/80 UNKNOWN 5",
            "KF1 .H4 1979/80 ABH. 5",
        ] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn oversize_letters_preserve_dated_and_enumerated_calls() {
        for (code, base) in [
            ("N6853.P5 D83213 2008 F", "N6853.P5 D83213 2008"),
            ("NA123 .A72 1999 FF", "NA123 .A72 1999"),
            ("NA123 .A72 1999 FFF", "NA123 .A72 1999"),
            ("N6490 P56 V.6 F", "N6490 P56"),
            ("B74 P4414 2004 F", "B74 P4414 2004"),
            ("PZ7.A1 AD 2001 F", "PZ7.A1 AD 2001"),
        ] {
            assert_eq!(super::lcc_item_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in
            ["F", "FF", "FFF", "QA76 .A1 F", "PZ7.A1 F", "QA76 .A1 201 F", "QA76 .A1 2001 FFFF", "QA76 .A1 2001 F UNKNOWN", "QA76 .A 2001 F", "QA76 UNKNOWN 2001 F", "WB18.2 .A1 2001 F", "QA75-76 2001 F", "QA76 .A1 V.2- F", "QA76 .A1 V. F"]
        {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn terminal_period_preserves_complete_dated_call_and_evidence() {
        for (code, base) in [
            ("LB3013.32 .A23 2005.", "LB3013.32 .A23 2005"),
            ("AC75 .S379 1993.", "AC75 .S379 1993"),
            ("PT9875.D5 M3 1976.", "PT9875.D5 M3 1976"),
            ("QA76 .A1 2001A.", "QA76 .A1 2001A"),
            ("E457.92 1967.", "E457.92 1967"),
            ("PZ7.T888 AD 1982.", "PZ7.T888 AD 1982"),
        ] {
            assert_eq!(super::lcc_item_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["QA76.", "QA76 .A1.", "QA76 .A1 200.", "QA76 .A1 2001..", "QA76 .A1 2001.UNKNOWN.", "QA76 .A1 UNKNOWN 2001.", "QA76 .A 2001.", "QA76 .A1 2 001.", "QA75-76 2001.", "WB18.2 .A1 2001.", "WEB 2001."] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn ebook_suffix_preserves_underlying_call_and_evidence() {
        for (code, base) in [
            ("PZ7.T888 AD 1982EB", "PZ7.T888 AD 1982"),
            ("B130 .I63 2001 VOL. 2EB", "B130 .I63 2001"),
            ("GT1720EB", "GT1720"),
            ("QA76 .A1 2001 EBOOK", "QA76 .A1 2001"),
            ("QA76 .A1 EB", "QA76 .A1"),
            ("BF357 .P47EB V. 2EB", "BF357 .P47"),
            ("QA76.76.D47 H46 2002EB EB", "QA76.76.D47 H46 2002"),
            ("QC178 .Q37 2002EBEB", "QC178 .Q37 2002"),
            ("QA76.5 1977.M5217EB", "QA76.5 1977.M5217"),
            ("PZ7.A1 DEBEB", "PZ7.A1 DEB"),
            ("TX553.A3 W67A NO.60EB", "TX553.A3 W67A"),
            ("QP905 .H236S V.175EB", "QP905 .H236S"),
            ("PZ3.M8346 LO5EB", "PZ3.M8346 LO5"),
        ] {
            assert_eq!(super::lcc_item_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in [
            "EB",
            "WEB",
            "WB18.2 .A1 2001EB",
            "QA76 UNKNOWN EBOOK",
            "QA76 .A1 2001EB UNKNOWN",
            "QA76 .A1 VOL.2-EB",
            "QA76 . EBOOK",
            "QA76 .UNKNOWN EBOOK",
            "QA76 .A EBOOK",
            "QA76 .A1 2001UNKNOWN EBOOK",
            "QA76 .A1 2001 .UNKNOWN EBOOK",
            "QA76.5 197.M5217EB",
            "QA76.5 1977.UNKNOWN EBOOK",
            "TX553.A3 W67B NO.60EB",
            "TX553.A3 W67A UNKNOWN EBOOK",
            "QP905 .H236T V.175EB",
            "QP905 .H236S UNKNOWN EBOOK",
            "PZ3.M8346 LO6EB",
            "PZ3.M8346 LO5 UNKNOWN EBOOK",
            "DT159.944 S69 201 EB",
            "QA76.76.C672 044 2000EB",
        ] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn textbook_grade_suffix_preserves_subject_and_evidence() {
        for (code, base) in [("BF723.A4 S27 2005 GR.3-6", "BF723.A4 S27 2005"), ("E175.8 .H37 2007 GR.5", "E175.8 .H37 2007"), ("QA107 .A1 2005 GRADE 3", "QA107 .A1 2005"), ("QA107 .A1 2005 GR.K-2", "QA107 .A1 2005")] {
            assert_eq!(super::lcc_item_base(code), Some(base));
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["QA107 .A1 2005 GR.3-", "QA107 .A1 2005 GR.6-3", "QA107 .A1 2005 GR.97", "QA107 .A1 2005 GR.3 UNKNOWN", "QA107 .A1 GR.3", "WB18.2 .A1 2005 GR.3", "GR.3"] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn spaced_caption_period_preserves_evidence() {
        for (code, base) in [("AM101 .I374 NO .56", "AM101 .I374"), ("DA670.S97 S97 VOL .76", "DA670.S97 S97"), ("PG3199 .G526 V .25", "PG3199 .G526")] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base));
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["AM101 .I374 NO .", "DA670.S97 S97 VOL .76-", "PG3199 .G526 V .25 UNKNOWN", "PG3199 .G526 V ..25"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn serial_year_issue_preserves_dated_call_and_evidence() {
        for (code, base) in [("AM64 .R15 1974:43", "AM64 .R15 1974"), ("AS281 .A34 1978:51", "AS281 .A34 1978"), ("BF38 .H45 1975:4", "BF38 .H45 1975")] {
            assert_eq!(super::lcc_item_base(code), Some(base));
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["AM64 .R15 74:43", "AM64 .R15 1974:", "AM64 .R15 1974:43-", "AM64 .R15 1974:43 UNKNOWN", "AM64 .R15 1974:4:3", "AM64 1974:43", "WB18.2 .C1 1974:43", "KF1 .A1 1974:43"] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn additional_conser_captions_preserve_evidence() {
        for (code, base) in [
            ("AS613.S8 A14 JAARG. 1, AFL. 1", "AS613.S8 A14"),
            ("DC611.R437 .P48 1985 PTIE 3", "DC611.R437 .P48 1985"),
            ("PE1119.A2 B4 1989 LVL.4", "PE1119.A2 B4 1989"),
            ("LB1576 .W71 1996, LVL.4, V.6", "LB1576 .W71 1996"),
            ("AS142 .C445 ROč.101, SEš.2", "AS142 .C445"),
            ("AS142 .C445 ROC\u{30c}.101, SES\u{30c}.2", "AS142 .C445"),
        ] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in [
            "AS161 .B39 SéR. UNKNOWN",
            "AS613.S8 A14 AFL.1-",
            "DC611.R437 .P48 1985 PTIE",
            "AS142 .C445 ROč.101, SEš.2 UNKNOWN",
            "AS142 .C445 ROč.101, 76, SEš.2",
            "PE1119.A2 B4 1989 LVL.",
            "PE1119.A2 B4 1989 LVL.4-",
            "PE1119.A2 B4 1989 LVL.4 UNKNOWN",
        ] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn finnish_number_caption_preserves_evidence() {
        for code in ["B2391.F5 A36 N:O 84", "B2391.F5 A36 N:O84"] {
            let base = "B2391.F5 A36";
            assert_eq!(super::lcc_enumeration_base(code), Some(base));
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["B2391.F5 A36 N:O", "B2391.F5 A36 N:O 84-", "B2391.F5 A36 N:O UNKNOWN", "B2391.F5 A36 N:O 84 UNKNOWN", "B2391.F5 A36 N:O 84-85"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn workbook_series_and_volume_year_preserve_evidence() {
        for (code, base) in [
            ("BX1751.3 .I58 WORKBOOK", "BX1751.3 .I58"),
            ("PN4121 .L72 2004 STUDENT WORKBOOK", "PN4121 .L72 2004"),
            ("PE1128.A2 R43 1994 LVL 1, WORKBOOK", "PE1128.A2 R43 1994"),
            ("E178.1 .T67 1982 WORKBOOK TEACHER'S EDITION", "E178.1 .T67 1982"),
            ("GN31 .B7 WORKBOOK 1976A", "GN31 .B7"),
            ("PC2129.E5 S546 2007 WORKBOOK PART 2", "PC2129.E5 S546 2007"),
            ("QD251.3 .B78 2004 WORKBOOK", "QD251.3 .B78 2004"),
            ("BF637.H4 C37 1980, WORKBOOK", "BF637.H4 C37 1980"),
        ] {
            assert_eq!(super::lcc_item_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in [
            "BC25 VOL. 16 OF 04",
            "BC25 VOL. 16 OF 2004 UNKNOWN",
            "BC25 VOL. OF 2004",
            "BC25 VOL. 16- OF 2004",
            "N6841 .A9 NOUV. PéRIODEUNKNOWN",
            "N6841 .A9 NOUV. PéRIODE, T. 31-",
            "QD251.3 .B78 2004 WORKBOOK UNKNOWN",
            "WB18.2 .C1 2004 WORKBOOK",
            "BX1751.3 WORKBOOK",
            "BX1751.3 .I WORKBOOK",
            "LC149.7 .A65 V.9- WORKBOOK",
            "GN31 .B7 WORKBOOK 76",
            "GN31 .B7 WORKBOOK 1976 UNKNOWN",
            "PC2129.E5 S546 2007 WORKBOOK PART 2-",
            "E178.1 .T67 1982 WORKBOOK TEACHER'S EDITION UNKNOWN",
        ] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn explicit_folio_size_after_a_dated_call_preserves_evidence() {
        for (code, base) in [
            ("B74 P4414 2004 FOL.", "B74 P4414 2004"),
            ("BS575 .S52713 1994B FOL.", "BS575 .S52713 1994B"),
            ("CN1178.S5 F8 1997 FOLIO", "CN1178.S5 F8 1997"),
            ("F452 .A72 1982 <FOL>", "F452 .A72 1982"),
            ("NA6245.L92 L827 1994 <FOL.>", "NA6245.L92 L827 1994"),
            ("N6549.S5 A4 1981 FOL", "N6549.S5 A4 1981"),
            ("TA175 .P53 1989<FOL.>", "TA175 .P53 1989"),
            ("PZ7.V266 WI 1992 <FOL.>", "PZ7.V266 WI 1992"),
            ("N7615.4 .N36 FOL", "N7615.4 .N36"),
            ("B74 P4414 FOLIO", "B74 P4414"),
        ] {
            assert_eq!(super::lcc_item_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in [
            "B74 P4414 2004 FFFF",
            "PZ7.V266 FOLIO",
            "B74 P4414 2004 FOL.UNKNOWN",
            "B74 P4414 2004 FOLIO UNKNOWN",
            "FOLIO",
            "WB18.2 .C1 2004 FOLIO",
            "F452 <FOL>",
            "F452 .A72 1982 <FOL",
            "F452 .A72 1982 <FOL> UNKNOWN",
            "F452 .A72 1982 <UNKNOWN>",
            "WB18.2 .C1 2004 <FOL>",
        ] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn oversize_plus_after_a_complete_dated_call_preserves_evidence() {
        for (code, base) in [("AP95.P3 T393 2016+", "AP95.P3 T393 2016"), ("B753.F34 S245 2017+", "B753.F34 S245 2017"), ("M452 .B415 1989++", "M452 .B415 1989")] {
            assert_eq!(super::lcc_item_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["QA76+", "QA76.C1 20+", "QA76.C1 2017+++", "QA76 2017+", "QA76.C1 2017+ UNKNOWN", "LAW+", "WB18.2 .C1 2017+", "QA76.C1-QA77.C1 2017+"] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn shelving_lc_suffix_requires_a_complete_known_call() {
        for (code, base) in [("AC149 .H758 1989 (LC)", "AC149 .H758 1989"), ("B2799.A4 K63 2001 (LC)", "B2799.A4 K63 2001")] {
            assert_eq!(super::lcc_item_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["(LC)", "CPB (LC)", "LAW (LC)", "WB18.2 (LC)", "QA3 UNKNOWN (LC)", "QA3 .L28 VOL. 1- (LC)", "QA3 (LC) EXTRA"] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn additional_part_and_chapter_captions_preserve_original_evidence() {
        for (code, base) in [
            ("BS410 .Z7 BAND 236", "BS410 .Z7"),
            ("BS1154.2 .B5 BD. 14, TEIL 4", "BS1154.2 .B5"),
            ("B3240 .F524 2006X, ABT. 2, BD. 5, TEILBD. 2", "B3240 .F524 2006X"),
            ("BJ1661 .S25 1984, SEC.3, NO.4", "BJ1661 .S25 1984"),
            ("DS251 .S79 CAHIER 44", "DS251 .S79"),
        ] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["BS1154 .A6 TEILBD., 5", "BS1154 .A6 TEILBD. 22/", "BS1154 .A6 TEIL UNKNOWN", "BS1154 .A6 BAND", "BS1154 .A6 CH.UNKNOWN", "BS1154 .A6 SEC. 1ERCYCLE", "BS1154 .A6 CAHIER 44 UNKNOWN"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn explicit_copy_labels_preserve_classification_and_evidence() {
        for (code, base) in [("BS2585.3 .M37 COPY 2", "BS2585.3 .M37"), ("DD286.4 .M57 2007 COPY 1", "DD286.4 .M57 2007"), ("BS2545.P68 M377 COP. 3", "BS2545.P68 M377"), ("BV4211.2 .E382 1982 COP. 2, COP. 3", "BV4211.2 .E382 1982")] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["BS2585.3 .M37 COPY", "BS2585.3 .M37 COPY UNKNOWN", "BS2585.3 .M37 COPY 2 UNKNOWN", "BS2585.3 .M37 COP. 2,", "BS2585.3 .M37 COP. 2-", "BS2585.3 .M37 COPIED 2"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn documented_supplement_and_numbered_item_labels_preserve_evidence() {
        // Western Libraries' spine-label table documents supp., supl., Reihe,
        // book and unit. Only a complete captioned suffix is detached.
        for (code, base) in [("AC145 .I93 1982 SUPP.2 V.14", "AC145 .I93 1982"), ("B2967 .B6 1975 REIHE 2A BD. 12", "B2967 .B6 1975"), ("QA3 .L28 BOOK 2", "QA3 .L28"), ("D387 .R425 PT. 1, UNIT 1", "D387 .R425")] {
            assert_eq!(super::lcc_enumeration_base(code), Some(base), "{code}");
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
            assert_eq!(keys.last().unwrap(), code);
        }
        for code in ["QA3 .L28 SUPP.UNKNOWN", "QA3 .L28 SUPL.?", "QA3 .L28 REIHE UNKNOWN", "QA3 .L28 BOOK NOT YET IN LC", "QA3 .L28 UNIT", "QA3 .L28 UNIT 1 UNKNOWN", "BOOK NOT YET IN LC", "IN PROCESS UNIT 1"] {
            assert!(super::lcc_enumeration_base(code).is_none(), "{code}");
        }
    }

    #[test]
    fn semicolon_years_keep_the_complete_call_and_original_evidence() {
        for (code, base) in [("GR203.135 .K33;1998", "GR203.135 .K33"), ("Z1073 .P63;1999", "Z1073 .P63"), ("BF633 .D87; 1994", "BF633 .D87")] {
            assert_eq!(super::lcc_item_base(code), Some(base));
            let keys = super::lcc_match_keys(code).unwrap();
            assert_eq!(keys.last().unwrap(), code);
            assert_eq!(&keys[..keys.len() - 1], super::lcc_match_keys(base).unwrap());
        }
        for code in ["WB18.2;1998", "LAW;1998", "BF633 .D;1994", "BF633 .D87;", "BF633 .D87;19", "BF633 .D87;1994 EXTRA", "BF633 .D87;QA76", "QA75-;1998"] {
            assert!(super::lcc_item_base(code).is_none(), "{code}");
        }
    }
}
