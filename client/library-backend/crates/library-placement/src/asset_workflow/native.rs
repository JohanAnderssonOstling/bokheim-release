//! Database policy for native file replay and interrupted directory moves.

use super::{AssetStore, BackendError, BookId, Database, DirId};
use std::collections::{HashMap, HashSet};
use sync_common::ROOT_DIR_ID;

pub(super) fn reconcile_file_jobs(assets: &AssetStore, database: &Database, jobs: Vec<library_database::NativeFileWork>) -> Result<(), BackendError> {
    let mut movable = HashMap::<BookId, Vec<library_model::RelativeBookPath>>::new();
    for job in jobs.iter().filter(|job| job.operation == "remove") {
        movable.entry(job.content_hash).or_default().push(job.original_path.clone());
    }
    let mut failure = None;
    for work in jobs {
        if let Err(error) = apply_file_work(assets, database, &movable, work) {
            failure.get_or_insert(error);
        }
    }
    failure.map_or(Ok(()), Err)
}

/// One placement job: removes a file, or materializes it under a free name.
/// Does nothing when the job is obsolete or another operation holds the book.
fn apply_file_work(assets: &AssetStore, database: &Database, movable: &HashMap<BookId, Vec<library_model::RelativeBookPath>>, work: library_database::NativeFileWork) -> Result<(), BackendError> {
    let Some(snapshot) = database.native_file_work_snapshot(work)? else { return Ok(()) };
    // Keep a staged source's cleanup pending until its live placement has
    // been restored. A failed restore must never delete the remaining bytes.
    if snapshot.work.operation == "remove" && snapshot.live && snapshot.resolved_path != snapshot.work.original_path {
        return Ok(());
    }
    let hash = snapshot.work.content_hash;
    let Some(_lease) = assets.try_exclusive_lease(&hash)? else { return Ok(()) };
    let mut path = snapshot.resolved_path.clone();
    let mut renamed = None;
    let prepared = match snapshot.work.operation.as_str() {
        "remove" if snapshot.live && snapshot.placement.is_some() => None,
        "remove" => {
            if !snapshot.live_book_paths.is_empty() && !assets.verified_placement_exists(&hash, &snapshot.live_book_paths)? {
                return Err(BackendError::message("destination copy is not ready; the source file has been kept"));
            }
            Some(assets.prepare_removal(&hash, &path)?)
        }
        "materialize" => {
            if let Some((dir, name)) = snapshot.placement.clone() {
                let mut alternatives = library_replica::file_name_candidates(&name, snapshot.occupied_names.iter().map(String::as_str).chain(std::iter::once(name.as_str())));
                while assets.placement_conflicts(&hash, &path)? {
                    let chosen = alternatives.next().ok_or_else(|| BackendError::message("no collision-free file name remains"))?;
                    path = path.with_file_name(&chosen).map_err(BackendError::operation)?;
                    renamed = Some((dir.clone(), chosen));
                }
                let movable = movable.get(&hash).map(Vec::as_slice).unwrap_or_default();
                let Some(prepared) = assets.prepare_placement(&hash, &path, &snapshot.book_paths, movable)? else {
                    return Ok(());
                };
                Some(prepared)
            } else {
                None
            }
        }
        operation => return Err(BackendError::message(format!("unknown file operation: {operation}"))),
    };
    let published = prepared.is_some();
    if let Some(prepared) = prepared {
        prepared.publish()?;
    }
    database.acknowledge_native_file_work(library_database::NativeFileWorkCompletion { snapshot, final_path: path.to_string(), renamed, published, thumbnail_set_exists: assets.thumbnail_set_exists(&hash)? })?;
    Ok(())
}

fn materialize_directory(assets: &AssetStore, database: &Database, id: DirId) -> Result<(), BackendError> {
    if id == ROOT_DIR_ID {
        return Ok(());
    }
    if !assets.root_available() {
        return Err(BackendError::message("library storage is unavailable"));
    }
    loop {
        let Some(snapshot) = database.native_directory_snapshot(&id)? else { return Ok(()) };
        let prepared = assets.inspect_directory(snapshot.id, &snapshot.relative_path, snapshot.projected_path.as_deref(), &snapshot.name, &snapshot.sibling_names)?;
        let Some(result) = prepared.publish()? else { continue };
        let completion = library_database::NativeDirectoryCompletion { snapshot, renamed_to: result.renamed_to, materialized: result.materialized };
        match database.acknowledge_native_directory(completion)? {
            library_database::NativeDirectoryAcknowledgement::Applied | library_database::NativeDirectoryAcknowledgement::Obsolete => return Ok(()),
            library_database::NativeDirectoryAcknowledgement::Retry => continue,
        }
    }
}

pub(super) async fn replay_directories(assets: &AssetStore, database: &Database, requested: Option<DirId>) -> Result<(), BackendError> {
    let work = database.native_directory_replay_work().map_err(BackendError::operation)?;
    let mut directories: Vec<_> = work.iter().map(|(_, directory)| *directory).collect();
    directories.extend(database.native_book_work_directory_ids().map_err(BackendError::operation)?);
    directories.extend(requested);
    directories.sort();
    directories.dedup();
    if let Some(requested) = requested {
        // Restrict interactive work to ancestors and intersecting physical
        // moves. A broken move in another subtree must not block imports.
        let mut paths = HashMap::new();
        for id in &directories {
            let desired = database.native_directory_snapshot(id).map_err(BackendError::operation)?.map(|snapshot| snapshot.relative_path);
            let old = database.native_directory_projected_path(id).map_err(BackendError::operation)?;
            paths.insert(*id, (desired, old));
        }
        let overlaps = |a: &str, b: &str| a == b || a.strip_prefix(b).is_some_and(|suffix| suffix.starts_with('/')) || b.strip_prefix(a).is_some_and(|suffix| suffix.starts_with('/'));
        let mut selected = HashSet::from([requested]);
        loop {
            let before = selected.len();
            for id in selected.clone() {
                for component in database.dir_path_components(&id).map_err(BackendError::operation)? {
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
    let selected: HashSet<_> = directories.iter().copied().collect();
    for directory in directories {
        for component in database.dir_path_components(&directory).map_err(BackendError::operation)? {
            if !ordered.contains(&component.id) {
                ordered.push(component.id);
            }
        }
    }
    // Hidden directories have no live ancestry, but interrupted physical
    // staging must still be recovered before permanent removal is replayed.
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
        let snapshot = database.native_directory_snapshot(directory)?;
        let Some(old) = database.native_directory_projected_path(directory)? else { continue };
        if let Some(staged) = assets.stage_directory(*directory, &old, snapshot.as_ref().map(|snapshot| snapshot.relative_path.as_str()))? {
            database.acknowledge_native_directory_staging(directory, &old, &staged)?;
        }
    }
    for directory in ordered {
        materialize_directory(assets, database, directory)?;
    }
    let ids = work.into_iter().filter(|(_, directory)| selected.contains(directory)).map(|(id, _)| id).collect::<Vec<_>>();
    database.complete_directory_replay(&ids)?;
    Ok(())
}
