use book_model::{BookFormat, BookMetadata, BookRecord, Contributor, ReadingPosition, UnixMillis};
use serde::{Deserialize, Serialize};

use unicode_normalization::UnicodeNormalization;
#[path = "lowercase_v1.rs"]
mod lowercase_v1;

/// Portable sibling-name identity used for folders and materialized files.
/// Display casing and accents are retained separately; comparison uses
/// compatibility normalization plus locale-independent lowercase mapping.
pub fn portable_name_key(value: &str) -> String {
    let mut key = String::with_capacity(value.len());
    for character in value.nfkc() {
        match lowercase_v1::LOWERCASE.binary_search_by_key(&character, |(upper, _)| *upper) {
            Ok(index) => key.push_str(lowercase_v1::LOWERCASE[index].1),
            Err(_) => key.push(character),
        }
    }
    key
}

/// The metadata sync register. Descriptions belong to a different register,
/// so this type cannot represent a description update or accidental clear.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncBookMetadata {
    pub title: String,
    pub subtitle: Option<String>,
    pub contributors: Vec<Contributor>,
    pub book: BookMetadata,
}

impl SyncBookMetadata {
    pub fn subtitle(&self) -> Option<&str> {
        self.subtitle.as_deref()
    }
}

impl From<BookRecord> for SyncBookMetadata {
    fn from(value: BookRecord) -> Self {
        Self { title: value.title, subtitle: value.subtitle, contributors: value.contributors, book: value.book }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BookLifecycleState {
    Present,
    /// Hidden from the active library, but retained in Trash with its assets
    /// and reading state intact.
    Deleted {
        /// Set only when deleting a folder removed the final active placement.
        /// This lets any replica restore exactly the books orphaned by that
        /// folder, without reviving books trashed independently beforehand.
        origin_folder_id: Option<String>,
    },
    /// Permanently removed. Replicas retain canonical registers so an
    /// older offline `Present` value cannot resurrect the book.
    Purged,
}

/// Facts needed to reconstruct a book even when its current lifecycle is deleted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BookFacts {
    pub added_at: UnixMillis,
    pub format: BookFormat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryLifecycleState {
    Present,
    Deleted,
    Purged,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReadingPositionState {
    pub location: ReadingPosition,
    pub progress: f32,
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn projection_v1_unicode_name_keys() {
        for (input, expected) in [("ＡＢＣ", "abc"), ("Kelvin", "kelvin"), ("İ", "i\u{307}"), ("Σς", "σς"), ("ﬃ", "ffi"), ("A\u{30a}", "å"), ("Straße", "straße")] {
            assert_eq!(portable_name_key(input), expected);
        }
        assert_ne!(portable_name_key("Straße"), portable_name_key("STRASSE"));
    }
}
