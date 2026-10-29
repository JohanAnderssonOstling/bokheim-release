//! Copy first, then atomically remove a revalidated source selection.
use super::LibraryClient;
use crate::library::{LibraryCommand, ReadyCommand};
use crate::BackendError;
use library_database::transfer::TransferPlan;
pub use library_database::transfer::TransferSelection;
use std::collections::HashMap;
use sync_common::{BookId, DirId};

#[derive(Default, Debug)]
pub struct LibraryTransferOutcome {
    pub created_folders: Vec<DirId>,
    pub created_placements: Vec<(BookId, DirId)>,
    pub removed_folders: Vec<(DirId, Option<DirId>)>,
    pub removed_placements: Vec<(BookId, DirId)>,
    /// Completed destination changes remain undoable after a partial failure.
    pub error: Option<BackendError>,
}
impl LibraryClient {
    pub async fn transfer_from(&self, source: &LibraryClient, selection: TransferSelection, destination: DirId, move_source: bool) -> LibraryTransferOutcome {
        let mut outcome = LibraryTransferOutcome::default();
        if let Err(error) = self.transfer_from_inner(source, selection, destination, move_source, &mut outcome).await {
            outcome.error = Some(error);
        }
        outcome
    }
    async fn transfer_from_inner(&self, source: &LibraryClient, selection: TransferSelection, destination: DirId, move_source: bool, outcome: &mut LibraryTransferOutcome) -> Result<(), BackendError> {
        if self.id() == source.id() {
            return Err("use ordinary placement operations within one library".into());
        }
        if !self.folder_destinations().await?.iter().any(|folder| folder.id == destination) {
            return Err("destination folder is unavailable".into());
        }
        let plan: TransferPlan = source.request(LibraryCommand::Ready(ReadyCommand::TransferPlan { selection })).await?;
        let mut folders = HashMap::new();
        for folder in &plan.folders {
            let parent = if plan.roots.contains(&folder.id) { destination } else { *folders.get(&folder.parent_id.ok_or("source folder has no parent")?).ok_or("source folder hierarchy is incomplete")? };
            let copy: DirId = self.request(LibraryCommand::Ready(ReadyCommand::CreateDirectoryWithId { parent_id: parent, name: folder.label.clone() })).await?;
            folders.insert(folder.id, copy);
            if plan.roots.contains(&folder.id) {
                outcome.created_folders.push(copy);
            }
        }
        for book in &plan.books {
            let parent = if book.direct { destination } else { *folders.get(&book.directory).ok_or("destination folder hierarchy is incomplete")? };
            let resource = source.transport.handle().await?.export_transfer_book(book.content_hash).await?;
            let imported = self.transport.handle().await?.import_transfer_book(parent, book.file_name.clone(), resource).await?;
            // Entire imported trees are already covered by their root's undo.
            if imported.created && book.direct {
                outcome.created_placements.push((imported.content_hash, parent));
            }
        }
        // No source mutation occurs until ALL destination commits succeed.
        if move_source {
            source.request::<()>(LibraryCommand::Ready(ReadyCommand::RemoveTransferredSelection { plan: plan.clone() })).await?;
            for root in &plan.roots {
                let folder = plan.folders.iter().find(|folder| folder.id == *root).ok_or("source root is missing")?;
                outcome.removed_folders.push((*root, folder.parent_id));
            }
            outcome.removed_placements.extend(plan.books.iter().filter(|book| book.direct).map(|book| (book.content_hash, book.directory)));
        }
        Ok(())
    }
}
