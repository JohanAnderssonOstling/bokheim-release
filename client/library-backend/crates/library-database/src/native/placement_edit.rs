//! Native command planning without publishing a placement before its bytes exist.
use super::{NativeDirectorySnapshot, NativeFileWorkSnapshot, NativeFileWorkSql};
use crate::browse::revision::Revision;
use crate::native::observations::{self, Revision as FileRevision};
use crate::placements::{copy_book_to_directory_in, copy_directory_in, live_file_name, move_directory_in, placement_path, queue_file_work, restore_book_placement_in, stored_book_path, transfer_book_placement_in};
use crate::projection::project_dirty;
use crate::transactions::WriteOutcome::Rollback;
use crate::{Database, DatabaseError};
use library_model::RelativeBookPath;
use rusqlite::OptionalExtension;
use std::collections::HashSet;
use sync_common::{BookId, DirId};

#[derive(Clone)]
pub struct NativeMoveSource {
    pub book: BookId,
    pub directory: DirId,
    pub original_path: RelativeBookPath,
    pub physical_path: RelativeBookPath,
}

#[derive(Clone, Debug)]
pub enum NativePlacementEdit {
    CopyBook { book: BookId, source: Option<DirId>, destination: DirId },
    RestoreBookPlacement { book: BookId, destination: DirId },
    TransferBook { book: BookId, source: DirId, destination: DirId, moving: bool },
    CopyDirectory { source: DirId, destination: DirId },
    MoveDirectory { directory: DirId, parent: Option<DirId>, name: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativePlacementResult {
    CopiedBook(bool),
    RestoredBookPlacement,
    TransferredBook { name: String, created: bool },
    CopiedDirectory(DirId),
    MovedDirectory(String),
}

#[derive(Clone)]
pub struct NativePlacementPlan {
    edit: NativePlacementEdit,
    revision: Revision,
    directory_ids: Vec<DirId>,
    pub result: NativePlacementResult,
    pub directories: Vec<NativeDirectorySnapshot>,
    pub files: Vec<NativeFileWorkSnapshot>,
    pub move_source: Option<NativeMoveSource>,
}

impl Database {
    /// Preview the existing logical command in a rolled-back transaction. The
    /// returned paths and generated folder IDs are stable inputs to slow I/O;
    /// neither placements nor sync publications escape this transaction.
    pub fn plan_native_placement(&self, edit: NativePlacementEdit) -> Result<NativePlacementPlan, DatabaseError> {
        self.with_write_transaction(|tx| {
            let revision = Revision::read(tx)?;
            let file_ids = self.native_file_work()?.into_iter().map(|work| work.id).collect::<HashSet<_>>();
            let directory_work = self.native_directory_replay_work()?.into_iter().collect::<HashSet<_>>();
            let mut affected = Vec::new();
            let mut move_source = None;
            if let NativePlacementEdit::TransferBook { book, source, moving: true, .. } = &edit {
                if let Some(name) = live_file_name(tx, source, book)? {
                    let original_path = placement_path(tx, source, &name)?;
                    let projected = tx.query_row("SELECT relative_path FROM local_file_projection WHERE content_hash=?1 AND dir_id=?2", (book, source.to_string()), |row| row.get::<_, String>(0)).optional()?;
                    let physical_path = projected.map(|path| stored_book_path(&path)).transpose()?.unwrap_or_else(|| original_path.clone());
                    affected.push((*book, original_path.clone()));
                    move_source = Some(NativeMoveSource { book: *book, directory: *source, original_path, physical_path });
                }
            }
            let mut directory_ids = Vec::new();
            let result = apply(tx, &edit, &mut || {
                let id = DirId::new_v4();
                directory_ids.push(id);
                id
            })?;
            project_dirty(tx)?;
            let directories = self
                .native_directory_replay_work()?
                .into_iter()
                .filter(|work| !directory_work.contains(work))
                .map(|(_, id)| self.native_directory_snapshot(&id)?.ok_or_else(|| DatabaseError::message("folder changed while planning")))
                .collect::<Result<Vec<_>, _>>()?;
            match &edit {
                NativePlacementEdit::CopyBook { book, destination, .. } | NativePlacementEdit::TransferBook { book, destination, .. } | NativePlacementEdit::RestoreBookPlacement { book, destination } => {
                    if let Some(name) = live_file_name(tx, destination, book)? {
                        affected.push((*book, placement_path(tx, destination, &name)?));
                    }
                }
                _ => {}
            }
            let files = self
                .native_file_work()?
                .into_iter()
                .filter(|work| !file_ids.contains(&work.id) || affected.iter().any(|(hash, path)| *hash == work.content_hash && path == &work.original_path))
                .map(|work| self.native_file_work_snapshot(work)?.ok_or_else(|| DatabaseError::message("placement changed while planning")))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Rollback(NativePlacementPlan { edit, revision, directory_ids, result, directories, files, move_source }))
        })
    }

    /// Remember both possible source paths before retiring bytes for a long
    /// move. Until the placement commits, replay restores the original path;
    /// after commit, that restore is obsolete and replay removes the staging
    /// file. This also recovers a crash between the rename and final commit.
    pub fn record_native_move_staging(&self, plan: &mut NativePlacementPlan, staged: &RelativeBookPath) -> Result<(), DatabaseError> {
        let source = plan.move_source.as_ref().ok_or_else(|| DatabaseError::message("operation has no move source"))?;
        let revision = self.write(|tx| {
            let current = Revision::read(tx)?;
            if current.folders != plan.revision.folders || current.visibility != plan.revision.visibility || current.availability != plan.revision.availability {
                return Err(DatabaseError::message("the source or destination changed during the operation; please try again"));
            }
            let name = live_file_name(tx, &source.directory, &source.book)?.ok_or_else(|| DatabaseError::message("move source changed; please try again"))?;
            if placement_path(tx, &source.directory, &name)? != source.original_path {
                return Err(DatabaseError::message("move source changed; please try again"));
            }
            let projected = tx.query_row("SELECT relative_path FROM local_file_projection WHERE content_hash=?1 AND dir_id=?2", (&source.book, source.directory.to_string()), |row| row.get::<_, String>(0)).optional()?;
            let current = projected.map(|path| stored_book_path(&path)).transpose()?.unwrap_or_else(|| source.original_path.clone());
            if current != source.physical_path {
                return Err(DatabaseError::message("move source changed; please try again"));
            }
            tx.execute(
                "INSERT INTO local_file_projection(content_hash,dir_id,relative_path) VALUES(?1,?2,?3) ON CONFLICT(content_hash,dir_id) DO UPDATE SET relative_path=excluded.relative_path",
                (&source.book, source.directory.to_string(), staged.as_str()),
            )?;
            queue_file_work(tx, "materialize", &source.book, &source.original_path)?;
            queue_file_work(tx, "remove", &source.book, staged)?;
            Revision::read(tx)
        })?;
        plan.revision = revision;
        Ok(())
    }

    /// All expensive copying/verification happens before this call. `publish`
    /// may perform short renames only, and an I/O error rolls back the logical
    /// command. Remaining removal jobs retire a cross-device move's source
    /// only after the destination placement has committed.
    pub fn commit_native_placement(&self, plan: NativePlacementPlan, materialized_files: &[String], materialized_directories: &[DirId], publish: impl FnOnce() -> Result<(), DatabaseError>) -> Result<NativePlacementResult, DatabaseError> {
        let materialized_files = materialized_files.iter().map(String::as_str).collect::<HashSet<_>>();
        let materialized_directories = materialized_directories.iter().copied().collect::<HashSet<_>>();
        self.write(|tx| {
            let current = Revision::read(tx)?;
            if current.folders != plan.revision.folders || current.visibility != plan.revision.visibility || current.availability != plan.revision.availability {
                return Err(DatabaseError::message("the source or destination changed during the operation; please try again"));
            }
            let mut ids = plan.directory_ids.iter().copied();
            let result = apply(tx, &plan.edit, &mut || ids.next().expect("unchanged folder tree uses its planned IDs"))?;
            project_dirty(tx)?;
            if result != plan.result {
                return Err(DatabaseError::message("placement changed during the operation; please try again"));
            }
            publish()?;
            for directory in &plan.directories {
                if !materialized_directories.contains(&directory.id) {
                    continue;
                }
                if let Some(previous) = &directory.projected_path {
                    if previous != &directory.relative_path {
                        let prefix = format!("{}/", previous.trim_end_matches('/'));
                        let replacement = format!("{}/", directory.relative_path.trim_end_matches('/'));
                        let upper = format!("{prefix}\u{10ffff}");
                        tx.native_move_projection_paths(&prefix, &replacement, &upper)?;
                        tx.native_move_directory_projection_paths(&prefix, &replacement, &upper)?;
                        tx.native_move_file_work_paths(&prefix, &replacement, &upper)?;
                    }
                }
                tx.native_set_directory_projection(&directory.id.to_string(), &directory.relative_path)?;
            }
            for file in &plan.files {
                if file.work.operation != "materialize" || !materialized_files.contains(file.resolved_path.as_str()) {
                    continue;
                }
                let (directory, _) = file.placement.as_ref().ok_or_else(|| DatabaseError::message("copy destination is no longer present"))?;
                observations::record_file(tx, directory, &file.work.content_hash, file.resolved_path.as_str(), None, FileRevision::Retained)?;
                // The preview's queue IDs were rolled back and can be reused by
                // other work. Acknowledge the current job by its full identity.
                tx.execute("DELETE FROM local_file_work WHERE operation='materialize' AND content_hash=?1 AND relative_path=?2", (&file.work.content_hash, file.work.original_path.as_str()))?;
            }
            Ok(result)
        })
    }
}

fn apply(tx: &rusqlite::Transaction<'_>, edit: &NativePlacementEdit, next_id: &mut impl FnMut() -> DirId) -> Result<NativePlacementResult, DatabaseError> {
    Ok(match edit {
        NativePlacementEdit::CopyBook { book, source, destination } => NativePlacementResult::CopiedBook(copy_book_to_directory_in(tx, book, source.as_ref(), destination)?),
        NativePlacementEdit::RestoreBookPlacement { book, destination } => {
            restore_book_placement_in(tx, book, destination, true)?;
            NativePlacementResult::RestoredBookPlacement
        }
        NativePlacementEdit::TransferBook { book, source, destination, moving } => {
            let created = live_file_name(tx, destination, book)?.is_none();
            NativePlacementResult::TransferredBook { name: transfer_book_placement_in(tx, book, source, destination, *moving)?, created }
        }
        NativePlacementEdit::CopyDirectory { source, destination } => NativePlacementResult::CopiedDirectory(copy_directory_in(tx, source, destination, next_id)?),
        NativePlacementEdit::MoveDirectory { directory, parent, name } => NativePlacementResult::MovedDirectory(move_directory_in(tx, directory, parent.as_ref(), name.as_deref())?),
    })
}
