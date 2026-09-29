//! Seeded schedules exercise the production database merge, publication and projection paths.
//! No network/service simulator: delivery order here deliberately exceeds server guarantees.
use super::convergence_tests::{change, db, facts, folder_events, metadata, present, pull};
use crate::Database;
use library_replica::{BookLifecycleState, DirectoryLifecycleState, MutationBody};
use rusqlite::types::Value;
use std::collections::BTreeMap;
use sync_common::{LibraryRevision, ServerMutation};

// Fixed algorithm so a seed remains reproducible across dependency upgrades.
struct Random(u64);
impl Random {
    fn pick(&mut self, upper: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 32) % upper as u64) as usize
    }
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.pick(i + 1));
        }
    }
}

fn seeds() -> Vec<u64> {
    if let Ok(seed) = std::env::var("BOKHEIM_CONVERGENCE_SEED") {
        return vec![seed.parse().expect("BOKHEIM_CONVERGENCE_SEED must be a u64")];
    }
    let cases = std::env::var("BOKHEIM_CONVERGENCE_CASES").map(|value| value.parse::<u64>().expect("BOKHEIM_CONVERGENCE_CASES must be a u64")).unwrap_or(32);
    assert!(cases > 0, "convergence suite must run at least one seed");
    (0..cases).collect()
}

fn history(seed: u64) -> Vec<ServerMutation> {
    let mut random = Random(seed);
    // Guaranteed cycle, name collision and missing parent, followed by random edits.
    let mut events = folder_events();
    events.extend([facts(10, 50), present(10, 51), metadata("en", 10, 52)]);
    for index in 0..64 {
        let dir_id = uuid::Uuid::from_u128(1 + random.pick(4) as u128);
        let body = match random.pick(8) {
            0 | 1 => MutationBody::DirectoryParent { dir_id, parent_id: uuid::Uuid::from_u128(random.pick(6) as u128) },
            2 => MutationBody::DirectoryName { dir_id, name: ["Shelf", "shelf", "Ｓｈｅｌｆ", "Shelf 2", "Étage", "E\u{301}tage", "..", "Folder", "A/B", "A\\B", "CON.txt", "Shelf. "][random.pick(12)].into() },
            3 => MutationBody::DirectoryLifecycle { dir_id, value: [DirectoryLifecycleState::Present, DirectoryLifecycleState::Deleted, DirectoryLifecycleState::Purged][random.pick(3)].clone() },
            4 => {
                MutationBody::BookLifecycle { content_hash: super::convergence_tests::hash(), // Field registers commute within an existing book's lifetime.
                // Purge/readd requires server admission order; covered by the
                // lifetime boundary and server exchange tests, not shuffling
                // discarded fields back into an invented current snapshot.
                value: [BookLifecycleState::Present, BookLifecycleState::Deleted { origin_folder_id: None }][random.pick(2)].clone() }
            }
            5 => MutationBody::Placement { dir_id, content_hash: super::convergence_tests::hash(), present: random.pick(2) == 0, origin_folder_id: None },
            6 => MutationBody::from_wire(&metadata(["en", "fr", "sv"][random.pick(3)], 1, 1).mutation).unwrap().unwrap(),
            _ => MutationBody::ReadingPosition {
                content_hash: super::convergence_tests::hash(),
                value: library_replica::ReadingPositionState { location: book_model::ReadingPosition::parse(&format!("epubcfi(/6/{})", 2 + 2 * random.pick(10))).unwrap(), progress: random.pick(101) as f32 / 100. },
            },
        };
        // Small clock range deliberately generates timestamp/rank/replica ties.
        let mut event = change(body, 20 + random.pick(5) as u64, 100 + index);
        event.replica_id = uuid::Uuid::from_u128(10 + random.pick(3) as u128);
        events.push(event);
    }
    events
}

fn rows(db: &Database, sql: &str) -> Vec<Vec<Value>> {
    let mut statement = db.connection.prepare(sql).unwrap();
    let count = statement.column_count();
    statement.query_map([], |row| (0..count).map(|column| row.get(column)).collect()).unwrap().collect::<Result<_, _>>().unwrap()
}

fn canonical(db: &Database) -> Vec<Vec<Value>> {
    rows(db, "SELECT state_kind,state_key,state_subkey,changed_at,conflict_rank,replica_id,replica_seq,mutation_id,body,book_key FROM sync_state_version ORDER BY state_kind,state_key,state_subkey")
}

fn projection(db: &Database) -> Vec<Vec<Vec<Value>>> {
    // Compare synchronized semantics; row IDs, physical paths, queues and scan
    // timestamps belong to each device. Tombstone times are publication clocks.
    [
        "SELECT id,parent_id,name,deleted_at IS NOT NULL,purged_at IS NOT NULL FROM dir WHERE id='00000000-0000-0000-0000-000000000000' OR EXISTS(SELECT 1 FROM sync_state_version v WHERE v.state_key=dir.id AND v.state_kind LIKE 'directory_%') ORDER BY id",
        "SELECT content_hash,format,added_at,title,subtitle,read_pos,read_progress,deleted_at IS NOT NULL,trash_origin_dir_id,book_metadata FROM book ORDER BY content_hash",
        "SELECT b.content_hash,l.language_tag,l.position FROM book_language l JOIN book b ON b.row_id=l.book_row_id ORDER BY b.content_hash,l.position",
        "SELECT b.content_hash,p.dir_id,p.deleted_at IS NOT NULL,p.trash_origin_dir_id FROM book_dir p JOIN book b ON b.row_id=p.book_row_id ORDER BY b.content_hash,p.dir_id",
    ].map(|sql| rows(db, sql)).to_vec()
}

fn winners(events: &[ServerMutation]) -> Vec<ServerMutation> {
    let mut winners = BTreeMap::new();
    for event in events {
        let m = &event.mutation;
        let key = (m.kind.clone(), m.entity_key.clone(), m.entity_subkey.clone());
        // Independent max-register oracle: no database merge/coalescing helper.
        let rank = (m.changed_at, m.conflict_rank, event.replica_id, m.replica_seq.get(), m.mutation_id);
        let entry = winners.entry(key).or_insert((rank, event.clone()));
        if rank > entry.0 {
            *entry = (rank, event.clone());
        }
    }
    winners.into_values().map(|(_, event)| event).collect()
}

fn deliver(db: &Database, events: &[ServerMutation], random: &mut Random) {
    let mut scheduled = events.to_vec();
    for _ in 0..events.len() / 3 {
        scheduled.push(events[random.pick(events.len())].clone());
    }
    random.shuffle(&mut scheduled);
    let mut offset = 0;
    while offset < scheduled.len() {
        let end = (offset + 1 + random.pick(9)).min(scheduled.len());
        pull(db, &scheduled[offset..end]);
        assert_visible_tree(db);
        offset = end;
    }
}

// Independent structural invariant, checked after every page, not only convergence.
fn assert_visible_tree(db: &Database) {
    let parents =
        db.connection.prepare("SELECT id,parent_id FROM dir WHERE deleted_at IS NULL").unwrap().query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap().collect::<Result<BTreeMap<_, _>, _>>().unwrap();
    let root = sync_common::ROOT_DIR_ID.to_string();
    for id in parents.keys() {
        let mut current = id;
        let mut visited = std::collections::BTreeSet::new();
        while current != &root {
            assert!(visited.insert(current), "visible directory cycle at {id}");
            current = parents.get(current).unwrap_or_else(|| panic!("visible directory {id} has missing/suppressed ancestor {current}"));
        }
    }
}

#[test]
fn seeded_delivery_pages_duplicates_restarts_and_recovery_converge() {
    for seed in seeds() {
        let result = std::panic::catch_unwind(|| {
            let events = history(seed);
            let expected = db();
            pull(&expected, &winners(&events));
            let directory = std::env::temp_dir().join(format!("bokheim-convergence-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&directory).unwrap();
            let mut devices = Vec::new();
            for device in 0..3 {
                let path = directory.join(format!("{device}.sqlite3"));
                let replica = Database::open(&path).unwrap();
                replica.initialize_library().unwrap();
                replica.connection.execute("UPDATE sync_metadata SET replica_id=?1", [uuid::Uuid::from_u128(1000 + device).to_string()]).unwrap();
                let mut random = Random(seed.wrapping_add(device as u64 * 12345));
                let mut scheduled = events.clone();
                random.shuffle(&mut scheduled);
                let split = 1 + random.pick(scheduled.len() - 1);
                deliver(&replica, &scheduled[..split], &mut random);
                drop(replica);
                let replica = Database::open(&path).unwrap();
                replica.initialize_library().unwrap();
                deliver(&replica, &scheduled[split..], &mut random);
                assert_eq!(canonical(&replica), canonical(&expected), "device={device}, canonical winners");
                assert_eq!(projection(&replica), projection(&expected), "device={device}, projection");
                assert!(replica.sync_publishable_mutations().unwrap().is_empty(), "remote projection generated an echo");
                devices.push(replica);
            }
            // Recover from a fully synchronized device: republication may change
            // versions, but must preserve payloads and the repaired tree.
            let source = &devices[seed as usize % devices.len()];
            source.sync_enqueue_missing_state_cells(&source.sync_inventory_page(None).unwrap()).unwrap();
            let sent = source.sync_publishable_mutations().unwrap();
            assert!(!sent.is_empty());
            assert_eq!(sent, source.sync_publishable_mutations().unwrap(), "retry mutated its snapshot");
            let replica_id: String = source.connection.query_row("SELECT replica_id FROM sync_metadata", [], |r| r.get(0)).unwrap();
            let recovered = sent
                .iter()
                .enumerate()
                .map(|(index, m)| ServerMutation { mutation: m.to_wire().unwrap(), replica_id: uuid::Uuid::parse_str(&replica_id).unwrap(), revision: LibraryRevision::new(index as u64 + 1).unwrap() })
                .collect::<Vec<_>>();
            source.sync_acknowledge_mutations(&sent.iter().map(|m| m.mutation_id).collect::<Vec<_>>()).unwrap();
            let fresh = db();
            pull(&fresh, &recovered);
            assert_eq!(projection(&fresh), projection(&expected), "recovery changed semantics");
            for (device, replica) in devices.iter().enumerate() {
                let mut random = Random(seed.wrapping_add(device as u64));
                deliver(replica, &recovered, &mut random);
                deliver(replica, &events, &mut random); // stale history after recovery
                assert_eq!(canonical(replica), canonical(&fresh), "recovered device={device}");
                assert_eq!(projection(replica), projection(&fresh), "recovered projection device={device}");
                assert!(replica.sync_publishable_mutations().unwrap().is_empty());
            }
            drop(devices);
            std::fs::remove_dir_all(directory).unwrap();
        });
        if let Err(error) = result {
            eprintln!("Reproduce with BOKHEIM_CONVERGENCE_SEED={seed} cargo test --release -p library-database seeded_delivery -- --nocapture");
            std::panic::resume_unwind(error);
        }
    }
}

#[test]
fn seeded_concurrent_local_folder_moves_repair_the_same_cycle() {
    for seed in seeds() {
        let left = db();
        let right = db();
        for (id, device) in [(100, &left), (101, &right)] {
            device.connection.execute("UPDATE sync_metadata SET replica_id=?1", [uuid::Uuid::from_u128(id).to_string()]).unwrap();
        }
        let first = uuid::Uuid::from_u128(1);
        let second = uuid::Uuid::from_u128(2);
        let mut initial = Vec::new();
        for id in [first, second] {
            for body in [
                MutationBody::DirectoryName { dir_id: id, name: "Shelf".into() },
                MutationBody::DirectoryParent { dir_id: id, parent_id: sync_common::ROOT_DIR_ID },
                MutationBody::DirectoryLifecycle { dir_id: id, value: DirectoryLifecycleState::Present },
            ] {
                initial.push(change(body, 1, initial.len() as u64 + 1));
            }
        }
        pull(&left, &initial);
        pull(&right, &initial);
        // Both commands are valid on their isolated device; their union is cyclic.
        left.move_directory(&first, Some(&second), None).unwrap();
        right.move_directory(&second, Some(&first), None).unwrap();
        let mut edits = Vec::new();
        for (id, device) in [(100, &left), (101, &right)] {
            let sent = device.sync_publishable_mutations().unwrap();
            edits.extend(sent.iter().map(|m| ServerMutation { mutation: m.to_wire().unwrap(), replica_id: uuid::Uuid::from_u128(id), revision: LibraryRevision::new(1).unwrap() }));
            device.sync_acknowledge_mutations(&sent.iter().map(|m| m.mutation_id).collect::<Vec<_>>()).unwrap();
        }
        let fresh = db();
        let all = initial.into_iter().chain(edits.clone()).collect::<Vec<_>>();
        pull(&fresh, &winners(&all));
        let parent = |id: uuid::Uuid| fresh.connection.query_row("SELECT parent_id FROM dir WHERE id=?1", [id.to_string()], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(parent(second), sync_common::ROOT_DIR_ID.to_string(), "greatest UUID must break the cycle, seed={seed}");
        assert_eq!(parent(first), second.to_string(), "other cycle edge must survive, seed={seed}");
        for (index, device) in [&left, &right].into_iter().enumerate() {
            deliver(device, &edits, &mut Random(seed.wrapping_add(index as u64)));
            assert_eq!(canonical(device), canonical(&fresh), "seed={seed}, device={index}");
            assert_eq!(projection(device), projection(&fresh), "seed={seed}, device={index}");
        }
    }
}
