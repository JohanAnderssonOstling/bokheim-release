use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct AuthorName(String);

/// Stable, locale-independent key used to compare author credits.
/// It groups spelling variants, but deliberately does not claim that two
/// matching names identify the same real person.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AuthorNameMatchKey(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorNameError;

impl std::fmt::Display for AuthorNameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("author name must be non-empty, bounded, and contain no control characters")
    }
}

impl std::error::Error for AuthorNameError {}

impl AuthorName {
    pub fn parse(value: &str) -> Result<Self, AuthorNameError> {
        let value = value.trim();
        if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
            Err(AuthorNameError)
        } else {
            Ok(Self(value.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn match_key(&self) -> AuthorNameMatchKey {
        AuthorNameMatchKey(normalized_author_match_key(&self.0))
    }
}

/// Through `parse`, so a name that crossed a process boundary carries the same
/// guarantee as one built locally rather than arriving as an unchecked string.
impl<'de> Deserialize<'de> for AuthorName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl AuthorNameMatchKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn normalized_author_match_key(value: &str) -> String {
    let normalized = value.nfkc().collect::<String>();
    let reordered = comma_reordered_name(&normalized);
    let reordered = reordered.as_deref().unwrap_or(normalized.as_str());
    normalized_author_match_key_without_reordering(reordered)
}

/// Reordering lives alongside identity matching so that a name matched as one
/// person must not then be displayed backwards.
fn comma_reordered_name(value: &str) -> Option<String> {
    crate::given_name_first(value)
}

/// The same key the rest of book-model uses, so a name compared here and a
/// name compared elsewhere cannot disagree.
fn normalized_author_match_key_without_reordering(value: &str) -> String {
    crate::comparison_key(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> String {
        AuthorName::parse(value).unwrap().match_key().as_str().to_owned()
    }

    #[test]
    fn author_match_key_normalizes_common_variants() {
        assert_eq!(key(" J.R.R.   Tolkien "), key("j. r. r. tolkien"));
        assert_eq!(key("Tolkien, J. R. R."), key("J.R.R. Tolkien"));
        assert_eq!(key("Le Guin, Ursula K."), key("Ursula K. Le Guin"));
        assert_eq!(key("Smith, John, Jr."), key("John Smith Jr."));
        assert_eq!(key("Jose\u{301} Saramago"), key("Jos\u{e9} Saramago"));
        assert_ne!(key("Garc\u{ed}a M\u{e1}rquez"), key("Garcia Marquez"));
    }

    #[test]
    fn author_match_key_does_not_infer_unmarked_or_organizational_name_order() {
        assert_ne!(key("Mao Zedong"), key("Zedong Mao"));
        assert_ne!(key("Smith, Jones & Company"), key("Jones Company Smith"));
        assert_eq!(key("王小明"), "王小明");
    }
}
