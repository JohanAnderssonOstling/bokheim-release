//! Snapshots and reversible source removal for transfers to another library.
use crate::sync::apply::registers::canonical_body;
use crate::{Database, DatabaseError};
use book_model::Annotation;
use content_address::ContentHash;
use library_model::LibraryFolderDestination;
use library_replica::{BookAddition, LibraryStateEntry};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use sync_common::storage::ProviderFile;
use sync_common::{BookId, DirId, ROOT_DIR_ID, StateKey};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferSelection {
    pub folders: Vec<DirId>,
    pub books: Vec<BookId>,
    pub source: Option<DirId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferBook {
    pub content_hash: BookId,
    /// Provider identities survive replacements, so moves also pin the file revision.
    #[serde(default)]
    pub provider_file: Option<ProviderFile>,
    pub directory: DirId,
    pub file_name: String,
    /// Direct selections go into the destination itself; tree placements keep their parent.
    pub direct: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPlan {
    pub selection: TransferSelection,
    pub roots: Vec<DirId>,
    /// Parents precede their children, including empty folders.
    pub folders: Vec<LibraryFolderDestination>,
    pub books: Vec<TransferBook>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookTransferState {
    pub content_hash: BookId,
    #[serde(default)]
    pub byte_checksum: Option<ContentHash>,
    #[serde(default)]
    pub provider_file: Option<ProviderFile>,
    pub addition: BookAddition,
    pub reading: Option<LibraryStateEntry>,
    #[serde(default)]
    pub chapter_read: Option<LibraryStateEntry>,
    pub annotations: Vec<Annotation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportedBook {
    pub content_hash: BookId,
    pub created: bool,
}

impl BookTransferState {
    pub fn expected_checksum(&self) -> Option<ContentHash> {
        self.byte_checksum.or_else(|| self.provider_file.as_ref().and_then(|file| file.content_hash)).or_else(|| self.content_hash.content_hash())
    }
    pub fn validate_source(&self) -> Result<(), DatabaseError> {
        if self.content_hash.is_external() {
            let file = self.provider_file.as_ref().ok_or_else(|| DatabaseError::message("transferred provider book has no file reference"))?;
            file.validate().map_err(DatabaseError::operation)?;
            if file.identity.book_id() != self.content_hash || file.content_hash.zip(self.byte_checksum).is_some_and(|(expected, actual)| expected != actual) {
                return Err(DatabaseError::message("transferred provider identity or checksum is invalid"));
            }
        } else if self.byte_checksum.is_some_and(|actual| Some(actual) != self.content_hash.content_hash()) {
            return Err(DatabaseError::message("transferred checksum does not match its content identity"));
        }
        Ok(())
    }

    /// Only call after verifying complete bytes. Reading state follows those
    /// bytes when the destination assigns a different storage identity.
    pub fn retarget(mut self, destination: BookId) -> Result<Self, DatabaseError> {
        self.validate_source()?;
        let original = self.content_hash;
        self.byte_checksum = self.expected_checksum();
        if destination.content_hash().zip(self.byte_checksum).is_some_and(|(target, original)| target != original) {
            return Err(DatabaseError::message("destination bytes differ from the transferred source"));
        }
        if let Some(pdf) = &mut self.addition.pdf {
            if !pdf.valid_for(&original.to_hex()) {
                return Err(DatabaseError::message("transferred PDF metadata does not match the source"));
            }
            pdf.content_hash = destination.to_hex();
        }
        if let Some(tracks) = self.addition.navigation.as_mut().and_then(|navigation| navigation.tracks.as_mut()) {
            for track in tracks {
                if let Some(id) = &mut track.archive_hash {
                    if *id != original {
                        return Err(DatabaseError::message("transferred audio index does not match the source"));
                    }
                    *id = destination;
                }
            }
        }
        for (entry, kind) in [(&mut self.reading, "reading_position"), (&mut self.chapter_read, "chapter_read_status")] {
            if let Some(entry) = entry {
                if entry.book() != Some(original) || entry.kind() != kind {
                    return Err(DatabaseError::message("transferred reading state does not match the source"));
                }
                match entry {
                    LibraryStateEntry::ReadingPosition { content_hash, .. } | LibraryStateEntry::ChapterReadStatus { content_hash, .. } => *content_hash = destination,
                    _ => return Err(DatabaseError::message("invalid transferred reading state")),
                }
            }
        }
        for annotation in &mut self.annotations {
            if annotation.content_hash != original {
                return Err(DatabaseError::message("transferred annotation does not match the source"));
            }
            annotation.content_hash = destination;
        }
        self.content_hash = destination;
        self.provider_file = None;
        Ok(self)
    }
}
impl Database {
    /// Active contents only; copying storage never mutates this source.
    pub fn storage_copy_plan(&self) -> Result<TransferPlan, DatabaseError> {
        self.with_browse_snapshot(|| {
            let folders = self.folder_destinations()?.into_iter().filter(|folder| folder.parent_id == Some(ROOT_DIR_ID)).map(|folder| folder.id).collect();
            let mut query = self.connection.prepare("SELECT b.content_hash FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id WHERE bd.dir_id=?1 AND bd.deleted_at IS NULL AND b.deleted_at IS NULL ORDER BY b.content_hash")?;
            let books = query.query_map([ROOT_DIR_ID.to_string()], |row| row.get(0))?.collect::<Result<Vec<_>, _>>()?;
            let selection = TransferSelection { folders, books, source: Some(ROOT_DIR_ID) };
            if selection.folders.is_empty() && selection.books.is_empty() {
                return Ok(TransferPlan { selection, roots: Vec::new(), folders: Vec::new(), books: Vec::new() });
            }
            self.transfer_plan_inner(selection)
        })
    }

    /// Retain an existing destination placement's name when the same hash is imported again.
    pub fn transfer_destination_name(&self, hash: &BookId, directory: &DirId) -> Result<Option<String>, DatabaseError> {
        Ok(self
            .connection
            .query_row(
                "SELECT bd.file_name FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id WHERE bd.dir_id=?1 AND b.content_hash=?2 AND bd.deleted_at IS NULL AND b.deleted_at IS NULL",
                rusqlite::params![directory.to_string(), hash],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn transfer_plan(&self, selection: TransferSelection) -> Result<TransferPlan, DatabaseError> {
        self.with_browse_snapshot(|| self.transfer_plan_inner(selection))
    }
    fn transfer_plan_inner(&self, selection: TransferSelection) -> Result<TransferPlan, DatabaseError> {
        if selection.folders.contains(&ROOT_DIR_ID) || (selection.folders.is_empty() && selection.books.is_empty()) {
            return Err(DatabaseError::message("select books or folders to transfer"));
        }
        let all = self.folder_destinations()?;
        let by_id: HashMap<_, _> = all.iter().map(|folder| (folder.id, folder)).collect();
        let selected: HashSet<_> = selection.folders.iter().copied().collect();
        let mut roots = Vec::new();
        for id in &selection.folders {
            let folder = by_id.get(id).ok_or_else(|| DatabaseError::message("source folder is unavailable"))?;
            let mut parent = folder.parent_id;
            let mut nested = false;
            let mut visited = HashSet::new();
            while let Some(id) = parent {
                if !visited.insert(id) {
                    return Err(DatabaseError::message("invalid source folder hierarchy"));
                }
                if selected.contains(&id) {
                    nested = true;
                    break;
                }
                parent = by_id.get(&id).and_then(|folder| folder.parent_id);
            }
            if !nested && !roots.contains(id) {
                roots.push(*id);
            }
        }
        let mut scope: HashSet<_> = roots.iter().copied().collect();
        let mut folders = Vec::new();
        for id in &roots {
            folders.push((*by_id[id]).clone());
        }
        let mut index = 0;
        while index < folders.len() {
            let parent = folders[index].id;
            for folder in &all {
                if folder.parent_id == Some(parent) && scope.insert(folder.id) {
                    folders.push(folder.clone());
                }
            }
            index += 1;
        }
        let mut statement = self.connection.prepare_cached("SELECT b.content_hash, bd.dir_id, bd.file_name FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id JOIN dir d ON d.id=bd.dir_id WHERE bd.deleted_at IS NULL AND b.deleted_at IS NULL AND d.deleted_at IS NULL AND (bd.dir_id IN (SELECT value FROM json_each(?1)) OR b.content_hash IN (SELECT unhex(value) FROM json_each(?2))) ORDER BY bd.dir_id,b.content_hash")?;
        let scope_json = serde_json::to_string(&scope).map_err(DatabaseError::operation)?;
        let books_json = serde_json::to_string(&selection.books).map_err(DatabaseError::operation)?;
        let placements = statement
            .query_map(rusqlite::params![scope_json, books_json], |row| {
                let id: String = row.get(1)?;
                let directory = id.parse().map_err(|error| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(error)))?;
                Ok(TransferBook { content_hash: row.get(0)?, provider_file: None, directory, file_name: row.get(2)?, direct: false })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut books: Vec<_> = placements.iter().filter(|book| scope.contains(&book.directory)).cloned().collect();
        for hash in &selection.books {
            let placement = placements.iter().find(|book| book.content_hash == *hash && selection.source.is_none_or(|source| source == book.directory)).ok_or_else(|| DatabaseError::message("source book placement is unavailable"))?;
            if !scope.contains(&placement.directory) && !books.iter().any(|book| book.direct && book.content_hash == *hash) {
                books.push(TransferBook { direct: true, ..placement.clone() });
            }
        }
        for book in &mut books {
            if book.content_hash.is_external() {
                book.provider_file = Some(self.provider_file(&book.content_hash)?.ok_or_else(|| DatabaseError::message("source provider file is unavailable"))?);
            }
        }
        Ok(TransferPlan { selection, roots, folders, books })
    }
    /// Revalidate and remove the entire source selection in one transaction.
    /// Copying a stale snapshot never discards changes made during the transfer.
    pub fn remove_transferred_selection(&self, plan: &TransferPlan) -> Result<(), DatabaseError> {
        self.write(|tx| {
            if self.transfer_plan_inner(plan.selection.clone())? != *plan {
                return Err(DatabaseError::message("source changed while copying; copied items were kept and the source was left unchanged"));
            }
            for root in &plan.roots {
                crate::placements::trash_directory_in(tx, root)?;
                crate::projection::project_dirty(tx)?;
            }
            for book in plan.books.iter().filter(|book| book.direct) {
                crate::placements::remove_book_placement_in(tx, &book.content_hash, &book.directory)?;
                crate::projection::project_dirty(tx)?;
            }
            Ok(())
        })
    }
    pub fn book_transfer_state(&self, hash: BookId) -> Result<BookTransferState, DatabaseError> {
        self.with_browse_snapshot(|| {
            let kind = if hash.is_external() { "external_book" } else { "book_add" };
            let bytes = match canonical_body(&self.connection, kind, &StateKey::Book(hash), "")? {
                Some((body, _)) => body,
                None => {
                    self.connection.query_row("SELECT body FROM local_book_draft WHERE content_hash=?1", [hash], |row| row.get::<_, Vec<u8>>(0)).optional()?.ok_or_else(|| DatabaseError::message("source book metadata is unavailable"))?
                }
            };
            let (content_hash, addition, provider_file) = match sync_common::wire::decode(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? {
                LibraryStateEntry::BookAdd { content_hash, value } => (content_hash, value, None),
                LibraryStateEntry::ExternalBook { content_hash, value } => (content_hash, value.addition, Some(value.file)),
                _ => return Err(DatabaseError::message("source book metadata is invalid")),
            };
            if hash != content_hash {
                return Err(DatabaseError::message("source book identity is invalid"));
            }
            let reading = canonical_body(&self.connection, "reading_position", &StateKey::Book(hash), "")?
                .map(|(bytes, _)| sync_common::wire::decode(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation))
                .transpose()?;
            let chapter_read = canonical_body(&self.connection, "chapter_read_status", &StateKey::Book(hash), "")?
                .map(|(bytes, _)| sync_common::wire::decode(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation))
                .transpose()?;
            let byte_checksum = hash.content_hash().or_else(|| provider_file.as_ref().and_then(|file| file.content_hash));
            let state = BookTransferState { content_hash: hash, byte_checksum, provider_file, addition, reading, chapter_read, annotations: self.annotations(hash)? };
            state.validate_source()?;
            Ok(state)
        })
    }

    pub fn merge_transferred_reader_state(&self, destination: BookId, source: BookTransferState) -> Result<(), DatabaseError> {
        let source = source.retarget(destination)?;
        self.write(|tx| merge_transfer_progress(tx, destination, source.reading, source.chapter_read, source.annotations))
    }
}

pub(crate) fn merge_transfer_progress(tx: &rusqlite::Transaction<'_>, book: BookId, reading: Option<LibraryStateEntry>, chapter_read: Option<LibraryStateEntry>, annotations: Vec<Annotation>) -> Result<(), DatabaseError> {
    use crate::sync::apply::registers::record_local;
    for (entry, kind) in [(reading, "reading_position"), (chapter_read, "chapter_read_status")] {
        if let Some(entry) = entry {
            if entry.book() != Some(book) || entry.kind() != kind {
                return Err(DatabaseError::message("invalid transferred reading state"));
            }
            if canonical_body(tx, kind, &StateKey::Book(book), "")?.is_none() {
                record_local(tx, entry)?;
            }
        }
    }
    for mut annotation in annotations {
        if annotation.content_hash != book {
            return Err(DatabaseError::message("invalid transferred annotation"));
        }
        let original_id = annotation.id.clone();
        for attempt in 0_u64.. {
            match canonical_body(tx, "annotation", &StateKey::Text(annotation.id.clone()), "")? {
                None => {
                    record_local(tx, LibraryStateEntry::Annotation { value: annotation })?;
                    break;
                }
                Some((body, _)) => {
                    let existing: LibraryStateEntry = sync_common::wire::decode(&body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)?;
                    if existing.book() == Some(book) {
                        break;
                    }
                    // Annotation keys belong to the library, while provider
                    // copies receive new book identities. Keep the original
                    // note and give this copy a stable key so retries also
                    // preserve edits or deletion of the destination note.
                    annotation.id = Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("bokheim/transfer-annotation/{book}/{original_id}/{attempt}").as_bytes()).to_string();
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::IngestionSql;
    use book_model::{AnnotationAnchor, AnnotationStyle};

    #[test]
    fn copying_a_note_to_another_identity_preserves_both_notes_and_retries_do_not_restore_deleted_copies() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let original = BookId::from(blake3::hash(b"original"));
        let destination = BookId::from(blake3::hash(b"copy"));
        db.write(|tx| {
            tx.ingestion_ensure_book(&original)?;
            tx.ingestion_ensure_book(&destination)?;
            Ok(())
        })
        .unwrap();
        let note = Annotation {
            id: "shared-note".into(),
            content_hash: original,
            anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/2)"),
            exact_text: "Text".into(),
            style: AnnotationStyle::Underline,
            color: 0xffffff,
            note: "Original note".into(),
            created_at: 7,
            toc_ordinal: None,
            progress: Some(42.0),
            modified_at: 9,
            deleted: false,
        };
        db.upsert_annotation(&note).unwrap();
        let mut copy = note.clone();
        copy.content_hash = destination;
        db.write(|tx| merge_transfer_progress(tx, destination, None, None, vec![copy.clone()])).unwrap();
        assert_eq!(db.annotations(original).unwrap(), vec![note.clone()]);
        let mut copied = db.annotations(destination).unwrap().pop().unwrap();
        assert_ne!(copied.id, note.id);
        let mut expected = copy.clone();
        expected.id = copied.id.clone();
        assert_eq!(copied, expected);

        copied.note = "Edited copy".into();
        copied.modified_at = 10;
        db.upsert_annotation(&copied).unwrap();
        db.write(|tx| merge_transfer_progress(tx, destination, None, None, vec![copy.clone()])).unwrap();
        assert_eq!(db.annotations(destination).unwrap(), vec![copied.clone()]);
        db.delete_annotation(&copied.id, 11).unwrap();
        db.write(|tx| merge_transfer_progress(tx, destination, None, None, vec![copy.clone()])).unwrap();
        // The canonical tombstone still owns the generated key.
        assert!(db.annotations(destination).unwrap().is_empty());
        assert_eq!(db.annotations(original).unwrap(), vec![note]);
    }
}
