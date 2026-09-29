use library_database::{Database, ImportCommit, NativeDirectoryAcknowledgement, NativeDirectoryCompletion, PublishedImport};
use sync_common::{ContentHash, DirId, ROOT_DIR_ID, StateCell};

fn database() -> Database {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();
    db
}

fn import() -> ImportCommit {
    let hash = ContentHash::new(&"9".repeat(64));
    ImportCommit {
        parent_id: ROOT_DIR_ID,
        file_name: "book.epub".into(),
        format: book_model::BookFormat::Epub,
        content_hash: hash,
        checksum: hash,
        size_bytes: 10,
        inspection_version: 1,
        inspection: Some(book_metadata::InspectedBook {
            pdf: None,
            audiobook: None,
            metadata: book_model::BookRecord {
                title: "Original".into(),
                subtitle: None,
                contributors: vec![book_model::Contributor::new("Original Author", book_model::MarcRelatorCode(*b"aut")).unwrap()],
                description: String::new(),
                book: Default::default(),
            },
            toc: vec![book_model::BookTocEntry { title: "Chapter".into(), target: "ch1".into(), children: vec![] }],
        }),
        published: PublishedImport { name: "book.epub".into(), relative_path: "/book.epub".into(), published: false, fingerprint: None },
        request_thumbnail: false,
        restore_paths: vec![],
    }
}

#[test]
#[cfg(feature = "scanner")]
fn scan_operation_finishes_only_with_a_readable_final_batch() {
    use library_database::{FilesystemScan, InspectedFilesystemScan, OperationActivity, ScanApplyResult};
    use library_replica::LibraryOperationState;
    let db = database();
    let discovery = FilesystemScan { readable: true, ..Default::default() };
    let inspected = InspectedFilesystemScan { failures: vec![], readable: true, books: vec![] };

    // Start without polling Settings: scan lifecycle must not depend on UI reads.
    let mut snapshot = db.scan_snapshot().unwrap();
    assert!(db.needs_scan().unwrap());
    assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &inspected, false).unwrap(), ScanApplyResult::Applied);
    assert!(db.needs_scan().unwrap());
    assert_ne!(db.operation_status(OperationActivity::default()).unwrap().operation.unwrap().state, LibraryOperationState::Completed);

    // A newer scan invalidates the old result; it cannot finish the new scan.
    let current = db.scan_snapshot().unwrap();
    assert_eq!(db.apply_filesystem_scan(&snapshot, &discovery, &inspected).unwrap(), ScanApplyResult::Stale);
    assert!(db.needs_scan().unwrap());
    assert_eq!(db.apply_filesystem_scan(&current, &discovery, &inspected).unwrap(), ScanApplyResult::Applied);
    assert!(!db.needs_scan().unwrap());
    let files_pending = db.operation_status(OperationActivity { local_busy: true, ..Default::default() }).unwrap().operation.unwrap();
    assert_ne!(files_pending.state, LibraryOperationState::Completed);
    let completed = db.operation_status(OperationActivity::default()).unwrap().operation.unwrap();
    assert_eq!(completed.state, LibraryOperationState::Completed);
    assert!(completed.waiting_reason.is_none());
    assert!(db.operation_status(OperationActivity::default()).unwrap().operation.is_none());

    // An unreadable directory walk still needs another scan on reopen.
    let snapshot = db.scan_snapshot().unwrap();
    let unreadable = InspectedFilesystemScan { failures: vec![], readable: false, books: vec![] };
    db.apply_filesystem_scan(&snapshot, &FilesystemScan { readable: false, ..Default::default() }, &unreadable).unwrap();
    let failed = db.operation_status(OperationActivity::default()).unwrap().operation.unwrap();
    assert!(failed.needs_attention);
    assert!(failed.waiting_reason.unwrap().contains("could not be scanned"));
    assert!(db.needs_scan().unwrap());
    let snapshot = db.scan_snapshot().unwrap();
    db.apply_filesystem_scan(&snapshot, &discovery, &inspected).unwrap();
    assert_eq!(db.operation_status(OperationActivity::default()).unwrap().operation.unwrap().state, LibraryOperationState::Completed);
}

#[test]
fn worker_reconciliation_persists_an_idle_import_completion() {
    use library_database::OperationActivity;
    let db = database();
    db.connection.execute("UPDATE sync_metadata SET operation_id='import',operation_kind='Import',operation_scan_complete=1,operation_completed=0 WHERE singleton=1", []).unwrap();

    // This is the call made by a worker after its final batch, without any
    // Settings transfer-status request.
    db.reconcile_operation(OperationActivity::default()).unwrap();

    assert!(db.connection.query_row("SELECT operation_completed FROM sync_metadata WHERE singleton=1", [], |row| row.get::<_, bool>(0)).unwrap());
}

#[test]
#[cfg(feature = "scanner")]
fn scan_operation_recovers_after_reopening_between_batches() {
    use library_database::{FilesystemScan, InspectedFilesystemScan, OperationActivity};
    use library_replica::LibraryOperationState;
    let root = std::env::temp_dir().join(format!("scan-operation-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("library.sqlite");
    let discovery = FilesystemScan { readable: true, ..Default::default() };
    let inspected = InspectedFilesystemScan { failures: vec![], readable: true, books: vec![] };
    {
        let db = Database::open(&path).unwrap();
        db.initialize_library().unwrap();
        let mut snapshot = db.scan_snapshot().unwrap();
        db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &inspected, false).unwrap();
    }
    {
        let db = Database::open(&path).unwrap();
        assert!(db.needs_scan().unwrap());
        let snapshot = db.scan_snapshot().unwrap();
        db.apply_filesystem_scan(&snapshot, &discovery, &inspected).unwrap();
        // Close before Settings reads completion.
    }
    {
        let db = Database::open(&path).unwrap();
        assert!(!db.needs_scan().unwrap());
        assert_eq!(db.operation_status(OperationActivity::default()).unwrap().operation.unwrap().state, LibraryOperationState::Completed);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_operation_does_not_present_future_sync_work_as_pending_uploads() {
    use library_database::OperationActivity;
    use library_replica::LibraryOperationState;
    let db = database();
    let book = import();
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    db.connection.execute("INSERT OR IGNORE INTO local_book_upload(content_hash,checksum,size_bytes) VALUES(?1,?1,10)", [hash.as_str()]).unwrap();
    let pending_changes: i64 = db.connection.query_row("SELECT count(*) FROM sync_outbox", [], |row| row.get(0)).unwrap();
    assert!(pending_changes > 0);

    let local = db.operation_status(OperationActivity { scanning: true, ..Default::default() }).unwrap().operation.unwrap();
    assert!(!local.requires_sync);
    assert_eq!(local.books, 1);
    assert_eq!((local.pending_uploads, local.pending_changes), (0, 0));
    assert!(!local.needs_attention);
    assert!(local.waiting_reason.is_none());

    // Finish local work while leaving its future sync records intact.
    db.connection.execute("UPDATE sync_metadata SET operation_scan_complete=1 WHERE singleton=1", []).unwrap();
    let completed = db.operation_status(OperationActivity::default()).unwrap().operation.unwrap();
    assert_eq!(completed.state, LibraryOperationState::Completed);
    assert!(!completed.requires_sync);
    assert_eq!(db.connection.query_row("SELECT count(*) FROM local_book_upload", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(db.connection.query_row("SELECT count(*) FROM sync_outbox", [], |row| row.get::<_, i64>(0)).unwrap(), pending_changes);

    let signed_in = db.operation_status(OperationActivity { signed_in: true, ..Default::default() }).unwrap().operation.unwrap();
    assert!(signed_in.requires_sync);
    assert_eq!((signed_in.pending_uploads, signed_in.pending_changes), (1, u64::try_from(pending_changes).unwrap()));

    // A real sync interrupted by sign-out should still ask the user to resume.
    let signed_out = db.operation_status(OperationActivity::default()).unwrap().operation.unwrap();
    assert!(signed_out.requires_sync);
    assert_eq!(signed_out.pending_uploads, 1);
    assert_eq!(signed_out.waiting_reason.as_deref(), Some("Sign in to finish syncing"));
}

fn assert_title_edit_syncs(db: &Database) {
    assert!(!db.connection.query_row("SELECT metadata_batch_flag(NULL)", [], |r| r.get::<_, bool>(0)).unwrap());
    db.clear_sync_outbox().unwrap();
    db.connection.execute("UPDATE book SET title='Edited'", []).unwrap();
    let mutations = db.sync_publishable_mutations().unwrap();
    assert_eq!(mutations.len(), 1);
    assert!(matches!(&mutations[0].body, library_replica::MutationBody::Metadata { value, .. } if value.title == "Edited"));
}

#[test]
fn successful_import_publishes_contributors_and_restores_metadata_sync() {
    let db = database();
    db.commit_import(import()).unwrap();
    let mutations = db.sync_publishable_mutations().unwrap();
    assert!(mutations.iter().any(|mutation| matches!(mutation.body, library_replica::MutationBody::BookLifecycle { value: library_replica::BookLifecycleState::Present, .. })));
    let metadata = mutations
        .iter()
        .find_map(|mutation| match &mutation.body {
            library_replica::MutationBody::Metadata { value, .. } => Some(value),
            _ => None,
        })
        .expect("import must publish its metadata register");
    assert_eq!(metadata.title, "Original");
    assert_eq!(metadata.contributors.len(), 1);
    assert_eq!(metadata.contributors[0].name(), "Original Author");
    assert_title_edit_syncs(&db);
}

#[test]
fn import_without_contributors_still_publishes_metadata() {
    let db = database();
    let mut book = import();
    book.inspection.as_mut().unwrap().metadata.contributors.clear();
    db.commit_import(book).unwrap();
    assert!(db.sync_publishable_mutations().unwrap().iter().any(|mutation| matches!(&mutation.body,
        library_replica::MutationBody::Metadata { value, .. } if value.title == "Original" && value.contributors.is_empty())));
}

#[test]
fn failed_contributor_write_rolls_back_and_restores_metadata_sync() {
    let db = database();
    db.commit_import(import()).unwrap();
    let queued_before = db.sync_publishable_mutations().unwrap();
    let identities_before: i64 = db.connection.query_row("SELECT count(*) FROM author_identity", [], |row| row.get(0)).unwrap();
    db.connection.execute_batch("CREATE TRIGGER reject_contributor BEFORE INSERT ON book_contributor BEGIN SELECT RAISE(ABORT,'injected contributor failure'); END;").unwrap();
    let changed_import = || {
        let mut next = import();
        next.inspection.as_mut().unwrap().metadata.title = "Changed title".into();
        // Unchanged contributors skip the INSERT entirely, so change the author
        // to ensure the injected failure exercises the transaction boundary.
        next.inspection.as_mut().unwrap().metadata.contributors = vec![book_model::Contributor::new("Changed author", book_model::MarcRelatorCode(*b"aut")).unwrap()];
        next
    };
    let error = db.commit_import(changed_import()).unwrap_err();
    assert!(error.to_string().contains("injected contributor failure"), "unexpected import failure: {error}");
    assert_eq!(db.connection.query_row("SELECT title FROM book", [], |r| r.get::<_, String>(0)).unwrap(), "Original");
    assert_eq!(db.connection.query_row("SELECT name FROM book_contributor", [], |r| r.get::<_, String>(0)).unwrap(), "Original Author");
    assert_eq!(db.connection.query_row("SELECT count(*) FROM author_identity", [], |row| row.get::<_, i64>(0)).unwrap(), identities_before, "failed import must not leave a new author identity");
    assert_eq!(db.sync_publishable_mutations().unwrap(), queued_before, "failed import must not change pending sync records");
    assert_title_edit_syncs(&db);
    db.connection.execute_batch("DROP TRIGGER reject_contributor;").unwrap();
    db.commit_import(changed_import()).unwrap();
    assert_eq!(db.connection.query_row("SELECT title FROM book", [], |r| r.get::<_, String>(0)).unwrap(), "Changed title");
    assert_eq!(db.connection.query_row("SELECT name FROM book_contributor", [], |r| r.get::<_, String>(0)).unwrap(), "Changed author");
}

fn recovery_payloads(db: &Database) -> Vec<(String, String, Vec<u8>)> {
    db.sync_publishable_mutations().unwrap();
    db.connection
        .prepare("SELECT state_kind,state_subkey,body FROM sync_outbox WHERE state_kind IN ('book_toc','pdf_reader_metadata') ORDER BY state_kind,state_subkey")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn recovery_republishes_toc_and_each_pdf_checksum() {
    let db = database();
    let book = import();
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    let mut cells = vec![StateCell { kind: "book_toc".into(), entity_key: hash.to_string(), entity_subkey: String::new() }];
    for (checksum, width) in [("a".repeat(64), 100.0), ("b".repeat(64), 200.0)] {
        let metadata =
            pdf_view_common::PdfReaderMetadata { version: 1, checksum: checksum.clone(), pages: vec![pdf_view_common::PdfStoredPage { width, height: 300.0, rotation: 0 }], skippable_pages: None, annotations: vec![], dependencies: None };
        db.connection.execute("INSERT INTO pdf_reader_metadata VALUES (?1,?2,?3)", library_database::rusqlite::params![hash.as_str(), checksum, serde_json::to_vec(&metadata).unwrap()]).unwrap();
        cells.push(StateCell { kind: "pdf_reader_metadata".into(), entity_key: hash.to_string(), entity_subkey: checksum });
    }
    let expected = recovery_payloads(&db);
    assert_eq!(expected.len(), 3);
    db.clear_sync_outbox().unwrap();
    assert_eq!(db.sync_enqueue_missing_state_cells(&cells).unwrap(), 3);
    assert_eq!(recovery_payloads(&db), expected);
    assert_eq!(db.sync_enqueue_missing_state_cells(&cells).unwrap(), 0);
    db.clear_sync_outbox().unwrap();
    db.sync_begin_cursor_recovery().unwrap();
    let mut after = None;
    loop {
        let (next, _) = db.sync_recover_state_page(after.as_ref()).unwrap();
        if next.is_none() {
            break;
        }
        after = next;
    }
    assert_eq!(recovery_payloads(&db), expected);
    assert!(!db.sync_cursor_recovery_pending().unwrap());
    let missing = StateCell { kind: "pdf_reader_metadata".into(), entity_key: hash.to_string(), entity_subkey: "c".repeat(64) };
    assert_eq!(db.sync_enqueue_missing_state_cells(&[missing]).unwrap(), 0);
}

fn materialize(db: &Database, id: DirId, renamed_to: Option<&str>) {
    let snapshot = db.native_directory_snapshot(&id).unwrap().unwrap();
    assert!(snapshot.relative_path.starts_with('/'));
    assert_eq!(db.acknowledge_native_directory(NativeDirectoryCompletion { snapshot, renamed_to: renamed_to.map(str::to_owned), materialized: true }).unwrap(), NativeDirectoryAcknowledgement::Applied);
}

fn projection(db: &Database, id: DirId) -> String {
    db.connection.query_row("SELECT relative_path FROM local_file_projection WHERE dir_id=?1", [id.to_string()], |r| r.get(0)).unwrap()
}

#[test]
fn directory_renames_move_descendants_but_not_similarly_named_siblings() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let child = db.create_directory(&shelf, &"Child".into()).unwrap().id;
    let sibling = db.create_directory(&ROOT_DIR_ID, &"Shelfish".into()).unwrap().id;
    let hash = ContentHash::new(&"9".repeat(64));
    for (id, path) in [(shelf, "/Shelf/book.epub"), (child, "/Shelf/Child/book.epub"), (sibling, "/Shelfish/book.epub")] {
        materialize(&db, id, None);
        db.seed_file_projection(&hash, &id, path);
    }
    materialize(&db, shelf, Some("Shelf (2)"));
    assert_eq!(projection(&db, shelf), "/Shelf (2)/book.epub");
    assert_eq!(projection(&db, child), "/Shelf (2)/Child/book.epub");
    assert_eq!(projection(&db, sibling), "/Shelfish/book.epub");
    assert_eq!(db.native_directory_snapshot(&child).unwrap().unwrap().projected_path.as_deref(), Some("/Shelf (2)/Child"));
    db.move_directory(&shelf, None, Some("Moved")).unwrap();
    materialize(&db, shelf, None);
    assert_eq!(projection(&db, child), "/Moved/Child/book.epub");
}

#[test]
fn directory_acknowledgement_rolls_back_when_projection_write_fails() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    materialize(&db, shelf, None);
    db.connection.execute_batch("CREATE TRIGGER reject_projection BEFORE UPDATE ON local_directory_projection BEGIN SELECT RAISE(ABORT,'injected projection failure'); END;").unwrap();
    let snapshot = db.native_directory_snapshot(&shelf).unwrap().unwrap();
    assert!(db.acknowledge_native_directory(NativeDirectoryCompletion { snapshot, renamed_to: Some("Changed".into()), materialized: true }).is_err());
    let current = db.native_directory_snapshot(&shelf).unwrap().unwrap();
    assert_eq!(current.name, "Shelf");
    assert_eq!(current.projected_path.as_deref(), Some("/Shelf"));
}

fn audible_job(db: &Database) -> (library_database::AudibleBatchJob, metadata_contract::audible::LookupResponse) {
    use metadata_contract::audible::*;
    let book = import();
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    let request = LookupRequest { title: "Audio".into(), authors: vec![], duration_ms: 1000, recording: Default::default(), chapters: vec![] };
    db.connection.execute("INSERT INTO audiobook_metadata(content_hash,duration_ms) VALUES(?1,1000)", [hash.as_str()]).unwrap();
    db.connection
        .execute(
            "INSERT INTO audible_enrichment_jobs(content_hash,policy,request_json,status,updated_at) VALUES(?1,?2,?3,'pending',0)",
            library_database::rusqlite::params![hash.as_str(), POLICY_VERSION, serde_json::to_string(&request).unwrap()],
        )
        .unwrap();
    let job = db.claim_audible_batch(10, hash).unwrap().unwrap();
    let response = LookupResponse {
        status: LookupStatus::Supported,
        candidates: vec![Candidate {
            asin: "B012345678".into(),
            region: "us".into(),
            title: "Audio".into(),
            subtitle: None,
            authors: vec![],
            narrators: vec![],
            publisher: None,
            description: None,
            release_date: None,
            isbns: vec![],
            abridged: None,
            provider_duration_ms: Some(1000),
            duration_ms: Some(1000),
            chapters: vec![],
            source: DiscoverySource::Api,
            metadata_provider: "audible".into(),
            chapter_provider: None,
        }],
        selected_asin: Some("B012345678".into()),
        selected_region: Some("us".into()),
        used_website_fallback: false,
        detail: String::new(),
        chapter_plan: Some(ChapterPlan {
            method: AlignmentMethod::EqualCountNames,
            chapters: vec![Chapter { title: "Opening".into(), start_ms: 0, end_ms: 400 }, Chapter { title: "Conclusion".into(), start_ms: 400, end_ms: 1000 }],
            renamed: 2,
            detail: String::new(),
        }),
    };
    (job, response)
}

#[test]
fn audible_success_stores_playable_and_synchronized_navigation() {
    let db = database();
    let (job, response) = audible_job(&db);
    db.clear_sync_outbox().unwrap();
    assert!(db.resolve_audible_batch(&job, &response, 400, 20).unwrap());
    let chapters = db.audiobook_chapters(&job.content_hash).unwrap();
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[1].title, "Conclusion");
    assert_eq!(chapters[1].start_ms, 400);
    assert_eq!(chapters[1].end_ms, 1000);
    assert!(
        db.sync_publishable_mutations()
            .unwrap()
            .iter()
            .any(|mutation| matches!(&mutation.body, library_replica::MutationBody::BookToc { value, .. } if value.entries.len() == 2 && value.entries[1].target == book_model::audiobook_toc_target(400) && value.duration_ms == Some(1000)))
    );
    assert!(db.audible_batch_candidates(500, &[job.content_hash]).unwrap().is_empty());
}

#[test]
fn stored_audiobook_navigation_syncs_with_its_duration() {
    let db = database();
    let hash = ContentHash::new(&"a".repeat(64));
    db.connection.execute("INSERT INTO book(content_hash) VALUES(?1)", [hash.as_str()]).unwrap();
    let entries = vec![book_model::BookTocEntry { title: "Only".into(), target: book_model::audiobook_toc_target(0), children: Vec::new() }];
    db.clear_sync_outbox().unwrap();
    db.connection.execute("INSERT INTO audiobook_metadata(content_hash,duration_ms) VALUES(?1,1000)", rusqlite::params![hash.to_string()]).unwrap();
    db.connection.execute("INSERT INTO book_toc(content_hash,entry_count,toc_json) VALUES(?1,1,?2)", rusqlite::params![hash.to_string(), serde_json::to_string(&entries).unwrap()]).unwrap();
    assert!(db.sync_publishable_mutations().unwrap().iter().any(|mutation| matches!(&mutation.body, library_replica::MutationBody::BookToc { value, .. } if value.entries == entries && value.duration_ms == Some(1000))));
}


#[test]
fn audible_toc_failure_preserves_claim_and_allows_retry() {
    let db = database();
    let (job, response) = audible_job(&db);
    db.connection.execute_batch("CREATE TRIGGER fail_audible_toc BEFORE UPDATE ON book_toc BEGIN SELECT RAISE(ABORT,'injected TOC failure'); END;").unwrap();
    assert!(db.resolve_audible_batch(&job, &response, 400, 20).is_err());
    let status: String = db.connection.query_row("SELECT status FROM audible_enrichment_jobs", [], |row| row.get(0)).unwrap();
    assert_eq!(status, "resolving");
    assert_eq!(db.audible_batch_candidates(500, &[job.content_hash]).unwrap(), vec![job.content_hash]);
    db.connection.execute_batch("DROP TRIGGER fail_audible_toc;").unwrap();
    assert!(db.resolve_audible_batch(&job, &response, 400, 20).unwrap());
}

#[test]
fn stale_audible_response_does_not_change_navigation() {
    let db = database();
    let (job, mut response) = audible_job(&db);
    assert!(db.resolve_audible_batch(&job, &response, 400, 20).unwrap());
    response.chapter_plan.as_mut().unwrap().chapters[0].title = "Stale".into();
    assert!(!db.resolve_audible_batch(&job, &response, 400, 21).unwrap());
    assert_eq!(db.audiobook_chapters(&job.content_hash).unwrap()[0].title, "Opening");
}

fn copy_fixture(db: &Database) -> (ContentHash, DirId, DirId) {
    let source = db.create_directory(&ROOT_DIR_ID, &"A".into()).unwrap().id;
    let destination = db.create_directory(&ROOT_DIR_ID, &"Z".into()).unwrap().id;
    let mut book = import();
    let hash = book.content_hash;
    book.parent_id = source;
    book.published.relative_path = "/A/book.epub".into();
    db.commit_import(book).unwrap();
    db.transfer_book_placement(&hash, &source, &destination, false).unwrap();
    (hash, source, destination)
}

fn restore_snapshot(db: &Database) -> library_database::NativeFileWorkSnapshot {
    let work = db.native_file_work().unwrap().into_iter().find(|work| work.operation == "restore").unwrap();
    db.native_file_work_snapshot(work).unwrap().unwrap()
}

fn restore_completion(snapshot: library_database::NativeFileWorkSnapshot) -> library_database::NativeFileWorkCompletion {
    library_database::NativeFileWorkCompletion { final_path: snapshot.resolved_path.clone(), snapshot, renamed: None, published: true, thumbnail_set_exists: true }
}

#[test]
fn copy_restores_and_acknowledges_the_queued_destination() {
    let db = database();
    let (_, source, destination) = copy_fixture(&db);
    let snapshot = restore_snapshot(&db);
    assert_eq!(snapshot.resolved_path, "/Z/book.epub");
    assert_eq!(snapshot.placement.as_ref().unwrap().0, destination.to_string());
    db.acknowledge_native_file_work(restore_completion(snapshot)).unwrap();
    assert_eq!(projection(&db, source), "/A/book.epub");
    assert_eq!(projection(&db, destination), "/Z/book.epub");
    assert!(db.native_file_work().unwrap().is_empty());
}

#[test]
fn directory_move_keeps_queued_restore_bound_to_its_destination() {
    let db = database();
    let (_, _, destination) = copy_fixture(&db);
    materialize(&db, destination, None);
    db.move_directory(&destination, None, Some("Moved")).unwrap();
    materialize(&db, destination, None);
    let snapshot = restore_snapshot(&db);
    assert_eq!(snapshot.resolved_path, "/Moved/book.epub");
    assert_eq!(snapshot.placement.as_ref().unwrap().0, destination.to_string());
    db.acknowledge_native_file_work(restore_completion(snapshot)).unwrap();
    assert_eq!(projection(&db, destination), "/Moved/book.epub");
}

#[test]
fn stale_restore_does_not_overwrite_projection_or_consume_work() {
    let db = database();
    let (_, _, destination) = copy_fixture(&db);
    let snapshot = restore_snapshot(&db);
    db.move_directory(&destination, None, Some("Moved")).unwrap();
    assert!(db.acknowledge_native_file_work(restore_completion(snapshot)).is_err());
    assert!(!db.native_file_work().unwrap().is_empty());
    let projections: i64 = db.connection.query_row("SELECT count(*) FROM local_file_projection WHERE dir_id=?1", [destination.to_string()], |row| row.get(0)).unwrap();
    assert_eq!(projections, 0);
}

#[test]
fn trash_work_does_not_target_another_live_copy() {
    let db = database();
    let (hash, source, destination) = copy_fixture(&db);
    db.acknowledge_native_file_work(restore_completion(restore_snapshot(&db))).unwrap();
    db.remove_book_placement(&hash, &source).unwrap();
    let work = db.native_file_work().unwrap().into_iter().find(|work| work.operation == "trash").unwrap();
    let snapshot = db.native_file_work_snapshot(work).unwrap().unwrap();
    assert_eq!(snapshot.resolved_path, "/A/book.epub");
    assert!(!snapshot.live);
    let completion = restore_completion(snapshot);
    db.acknowledge_native_file_work(completion).unwrap();
    assert_eq!(projection(&db, destination), "/Z/book.epub");
}

#[cfg(feature = "scanner")]
mod scanner {
    use super::*;
    use library_database::{FilesystemScan, InspectedFilesystemScan, ScanApplyResult, ScanFileObservation, ScannedBook};

    fn fixture() -> (Database, ContentHash) {
        let db = database();
        let mut book = import();
        let hash = book.content_hash;
        book.published.published = true;
        book.published.fingerprint = Some("1".into());
        db.commit_import(book).unwrap();
        drain_work(&db);
        (db, hash)
    }

    fn drain_work(db: &Database) {
        db.connection.execute_batch("DELETE FROM local_file_work; DELETE FROM local_book_work; DELETE FROM local_directory_work;").unwrap();
    }

    fn observation(directory: DirId, name: &str, fingerprint: u64) -> ScanFileObservation {
        ScanFileObservation { directory, name: name.into(), path: name.into(), fingerprint }
    }

    fn replacement() -> ScannedBook {
        let hash = ContentHash::new(&"b".repeat(64));
        ScannedBook {
            directory: ROOT_DIR_ID,
            name: "book.epub".into(),
            path: "book.epub".into(),
            fingerprint: 2,
            content_hash: hash,
            checksum: hash,
            size_bytes: 20,
            extension: "epub".into(),
            inspection: import().inspection.unwrap(),
            inspection_version: 1,
        }
    }

    fn local_state(db: &Database, hash: ContentHash, directory: DirId) -> (bool, bool, String, i64) {
        db.connection.query_row("SELECT bd.deleted_at IS NULL,bd.is_downloaded,bd.local_hash,(SELECT count(*) FROM local_file_projection p WHERE p.content_hash=b.content_hash AND p.dir_id=bd.dir_id) FROM book_dir bd JOIN book b ON b.row_id=bd.book_row_id WHERE b.content_hash=?1 AND bd.dir_id=?2", library_database::rusqlite::params![hash.as_str(), directory.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap()
    }

    #[test]
    fn scan_batches_publish_books_before_final_missing_file_cleanup() {
        let (db, missing) = fixture();
        let mut snapshot = db.scan_snapshot().unwrap();
        let mut first = replacement();
        first.name = "first.epub".into();
        first.path = first.name.clone();
        let first_hash = first.content_hash;
        let discovery = FilesystemScan { readable: true, files: vec![observation(ROOT_DIR_ID, "first.epub", 2), observation(ROOT_DIR_ID, "second.epub", 2)], ..Default::default() };
        assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![first] }, false).unwrap(), ScanApplyResult::Applied);
        assert_eq!(db.browse_book_count().unwrap(), 2, "first batch is immediately browsable");
        assert!(local_state(&db, first_hash, ROOT_DIR_ID).1);
        assert!(local_state(&db, missing, ROOT_DIR_ID).1, "incomplete scans must not clear missing files");
        let mut second = replacement();
        second.name = "second.epub".into();
        second.path = second.name.clone();
        second.content_hash = ContentHash::new(&"c".repeat(64));
        second.checksum = second.content_hash;
        assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![second] }, false).unwrap(), ScanApplyResult::Applied);
        assert_eq!(db.browse_book_count().unwrap(), 3);
        assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![] }, true).unwrap(), ScanApplyResult::Applied);
        assert!(!local_state(&db, missing, ROOT_DIR_ID).1);
        assert!(local_state(&db, first_hash, ROOT_DIR_ID).1);
    }

    #[test]
    fn concurrent_changes_reject_later_scan_batches_without_losing_committed_books() {
        let db = database();
        let mut snapshot = db.scan_snapshot().unwrap();
        let book = replacement();
        let hash = book.content_hash;
        let discovery = FilesystemScan { readable: true, files: vec![observation(ROOT_DIR_ID, "book.epub", 2)], ..Default::default() };
        assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![book] }, false).unwrap(), ScanApplyResult::Applied);
        db.queue_directory_work(&ROOT_DIR_ID).unwrap();
        let guard = snapshot.work_revision;
        assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![] }, true).unwrap(), ScanApplyResult::Stale);
        assert_eq!(snapshot.work_revision, guard);
        assert!(local_state(&db, hash, ROOT_DIR_ID).1);
    }

    #[test]
    fn scan_commits_bisac_subjects_with_valid_relationships() {
        let db = database();
        let mut book = replacement();
        book.inspection.metadata.book.subjects = vec![book_model::BookSubject::new(None, "History / United States / 20th Century", "dc:subject", Some("bisac".into()), Some("HIS036060".into())).unwrap()];
        let snapshot = db.scan_snapshot().unwrap();
        let discovery = FilesystemScan { readable: true, files: vec![observation(ROOT_DIR_ID, "book.epub", 2)], ..Default::default() };
        assert_eq!(db.apply_filesystem_scan(&snapshot, &discovery, &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![book] }).unwrap(), ScanApplyResult::Applied);
        assert!(db.connection.query_row("SELECT count(*) FROM book_subject_assignment WHERE system_id='bisac'", [], |r| r.get::<_, i64>(0)).unwrap() > 0);
        let coded_node: (String, bool) = db.connection.query_row("SELECT parent_path,assignable FROM subject_node WHERE system_id='bisac' AND code='HIS036060'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(coded_node.1);
        assert!(db.connection.query_row("SELECT EXISTS(SELECT 1 FROM subject_node WHERE system_id='bisac' AND path=?1)", [&coded_node.0], |r| r.get::<_, bool>(0)).unwrap());
        assert!(!db.connection.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
    }

    #[test]
    fn replacement_updates_only_the_old_placement_and_commits_other_discoveries() {
        let (db, old) = fixture();
        let other = db.create_directory(&ROOT_DIR_ID, &"Other".into()).unwrap().id;
        db.transfer_book_placement(&old, &ROOT_DIR_ID, &other, false).unwrap();
        db.seed_file_projection(&old, &other, "/Other/book.epub");
        drain_work(&db);
        let snapshot = db.scan_snapshot().unwrap();
        let replaced = replacement();
        let new_hash = replaced.content_hash;
        let mut extra = replacement();
        extra.name = "another.epub".into();
        extra.path = extra.name.clone();
        extra.content_hash = ContentHash::new(&"c".repeat(64));
        extra.checksum = extra.content_hash;
        let discovery = FilesystemScan {
            readable: true,
            directories: vec![],
            files: vec![observation(ROOT_DIR_ID, "book.epub", 2), observation(ROOT_DIR_ID, "another.epub", 2), ScanFileObservation { path: "Other/book.epub".into(), ..observation(other, "book.epub", 1) }],
        };
        assert_eq!(db.apply_filesystem_scan(&snapshot, &discovery, &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![replaced, extra] }).unwrap(), ScanApplyResult::Applied);
        assert_eq!(local_state(&db, old, ROOT_DIR_ID), (false, false, String::new(), 0));
        assert!(local_state(&db, old, other).0);
        assert!(local_state(&db, old, other).1);
        assert_eq!(local_state(&db, new_hash, ROOT_DIR_ID), (true, true, "2".into(), 1));
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book WHERE deleted_at IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
    }

    #[test]
    fn failed_replacement_rolls_back_old_placement_and_projection() {
        let (db, old) = fixture();
        let snapshot = db.scan_snapshot().unwrap();
        db.connection.execute_batch("CREATE TRIGGER fail_scan_import BEFORE INSERT ON book BEGIN SELECT RAISE(ABORT,'injected import failure'); END;").unwrap();
        let result = db.apply_filesystem_scan(
            &snapshot,
            &FilesystemScan { readable: true, files: vec![observation(ROOT_DIR_ID, "book.epub", 2)], ..Default::default() },
            &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![replacement()] },
        );
        assert!(result.is_err());
        assert_eq!(local_state(&db, old, ROOT_DIR_ID), (true, true, "1".into(), 1));
    }

    #[test]
    fn ordinary_import_still_rejects_an_occupied_filename() {
        let (db, old) = fixture();
        let mut book = import();
        book.content_hash = ContentHash::new(&"b".repeat(64));
        assert!(db.commit_import(book).is_err());
        assert_eq!(local_state(&db, old, ROOT_DIR_ID), (true, true, "1".into(), 1));
    }

    #[test]
    fn complete_scan_clears_only_missing_local_availability_without_sync_deletions() {
        let (db, hash) = fixture();
        // Downloads can have an empty fingerprint and must still be reconciled.
        db.connection.execute("UPDATE book_dir SET local_hash=''", []).unwrap();
        db.clear_sync_outbox().unwrap();
        let snapshot = db.scan_snapshot().unwrap();
        assert!(snapshot.excluded_paths.is_empty());
        assert_eq!(db.apply_filesystem_scan(&snapshot, &FilesystemScan { readable: true, ..Default::default() }, &InspectedFilesystemScan { readable: true, ..Default::default() }).unwrap(), ScanApplyResult::Applied);
        assert_eq!(local_state(&db, hash, ROOT_DIR_ID), (true, false, String::new(), 0));
        assert!(!db.is_book_downloaded(&hash).unwrap());
        assert!(db.sync_publishable_mutations().unwrap().is_empty());
        assert_eq!(db.browse_book_detail(hash).unwrap().book.title, "Original");
    }

    #[test]
    fn incomplete_or_excluded_scans_preserve_local_availability() {
        for (discovery_readable, inspection_readable, excluded) in [(false, true, false), (true, false, false), (true, true, true)] {
            let (db, hash) = fixture();
            let mut snapshot = db.scan_snapshot().unwrap();
            if excluded {
                snapshot.excluded_paths.push("book.epub".into());
            }
            db.apply_filesystem_scan(&snapshot, &FilesystemScan { readable: discovery_readable, ..Default::default() }, &InspectedFilesystemScan { readable: inspection_readable, ..Default::default() }).unwrap();
            assert_eq!(local_state(&db, hash, ROOT_DIR_ID), (true, true, "1".into(), 1));
        }
    }

    #[test]
    fn missing_copy_does_not_clear_an_observed_copy() {
        let (db, hash) = fixture();
        let other = db.create_directory(&ROOT_DIR_ID, &"Other".into()).unwrap().id;
        db.transfer_book_placement(&hash, &ROOT_DIR_ID, &other, false).unwrap();
        db.seed_file_projection(&hash, &other, "/Other/book.epub");
        drain_work(&db);
        let snapshot = db.scan_snapshot().unwrap();
        db.apply_filesystem_scan(
            &snapshot,
            &FilesystemScan { readable: true, files: vec![ScanFileObservation { path: "Other/book.epub".into(), ..observation(other, "book.epub", 1) }], ..Default::default() },
            &InspectedFilesystemScan { readable: true, ..Default::default() },
        )
        .unwrap();
        assert!(!local_state(&db, hash, ROOT_DIR_ID).1);
        assert!(local_state(&db, hash, other).1);
        assert!(db.is_book_downloaded(&hash).unwrap());
    }

    #[test]
    fn local_change_after_snapshot_rejects_scan_without_cleanup() {
        let (db, hash) = fixture();
        let snapshot = db.scan_snapshot().unwrap();
        db.connection.execute("UPDATE book_dir SET local_hash='newer'", []).unwrap();
        assert_eq!(db.apply_filesystem_scan(&snapshot, &FilesystemScan { readable: true, ..Default::default() }, &InspectedFilesystemScan { failures: vec![], readable: true, books: vec![replacement()] }).unwrap(), ScanApplyResult::Stale);
        assert_eq!(local_state(&db, hash, ROOT_DIR_ID), (true, true, "newer".into(), 1));
    }
}

#[test]
fn cloud_presence_reset_handles_empty_queues_and_persists_policy() {
    let db = database();
    for enabled in [false, true] {
        db.set_asset_storage_policy(enabled, true).unwrap();
        assert_eq!(db.connection.query_row("SELECT cloud_storage_enabled FROM sync_metadata", [], |r| r.get::<_, bool>(0)).unwrap(), enabled);
        assert!(db.pending_book_uploads(0, 10).unwrap().is_empty());
    }
}

#[test]
fn cloud_presence_reset_requeues_books_and_refreshes_existing_work() {
    use library_replica::BlobKind;
    let db = database();
    let mut book = import();
    let hash = book.content_hash;
    book.published.published = true;
    book.published.fingerprint = Some("1".into());
    db.commit_import(book).unwrap();
    db.record_remote_asset(BlobKind::Book, &hash).unwrap();
    db.settle_rejected_asset(&hash, library_database::RejectedAsset::BookUpload { intent: None }, "quota").unwrap();
    let before = db.book_upload_intent(&hash).unwrap().unwrap();
    let work_before: i64 = db.connection.query_row("SELECT id FROM asset_work WHERE content_hash=?1", [hash.as_str()], |r| r.get(0)).unwrap();

    db.set_asset_storage_policy(true, true).unwrap();
    let after = db.book_upload_intent(&hash).unwrap().unwrap();
    assert!(after.id > before.id);
    assert_eq!(after.checksum, before.checksum);
    assert_eq!(after.size_bytes, before.size_bytes);
    assert!(!db.remote_asset_hashes(BlobKind::Book).unwrap().contains(&hash));
    assert!(db.book_upload_rejection_reason(&hash).unwrap().is_none());
    let work_after: i64 = db.connection.query_row("SELECT id FROM asset_work WHERE content_hash=?1", [hash.as_str()], |r| r.get(0)).unwrap();
    assert!(work_after > work_before);

    // An already-uploaded book has no intent, but its local baseline must
    // recreate one when the cloud-presence cache is reset.
    db.connection.execute("DELETE FROM local_book_upload WHERE content_hash=?1", [hash.as_str()]).unwrap();
    db.record_remote_asset(BlobKind::Book, &hash).unwrap();
    db.set_asset_storage_policy(true, true).unwrap();
    let prepared = db.upload_preparation_snapshot().unwrap();
    db.finish_upload_preparation(&prepared).unwrap();
    let requeued = db.pending_book_uploads(0, 10).unwrap();
    assert_eq!(requeued.len(), 1);
    assert_eq!(requeued[0].content_hash, hash);
    assert_eq!(requeued[0].checksum, before.checksum);
    assert_eq!(requeued[0].size_bytes, before.size_bytes);
}

fn assert_uninitialized(db: &Database) {
    assert!(db.connection.is_autocommit(), "failed initialization must release its transaction");
    assert_eq!(db.connection.query_row("PRAGMA main.user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(db.connection.query_row("SELECT count(*) FROM main.sqlite_schema", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn interrupted_initialization_rolls_back_and_retries_after_reopen() {
    struct TestDirectory(std::path::PathBuf);
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = TestDirectory(std::env::temp_dir().join(format!("library-init-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&directory.0).unwrap();
    for budget in [500, 5000] {
        let path = directory.0.join(format!("{budget}.sqlite"));
        {
            let db = Database::open(&path).unwrap();
            let mut operations = 0;
            // Interrupt once, leaving rollback free to finish normally.
            db.connection
                .progress_handler(
                    100,
                    Some(move || {
                        operations += 100;
                        operations == budget
                    }),
                )
                .unwrap();
            let result = db.initialize_library();
            db.connection.progress_handler(0, None::<fn() -> bool>).unwrap();
            assert!(result.is_err(), "expected initialization interruption at {budget}");
            assert_uninitialized(&db);
        }
        let replica = {
            let db = Database::open(&path).unwrap();
            assert_uninitialized(&db);
            let replica = db.initialize_library().unwrap();
            assert_eq!(db.initialize_library().unwrap(), replica);
            db.commit_import(import()).unwrap();
            replica
        };
        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.initialize_library().unwrap(), replica);
        assert_eq!(reopened.browse_book_detail(import().content_hash).unwrap().book.title, "Original");
    }
}

#[test]
fn failing_final_schema_version_write_rolls_back_schema_and_replica() {
    use library_database::rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let db = Database::open(":memory:").unwrap();
    db.connection
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Pragma { pragma_name: "user_version", pragma_value: Some(_) } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let result = db.initialize_library();
    db.connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
    assert!(result.is_err());
    assert_uninitialized(&db);
    let replica = db.initialize_library().unwrap();
    assert_eq!(db.initialize_library().unwrap(), replica);
}

#[test]
fn subject_cache_rebuilds_before_subject_browsing() {
    let db = database();
    let mut book = import();
    let hash = book.content_hash;
    book.inspection.as_mut().unwrap().metadata.book.subjects = vec![book_model::BookSubject::new(None, "History / United States / 20th Century", "dc:subject", Some("bisac".into()), Some("HIS036060".into())).unwrap()];
    db.commit_import(book).unwrap();
    // An empty cache must be built before browsing.
    db.connection.execute("DELETE FROM library_subject", []).unwrap();
    let (path, parent): (String, String) = db
        .connection
        .query_row("SELECT route.path,route.parent_path FROM book_unified_concept assignment JOIN curated.unified_concept_paths route ON route.concept_id=assignment.concept_id WHERE route.path!=route.parent_path LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    let mut query = library_model::LibraryBrowseQuery {
        location: parent,
        search: String::new(),
        file_types: vec![],
        chip_sort: library_model::BrowseChipSort::Alphabetical,
        book_sort: library_model::BrowseBookSort::Alphabetical,
        languages: vec![],
        hide_finished: false,
        include_direct_child_books: false,
    };
    let parent_contents = db.browse_subject_contents(&query, false).unwrap();
    assert!(parent_contents.children.is_empty());
    assert!(parent_contents.books.is_empty());
    assert!(db.connection.query_row("SELECT COUNT(*) FROM library_subject", [], |row| row.get::<_, i64>(0)).unwrap() > 0);
    query.location = path;
    let contents = db.browse_subject_contents(&query, false).unwrap();
    assert!(contents.books.is_empty());
    query.location = "Subject".into();
    query.search.clear();
    let root_contents = db.browse_subject_contents(&query, false).unwrap();
    assert_eq!(root_contents.books[0].content_hash, hash);
    query.search = "Original".into();
    let searched = db.browse_subject_contents(&query, false).unwrap();
    assert!(searched.children.is_empty());
    assert_eq!(searched.books.len(), 1);
}

fn subject_query(location: &str) -> library_model::LibraryBrowseQuery {
    library_model::LibraryBrowseQuery {
        location: location.into(),
        search: String::new(),
        file_types: vec![],
        languages: vec![],
        chip_sort: library_model::BrowseChipSort::Alphabetical,
        book_sort: library_model::BrowseBookSort::Alphabetical,
        hide_finished: false,
        include_direct_child_books: false,
    }
}

fn cached_subject_fixture() -> (Database, String, String) {
    let db = database();
    for index in 1..=4 {
        let mut book = import();
        book.content_hash = ContentHash::new(&format!("{index:064x}"));
        book.checksum = book.content_hash;
        book.file_name = format!("book{index}.epub");
        book.published.name = book.file_name.clone();
        book.published.relative_path = format!("/{}", book.file_name);
        book.inspection.as_mut().unwrap().metadata.title = format!("Book {index}");
        book.inspection.as_mut().unwrap().metadata.book.subjects=vec![book_model::BookSubject::new(None,"History / United States / 20th Century","dc:subject",Some("bisac".into()),Some("HIS036060".into())).unwrap()];
        db.commit_import(book).unwrap();
    }
    let (path,parent):(String,String)=db.connection.query_row("SELECT path,parent_path FROM curated.unified_concept_paths WHERE concept_id IN (SELECT concept_id FROM book_unified_concept) AND path!=parent_path AND parent_path!='Subject' LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    // Subsequent browse-only edits explicitly operate on the derived fixture.
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'",[]).unwrap();
    (db,path,parent)
}

#[test]
fn subject_cache_is_reused_and_updated_after_placement_and_subject_edits() {
    use library_database::rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let (db, _, _) = cached_subject_fixture();
    let root = db.browse_subject_contents(&subject_query("Subject"), false).unwrap();
    let query = subject_query(&root.children[0].id);
    let initial = db.browse_subject_contents(&query, false).unwrap();
    assert!(initial.children.is_empty());
    assert_eq!(initial.books.len(), 4);
    let changes = db.connection.total_changes();
    // Once built, membership comes from the cache; names come from the immutable taxonomy.
    db.connection
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read { table_name: "book_unified_concept", .. } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    assert_eq!(db.browse_subject_contents(&query, false).unwrap().children.len(), initial.children.len());
    db.connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
    assert_eq!(db.connection.total_changes(), changes);
    db.connection.execute("UPDATE book_dir SET deleted_at=10 WHERE book_row_id=(SELECT row_id FROM book WHERE title='Book 4')", []).unwrap();
    let shrunk = db.browse_subject_contents(&query, false).unwrap();
    assert!(shrunk.children.is_empty());
    assert_eq!(shrunk.books.len(), 3);
    db.connection.execute("UPDATE book_dir SET deleted_at=NULL", []).unwrap();
    assert_eq!(db.browse_subject_contents(&query, false).unwrap().books.len(), 4);
    db.connection.execute("DELETE FROM book_unified_concept", []).unwrap();
    let empty = db.browse_subject_contents(&query, false).unwrap();
    assert!(empty.children.is_empty() && empty.books.is_empty());
    assert_eq!(db.connection.query_row("SELECT COUNT(*) FROM subject_browse_member", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn subject_cache_rebuild_failure_rolls_back_and_retries() {
    let (db, _, _) = cached_subject_fixture();
    db.connection.execute_batch("DELETE FROM subject_browse_member; DELETE FROM library_subject; UPDATE subject_browse_state SET built_revision=-1; INSERT OR IGNORE INTO subject_browse_dirty_book SELECT row_id FROM book;").unwrap();
    db.connection.execute_batch("CREATE TRIGGER reject_subject_cache BEFORE INSERT ON subject_browse_member BEGIN SELECT RAISE(ABORT,'injected cache failure'); END;").unwrap();
    assert!(db.browse_subject_contents(&subject_query("Subject"), false).is_err());
    assert_eq!(db.connection.query_row("SELECT COUNT(*) FROM library_subject", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert!(db.connection.query_row("SELECT COUNT(*) FROM subject_browse_dirty_book", [], |r| r.get::<_, i64>(0)).unwrap() > 0);
    db.connection.execute_batch("DROP TRIGGER reject_subject_cache").unwrap();
    assert!(!db.browse_subject_contents(&subject_query("Subject"), false).unwrap().children.is_empty());
}

#[test]
fn single_folder_walk_handles_nested_transactions_renames_and_broken_ancestry() {
    let db = database();
    let parent = DirId::new_v4();
    let child = DirId::new_v4();
    db.seed_dir(&parent, &ROOT_DIR_ID, "Shelf");
    db.seed_dir(&child, &parent, "History");
    assert_eq!(db.directory_relative_path_string(&child).unwrap().as_deref(), Some("Shelf/History"));
    assert_eq!(db.dir_path_components(&child).unwrap().iter().map(|r| r.id).collect::<Vec<_>>(), vec![parent, child]);
    let tx = db.connection.unchecked_transaction().unwrap();
    tx.execute("UPDATE dir SET name='Renamed' WHERE id=?1", [parent.to_string()]).unwrap();
    assert_eq!(db.directory_relative_path_string(&child).unwrap().as_deref(), Some("Renamed/History"));
    tx.rollback().unwrap();
    assert_eq!(db.directory_relative_path_string(&child).unwrap().as_deref(), Some("Shelf/History"));
    db.connection.execute("UPDATE dir SET deleted_at=1 WHERE id=?1", [parent.to_string()]).unwrap();
    assert_eq!(db.directory_relative_path_string(&child).unwrap(), None);
    assert!(db.dir_path_components(&child).unwrap().is_empty());
    assert_eq!(db.directory_relative_path_string(&DirId::new_v4()).unwrap(), None);
    db.connection.execute("UPDATE dir SET deleted_at=NULL,parent_id=?1 WHERE id=?2", [child.to_string(), parent.to_string()]).unwrap();
    assert!(db.directory_relative_path_string(&child).unwrap_err().to_string().contains("cycle"));
    assert!(db.connection.is_autocommit(), "failed walks must release their snapshot");
}


#[test]
fn subject_route_cache_invalidates_when_the_bundled_taxonomy_changes() {
    let (db, _, _) = cached_subject_fixture();
    db.browse_subject_contents(&subject_query("Subject"), false).unwrap();
    db.connection.execute("UPDATE subject_browse_state SET taxonomy_revision='previous bundle'", []).unwrap();
    // An ID can refer to a different route in a new bundle, so all old memberships must go.
    db.connection.execute("INSERT INTO subject_browse_member SELECT -100,row_id,1 FROM book", []).unwrap();
    db.connection.execute("INSERT INTO library_subject VALUES(-100)", []).unwrap();
    let view = db.browse_subject_contents(&subject_query("Subject"), false).unwrap();
    assert_eq!(view.children[0].book_count, 4);
    assert_eq!(db.connection.query_row("SELECT count(*) FROM subject_browse_member WHERE route_id=-100", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(db.connection.query_row("SELECT taxonomy_revision FROM subject_browse_state", [], |r| r.get::<_, String>(0)).unwrap(), format!("{}:{}", subject_projection::BUNDLED_UNIFIED_TAXONOMY_REVISION_ID, subject_projection::UNIFIED_MATCHER_VERSION));
}

#[test]
fn subject_route_ids_preserve_facets_author_labels_and_book_detail_paths() {
    let (db, path, parent) = cached_subject_fixture();
    let label: String = db.connection.query_row("SELECT label FROM curated.unified_concept_paths WHERE path=?1", [&path], |r| r.get(0)).unwrap();
    let (_, formats) = db.browse_subject_facet_counts(&parent, &label, &[], &[], false).unwrap();
    assert_eq!(formats.iter().map(|f| f.book_count).sum::<u32>(), 4);
    let (_, missing) = db.browse_subject_facet_counts("No such subject", "", &[], &[], false).unwrap();
    assert!(missing.is_empty());
    let authors = db.browse_authors(Default::default(), false).unwrap();
    assert!(authors.authors.iter().any(|a| a.subjects.iter().any(|s| s.label == label)));
    let detail = db.browse_book_detail(ContentHash::new(&format!("{:064x}", 1))).unwrap();
    assert!(detail.subject_paths.contains(&path));
}


fn browse_folder(db: &Database, dir: DirId, search: &str, languages: &[&str]) -> library_model::BrowseContents {
    let mut query = subject_query(&dir.to_string());
    query.search = search.into();
    query.languages = languages.iter().map(|language| (*language).to_owned()).collect();
    db.browse_folder_contents(&query, false).unwrap()
}

fn import_in_folder(db: &Database, index: u64, dir: DirId) -> ContentHash {
    let mut book = import();
    book.parent_id = dir;
    book.content_hash = ContentHash::new(&format!("{index:064x}"));
    book.checksum = book.content_hash;
    book.file_name = format!("book{index}.epub");
    book.published.name = book.file_name.clone();
    book.published.relative_path = format!("/{}", book.file_name);
    book.inspection.as_mut().unwrap().metadata.title = format!("Title {index}");
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    hash
}

fn place_book(db: &Database, hash: ContentHash, dir: DirId, downloaded: bool) {
    db.connection
        .execute(
            "INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) SELECT ?1,row_id,'copy.epub','',?3 FROM book WHERE content_hash=?2",
            library_database::rusqlite::params![dir.to_string(), hash.as_str(), downloaded],
        )
        .unwrap();
}

#[test]
fn folder_projection_preserves_hidden_paths_distinct_counts_and_search_sources() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let nested = db.create_directory(&shelf, &"Nested".into()).unwrap().id;
    let hidden = db.create_directory(&shelf, &".Hidden".into()).unwrap().id;
    let empty = db.create_directory(&ROOT_DIR_ID, &"Empty".into()).unwrap().id;
    let first = import_in_folder(&db, 101, nested);
    let second = import_in_folder(&db, 102, hidden);
    let third = import_in_folder(&db, 103, shelf);
    // Exercise derived browse rows without producing unrelated canonical edits.
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'",[]).unwrap();
    db.connection.execute("UPDATE book_dir SET is_downloaded=0", []).unwrap();
    place_book(&db, first, shelf, false);
    place_book(&db, first, hidden, true);
    db.connection.execute("UPDATE book_dir SET is_downloaded=1 WHERE book_row_id IN (SELECT row_id FROM book WHERE content_hash IN (?1,?2))", [second.as_str(), third.as_str()]).unwrap();
    db.connection.execute("UPDATE book SET format='pdf' WHERE content_hash=?1", [third.as_str()]).unwrap();
    db.connection.execute("INSERT INTO book_language(book_row_id,position,language_tag) SELECT row_id,0,' EN_us ' FROM book WHERE content_hash!=?1", [third.as_str()]).unwrap();
    let view = browse_folder(&db, ROOT_DIR_ID, "", &[]);
    let chip = view.children.iter().find(|c| c.id == shelf.to_string()).unwrap();
    assert_eq!((chip.book_count, chip.downloaded_book_count), (2, 1), "hidden downloads must not inflate visible subtree counts");
    assert_eq!(view.children.iter().find(|c| c.id == empty.to_string()).unwrap().book_count, 0);
    assert!(view.books.is_empty(), "ordinary browsing only shows direct placements");
    let direct = browse_folder(&db, shelf, "", &[]);
    assert_eq!(direct.books.len(), 2);
    assert!(direct.books.iter().all(|b| b.source_directory == Some(shelf)));
    let searched = browse_folder(&db, shelf, "Title", &[]);
    assert_eq!(searched.books.len(), 3, "search includes dot-directory placements and deduplicates books");
    let source = [shelf, nested, hidden].into_iter().min_by_key(|id| id.to_string()).unwrap();
    assert_eq!(searched.books.iter().find(|b| b.content_hash == first).unwrap().source_directory, Some(source));
    let filtered = browse_folder(&db, ROOT_DIR_ID, "", &["en"]);
    assert_eq!(filtered.children.len(), 1);
    assert_eq!(filtered.children[0].book_count, 1);
    assert_eq!(browse_folder(&db, shelf, "Title", &["EN_us"]).books.len(), 2);
    let (languages, formats) = db.browse_folder_facet_counts(&shelf.to_string(), "", &[], &["en"], false).unwrap();
    assert_eq!(languages.iter().find(|l| l.language == "en").unwrap().book_count, 2);
    assert_eq!(formats.iter().map(|f| f.book_count).sum::<u32>(), 2);
    // A folder-name search still finds matching descendant chips and breadcrumbs.
    let nested_search = browse_folder(&db, ROOT_DIR_ID, "Nested", &[]);
    assert_eq!(nested_search.children.len(), 1);
    assert_eq!(nested_search.children[0].path.as_deref(), Some("Shelf"));
    assert_eq!(nested_search.children[0].book_count, 1);
    // Removing one copy preserves membership; moving the last visible copy
    // changes chip counts while search can still find its hidden placement.
    db.connection.execute("DELETE FROM book_dir WHERE dir_id=?1 AND book_row_id=(SELECT row_id FROM book WHERE content_hash=?2)", [shelf.to_string(), first.to_string()]).unwrap();
    assert_eq!(browse_folder(&db, ROOT_DIR_ID, "", &[]).children.iter().find(|c| c.id == shelf.to_string()).unwrap().book_count, 2);
    db.connection.execute("UPDATE book_dir SET dir_id=?1 WHERE dir_id=?2 AND book_row_id=(SELECT row_id FROM book WHERE content_hash=?3)", [empty.to_string(), nested.to_string(), first.to_string()]).unwrap();
    let moved = browse_folder(&db, ROOT_DIR_ID, "", &[]);
    assert_eq!(moved.children.iter().find(|c| c.id == shelf.to_string()).unwrap().book_count, 1);
    assert_eq!(moved.children.iter().find(|c| c.id == empty.to_string()).unwrap().book_count, 1);
    assert_eq!(browse_folder(&db, shelf, "Title", &[]).books.iter().find(|b| b.content_hash == first).unwrap().source_directory, Some(hidden));
    // Book visibility is read live, independently of placement-cache invalidation.
    db.connection.execute("UPDATE book SET hidden_at=1 WHERE content_hash=?1", [first.as_str()]).unwrap();
    assert_eq!(browse_folder(&db, ROOT_DIR_ID, "", &[]).children.iter().find(|c| c.id == shelf.to_string()).unwrap().book_count, 1);
}

#[test]
fn folder_projection_batches_changes_rolls_back_failed_refresh_and_reuses_clean_state() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let hidden = db.create_directory(&shelf, &".Hidden".into()).unwrap().id;
    let other = db.create_directory(&ROOT_DIR_ID, &"Other".into()).unwrap().id;
    let hash = import_in_folder(&db, 201, hidden);
    browse_folder(&db, ROOT_DIR_ID, "", &[]);
    let rows_before: i64 = db.connection.query_row("SELECT count(*) FROM folder_book_ownership", [], |r| r.get(0)).unwrap();
    db.connection.execute_batch("CREATE TEMP TRIGGER reject_folder_refresh BEFORE INSERT ON folder_book_ownership BEGIN SELECT RAISE(ABORT,'refresh failure'); END;").unwrap();
    // Clean reads perform no projection writes.
    browse_folder(&db, ROOT_DIR_ID, "", &[]);
    for downloaded in [0, 1, 0] {
        db.connection.execute("UPDATE book_dir SET is_downloaded=?1", [downloaded]).unwrap();
    }
    assert_eq!(db.connection.query_row("SELECT count(*) FROM folder_browse_dirty_book", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let failed = db.browse_library_folder(&ROOT_DIR_ID, false, library_model::LibraryFileTypeFilter::All).err().unwrap();
    assert!(failed.to_string().contains("refresh failure"));
    assert_eq!(db.connection.query_row("SELECT count(*) FROM folder_book_ownership", [], |r| r.get::<_, i64>(0)).unwrap(), rows_before);
    assert_eq!(db.connection.query_row("SELECT count(*) FROM folder_browse_dirty_book", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    db.connection.execute_batch("DROP TRIGGER reject_folder_refresh;").unwrap();
    browse_folder(&db, ROOT_DIR_ID, "", &[]);
    assert_eq!(db.connection.query_row("SELECT count(*) FROM folder_browse_dirty_book", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
    db.connection.execute("UPDATE dir SET name='Visible',parent_id=?1 WHERE id=?2", [other.to_string(), hidden.to_string()]).unwrap();
    db.connection.execute("UPDATE sync_metadata SET change_origin='local'", []).unwrap();
    let view = browse_folder(&db, ROOT_DIR_ID, "", &[]);
    assert_eq!(view.children.iter().find(|c| c.id == shelf.to_string()).unwrap().book_count, 0);
    assert_eq!(view.children.iter().find(|c| c.id == other.to_string()).unwrap().book_count, 1);
    db.connection.execute("UPDATE book_dir SET deleted_at=1 WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", [hash.as_str()]).unwrap();
    assert_eq!(browse_folder(&db, ROOT_DIR_ID, "", &[]).children.iter().find(|c| c.id == other.to_string()).unwrap().book_count, 0);
    db.connection.execute("UPDATE book_dir SET deleted_at=NULL", []).unwrap();
    db.connection.execute("UPDATE dir SET deleted_at=1 WHERE id=?1", [other.to_string()]).unwrap();
    assert!(browse_folder(&db, ROOT_DIR_ID, "Title", &[]).books.is_empty(), "deleted ancestors block membership");
    assert_eq!(browse_folder(&db, hidden, "", &[]).books.len(), 1, "a live detached subtree retains its own membership");
}


#[test]
fn grouped_folder_chips_use_visible_membership_and_normalized_languages() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let child = db.create_directory(&shelf, &"Child".into()).unwrap().id;
    let empty = db.create_directory(&shelf, &"Empty".into()).unwrap().id;
    let hidden = db.create_directory(&child, &".Hidden".into()).unwrap().id;
    import_in_folder(&db, 301, child);
    import_in_folder(&db, 302, hidden);
    db.connection.execute("INSERT INTO book_language(book_row_id,position,language_tag) SELECT row_id,0,'en_US' FROM book", []).unwrap();
    let mut query = subject_query(&ROOT_DIR_ID.to_string());
    let unfiltered = db.browse_folder(&query, false).unwrap();
    assert_eq!(unfiltered.chip_groups.len(), 1);
    assert_eq!(unfiltered.contents.children.iter().map(|row| row.id.clone()).collect::<Vec<_>>(), [child.to_string(), empty.to_string()]);
    let downloaded = db.browse_folder(&query, true).unwrap();
    assert_eq!(downloaded.contents.children.len(), 1);
    assert_eq!(downloaded.contents.children[0].id, child.to_string());
    query.languages = vec!["en".into()];
    let page = db.browse_folder(&query, false).unwrap();
    let (contents, groups) = (page.contents, page.chip_groups);
    assert_eq!(groups.len(), 1);
    assert_eq!(contents.children.len(), 1);
    assert_eq!(contents.children[0].id, child.to_string());
    assert_eq!(contents.children[0].book_count, 1);
    query.languages = vec!["sv".into()];
    let no_matches = db.browse_folder(&query, false).unwrap();
    assert!(no_matches.contents.children.is_empty());
    assert!(no_matches.chip_groups.is_empty());
}

#[test]
fn downloaded_folder_sorting_uses_scoped_counts_and_normalized_names() {
    let db = database();
    let mut index = 9000;
    for (name, downloads, remote) in [("Écology", 1, 0), ("Zoology", 1, 2), ("Most", 2, 0), ("Remote", 0, 1), ("Empty", 0, 0)] {
        let directory = db.create_directory(&ROOT_DIR_ID, &name.into()).unwrap().id;
        for book in 0..downloads + remote {
            index += 1;
            let hash = import_in_folder(&db, index, directory);
            db.connection.execute("UPDATE book_dir SET is_downloaded=?1 WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?2)", library_database::rusqlite::params![book < downloads, hash.as_str()]).unwrap();
        }
    }
    db.connection.execute("INSERT INTO book_language(book_row_id,position,language_tag) SELECT row_id,0,'en-US' FROM book", []).unwrap();
    for filtered in [false, true] {
        for search in ["", "ology"] {
            for sort in [library_model::BrowseChipSort::Alphabetical, library_model::BrowseChipSort::BookCount] {
                let mut query = subject_query(&ROOT_DIR_ID.to_string());
                query.search = search.into();
                query.chip_sort = sort;
                if filtered {
                    query.languages = vec!["en".into()];
                    query.file_types = vec![library_model::LibraryFileTypeFilter::Book];
                }
                let contents = db.browse_folder_contents(&query, true).unwrap();
                let expected = if !search.is_empty() {
                    vec![("Écology", 1), ("Zoology", 1)]
                } else if sort == library_model::BrowseChipSort::BookCount {
                    vec![("Most", 2), ("Écology", 1), ("Zoology", 1)]
                } else {
                    vec![("Écology", 1), ("Most", 2), ("Zoology", 1)]
                };
                assert_eq!(contents.children.iter().map(|row| (row.name.as_str(), row.book_count)).collect::<Vec<_>>(), expected);
                assert!(contents.children.iter().all(|row| row.book_count == row.downloaded_book_count));
            }
        }
    }
    let picker = db.browse_library_folder(&ROOT_DIR_ID, true, library_model::LibraryFileTypeFilter::All).unwrap();
    assert_eq!(picker.children.len(), 3);
    let zoology = picker.children.iter().find(|row| row.name == "Zoology").unwrap();
    assert_eq!((zoology.book_count, zoology.downloaded_book_count), (3, 1));
}

#[test]
fn subject_cards_and_facets_share_normalized_language_filtering() {
    let (db, path, _) = cached_subject_fixture();
    db.connection.execute("INSERT INTO book_language(book_row_id,position,language_tag) SELECT row_id,0,' EN_us ' FROM book", []).unwrap();
    db.connection.execute("INSERT INTO book_language(book_row_id,position,language_tag) SELECT row_id,1,'en-GB' FROM book", []).unwrap();
    let root = db.browse_subject_contents(&subject_query("Subject"), false).unwrap();
    let mut query = subject_query(&root.children[0].id);
    query.languages = vec!["en".into()];
    assert_eq!(db.browse_subject_contents(&query, false).unwrap().books.len(), 4);
    let (languages, formats) = db.browse_subject_facet_counts(&path, "", &[], &["en"], false).unwrap();
    assert_eq!(languages.iter().find(|l| l.language == "en").unwrap().book_count, 4);
    assert_eq!(formats.iter().map(|f| f.book_count).sum::<u32>(), 4);
}

#[test]
fn browse_progress_is_derived_without_rewriting_sync_registers_and_audio_is_book_scoped() {
    let db = database();
    let hash = import_in_folder(&db, 401, ROOT_DIR_ID);
    // Browse progress comes from the dedicated register, independent of the location.
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
    for (position, progress, expected) in [("0.25", 0., 0.), ("35", 0., 0.), ("chapter.xhtml", 0., 0.), ("0.25", 70., 70.), ("101", 0., 0.), (" 0.5suffix", 0., 0.)] {
        db.connection.execute("UPDATE book SET read_pos=?1,read_progress=?2 WHERE content_hash=?3", library_database::rusqlite::params![position, progress, hash.as_str()]).unwrap();
        assert_eq!(db.browse_book_detail(hash).unwrap().book.progress, f64::from(expected) as f32);
        let stored: (String, f64) = db.connection.query_row("SELECT read_pos,read_progress FROM book WHERE content_hash=?1", [hash.as_str()], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(stored, (position.to_owned(), progress));
    }
    db.connection.execute("INSERT INTO audiobook_metadata(content_hash,duration_ms) VALUES(?1,12345)", [hash.as_str()]).unwrap();
    db.connection.execute("UPDATE book_toc SET entry_count=7 WHERE content_hash=?1", [hash.as_str()]).unwrap();
    let folder = db.create_directory(&ROOT_DIR_ID, &"Copy".into()).unwrap().id;
    place_book(&db, hash, folder, false);
    let card = db.browse_book_detail(hash).unwrap().book;
    assert_eq!(card.audiobook_duration_ms, Some(12345));
    assert_eq!(card.audiobook_chapter_count, Some(7));
    db.connection.execute("DELETE FROM audiobook_metadata WHERE content_hash=?1", [hash.as_str()]).unwrap();
    let card = db.browse_book_detail(hash).unwrap().book;
    assert_eq!(card.audiobook_duration_ms, None);
    assert_eq!(card.audiobook_chapter_count, None);
}

fn without_navigation(mutations: Vec<library_replica::StateMutation>) -> Vec<Vec<u8>> {
    // Migration reissues transport identities; the domain payloads must survive.
    let mut values=mutations.into_iter().filter(|m|!matches!(m.body,library_replica::MutationBody::BookToc {..})).map(|m|sync_common::wire::encode(&m.body).unwrap()).collect::<Vec<_>>();
    values.sort();values
}

fn navigation_documents(mutations: &[library_replica::StateMutation]) -> Vec<(String, book_model::BookTocDocument)> {
    let mut documents = mutations
        .iter()
        .filter_map(|mutation| match &mutation.body {
            library_replica::MutationBody::BookToc { content_hash, value } => Some((content_hash.to_string(), value.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    documents.sort_by(|left, right| left.0.cmp(&right.0));
    documents
}


#[test]
fn author_subject_chips_match_previous_ranking_with_multiple_routes_and_duplicate_credits() {
    let (db, _, _) = cached_subject_fixture();
    // Include several multiply-routed concepts, not merely a single leaf.
    let concepts = db
        .connection
        .prepare("SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 GROUP BY concept_id HAVING count(DISTINCT parent_route_id)>1 ORDER BY concept_id LIMIT 5")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(concepts.len(), 5);
    for (index, concept) in concepts.iter().enumerate() {
        db.connection.execute("INSERT OR IGNORE INTO book_unified_concept SELECT row_id,?1,1 FROM book WHERE row_id%3<=?2", library_database::rusqlite::params![concept, index as i64 % 3]).unwrap();
    }
    // Two credits for the same identity must still count one book.
    db.connection.execute("INSERT INTO book_contributor(book_row_id,position,author_identity_id,name,role) SELECT book_row_id,10,author_identity_id,name,role FROM book_contributor WHERE position=0", []).unwrap();
    db.connection.execute("UPDATE book_dir SET is_downloaded=(book_row_id%2)", []).unwrap();
    let sql = include_str!("fixtures/author_subjects_v60.sql");
    for downloaded in [false, true] {
        let mut expected = std::collections::HashMap::<String, Vec<library_model::AuthorSubject>>::new();
        let mut statement = db.connection.prepare(sql).unwrap();
        let mut rows = statement.query(library_database::rusqlite::named_params! {":downloaded_only": downloaded, ":per_author": 3}).unwrap();
        while let Some(row) = rows.next().unwrap() {
            expected.entry(row.get(0).unwrap()).or_default().push(library_model::AuthorSubject::new(row.get(1).unwrap(), row.get(2).unwrap()));
        }
        let authors = db.browse_authors(Default::default(), downloaded).unwrap();
        assert!(!authors.authors.is_empty());
        for author in authors.authors {
            assert_eq!(author.subjects, expected.remove(&author.id.to_string()).unwrap_or_default());
        }
        assert!(expected.is_empty());
    }
}

#[test]
fn stored_search_text_tracks_titles_subtitles_and_contributor_edits() {
    let db = database();
    let first = import_in_folder(&db, 601, ROOT_DIR_ID);
    let second = import_in_folder(&db, 602, ROOT_DIR_ID);
    db.connection.execute("UPDATE book SET title='École',subtitle='Über les livres' WHERE content_hash=?1", [first.as_str()]).unwrap();
    assert_eq!(db.browse_library_search("  ECOLE:   uber ", false).unwrap().title_matches[0].content_hash, first);
    assert!(db.browse_library_search("ecole uber", false).unwrap().title_matches.is_empty(), "punctuation remains significant");
    db.connection.execute("UPDATE book_contributor SET name='Émile' WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", [first.as_str()]).unwrap();
    db.connection.execute("INSERT INTO book_contributor(book_row_id,position,author_identity_id,name,role) SELECT book_row_id,5,author_identity_id,'Björk',role FROM book_contributor WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1) AND position=0",[first.as_str()]).unwrap();
    assert_eq!(db.browse_book_detail(first).unwrap().book.author, "Émile, Björk");
    assert_eq!(db.browse_library_search("bjork", false).unwrap().author_matches[0].content_hash, first);
    db.connection.execute("UPDATE book_contributor SET position=10 WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1) AND position=0", [first.as_str()]).unwrap();
    assert_eq!(db.browse_book_detail(first).unwrap().book.author, "Björk, Émile");
    // A remote reassignment invalidates both the old and new book's text.
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
    db.connection
        .execute("UPDATE book_contributor SET book_row_id=(SELECT row_id FROM book WHERE content_hash=?1) WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?2) AND position=5", [second.as_str(), first.as_str()])
        .unwrap();
    db.connection.execute("UPDATE sync_metadata SET change_origin='local'", []).unwrap();
    let matches = db.browse_library_search("bjork", false).unwrap().author_matches;
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].content_hash, second);
    assert_eq!(db.browse_book_detail(first).unwrap().book.author, "Émile");
    db.connection.execute("UPDATE book_contributor SET role=X'74726c' WHERE position=5", []).unwrap();
    assert!(db.browse_library_search("bjork", false).unwrap().author_matches.is_empty());
    db.connection.execute("DELETE FROM book_contributor WHERE book_row_id=(SELECT row_id FROM book WHERE content_hash=?1)", [first.as_str()]).unwrap();
    assert_eq!(db.browse_book_detail(first).unwrap().book.author, "");
    assert!(db.browse_library_search("emile", false).unwrap().author_matches.is_empty());
    let tx = db.connection.unchecked_transaction().unwrap();
    tx.execute("UPDATE book SET title='Rolled back',subtitle=NULL WHERE content_hash=?1", [first.as_str()]).unwrap();
    tx.rollback().unwrap();
    assert_eq!(db.browse_library_search("uber", false).unwrap().title_matches[0].content_hash, first);
    assert!(db.browse_library_search("rolled back", false).unwrap().title_matches.is_empty());
}

#[test]
fn library_search_reads_stored_keys_without_normalizing_metadata_again() {
    let db = database();
    let hash = import_in_folder(&db, 701, ROOT_DIR_ID);
    // Queries still normalize the user's input in Rust. No database scalar call
    // should be needed to search/sort title and author metadata after its write.
    db.connection
        .create_scalar_function("normalize_search_text", 1, library_database::rusqlite::functions::FunctionFlags::SQLITE_UTF8, |_| -> library_database::rusqlite::Result<String> { Err(library_database::rusqlite::Error::InvalidQuery) })
        .unwrap();
    assert_eq!(db.browse_library_search("title", false).unwrap().title_matches[0].content_hash, hash);
    assert_eq!(db.browse_library_search("original author", false).unwrap().author_matches[0].content_hash, hash);
}


#[test]
fn folder_count_queries_do_not_scan_unrelated_library_books() {
    use library_database::rusqlite::{StatementStatus, named_params};
    let db = database();
    let small = db.create_directory(&ROOT_DIR_ID, &"Small".into()).unwrap().id;
    let child = db.create_directory(&small, &"Child".into()).unwrap().id;
    let big = db.create_directory(&ROOT_DIR_ID, &"Big".into()).unwrap().id;
    import_in_folder(&db, 901, child);
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
    let tx = db.connection.unchecked_transaction().unwrap();
    tx.execute_batch("WITH RECURSIVE numbers(n) AS (VALUES(1000) UNION ALL SELECT n+1 FROM numbers WHERE n<2999) INSERT INTO book(content_hash,title,format) SELECT printf('%064x',n),'Other book','epub' FROM numbers;").unwrap();
    tx.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) SELECT ?1,row_id,content_hash || '.epub','',1 FROM book WHERE title='Other book'", [big.to_string()]).unwrap();
    tx.execute(
        "WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<1000)
        INSERT INTO dir(id,parent_id,name) SELECT printf('10000000-0000-0000-0000-%012x',n),?1,printf('Unrelated %d',n) FROM numbers",
        [big.to_string()],
    )
    .unwrap();
    tx.commit().unwrap();
    browse_folder(&db, ROOT_DIR_ID, "", &[]);
    let source = include_str!("../src/browse/sql/folders.sql");
    let query = |name: &str| source.split(&format!("-- name: {name}?\n")).nth(1).unwrap().split("-- name:").next().unwrap();
    let directory = small.to_string();
    let directories = serde_json::to_string(&[directory.clone()]).unwrap();
    let mut direct = db.connection.prepare(query("get_immediate_sub_dirs")).unwrap();
    let count: i64 = direct.query_row(named_params! {":dir_id":directory,":downloaded_only":false,":chip_sort":0}, |r| r.get(2)).unwrap();
    assert_eq!(count, 1);
    assert!(direct.get_status(StatementStatus::FullscanStep) < 100, "small-folder counts must not scan 2,000 unrelated books");
    db.connection.execute("INSERT OR IGNORE INTO book_language(book_row_id,position,language_tag) SELECT row_id,0,'en-US' FROM book WHERE title!='Other book'", []).unwrap();
    let mut filtered = db.connection.prepare(query("get_filtered_immediate_sub_dirs")).unwrap();
    let mut previous = db.connection.prepare(query("get_sub_dirs")).unwrap();
    let read_folder = |r: &library_database::rusqlite::Row<'_>| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<String>>(4)?));
    for sort in [0, 1] {
        for (formats, languages) in [(1, ""), (2, ""), (3, ""), (0, ",en,"), (1, ",en,"), (1, ",sv,")] {
            filtered.reset_status(StatementStatus::FullscanStep);
            let actual = filtered.query_map(named_params! {":dir_id":directory,":downloaded_only":false,":file_types":formats,":chip_sort":sort,":languages":languages}, read_folder).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            let expected = previous
                .query_map(named_params! {":dir_id":directory,":downloaded_only":false,":normalized_query":"",":file_types":formats,":chip_sort":sort,":languages":languages}, read_folder)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(actual, expected);
            assert!(filtered.get_status(StatementStatus::FullscanStep) < 100, "metadata filters must not scan unrelated books or expand unrelated directory paths");
        }
    }
    let mut grouped = db.connection.prepare(query("group_folder_children_with")).unwrap();
    for (formats, languages, expected) in [(0, "", vec![1]), (1, ",en,", vec![1]), (2, "", vec![])] {
        db.connection.execute("INSERT OR IGNORE INTO book_language(book_row_id,position,language_tag) SELECT row_id,0,'en-US' FROM book WHERE title!='Other book'", []).unwrap();
        grouped.reset_status(StatementStatus::FullscanStep);
        let counts =
            grouped.query_map(named_params! {":directory_ids":directories,":file_types":formats,":languages":languages,":chip_sort":0,":downloaded_only":false}, |r| r.get::<_, i64>(3)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(counts, expected);
        assert!(grouped.get_status(StatementStatus::FullscanStep) < 100, "grouped counts must not scan unrelated books");
    }
}


#[test]
fn dirty_folder_refresh_does_not_scan_unrelated_placements() {
    use library_database::rusqlite::StatementStatus;
    let db = database();
    let target = db.create_directory(&ROOT_DIR_ID, &"Target".into()).unwrap().id;
    let hash = import_in_folder(&db, 901, target);
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
    db.connection
        .execute_batch(
            "WITH RECURSIVE numbers(n) AS (VALUES(1000) UNION ALL SELECT n+1 FROM numbers WHERE n<2999)
        INSERT INTO book(content_hash,title,format) SELECT printf('%064x',n),'Other book','epub' FROM numbers;",
        )
        .unwrap();
    db.connection
        .execute(
            "INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded)
        SELECT ?1,row_id,content_hash || '.epub','',1 FROM book WHERE title='Other book'",
            [ROOT_DIR_ID.to_string()],
        )
        .unwrap();
    browse_folder(&db, ROOT_DIR_ID, "", &[]);
    let book: i64 = db.connection.query_row("SELECT row_id FROM book WHERE content_hash=?1", [hash.as_str()], |r| r.get(0)).unwrap();
    db.connection.execute("UPDATE book_dir SET is_downloaded=0 WHERE book_row_id=?1", [book]).unwrap();
    assert_eq!(db.connection.query_row("SELECT count(*) FROM folder_browse_dirty_book", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let query = include_str!("../src/browse/sql/folder_cache.sql").split("-- name: folder_cache_placements?\n").nth(1).unwrap().split("-- name:").next().unwrap();
    let mut statement = db.connection.prepare(query).unwrap();
    let placements = statement.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(placements, vec![(book, target.to_string(), false)]);
    assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0, "one dirty book must use indexed placement lookups, regardless of unrelated books");
    // Exercise the actual refresh too: the changed download state must reach
    // the cached membership while the book remains visible in its folder.
    assert_eq!(browse_folder(&db, target, "", &[]).books.len(), 1);
    assert_eq!(
        db.connection.query_row("SELECT downloaded_placement_count FROM folder_book_ownership WHERE folder_id=?1 AND book_row_id=?2", library_database::rusqlite::params![target.to_string(), book], |r| r.get::<_, i64>(0)).unwrap(),
        0
    );
}

#[test]
fn subject_format_facets_match_previous_query_after_membership_changes() {
    use library_database::rusqlite::named_params;
    let (db, path, parent) = cached_subject_fixture();
    db.connection.execute("UPDATE book SET format=CASE row_id%3 WHEN 0 THEN 'm4b' WHEN 1 THEN 'epub' ELSE 'pdf' END", []).unwrap();
    db.connection.execute("UPDATE book_dir SET is_downloaded=book_row_id%2", []).unwrap();
    db.connection.execute("INSERT INTO book_language SELECT row_id,0,CASE row_id%2 WHEN 0 THEN 'sv' ELSE 'en-US' END FROM book", []).unwrap();
    db.connection.execute("INSERT INTO book_language SELECT row_id,1,'en-GB' FROM book WHERE row_id%2=1", []).unwrap();
    let extra = db.create_directory(&ROOT_DIR_ID, &"Extra".into()).unwrap().id;
    // Duplicate placements and overlapping ancestor assignments still count once.
    db.connection.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) SELECT ?1,row_id,'extra.epub','',1 FROM book WHERE row_id=1", [extra.to_string()]).unwrap();
    db.connection.execute("INSERT OR IGNORE INTO book_unified_concept SELECT 1,concept_id,1 FROM curated.unified_concept_paths WHERE path=?1 AND concept_id>0", [&parent]).unwrap();
    let source = include_str!("../src/browse/sql/subjects.sql");
    let combined = source.split("-- name: get_subject_facets?\n").nth(1).unwrap().split("-- name:").next().unwrap().trim().trim_end_matches(';');
    // The SQL section may include comments before the next named statement.
    let combined = combined.split_once(";\n").map_or(combined, |(statement, _)| statement);
    let sql = format!("SELECT value,book_count FROM ({combined}) WHERE kind=0");
    let language_sql = format!("SELECT value,book_count FROM ({combined}) WHERE kind=1");
    let label = book_model::normalize_search_text(path.rsplit(" / ").next().unwrap());
    for state in 0..4 {
        match state {
            1 => {
                db.connection.execute("UPDATE book SET hidden_at=1 WHERE row_id=1", []).unwrap();
            }
            2 => {
                db.connection.execute("UPDATE book SET hidden_at=NULL", []).unwrap();
                db.connection.execute("UPDATE book SET deleted_at=1 WHERE row_id=2", []).unwrap();
                db.connection.execute("UPDATE book_dir SET deleted_at=1 WHERE book_row_id=3", []).unwrap();
            }
            3 => {
                db.connection.execute("UPDATE book SET deleted_at=NULL", []).unwrap();
                db.connection.execute("UPDATE book_dir SET deleted_at=NULL", []).unwrap();
            }
            _ => {}
        }
        // Refresh through the public API before reading the cache directly.
        db.browse_subject_facet_counts(&parent, "", &[], &[], false).unwrap();
        let route: i64 = db.connection.query_row("SELECT route_id FROM subject_route_paths WHERE path=?1", [&parent], |r| r.get(0)).unwrap();
        let mut actual = db.connection.prepare(&sql).unwrap();
        let mut previous = db.connection.prepare(include_str!("fixtures/subject_format_facets_before.sql")).unwrap();
        let mut language_actual = db.connection.prepare(&language_sql).unwrap();
        let mut language_previous = db.connection.prepare(include_str!("fixtures/subject_language_facets_before.sql")).unwrap();
        for route in [route, 0, -1] {
            for search in ["", "book 1", "original author", label.as_str(), "no matching book qzx"] {
                // The synthetic root exercises counting; descendant matching is
                // checked under the real parent without walking the entire taxonomy.
                if route == 0 && !search.is_empty() {
                    continue;
                }
                for formats in [0, 1, 2, 4, 3] {
                    for downloaded in [false, true] {
                        let params = named_params! {":route_id":route, ":normalized_query":search, ":file_types":formats, ":downloaded_only":downloaded};
                        let read = |r: &library_database::rusqlite::Row<'_>| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?));
                        let got = language_actual.query_map(params, read).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
                        let expected = language_previous.query_map(params, read).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
                        assert_eq!(got, expected, "language facets: state={state}, route={route}, search={search}, formats={formats}, downloaded={downloaded}");
                    }
                }
                for languages in ["", ",en,", ",sv,"] {
                    for downloaded in [false, true] {
                        let params = named_params! {":route_id":route, ":normalized_query":search, ":languages":languages, ":downloaded_only":downloaded};
                        let read = |r: &library_database::rusqlite::Row<'_>| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?));
                        let got = actual.query_map(params, read).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
                        let expected = previous.query_map(params, read).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
                        assert_eq!(got, expected, "state={state}, route={route}, search={search}, languages={languages}, downloaded={downloaded}");
                    }
                }
            }
        }
    }
}


#[test]
fn deleted_book_thumbnail_work_is_cancelled_and_restored_when_needed() {
    let db = database();
    let book = import();
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    db.connection.execute("INSERT INTO local_thumbnail_work(content_hash,state) VALUES(?1,'pending')", [hash.as_str()]).unwrap();
    assert_eq!(db.pending_thumbnail_job_count().unwrap(), 1);
    db.connection.execute("UPDATE book SET deleted_at=1 WHERE content_hash=?1", [hash.as_str()]).unwrap();
    assert_eq!(db.pending_thumbnail_job_count().unwrap(), 0);
    assert_eq!(db.connection.query_row("SELECT count(*) FROM local_thumbnail_work", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    // A late worker completion must not resurrect a deleted book's pending job.
    db.connection.execute("INSERT INTO local_thumbnail_work(content_hash,state) VALUES(?1,'pending')", [hash.as_str()]).unwrap();
    assert_eq!(db.pending_thumbnail_job_count().unwrap(), 0);
    db.connection.execute("UPDATE book SET deleted_at=NULL WHERE content_hash=?1", [hash.as_str()]).unwrap();
    assert_eq!(db.pending_thumbnail_job_count().unwrap(), 1);
}



#[cfg(feature = "scanner")]
#[test]
fn scan_file_failures_finish_and_remain_visible_until_next_scan() {
    use library_database::{FilesystemScan, InspectedFilesystemScan, OperationActivity};
    use library_replica::LibraryOperationState;
    let db = database();
    let mut snapshot = db.scan_snapshot().unwrap();
    let discovery = FilesystemScan { readable: true, ..Default::default() };
    let failures = vec![("Broken.epub".to_owned(), "Invalid ZIP header".to_owned())];
    // Even a batch containing only failed files must persist their diagnostics.
    db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { failures: failures.clone(), readable: false, books: vec![] }, false).unwrap();
    db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { readable: false, ..Default::default() }, true).unwrap();
    assert!(!db.needs_scan().unwrap());
    for _ in 0..2 {
        let status = db.operation_status(OperationActivity::default()).unwrap().operation.unwrap();
        assert_eq!(status.state, LibraryOperationState::Completed);
        assert!(status.waiting_reason.is_none());
        assert_eq!(status.scan_failures, failures);
        assert_eq!(status.failures, 1);
    }
    let snapshot = db.scan_snapshot().unwrap();
    db.apply_filesystem_scan(&snapshot, &discovery, &InspectedFilesystemScan { readable: true, ..Default::default() }).unwrap();
    assert!(db.operation_status(OperationActivity::default()).unwrap().operation.unwrap().scan_failures.is_empty());
}


#[test]
fn enriched_subjects_survive_later_page_evidence_and_reload() {
    let db = database();
    let input = import();
    let hash = input.content_hash;
    db.commit_import(input).unwrap();
    db.apply_rich_enrichment(library_database::RichEnrichmentOutcome {
        content_hash: hash,
        provider: "test".into(),
        isbn: "9780061804038".into(),
        matched: None,
        subjects: vec![book_model::BookSubject::new(None, "Business", "openlibrary:metadata:work", Some("lcc".into()), Some("HD2796".into())).unwrap()],
        status: "updated".into(),
        detail: None,
        replace_previous_work_classifications: false,
    })
    .unwrap();
    let mut evidence = book_model::BookMetadata::default();
    evidence.identifiers.push(book_model::Identifier::new("9780140441451", book_model::Scheme::Isbn, book_model::Scope::Edition).unwrap());
    assert!(db.merge_evidence_pages(library_database::MergeEvidenceOutcome { content_hash: hash, evidence }).unwrap());
    // A fresh file inspection of the same content must preserve enrichment too.
    db.commit_import(import()).unwrap();
    let refreshed = db.refresh_subject_enrichment(vec![hash], Default::default()).unwrap();
    assert_eq!(refreshed[0].book.subjects[0].code(), Some("HD2796"));
    assert_eq!(refreshed[0].book.identifiers[0].value(), "9780140441451");
    assert_eq!(db.connection.query_row("SELECT count(*) FROM book_subject WHERE code='HD2796'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    db.apply_rich_enrichment(library_database::RichEnrichmentOutcome {
        content_hash: hash,
        provider: "test".into(),
        isbn: "9780061804038".into(),
        matched: None,
        subjects: vec![],
        status: "updated".into(),
        detail: None,
        replace_previous_work_classifications: true,
    })
    .unwrap();
    assert!(db.refresh_subject_enrichment(vec![hash], Default::default()).unwrap()[0].book.subjects.is_empty());
    assert_eq!(db.connection.query_row("SELECT count(*) FROM book_subject", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn subject_checkpoints_respect_backoff_restart_exhaustion_and_revision() {
    let db = database();
    let input = import();
    let hash = input.content_hash;
    db.commit_import(input).unwrap();
    let expected = db.connection.query_row("SELECT book_metadata FROM book", [], |r| r.get::<_, Vec<u8>>(0)).unwrap();
    let scope = serde_json::json!([hash.as_str()]).to_string();
    db.save_subject_checkpoint(library_database::SubjectCheckpointOutcome {
        content_hash: hash,
        phase: book_enrichment::SubjectPhase::Exhausted,
        expected,
        title: "Original".into(),
        complete: true,
        retry_seconds: 2592000,
        empty_metadata: false,
    })
    .unwrap();
    assert!(db.subject_enrichment_candidates("", &scope).unwrap().is_empty());
    db.connection.execute("UPDATE local_subject_pipeline SET retry_at=unixepoch()-1", []).unwrap();
    let mut jobs = db.subject_enrichment_candidates("", &scope).unwrap();
    assert_eq!(jobs.len(), 1);
    let book = jobs[0].book.clone();
    assert_eq!(jobs[0].pipeline.next_step(&book), book_enrichment::SubjectStep::InspectPages);
    db.connection.execute("UPDATE local_subject_pipeline SET complete=0,retry_at=unixepoch()+60", []).unwrap();
    assert!(db.subject_enrichment_candidates("", &scope).unwrap().is_empty());
    db.connection.execute("UPDATE local_subject_pipeline SET revision=3", []).unwrap();
    let mut jobs = db.subject_enrichment_candidates("", &scope).unwrap();
    assert_eq!(jobs.len(), 1);
    let book = jobs[0].book.clone();
    assert_eq!(jobs[0].pipeline.next_step(&book), book_enrichment::SubjectStep::InspectPages);
}


#[test]
fn rescan_recovers_legacy_projection_only_subjects() {
    let db = database();
    let input = import();
    let hash = input.content_hash;
    db.commit_import(input).unwrap();
    db.connection.execute("INSERT INTO book_subject(book_row_id,position,name,source,authority,code) SELECT row_id,0,'History','metadata:work','lcc','DC38' FROM book", []).unwrap();
    db.commit_import(import()).unwrap();
    let refreshed = db.refresh_subject_enrichment(vec![hash], Default::default()).unwrap();
    assert_eq!(refreshed[0].book.subjects[0].code(), Some("DC38"));
    assert_eq!(db.connection.query_row("SELECT count(*) FROM book_subject_assignment", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}
