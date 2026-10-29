//! Bibliographic values shared by local-library metadata.

mod annotations;
mod audiobook;
mod book_format;
mod canonical;
mod chapter_read;
mod credits;
mod identifier;
mod language;
mod marc;
mod names;
mod reading;
pub mod record_transport;
mod search_text;
mod subject;
mod titles;
mod toc_target;
pub mod toc_transport;
pub use toc_target::{cbz_toc_page, cbz_toc_target};
mod validation;
mod year;

pub use annotations::{Annotation, AnnotationAnchor, AnnotationStyle, PdfAnnotationAnchor, PdfAnnotationRect, EXTERNAL_PDF_ANNOTATION_ID_PREFIX};
pub use audiobook::{AudiobookChapter, ChapterOrigin};
pub use book_format::BookFormat;
pub use canonical::{canonical_doi, canonical_isbn, canonical_uuid, from_content_candidate, from_text};
pub use chapter_read::ChapterReadStatus;
pub use content_address::BookId;
pub use credits::{agent_id_from_migration_seed, tracks_match_archive, AgentId, AudiobookTrack, AuthorId, BookDetails, BookMetadata, BookMetadataError, BookRecord, BookTocDocument, BookTocEntry, Contributor, ContributorId};
pub use identifier::{from_metadata_value, push_isbn, Identifier, IdentifierError, Scheme, Scope};
pub use language::{LanguageTag, LanguageTagError};
pub use marc::{marc_relator_code_from_metadata_role, MarcRelatorCode, AUTHOR_MARC_RELATOR_CODE, CONTRIBUTOR_MARC_RELATOR_CODE, NARRATOR_MARC_RELATOR_CODE};
pub use names::{comparison_key, given_name_first, looks_like_organization, recognized_suffix};
pub use reading::{
    audiobook_position_millis, audiobook_reading_position, layout_fraction_progress, pdf_page_from_reading_position, pdf_page_position_from_reading_position, pdf_page_position_reading_position, pdf_page_progress, pdf_page_reading_position,
    valid_epub_cfi, valid_reading_position, EpubCfi, InvalidEpubCfi, InvalidReadProgress, InvalidReadingPosition, ReadProgress, ReadingEpubCfi, ReadingPosition,
};
pub use search_text::{normalize_search_text, normalized_substring_match};
pub use subject::{BookSubject, SubjectError};
pub use toc_target::{audiobook_chapter_display_title, audiobook_toc_position, audiobook_toc_target, pdf_toc_page, pdf_toc_target, AUDIOBOOK_TOC_TARGET_PREFIX, PDF_TOC_TARGET_PREFIX};
pub use year::unique_book_year;

/// A non-negative Unix timestamp measured in milliseconds.
pub type UnixMillis = u64;
