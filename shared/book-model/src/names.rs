//! Personal names written the other way round.
//!
//! Authority files list people under their family name — "Yakobson, Alexander" —
//! and PDF `/Author` fields inherit the habit from whatever file passed through.
//! A reader wants the name as its owner writes it, and display order is what the
//! index sorts by, so the inverted form is rewritten up front and only the
//! reading-order name is ever stored.
//!
//! This lives here, below both ingestion and authority matching, because the
//! two used to answer the question separately: identity matching reordered
//! names to compare them while ingestion stored whatever the file said, so the
//! same person could match correctly and still be displayed backwards.

use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

/// "Yakobson, Alexander" → "Alexander Yakobson", and nothing otherwise.
///
/// Returns `None` — rather than guessing — when the value is not a two-part
/// personal name: one part, three or more parts (except a recognised suffix),
/// or a name that reads as an organisation. "Yakobson, Alexander, Gat, Azar" is
/// two people, and "Grose, Peter, 1934-" is a person and a life date; neither
/// can be reordered without deciding which, so neither is.
pub fn given_name_first(value: &str) -> Option<String> {
    if looks_like_organization(value) {
        return None;
    }
    let normalized = value.nfkc().collect::<String>();
    let parts = normalized.split(',').map(str::trim).collect::<Vec<_>>();
    match parts.as_slice() {
        [family, given] if !family.is_empty() && !given.is_empty() => Some(format!("{given} {family}")),
        [family, given, suffix] if !family.is_empty() && !given.is_empty() && recognized_suffix(suffix) => Some(format!("{given} {family} {suffix}")),
        _ => None,
    }
}

/// A generational suffix belongs to the person, so it survives the reordering
/// in the place it is read aloud: "King, Martin Luther, Jr." reads back as
/// "Martin Luther King Jr.".
pub fn recognized_suffix(value: &str) -> bool {
    matches!(comparison_key(value).as_str(), "jr" | "sr" | "ii" | "iii" | "iv")
}

/// An organisation has no given name to move, and reordering one produces
/// nonsense: "Ministry of Defence, UK" is not a person called UK.
pub fn looks_like_organization(value: &str) -> bool {
    if value.contains('&') {
        return true;
    }
    let key = comparison_key(value);
    key.split_whitespace().any(|word| matches!(word, "association" | "committee" | "company" | "corporation" | "institute" | "press" | "society" | "university" | "inc" | "llc" | "ltd" | "department" | "ministry" | "office" | "bureau"))
}

/// Letters and digits only, lowercased, with everything else standing in for a
/// single space. Used for comparing words, never for display.
pub fn comparison_key(value: &str) -> String {
    let mut key = String::new();
    let mut separator_pending = false;
    for character in value.nfkc() {
        if character.is_alphanumeric() || is_combining_mark(character) {
            if separator_pending && !key.is_empty() {
                key.push(' ');
            }
            separator_pending = false;
            key.extend(character.to_lowercase());
        } else {
            separator_pending = true;
        }
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_part_personal_name_is_reordered() {
        assert_eq!(given_name_first("Yakobson, Alexander").as_deref(), Some("Alexander Yakobson"));
        assert_eq!(given_name_first("Ekman, Kerstin").as_deref(), Some("Kerstin Ekman"));
        assert_eq!(given_name_first("Boyer, Paul S.").as_deref(), Some("Paul S. Boyer"));
    }

    #[test]
    fn a_generational_suffix_stays_with_the_person() {
        assert_eq!(given_name_first("King, Martin Luther, Jr.").as_deref(), Some("Martin Luther King Jr."));
    }

    #[test]
    fn nothing_that_is_not_plainly_one_person_is_touched() {
        assert_eq!(given_name_first("Robert D. Putnam"), None, "already given-name-first");
        assert_eq!(given_name_first("Yakobson, Alexander, Gat, Azar"), None, "two people");
        assert_eq!(given_name_first("Grose, Peter, 1934-"), None, "a life date, not a given name");
        assert_eq!(given_name_first("Oxford University Press, Oxford"), None, "an organisation");
        assert_eq!(given_name_first("Smith & Sons, Ltd"), None);
        assert_eq!(given_name_first(""), None);
        assert_eq!(given_name_first("Ekman,"), None, "half a name is not a name");
    }
}
