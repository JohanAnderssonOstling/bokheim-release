//! Sequential import orchestration shared by direct readers and durable staging.
//! Adapters own source access and checkpoints; the workflow owns progress cadence
//! and always releases the activity after an attempted batch.
use std::collections::BTreeMap;
use sync_common::DirId;

pub trait Batch {
    type Error;
    fn len(&self) -> usize;
    async fn process(&mut self, index: usize) -> Result<(), Self::Error>;
    async fn progress(&mut self) -> Result<(), Self::Error>;
    async fn finish(&mut self) -> Result<(), Self::Error>;
}

pub async fn run<B: Batch>(batch: &mut B) -> Result<(), B::Error> {
    let result = async {
        batch.progress().await?;
        let mut last_progress = web_time::Instant::now();
        for index in 0..batch.len() {
            batch.process(index).await?;
            if last_progress.elapsed() >= PROGRESS_INTERVAL {
                batch.progress().await?;
                last_progress = web_time::Instant::now();
            }
        }
        batch.progress().await
    }
    .await;
    let finished = batch.finish().await;
    result?;
    finished
}

pub(crate) const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

// A report belongs to one live import activity. Recovery starts a new reporter
// and supplies the outcomes restored from its journal.
#[derive(Default)]
pub struct ProgressReporter {
    reported: Option<(u64, u64)>,
}

pub struct ProgressDelta {
    pub total: u64,
    pub succeeded: u64,
    pub failed: u64,
}

impl ProgressReporter {
    pub fn advance(&mut self, total: u64, succeeded: u64, failed: u64) -> Result<Option<ProgressDelta>, String> {
        let (previous_succeeded, previous_failed) = self.reported.unwrap_or_default();
        let succeeded_delta = succeeded.checked_sub(previous_succeeded).ok_or("reported import results changed")?;
        let failed_delta = failed.checked_sub(previous_failed).ok_or("reported import results changed")?;
        if self.reported.is_some() && succeeded_delta == 0 && failed_delta == 0 {
            return Ok(None);
        }
        let total = if self.reported.is_none() { total } else { 0 };
        self.reported = Some((succeeded, failed));
        Ok(Some(ProgressDelta { total, succeeded: succeeded_delta, failed: failed_delta }))
    }
}

pub fn file_failure(path: &[String], error: &str) -> crate::ImportFailure {
    log::warn!("Could not import {}: {error}", path.join("/"));
    crate::ImportFailure::operation(crate::ImportFailureKind::File, path, error)
}

pub fn folder_failure(path: &[String], error: &str) -> crate::ImportFailure {
    log::warn!("Could not import folder {}: {error}", path.join("/"));
    crate::ImportFailure::operation(crate::ImportFailureKind::Folder, path, error)
}

#[derive(Default)]
pub struct Destinations(BTreeMap<Vec<String>, DirId>);

impl Destinations {
    pub fn new(directories: Vec<(Vec<String>, DirId)>) -> Self {
        Self(directories.into_iter().collect())
    }

    pub fn resolve<'a>(&self, path: &'a [String]) -> Result<(DirId, &'a str), String> {
        let (name, parent) = path.split_last().ok_or("import file path is empty")?;
        let parent = *self.0.get(parent).ok_or("Parent folder could not be imported")?;
        Ok((parent, name))
    }
}

pub fn supported(path: &[String]) -> bool {
    path.last().is_some_and(|name| book_model::BookFormat::from_import_extension(name.rsplit_once('.').map(|(_, extension)| extension).unwrap_or_default()).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_preserve_category_path_and_diagnostic() {
        let path = vec!["Shelf".into(), "Book.epub".into()];
        let failure = file_failure(&path, "io error");
        assert_eq!(failure.kind, crate::ImportFailureKind::File);
        assert_eq!(failure.path, path);
        assert_eq!(failure.diagnostic.as_deref(), Some("io error"));
        assert_eq!(folder_failure(&[], "io error").kind, crate::ImportFailureKind::Folder);
    }

    #[test]
    fn selected_files_use_the_same_format_rules_on_both_platforms() {
        for name in ["book.epub", "book.pdf", "book.m4b", "book.EPUB"] {
            assert!(supported(&["Shelf".into(), name.into()]), "{name}");
        }
        for name in ["cover.jpg", "book.epub.tmp", "epub", ""] {
            assert!(!supported(&[name.into()]), "{name}");
        }
        assert!(!supported(&[]));
    }
}
