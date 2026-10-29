//! A library-owned torrent worker. Payloads enter the normal filesystem scan;
//! SQLite retains metadata and book associations independently of their paths.
use super::LibrarySession;
use crate::BackendError;
use client_platform_runtime::transfers::{self, TransferPolicy};
use client_platform_runtime::connectivity::Connectivity;
use library_database::{torrents::TorrentRecord, Database};
use library_model::{TorrentInput, TorrentStatus};
use librqbit::api::TorrentIdOrHash;
use librqbit::dht::DhtPersistenceConfig;
use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, Session, SessionOptions};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;
use tokio::sync::{watch, Mutex, MutexGuard, Notify};

#[derive(Default)]
pub(crate) struct TorrentService {
    engine: Mutex<Option<Arc<Session>>>,
    requests: Arc<StdMutex<HashMap<String, (watch::Sender<bool>, Option<String>)>>>,
    wake: Notify,
    active_adds: AtomicUsize,
    connectivity: StdMutex<Option<watch::Receiver<bool>>>,
}

struct AddingTorrent<'a>(&'a TorrentService);

impl Drop for AddingTorrent<'_> {
    fn drop(&mut self) {
        self.0.active_adds.fetch_sub(1, Ordering::AcqRel);
        self.0.wake();
    }
}

pub(crate) struct TorrentRequest {
    id: String,
    cancelled: watch::Receiver<bool>,
    requests: Arc<StdMutex<HashMap<String, (watch::Sender<bool>, Option<String>)>>>,
}

impl Drop for TorrentRequest {
    fn drop(&mut self) {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.id);
    }
}

impl Drop for TorrentService {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.get_mut() {
            engine.cancellation_token().cancel();
        }
    }
}

fn failure(error: impl std::fmt::Display) -> BackendError {
    BackendError::message(error.to_string())
}

fn torrent_id(hash: &str) -> Result<TorrentIdOrHash, BackendError> {
    Ok(TorrentIdOrHash::Hash(hash.parse().map_err(failure)?))
}

fn folder_name(name: &str) -> String {
    let name: String = name.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).take(120).collect();
    let name = name.trim_matches(|c| c == '.' || c == ' ');
    if name.is_empty() {
        "Torrent download".into()
    } else {
        name.into()
    }
}

impl TorrentService {
    fn network(&self) -> Result<Connectivity, BackendError> {
        let mut source = self.connectivity.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(all(feature = "kobo", target_os = "linux"))]
        if source.is_none() {
            *source = Some(client_platform_kobo::device::wifi::subscribe_connectivity().map_err(failure)?);
        }
        Ok(source.as_ref().map_or_else(Connectivity::unrestricted, |changes| Connectivity::new(changes.clone())))
    }

    #[cfg(test)]
    pub(in crate::library) fn set_connectivity(&self, changes: watch::Receiver<bool>) {
        *self.connectivity.lock().unwrap_or_else(PoisonError::into_inner) = Some(changes);
    }

    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    async fn engine(slot: &mut Option<Arc<Session>>, root: &Path, db: &Database) -> Result<Arc<Session>, BackendError> {
        if let Some(engine) = slot {
            return Ok(engine.clone());
        }
        let engine = Session::new_with_opts(
            root.join(".bokheim-torrents"),
            SessionOptions {
                listen: Some(Default::default()),
                dht: Some(DhtSessionConfig { persistence: Some(DhtPersistenceConfig { config_filename: Some(db.path().with_file_name("torrent-dht.json")), ..Default::default() }), ..Default::default() }),
                ..Default::default()
            },
        )
        .await
        .map_err(failure)?;
        *slot = Some(engine.clone());
        Ok(engine)
    }

    pub(crate) async fn add(&self, db: &Database, root: &Path, input: TorrentInput) -> Result<String, BackendError> {
        self.add_allowed(db, root, input, None).await
    }

    pub(crate) fn begin_request(&self, request: &str) -> TorrentRequest {
        let receiver = self.requests.lock().unwrap_or_else(PoisonError::into_inner).entry(request.to_owned()).or_insert_with(|| (watch::channel(false).0, None)).0.subscribe();
        TorrentRequest { id: request.to_owned(), cancelled: receiver, requests: self.requests.clone() }
    }

    pub(crate) fn prepare_request(&self, request: String) {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).entry(request).or_insert_with(|| (watch::channel(false).0, None));
    }

    pub(crate) fn cancel_request(&self, request: &str) -> Option<String> {
        let (sender, hash) = self.requests.lock().unwrap_or_else(PoisonError::into_inner).get(request)?.clone();
        sender.send_replace(true);
        hash
    }

    async fn add_request(&self, db: &Database, root: &Path, input: TorrentInput, mut request: TorrentRequest) -> Result<String, BackendError> {
        let id = request.id.clone();
        tokio::select! {
            biased;
            _ = request.cancelled.wait_for(|cancelled| *cancelled) => Err(failure("Download cancelled")),
            result = self.add_allowed(db, root, input, Some(&id)) => result,
        }
    }

    async fn add_allowed(&self, db: &Database, root: &Path, input: TorrentInput, request: Option<&str>) -> Result<String, BackendError> {
        if !root.is_dir() {
            return Err(BackendError::classified(client_runtime::Failure::LibraryFolderUnavailable, "this library's folder is unavailable"));
        }
        let (torrent, source) = match input {
            TorrentInput::Magnet(magnet) => {
                if !magnet.starts_with("magnet:") {
                    return Err(failure("expected a magnet link"));
                }
                (AddTorrent::from_url(magnet.clone()), Some(magnet))
            }
            TorrentInput::File(bytes) => {
                if bytes.len() > 16 * 1024 * 1024 {
                    return Err(failure("torrent metadata is too large"));
                }
                (AddTorrent::from_bytes(bytes.into_vec()), None)
            }
        };
        let mut network = self.network()?;
        network.wait_online().await;
        // Keep the session alive while a magnet is resolving, before its
        // durable database record exists. Cancellation also wakes cleanup.
        self.active_adds.fetch_add(1, Ordering::AcqRel);
        let _adding = AddingTorrent(self);
        let engine = {
            let mut slot = self.engine.lock().await;
            Self::engine(&mut slot, root, db).await?
        };
        // Resolve metadata before accepting the job. Re-add the metadata bytes
        // rather than the magnet so its optional file-selection hint cannot
        // override the application's policy of downloading every file.
        let response = tokio::select! {
            _ = network.changed() => return Err(failure("Connection lost while resolving torrent metadata; try again when connected")),
            response = tokio::time::timeout(Duration::from_secs(120), engine.add_torrent(torrent, Some(AddTorrentOptions { list_only: true, ..Default::default() }))) => response.map_err(failure)?.map_err(failure)?,
        };
        let list = match response {
            AddTorrentResponse::ListOnly(list) => list,
            AddTorrentResponse::AlreadyManaged(_, handle) | AddTorrentResponse::Added(_, handle) => return Ok(handle.info_hash().as_string()),
        };
        let hash = list.info_hash.as_string();
        if db.torrents()?.iter().any(|t| t.info_hash == hash) {
            return Ok(hash);
        }
        let name = folder_name(&list.info.name_or_else(|| hash.clone()));
        let stage = format!(".bokheim-torrents/{hash}/{name}");
        let files = list.info.iter_file_details().enumerate().filter(|(_, file)| !file.attrs().padding).map(|(index, file)| (index, file.filename.to_pathbuf().to_string_lossy().replace('\\', "/"))).collect::<Vec<_>>();
        let record = TorrentRecord { info_hash: hash.clone(), metadata: list.torrent_bytes.to_vec(), source, staging_path: stage, output_path: name, imported: false, publishing: false, keep_seeding: true, folder_id: None, error: None };
        db.save_torrent(&record, &files)?;
        if let Some(request) = request {
            if let Some((_, saved_hash)) = self.requests.lock().unwrap_or_else(PoisonError::into_inner).get_mut(request) {
                *saved_hash = Some(hash.clone());
            }
        }
        Self::resume(&engine, db, root, &record, list.seen_peers).await?;
        Ok(hash)
    }

    async fn resume(engine: &Arc<Session>, db: &Database, root: &Path, record: &TorrentRecord, initial_peers: Vec<SocketAddr>) -> Result<(), BackendError> {
        let relative = db.torrent_output_path(record)?.ok_or_else(|| failure("torrent folder is unavailable"))?;
        let output = root.join(relative.trim_start_matches('/'));
        if record.imported {
            // Never recreate an old folder or redownload bytes while a user
            // move, disconnected drive or deletion has made a payload absent.
            let list = engine.add_torrent(AddTorrent::from_bytes(record.metadata.clone()), Some(AddTorrentOptions { list_only: true, ..Default::default() })).await.map_err(failure)?;
            if let AddTorrentResponse::ListOnly(list) = list {
                for file in list.info.iter_file_details().filter(|file| !file.attrs().padding) {
                    let path = output.join(file.filename.to_pathbuf());
                    let metadata = std::fs::symlink_metadata(path).map_err(|_| failure("torrent files are missing; restore their folder before resuming"))?;
                    if !metadata.is_file() || metadata.len() != file.len {
                        db.keep_torrent_seeding(&record.info_hash, false)?;
                        return Err(failure("torrent files changed; seeding stopped"));
                    }
                }
            }
        }
        let handle = engine
            .add_torrent(
                AddTorrent::from_bytes(record.metadata.clone()),
                Some(AddTorrentOptions { output_folder: Some(output.to_string_lossy().into_owned()), overwrite: true, paused: record.imported, initial_peers: Some(initial_peers), ..Default::default() }),
            )
            .await
            .map_err(failure)?
            .into_handle()
            .ok_or_else(|| failure("torrent did not start"))?;
        if record.imported {
            handle.wait_until_initialized().await.map_err(failure)?;
            if !handle.stats().finished {
                engine.delete(torrent_id(&record.info_hash)?, false).await.map_err(failure)?;
                db.keep_torrent_seeding(&record.info_hash, false)?;
                return Err(failure("torrent files changed; seeding stopped"));
            }
            engine.unpause(&handle).await.map_err(failure)?;
        }
        db.set_torrent_error(&record.info_hash, None)?;
        Ok(())
    }

    pub(crate) async fn cancel(&self, db: &Database, root: &Path, hash: &str) -> Result<(), BackendError> {
        let slot = self.engine.lock().await;
        let record = db.torrents()?.into_iter().find(|record| record.info_hash == hash);
        if record.as_ref().is_some_and(|record| record.imported) {
            return Ok(());
        }
        if let Some(engine) = slot.as_ref() {
            if engine.get(torrent_id(hash)?).is_some() {
                engine.delete(torrent_id(hash)?, false).await.map_err(failure)?;
            }
        }
        db.forget_torrent(hash)?;
        self.wake();
        if let Some(record) = record {
            let staged = root.join(record.staging_path);
            match std::fs::remove_dir_all(staged) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(failure(error)),
            }
        }
        Ok(())
    }

    /// Hold this guard over physical placement work. Removing session entries
    /// releases storage handles on Windows too; the next pass reopens them.
    pub(crate) async fn before_moves(&self) -> Result<MutexGuard<'_, Option<Arc<Session>>>, BackendError> {
        let guard = self.engine.lock().await;
        // The worker waits for this guard to be released after physical moves.
        self.wake();
        if let Some(engine) = guard.as_ref() {
            let ids = engine.with_torrents(|torrents| torrents.map(|(id, _)| id).collect::<Vec<_>>());
            for id in ids {
                engine.delete(TorrentIdOrHash::Id(id), false).await.map_err(failure)?;
            }
        }
        Ok(guard)
    }

    pub(crate) fn statuses(&self, db: &Database) -> Result<Vec<TorrentStatus>, BackendError> {
        let slot = self.engine.try_lock().ok();
        db.torrents()?
            .into_iter()
            .map(|record| {
                let stats = slot.as_ref().and_then(|s| s.as_ref()).and_then(|engine| engine.get(torrent_id(&record.info_hash).ok()?)).map(|handle| handle.stats());
                Ok(TorrentStatus {
                    info_hash: record.info_hash,
                    name: Path::new(&record.output_path).file_name().unwrap_or_default().to_string_lossy().into_owned(),
                    progress_bytes: stats.as_ref().map_or(0, |s| s.progress_bytes),
                    total_bytes: stats.as_ref().map_or(0, |s| s.total_bytes),
                    uploaded_bytes: stats.as_ref().map_or(0, |s| s.uploaded_bytes),
                    imported: record.imported,
                    keep_seeding: record.keep_seeding,
                    error: stats.and_then(|s| s.error).or(record.error),
                })
            })
            .collect()
    }

    #[cfg(test)]
    async fn pass(&self, library: &LibrarySession) -> Result<(), BackendError> {
        self.pass_with_policy(library, &transfers::POLICY).await
    }

    async fn pass_with_policy(&self, library: &LibrarySession, policy: &TransferPolicy) -> Result<(), BackendError> {
        let records = library.db.torrents()?;
        // Pending torrents are explicit downloads, retained across restarts.
        // Only completed torrents' automatic seeding follows network cost.
        if !policy.seeding_allowed() && records.iter().all(|record| record.imported) && self.active_adds.load(Ordering::Acquire) == 0 {
            if let Some(engine) = self.engine.lock().await.take() {
                engine.stop().await;
            }
            return Ok(());
        }
        if records.is_empty() && self.engine.try_lock().is_ok_and(|slot| slot.is_none()) {
            return Ok(());
        }
        let mut slot = self.engine.lock().await;
        let records = library.db.torrents()?;
        if records.iter().all(|record| record.imported && !record.keep_seeding) && self.active_adds.load(Ordering::Acquire) == 0 {
            if let Some(engine) = slot.take() {
                engine.stop().await;
            }
            return Ok(());
        }
        if let Some(engine) = slot.as_ref() {
            let forgotten = engine.with_torrents(|torrents| torrents.filter(|(_, t)| !records.iter().any(|r| r.info_hash == t.info_hash().as_string())).map(|(id, _)| id).collect::<Vec<_>>());
            for id in forgotten {
                engine.delete(TorrentIdOrHash::Id(id), false).await.map_err(failure)?;
            }
        }
        if records.is_empty() {
            return Ok(());
        }
        let root = PathBuf::from(library._scanner.root());
        if !root.is_dir() {
            return Ok(());
        }
        let engine = Self::engine(&mut slot, &root, &library.db).await?;
        library.db.bind_torrent_books()?;
        for record in records {
            if record.publishing && !root.join(&record.staging_path).exists() {
                if root.join(&record.output_path).is_dir() {
                    library.db.torrent_published(&record.info_hash, &record.output_path)?;
                } else {
                    library.db.set_torrent_error(&record.info_hash, Some("interrupted torrent move: completed folder is unavailable"))?;
                }
                continue;
            }
            let id = torrent_id(&record.info_hash)?;
            if record.imported && (!record.keep_seeding || !policy.seeding_allowed()) {
                if engine.get(id).is_some() {
                    engine.delete(torrent_id(&record.info_hash)?, false).await.map_err(failure)?;
                }
                continue;
            }
            let Some(handle) = engine.get(id) else {
                if let Err(error) = Self::resume(&engine, &library.db, &root, &record, Vec::new()).await {
                    library.db.set_torrent_error(&record.info_hash, Some(&error.to_string()))?;
                }
                continue;
            };
            let stats = handle.stats();
            if let Some(error) = &stats.error {
                library.db.set_torrent_error(&record.info_hash, Some(error))?;
            }
            if record.imported || !stats.finished {
                continue;
            }
            engine.delete(torrent_id(&record.info_hash)?, false).await.map_err(failure)?;
            drop(handle);
            let staged = root.join(&record.staging_path);
            // Staging is inside the library filesystem: publication is a
            // rename. Journal it on the database owner's thread before the
            // rename, without an await between publication and acknowledgement.
            let placed = library_files::filesystem::place_into_directory_with_intent(&staged, &root, &[], true, |destination| {
                let relative = destination.strip_prefix(&root).map_err(std::io::Error::other)?;
                library.db.torrent_publish_intent(&record.info_hash, &relative.to_string_lossy()).map_err(std::io::Error::other)
            })
            .map_err(failure)?;
            library.db.torrent_published(&record.info_hash, &placed.strip_prefix(&root).map_err(failure)?.to_string_lossy())?;
            self.wake();
            library.events.publish_update(library_model::LibraryUpdate::Contents);
            // The normal watcher/scan owns book inspection and library IDs.
        }
        Ok(())
    }

    pub(crate) async fn run(&self, library: &LibrarySession) {
        self.run_with_policy(library, &transfers::POLICY).await;
    }

    async fn run_with_policy(&self, library: &LibrarySession, policy: &TransferPolicy) {
        let network = match self.network() {
            Ok(network) => network,
            Err(error) => { log::warn!("Could not observe torrent connectivity: {error}"); return; }
        };
        self.run_with_network(library, policy, network).await;
    }

    async fn run_with_network(&self, library: &LibrarySession, policy: &TransferPolicy, mut network: Connectivity) {
        let mut changes = policy.subscribe();
        let mut retry = Duration::from_secs(30);
        let mut previous_progress = 0;
        loop {
            if !network.online() {
                // No peer discovery, trackers or progress timer while offline.
                if self.active_adds.load(Ordering::Acquire) == 0 {
                    if let Some(engine) = self.engine.lock().await.take() { engine.stop().await; }
                }
                tokio::select! {
                    _ = network.changed() => {},
                    _ = self.wake.notified() => {},
                }
                retry = Duration::from_secs(30);
                continue;
            }
            let pass = tokio::select! {
                _ = network.changed() => continue,
                result = self.pass_with_policy(library, policy) => result,
            };
            let progress = self.engine.lock().await.as_ref().map_or(0, |engine| {
                engine.with_torrents(|torrents| torrents.map(|(_, torrent)| torrent.stats().progress_bytes).sum::<u64>())
            });
            if progress > previous_progress { retry = Duration::from_secs(30); }
            previous_progress = previous_progress.max(progress);
            let next = match pass.and_then(|()| self.next_wake(library, policy, retry)) {
                Ok((next, failed)) => {
                    if failed {
                        retry = retry.saturating_mul(2).min(Duration::from_secs(15 * 60));
                        if self.active_adds.load(Ordering::Acquire) == 0 {
                            if let Some(engine) = self.engine.lock().await.take() { engine.stop().await; }
                        }
                    }
                    next
                },
                Err(error) => {
                    log::warn!("torrent transfer will retry: {error}");
                    let next = Some(retry);
                    retry = retry.saturating_mul(2).min(Duration::from_secs(15 * 60));
                    next
                }
            };
            tokio::select! {
                _ = network.changed() => { retry = Duration::from_secs(30); },
                _ = self.wake.notified() => { retry = Duration::from_secs(30); },
                _ = changes.changed() => { retry = Duration::from_secs(30); },
                elapsed = async {
                    match next {
                        Some(delay) => crate::executor::sleep(delay).await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Err(error) = elapsed {
                        log::warn!("torrent transfer retries stopped: {error}");
                        return;
                    }
                },
            }
        }
    }

    #[cfg(test)]
    fn next_check(&self, library: &LibrarySession, policy: &TransferPolicy) -> Result<Option<Duration>, BackendError> {
        self.next_wake(library, policy, Duration::from_secs(30)).map(|(next, _)| next)
    }

    fn next_wake(&self, library: &LibrarySession, policy: &TransferPolicy, retry: Duration) -> Result<(Option<Duration>, bool), BackendError> {
        let records = library.db.torrents()?;
        Ok(if records.iter().any(|record| !record.imported && record.error.is_none()) {
            (Some(Duration::from_secs(2)), false)
        } else if records.iter().any(|record| !record.imported) {
            (Some(retry), true)
        } else if policy.seeding_allowed() && records.iter().any(|record| record.keep_seeding) {
            // Peer traffic belongs to librqbit. Reconcile stable seeds less
            // often; user actions, moves and scans wake us immediately.
            (Some(Duration::from_secs(30)), false)
        } else {
            (None, false)
        })
    }
}

impl LibrarySession {
    pub(crate) async fn add_torrent(&self, input: TorrentInput) -> Result<String, BackendError> {
        self.torrents.add(&self.db, Path::new(self._scanner.root()), input).await
    }

    pub(crate) async fn add_torrent_request(&self, input: TorrentInput, request: TorrentRequest) -> Result<String, BackendError> {
        self.torrents.add_request(&self.db, Path::new(self._scanner.root()), input, request).await
    }

    pub(crate) async fn cancel_torrent(&self, hash: &str) -> Result<(), BackendError> {
        self.torrents.cancel(&self.db, Path::new(self._scanner.root()), hash).await
    }
}

#[cfg(test)]
#[path = "torrent_tests.rs"]
mod tests;
