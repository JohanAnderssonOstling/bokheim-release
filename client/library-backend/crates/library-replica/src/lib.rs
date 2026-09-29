//! Client-replica interpretation of opaque sync payloads.
//!
//! The sync service depends only on `sync-common` and never links this crate.

use book_model::AnnotationState;
pub mod filenames;
mod folder_management;
mod folder_names;
pub use folder_names::project_folder_name;
mod library_state;

pub use folder_management::{FolderDestinationPolicy, numbered_file_name, numbered_folder_name, unique_file_name, unique_folder_name};
pub use library_state::{BookFacts, BookLifecycleState, DirectoryLifecycleState, ReadingPositionState, SyncBookMetadata, portable_name_key};

use serde::{Deserialize, Serialize};

mod transfer;
mod transfer_values;
use std::collections::HashMap;
use sync_common::ContentHash;
pub use sync_common::VersionKey;
use sync_common::{DeclaredBlobReference, DirId, FileName, MutationId, ReplicaSeq, UnixMillis, WireMutation};
pub use transfer::{BlobKey, BlobKind, LibraryOperation, LibraryOperationKind, LibraryOperationState, LibraryTransfers, ScanProgress, TransferJobKind, TransferOrigin, TransferState, TransferStatus};
pub use transfer_values::{BookPlacement, BookUploadIntent, RelativeBookPath, RelativeBookPathError, RelativeDirPath, RelativeDirPathError};

mod directory_projection;
mod mutation_protobuf;
pub use directory_projection::{DirectoryIntent, DirectoryProjection, project_directories};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StateKey {
    DirectoryName(DirId),
    DirectoryParent(DirId),
    DirectoryLifecycle(DirId),
    BookLifecycle(ContentHash),
    BookFacts(ContentHash),
    Placement { content_hash: ContentHash, dir_id: DirId },
    ReadingPosition(ContentHash),
    Annotation(String),
    Metadata(ContentHash),
    Description(ContentHash),
    PdfReaderMetadata(ContentHash, String),
    BookToc(ContentHash),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MutationBody {
    DirectoryName {
        dir_id: DirId,
        name: FileName,
    },
    DirectoryParent {
        dir_id: DirId,
        parent_id: DirId,
    },
    DirectoryLifecycle {
        dir_id: DirId,
        value: DirectoryLifecycleState,
    },
    BookLifecycle {
        content_hash: ContentHash,
        value: BookLifecycleState,
    },
    Placement {
        dir_id: DirId,
        content_hash: ContentHash,
        present: bool,
        origin_folder_id: Option<String>,
    },
    ReadingPosition {
        content_hash: ContentHash,
        value: ReadingPositionState,
    },
    Annotation {
        annotation_id: String,
        value: AnnotationState,
    },
    Metadata {
        content_hash: ContentHash,
        value: SyncBookMetadata,
    },
    Description {
        content_hash: ContentHash,
        value: String,
    },
    PdfReaderMetadata {
        content_hash: ContentHash,
        value: pdf_view_common::PdfReaderMetadata,
    },
    /// The book's navigation, keyed by the book's identity rather than
    /// by the bytes it was read from. Transported because it is the one
    /// navigation a replica cannot always rebuild: an audiobook's chapters may
    /// have come from enrichment rather than from the file. An audiobook's
    /// total duration travels with its entries: without it a replica can show
    /// the chapters but cannot play the book.
    BookToc {
        content_hash: ContentHash,
        value: book_model::BookTocDocument,
    },
    BookFacts {
        content_hash: ContentHash,
        value: BookFacts,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StateMutation {
    /// Stable retry/idempotency key used for conditional outbox acknowledgement.
    pub mutation_id: MutationId,
    pub body: MutationBody,
    pub changed_at: UnixMillis,
    pub replica_seq: ReplicaSeq,
    pub origin: Option<sync_common::MutationOrigin>,
}

/// Reduces pending mutations produced by one replica without changing their
/// observable server-side meaning.
///
/// Ordinary state domains are unconditional registers and safely collapse to
/// their greatest local version.
pub fn reduce_same_replica_mutations<'a>(mutations: impl IntoIterator<Item = &'a StateMutation>) -> Vec<&'a StateMutation> {
    let mut registers = HashMap::<StateKey, &'a StateMutation>::new();
    let mut reduced = Vec::new();
    for mutation in mutations {
        // Forwarded winners can belong to different replicas. Do not reduce
        // them using the publisher's local sequence order.
        if mutation.origin.is_some() {
            reduced.push(mutation);
            continue;
        }
        let key = mutation.body.state_key();
        match registers.get(&key) {
            Some(current) if !same_replica_candidate_wins(current, mutation) => {}
            _ => {
                registers.insert(key, mutation);
            }
        }
    }
    reduced.extend(registers.into_values());
    reduced.sort_by_key(|mutation| (mutation.replica_seq, mutation.mutation_id));
    reduced
}

/// Reduces unsent local mutations to one canonical winner per state cell.
pub fn coalesce_mutations_for_push(mutations: Vec<StateMutation>) -> Vec<StateMutation> {
    reduce_same_replica_mutations(&mutations).into_iter().cloned().collect()
}

/// Encodes replica-domain mutations and partitions them into bounded protocol
/// pages while retaining oversized mutation identities for diagnostics.
pub fn prepare_push(changes: Vec<StateMutation>) -> Result<sync_common::PushBatcher, Box<dyn std::error::Error>> {
    let mutations = changes.into_iter().map(|mutation| mutation.to_wire()).collect::<Result<Vec<_>, _>>()?;
    Ok(sync_common::PushBatcher::new(mutations)?)
}

fn same_replica_candidate_wins(current: &StateMutation, candidate: &StateMutation) -> bool {
    candidate.body.state_key() == current.body.state_key()
        && sync_common::compare_same_replica_versions(
            (candidate.changed_at, candidate.body.conflict_rank(), candidate.replica_seq, candidate.mutation_id),
            (current.changed_at, current.body.conflict_rank(), current.replica_seq, current.mutation_id),
        )
        .is_gt()
}

impl MutationBody {
    /// Book bytes that must be uploaded before this state can be shared.
    /// Removals must remain possible even when a local import never uploaded.
    pub fn upload_dependency(&self) -> Option<sync_common::ContentHash> {
        match self {
            Self::DirectoryName { .. } | Self::DirectoryParent { .. } | Self::DirectoryLifecycle { .. } | Self::BookLifecycle { value: BookLifecycleState::Purged, .. } | Self::Placement { present: false, .. } => None,
            Self::Annotation { value, .. } => (!value.deleted).then_some(value.content_hash),
            Self::BookFacts { content_hash, .. }
            | Self::BookLifecycle { content_hash, .. }
            | Self::Placement { content_hash, .. }
            | Self::ReadingPosition { content_hash, .. }
            | Self::Metadata { content_hash, .. }
            | Self::Description { content_hash, .. }
            | Self::PdfReaderMetadata { content_hash, .. }
            | Self::BookToc { content_hash, .. } => Some(*content_hash),
        }
    }

    /// Stable, open wire kind. Every producer obtains its kind here so a
    /// misspelled literal cannot create an unreachable register.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::DirectoryName { .. } => sync_common::mutation_kind::DIRECTORY_NAME,
            Self::DirectoryParent { .. } => sync_common::mutation_kind::DIRECTORY_PARENT,
            Self::DirectoryLifecycle { .. } => sync_common::mutation_kind::DIRECTORY_LIFECYCLE,
            Self::BookLifecycle { .. } => sync_common::mutation_kind::BOOK_LIFECYCLE,
            Self::BookFacts { .. } => sync_common::mutation_kind::BOOK_FACTS,
            Self::Placement { .. } => sync_common::mutation_kind::PLACEMENT,
            Self::ReadingPosition { .. } => sync_common::mutation_kind::READING_POSITION,
            Self::Annotation { .. } => sync_common::mutation_kind::ANNOTATION,
            Self::Metadata { .. } => sync_common::mutation_kind::METADATA,
            Self::PdfReaderMetadata { .. } => sync_common::mutation_kind::PDF_READER_METADATA,
            Self::Description { .. } => sync_common::mutation_kind::DESCRIPTION,
            Self::BookToc { .. } => sync_common::mutation_kind::BOOK_TOC,
        }
    }

    /// Encodes the domain value into the generic wire envelope.
    pub fn to_wire(&self, mutation_id: MutationId, changed_at: UnixMillis, replica_seq: ReplicaSeq) -> Result<WireMutation, sync_common::transport::WireError> {
        let (entity_key, entity_subkey, blob_reference) = match self {
            Self::DirectoryName { dir_id, .. } => (dir_id.to_string(), String::new(), None),
            Self::DirectoryParent { dir_id, .. } => (dir_id.to_string(), String::new(), None),
            Self::DirectoryLifecycle { dir_id, .. } => (dir_id.to_string(), String::new(), None),
            Self::BookLifecycle { content_hash, value } => {
                let blob_reference = match value {
                    BookLifecycleState::Present | BookLifecycleState::Deleted { .. } => DeclaredBlobReference { present: true, content_hash: Some(content_hash.clone()) },
                    BookLifecycleState::Purged => DeclaredBlobReference { present: false, content_hash: None },
                };
                (content_hash.to_string(), String::new(), Some(blob_reference))
            }
            Self::Placement { dir_id, content_hash, .. } => (content_hash.to_string(), dir_id.to_string(), None),
            Self::ReadingPosition { content_hash, .. } => (content_hash.to_string(), String::new(), None),
            Self::Annotation { annotation_id, value } => (annotation_id.clone(), String::new(), Some(DeclaredBlobReference { present: !value.deleted, content_hash: Some(value.content_hash) })),
            Self::PdfReaderMetadata { content_hash, value } => (content_hash.to_string(), value.checksum.clone(), None),
            Self::BookFacts { content_hash, .. } | Self::Metadata { content_hash, .. } | Self::Description { content_hash, .. } | Self::BookToc { content_hash, .. } => (content_hash.to_string(), String::new(), None),
        };
        // Non-lifecycle declarations are upload prerequisites only; they do not
        // increment the server's lifecycle reference count.
        let blob_reference = blob_reference.or_else(|| {
            self.upload_dependency()
                .map(|hash| DeclaredBlobReference { present: true, content_hash: Some(hash) })
                .or_else(|| matches!(self, Self::Placement { present: false, .. } | Self::Annotation { .. }).then_some(DeclaredBlobReference { present: false, content_hash: None }))
        });
        let value = mutation_protobuf::encode(self)?;
        Ok(WireMutation { origin: None, mutation_id, kind: self.kind().to_owned(), entity_key, entity_subkey, value, conflict_rank: self.conflict_rank(), blob_reference, changed_at, replica_seq })
    }

    /// Decodes a recognized wire value. Unknown kinds are intentionally
    /// forward-compatible and return `None` without interpreting their bytes.
    pub fn from_wire(wire_mutation: &WireMutation) -> Result<Option<Self>, sync_common::transport::WireError> {
        if !matches!(
            wire_mutation.kind.as_str(),
            sync_common::mutation_kind::DIRECTORY_NAME
                | sync_common::mutation_kind::DIRECTORY_PARENT
                | sync_common::mutation_kind::DIRECTORY_LIFECYCLE
                | sync_common::mutation_kind::BOOK_LIFECYCLE
                | sync_common::mutation_kind::BOOK_FACTS
                | sync_common::mutation_kind::PLACEMENT
                | sync_common::mutation_kind::READING_POSITION
                | sync_common::mutation_kind::ANNOTATION
                | sync_common::mutation_kind::METADATA
                | sync_common::mutation_kind::PDF_READER_METADATA
                | sync_common::mutation_kind::DESCRIPTION
                | sync_common::mutation_kind::BOOK_TOC
        ) {
            return Ok(None);
        }
        mutation_protobuf::decode(wire_mutation).map(Some)
    }
    /// Every field of every variant is bound here (payload fields discarded
    /// as `_` by name) rather than skipped with `..`, so adding a new field
    /// to any `MutationBody` variant fails to compile here until someone
    /// decides whether it belongs in the cell's identity or not.
    pub fn state_key(&self) -> StateKey {
        match self {
            Self::DirectoryName { dir_id, name: _ } => StateKey::DirectoryName(dir_id.clone()),
            Self::DirectoryParent { dir_id, parent_id: _ } => StateKey::DirectoryParent(dir_id.clone()),
            Self::DirectoryLifecycle { dir_id, value: _ } => StateKey::DirectoryLifecycle(dir_id.clone()),
            Self::BookLifecycle { content_hash, value: _ } => StateKey::BookLifecycle(content_hash.clone()),
            Self::BookFacts { content_hash, value: _ } => StateKey::BookFacts(*content_hash),
            Self::Placement { dir_id, content_hash, .. } => StateKey::Placement { content_hash: content_hash.clone(), dir_id: dir_id.clone() },
            Self::ReadingPosition { content_hash, value: _ } => StateKey::ReadingPosition(content_hash.clone()),
            Self::Annotation { annotation_id, value: _ } => StateKey::Annotation(annotation_id.clone()),
            Self::Metadata { content_hash, value: _ } => StateKey::Metadata(content_hash.clone()),
            Self::PdfReaderMetadata { content_hash, value } => StateKey::PdfReaderMetadata(*content_hash, value.checksum.clone()),
            Self::Description { content_hash, value: _ } => StateKey::Description(content_hash.clone()),
            Self::BookToc { content_hash, value: _ } => StateKey::BookToc(content_hash.clone()),
        }
    }

    /// Rank used inside [`VersionKey`]. Destructive updates in unconditional
    /// registers win an exact timestamp tie.
    pub fn conflict_rank(&self) -> u8 {
        match self {
            Self::Placement { present: false, .. } => 1,
            Self::DirectoryLifecycle { value: DirectoryLifecycleState::Deleted, .. } | Self::BookLifecycle { value: BookLifecycleState::Deleted { .. }, .. } | Self::Annotation { value: AnnotationState { deleted: true, .. }, .. } => 1,
            Self::DirectoryLifecycle { value: DirectoryLifecycleState::Purged, .. } | Self::BookLifecycle { value: BookLifecycleState::Purged, .. } => 2,
            _ => 0,
        }
    }
}

impl StateMutation {
    pub fn to_wire(&self) -> Result<WireMutation, sync_common::transport::WireError> {
        let mut wire = self.body.to_wire(self.mutation_id, self.changed_at, self.replica_seq)?;
        wire.origin = self.origin.clone();
        Ok(wire)
    }

    pub fn from_wire(wire_mutation: &WireMutation) -> Result<Option<Self>, sync_common::transport::WireError> {
        Ok(MutationBody::from_wire(wire_mutation)?.map(|body| Self { mutation_id: wire_mutation.mutation_id, body, changed_at: wire_mutation.changed_at, replica_seq: wire_mutation.replica_seq, origin: wire_mutation.origin.clone() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::{AnnotationAnchor, AnnotationStyle, BookFormat, ReadingPosition};
    use book_model::{BookSubject, LocalizedText};

    fn content_hash(character: char) -> ContentHash {
        ContentHash::new(&character.to_string().repeat(64))
    }

    fn dir_id(value: &str) -> DirId {
        DirId::parse_str(value).unwrap()
    }

    fn metadata() -> SyncBookMetadata {
        let mut book = book_model::BookMetadata::default();
        book.subjects.push(
            BookSubject::with_details(None, "Historiska romaner", vec![LocalizedText::new("en", "Historical fiction").unwrap()], Some("Romaner, historiska".to_owned()), Vec::new(), "test:subject", Some("saogf".to_owned()), None).unwrap(),
        );
        SyncBookMetadata { title: "Book".to_owned(), subtitle: None, contributors: Vec::new(), book }
    }

    #[test]
    fn every_library_payload_round_trips_through_the_opaque_protocol_envelope() {
        let first_dir = dir_id("11111111-1111-4111-8111-111111111111");
        let second_dir = dir_id("22222222-2222-4222-8222-222222222222");
        let first_hash = content_hash('a');
        let bodies = vec![
            MutationBody::DirectoryName { dir_id: first_dir, name: "Shelf".to_owned() },
            MutationBody::DirectoryParent { dir_id: first_dir, parent_id: second_dir },
            MutationBody::DirectoryLifecycle { dir_id: first_dir, value: DirectoryLifecycleState::Present },
            MutationBody::BookLifecycle { content_hash: first_hash, value: BookLifecycleState::Present },
            MutationBody::BookFacts { content_hash: first_hash, value: BookFacts { added_at: 1, format: BookFormat::Epub } },
            MutationBody::Placement { dir_id: first_dir, content_hash: first_hash, present: true, origin_folder_id: None },
            MutationBody::ReadingPosition { content_hash: first_hash, value: ReadingPositionState { location: ReadingPosition::parse("epubcfi(/6/2)").unwrap(), progress: 0.5 } },
            MutationBody::Annotation {
                annotation_id: "annotation".to_owned(),
                value: AnnotationState {
                    content_hash: first_hash,
                    anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/2!/4/1:0)"),
                    exact_text: "Text".to_owned(),
                    style: AnnotationStyle::Highlight,
                    color: "#ffee00".to_owned(),
                    note: String::new(),
                    created_at: 1,
                    modified_at: 2,
                    deleted: false,
                    toc_ordinal: Some(5),
                    progress: Some(42.5),
                },
            },
            MutationBody::Metadata { content_hash: first_hash, value: metadata() },
            MutationBody::Description { content_hash: first_hash, value: "Independent description".into() },
            MutationBody::BookToc {
                content_hash: first_hash,
                value: book_model::BookTocDocument { entries: vec![book_model::BookTocEntry { title: "One".into(), target: book_model::audiobook_toc_target(0), children: Vec::new() }], duration_ms: Some(1000), tracks: None },
            },
        ];

        for (index, body) in bodies.into_iter().enumerate() {
            let mutation = StateMutation { origin: None, mutation_id: MutationId::new(), body, changed_at: 100 + index as u64, replica_seq: ReplicaSeq::new(index as u64 + 1).unwrap() };
            let wire = mutation.to_wire().unwrap();
            assert_eq!(wire.kind, mutation.body.kind());
            let dependency = (index >= 3).then_some(first_hash);
            assert_eq!(mutation.body.upload_dependency(), dependency);
            assert_eq!(wire.blob_reference.as_ref().and_then(|r| r.content_hash), dependency);
            assert!(!wire.value.starts_with(b"BKHM"), "network mutation values must not use the bincode storage envelope");
            sync_common::transport::decode_mutation_value(&wire.value, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
            assert_eq!(StateMutation::from_wire(&wire).unwrap(), Some(mutation));
        }
    }

    #[test]
    fn removing_an_unuploaded_import_does_not_require_its_bytes() {
        let hash = content_hash('a');
        let removals = [MutationBody::BookLifecycle { content_hash: hash, value: BookLifecycleState::Purged }, MutationBody::Placement { dir_id: sync_common::ROOT_DIR_ID, content_hash: hash, present: false, origin_folder_id: None }];
        for body in removals {
            assert_eq!(body.upload_dependency(), None);
            let wire = body.to_wire(MutationId::new(), 1, ReplicaSeq::new(1).unwrap()).unwrap();
            assert_eq!(wire.blob_reference, Some(DeclaredBlobReference { present: false, content_hash: None }));
        }
    }

    #[test]
    fn unknown_service_payloads_remain_forward_compatible() {
        let wire = WireMutation {
            origin: None,
            mutation_id: MutationId::new(),
            kind: "future_kind".to_owned(),
            entity_key: String::new(),
            entity_subkey: String::new(),
            value: Vec::new(),
            conflict_rank: 0,
            blob_reference: None,
            changed_at: 1,
            replica_seq: ReplicaSeq::new(1).unwrap(),
        };
        assert_eq!(StateMutation::from_wire(&wire).unwrap(), None);
    }

    #[test]
    fn placement_updates_coalesce_to_the_latest_register_value() {
        let dir_id = dir_id("11111111-1111-4111-8111-111111111111");
        let claim = StateMutation { origin: None, mutation_id: MutationId::new(), body: MutationBody::Placement { dir_id, content_hash: content_hash('a'), present: true, origin_folder_id: None }, changed_at: 1, replica_seq: ReplicaSeq::new(1).unwrap() };
        let release =
            StateMutation { origin: None, mutation_id: MutationId::new(), body: MutationBody::Placement { dir_id, content_hash: content_hash('a'), present: false, origin_folder_id: None }, changed_at: 2, replica_seq: ReplicaSeq::new(2).unwrap() };
        let reduced = coalesce_mutations_for_push(vec![claim, release]);
        assert!(matches!(reduced.as_slice(), [StateMutation { body: MutationBody::Placement { present: false, .. }, .. }]));
    }
}
