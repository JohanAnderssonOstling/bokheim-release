//! Bibliographic value types: publisher, language, identifier, contributor,
//! and other book metadata carried by synchronized book state.
//!
//! These types are validated data, not synchronization logic; the merge
//! policy that treats a whole [`BookMetadata`] value as a single
//! last-writer-wins register lives with the mutation protocol, not here.

use crate::{BookSubject, Identifier, LanguageTag, MarcRelatorCode};
use crate::{IdentifierError, LanguageTagError, SubjectError};
use content_address::BookId;
use serde::{Deserialize, Serialize};
use std::fmt;

pub type AgentId = uuid::Uuid;
pub type AuthorId = AgentId;
pub type ContributorId = AgentId;

pub fn agent_id_from_migration_seed(seed: &[u8]) -> AgentId {
    const NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([0x7f, 0xe7, 0x7e, 0x34, 0x29, 0x7f, 0x4a, 0xfa, 0x99, 0x8d, 0x2f, 0x86, 0xf0, 0x58, 0x7b, 0x31]);
    uuid::Uuid::new_v5(&NAMESPACE, seed)
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
    pub publishers: Vec<String>,
    pub languages: Vec<LanguageTag>,
    /// Publication year, when known.
    pub year: Option<i32>,
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

/// Book metadata shared by inspection, library state, and synchronization.
/// Description is a separate value and is never cleared by a metadata update.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BookDetails {
    pub title: String,
    pub subtitle: Option<String>,
    pub contributors: Vec<Contributor>,
    pub book: BookMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookRecord {
    pub metadata: BookDetails,
    pub description: String,
}

impl std::ops::Deref for BookRecord {
    type Target = BookDetails;
    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}
impl std::ops::DerefMut for BookRecord {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.metadata
    }
}

impl BookDetails {
    pub fn authors(&self) -> impl Iterator<Item = &Contributor> {
        self.contributors.iter().filter(|contributor| contributor.role_code() == MarcRelatorCode(*b"aut"))
    }
}

// Preserve the existing flat JSON and positional binary formats. These adapters
// borrow or deserialize the canonical fields; they do not define another value model.
impl Serialize for BookRecord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !serializer.is_human_readable() {
            return (&self.title, &self.subtitle, &self.contributors, &self.description, &self.book).serialize(serializer);
        }
        #[derive(Serialize)]
        struct Fields<'a> {
            #[serde(flatten)]
            metadata: &'a BookDetails,
            description: &'a str,
        }
        Fields { metadata: &self.metadata, description: &self.description }.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for BookRecord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if !deserializer.is_human_readable() {
            let (title, subtitle, contributors, description, book) = Deserialize::deserialize(deserializer)?;
            return Ok(Self { metadata: BookDetails { title, subtitle, contributors, book }, description });
        }
        #[derive(Deserialize)]
        struct Fields {
            #[serde(flatten)]
            metadata: BookDetails,
            description: String,
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self { metadata: fields.metadata, description: fields.description })
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
    /// Book identity whose ZIP offsets this track index describes. Provider
    /// readers also pin the file revision so replacements cannot reuse offsets.
    #[serde(default)]
    pub archive_hash: Option<BookId>,
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

pub fn tracks_match_archive(tracks: &[AudiobookTrack], hash: BookId) -> bool {
    !tracks.is_empty() && tracks.iter().all(|track| track.valid() && track.archive_hash == Some(hash))
}

/// Complete value of the navigation register. Replacement includes absence:
/// `None` clears a prior duration or track index; empty entries clear the TOC.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookTocDocument {
    #[serde(with = "crate::toc_transport")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookMetadataError;

impl fmt::Display for BookMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("book metadata must contain bounded valid text")
    }
}

impl std::error::Error for BookMetadataError {}

impl From<IdentifierError> for BookMetadataError {
    fn from(_: IdentifierError) -> Self {
        Self
    }
}

impl From<SubjectError> for BookMetadataError {
    fn from(_: SubjectError) -> Self {
        Self
    }
}

impl From<LanguageTagError> for BookMetadataError {
    fn from(_: LanguageTagError) -> Self {
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
