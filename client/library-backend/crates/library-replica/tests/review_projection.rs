use library_replica::{DirectoryIntent, DirectoryLifecycleState, portable_name_key, project_directories};
use std::collections::{HashMap, HashSet};
use sync_common::{DirId, ROOT_DIR_ID};

#[test]
fn exhaustive_small_graphs_have_stable_reachable_unique_projections() {
    // All three-node parent graphs, including missing parents, self-loops,
    // multi-node cycles, and every combination of the three lifecycles.
    for edges in 0..125 {
        for states in 0..27 {
            let make = |reverse: bool| {
                let mut result = HashMap::new();
                for index in if reverse { vec![2, 1, 0] } else { vec![0, 1, 2] } {
                    let id = DirId::from_u128(index + 1);
                    let parent_id = DirId::from_u128((edges / 5u128.pow(index as u32)) % 5);
                    let lifecycle = match (states / 3u128.pow(index as u32)) % 3 {
                        0 => DirectoryLifecycleState::Present,
                        1 => DirectoryLifecycleState::Deleted,
                        _ => DirectoryLifecycleState::Purged,
                    };
                    result.insert(id, DirectoryIntent { id, parent_id, name: "Shelf".into(), lifecycle });
                }
                result
            };
            let intents = make(false);
            let projection = project_directories(&intents);
            assert_eq!(projection, project_directories(&make(true)));
            let mut active = HashSet::new();
            let mut paths = HashSet::new();
            for id in &projection.ordered_active {
                assert!(!projection.suppressed.contains(id));
                assert_eq!(intents[id].lifecycle, DirectoryLifecycleState::Present);
                let parent = projection.parents[id];
                assert!(parent == ROOT_DIR_ID || active.contains(&parent));
                assert!(active.insert(*id));
                assert!(paths.insert((parent, portable_name_key(&projection.names[id]))));
            }
            assert_eq!(active.len() + projection.suppressed.len(), 3);
        }
    }
}
