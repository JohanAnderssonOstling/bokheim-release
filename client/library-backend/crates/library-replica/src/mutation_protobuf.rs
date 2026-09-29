//! Explicit protobuf mapping for client-owned mutation payloads.
//!
//! The service deliberately treats the resulting bytes as opaque. Compatibility
//! is governed by the stable field numbers in `bokheim_wire.proto`, not by the
//! Rust/Serde representation used for local persistence.

use super::MutationBody;
use crate::{BookLifecycleState, DirectoryLifecycleState, ReadingPositionState, SyncBookMetadata};
use book_model::{
    AnnotationAnchor, AnnotationState, AnnotationStyle, BookDate, BookFormat, BookMetadata, BookSubject, Contributor, Identifier, LanguageTag, LocalizedText, MarcRelatorCode, PdfAnnotationAnchor, PdfAnnotationRect, PublisherCredit,
    ReadingPosition, Scheme, Scope,
};
use sync_common::transport::{self, protobuf as pb};
use sync_common::{ContentHash, WireMutation};

fn decode_error(message: impl Into<String>) -> transport::WireError {
    transport::WireError::Decode(message.into())
}

fn required<T>(value: Option<T>, field: &str) -> Result<T, transport::WireError> {
    value.ok_or_else(|| decode_error(format!("mutation protobuf is missing {field}")))
}

pub(super) fn encode(body: &MutationBody) -> Result<Vec<u8>, transport::WireError> {
    use pb::mutation_value::Kind;
    let kind = match body {
        MutationBody::BookFacts { value, .. } => Kind::BookFacts(pb::BookFacts { added_at: value.added_at, format: encode_book_format(value.format) as i32 }),
        MutationBody::DirectoryName { name, .. } => Kind::DirectoryName(name.clone()),
        MutationBody::DirectoryParent { parent_id, .. } => Kind::DirectoryParent(parent_id.to_string()),
        MutationBody::DirectoryLifecycle { value, .. } => Kind::DirectoryLifecycle(encode_directory_lifecycle(*value) as i32),
        MutationBody::BookLifecycle { value, .. } => Kind::BookLifecycle(encode_book_lifecycle(value)),
        MutationBody::Placement { present, origin_folder_id, .. } => Kind::Placement(pb::Placement { present: *present, origin_folder_id: origin_folder_id.clone() }),
        MutationBody::ReadingPosition { value, .. } => Kind::ReadingPosition(encode_reading_position(value)),
        MutationBody::Annotation { value, .. } => Kind::Annotation(encode_annotation(value)),
        MutationBody::Metadata { value, .. } => Kind::Metadata(encode_book_record(value)),
        MutationBody::PdfReaderMetadata { value, .. } => {
            if !value.valid_for(&value.checksum) {
                return Err(decode_error("invalid PDF reader metadata"));
            }
            Kind::PdfReaderMetadata(serde_json::to_vec(value).map_err(|e| decode_error(e.to_string()))?)
        }
        MutationBody::Description { value, .. } => Kind::Description(value.clone()),
        MutationBody::BookToc { value, .. } => {
            let bytes = serde_json::to_vec(value).map_err(|e| decode_error(e.to_string()))?;
            if bytes.len() > MAX_BOOK_TOC_BYTES {
                return Err(decode_error("navigation document exceeds the mutation envelope"));
            }
            Kind::BookToc(bytes)
        }
    };
    transport::encode_mutation_value(&pb::MutationValue { kind: Some(kind) })
}

/// A navigation document has to fit one mutation. The cap matches the bound a
/// stored PDF snapshot already lives under, so neither side can produce a
/// register the other refuses.
const MAX_BOOK_TOC_BYTES: usize = 512 * 1024;

pub(super) fn decode(wire: &WireMutation) -> Result<MutationBody, transport::WireError> {
    use pb::mutation_value::Kind;
    let value = transport::decode_mutation_value(&wire.value, transport::MAX_DECODED_REQUEST_BYTES)?;
    let kind = required(value.kind, "value")?;
    let key = wire.entity_key.as_str();
    match (wire.kind.as_str(), kind) {
        (sync_common::mutation_kind::DIRECTORY_NAME, Kind::DirectoryName(name)) => Ok(MutationBody::DirectoryName { dir_id: parse_uuid(key, "directory id")?, name }),
        (sync_common::mutation_kind::DIRECTORY_PARENT, Kind::DirectoryParent(parent_id)) => Ok(MutationBody::DirectoryParent { dir_id: parse_uuid(key, "directory id")?, parent_id: parse_uuid(&parent_id, "parent directory id")? }),
        (sync_common::mutation_kind::DIRECTORY_LIFECYCLE, Kind::DirectoryLifecycle(value)) => Ok(MutationBody::DirectoryLifecycle { dir_id: parse_uuid(key, "directory id")?, value: decode_directory_lifecycle(value)? }),
        (sync_common::mutation_kind::BOOK_FACTS, Kind::BookFacts(value)) => {
            Ok(MutationBody::BookFacts { content_hash: ContentHash::new(key), value: crate::BookFacts { added_at: value.added_at, format: decode_book_format(value.format)? } })
        }
        (sync_common::mutation_kind::BOOK_LIFECYCLE, Kind::BookLifecycle(value)) => Ok(MutationBody::BookLifecycle { content_hash: ContentHash::new(key), value: decode_book_lifecycle(value)? }),
        (sync_common::mutation_kind::PLACEMENT, Kind::Placement(value)) => {
            Ok(MutationBody::Placement { dir_id: parse_uuid(&wire.entity_subkey, "placement directory id")?, content_hash: ContentHash::new(key), present: value.present, origin_folder_id: value.origin_folder_id })
        }
        (sync_common::mutation_kind::READING_POSITION, Kind::ReadingPosition(value)) => Ok(MutationBody::ReadingPosition { content_hash: ContentHash::new(key), value: decode_reading_position(value)? }),
        (sync_common::mutation_kind::ANNOTATION, Kind::Annotation(value)) => {
            let value = decode_annotation(value)?;
            if !wire.blob_reference.as_ref().is_some_and(|r| r.content_hash == Some(value.content_hash) && r.present == !value.deleted) {
                return Err(decode_error("annotation owner declaration does not match its value"));
            }
            Ok(MutationBody::Annotation { annotation_id: key.to_owned(), value })
        }
        (sync_common::mutation_kind::PDF_READER_METADATA, Kind::PdfReaderMetadata(bytes)) => {
            let value: pdf_view_common::PdfReaderMetadata = serde_json::from_slice(&bytes).map_err(|e| decode_error(e.to_string()))?;
            if !value.valid_for(&wire.entity_subkey) {
                return Err(decode_error("PDF metadata revision mismatch"));
            }
            Ok(MutationBody::PdfReaderMetadata { content_hash: ContentHash::new(key), value })
        }
        (sync_common::mutation_kind::BOOK_TOC, Kind::BookToc(bytes)) => {
            if bytes.len() > MAX_BOOK_TOC_BYTES {
                return Err(decode_error("navigation document exceeds the mutation envelope"));
            }
            let value = serde_json::from_slice(&bytes).map_err(|e| decode_error(e.to_string()))?;
            Ok(MutationBody::BookToc { content_hash: ContentHash::new(key), value })
        }
        (sync_common::mutation_kind::DESCRIPTION, Kind::Description(value)) => Ok(MutationBody::Description { content_hash: ContentHash::new(key), value }),
        (sync_common::mutation_kind::METADATA, Kind::Metadata(value)) => Ok(MutationBody::Metadata { content_hash: ContentHash::new(key), value: decode_book_record(value)? }),
        _ => Err(decode_error("mutation kind does not match its protobuf value")),
    }
}

fn parse_uuid(value: &str, field: &str) -> Result<sync_common::DirId, transport::WireError> {
    sync_common::DirId::parse_str(value).map_err(|error| decode_error(format!("invalid {field}: {error}")))
}

fn encode_directory_lifecycle(value: DirectoryLifecycleState) -> pb::DirectoryLifecycle {
    match value {
        DirectoryLifecycleState::Present => pb::DirectoryLifecycle::Present,
        DirectoryLifecycleState::Deleted => pb::DirectoryLifecycle::Deleted,
        DirectoryLifecycleState::Purged => pb::DirectoryLifecycle::Purged,
    }
}

fn decode_directory_lifecycle(value: i32) -> Result<DirectoryLifecycleState, transport::WireError> {
    match pb::DirectoryLifecycle::try_from(value).map_err(|_| decode_error("unknown directory lifecycle"))? {
        pb::DirectoryLifecycle::Present => Ok(DirectoryLifecycleState::Present),
        pb::DirectoryLifecycle::Deleted => Ok(DirectoryLifecycleState::Deleted),
        pb::DirectoryLifecycle::Purged => Ok(DirectoryLifecycleState::Purged),
        pb::DirectoryLifecycle::Unspecified => Err(decode_error("unspecified directory lifecycle")),
    }
}

fn encode_book_lifecycle(value: &BookLifecycleState) -> pb::BookLifecycle {
    use pb::book_lifecycle::State;
    let state = match value {
        BookLifecycleState::Present => State::Present(pb::PresentBook {}),
        BookLifecycleState::Deleted { origin_folder_id } => State::Deleted(pb::DeletedBook { origin_folder_id: origin_folder_id.clone() }),
        BookLifecycleState::Purged => State::Purged(pb::PurgedBook {}),
    };
    pb::BookLifecycle { state: Some(state) }
}

fn decode_book_lifecycle(value: pb::BookLifecycle) -> Result<BookLifecycleState, transport::WireError> {
    use pb::book_lifecycle::State;
    match required(value.state, "book lifecycle state")? {
        State::Present(_) => Ok(BookLifecycleState::Present),
        State::Deleted(value) => Ok(BookLifecycleState::Deleted { origin_folder_id: value.origin_folder_id }),
        State::Purged(_) => Ok(BookLifecycleState::Purged),
    }
}

fn encode_book_format(value: BookFormat) -> pb::BookFormat {
    match value {
        BookFormat::Epub => pb::BookFormat::Epub,
        BookFormat::Pdf => pb::BookFormat::Pdf,
        BookFormat::Mobi => pb::BookFormat::Mobi,
        BookFormat::M4b => pb::BookFormat::M4b,
        BookFormat::Mp3Folder => pb::BookFormat::Mp3Folder,
    }
}

fn decode_book_format(value: i32) -> Result<BookFormat, transport::WireError> {
    match pb::BookFormat::try_from(value).map_err(|_| decode_error("unknown book format"))? {
        pb::BookFormat::Epub => Ok(BookFormat::Epub),
        pb::BookFormat::Pdf => Ok(BookFormat::Pdf),
        pb::BookFormat::Mobi => Ok(BookFormat::Mobi),
        pb::BookFormat::M4b => Ok(BookFormat::M4b),
        pb::BookFormat::Mp3Folder => Ok(BookFormat::Mp3Folder),
        pb::BookFormat::Fb2 | pb::BookFormat::Cbz | pb::BookFormat::Html | pb::BookFormat::Webpub | pb::BookFormat::Unspecified => Err(decode_error("unsupported book format")),
    }
}

fn encode_reading_position(value: &ReadingPositionState) -> pb::ReadingPosition {
    use pb::reading_position::Location;
    let location = match &value.location {
        ReadingPosition::Epub(_) => Location::EpubCfi(value.location.as_str()),
        ReadingPosition::Pdf(page) => Location::PdfPage(*page),
        ReadingPosition::PdfAtPoint { page, full_page_position } => {
            return pb::ReadingPosition { location: Some(Location::PdfPage(*page)), pdf_full_page_position: Some(full_page_position.max(0.0)), progress: value.progress };
        }
        ReadingPosition::Audiobook(millis) => Location::AudiobookMillis(*millis),
    };
    pb::ReadingPosition { location: Some(location), progress: value.progress, pdf_full_page_position: None }
}

fn decode_reading_position(value: pb::ReadingPosition) -> Result<ReadingPositionState, transport::WireError> {
    use pb::reading_position::Location;
    let full_page_position = value.pdf_full_page_position;
    let location = match required(value.location, "reading location")? {
        Location::EpubCfi(value) => ReadingPosition::epub_cfi(&value).map_err(|_| decode_error("invalid EPUB reading position"))?,
        Location::PdfPage(page) => match full_page_position {
            Some(full_page_position) if full_page_position.is_finite() => ReadingPosition::pdf_page_at_point(page, full_page_position.max(0.0)),
            Some(_) => return Err(decode_error("invalid PDF full-page position")),
            None => ReadingPosition::pdf_page(page),
        },
        Location::AudiobookMillis(value) => ReadingPosition::audiobook_millis(value),
    };
    Ok(ReadingPositionState { location, progress: value.progress })
}

fn encode_annotation(value: &AnnotationState) -> pb::Annotation {
    use pb::annotation_anchor::Anchor;
    let anchor = match &value.anchor {
        AnnotationAnchor::EpubCfi { cfi } => Anchor::EpubCfi(cfi.clone()),
        AnnotationAnchor::Pdf { page_index, rects, fallback_cfi } => Anchor::Pdf(pb::PdfAnnotationAnchor {
            page_index: *page_index,
            rects: rects.iter().map(|rect| pb::PdfAnnotationRect { left: rect.left, top: rect.top, width: rect.width, height: rect.height }).collect(),
            fallback_cfi: fallback_cfi.clone(),
        }),
    };
    pb::Annotation {
        content_hash: value.content_hash.to_string(),
        anchor: Some(pb::AnnotationAnchor { anchor: Some(anchor) }),
        exact_text: value.exact_text.clone(),
        style: encode_annotation_style(value.style.clone()) as i32,
        color: value.color.clone(),
        note: value.note.clone(),
        created_at: value.created_at,
        modified_at: value.modified_at,
        deleted: value.deleted,
        toc_ordinal: value.toc_ordinal,
        progress: value.progress,
    }
}

fn decode_annotation(value: pb::Annotation) -> Result<AnnotationState, transport::WireError> {
    use pb::annotation_anchor::Anchor;
    let anchor = match required(required(value.anchor, "annotation anchor")?.anchor, "annotation anchor value")? {
        Anchor::EpubCfi(cfi) => AnnotationAnchor::epub_cfi(cfi),
        Anchor::Pdf(value) => AnnotationAnchor::pdf(PdfAnnotationAnchor::new(value.page_index, value.rects.into_iter().map(|rect| PdfAnnotationRect::new(rect.left, rect.top, rect.width, rect.height)).collect(), value.fallback_cfi)),
    };
    Ok(AnnotationState {
        content_hash: ContentHash::new(&value.content_hash),
        anchor,
        exact_text: value.exact_text,
        style: decode_annotation_style(value.style)?,
        color: value.color,
        note: value.note,
        created_at: value.created_at,
        modified_at: value.modified_at,
        deleted: value.deleted,
        toc_ordinal: value.toc_ordinal,
        progress: value.progress,
    })
}

fn encode_annotation_style(value: AnnotationStyle) -> pb::AnnotationStyle {
    match value {
        AnnotationStyle::Highlight => pb::AnnotationStyle::Highlight,
        AnnotationStyle::Underline => pb::AnnotationStyle::Underline,
        AnnotationStyle::Squiggly => pb::AnnotationStyle::Squiggly,
        AnnotationStyle::Strikethrough => pb::AnnotationStyle::Strikethrough,
    }
}

fn decode_annotation_style(value: i32) -> Result<AnnotationStyle, transport::WireError> {
    match pb::AnnotationStyle::try_from(value).map_err(|_| decode_error("unknown annotation style"))? {
        pb::AnnotationStyle::Highlight => Ok(AnnotationStyle::Highlight),
        pb::AnnotationStyle::Underline => Ok(AnnotationStyle::Underline),
        pb::AnnotationStyle::Squiggly => Ok(AnnotationStyle::Squiggly),
        pb::AnnotationStyle::Strikethrough => Ok(AnnotationStyle::Strikethrough),
        pb::AnnotationStyle::Unspecified => Err(decode_error("unspecified annotation style")),
    }
}

fn encode_book_record(value: &SyncBookMetadata) -> pb::BookRecord {
    pb::BookRecord { title: value.title.clone(), contributors: value.contributors.iter().map(encode_contributor).collect(), book: Some(encode_book_metadata(&value.book)), subtitle: value.subtitle.clone() }
}

fn decode_book_record(value: pb::BookRecord) -> Result<SyncBookMetadata, transport::WireError> {
    Ok(SyncBookMetadata {
        title: value.title,
        subtitle: value.subtitle,
        contributors: value.contributors.into_iter().map(decode_contributor).collect::<Result<_, _>>()?,
        book: value.book.map(decode_book_metadata).transpose()?.unwrap_or_default(),
    })
}

fn encode_contributor(value: &Contributor) -> pb::Contributor {
    pb::Contributor { contributor_id: value.contributor_id().to_string(), name: value.name().to_owned(), role: value.role_code().0.to_vec() }
}

fn decode_contributor(value: pb::Contributor) -> Result<Contributor, transport::WireError> {
    let contributor_id = book_model::ContributorId::parse_str(&value.contributor_id).map_err(|error| decode_error(format!("invalid contributor id: {error}")))?;
    let role: [u8; 3] = value.role.try_into().map_err(|_| decode_error("contributor role must contain exactly three bytes"))?;
    Contributor::with_id(contributor_id, value.name, MarcRelatorCode(role)).map_err(|error| decode_error(error.to_string()))
}

fn encode_localized_text(value: &LocalizedText) -> pb::LocalizedText {
    pb::LocalizedText { language: value.language.clone(), value: value.value.clone() }
}

fn decode_localized_text(value: pb::LocalizedText) -> Result<LocalizedText, transport::WireError> {
    LocalizedText::new(value.language, value.value).map_err(|error| decode_error(error.to_string()))
}

fn encode_book_metadata(value: &BookMetadata) -> pb::BookMetadata {
    pb::BookMetadata {
        identifiers: value.identifiers.iter().map(encode_book_identifier).collect(),
        publishers: value.publishers.iter().map(|publisher| pb::Publisher { publisher_id: publisher.publisher_id().to_string(), name: publisher.name().as_str().to_owned() }).collect(),
        languages: value.languages.iter().map(|language| language.as_str().to_owned()).collect(),
        dates: value.dates.iter().map(|date| pb::BookDate { id: date.id().map(str::to_owned), value: date.value().to_owned(), event: date.event().map(str::to_owned), source: Some(date.source().to_owned()) }).collect(),
        subjects: value
            .subjects
            .iter()
            .map(|subject| pb::BookSubject {
                id: subject.id().map(str::to_owned),
                name: subject.name().to_owned(),
                authority: subject.authority().map(str::to_owned),
                code: subject.code().map(str::to_owned),
                source: Some(subject.source().to_owned()),
                name_localizations: subject.name_localizations.iter().map(encode_localized_text).collect(),
                sort_name: subject.sort_name.clone(),
                sort_name_localizations: subject.sort_name_localizations.iter().map(encode_localized_text).collect(),
            })
            .collect(),
        // The model no longer carries collections, alternate_titles (subtitle
        // is now its own wire field on BookRecord), formats, rights, sources,
        // types, relations, coverage, or properties; these wire fields stay empty.
        collections: Vec::new(),
        alternate_titles: Vec::new(),
        formats: Vec::new(),
        rights: Vec::new(),
        sources: Vec::new(),
        types: Vec::new(),
        relations: Vec::new(),
        coverage: Vec::new(),
        properties: Vec::new(),
    }
}

fn decode_book_metadata(value: pb::BookMetadata) -> Result<BookMetadata, transport::WireError> {
    Ok(BookMetadata {
        identifiers: value.identifiers.into_iter().map(decode_book_identifier).collect::<Result<_, _>>()?,
        publishers: value
            .publishers
            .into_iter()
            .map(|publisher| {
                let publisher_id = book_model::PublisherId::parse_str(&publisher.publisher_id).map_err(|error| decode_error(format!("invalid publisher id: {error}")))?;
                PublisherCredit::with_id(publisher_id, publisher.name).map_err(|error| decode_error(error.to_string()))
            })
            .collect::<Result<_, _>>()?,
        languages: value.languages.into_iter().map(|language| LanguageTag::parse(language).map_err(|error| decode_error(error.to_string()))).collect::<Result<_, _>>()?,
        dates: value
            .dates
            .into_iter()
            .map(|date| BookDate::new(date.id, date.value, date.event, date.source.ok_or_else(|| decode_error("missing date source"))?).map_err(|error| decode_error(error.to_string())))
            .collect::<Result<_, _>>()?,
        subjects: value
            .subjects
            .into_iter()
            .map(|subject| {
                BookSubject::with_details(
                    subject.id,
                    subject.name,
                    subject.name_localizations.into_iter().map(decode_localized_text).collect::<Result<_, _>>()?,
                    subject.sort_name,
                    subject.sort_name_localizations.into_iter().map(decode_localized_text).collect::<Result<_, _>>()?,
                    subject.source.ok_or_else(|| decode_error("missing subject source"))?,
                    subject.authority,
                    subject.code,
                )
                .map_err(|error| decode_error(error.to_string()))
            })
            .collect::<Result<_, _>>()?,
        // Wire collections, alternate_titles, formats, rights, sources, types,
        // relations, coverage, and properties predate provenance removal and
        // are dropped.
    })
}

fn encode_book_identifier(value: &Identifier) -> pb::BookIdentifier {
    let (scheme, other_scheme) = match value.scheme() {
        Scheme::Unspecified => (pb::IdentifierScheme::Unspecified, None),
        Scheme::Isbn => (pb::IdentifierScheme::Isbn, None),
        Scheme::Asin => (pb::IdentifierScheme::Asin, None),
        Scheme::Doi => (pb::IdentifierScheme::Doi, None),
        Scheme::Issn => (pb::IdentifierScheme::Issn, None),
        Scheme::Oclc => (pb::IdentifierScheme::Oclc, None),
        Scheme::Lccn => (pb::IdentifierScheme::Lccn, None),
        Scheme::Uuid => (pb::IdentifierScheme::Uuid, None),
        Scheme::Calibre => (pb::IdentifierScheme::Calibre, None),
        Scheme::Google => (pb::IdentifierScheme::Google, None),
        Scheme::Goodreads => (pb::IdentifierScheme::Goodreads, None),
        Scheme::Kobo => (pb::IdentifierScheme::Kobo, None),
        Scheme::Other(value) => (pb::IdentifierScheme::Other, Some(value.clone())),
    };
    let mut identifier_types = Vec::new();
    let scope = match value.scope() {
        Scope::Book => "book",
        Scope::Edition => "edition",
        Scope::Work => "work",
    };
    identifier_types.push(pb::BookIdentifierType { value: scope.to_owned(), scheme: Some("bokheim:scope".to_owned()) });
    pb::BookIdentifier { value: value.value().to_owned(), scheme: scheme as i32, other_scheme, identifier_types }
}

fn decode_book_identifier(value: pb::BookIdentifier) -> Result<Identifier, transport::WireError> {
    let scheme = match pb::IdentifierScheme::try_from(value.scheme).map_err(|_| decode_error("unknown book identifier scheme"))? {
        pb::IdentifierScheme::Unspecified => Scheme::Unspecified,
        pb::IdentifierScheme::Isbn => Scheme::Isbn,
        pb::IdentifierScheme::Asin => Scheme::Asin,
        pb::IdentifierScheme::Doi => Scheme::Doi,
        pb::IdentifierScheme::Issn => Scheme::Issn,
        pb::IdentifierScheme::Oclc => Scheme::Oclc,
        pb::IdentifierScheme::Lccn => Scheme::Lccn,
        pb::IdentifierScheme::Uuid => Scheme::Uuid,
        pb::IdentifierScheme::Calibre => Scheme::Calibre,
        pb::IdentifierScheme::Google => Scheme::Google,
        pb::IdentifierScheme::Goodreads => Scheme::Goodreads,
        pb::IdentifierScheme::Kobo => Scheme::Kobo,
        pb::IdentifierScheme::Other => Scheme::Other(required(value.other_scheme, "other identifier scheme")?),
    };
    let scope = value
        .identifier_types
        .iter()
        .find(|value| value.scheme.as_deref() == Some("bokheim:scope"))
        .and_then(|value| match value.value.as_str() {
            "edition" => Some(Scope::Edition),
            "work" => Some(Scope::Work),
            "book" => Some(Scope::Book),
            _ => None,
        })
        .ok_or_else(|| decode_error("missing or invalid identifier scope"))?;
    Identifier::new(value.value, scheme, scope).map_err(|error| decode_error(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_round_trip_all_scopes_and_reject_missing_evidence() {
        for scope in [Scope::Book, Scope::Edition, Scope::Work] {
            let identifier = Identifier::new("9780306406157", Scheme::Isbn, scope).unwrap();
            let encoded = encode_book_identifier(&identifier);
            assert_eq!(decode_book_identifier(encoded.clone()).unwrap(), identifier);
            let mut incomplete = encoded.clone();
            incomplete.identifier_types.retain(|item| item.scheme.as_deref() != Some("bokheim:scope"));
            assert!(decode_book_identifier(incomplete).is_err());
        }
    }

    #[test]
    fn book_values_require_provenance() {
        let encoded = pb::BookMetadata {
            dates: vec![pb::BookDate { id: None, value: "1965".to_owned(), event: Some("book".to_owned()), source: None }],
            alternate_titles: vec![pb::BookTitle { id: None, value: "Dune World".to_owned(), title_type: Some("alternative".to_owned()), sort_as: None, source: None }],
            ..Default::default()
        };

        assert!(decode_book_metadata(encoded).is_err());
    }

    #[test]
    fn wire_properties_are_dropped() {
        let encoded = pb::BookMetadata {
            properties: vec![pb::BookProperty { id: None, property: "belongs-to-collection".to_owned(), value: "Dune".to_owned(), refines: None, scheme: None, language: None, source: Some("epub:package:meta".to_owned()) }],
            ..Default::default()
        };

        let decoded = decode_book_metadata(encoded).unwrap();
        assert!(encode_book_metadata(&decoded).properties.is_empty());
    }
}
