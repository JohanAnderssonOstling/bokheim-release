//! Speech-only normalization; document text and highlighting remain untouched.
use segment_rules::abbreviations::{ENGLISH_POLICY, is_proper_name_token, is_reference_token};
use segment_rules::char_class::{contains_line_break, is_closing_enclosure};
use segment_rules::{LexicalKind, lexemes};
use std::borrow::Cow;
use std::ops::Range;

#[path = "speech_text/months.rs"]
mod months;
use months::normalize_months;
#[path = "speech_text/semantics.rs"]
mod semantics;
use semantics::{normalize_number_words, normalize_semantics};
#[path = "speech_text/calendar.rs"]
mod calendar;
#[path = "speech_text/decimals.rs"]
mod decimals;
#[path = "speech_text/dimensions.rs"]
mod dimensions;
#[path = "speech_text/entities.rs"]
mod entities;
#[path = "speech_text/fractions.rs"]
mod fractions;
#[path = "speech_text/numbers.rs"]
mod numbers;
#[path = "speech_text/quantity_abbreviations.rs"]
mod quantity_abbreviations;
#[path = "speech_text/ratios.rs"]
mod ratios;
#[path = "speech_text/scientific.rs"]
mod scientific;
#[cfg(test)]
#[path = "speech_text/tests.rs"]
mod tests;
#[path = "speech_text/times.rs"]
mod times;

const ROMAN_DIGITS: [(&str, u16); 13] = [("M", 1000), ("CM", 900), ("D", 500), ("CD", 400), ("C", 100), ("XC", 90), ("L", 50), ("XL", 40), ("X", 10), ("IX", 9), ("V", 5), ("IV", 4), ("I", 1)];

/// Preserve the original allocation until a rule actually changes its text.
struct TextEdits<'a> {
    text: &'a str,
    result: Option<String>,
    cursor: usize,
}

impl<'a> TextEdits<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, result: None, cursor: 0 }
    }

    fn replace(&mut self, span: Range<usize>, replacement: &str) {
        if &self.text[span.clone()] == replacement {
            return;
        }
        let result = self.result.get_or_insert_with(|| String::with_capacity(self.text.len()));
        result.push_str(&self.text[self.cursor..span.start]);
        result.push_str(replacement);
        self.cursor = span.end;
    }

    fn finish(self) -> Cow<'a, str> {
        match self.result {
            Some(mut result) => {
                result.push_str(&self.text[self.cursor..]);
                Cow::Owned(result)
            }
            None => Cow::Borrowed(self.text),
        }
    }
}

/// Stages borrow their whole input when unchanged. Retain an already owned
/// result rather than copying it again for the next unchanged stage.
fn normalize_step<'a>(text: Cow<'a, str>, normalize: impl for<'b> Fn(&'b str) -> Cow<'b, str>) -> Cow<'a, str> {
    match text {
        Cow::Borrowed(text) => normalize(text),
        Cow::Owned(text) => match normalize(&text) {
            Cow::Borrowed(_) => Cow::Owned(text),
            Cow::Owned(result) => Cow::Owned(result),
        },
    }
}

fn roman_number(word: &str) -> Option<u16> {
    if word.is_empty() || word.len() > 15 {
        return None;
    }
    let mut value = 0_u16;
    let mut previous = 0;
    for character in word.bytes().rev() {
        let digit = match character {
            b'I' => 1,
            b'V' => 5,
            b'X' => 10,
            b'L' => 50,
            b'C' => 100,
            b'D' => 500,
            b'M' => 1000,
            _ => return None,
        };
        if digit < previous {
            value -= digit;
        } else {
            value += digit;
        }
        previous = digit;
    }
    if !(1..=3999).contains(&value) {
        return None;
    }
    // Reject words and malformed spellings such as CIVIL, IIX, or VX.
    let mut remainder = value;
    let mut canonical = String::new();
    for (symbol, digit) in ROMAN_DIGITS {
        canonical.push_str(&symbol.repeat(usize::from(remainder / digit)));
        remainder %= digit;
    }
    (canonical == word).then_some(value)
}

fn normalize_reading_labels(text: &str) -> Cow<'_, str> {
    let mut result = TextEdits::new(text);
    let mut previous_word: Option<&str> = None;
    // I.M. Pei and C.S. Lewis are names, rather than numbered headings.
    let starts_with_initials = words(text).nth(1).is_some_and(|word| dotted_initial(text, &word));
    let mut offset = 0;
    for piece in text.split_inclusive(|character: char| !character.is_alphanumeric()) {
        let start = offset;
        offset += piece.len();
        let word = piece.trim_end_matches(|character: char| !character.is_alphanumeric());
        if word.is_empty() {
            continue;
        }
        let suffix = &piece[word.len()..];
        let after_label = previous_word.is_some_and(|previous| {
            ["chapter", "ch", "chapt", "book", "bk", "part", "pt", "volume", "vol", "vols", "section", "sec", "figure", "fig", "act", "scene", "appendix", "appx", "article", "art", "paragraph", "par"]
                .iter()
                .any(|label| previous.eq_ignore_ascii_case(label))
        });
        let uppercase = word.bytes().all(|character| character.is_ascii_uppercase());
        let isolated_label = word != "I" && text.trim() == word;
        let numbered_heading = previous_word.is_none() && word.len() > 1 && matches!(suffix, "." | ")") && !starts_with_initials;
        // Uppercase labels avoid ordinary words such as "Part mix"; single
        // dotted letters at the start of prose retain their initial reading.
        let number = (uppercase && (after_label || isolated_label || numbered_heading)).then(|| roman_number(word)).flatten();
        if let Some(number) = number {
            result.replace(start..start + word.len(), &number.to_string());
        }
        previous_word = Some(word);
    }
    result.finish()
}

struct Word<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

fn words(text: &str) -> impl Iterator<Item = Word<'_>> {
    lexemes(text).filter(|lexeme| matches!(lexeme.kind(), LexicalKind::Word | LexicalKind::Numeric)).map(|lexeme| {
        let span = lexeme.span();
        Word { text: lexeme.text(), start: span.start, end: span.end }
    })
}

fn dotted_initial(text: &str, word: &Word<'_>) -> bool {
    if word.text.len() != 1 || !word.text.as_bytes()[0].is_ascii_uppercase() {
        return false;
    }
    let Some(tail) = text[word.end..].strip_prefix('.') else { return false };
    if tail.starts_with('.') {
        return false;
    }
    // Allow adjacent dotted initials (J.R.R.), but leave domains and decimal
    // suffixes such as B.com and B.2 intact.
    tail.chars().next().is_none_or(|character| !character.is_alphanumeric() || (character.is_ascii_uppercase() && tail.as_bytes().get(1) == Some(&b'.')))
}

/// Keep the final period when an abbreviation ends a sentence. An internal
/// period disappears, but commas, quotes, whitespace, and other punctuation
/// remain in the original text around the replacement.
fn abbreviation_end(text: &str, period: usize, name_continues: bool) -> usize {
    let next = text[period + 1..].trim_start_matches(|character: char| character.is_whitespace() || is_closing_enclosure(character)).chars().next();
    if name_continues || next.is_some_and(|character| character.is_lowercase() || character.is_numeric() || matches!(character, ',' | ';' | ':' | '?' | '!' | '.')) { period + 1 } else { period }
}

fn reference_continues(text: &str, word: &Word<'_>, next: Option<&Word<'_>>) -> bool {
    next.is_some_and(|next| {
        let gap = &text[word.end..next.start];
        let reference = is_reference_token(next.text) || (next.text.len() == 1 && next.text.as_bytes()[0].is_ascii_uppercase());
        reference && gap.chars().all(|character| character.is_whitespace() || character == '.') && !text[next.end..].chars().next().is_some_and(char::is_alphanumeric)
    })
}

fn single_abbreviation(word: &str, next_name: bool, previous_name: bool, reference: bool) -> Option<&'static str> {
    match word.to_ascii_lowercase().as_str() {
        "mr" => Some("Mister"),
        "mrs" => Some("Missus"),
        "ms" => Some("Miz"),
        "mx" => Some("Mix"),
        "dr" if next_name => Some("Doctor"),
        "prof" => Some("Professor"),
        "asst" => Some("Assistant"),
        "esq" => Some("Esquire"),
        "messrs" => Some("Misters"),
        "rev" | "revd" if next_name => Some("Reverend"),
        "hon" if next_name => Some("Honorable"),
        "gen" if next_name => Some("General"),
        "capt" | "cpt" if next_name => Some("Captain"),
        "col" if next_name => Some("Colonel"),
        "maj" if next_name => Some("Major"),
        "lt" if next_name => Some("Lieutenant"),
        "sgt" | "sergt" if next_name => Some("Sergeant"),
        "pvt" if next_name => Some("Private"),
        "gov" if next_name => Some("Governor"),
        "sen" if next_name => Some("Senator"),
        "pres" if next_name => Some("President"),
        "jr" if previous_name => Some("Junior"),
        "sr" if previous_name => Some("Senior"),
        "senr" => Some("Senior"),
        "etc" => Some("et cetera"),
        "vs" => Some("versus"),
        "cf" => Some("compare"),
        "ibid" => Some("in the same source"),
        "approx" => Some("approximately"),
        "incl" => Some("including"),
        "excl" => Some("excluding"),
        "dept" => Some("department"),
        "misc" => Some("miscellaneous"),
        "intl" => Some("international"),
        "qty" => Some("quantity"),
        "acct" => Some("account"),
        "mfg" => Some("manufacturing"),
        "mfr" => Some("manufacturer"),
        "encl" => Some("enclosure"),
        "estd" => Some("established"),
        "regd" => Some("registered"),
        "advt" => Some("advertisement"),
        "aftn" => Some("afternoon"),
        "incog" => Some("incognito"),
        "retd" => Some("retired"),
        "orig" => Some("original"),
        "suppl" => Some("supplement"),
        "viz" => Some("namely"),
        "ans" => Some("answer"),
        "prev" => Some("previous"),
        "glos" => Some("glossary"),
        "outl" => Some("outline"),
        "tgm" => Some("telegram"),
        "mss" => Some("manuscripts"),
        "engl" => Some("English"),
        "fahr" => Some("Fahrenheit"),
        "spp" => Some("species"),
        "ssp" | "subsp" => Some("subspecies"),
        "univ" => Some("University"),
        "bros" if previous_name => Some("Brothers"),
        // Clear geographic spellings still obey the catalog's capitalization.
        // Word/name collisions such as Wash., Mass., Miss., Ind., Penn.,
        // Conn., Tex., Ark., and Ore.
        // need more evidence and deliberately have no spoken expansion here.
        "ariz" => Some("Arizona"),
        "calif" => Some("California"),
        "fla" => Some("Florida"),
        "mich" => Some("Michigan"),
        "minn" => Some("Minnesota"),
        "neb" | "nebr" => Some("Nebraska"),
        "nev" => Some("Nevada"),
        "okla" => Some("Oklahoma"),
        "oreg" => Some("Oregon"),
        "penns" => Some("Pennsylvania"),
        "tenn" => Some("Tennessee"),
        "va" => Some("Virginia"),
        "wis" => Some("Wisconsin"),
        "wy" | "wyo" => Some("Wyoming"),
        "addr" => Some("address"),
        "amt" => Some("amount"),
        "attn" => Some("attention"),
        "abbr" => Some("abbreviation"),
        "cont" | "contd" => Some("continued"),
        "bldg" => Some("building"),
        "tel" => Some("telephone"),
        "resp" => Some("respectively"),
        "mt" if next_name => Some("Mount"),
        "ave" if previous_name => Some("Avenue"),
        "rd" if previous_name => Some("Road"),
        "blvd" if previous_name => Some("Boulevard"),
        "co" if previous_name => Some("Company"),
        "corp" if previous_name => Some("Corporation"),
        "inc" if previous_name => Some("Incorporated"),
        "ltd" if previous_name => Some("Limited"),
        "fig" if reference => Some("figure"),
        "vol" if reference => Some("volume"),
        "vols" if reference => Some("volumes"),
        "ch" | "chapt" if reference => Some("chapter"),
        "sec" if reference => Some("section"),
        "ref" if reference => Some("reference"),
        "no" if reference => Some("number"),
        "nos" if reference => Some("numbers"),
        "appx" if reference => Some("appendix"),
        "p" | "pg" if reference => Some("page"),
        "pp" if reference => Some("pages"),
        "art" if reference => Some("article"),
        "par" if reference => Some("paragraph"),
        "bk" if reference => Some("book"),
        "pt" if reference => Some("part"),
        "ed" if reference => Some("edition"),
        // Catalog recognition controls segmentation, not meaning. Forms such
        // as Pref. (preface/preference), phys. (physics/physical/physician),
        // app. (appendix/application), and St. (saint/street) need stronger
        // context. Grammar-dependent endings such as biol. also stay intact.
        _ => None,
    }
}

fn normalize_abbreviations(text: &str) -> Cow<'_, str> {
    if !text.contains('.') {
        return Cow::Borrowed(text);
    }
    let mut words = words(text).peekable();
    let mut result = TextEdits::new(text);
    let mut previous = None;
    while let Some(word) = words.next() {
        if word.start < result.cursor {
            previous = Some(word);
            continue;
        }
        let next = words.peek();
        // Whitespace separates a name from its initial. A neighboring dotted
        // initial can also be adjacent, as in J.R.R. Tolkien.
        let next_name = next.is_some_and(|next| {
            let gap = &text[word.end..next.start];
            (is_proper_name_token(next.text) || dotted_initial(text, next)) && gap.chars().all(|character| character.is_whitespace() || character == '.')
        });
        let previous_name = previous.as_ref().is_some_and(|previous: &Word<'_>| {
            let gap = &text[previous.end..word.start];
            (is_proper_name_token(previous.text) || dotted_initial(text, previous)) && gap.chars().all(|character| character.is_whitespace() || character == '.')
        });
        let mut replacement = None;
        for (abbreviation, expansion) in [("e.g.", "for example"), ("i.e.", "that is"), ("n.b.", "note well"), ("p.s.", "postscript"), ("ph.d.", "Doctor of Philosophy"), ("et al.", "and others")] {
            let end = word.start + abbreviation.len();
            if text.get(word.start..end).is_some_and(|candidate| candidate.eq_ignore_ascii_case(abbreviation) && ENGLISH_POLICY.recognizes(text, word.start..end)) && !text[end..].chars().next().is_some_and(char::is_alphanumeric) {
                replacement = Some((expansion, abbreviation_end(text, end - 1, false)));
                break;
            }
        }
        if replacement.is_none() && dotted_initial(text, &word) {
            // Keep the initial itself and let the voice pronounce it.
            replacement = Some((word.text, word.end + 1));
        }
        let reference = reference_continues(text, &word, next);
        let title_continues = next_name
            && matches!(
                word.text.to_ascii_lowercase().as_str(),
                "mr" | "mrs" | "ms" | "mx" | "dr" | "prof" | "asst" | "messrs" | "rev" | "revd" | "hon" | "gen" | "capt" | "cpt" | "col" | "maj" | "lt" | "sgt" | "sergt" | "pvt" | "gov" | "sen" | "pres" | "mt"
            );
        let period_boundary = text[word.end..].strip_prefix('.').is_some_and(|tail| !tail.starts_with('.') && (!tail.chars().next().is_some_and(char::is_alphanumeric) || reference || title_continues));
        if replacement.is_none() && period_boundary && (ENGLISH_POLICY.recognizes(text, word.start..word.end + 1) || (title_continues && ENGLISH_POLICY.contains(&text[word.start..word.end + 1])) || word.text.eq_ignore_ascii_case("p")) {
            let expansion = single_abbreviation(word.text, next_name, previous_name, reference);
            if let Some(expansion) = expansion {
                replacement = Some((expansion, abbreviation_end(text, word.end, title_continues || reference)));
            }
        }
        if let Some((expansion, end)) = replacement {
            result.replace(word.start..end, expansion);
            if next.is_some_and(|next| next.start == end) {
                result.replace(end..end, " ");
            }
        }
        previous = Some(word);
    }
    result.finish()
}

fn normalize_layout(text: &str) -> Cow<'_, str> {
    if !text.contains(['\0', '\u{00ad}', 'ﬀ', 'ﬁ', 'ﬂ', 'ﬃ', 'ﬄ']) {
        return Cow::Borrowed(text);
    }
    let mut result = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\0' => {}
            '\u{00ad}' => {
                // Only an explicit discretionary hyphen licenses joining across
                // a line break. Ordinary hyphens and paragraph breaks remain.
                let whitespace = characters.clone().take_while(|character| character.is_whitespace()).collect::<String>();
                let next = characters.clone().find(|character| !character.is_whitespace());
                let line_breaks = whitespace.replace("\r\n", "\n").chars().filter(|character| matches!(character, '\r' | '\n')).count();
                if contains_line_break(&whitespace) && line_breaks <= 1 && result.chars().next_back().is_some_and(char::is_alphabetic) && next.is_some_and(char::is_lowercase) {
                    for _ in whitespace.chars() {
                        characters.next();
                    }
                }
            }
            'ﬀ' => result.push_str("ff"),
            'ﬁ' => result.push_str("fi"),
            'ﬂ' => result.push_str("fl"),
            'ﬃ' => result.push_str("ffi"),
            'ﬄ' => result.push_str("ffl"),
            _ => result.push(character),
        }
    }
    Cow::Owned(result)
}

fn normalize_asterisks(text: &str) -> Cow<'_, str> {
    let mut result = TextEdits::new(text);
    let mut asterisks = text.match_indices('*').peekable();
    while let Some((start, _)) = asterisks.next() {
        let mut end = start + 1;
        while asterisks.peek().is_some_and(|(index, _)| *index == end) {
            asterisks.next();
            end += 1;
        }
        // Silence footnote and emphasis markers without joining adjacent words.
        let separates_words = text[..start].chars().next_back().is_some_and(char::is_alphanumeric) && text[end..].chars().next().is_some_and(char::is_alphanumeric);
        result.replace(start..end, if separates_words { " " } else { "" });
    }
    result.finish()
}

fn normalize_prose(text: &str) -> Cow<'_, str> {
    let text = normalize_asterisks(text);
    let text = normalize_step(text, normalize_reading_labels);
    let text = normalize_step(text, normalize_months);
    let text = normalize_step(text, normalize_abbreviations);
    normalize_step(text, |text| {
        let mut result = TextEdits::new(text);
        for (start, dash) in text.match_indices('—') {
            result.replace(start..start + dash.len(), " ");
        }
        result.finish()
    })
}

pub(super) fn prepare_speech_text(text: &str) -> Cow<'_, str> {
    let text = normalize_step(normalize_layout(text), |text| normalize_semantics(text, normalize_prose));
    normalize_step(text, normalize_number_words)
}
