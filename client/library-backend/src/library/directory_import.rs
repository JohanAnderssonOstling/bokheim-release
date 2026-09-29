//! One native backend job owns the complete selected-directory import.
use super::{DirectoryImport, LibrarySession};
use import_pipeline::import_workflow::{Destinations, ProgressReporter};

impl LibrarySession {
    pub(crate) async fn import_selected_directory(
        &self, parent_id: crate::DirId, directory: DirectoryImport, create_root: bool, activity_id: Option<uuid::Uuid>, cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<crate::ImportFailure>, crate::BackendError> {
        if cancelled() {
            return Err(crate::BackendError::message("directory import cancelled"));
        }
        let activity = self.directory_import_activity_with_id(activity_id.unwrap_or_else(uuid::Uuid::new_v4));
        let DirectoryImport { name, directories, mut files } = directory;
        let single_book = !create_root && directories.is_empty() && files.len() == 1;
        let prepared = self.prepare_directory_import(&parent_id, create_root.then_some(name), directories).await?;
        let parents = Destinations::new(prepared.directories);
        let mut failures = prepared.failures;
        files.retain(|file| import_pipeline::import_workflow::supported(&file.path));
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut batch =
            DirectBatch { cancelled, library: self, files: files.into_iter().map(Some).collect(), parents, single_book, activity: activity.id, progress: ProgressReporter::default(), succeeded: 0, failed: 0, failures: &mut failures };
        import_pipeline::import_workflow::run(&mut batch).await?;
        Ok(failures)
    }
}

struct DirectBatch<'a> {
    cancelled: &'a dyn Fn() -> bool,
    library: &'a LibrarySession,
    files: Vec<Option<crate::DirectoryImportFile>>,
    parents: Destinations,
    single_book: bool,
    activity: uuid::Uuid,
    progress: ProgressReporter,
    succeeded: u64,
    failed: u64,
    failures: &'a mut Vec<crate::ImportFailure>,
}
impl import_pipeline::import_workflow::Batch for DirectBatch<'_> {
    type Error = crate::BackendError;
    fn len(&self) -> usize {
        self.files.len()
    }
    async fn process(&mut self, index: usize) -> Result<(), crate::BackendError> {
        if (self.cancelled)() {
            return Err(crate::BackendError::message("directory import cancelled"));
        }
        let file = self.files[index].take().expect("import item processed once");
        let observer = self.library.start_import_file(self.activity, file.path.last().cloned().unwrap_or_default(), file.source.size_bytes());
        let result = async {
            let (parent_id, name) = self.parents.resolve(&file.path)?;
            let reader = file.source.open().await?;
            let result = self.library.import_book_reader_with_progress(&parent_id, reader, name.to_owned(), self.single_book, observer).await;
            result
        }
        .await;
        if result.is_ok() {
            self.succeeded += 1;
        } else {
            self.failed += 1;
        }
        if let Err(error) = result {
            let summary = import_pipeline::import_workflow::file_failure(&file.path, &error.to_string());
            self.failures.push(summary);
        }
        self.library.finish_import_file(self.activity);
        self.progress().await?;
        Ok(())
    }
    async fn progress(&mut self) -> Result<(), crate::BackendError> {
        if let Some(update) = self.progress.advance(self.files.len() as u64, self.succeeded, self.failed)? {
            self.library.advance_directory_import(self.activity, update.total, update.succeeded, update.failed);
        }
        Ok(())
    }
    async fn finish(&mut self) -> Result<(), crate::BackendError> {
        self.progress().await
    }
}
