use crate::query_methods::query_rows;
use library_database::{Database, ImportCommit, NativeDirectoryAcknowledgement, NativeDirectoryCompletion};
use library_model::PublishedStagedBook;
use sync_common::{BookId, DirId, ROOT_DIR_ID, StateAddress};

fn database() -> Database {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();
    db
}

fn import() -> ImportCommit {
    let hash = BookId::new(&"9".repeat(64));
    ImportCommit {
        parent_id: ROOT_DIR_ID,
        file_name: "book.epub".into(),
        format: book_model::BookFormat::Epub,
        content_hash: hash,
        size_bytes: 10,
        inspection_version: 1,
        inspection: Some(book_inspection::InspectedBook {
            pdf: None,
            audiobook: None,
            metadata: book_model::BookRecord {
                metadata: book_model::BookDetails { title: "Original".into(), subtitle: None, contributors: vec![book_model::Contributor::new("Original Author", book_model::MarcRelatorCode(*b"aut")).unwrap()], book: Default::default() },
                description: String::new(),
            },
            toc: vec![book_model::BookTocEntry { title: "Chapter".into(), target: "ch1".into(), children: vec![] }],
        }),
        published: PublishedStagedBook { name: "book.epub".into(), relative_path: "/book.epub".into(), published: false, fingerprint: None },
        request_thumbnail: false,
        restore_paths: vec![],
    }
}

#[test]
fn browse_book_detail_projects_only_authors_and_narrators() {
    use book_model::{AUTHOR_MARC_RELATOR_CODE, Contributor, MarcRelatorCode, NARRATOR_MARC_RELATOR_CODE};

    let db = database();
    let mut book = import();
    let hash = book.content_hash;
    book.inspection.as_mut().unwrap().metadata.contributors = vec![
        Contributor::new("Author", AUTHOR_MARC_RELATOR_CODE).unwrap(),
        Contributor::new("Translator", MarcRelatorCode(*b"trl")).unwrap(),
        Contributor::new("First narrator", NARRATOR_MARC_RELATOR_CODE).unwrap(),
        Contributor::new("Editor", MarcRelatorCode(*b"edt")).unwrap(),
        Contributor::new("Second narrator", NARRATOR_MARC_RELATOR_CODE).unwrap(),
    ];
    db.commit_import(book).unwrap();

    let detail = db.browse_book_detail(&hash).unwrap();
    assert_eq!(detail.authors.iter().map(|author| author.name.as_str()).collect::<Vec<_>>(), ["Author"]);
    assert_eq!(detail.credits.iter().map(|credit| (credit.name.as_str(), credit.role.as_str())).collect::<Vec<_>>(), [("First narrator", "nrt"), ("Second narrator", "nrt")]);
}

#[test]
fn old_library_schemas_are_refused_without_modification() {
    for version in [8, 9, 10, 11, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35] {
        let db = database();
        db.connection.pragma_update(None, "user_version", version).unwrap();
        assert!(db.initialize_library().unwrap_err().to_string().contains("unsupported library schema version"));
        let retained: i64 = db.connection.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
        assert_eq!(retained, version);
    }
}

#[test]
fn reimport_does_not_replace_immutable_file_metadata() {
    let db = database();
    db.commit_import(import()).unwrap();
    db.clear_sync_outbox().unwrap();
    let mut edited = import();
    edited.inspection.as_mut().unwrap().metadata.title = "Edited".into();
    db.commit_import(edited).unwrap();
    assert!(db.sync_publishable_mutations().unwrap().is_empty());
    assert_eq!(db.browse_book_detail(&import().content_hash).unwrap().book.title, "Original");
}

#[test]
fn catalog_subject_overrides_survive_import_reprojection_reopen_and_sync() {
    use book_model::BookSubject;
    use library_replica::LibraryStateEntry;
    use subject_projection::{MANUAL_SUBJECT_EXCLUSION_SOURCE, UNIFIED_SYSTEM_ID};
    use sync_common::{Delivery, LibraryRevision, OpaqueStateEntry, PullStateResponse, SyncCursor, Versioned, transport};

    let path = std::env::temp_dir().join(format!("catalog-subject-persistence-{}.db", uuid::Uuid::new_v4()));
    let db = Database::open(&path).unwrap();
    db.initialize_library().unwrap();
    let mut book = import();
    let hash = book.content_hash;
    let embedded = &mut book.inspection.as_mut().unwrap().metadata;
    embedded.book.subjects = vec![BookSubject::new(None, "PZ3", "metadata:work", Some("lcc".into()), Some("PZ3".into())).unwrap()];
    let mut catalog = embedded.clone();
    catalog.book.subjects.extend([
        BookSubject::new(None, "Short Stories", "manual:subject", Some(UNIFIED_SYSTEM_ID.into()), Some("Literature / Genres / Literary Forms / Forms & Literary Traditions / Short Stories".into())).unwrap(),
        BookSubject::new(None, "English", MANUAL_SUBJECT_EXCLUSION_SOURCE, Some(UNIFIED_SYSTEM_ID.into()), Some("Literature / By Language / English".into())).unwrap(),
        BookSubject::new(None, "Language", MANUAL_SUBJECT_EXCLUSION_SOURCE, Some(UNIFIED_SYSTEM_ID.into()), Some("Language".into())).unwrap(),
    ]);
    // The shared import path supplies the hash-matched catalog answer atomically.
    db.commit_import_with_catalog_answer(book, None, Some(catalog.clone())).unwrap();

    let verify = |db: &Database| {
        let id: i64 = db.connection.fixtures_read_book_row_id(&hash, |row| row.get(0)).unwrap();
        let concepts = query_rows(|callback| db.connection.fixtures_subject_projection_concepts(id, callback), |row| row.get::<_, i64>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(concepts, [4124]);
        assert_eq!(db.enrichment_cover_record(&hash).unwrap().unwrap().book.subjects, catalog.book.subjects);
        assert!(db.enrichment_candidates(&[hash], i64::MAX).unwrap().is_empty());
    };
    verify(&db);
    let publication = db.sync_publishable_mutations().unwrap().into_iter().find(|publication| matches!(&publication.state.value, LibraryStateEntry::BookAdd { content_hash, .. } if *content_hash == hash)).unwrap();
    db.connection.fixtures_update_library_local_state_taxonomy_revision().unwrap();
    drop(db);
    let reopened = Database::open(&path).unwrap();
    reopened.initialize_library().unwrap();
    verify(&reopened);

    let receiving = database();
    receiving.commit_import(import()).unwrap();
    let delivered = Delivery { state: transport::representation::<_, Versioned<OpaqueStateEntry>>(&publication.state).unwrap(), revision: LibraryRevision::new(1).unwrap() };
    receiving.sync_commit_pull_response(&[], &PullStateResponse { mutations: vec![delivered], next_cursor: SyncCursor::default(), has_more: false }).unwrap();
    verify(&receiving);
    drop(reopened);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn reimport_with_catalog_answer_preserves_an_existing_definitive_answer() {
    let db = database();
    let hash = import().content_hash;
    let original = import().inspection.unwrap().metadata;
    db.commit_import_with_catalog_answer(import(), None, Some(original.clone())).unwrap();
    let mut changed = original.clone();
    changed.metadata.title = "Later catalog correction".into();
    db.commit_import_with_catalog_answer(import(), None, Some(changed)).unwrap();
    assert_eq!(db.enrichment_cover_record(&hash).unwrap().unwrap(), original);
    assert!(db.enrichment_candidates(&[hash], i64::MAX).unwrap().is_empty());
}

#[test]
fn failed_contributor_write_rolls_back_book_addition() {
    let db = database();
    db.commit_import(import()).unwrap();
    let queued_before = db.sync_publishable_mutations().unwrap();
    let identities_before: i64 = db.connection.fixtures_read_author_identity_count(|row| row.get(0)).unwrap();
    db.connection.fixtures_create_book_contributor().unwrap();
    let new_import = || {
        let mut next = import();
        next.content_hash = BookId::new(&"b".repeat(64));
        next.file_name = "new-book.epub".into();
        next.published.name = "new-book.epub".into();
        next.published.relative_path = "/new-book.epub".into();
        next.inspection.as_mut().unwrap().metadata.title = "New title".into();
        next.inspection.as_mut().unwrap().metadata.contributors = vec![book_model::Contributor::new("New author", book_model::MarcRelatorCode(*b"aut")).unwrap()];
        next
    };
    let error = db.commit_import(new_import()).unwrap_err();
    assert!(error.to_string().contains("injected contributor failure"), "unexpected import failure: {error}");
    assert_eq!(db.connection.fixtures_read_book_count(|r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(db.connection.fixtures_read_author_identity_count(|row| row.get::<_, i64>(0)).unwrap(), identities_before);
    assert_eq!(db.sync_publishable_mutations().unwrap(), queued_before);
    db.connection.fixtures_drop_TRIGGER_2().unwrap();
    db.commit_import(new_import()).unwrap();
    assert_eq!(db.browse_book_detail(&new_import().content_hash).unwrap().book.title, "New title");
}

fn recovery_payloads(db: &Database) -> Vec<(String, String, Vec<u8>)> {
    db.sync_publishable_mutations().unwrap();
    query_rows(|callback| db.connection.fixtures_read_sync_outbox_state_kind_state_subkey_body(callback), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap()
}

#[test]
fn recovery_republishes_the_complete_book_addition() {
    let db = database();
    let mut book = import();
    let hash = book.content_hash;
    book.inspection.as_mut().unwrap().pdf = Some(pdf_view_common::PdfReaderMetadata {
        version: 1,
        content_hash: hash.to_string(),
        pages: vec![pdf_view_common::PdfStoredPage { width: 100.0, height: 300.0, rotation: 0 }],
        skippable_pages: None,
        annotations: vec![],
        dependencies: None,
    });
    db.commit_import(book).unwrap();
    complete_no_match(&db, &hash);
    let cells = vec![StateAddress { kind: "book_add".into(), entity_key: hash.into(), entity_subkey: String::new() }];
    let expected = recovery_payloads(&db);
    assert_eq!(expected.len(), 1);
    db.clear_sync_outbox().unwrap();
    assert_eq!(db.sync_enqueue_missing_state_cells(&cells).unwrap(), 1);
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
    let missing = StateAddress { kind: "book_add".into(), entity_key: hash.into(), entity_subkey: "c".repeat(64) };
    assert_eq!(db.sync_enqueue_missing_state_cells(&[missing]).unwrap(), 0);
}

fn materialize(db: &Database, id: DirId, renamed_to: Option<&str>) {
    let snapshot = db.native_directory_snapshot(&id).unwrap().unwrap();
    assert!(snapshot.relative_path.starts_with('/'));
    assert_eq!(db.acknowledge_native_directory(NativeDirectoryCompletion { snapshot, renamed_to: renamed_to.map(str::to_owned), materialized: true }).unwrap(), NativeDirectoryAcknowledgement::Applied);
}

fn projection(db: &Database, id: DirId) -> String {
    db.connection.fixtures_read_local_file_projection_relative_path(&(id.to_string()), |r| r.get(0)).unwrap()
}

#[test]
fn directory_acknowledgement_rolls_back_when_projection_write_fails() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    materialize(&db, shelf, None);
    db.connection.fixtures_create_ON().unwrap();
    let snapshot = db.native_directory_snapshot(&shelf).unwrap().unwrap();
    assert!(db.acknowledge_native_directory(NativeDirectoryCompletion { snapshot, renamed_to: Some("Changed".into()), materialized: true }).is_err());
    let current = db.native_directory_snapshot(&shelf).unwrap().unwrap();
    assert_eq!(current.name, "Shelf");
    assert_eq!(current.projected_path.as_deref(), Some("/Shelf"));
}

#[test]
fn definitive_enrichment_answers_do_not_expire() {
    use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};

    for status in [BookEnrichmentStatus::Found, BookEnrichmentStatus::NoMatch] {
        let db = database();
        let book = import();
        let hash = book.content_hash;
        db.commit_import(book).unwrap();
        let candidate = db.enrichment_candidates(&[hash], 0).unwrap().pop().unwrap();
        let answer = BookEnrichment { key: String::new(), status, record: (status == BookEnrichmentStatus::Found).then_some(candidate.record), toc: None, popularity: None, cover: None, detail: String::new() };
        db.record_enrichment(&hash, &candidate.request_key, &answer, 100).unwrap();
        assert!(db.enrichment_candidates(&[hash], 100).unwrap().is_empty());
        assert!(db.enrichment_candidates(&[hash], 10 * 365 * 24 * 60 * 60).unwrap().is_empty());
        assert_eq!(db.enrichment_next_retry().unwrap(), None);
    }
}

#[test]
fn failed_enrichment_retries_until_a_definitive_answer() {
    use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};

    let db = database();
    let book = import();
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    let candidate = db.enrichment_candidates(&[hash], 0).unwrap().pop().unwrap();
    let mut answer = BookEnrichment { key: String::new(), status: BookEnrichmentStatus::Failed, record: None, toc: None, popularity: None, cover: None, detail: String::new() };
    db.record_enrichment(&hash, &candidate.request_key, &answer, 100).unwrap();
    assert_eq!(db.enrichment_next_retry().unwrap(), Some(160));
    assert!(db.enrichment_candidates(&[hash], 159).unwrap().is_empty());
    assert_eq!(db.enrichment_candidates(&[hash], 160).unwrap().len(), 1);
    answer.status = BookEnrichmentStatus::NoMatch;
    db.record_enrichment(&hash, &candidate.request_key, &answer, 160).unwrap();
    assert_eq!(db.enrichment_next_retry().unwrap(), None);
    assert!(db.enrichment_candidates(&[hash], i64::MAX).unwrap().is_empty());
}

#[test]
fn enrichment_reads_identifiers_from_the_stored_metadata_document() {
    use book_model::{Scope, push_isbn};

    let db = database();
    let mut book = import();
    let hash = book.content_hash;
    let identifiers = &mut book.inspection.as_mut().unwrap().metadata.book.identifiers;
    push_isbn(identifiers, "9780393243277", Scope::Book).unwrap();
    let expected = identifiers.clone();
    db.commit_import(book).unwrap();

    let candidates = db.enrichment_candidates(&[hash], 0).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].record.book.identifiers, expected);
}

#[test]
fn navigation_on_book_distinguishes_missing_and_empty_and_replaces_playback_fields() {
    use crate::books::toc::TocSql;

    let db = database();
    let hash = BookId::new(&"a".repeat(64));
    db.connection.fixtures_insert_book_7(&(&hash)).unwrap();
    let read_toc = || {
        let mut toc = None;
        db.connection
            .book_toc_select(&hash, |row| {
                toc = Some(row.get::<_, Vec<u8>>(0)?);
                Ok(())
            })
            .unwrap();
        toc
    };
    assert_eq!(read_toc(), None);

    db.connection.navigation_upsert(&hash, 0, b"[]", Some(1000), Some(b"[]")).unwrap();
    assert_eq!(read_toc().as_deref(), Some(b"[]".as_slice()));
    db.connection.navigation_upsert(&hash, 0, b"[]", None, None).unwrap();
    let playback: (Option<i64>, Option<String>) = db.connection.fixtures_read_book_duration_ms_tracks_json(&(&hash), |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(playback, (None, None));
    assert_eq!(read_toc().as_deref(), Some(b"[]".as_slice()));
}

#[test]
fn stored_audiobook_navigation_syncs_with_its_addition() {
    let db = database();
    let mut book = import();
    book.format = book_model::BookFormat::M4b;
    book.inspection.as_mut().unwrap().audiobook = Some(book_inspection::AudiobookMetadata {
        title: "Only".into(),
        authors: Vec::new(),
        description: String::new(),
        duration_ms: 1000,
        chapters: vec![book_model::AudiobookChapter { title: "Only".into(), start_ms: 0, end_ms: 1000 }],
        tracks: Vec::new(),
        chapter_origin: book_model::ChapterOrigin::EmbeddedM4b,
        recording: Default::default(),
    });
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    complete_no_match(&db, &hash);
    assert!(db.sync_publishable_mutations().unwrap().iter().any(|mutation| matches!(&mutation.state.value,
        library_replica::LibraryStateEntry::BookAdd { value, .. } if value.navigation.as_ref().is_some_and(|nav| nav.duration_ms == Some(1000) && nav.entries.len() == 1))));
}

fn copy_fixture(db: &Database) -> (BookId, DirId, DirId) {
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
    let work = db.native_file_work().unwrap().into_iter().find(|work| work.operation == "materialize").unwrap();
    db.native_file_work_snapshot(work).unwrap().unwrap()
}

fn restore_completion(snapshot: library_database::NativeFileWorkSnapshot) -> library_database::NativeFileWorkCompletion {
    library_database::NativeFileWorkCompletion { final_path: snapshot.resolved_path.to_string(), snapshot, renamed: None, published: true, thumbnail_set_exists: true }
}

#[test]
fn copy_restores_and_acknowledges_the_queued_destination() {
    let db = database();
    let (_, source, destination) = copy_fixture(&db);
    let snapshot = restore_snapshot(&db);
    assert_eq!(snapshot.resolved_path.as_str(), "/Z/book.epub");
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
    assert_eq!(snapshot.resolved_path.as_str(), "/Moved/book.epub");
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
    let projections: i64 = db.connection.fixtures_read_local_file_projection_count(&(destination.to_string()), |row| row.get(0)).unwrap();
    assert_eq!(projections, 0);
}

#[test]
fn trash_work_does_not_target_another_live_copy() {
    let db = database();
    let (hash, source, destination) = copy_fixture(&db);
    db.acknowledge_native_file_work(restore_completion(restore_snapshot(&db))).unwrap();
    db.remove_book_placement(&hash, &source).unwrap();
    assert!(db.native_file_work().unwrap().is_empty(), "soft removal must retain physical copies");
    assert_eq!(projection(&db, source), "/A/book.epub");
    assert_eq!(projection(&db, destination), "/Z/book.epub");
}

#[cfg(feature = "scanner")]
mod scanner {
    use super::*;
    use library_database::{FilesystemScan, InspectedFilesystemScan, ScanApplyResult, ScanFileObservation, ScannedBook};

    fn fixture() -> (Database, BookId) {
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
        db.connection.fixtures_delete_local_file_work().unwrap();
    }

    fn observation(directory: DirId, name: &str, fingerprint: u64) -> ScanFileObservation {
        ScanFileObservation { directory, name: name.into(), path: name.into(), fingerprint }
    }

    fn replacement() -> ScannedBook {
        let hash = BookId::new(&"b".repeat(64));
        ScannedBook {
            directory: ROOT_DIR_ID,
            name: "book.epub".into(),
            path: "book.epub".into(),
            fingerprint: 2,
            content_hash: hash,
            size_bytes: 20,
            extension: "epub".into(),
            inspection: import().inspection.unwrap(),
            inspection_version: 1,
        }
    }

    fn local_state(db: &Database, hash: BookId, directory: DirId) -> (bool, bool, String, i64) {
        db.connection.fixtures_read_local_file_projection_bd_deleted_at_bd(&(&hash), &(directory.to_string()), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap()
    }

    #[test]
    fn storage_recovery_forgets_only_local_state_and_preserves_remote_and_reading_state() {
        use library_model::{BlobKind, DownloadState, TransferOrigin};
        let (db, hash) = fixture();
        db.record_remote_asset(BlobKind::Book, &hash).unwrap();
        db.update_reading_position(&hash, "epubcfi(/6/2)", Some(0.5), None).unwrap();
        db.sync_request_book_downloads(&[hash], TransferOrigin::UserInitiated).unwrap();
        db.clear_sync_outbox().unwrap();
        assert!(db.is_book_downloaded(&hash).unwrap());
        assert_eq!(db.book_count().unwrap(), 1);
        let before = recovery_payloads(&db);

        db.reset_local_storage().unwrap();

        assert!(!db.is_book_downloaded(&hash).unwrap());
        assert_eq!(db.transfer_download_state(&hash, false).unwrap(), DownloadState::NotDownloaded);
        assert!(db.remote_asset_hashes(BlobKind::Book).unwrap().contains(&hash));
        assert_eq!(db.reading_position(&hash).unwrap().as_deref(), Some("epubcfi(/6/2)"));
        assert_eq!(db.book_count().unwrap(), 1);
        assert!(db.native_file_work().unwrap().is_empty());
        assert!(db.book_download_requests(-1, None, 32).unwrap().is_empty());
        assert_eq!(recovery_payloads(&db), before, "storage recovery must not publish a deletion");
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
        assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { readable: true, books: vec![first] }, false, None).unwrap(), ScanApplyResult::Applied);
        assert_eq!(
            ({
                use library_database::DatabaseError;
                use library_database::browse::{CountsSql, HomeSql};
                let database = &db;
                (|| -> Result<usize, DatabaseError> {
                    let mut count = 0_i64;
                    database.connection.count_all_books(|row| {
                        count = row.get(0)?;
                        Ok(())
                    })?;
                    Ok(usize::try_from(count).map_err(|_| DatabaseError::message("library book count must not be negative"))?)
                })()
            })
            .unwrap(),
            2,
            "first batch is immediately browsable"
        );
        assert!(local_state(&db, first_hash, ROOT_DIR_ID).1);
        assert!(local_state(&db, missing, ROOT_DIR_ID).1, "incomplete scans must not clear missing files");
        let mut second = replacement();
        second.name = "second.epub".into();
        second.path = second.name.clone();
        second.content_hash = BookId::new(&"c".repeat(64));

        assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { readable: true, books: vec![second] }, false, None).unwrap(), ScanApplyResult::Applied);
        assert_eq!(
            ({
                use library_database::DatabaseError;
                use library_database::browse::{CountsSql, HomeSql};
                let database = &db;
                (|| -> Result<usize, DatabaseError> {
                    let mut count = 0_i64;
                    database.connection.count_all_books(|row| {
                        count = row.get(0)?;
                        Ok(())
                    })?;
                    Ok(usize::try_from(count).map_err(|_| DatabaseError::message("library book count must not be negative"))?)
                })()
            })
            .unwrap(),
            3
        );
        assert_eq!(db.apply_filesystem_scan_batch(&mut snapshot, &discovery, &InspectedFilesystemScan { readable: true, books: vec![] }, true, None).unwrap(), ScanApplyResult::Applied);
        assert!(!local_state(&db, missing, ROOT_DIR_ID).1);
        assert!(local_state(&db, first_hash, ROOT_DIR_ID).1);
    }

    #[test]
    fn ordinary_import_still_rejects_an_occupied_filename() {
        let (db, old) = fixture();
        let mut book = import();
        book.content_hash = BookId::new(&"b".repeat(64));
        assert!(db.commit_import(book).is_err());
        assert_eq!(local_state(&db, old, ROOT_DIR_ID), (true, true, "1".into(), 1));
    }
}

#[test]
fn cloud_presence_reset_handles_empty_queues_and_persists_policy() {
    let db = database();
    for enabled in [false, true] {
        db.set_asset_storage_policy(enabled, true).unwrap();
        assert_eq!(db.connection.fixtures_read_sync_metadata_cloud_storage_enabled(|r| r.get::<_, bool>(0)).unwrap(), enabled);
        assert!(db.pending_book_uploads(0, 10).unwrap().is_empty());
    }
}

fn assert_uninitialized(db: &Database) {
    assert!(db.connection.is_autocommit(), "failed initialization must release its transaction");
    assert_eq!(db.connection.fixtures_check_main_user_version(|r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(db.connection.fixtures_read_main_sqlite_schema_count(|r| r.get::<_, i64>(0)).unwrap(), 0);
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
    for budget in [500, 3000] {
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
        assert_eq!(reopened.browse_book_detail(&import().content_hash).unwrap().book.title, "Original");
    }
}

#[test]
fn reopening_current_library_does_not_commit_database_changes() {
    use library_database::rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    struct TestDirectory(std::path::PathBuf);
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let directory = TestDirectory(std::env::temp_dir().join(format!("library-reopen-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("library.sqlite");
    let db = Database::open(&path).unwrap();
    let replica = db.initialize_library().unwrap();
    let mode: String = db.connection.pragma_query_value(Some("main"), "journal_mode", |row| row.get(0)).unwrap();
    assert_eq!(mode, "wal", "new libraries must establish persistent WAL mode");
    let observer = library_database::rusqlite::Connection::open(&path).unwrap();
    let data_version = || observer.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0)).unwrap();
    let before = data_version();
    drop(db);

    let reopened = Database::open(&path).unwrap();
    let mode: String = reopened.connection.pragma_query_value(Some("main"), "journal_mode", |row| row.get(0)).unwrap();
    assert_eq!(mode, "wal", "WAL must survive closing and reopening the database");
    reopened.connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Pragma { pragma_name, .. } if pragma_name.eq_ignore_ascii_case("journal_mode") => Authorization::Deny,
        _ => Authorization::Allow,
    })).unwrap();
    // Exercise both ordinary connection configuration and initialization with
    // every journal-mode read/write forbidden, including read-only queries.
    crate::configure_library_connection(&reopened.connection).unwrap();
    assert_eq!(reopened.initialize_library().unwrap(), replica);
    reopened.connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
    assert_eq!(data_version(), before, "opening a current library must not commit a write");

    // Supported older libraries still record the version after migration.
    reopened.connection.pragma_update(None, "user_version", 40).unwrap();
    let before_migration = data_version();
    assert_eq!(reopened.initialize_library().unwrap(), replica);
    assert_eq!(reopened.connection.fixtures_check_main_user_version(|row| row.get::<_, i64>(0)).unwrap(), crate::LIBRARY_SCHEMA_VERSION);
    assert_ne!(data_version(), before_migration);
}

#[test]
fn failed_wal_setup_leaves_library_uninitialized_and_can_retry() {
    use library_database::rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    let db = Database::open(":memory:").unwrap();
    db.connection.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Pragma { pragma_name, .. } if pragma_name.eq_ignore_ascii_case("journal_mode") => Authorization::Deny,
        _ => Authorization::Allow,
    })).unwrap();
    assert!(db.initialize_library().is_err());
    db.connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
    assert!(db.connection.is_autocommit());
    assert_uninitialized(&db);
    let replica = db.initialize_library().unwrap();
    assert_eq!(db.initialize_library().unwrap(), replica);
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
fn subject_browsing_reads_assignments_without_cached_membership() {
    let db = database();
    let mut book = import();
    let hash = book.content_hash;
    book.inspection.as_mut().unwrap().metadata.book.subjects = vec![book_model::BookSubject::new(None, "History / United States / 20th Century", "dc:subject", Some("bisac".into()), Some("HIS036060".into())).unwrap()];
    db.commit_import(book).unwrap();
    let (path, parent): (i64, i64) = db.connection.fixtures_read_book_unified_concept_route_route_id_route(|row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    let mut query = library_model::LibraryBrowseQuery {
        location: library_model::BrowseLocation::Subject(parent),
        search: String::new(),
        file_types: vec![],
        chip_sort: library_model::BrowseChipSort::Alphabetical,
        book_sort: library_model::BrowseBookSort::Alphabetical,
        languages: vec![],
        hide_finished: false,
    };
    let parent_contents = db.browse_contents(&query, false).unwrap();
    assert!(parent_contents.children.is_empty());
    assert!(parent_contents.books.is_empty());
    query.location = library_model::BrowseLocation::Subject(path);
    let contents = db.browse_contents(&query, false).unwrap();
    assert!(contents.books.is_empty());
    query.location = library_model::BrowseLocation::Subject(0);
    query.search.clear();
    let root_contents = db.browse_contents(&query, false).unwrap();
    assert_eq!(root_contents.books[0].content_hash(), Some(hash));
    query.search = "Original".into();
    let searched = db.browse_contents(&query, false).unwrap();
    assert!(searched.children.is_empty());
    assert_eq!(searched.books.len(), 1);
}

fn subject_query(location: library_model::BrowseLocation) -> library_model::LibraryBrowseQuery {
    library_model::LibraryBrowseQuery {
        location,
        search: String::new(),
        file_types: vec![],
        languages: vec![],
        chip_sort: library_model::BrowseChipSort::Alphabetical,
        book_sort: library_model::BrowseBookSort::Alphabetical,
        hide_finished: false,
    }
}

fn subject_fixture() -> (Database, String, String) {
    let db = database();
    for index in 1..=4 {
        let mut book = import();
        book.content_hash = BookId::new(&format!("{index:064x}"));

        book.file_name = format!("book{index}.epub");
        book.published.name = book.file_name.clone();
        book.published.relative_path = format!("/{}", book.file_name);
        book.inspection.as_mut().unwrap().metadata.title = format!("Book {index}");
        book.inspection.as_mut().unwrap().metadata.book.subjects = vec![book_model::BookSubject::new(None, "History / United States / 20th Century", "dc:subject", Some("bisac".into()), Some("HIS036060".into())).unwrap()];
        db.commit_import(book).unwrap();
    }
    let (path, parent): (String, String) = db.connection.fixtures_read_curated_unified_concept_paths_path_parent_path(|r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    // Subsequent browse-only edits explicitly operate on the derived fixture.

    (db, path, parent)
}

#[test]
fn subject_browsing_is_read_only_and_follows_placement_and_assignment_edits() {
    let (db, _, _) = subject_fixture();
    let root = db.browse_contents(&subject_query(library_model::BrowseLocation::Subject(0)), false).unwrap();
    let query = subject_query(root.children[0].id);
    let initial = db.browse_contents(&query, false).unwrap();
    assert!(initial.children.is_empty());
    assert_eq!(initial.books.len(), 4);
    let changes = db.connection.total_changes();
    db.connection.pragma_update(None, "query_only", true).unwrap();
    assert_eq!(db.browse_contents(&query, false).unwrap().children.len(), initial.children.len());
    db.browse(&query, false).unwrap();
    db.connection.pragma_update(None, "query_only", false).unwrap();
    assert_eq!(db.connection.total_changes(), changes);
    db.connection.fixtures_update_book_dir_deleted_at().unwrap();
    let shrunk = db.browse_contents(&query, false).unwrap();
    assert!(shrunk.children.is_empty());
    assert_eq!(shrunk.books.len(), 3);
    db.connection.fixtures_update_book_dir_deleted_at_2().unwrap();
    assert_eq!(db.browse_contents(&query, false).unwrap().books.len(), 4);
    db.connection.fixtures_delete_book_unified_concept_4().unwrap();
    let empty = db.browse_contents(&query, false).unwrap();
    assert!(empty.children.is_empty() && empty.books.is_empty());
}

#[test]
fn failed_subject_read_releases_snapshot_and_can_retry() {
    use library_database::rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let (db, _, _) = subject_fixture();
    db.connection
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Read { table_name: "book_unified_concept", .. } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    assert!(db.browse_contents(&subject_query(library_model::BrowseLocation::Subject(0)), false).is_err());
    assert!(db.connection.is_autocommit());
    db.connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
    assert!(!db.browse_contents(&subject_query(library_model::BrowseLocation::Subject(0)), false).unwrap().children.is_empty());
}

fn browse_folder(db: &Database, dir: DirId, search: &str, languages: &[&str]) -> library_model::BrowseContents {
    let mut query = subject_query(library_model::BrowseLocation::Folder(dir));
    query.search = search.into();
    query.languages = languages.iter().map(|language| (*language).to_owned()).collect();
    db.browse_contents(&query, false).unwrap()
}

fn import_in_folder(db: &Database, index: u64, dir: DirId) -> BookId {
    let mut book = import();
    book.parent_id = dir;
    book.content_hash = BookId::new(&format!("{index:064x}"));

    book.file_name = format!("book{index}.epub");
    book.published.name = book.file_name.clone();
    book.published.relative_path = format!("/{}", book.file_name);
    book.inspection.as_mut().unwrap().metadata.title = format!("Title {index}");
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    hash
}

fn place_book(db: &Database, hash: BookId, dir: DirId, downloaded: bool) {
    db.connection.fixtures_insert_book_dir_4(&(dir.to_string()), &(&hash), &downloaded).unwrap();
}

#[test]
fn all_books_lists_unique_live_books_across_folders() {
    use library_model::BrowseLocation;

    let db = database();
    let query = subject_query(BrowseLocation::Books);
    assert!(db.browse(&query, false).unwrap().contents.books.is_empty());
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let nested = db.create_directory(&shelf, &"Nested".into()).unwrap().id;
    let second = import_in_folder(&db, 102, nested);
    let first = import_in_folder(&db, 101, ROOT_DIR_ID);
    place_book(&db, first, shelf, true);
    let deleted = import_in_folder(&db, 103, shelf);
    db.connection.fixtures_update_book_deleted_at(&(&deleted)).unwrap();
    let unplaced = import_in_folder(&db, 104, shelf);
    db.connection.fixtures_delete_book_dir_2(&(shelf.to_string()), &unplaced).unwrap();

    let page = db.browse(&query, false).unwrap();
    assert_eq!(page.contents.books.iter().map(|book| book.content_hash()).collect::<Vec<_>>(), [Some(first), Some(second)]);
    assert!(page.contents.books.iter().all(|book| book.source_directory.is_none()));
    assert!(page.contents.children.is_empty());
    assert!(page.sections.is_empty());
    assert!(page.path.is_empty());
    assert!(page.ancestors.is_empty());
    assert_eq!(page.contents.books.len(), db.book_count().unwrap());
    assert_eq!(page.format_counts.iter().map(|count| count.book_count).sum::<u32>(), 2);
    assert_eq!(db.browse_contents(&query, false).unwrap(), page.contents);
}

#[test]
fn all_books_uses_shared_search_filters_and_book_order() {
    use crate::assets::TransfersSql;
    use crate::projection::ProjectionSql;
    use library_model::{BrowseBookSort, BrowseLocation, LibraryFileTypeFilter};

    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let first = import_in_folder(&db, 101, ROOT_DIR_ID);
    let second = import_in_folder(&db, 102, shelf);
    let third = import_in_folder(&db, 103, shelf);
    db.connection.fixtures_update_book_dir_is_downloaded().unwrap();
    db.connection.fixtures_update_book_dir_is_downloaded_2(&(&first), &(&second)).unwrap();
    db.connection.asset_clear_book_paths(&(&third)).unwrap();
    db.connection.fixtures_update_book_format(&(&third)).unwrap();
    db.connection.fixtures_insert_book_language_4(&(&third)).unwrap();
    // Derived reading state provides the recent-reading sort order.
    db.connection.projection_set_book_reading(&first, Some("reading"), 50.0_f32, Some(20)).unwrap();
    db.connection.projection_set_book_reading(&second, Some("finished"), 100.0_f32, Some(30)).unwrap();
    let mut query = subject_query(BrowseLocation::Books);
    let hashes = |query: &library_model::LibraryBrowseQuery, downloaded_only| db.browse_contents(query, downloaded_only).unwrap().books.into_iter().map(|book| book.content_hash().unwrap()).collect::<Vec<_>>();
    assert_eq!(hashes(&query, true), [first, second]);
    query.search = "TITLE 102".into();
    assert_eq!(hashes(&query, false), [second]);
    query.search = "original author".into();
    assert_eq!(hashes(&query, false), [first, second, third]);
    query.search = "Shelf".into();
    assert!(hashes(&query, false).is_empty());
    query.search.clear();
    query.file_types = vec![LibraryFileTypeFilter::Pdf];
    assert_eq!(hashes(&query, false), [third]);
    query.file_types.clear();
    query.languages = vec!["EN_us".into()];
    assert_eq!(hashes(&query, false), [first, second]);
    query.languages.clear();
    query.book_sort = BrowseBookSort::RecentlyRead;
    assert_eq!(hashes(&query, false), [second, first, third]);
    query.hide_finished = true;
    assert_eq!(hashes(&query, false), [first, third]);
    query.search = "no matches".into();
    assert!(hashes(&query, false).is_empty());
}

#[test]
fn empty_folders_remain_visible_in_local_and_paged_browsing() {
    use library_model::{BrowseLocation, BrowsePageRequest};

    let db = database();
    let empty = db.create_directory(&ROOT_DIR_ID, &"Empty".into()).unwrap().id;
    let parent = db.create_directory(&ROOT_DIR_ID, &"Projects".into()).unwrap().id;
    let nested = db.create_directory(&parent, &"Empty nested".into()).unwrap().id;
    let remote = db.create_directory(&ROOT_DIR_ID, &"Remote books".into()).unwrap().id;
    import_in_folder(&db, 401, remote);
    db.connection.fixtures_update_book_dir_is_downloaded_3(&0).unwrap();
    let local = db.create_directory(&ROOT_DIR_ID, &"Local books".into()).unwrap().id;
    import_in_folder(&db, 402, local);
    let query = subject_query(BrowseLocation::Folder(ROOT_DIR_ID));

    for downloaded_only in [false, true] {
        let views = [db.browse(&query, downloaded_only).unwrap(), db.browse_sections(&query, downloaded_only, 2).unwrap(), db.browse_page(&query, downloaded_only, BrowsePageRequest::default()).unwrap()];
        for view in views {
            for id in [empty, parent, local] {
                assert!(view.contents.children.iter().any(|row| row.id == BrowseLocation::Folder(id)), "folder {id} disappeared with downloaded_only={downloaded_only}");
            }
            assert_eq!(view.contents.children.iter().any(|row| row.id == BrowseLocation::Folder(remote)), !downloaded_only);
            let section = view.sections.iter().find(|section| section.parent.id == BrowseLocation::Folder(empty)).unwrap();
            assert_eq!(section.parent.book_count, 0);
            assert!(section.books.is_empty());
            let container = view.sections.iter().find(|section| section.parent.id == BrowseLocation::Folder(parent)).unwrap();
            assert_eq!(container.children[0].id, BrowseLocation::Folder(nested));
        }
        let contents = db.browse_contents(&query, downloaded_only).unwrap();
        assert!(contents.children.iter().any(|row| row.id == BrowseLocation::Folder(empty)));

        let nested_view = db.browse(&subject_query(BrowseLocation::Folder(parent)), downloaded_only).unwrap();
        assert_eq!(nested_view.contents.children[0].id, BrowseLocation::Folder(nested));

        let mut search = query.clone();
        search.search = "EMPTY".into();
        let found = db.browse_contents(&search, downloaded_only).unwrap();
        assert_eq!(found.children.len(), 2);
        assert_eq!(found.children.iter().find(|row| row.id == BrowseLocation::Folder(nested)).unwrap().path.as_deref(), Some("Projects"));
    }
}

#[test]
fn empty_folders_still_respect_explicit_book_filters() {
    use library_model::{BrowseLocation, LibraryFileTypeFilter};

    let db = database();
    let empty = db.create_directory(&ROOT_DIR_ID, &"Empty".into()).unwrap().id;
    let mut query = subject_query(BrowseLocation::Folder(ROOT_DIR_ID));
    query.file_types = vec![LibraryFileTypeFilter::Book];
    assert!(db.browse_contents(&query, true).unwrap().children.is_empty());
    query.file_types.clear();
    query.languages = vec!["en".into()];
    assert!(db.browse_contents(&query, true).unwrap().children.is_empty());
    query.languages.clear();
    query.hide_finished = true;
    assert!(db.browse_contents(&query, true).unwrap().children.is_empty());
    query.hide_finished = false;
    assert_eq!(db.browse_contents(&query, true).unwrap().children[0].id, BrowseLocation::Folder(empty));
}

#[test]
fn folder_counts_include_all_placements_and_search_sources() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let nested = db.create_directory(&shelf, &"Nested".into()).unwrap().id;
    let branch = db.create_directory(&shelf, &"Branch".into()).unwrap().id;
    let empty = db.create_directory(&ROOT_DIR_ID, &"Empty".into()).unwrap().id;
    let first = import_in_folder(&db, 101, nested);
    let second = import_in_folder(&db, 102, branch);
    let third = import_in_folder(&db, 103, shelf);
    // Exercise derived browse rows without producing unrelated canonical edits.

    db.connection.fixtures_update_book_dir_is_downloaded().unwrap();
    place_book(&db, first, shelf, false);
    place_book(&db, first, branch, true);
    db.connection.fixtures_update_book_dir_is_downloaded_2(&(&second), &(&third)).unwrap();
    db.connection.fixtures_update_book_format(&(&third)).unwrap();
    db.connection.fixtures_insert_book_language_4(&(&third)).unwrap();
    let view = browse_folder(&db, ROOT_DIR_ID, "", &[]);
    let chip = view.children.iter().find(|c| c.id == library_model::BrowseLocation::Folder(shelf)).unwrap();
    assert_eq!((chip.book_count, chip.downloaded_book_count), (5, 3));
    assert_eq!(view.children.iter().find(|c| c.id == library_model::BrowseLocation::Folder(empty)).unwrap().book_count, 0);
    assert!(view.books.is_empty(), "ordinary browsing only shows direct placements");
    let direct = browse_folder(&db, shelf, "", &[]);
    assert_eq!(direct.books.len(), 2);
    assert!(direct.books.iter().all(|b| b.source_directory == Some(shelf)));
    let searched = browse_folder(&db, shelf, "Title", &[]);
    assert_eq!(searched.books.len(), 5, "search returns each descendant placement");
    assert_eq!(searched.books.iter().filter(|b| b.content_hash() == Some(first)).count(), 3);
    let source = [shelf, nested, branch].into_iter().min_by_key(|id| id.to_string()).unwrap();
    assert_eq!(searched.books.iter().find(|b| b.content_hash() == Some(first)).unwrap().source_directory, Some(source));
    let filtered = browse_folder(&db, ROOT_DIR_ID, "", &["en"]);
    assert_eq!(filtered.children.len(), 1);
    assert_eq!(filtered.children[0].book_count, 4);
    assert_eq!(browse_folder(&db, shelf, "Title", &["EN_us"]).books.len(), 4);
    let mut facet_query = subject_query(library_model::BrowseLocation::Folder(shelf));
    facet_query.languages = vec!["en".into()];
    let facets = db.browse(&facet_query, false).unwrap();
    let (languages, formats) = (facets.language_counts, facets.format_counts);
    assert_eq!(languages.iter().find(|l| l.language == "en").unwrap().book_count, 2);
    assert_eq!(formats.iter().map(|f| f.book_count).sum::<u32>(), 3);
    // A folder-name search still finds matching descendant chips and breadcrumbs.
    let nested_search = browse_folder(&db, ROOT_DIR_ID, "Nested", &[]);
    assert_eq!(nested_search.children.len(), 1);
    assert_eq!(nested_search.children[0].path.as_deref(), Some("Shelf"));
    assert_eq!(nested_search.children[0].book_count, 1);
    // Every removed or moved placement changes the subtree count.
    db.connection.fixtures_delete_book_dir_2(&(shelf.to_string()), &first).unwrap();
    assert_eq!(browse_folder(&db, ROOT_DIR_ID, "", &[]).children.iter().find(|c| c.id == library_model::BrowseLocation::Folder(shelf)).unwrap().book_count, 4);
    db.connection.fixtures_update_book_dir_dir_id(&(empty.to_string()), &(nested.to_string()), &first).unwrap();
    let moved = browse_folder(&db, ROOT_DIR_ID, "", &[]);
    assert_eq!(moved.children.iter().find(|c| c.id == library_model::BrowseLocation::Folder(shelf)).unwrap().book_count, 3);
    assert_eq!(moved.children.iter().find(|c| c.id == library_model::BrowseLocation::Folder(empty)).unwrap().book_count, 1);
    assert_eq!(browse_folder(&db, shelf, "Title", &[]).books.iter().find(|b| b.content_hash() == Some(first)).unwrap().source_directory, Some(branch));
}

#[test]
fn folder_counts_follow_writes_and_browsing_is_read_only() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let branch = db.create_directory(&shelf, &"Branch".into()).unwrap().id;
    let other = db.create_directory(&ROOT_DIR_ID, &"Other".into()).unwrap().id;
    let hash = import_in_folder(&db, 201, branch);
    let counts = || {
        let view = browse_folder(&db, ROOT_DIR_ID, "", &[]);
        let folder = view.children.iter().find(|row| row.id == library_model::BrowseLocation::Folder(shelf)).unwrap();
        (folder.book_count, folder.downloaded_book_count)
    };
    assert_eq!(counts(), (1, 1));
    for downloaded in [0, 1, 0] {
        db.connection.fixtures_update_book_dir_is_downloaded_3(&downloaded).unwrap();
        assert_eq!(counts(), (1, downloaded as usize));
    }
    let tx = db.connection.unchecked_transaction().unwrap();
    tx.fixtures_update_book_dir_deleted_at_3().unwrap();
    assert_eq!(db.connection.fixtures_read_book_dir_count(|r| r.get::<_, i64>(0)).unwrap(), 0);
    tx.rollback().unwrap();
    assert_eq!(counts(), (1, 0));
    db.connection.pragma_update(None, "query_only", true).unwrap();
    assert_eq!(browse_folder(&db, branch, "", &[]).books.len(), 1);
    db.connection.pragma_update(None, "query_only", false).unwrap();

    db.connection.fixtures_update_dir_name_parent_id(&(other.to_string()), &(branch.to_string())).unwrap();

    let view = browse_folder(&db, ROOT_DIR_ID, "", &[]);
    assert_eq!(view.children.iter().find(|c| c.id == library_model::BrowseLocation::Folder(shelf)).unwrap().book_count, 0);
    assert_eq!(view.children.iter().find(|c| c.id == library_model::BrowseLocation::Folder(other)).unwrap().book_count, 1);
    db.connection.fixtures_update_book_dir_deleted_at_4(&(&hash)).unwrap();
    assert_eq!(browse_folder(&db, ROOT_DIR_ID, "", &[]).children.iter().find(|c| c.id == library_model::BrowseLocation::Folder(other)).unwrap().book_count, 0);
    db.connection.fixtures_update_book_dir_deleted_at_2().unwrap();
    db.connection.fixtures_update_dir_deleted_at(&(other.to_string())).unwrap();
    assert!(browse_folder(&db, ROOT_DIR_ID, "Title", &[]).books.is_empty(), "deleted ancestors block membership");
    assert_eq!(browse_folder(&db, branch, "", &[]).books.len(), 1, "a live detached subtree retains its own membership");
}

#[test]
fn folder_sections_use_subtree_counts_and_normalized_languages() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let child = db.create_directory(&shelf, &"Child".into()).unwrap().id;
    let empty = db.create_directory(&shelf, &"Empty".into()).unwrap().id;
    let branch = db.create_directory(&child, &"Branch".into()).unwrap().id;
    import_in_folder(&db, 301, child);
    import_in_folder(&db, 302, branch);
    db.connection.fixtures_insert_book_language_5().unwrap();
    let mut query = subject_query(library_model::BrowseLocation::Folder(ROOT_DIR_ID));
    let unfiltered = db.browse(&query, false).unwrap();
    assert_eq!(unfiltered.sections.len(), 1);
    assert_eq!(unfiltered.sections[0].parent.id, library_model::BrowseLocation::Folder(shelf));
    assert_eq!(unfiltered.sections[0].children.iter().map(|row| row.id).collect::<Vec<_>>(), [library_model::BrowseLocation::Folder(child), library_model::BrowseLocation::Folder(empty)]);
    let downloaded = db.browse(&query, true).unwrap();
    assert_eq!(downloaded.sections[0].children.iter().map(|row| row.id).collect::<Vec<_>>(), [library_model::BrowseLocation::Folder(child), library_model::BrowseLocation::Folder(empty)]);
    query.languages = vec!["en".into()];
    let page = db.browse(&query, false).unwrap();
    assert_eq!(page.sections.len(), 1);
    let contents = &page.sections[0];
    assert_eq!(contents.children.len(), 1);
    assert_eq!(contents.children[0].id, library_model::BrowseLocation::Folder(child));
    assert_eq!(contents.children[0].book_count, 2);
    query.languages = vec!["sv".into()];
    let no_matches = db.browse(&query, false).unwrap();
    assert!(no_matches.contents.children.is_empty());
    assert!(no_matches.sections.is_empty());
}

#[test]
fn folder_sections_preserve_wrapper_order_and_direct_search_results() {
    let db = database();
    let wrapper = db.create_directory(&ROOT_DIR_ID, &"A wrapper".into()).unwrap().id;
    let nested = db.create_directory(&wrapper, &"Nested".into()).unwrap().id;
    let plain = db.create_directory(&ROOT_DIR_ID, &"Z plain".into()).unwrap().id;
    import_in_folder(&db, 303, nested);
    import_in_folder(&db, 304, plain);
    let query = subject_query(library_model::BrowseLocation::Folder(ROOT_DIR_ID));
    let page = db.browse(&query, false).unwrap();
    assert_eq!(page.contents.children.iter().map(|row| row.id).collect::<Vec<_>>(), [library_model::BrowseLocation::Folder(wrapper), library_model::BrowseLocation::Folder(plain)]);
    assert_eq!(page.sections.iter().map(|section| section.parent.id).collect::<Vec<_>>(), [library_model::BrowseLocation::Folder(plain), library_model::BrowseLocation::Folder(wrapper)]);
    assert_eq!(page.sections[1].children[0].id, library_model::BrowseLocation::Folder(nested));
    assert_eq!(page.sections[0].books.len(), 1);
    assert!(page.sections[1].books.is_empty());
    let search = library_model::LibraryBrowseQuery { search: "Nested".into(), ..query };
    let searched = db.browse(&search, false).unwrap();
    assert!(searched.sections.is_empty());
    assert_eq!(searched.contents.children[0].id, library_model::BrowseLocation::Folder(nested));
    assert_eq!(searched.contents.children[0].path.as_deref(), Some("A wrapper"));
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
            db.connection.fixtures_update_book_dir_is_downloaded_4(&(book < downloads), &(&hash)).unwrap();
        }
    }
    db.connection.fixtures_insert_book_language_5().unwrap();
    for filtered in [false, true] {
        for search in ["", "ology"] {
            for sort in [library_model::BrowseChipSort::Alphabetical, library_model::BrowseChipSort::BookCount] {
                let mut query = subject_query(library_model::BrowseLocation::Folder(ROOT_DIR_ID));
                query.search = search.into();
                query.chip_sort = sort;
                if filtered {
                    query.languages = vec!["en".into()];
                    query.file_types = vec![library_model::LibraryFileTypeFilter::Book];
                }
                let contents = db.browse_contents(&query, true).unwrap();
                let mut expected = if !search.is_empty() {
                    vec![("Écology", 1), ("Zoology", 1)]
                } else if sort == library_model::BrowseChipSort::BookCount {
                    vec![("Most", 2), ("Écology", 1), ("Zoology", 1)]
                } else {
                    vec![("Écology", 1), ("Most", 2), ("Zoology", 1)]
                };
                if !filtered && search.is_empty() {
                    let position = if sort == library_model::BrowseChipSort::BookCount { expected.len() } else { 1 };
                    expected.insert(position, ("Empty", 0));
                }
                assert_eq!(contents.children.iter().map(|row| (row.name.as_str(), row.book_count)).collect::<Vec<_>>(), expected);
                assert!(contents.children.iter().all(|row| row.book_count == row.downloaded_book_count));
            }
        }
    }
}

#[test]
fn subject_cards_and_global_facets_use_normalized_languages() {
    let (db, _path, _) = subject_fixture();
    db.connection.fixtures_insert_book_language_5().unwrap();
    let root = db.browse_contents(&subject_query(library_model::BrowseLocation::Subject(0)), false).unwrap();
    let mut query = subject_query(root.children[0].id);
    query.languages = vec!["en".into()];
    assert_eq!(db.browse_contents(&query, false).unwrap().books.len(), 4);
    let facets = db.browse(&query, false).unwrap();
    let (languages, formats) = (facets.language_counts, facets.format_counts);
    assert_eq!(languages.iter().find(|l| l.language == "en").unwrap().book_count, 4);
    assert_eq!(db.library_language_counts().unwrap(), languages);
    assert_eq!(formats.iter().map(|f| f.book_count).sum::<u32>(), 4);
}

#[test]
fn browse_progress_is_derived_without_rewriting_sync_registers_and_audio_is_book_scoped() {
    let db = database();
    let hash = import_in_folder(&db, 401, ROOT_DIR_ID);
    // Browse progress comes from the dedicated register, independent of the location.

    for (position, progress, expected) in [("0.25", 0., 0.), ("35", 0., 0.), ("chapter.xhtml", 0., 0.), ("0.25", 70., 70.), ("101", 0., 0.), (" 0.5suffix", 0., 0.)] {
        db.connection.fixtures_update_book_read_pos_read_progress(&position, &progress, &(&hash)).unwrap();
        assert_eq!(db.browse_book_detail(&hash).unwrap().book.progress, f64::from(expected) as f32);
        let stored: (String, f64) = db.connection.fixtures_read_book_read_pos_read_progress(&(&hash), |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(stored, (position.to_owned(), progress));
    }
    db.connection.fixtures_update_book_duration_ms(&(&hash)).unwrap();
    db.connection.fixtures_update_book_entry_count(&(&hash)).unwrap();
    let folder = db.create_directory(&ROOT_DIR_ID, &"Copy".into()).unwrap().id;
    place_book(&db, hash, folder, false);
    let detail = db.browse_book_detail(&hash).unwrap();
    assert_eq!(detail.book.audiobook_duration_ms, Some(12345));
    assert_eq!(detail.audiobook_chapter_count, Some(7));
    db.connection.fixtures_update_book_duration_ms_2(&(&hash)).unwrap();
    let detail = db.browse_book_detail(&hash).unwrap();
    assert_eq!(detail.book.audiobook_duration_ms, None);
    assert_eq!(detail.audiobook_chapter_count, None);
}

/// Books a search from the root finds, by title or author.
fn searched(db: &Database, search: &str) -> Vec<BookId> {
    browse_folder(db, ROOT_DIR_ID, search, &[]).books.into_iter().map(|book| book.content_hash().unwrap()).collect()
}

#[test]
fn search_reads_stored_keys_without_normalizing_metadata_again() {
    let db = database();
    let hash = import_in_folder(&db, 701, ROOT_DIR_ID);
    // Queries still normalize the user's input in Rust. No database scalar call
    // should be needed to search/sort title and author metadata after its write.
    db.connection
        .create_scalar_function("normalize_search_text", 1, library_database::rusqlite::functions::FunctionFlags::SQLITE_UTF8, |_| -> library_database::rusqlite::Result<String> { Err(library_database::rusqlite::Error::InvalidQuery) })
        .unwrap();
    assert_eq!(searched(&db, "title")[0], hash);
    assert_eq!(searched(&db, "original author")[0], hash);
}

#[test]
fn folder_count_queries_do_not_scan_unrelated_library_books() {
    use crate::browse::FoldersSql;
    use crate::query_methods::active_query_fullscan_steps;
    let db = database();
    let small = db.create_directory(&ROOT_DIR_ID, &"Small".into()).unwrap().id;
    let child = db.create_directory(&small, &"Child".into()).unwrap().id;
    let big = db.create_directory(&ROOT_DIR_ID, &"Big".into()).unwrap().id;
    import_in_folder(&db, 901, child);

    let tx = db.connection.unchecked_transaction().unwrap();
    tx.fixtures_read_numbers_numbers_n_n().unwrap();
    tx.fixtures_insert_book_dir_5(&(big.to_string())).unwrap();
    tx.fixtures_read_numbers_numbers_n_n_2(&(big.to_string())).unwrap();
    tx.commit().unwrap();
    browse_folder(&db, ROOT_DIR_ID, "", &[]);
    let directories = serde_json::to_string(&[child.to_string()]).unwrap();
    let mut count = 0;
    db.connection
        .folder_direct_counts(&directories, 0, "", 0, f64::from(library_model::FINISHED_PROGRESS_THRESHOLD), |row| {
            count = row.get::<_, i64>(1)?;
            assert!(active_query_fullscan_steps(&db.connection) < 100, "direct counts must not scan unrelated books");
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 1);
    db.connection.fixtures_insert_book_language_6().unwrap();
    for (formats, languages) in [(1, ""), (2, ""), (3, ""), (0, ",en,"), (1, ",en,"), (1, ",sv,")] {
        let mut actual = 0;
        db.connection
            .folder_direct_counts(&directories, formats, languages, 0, f64::from(library_model::FINISHED_PROGRESS_THRESHOLD), |row| {
                actual = row.get::<_, i64>(1)?;
                assert!(active_query_fullscan_steps(&db.connection) < 100, "filtered counts must not scan unrelated books");
                Ok(())
            })
            .unwrap();
        assert_eq!(actual, i64::from((formats == 0 || formats & 1 != 0) && (languages.is_empty() || languages == ",en,")));
    }
    assert_eq!(browse_folder(&db, ROOT_DIR_ID, "", &[]).children.iter().find(|row| row.id == library_model::BrowseLocation::Folder(small)).unwrap().book_count, 1);
}

#[test]
fn read_time_directory_counts_follow_deletes_and_physical_cascades() {
    let db = database();
    let shelf = db.create_directory(&ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let branch = db.create_directory(&shelf, &"Branch".into()).unwrap().id;
    let hash = import_in_folder(&db, 901, branch);
    place_book(&db, hash, shelf, true);
    let root_count = || browse_folder(&db, ROOT_DIR_ID, "", &[]).children.iter().map(|row| row.book_count).sum::<usize>();
    assert_eq!(root_count(), 2);
    db.connection.fixtures_update_book_deleted_at(&(&hash)).unwrap();
    assert_eq!(root_count(), 0);
    db.connection.fixtures_update_book_deleted_at_2(&(&hash)).unwrap();
    assert_eq!(root_count(), 2);
    db.connection.fixtures_delete_dir(&(branch.to_string())).unwrap();
    assert_eq!(root_count(), 1);
    db.connection.fixtures_delete_book(&(&hash)).unwrap();
    assert_eq!(root_count(), 0);
    import_in_folder(&db, 902, shelf);
    db.connection.fixtures_delete_dir(&(shelf.to_string())).unwrap();
    assert_eq!(root_count(), 0);
}

fn projected_subject_codes(db: &Database) -> Vec<String> {
    query_rows(|callback| db.connection.fixtures_read_book_subject_code(callback), |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
}

#[cfg(test)]
use crate::sql_fixtures::FixturesSql;

fn complete_no_match(db: &Database, hash: &BookId) {
    use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};
    let Some(candidate) = db.enrichment_candidates(&[*hash], 0).unwrap().pop() else { return };
    db.record_enrichment(hash, &candidate.request_key, &BookEnrichment { key: String::new(), status: BookEnrichmentStatus::NoMatch, record: None, toc: None, popularity: None, cover: None, detail: String::new() }, 0).unwrap();
}

#[test]
fn failed_enrichment_blocks_uploads_and_metadata_only_sync_until_a_definitive_answer() {
    use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};
    let db = database();
    let book = import();
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    db.connection.fixtures_ready_book_upload(&hash).unwrap();
    assert!(db.pending_book_uploads(0, 10).unwrap().is_empty());
    assert!(db.transfer_snapshot(&[hash]).unwrap().asset_for(&hash).unwrap().upload.intent.is_none());
    db.set_asset_storage_policy(false, false).unwrap();
    assert!(db.sync_publishable_mutations().unwrap().is_empty());
    let candidate = db.enrichment_candidates(&[hash], 0).unwrap().pop().unwrap();
    let mut answer = BookEnrichment { key: String::new(), status: BookEnrichmentStatus::Failed, record: None, toc: None, popularity: None, cover: None, detail: String::new() };
    db.record_enrichment(&hash, &candidate.request_key, &answer, 0).unwrap();
    assert_eq!(db.enrichment_next_retry().unwrap(), Some(60));
    assert!(db.enrichment_candidates(&[hash], 59).unwrap().is_empty());
    assert_eq!(db.enrichment_candidates(&[hash], 60).unwrap().len(), 1);
    assert!(db.sync_publishable_mutations().unwrap().is_empty());
    answer.status = BookEnrichmentStatus::NoMatch;
    db.record_enrichment(&hash, &candidate.request_key, &answer, 60).unwrap();
    assert!(db.enrichment_next_retry().unwrap().is_none());
    assert!(db.sync_publishable_mutations().unwrap().iter().any(|publication| matches!(publication.state.value, library_replica::LibraryStateEntry::BookAdd { .. })));
    db.set_asset_storage_policy(true, false).unwrap();
    assert_eq!(db.pending_book_uploads(0, 10).unwrap().len(), 1);
    assert!(db.transfer_snapshot(&[hash]).unwrap().asset_for(&hash).unwrap().upload.intent.is_some());
}

#[test]
fn enrichment_cover_checks_preserve_transfer_state_and_survive_reopening() {
    let path = std::env::temp_dir().join(format!("enrichment-cover-check-{}.db", uuid::Uuid::new_v4()));
    let db = Database::open(&path).unwrap();
    db.initialize_library().unwrap();
    let book = import();
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    db.complete_thumbnail_work(&hash, false).unwrap();
    assert!(db.enrichment_cover_candidates(&[hash]).unwrap().is_empty());
    let candidate = db.enrichment_candidates(&[hash], 0).unwrap().pop().unwrap();
    let answer = metadata_contract::books::BookEnrichment { key: String::new(), status: metadata_contract::books::BookEnrichmentStatus::NoMatch, record: None, toc: None, popularity: None, cover: None, detail: String::new() };
    db.record_enrichment(&hash, &candidate.request_key, &answer, 0).unwrap();
    assert_eq!(db.enrichment_cover_record(&hash).unwrap().unwrap().title, "Original");
    assert_eq!(db.enrichment_cover_candidates(&[hash]).unwrap(), vec![hash]);
    db.mark_enrichment_cover_checked(&hash).unwrap();
    assert!(db.enrichment_cover_candidates(&[hash]).unwrap().is_empty());
    assert_eq!(db.thumbnail_work_state(&hash).unwrap().as_deref(), Some("no_cover"));
    drop(db);
    let reopened = Database::open(&path).unwrap();
    reopened.initialize_library().unwrap();
    assert!(reopened.enrichment_cover_candidates(&[hash]).unwrap().is_empty());
    assert_eq!(reopened.thumbnail_work_state(&hash).unwrap().as_deref(), Some("no_cover"));
    drop(reopened);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn direct_cover_lookup_uses_file_title_and_enriched_attributes_without_a_server_cover_id() {
    use book_model::{AUTHOR_MARC_RELATOR_CODE, Contributor, Identifier, Scheme, Scope};
    use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};
    let db = database();
    let book = import();
    let hash = book.content_hash;
    db.commit_import(book).unwrap();
    let candidate = db.enrichment_candidates(&[hash], 0).unwrap().pop().unwrap();
    let mut enriched = candidate.record.clone();
    enriched.title = "Provider edition title".into();
    enriched.description = "Provider description".into();
    enriched.contributors = vec![Contributor::new("Matched Author", AUTHOR_MARC_RELATOR_CODE).unwrap()];
    enriched.book.identifiers.push(Identifier::new("9780306406157", Scheme::Isbn, Scope::Book).unwrap());
    db.record_enrichment(&hash, &candidate.request_key, &BookEnrichment { key: String::new(), status: BookEnrichmentStatus::Found, record: Some(enriched.clone()), toc: None, cover: None, popularity: None, detail: String::new() }, 0)
        .unwrap();
    let record = db.enrichment_cover_record(&hash).unwrap().unwrap();
    assert_eq!(record.title, "Original");
    assert_eq!(record.book.identifiers, enriched.book.identifiers);
    assert_eq!(record.contributors, enriched.contributors);
    assert!(record.description.is_empty());
    assert!(db.enrichment_cover_source(&hash).unwrap().is_none());
}

#[test]
fn flat_book_copy_preserves_each_source_and_file_name() {
    let db = database();
    let first_source = db.create_directory(&ROOT_DIR_ID, &"First".into()).unwrap().id;
    let second_source = db.create_directory(&ROOT_DIR_ID, &"Second".into()).unwrap().id;
    let destination = db.create_directory(&ROOT_DIR_ID, &"Copies".into()).unwrap().id;
    let first = import_in_folder(&db, 901, first_source);
    let second = import_in_folder(&db, 902, second_source);

    for hash in [first, second] {
        assert!(db.copy_book_to_directory(&hash, None, &destination).unwrap());
    }
    assert_eq!(browse_folder(&db, first_source, "", &[]).books[0].content_hash(), Some(first));
    assert_eq!(browse_folder(&db, second_source, "", &[]).books[0].content_hash(), Some(second));
    assert_eq!(browse_folder(&db, destination, "", &[]).books.len(), 2);
    assert!(db.has_book_named(&destination, "book901.epub").unwrap());
    assert!(db.has_book_named(&destination, "book902.epub").unwrap());
    assert_eq!(db.browse_contents(&subject_query(library_model::BrowseLocation::Books), false).unwrap().books.len(), 2);
}

#[test]
fn flat_book_copy_skips_existing_assignments_and_retains_them_on_undo() {
    let db = database();
    let destination = db.create_directory(&ROOT_DIR_ID, &"Copies".into()).unwrap().id;
    let first = import_in_folder(&db, 903, ROOT_DIR_ID);
    let second = import_in_folder(&db, 904, ROOT_DIR_ID);
    assert!(db.copy_book_to_directory(&first, Some(&ROOT_DIR_ID), &destination).unwrap());
    db.clear_sync_outbox().unwrap();
    let work = || db.native_file_work().unwrap().into_iter().map(|work| (work.id, work.operation, work.content_hash, work.original_path.to_string())).collect::<Vec<_>>();
    let work_before = work();
    assert!(!db.copy_book_to_directory(&first, None, &destination).unwrap());
    assert!(!db.copy_book_to_directory(&first, Some(&destination), &destination).unwrap());
    assert!(db.sync_publishable_mutations().unwrap().is_empty());
    assert_eq!(work(), work_before);

    // The shared copy workflow records undo only for newly created placements.
    let mut undo = Vec::new();
    for hash in [first, second] {
        if db.copy_book_to_directory(&hash, None, &destination).unwrap() {
            undo.push(hash);
        }
    }
    assert_eq!(undo, [second]);
    for hash in undo {
        assert!(!db.remove_book_placement(&hash, &destination).unwrap());
    }
    assert_eq!(browse_folder(&db, destination, "", &[]).books[0].content_hash(), Some(first));
    assert_eq!(browse_folder(&db, ROOT_DIR_ID, "", &[]).books.len(), 2);
}

#[test]
fn flat_book_copy_failure_keeps_the_successful_prefix() {
    let db = database();
    let destination = db.create_directory(&ROOT_DIR_ID, &"Copies".into()).unwrap().id;
    let first = import_in_folder(&db, 905, ROOT_DIR_ID);
    let missing = BookId::new(&"f".repeat(64));
    assert!(db.copy_book_to_directory(&first, None, &destination).unwrap());
    assert!(db.copy_book_to_directory(&missing, None, &destination).is_err());
    assert_eq!(browse_folder(&db, destination, "", &[]).books[0].content_hash(), Some(first));
    assert_eq!(browse_folder(&db, ROOT_DIR_ID, "", &[]).books[0].content_hash(), Some(first));
}
