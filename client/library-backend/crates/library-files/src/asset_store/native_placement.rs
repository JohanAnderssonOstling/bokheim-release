//! Native directory placement and durable filesystem reconciliation.
use super::{imp, AssetStore, AssetStoreError};
use sync_common::{ContentHash, DirId};

/// Native directory placement is separate from shared asset byte access.
pub struct NativePlacement<'a> {
    pub(super) store: &'a AssetStore,
}

impl NativePlacement<'_> {
    pub fn create_directory(&self, relative: &str, id: DirId) -> Result<String, AssetStoreError> {
        imp::create_directory(&self.store.0, relative, id)
    }

    pub fn root_available(&self) -> bool {
        imp::root_available(&self.store.0)
    }
    pub fn directory_available(&self, relative: &str) -> bool {
        imp::directory_available(&self.store.0, relative)
    }

    /// Prepares a directory together with pending moves that may occupy its path.
    pub async fn prepare_directory_with_database(&self, database: &library_database::Database, id: DirId) -> Result<(), AssetStoreError> {
        self.replay_directories(database, Some(id)).await
    }

    pub fn purge_trash(&self, hash: &ContentHash) -> Result<(), AssetStoreError> {
        imp::purge_trash(&self.store.0, hash)
    }
}

impl NativePlacement<'_> {
    /// Replay metadata-committed file work. Database snapshots and
    /// acknowledgements are short; all filesystem work between them is
    /// connection-free.
    pub async fn reconcile_file_work(&self, database: &library_database::Database) -> Result<(), AssetStoreError> {
        let jobs = database.native_file_work().map_err(|error| AssetStoreError::operation(error))?;
        let movable = Self::movable_sources(&jobs);
        let mut failure = None;
        for work in jobs {
            if let Err(error) = self.reconcile_file_job(database, work, &movable).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Same as [`Self::reconcile_file_work`], scoped to one book so a reader
    /// never blocks on an unrelated large copy.
    pub async fn reconcile_file_work_for(&self, database: &library_database::Database, hash: &ContentHash) -> Result<(), AssetStoreError> {
        let jobs: Vec<_> = database.native_file_work().map_err(|error| AssetStoreError::operation(error))?.into_iter().filter(|work| &work.content_hash == hash).collect();
        let movable = Self::movable_sources(&jobs);
        let mut failure = None;
        for work in jobs {
            if let Err(error) = self.reconcile_file_job(database, work, &movable).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Paths a queued `trash` job already claims: safe for a paired
    /// `restore` to relocate instead of copy.
    fn movable_sources(jobs: &[library_database::NativeFileWork]) -> std::collections::HashMap<ContentHash, Vec<String>> {
        let mut movable = std::collections::HashMap::<ContentHash, Vec<String>>::new();
        for job in jobs.iter().filter(|job| job.operation == "trash") {
            movable.entry(job.content_hash).or_default().push(job.original_path.clone());
        }
        movable
    }

    async fn reconcile_file_job(&self, database: &library_database::Database, work: library_database::NativeFileWork, movable: &std::collections::HashMap<ContentHash, Vec<String>>) -> Result<(), AssetStoreError> {
        let Some(snapshot) = database.native_file_work_snapshot(work).map_err(|error| AssetStoreError::operation(error))? else { return Ok(()) };
        let Some(_lease) = imp::lease(&self.store.0, snapshot.work.content_hash.as_str(), true)? else { return Ok(()) };
        let hash = snapshot.work.content_hash;
        let mut path = snapshot.resolved_path.clone();
        let mut renamed = None;
        let prepared = if snapshot.work.operation == "trash" {
            if snapshot.live && snapshot.placement.is_some() {
                None
            } else {
                Some(imp::prepare_trash(&self.store.0, &hash, &path)?)
            }
        } else if snapshot.work.operation == "restore" {
            if let Some((dir, name)) = snapshot.placement.clone() {
                let mut occupied = snapshot.occupied_names.clone();
                let parent = path.rsplit_once('/').map(|(parent, _)| parent.to_owned()).unwrap_or_default();
                while imp::placement_conflicts(&self.store.0, &hash, &path)? {
                    let chosen = library_replica::unique_file_name(&name, occupied.iter().map(String::as_str));
                    occupied.push(chosen.clone());
                    path = format!("{parent}/{chosen}");
                    renamed = Some((dir.clone(), chosen));
                }
                let empty = Vec::new();
                let movable_sources = movable.get(&hash).unwrap_or(&empty);
                let Some(prepared) = imp::prepare_restore(&self.store.0, &hash, &path, snapshot.expected_checksum.as_deref(), &snapshot.book_paths, movable_sources)? else { return Ok(()) };
                Some(prepared)
            } else {
                None
            }
        } else {
            return Err(AssetStoreError::operation(format!("unknown file operation: {}", snapshot.work.operation)));
        };
        let published = if let Some(prepared) = prepared {
            prepared.publish()?;
            true
        } else {
            false
        };
        let acknowledgement = library_database::NativeFileWorkCompletion { snapshot, final_path: path, renamed, published, thumbnail_set_exists: self.store.thumbnail_set_exists(&hash)? };
        let _ = database.acknowledge_native_file_work(acknowledgement).map_err(|error| AssetStoreError::operation(error))?;
        Ok(())
    }

    /// Prepare recorded folder/book changes before replaying file operations.
    /// Folder moves rewrite remembered source paths, so a preparation failure
    /// must prevent source retirement until those dependencies are resolved.
    pub async fn run_jobs(&self, database: &library_database::Database) -> Result<(), AssetStoreError> {
        let books = database.native_book_replay_work_ids().map_err(AssetStoreError::operation)?;
        self.run_directory_work(database).await?;
        self.reconcile_file_work(database).await?;
        database.complete_native_book_replay(&books).map_err(AssetStoreError::operation)
    }

    /// Replays every queued directory move/rename; cheap since each is a
    /// same-volume `fs::rename`, unlike file work.
    pub async fn run_directory_work(&self, database: &library_database::Database) -> Result<(), AssetStoreError> {
        self.replay_directories(database, None).await
    }

    // Both interactive preparation and background replay must recover staged
    // sources and vacate managed destinations before creating any folders.
    async fn replay_directories(&self, database: &library_database::Database, requested: Option<DirId>) -> Result<(), AssetStoreError> {
        let work = database.native_directory_replay_work().map_err(AssetStoreError::operation)?;
        let mut directories: Vec<_> = work.iter().map(|(_, directory)| *directory).collect();
        directories.extend(database.native_book_work_directory_ids().map_err(|error| AssetStoreError::operation(error))?);
        directories.extend(requested);
        directories.sort();
        directories.dedup();
        if let Some(requested) = requested {
            // Restrict interactive work to ancestors and intersecting physical
            // moves. A broken move in another subtree must not block imports.
            let mut paths = std::collections::HashMap::new();
            for id in &directories {
                let desired = database.native_directory_snapshot(id).map_err(AssetStoreError::operation)?.map(|snapshot| snapshot.relative_path);
                let old = database.native_directory_projected_path(id).map_err(AssetStoreError::operation)?;
                paths.insert(*id, (desired, old));
            }
            let overlaps = |a: &str, b: &str| a == b || a.strip_prefix(b).is_some_and(|suffix| suffix.starts_with('/')) || b.strip_prefix(a).is_some_and(|suffix| suffix.starts_with('/'));
            let mut selected = std::collections::HashSet::from([requested]);
            loop {
                let before = selected.len();
                for id in selected.clone() {
                    for component in database.dir_path_components(&id).map_err(AssetStoreError::operation)? {
                        selected.insert(component.id);
                    }
                    let Some((desired, old)) = paths.get(&id) else { continue };
                    for (candidate, (candidate_desired, candidate_old)) in &paths {
                        if [desired, old].into_iter().flatten().any(|path| [candidate_desired, candidate_old].into_iter().flatten().any(|other| overlaps(path, other))) {
                            selected.insert(*candidate);
                        }
                    }
                }
                if selected.len() == before {
                    break;
                }
            }
            directories.retain(|id| selected.contains(id));
            directories.extend(selected);
            directories.sort();
            directories.dedup();
        }
        // Materialize effective parents first. In particular, lift a repaired
        // cycle root out of its old ancestor before moving that ancestor below it.
        let mut ordered = Vec::new();
        let selected: std::collections::HashSet<_> = directories.iter().copied().collect();
        for directory in directories {
            for component in database.dir_path_components(&directory).map_err(AssetStoreError::operation)? {
                if !ordered.contains(&component.id) {
                    ordered.push(component.id);
                }
            }
        }
        // Hidden directories have no live ancestry, but interrupted physical
        // staging must still be recovered before their Trash work is replayed.
        let mut recovery = ordered.clone();
        for (_, id) in &work {
            if selected.contains(id) && !recovery.contains(id) {
                recovery.push(*id);
            }
        }
        // Stage changing managed folders first, including name swaps. Otherwise
        // a destination still occupied by its previous owner looks like a foreign
        // collision and causes a new, device-dependent synchronized rename.
        for directory in &recovery {
            imp::stage_directory_with_database(&self.store.0, database, *directory)?;
        }
        for directory in ordered {
            imp::prepare_directory_with_database(&self.store.0, database, directory).await?;
        }
        database.complete_native_directory_replay(&work.into_iter().filter(|(_, directory)| selected.contains(directory)).map(|(id, _)| id).collect::<Vec<_>>()).map_err(AssetStoreError::operation)
    }


}
