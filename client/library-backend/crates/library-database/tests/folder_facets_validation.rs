//! Direct folder placement queries, read-time counts, and format facets.
use crate::query_methods::query_rows;
use library_database::{Database, rusqlite::params};
use library_model::LibraryFileTypeFilter;
const ROOT: &str = "00000000-0000-0000-0000-000000000000";
const TARGET: &str = "00000000-0000-0000-0000-000000000001";
const CHILD: &str = "00000000-0000-0000-0000-000000000002";
const BRANCH: &str = "00000000-0000-0000-0000-000000000003";
const OTHER: &str = "00000000-0000-0000-0000-000000000004";
fn fixture(books: usize) -> Database {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();

    let tx = db.connection.unchecked_transaction().unwrap();
    for (id, parent, name) in [(TARGET, ROOT, "Target"), (CHILD, TARGET, "Nested"), (BRANCH, TARGET, "Branch"), (OTHER, ROOT, "Targetish")] {
        tx.fixtures_insert_dir(&id, &parent, &name).unwrap();
    }
    for i in 1..=books as i64 {
        tx.fixtures_insert_book_9(&(format!("{i:064x}")), &(format!("Book {i}")), &([1, 2, 8][i as usize % 3])).unwrap();
        tx.fixtures_insert_book_dir_2(&([TARGET, CHILD, BRANCH, OTHER][i as usize % 4]), &i, &(format!("{i}.epub")), &(i % 2)).unwrap();
        if i % 11 == 0 {
            tx.fixtures_insert_book_dir_3(&OTHER, &i, &(format!("extra{i}.epub"))).unwrap();
        }
        tx.fixtures_insert_book_language_2(&i, &(if i % 3 == 0 { "sv" } else { "en" })).unwrap();
        if i % 7 == 0 {
            tx.fixtures_insert_book_language_3(&i).unwrap();
        }
    }
    tx.commit().unwrap();
    db
}
fn query(location: &str, search: &str) -> library_model::LibraryBrowseQuery {
    library_model::LibraryBrowseQuery {
        location: library_model::BrowseLocation::Folder(location.parse().unwrap()),
        search: search.into(),
        file_types: vec![],
        languages: vec![],
        chip_sort: Default::default(),
        book_sort: Default::default(),
        hide_finished: false,
    }
}
#[test]
fn direct_descendants_and_directory_counts_follow_mutations() {
    let db = fixture(32);
    for mutation in 0..6 {
        match mutation {
            0 => db.connection.fixtures_keep_folder_state(),
            1 => db.connection.fixtures_trash_every_third_placement(),
            2 => db.connection.fixtures_trash_every_seventh_book(),
            3 => db.connection.fixtures_reparent_nested_folder(),
            4 => db.connection.fixtures_trash_target_folder(),
            _ => db.connection.fixtures_restore_folders_and_change_formats(),
        }
        .unwrap();
        let ids = query_rows(|callback| db.connection.fixtures_read_dir_id(callback), |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        for id in ids {
            for folder in db.browse_contents(&query(&id, ""), false).unwrap().children {
                let library_model::BrowseLocation::Folder(folder_id) = folder.id else { panic!("folder browse returned a subject") };
                let expected = db.connection.fixtures_read_subtree_subtree_id_d(&(folder_id.to_string()), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap();
                assert_eq!((folder.book_count as i64, folder.downloaded_book_count as i64), expected, "{mutation}: {}", folder.id);
            }
        }
        // Both cards and counts reflect the committed writes immediately.
        let mut hashes = db.browse_contents(&query(TARGET, "book"), false).unwrap().books.iter().map(|b| b.content_hash().unwrap().to_string()).collect::<Vec<_>>();
        hashes.sort();

        let expected = query_rows(|callback| db.connection.fixtures_read_dir_subtree_id_id(&TARGET, callback), |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(hashes, expected, "{mutation}");
        let formats = db
            .browse(&query(ROOT, ""), false)
            .unwrap()
            .format_counts
            .iter()
            .map(|count| {
                let category = match count.format {
                    LibraryFileTypeFilter::Pdf => 2,
                    LibraryFileTypeFilter::Audiobook => 4,
                    _ => 1,
                };
                (category, i64::from(count.book_count))
            })
            .collect::<Vec<_>>();
        let expected = query_rows(|callback| db.connection.fixtures_read_book_b_format_category(callback), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(formats, expected, "{mutation}");
    }
}

#[cfg(test)]
use crate::sql_fixtures::FixturesSql;
