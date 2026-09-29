//! Bibliographic value types: publisher, language, identifier, contributor,
//! and other book metadata carried by synchronized book state.
//!
//! These types are validated data, not synchronization logic; the merge
//! policy that treats a whole [`BookMetadata`] value as a single
//! last-writer-wins register lives with the mutation protocol, not here.

use crate::{BookDate, BookSubject, Identifier, LanguageTag, MarcRelatorCode};
use serde::{Deserialize, Serialize};
use std::fmt;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

pub type AgentId = uuid::Uuid;
pub type AuthorId = AgentId;
pub type PublisherId = AgentId;
pub type ContributorId = AgentId;

pub fn agent_id_from_migration_seed(seed: &[u8]) -> AgentId {
    const NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([0x7f, 0xe7, 0x7e, 0x34, 0x29, 0x7f, 0x4a, 0xfa, 0x99, 0x8d, 0x2f, 0x86, 0xf0, 0x58, 0x7b, 0x31]);
    uuid::Uuid::new_v5(&NAMESPACE, seed)
}

/// A publisher spelling exactly as credited by a book. Equality is
/// deliberately distinct from publisher identity; [`PublisherName::match_key`]
/// supplies the conservative grouping key used by library indexes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PublisherName(String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PublisherMatchKey(String);

impl PublisherName {
    pub fn parse(value: impl AsRef<str>) -> Result<Self, BookMetadataError> {
        normalized_book_text(value.as_ref()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn match_key(&self) -> PublisherMatchKey {
        let normalized = self.0.nfkc().collect::<String>();
        let mut key = String::with_capacity(normalized.len());
        let mut separator_pending = false;
        for character in normalized.chars() {
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
        PublisherMatchKey(key)
    }
}

impl PublisherMatchKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PublisherName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// One exact publisher credit linked to a stable Bokheim agent. The credited
/// spelling remains book evidence; identity resolution can later add
/// authority identifiers without rewriting that evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublisherCredit {
    publisher_id: PublisherId,
    name: PublisherName,
}

impl PublisherCredit {
    #[cfg(feature = "random-ids")]
    pub fn new(name: impl AsRef<str>) -> Result<Self, BookMetadataError> {
        Self::with_id(PublisherId::new_v4(), name)
    }

    pub fn with_id(publisher_id: PublisherId, name: impl AsRef<str>) -> Result<Self, BookMetadataError> {
        Ok(Self { publisher_id, name: PublisherName::parse(name)? })
    }

    pub const fn publisher_id(&self) -> PublisherId {
        self.publisher_id
    }

    pub const fn agent_id(&self) -> AgentId {
        self.publisher_id
    }

    pub fn name(&self) -> &PublisherName {
        &self.name
    }
}

impl<'de> Deserialize<'de> for PublisherCredit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct EncodedPublisherCredit {
            publisher_id: PublisherId,
            name: PublisherName,
        }

        let encoded = EncodedPublisherCredit::deserialize(deserializer)?;
        Self::with_id(encoded.publisher_id, encoded.name.as_str()).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Contributor {
    contributor_id: ContributorId,
    name: String,
    role: MarcRelatorCode,
}

impl Contributor {
    #[cfg(feature = "random-ids")]
    pub fn new(name: impl AsRef<str>, role: MarcRelatorCode) -> Result<Self, BookMetadataError> {
        Self::with_id(ContributorId::new_v4(), name, role)
    }

    pub fn with_id(contributor_id: ContributorId, name: impl AsRef<str>, role: MarcRelatorCode) -> Result<Self, BookMetadataError> {
        Ok(Self { contributor_id, name: normalized_book_text(name.as_ref())?, role })
    }

    pub const fn contributor_id(&self) -> ContributorId {
        self.contributor_id
    }

    pub const fn agent_id(&self) -> AgentId {
        self.contributor_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn role_code(&self) -> MarcRelatorCode {
        self.role
    }
}

impl<'de> Deserialize<'de> for Contributor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct EncodedContributor {
            contributor_id: ContributorId,
            name: String,
            role: MarcRelatorCode,
        }

        let encoded = EncodedContributor::deserialize(deserializer)?;
        Self::with_id(encoded.contributor_id, encoded.name, encoded.role).map_err(serde::de::Error::custom)
    }
}

/// Bibliographic metadata that accompanies the title, author credits, and
/// description carried alongside it in synchronized book state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookMetadata {
    pub identifiers: Vec<Identifier>,
    pub publishers: Vec<PublisherCredit>,
    pub languages: Vec<LanguageTag>,
    pub dates: Vec<BookDate>,
    pub subjects: Vec<BookSubject>,
}

impl BookMetadata {
    /// Promote ISBNs embedded in uncontrolled subject metadata while preserving
    /// declarations and existing identifiers.
    pub fn infer_embedded_subject_isbns(&mut self) -> Result<(), BookMetadataError> {
        for subject in &self.subjects {
            if !subject.source().rsplit(':').next().is_some_and(|source| source.eq_ignore_ascii_case("subject")) || subject.authority().is_some_and(|authority| !authority.eq_ignore_ascii_case("isbn")) {
                continue;
            }
            for isbn in crate::from_text(subject.name()) {
                crate::push_isbn(&mut self.identifiers, isbn, crate::Scope::Book)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BookRecord {
    pub title: String,
    pub subtitle: Option<String>,
    pub contributors: Vec<Contributor>,
    pub description: String,
    pub book: BookMetadata,
}

impl BookRecord {
    pub fn authors(&self) -> impl Iterator<Item = &Contributor> {
        self.contributors.iter().filter(|contributor| contributor.role_code() == MarcRelatorCode(*b"aut"))
    }
}

/// One entry in a book's own hierarchical navigation document. It is
/// shared by library ingestion and library inspection; UI contracts only
/// project it at their process boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookTocEntry {
    pub title: String,
    pub target: String,
    pub children: Vec<BookTocEntry>,
}

/// A synchronized navigation document: the book's own entries, plus an
/// audiobook's total duration when the writer knows it. The duration is what
/// turns the last chapter's start into its end, so a replica that only
/// receives entries can show navigation but cannot play the book.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudiobookTrack {
    pub name: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub offset: u64,
    pub length: u64,
    /// ZIP revision whose stored byte offsets this track index describes.
    #[serde(default)]
    pub archive_checksum: Option<content_address::ContentHash>,
}

impl AudiobookTrack {
    pub fn valid(&self) -> bool {
        !self.name.is_empty()
            && self.name.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'))
            && self.name.to_ascii_lowercase().ends_with(".mp3")
            && self.end_ms > self.start_ms
            && self.length > 0
            && self.offset.checked_add(self.length).is_some()
    }
}

pub fn tracks_match_archive(tracks: &[AudiobookTrack], checksum: content_address::ContentHash) -> bool {
    !tracks.is_empty() && tracks.iter().all(|track| track.valid() && track.archive_checksum == Some(checksum))
}

/// Complete value of the navigation register. Replacement includes absence:
/// `None` clears a prior duration or track index; empty entries clear the TOC.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookTocDocument {
    pub entries: Vec<BookTocEntry>,
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub tracks: Option<Vec<AudiobookTrack>>,
}

impl BookTocDocument {
    pub fn entries(entries: Vec<BookTocEntry>) -> Self {
        Self { entries, duration_ms: None, tracks: None }
    }
}

#[cfg(test)]
mod toc_document_tests {
    use super::*;

    #[test]
    fn track_index_only_matches_the_archive_revision_it_describes() {
        let first = content_address::ContentHash::new(&"a".repeat(64));
        let second = content_address::ContentHash::new(&"b".repeat(64));
        let track = AudiobookTrack { name: "01.mp3".into(), start_ms: 0, end_ms: 1000, offset: 42, length: 100, archive_checksum: Some(first) };
        assert!(tracks_match_archive(&[track.clone()], first));
        assert!(!tracks_match_archive(&[track.clone()], second));
        assert!(!tracks_match_archive(&[AudiobookTrack { archive_checksum: None, ..track }], first));
    }

    fn entry(title: &str, target: &str) -> BookTocEntry {
        BookTocEntry { title: title.into(), target: target.into(), children: Vec::new() }
    }

    #[test]
    fn document_round_trips_with_and_without_duration() {
        let without_duration = BookTocDocument::entries(vec![entry("One", "audiobook-position-ms:0")]);
        assert_eq!(serde_json::from_str::<BookTocDocument>(&serde_json::to_string(&without_duration).unwrap()).unwrap(), without_duration);
        let with_duration = BookTocDocument { entries: vec![entry("One", "audiobook-position-ms:0")], duration_ms: Some(1000), tracks: None };
        assert_eq!(serde_json::from_str::<BookTocDocument>(&serde_json::to_string(&with_duration).unwrap()).unwrap(), with_duration);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookMetadataError;

impl fmt::Display for BookMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("book metadata must contain bounded valid text")
    }
}

impl std::error::Error for BookMetadataError {}

impl From<crate::IdentifierError> for BookMetadataError {
    fn from(_: crate::IdentifierError) -> Self {
        Self
    }
}

impl From<crate::CollectionError> for BookMetadataError {
    fn from(_: crate::CollectionError) -> Self {
        Self
    }
}

impl From<crate::SubjectError> for BookMetadataError {
    fn from(_: crate::SubjectError) -> Self {
        Self
    }
}

impl From<crate::DateError> for BookMetadataError {
    fn from(_: crate::DateError) -> Self {
        Self
    }
}

impl From<crate::ValueError> for BookMetadataError {
    fn from(_: crate::ValueError) -> Self {
        Self
    }
}

impl From<crate::LanguageTagError> for BookMetadataError {
    fn from(_: crate::LanguageTagError) -> Self {
        Self
    }
}

const MAX_BOOK_TEXT_BYTES: usize = 4096;

fn bounded_book_text(value: &str) -> Result<String, BookMetadataError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_BOOK_TEXT_BYTES || value.chars().any(char::is_control) {
        return Err(BookMetadataError);
    }
    Ok(value.to_owned())
}

fn normalized_book_text(value: &str) -> Result<String, BookMetadataError> {
    let value = bounded_book_text(value)?;
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (normalized.len() <= MAX_BOOK_TEXT_BYTES).then_some(normalized).ok_or(BookMetadataError)
}
