//! Durable staged import workflow over typed storage and library services.
use crate::import_journal::{self, Journal, Progress};
use client_runtime::BackendError;
use serde::{Deserialize, Serialize};
use sync_common::{ContentHash, DirId, LibraryId};

#[derive(Debug)]
pub struct Error {
    pub source: BackendError,
    pub transport: bool,
    pub storage_full: bool,
}
fn error(value: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Error {
    Error { source: BackendError::operation(value), transport: false, storage_full: false }
}
pub struct StagedBook {
    pub parent_id: DirId,
    pub file_name: String,
    pub physical: String,
    pub length: u64,
    pub hash: ContentHash,
}
pub trait FileAccess {
    fn read(&self) -> Result<Vec<u8>, Error>;
    fn write(&self, bytes: &[u8], offset: u64) -> Result<(), Error>;
    fn truncate(&self, offset: u64) -> Result<(), Error>;
    fn flush(&self) -> Result<(), Error>;
    fn persist(&self, state: &Journal, bytes: Vec<u8>) -> Result<(), Error> {
        self.write(&bytes, (state.offset - bytes.len()) as u64)?;
        self.flush()
    }
}
pub trait Storage {
    type File: FileAccess;
    fn staged_path(&self, manifest: &Manifest, index: usize) -> String;
    async fn open_file(&self, name: &str) -> Result<Self::File, Error>;
    fn source_metadata(&self, index: u32) -> Result<(u64, f64), Error>;
    async fn read_source(&self, index: u32, start: u64, end: u64) -> Result<Vec<u8>, Error>;
}
/// The library's answer to preparation: resolved directories plus early failures.
pub struct PreparedImport {
    pub directories: Vec<(Vec<String>, DirId)>,
    pub failures: Vec<crate::ImportFailure>,
}
/// One debounced progress report for the library to publish.
pub struct AdvanceProgress {
    pub activity: uuid::Uuid,
    pub total: u64,
    pub succeeded: u64,
    pub failed: u64,
}
pub trait Library {
    async fn prepare(&self, activity: uuid::Uuid, manifest: &Manifest) -> Result<PreparedImport, Error>;
    async fn import(&self, book: StagedBook) -> Result<(), Error>;
    async fn advance(&self, progress: AdvanceProgress) -> Result<(), Error>;
    async fn finish(&self, activity: uuid::Uuid) -> Result<(), Error>;
    async fn cleanup(&self, job: String, files: u32) -> Result<(), Error>;
}
#[derive(Clone, Copy)]
pub enum Phase {
    Copying,
    Adding,
    Complete,
}
pub struct Update {
    pub id: String,
    pub name: String,
    pub phase: Phase,
    pub counts: Progress,
    pub current_file: Option<String>,
    pub failures: Option<Vec<crate::ImportFailure>>,
}
struct Reporter<F> {
    progress: F,
    id: String,
    name: String,
}
impl<F: Fn(Update) -> Result<(), Error>> Reporter<F> {
    fn emit(&self, phase: Phase, counts: Progress, current_file: Option<&str>, failures: Option<&[crate::ImportFailure]>) -> Result<(), Error> {
        (self.progress)(Update { id: self.id.clone(), name: self.name.clone(), phase, counts, current_file: current_file.map(str::to_owned), failures: failures.map(<[crate::ImportFailure]>::to_vec) })
    }
}
struct ImportIo<S>(S);
impl<S> std::ops::Deref for ImportIo<S> {
    type Target = S;
    fn deref(&self) -> &S {
        &self.0
    }
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub library: LibraryId,
    pub parent: DirId,
    pub name: String,
    pub create_root: bool,
    pub directories: Vec<Vec<String>>,
    pub files: Vec<File>,
}

#[derive(Deserialize, Serialize)]
pub struct File {
    pub path: Vec<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified: f64,
}

impl<S: Storage> ImportIo<S> {
    fn staged_path(&self, manifest: &Manifest, index: usize) -> String {
        self.0.staged_path(manifest, index)
    }

    async fn manifest(&self, job: Manifest) -> Result<Manifest, Error> {
        let file = self.open_file("manifest").await?;
        let bytes = file.read()?;
        if let Ok(manifest) = serde_json::from_slice(&bytes) {
            return Ok(manifest);
        }
        // A torn initial manifest can be replaced while the coordinator still
        // owns the source Files. Recovery without those capabilities fails.
        let mut manifest = job;
        for (index, entry) in manifest.files.iter_mut().enumerate() {
            let (size, modified) = self.source_metadata(index as u32)?;
            entry.size = size;
            entry.modified = modified;
        }
        file.truncate(0)?;
        file.write(&serde_json::to_vec(&manifest).map_err(error)?, 0)?;
        file.flush()?;
        Ok(manifest)
    }

    async fn copy_file<F: Fn(Update) -> Result<(), Error>>(&self, index: u32, file: &File, reporter: &Reporter<F>, counts: Progress) -> Result<String, Error> {
        let (size, modified) = self.source_metadata(index)?;
        if size != file.size || modified != file.modified {
            return Err(error("The source folder must be selected again to finish copying"));
        }
        let target = self.open_file(&index.to_string()).await?;
        target.truncate(0)?;
        let mut hasher = blake3::Hasher::new();
        let mut position = 0;
        let mut last_progress = web_time::Instant::now();
        let current = file.path.join("/");
        while position < file.size {
            let end = (position + 1024 * 1024).min(file.size);
            let bytes = self.read_source(index, position, end).await?;
            if bytes.is_empty() || bytes.len() as u64 > end - position {
                return Err(error("Source file ended unexpectedly"));
            }
            target.write(&bytes, position)?;
            hasher.update(&bytes);
            position += bytes.len() as u64;
            if last_progress.elapsed() >= std::time::Duration::from_millis(100) {
                reporter.emit(Phase::Copying, counts.clone().with_in_flight(position), Some(&current), None)?;
                last_progress = web_time::Instant::now();
            }
        }
        target.flush()?;
        Ok(hasher.finalize().to_hex().to_string())
    }
}

pub async fn run<S: Storage>(job: Manifest, io: S, client: &impl Library, progress: impl Fn(Update) -> Result<(), Error>) -> Result<Vec<crate::ImportFailure>, Error> {
    let io = ImportIo(io);
    let manifest = io.manifest(job).await?;
    let journal = io.open_file("journal").await?;
    let bytes = journal.read()?;
    let mut state = import_journal::replay(&bytes).map_err(error)?.with_files(manifest.files.iter().map(|file| file.size).collect()).map_err(error)?;
    journal.truncate(state.offset as u64)?;
    let reporter = Reporter { progress, id: manifest.id.clone(), name: manifest.name.clone() };
    if let Some(failures) = &state.completed {
        reporter.emit(Phase::Complete, state.progress(0), None, Some(failures))?;
        return Ok(failures.clone());
    }
    reporter.emit(Phase::Copying, state.progress(0), None, None)?;
    for (index, file) in manifest.files.iter().enumerate() {
        if state.copied_hash(index).is_some() {
            continue;
        }
        let hash = io.copy_file(index as u32, file, &reporter, state.progress(0)).await?;
        let checkpoint = state.record_copied(index, hash).map_err(error)?;
        journal.persist(&state, checkpoint)?;
        reporter.emit(Phase::Copying, state.progress(0), None, None)?;
    }
    // Every selected file is now durably copied. Metadata requests begin here.
    reporter.emit(Phase::Adding, state.progress(0), None, None)?;
    let activity = uuid::Uuid::new_v4();
    let prepared: PreparedImport = client.prepare(activity, &manifest).await?;
    state.prepare_directories(prepared.directories);
    let mut failures = prepared.failures;
    let mut batch = Adding { state: &mut state, journal: &journal, client, storage: &io, manifest: &manifest, reporter: &reporter, activity, failures: &mut failures };
    crate::import_workflow::run(&mut batch).await?;
    client.cleanup(manifest.id, manifest.files.len() as u32).await?;
    let checkpoint = state.record_complete(failures.clone()).map_err(error)?;
    journal.persist(&state, checkpoint)?;
    reporter.emit(Phase::Complete, state.progress(0), None, Some(&failures))?;
    Ok(failures)
}

struct Adding<'a, L, F, S, P> {
    state: &'a mut Journal,
    journal: &'a F,
    client: &'a L,
    storage: &'a ImportIo<S>,
    manifest: &'a Manifest,
    reporter: &'a Reporter<P>,
    activity: uuid::Uuid,
    failures: &'a mut Vec<crate::ImportFailure>,
}
impl<L: Library, F: FileAccess, S: Storage, P: Fn(Update) -> Result<(), Error>> crate::import_workflow::Batch for Adding<'_, L, F, S, P> {
    type Error = Error;
    fn len(&self) -> usize {
        self.manifest.files.len()
    }
    async fn process(&mut self, index: usize) -> Result<(), Error> {
        let file = &self.manifest.files[index];
        if !self.state.has_added(index) {
            let result = async {
                let command = self.state.import_command(self.storage.staged_path(self.manifest, index), index, &file.path).map_err(error)?;
                self.client.import(command).await?;
                Ok::<_, Error>(())
            }
            .await;
            let failure = match result {
                Ok(()) => None,
                Err(value) => {
                    if value.transport || value.storage_full {
                        return Err(value);
                    }
                    Some(value.source.to_string())
                }
            };
            let checkpoint = self.state.record_added(index, failure).map_err(error)?;
            self.journal.persist(self.state, checkpoint)?;
        }
        if let Some(error) = self.state.added_error(index).filter(|error| !error.is_empty()) {
            self.failures.push(crate::import_workflow::file_failure(&file.path, error));
        }
        self.reporter.emit(Phase::Adding, self.state.progress(0), Some(&file.path.join("/")), None)?;
        Ok(())
    }
    async fn progress(&mut self) -> Result<(), Error> {
        if let Some(command) = self.state.advance_command(self.activity).map_err(error)? {
            self.client.advance(command).await?;
        }
        Ok(())
    }
    async fn finish(&mut self) -> Result<(), Error> {
        self.client.finish(self.activity).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        rc::Rc,
    };
    #[derive(Clone, Default)]
    struct Memory(Rc<RefCell<BTreeMap<String, Vec<u8>>>>, Rc<Cell<usize>>);
    struct Handle(Memory, String);
    impl FileAccess for Handle {
        fn read(&self) -> Result<Vec<u8>, Error> {
            Ok(self.0 .0.borrow().get(&self.1).cloned().unwrap_or_default())
        }
        fn write(&self, bytes: &[u8], offset: u64) -> Result<(), Error> {
            let mut files = self.0 .0.borrow_mut();
            let file = files.entry(self.1.clone()).or_default();
            file.resize(file.len().max(offset as usize + bytes.len()), 0);
            file[offset as usize..offset as usize + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }
        fn truncate(&self, offset: u64) -> Result<(), Error> {
            self.0 .0.borrow_mut().entry(self.1.clone()).or_default().resize(offset as usize, 0);
            Ok(())
        }
        fn flush(&self) -> Result<(), Error> {
            Ok(())
        }
    }
    impl Storage for Memory {
        type File = Handle;
        fn staged_path(&self, manifest: &Manifest, index: usize) -> String {
            format!("staged/{}/{index}", manifest.id)
        }
        async fn open_file(&self, name: &str) -> Result<Handle, Error> {
            Ok(Handle(self.clone(), name.into()))
        }
        fn source_metadata(&self, _: u32) -> Result<(u64, f64), Error> {
            Ok((4, 1.))
        }
        async fn read_source(&self, index: u32, start: u64, end: u64) -> Result<Vec<u8>, Error> {
            self.1.set(self.1.get() + 1);
            Ok(vec![index as u8; (end - start) as usize])
        }
    }
    struct Backend {
        storage: Memory,
        interrupted: Cell<bool>,
        storage_full: Cell<bool>,
        imported: RefCell<Vec<String>>,
        physical: RefCell<Vec<String>>,
        finished: Cell<usize>,
        cleaned: Cell<bool>,
    }
    impl Library for Backend {
        async fn prepare(&self, _: uuid::Uuid, _: &Manifest) -> Result<PreparedImport, Error> {
            let files = self.storage.0.borrow();
            let journal = import_journal::replay(files.get("journal").unwrap()).unwrap();
            assert!(journal.copied_hash(0).is_some() && journal.copied_hash(1).is_some(), "all source copies must be checkpointed before metadata starts");
            Ok(PreparedImport { directories: vec![(vec![], DirId::from_u128(1))], failures: vec![] })
        }
        async fn import(&self, book: StagedBook) -> Result<(), Error> {
            if book.file_name == "second.epub" && self.interrupted.replace(false) {
                return Err(Error { source: "disconnected".into(), transport: true, storage_full: false });
            }
            if book.file_name == "second.epub" && self.storage_full.replace(false) {
                return Err(Error { source: "browser storage is full".into(), transport: false, storage_full: true });
            }
            self.imported.borrow_mut().push(book.file_name);
            self.physical.borrow_mut().push(book.physical);
            Ok(())
        }
        async fn advance(&self, _: AdvanceProgress) -> Result<(), Error> {
            Ok(())
        }
        async fn finish(&self, _: uuid::Uuid) -> Result<(), Error> {
            self.finished.set(self.finished.get() + 1);
            Ok(())
        }
        async fn cleanup(&self, _: String, _: u32) -> Result<(), Error> {
            self.cleaned.set(true);
            Ok(())
        }
    }
    fn manifest() -> Manifest {
        Manifest {
            id: "import".into(),
            library: sync_common::LibraryId::from_u128(1),
            parent: DirId::from_u128(1),
            name: "Books".into(),
            create_root: false,
            directories: vec![],
            files: ["first.epub", "second.epub"].into_iter().map(|name| File { path: vec![name.into()], size: 0, modified: 0. }).collect(),
        }
    }
    #[tokio::test]
    async fn interruption_preserves_checkpoints_and_resumes_without_recopying_or_readding() {
        let storage = Memory::default();
        let backend = Backend { storage: storage.clone(), interrupted: Cell::new(true), storage_full: Cell::new(false), imported: RefCell::new(vec![]), physical: RefCell::new(vec![]), finished: Cell::new(0), cleaned: Cell::new(false) };
        assert!(run(manifest(), storage.clone(), &backend, |_| Ok(())).await.unwrap_err().transport);
        assert_eq!(&*backend.imported.borrow(), &["first.epub"]);
        assert_eq!(&*backend.physical.borrow(), &["staged/import/0"]);
        assert!(!backend.cleaned.get());
        assert_eq!(backend.finished.get(), 1);
        assert!(run(manifest(), storage.clone(), &backend, |_| Ok(())).await.unwrap().is_empty());
        assert_eq!(storage.1.get(), 2, "checkpointed source bytes must not be read again");
        assert_eq!(&*backend.imported.borrow(), &["first.epub", "second.epub"]);
        assert!(backend.cleaned.get());
        assert_eq!(backend.finished.get(), 2);
        assert!(run(manifest(), storage, &backend, |update| {
            assert!(matches!(update.phase, Phase::Complete));
            assert_eq!(update.counts.processed, 2);
            assert_eq!(update.counts.failed, 0);
            Ok(())
        })
        .await
        .unwrap()
        .is_empty());
        assert_eq!(backend.finished.get(), 2, "completed imports must not restart library work");
    }
    #[tokio::test]
    async fn full_storage_pauses_instead_of_recording_failed_books() {
        let storage = Memory::default();
        let backend = Backend { storage: storage.clone(), interrupted: Cell::new(false), storage_full: Cell::new(true), imported: RefCell::new(vec![]), physical: RefCell::new(vec![]), finished: Cell::new(0), cleaned: Cell::new(false) };
        assert!(run(manifest(), storage.clone(), &backend, |_| Ok(())).await.unwrap_err().storage_full);
        assert_eq!(&*backend.imported.borrow(), &["first.epub"]);
        assert!(!backend.cleaned.get());
        assert!(run(manifest(), storage, &backend, |_| Ok(())).await.unwrap().is_empty());
        assert_eq!(&*backend.imported.borrow(), &["first.epub", "second.epub"]);
    }
}
