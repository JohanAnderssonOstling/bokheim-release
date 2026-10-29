use library_database::Database;
use library_replica::{LibraryStateEntry, StatePublication};
use sync_common::{Delivery, DirId, LibraryRevision, PullStateResponse, ROOT_DIR_ID, ReplicaId, ReplicaSeq, StateDelivery, SyncCursor};

fn database() -> Database {
    let database = Database::open(":memory:").unwrap();
    database.initialize_library().unwrap();
    database
}

fn delivery(publication: &StatePublication) -> StateDelivery {
    Delivery { state: sync_common::transport::representation::<_, sync_common::Versioned<sync_common::OpaqueStateEntry>>(&publication.state).unwrap(), revision: LibraryRevision::new(1).unwrap() }
}

fn pull(database: &Database, changes: Vec<StateDelivery>) {
    database.sync_commit_pull_response(&[], &PullStateResponse { mutations: changes, next_cursor: SyncCursor::default(), has_more: false }).unwrap();
}

#[test]
fn recovery_forwards_the_winning_state_without_authoring_an_edit() {
    let older = LibraryStateEntry::DirectoryName { dir_id: DirId::from_u128(7), name: "Older".into() }.publication(ReplicaId::from_u128(1), 10, ReplicaSeq::new(90).unwrap());
    // The winner's sequence must differ from the local replica's first ones,
    // or forwarding it could not be told apart from a fresh local publication.
    let winner = LibraryStateEntry::DirectoryName { dir_id: DirId::from_u128(7), name: "Winner".into() }.publication(ReplicaId::from_u128(2), 10, ReplicaSeq::new(40).unwrap());
    for changes in [vec![delivery(&older), delivery(&winner)], vec![delivery(&winner), delivery(&older)]] {
        let database = database();
        pull(&database, changes);
        assert!(database.sync_publishable_mutations().unwrap().is_empty());
        let address = winner.state.value.address();
        assert_eq!(database.sync_enqueue_missing_state_cells(&[address.clone()]).unwrap(), 1);
        let first = database.sync_publishable_mutations().unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].state, winner.state);
        assert_ne!(first[0].publisher_seq, winner.publisher_seq);
        assert_eq!(database.sync_publishable_mutations().unwrap(), first);
        assert_eq!(database.sync_enqueue_missing_state_cells(&[address.clone()]).unwrap(), 0);
        database.sync_acknowledge_mutations(&[first[0].publisher_seq]).unwrap();
        assert!(database.sync_publishable_mutations().unwrap().is_empty());
        assert_eq!(database.sync_enqueue_missing_state_cells(&[address]).unwrap(), 1);
        let second = database.sync_publishable_mutations().unwrap();
        assert_eq!(second[0].state, winner.state);
        assert_ne!(second[0].publisher_seq, first[0].publisher_seq);
    }
}

#[test]
fn acknowledging_an_old_publication_cannot_remove_a_new_edit() {
    let database = database();
    let id = DirId::from_u128(7);
    database.create_directory_with_id(&id, &ROOT_DIR_ID, &"First".into()).unwrap();
    let first = database.sync_publishable_mutations().unwrap();
    database.move_directory(&id, None, Some("Second")).unwrap();
    let before_ack = database.sync_publishable_mutations().unwrap();
    let new_name = before_ack.iter().find(|publication| matches!(&publication.state.value, LibraryStateEntry::DirectoryName { name, .. } if name == "Second")).unwrap();
    assert_eq!(new_name.state.version.replica_seq, new_name.publisher_seq);
    database.sync_acknowledge_mutations(&first.iter().map(|publication| publication.publisher_seq).collect::<Vec<_>>()).unwrap();
    let after_ack = database.sync_publishable_mutations().unwrap();
    assert!(after_ack.contains(new_name));
}

#[test]
fn outbox_readers_share_bounds_watermarks_and_ordering() {
    let db = database();
    for index in 1..=350 {
        db.create_directory_with_id(&DirId::from_u128(index), &ROOT_DIR_ID, &format!("Shelf {index}")).unwrap();
    }
    let through = db.sync_outbox_watermark().unwrap();
    let native = db.sync_publishable_mutations().unwrap();
    assert!(native.len() > 256);
    db.create_directory_with_id(&DirId::from_u128(351), &ROOT_DIR_ID, &"Newer edit".into()).unwrap();
    let mut after = i64::MIN;
    let mut paged = Vec::new();
    loop {
        let page = db.sync_pending_mutations_page(after, through).unwrap();
        assert!(page.len() <= 256);
        let Some(last) = page.last() else {
            break;
        };
        after = last.publisher_seq.get() as i64;
        let ids: Vec<_> = page.iter().map(|publication| publication.publisher_seq).collect();
        paged.extend(page);
        db.sync_acknowledge_mutations(&ids).unwrap();
    }
    assert_eq!(paged, native);
    let remaining = db.sync_publishable_mutations().unwrap();
    assert!(!remaining.is_empty());
    assert!(remaining.iter().all(|publication| publication.publisher_seq.get() as i64 > through));
}

#[test]
fn outbox_reader_allows_one_large_snapshot_and_continues_after_it() {
    let db = database();
    let large = "Large shelf ".repeat(32 * 1024);
    db.create_directory_with_id(&DirId::from_u128(1), &ROOT_DIR_ID, &large).unwrap();
    db.create_directory_with_id(&DirId::from_u128(2), &ROOT_DIR_ID, &"Small shelf".into()).unwrap();
    let native = db.sync_publishable_mutations().unwrap();
    let through = db.sync_outbox_watermark().unwrap();
    let mut paged = Vec::new();
    let mut after = i64::MIN;
    loop {
        let page = db.sync_pending_mutations_page(after, through).unwrap();
        let Some(last) = page.last() else {
            break;
        };
        after = last.publisher_seq.get() as i64;
        let bytes: usize = page.iter().map(|publication| sync_common::wire::encode(&publication.state.value).unwrap().len()).sum();
        assert!(bytes <= 256 * 1024 || page.len() == 1);
        paged.extend(page);
    }
    assert_eq!(paged, native);
}

#[test]
fn prepared_outbox_orders_lifecycles_and_survives_acknowledgements_and_new_edits() {
    let db = database();
    db.set_asset_storage_policy(false, false).unwrap();
    for index in 1..=350 {
        db.create_directory_with_id(&DirId::from_u128(index), &ROOT_DIR_ID, &format!("Shelf {index}")).unwrap();
    }
    let hash = import_book(&db, ROOT_DIR_ID, 1, "book.epub");
    complete_no_match(&db, &hash);
    let mut expected = db.sync_publishable_mutations().unwrap();
    expected.sort_by_key(|p| !matches!(p.state.value, LibraryStateEntry::BookLifecycle { .. }));
    let mut scan = db.sync_prepare_outbox().unwrap();
    db.create_directory_with_id(&DirId::from_u128(351), &ROOT_DIR_ID, &"Later shelf".into()).unwrap();
    let mut publications = Vec::new();
    loop {
        let page = db.sync_next_outbox_page(&mut scan).unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 256);
        let ids: Vec<_> = page.iter().map(|p| p.publisher_seq).collect();
        publications.extend(page);
        db.sync_acknowledge_mutations(&ids).unwrap();
    }
    assert_eq!(publications, expected);
    assert!(!db.sync_publishable_mutations().unwrap().is_empty());
    assert!(db.sync_next_outbox_page(&mut scan).unwrap().is_empty());
}

fn import_book(database: &Database, folder: DirId, number: u64, name: &str) -> sync_common::BookId {
    let hash = sync_common::BookId::new(&format!("{number:064x}"));
    database
        .commit_import(library_database::ImportCommit {
            parent_id: folder,
            file_name: name.into(),
            format: book_model::BookFormat::Epub,
            content_hash: hash,
            size_bytes: 100,
            inspection_version: 1,
            inspection: Some(book_inspection::InspectedBook {
                pdf: None,
                audiobook: None,
                metadata: book_model::BookRecord { metadata: book_model::BookDetails { title: format!("Book {number}"), ..Default::default() }, description: "Description".into() },
                toc: vec![],
            }),
            published: library_model::PublishedStagedBook { name: name.into(), relative_path: format!("/{name}"), published: false, fingerprint: None },
            request_thumbnail: false,
            restore_paths: vec![],
        })
        .unwrap();
    hash
}

fn acknowledge(database: &Database) {
    let ids = database.sync_publishable_mutations().unwrap().iter().map(|p| p.publisher_seq).collect::<Vec<_>>();
    database.sync_acknowledge_mutations(&ids).unwrap();
}

#[test]
fn reader_metadata_imports_publish_complete_documents_once_and_skip_identical_reinspection() {
    let database = database();
    let hash = sync_common::BookId::new(&format!("{:064x}", 1));
    let pdf = pdf_view_common::PdfReaderMetadata {
        version: 1,
        content_hash: hash.to_string(),
        pages: vec![pdf_view_common::PdfStoredPage { width: 600.0, height: 800.0, rotation: 0 }],
        skippable_pages: None,
        annotations: vec![],
        dependencies: None,
    };
    let pdf_import = || library_database::ImportCommit {
        parent_id: ROOT_DIR_ID,
        file_name: "book.pdf".into(),
        format: book_model::BookFormat::Pdf,
        content_hash: hash,
        size_bytes: 100,
        inspection_version: 1,
        inspection: Some(book_inspection::InspectedBook {
            pdf: Some(pdf.clone()),
            audiobook: None,
            metadata: book_model::BookRecord { metadata: book_model::BookDetails { title: "Reader metadata".into(), ..Default::default() }, description: String::new() },
            toc: vec![book_model::BookTocEntry { title: "Page 1".into(), target: "page:1".into(), children: vec![] }],
        }),
        published: library_model::PublishedStagedBook { name: "book.pdf".into(), relative_path: "/book.pdf".into(), published: false, fingerprint: None },
        request_thumbnail: false,
        restore_paths: vec![],
    };
    database.commit_import(pdf_import()).unwrap();
    complete_no_match(&database, &hash);
    assert_eq!(database.pdf_metadata(&hash).unwrap(), Some(pdf.clone()));
    assert_eq!(database.sync_publishable_mutations().unwrap().iter().filter(|p| matches!(p.state.value, LibraryStateEntry::BookAdd { ref value, .. } if value.pdf.is_some())).count(), 1);
    acknowledge(&database);
    database.commit_import(pdf_import()).unwrap();
    complete_no_match(&database, &hash);
    assert!(database.sync_publishable_mutations().unwrap().is_empty());

    let hash = sync_common::BookId::new(&format!("{:064x}", 2));
    let tracks = vec![book_model::AudiobookTrack { name: "chapter.mp3".into(), start_ms: 0, end_ms: 1000, offset: 0, length: 100, archive_hash: Some(hash) }];
    let metadata = book_inspection::AudiobookMetadata {
        title: "Audio".into(),
        authors: vec![],
        description: String::new(),
        duration_ms: 1000,
        chapters: vec![book_model::AudiobookChapter { title: "Chapter".into(), start_ms: 0, end_ms: 1000 }],
        tracks: tracks.clone(),
        chapter_origin: book_model::ChapterOrigin::TrackFiles,
        recording: Default::default(),
    };
    let audio_import = || library_database::ImportCommit {
        parent_id: ROOT_DIR_ID,
        file_name: "audio.zip".into(),
        format: book_model::BookFormat::Mp3Folder,
        content_hash: hash,
        size_bytes: 100,
        inspection_version: 1,
        inspection: Some(book_inspection::InspectedBook {
            pdf: None,
            audiobook: Some(metadata.clone()),
            metadata: book_model::BookRecord { metadata: book_model::BookDetails { title: "Reader metadata".into(), ..Default::default() }, description: String::new() },
            toc: vec![],
        }),
        published: library_model::PublishedStagedBook { name: "audio.zip".into(), relative_path: "/audio.zip".into(), published: false, fingerprint: None },
        request_thumbnail: false,
        restore_paths: vec![],
    };
    database.commit_import(audio_import()).unwrap();
    complete_no_match(&database, &hash);
    let docs = database.sync_publishable_mutations().unwrap();
    let docs = docs
        .iter()
        .filter_map(|p| match &p.state.value {
            LibraryStateEntry::BookAdd { content_hash, value } if *content_hash == hash => value.navigation.as_ref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].duration_ms, Some(1000));
    assert_eq!(docs[0].tracks.as_ref(), Some(&tracks));
    assert_eq!(database.audiobook_tracks(&hash).unwrap(), tracks);
    acknowledge(&database);
    database.commit_import(audio_import()).unwrap();
    complete_no_match(&database, &hash);
    assert!(database.sync_publishable_mutations().unwrap().is_empty());
    let mut changed_import = audio_import();
    let changed = changed_import.inspection.as_mut().unwrap().audiobook.as_mut().unwrap();
    changed.duration_ms = 2000;
    changed.tracks[0].end_ms = 2000;
    changed.chapters[0].end_ms = 2000;
    changed.recording.narrators = vec!["Changed narrator".into()];
    database.commit_import(changed_import).unwrap();
    assert!(database.sync_publishable_mutations().unwrap().is_empty());
    assert_eq!(database.audiobook_tracks(&hash).unwrap(), tracks);
    let playback = database.audiobook_playback_metadata(&hash).unwrap().unwrap();
    assert_eq!(playback.duration_ms, 1000);
    assert_eq!(playback.narrator, None);
}

#[test]
fn offline_addition_waits_for_enrichment_then_publishes_one_complete_document() {
    use book_model::{AUTHOR_MARC_RELATOR_CODE, Contributor};
    use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};
    let source = database();
    let target = database();
    let hash = import_book(&source, ROOT_DIR_ID, 7, "book.epub");
    assert_eq!(source.browse_book_detail(&hash).unwrap().book.title, "Book 7");
    source.update_reading_position(&hash, "epubcfi(/6/2)", Some(0.25), None).unwrap();
    assert!(source.sync_publishable_mutations().unwrap().is_empty());
    let candidate = source.enrichment_candidates(&[hash], 0).unwrap().pop().unwrap();
    let mut record = candidate.record;
    record.title = "Provider title".into();
    record.description = "Provider description".into();
    record.book.year = Some(2005);
    record.contributors = vec![Contributor::new("Enriched author", AUTHOR_MARC_RELATOR_CODE).unwrap()];
    let answer = BookEnrichment {
        key: String::new(),
        status: BookEnrichmentStatus::Found,
        record: Some(record),
        toc: None,
        popularity: Some(metadata_contract::WorkPopularity { open_library_work_id: "OL10W".into(), rating_count: 12, snapshot_date: "2026-09-01".into() }),
        cover: Some(metadata_contract::books::CoverSource { id: "cover-7".into() }),
        detail: String::new(),
    };
    source.record_enrichment(&hash, &candidate.request_key, &answer, 0).unwrap();
    let additions = source.sync_publishable_mutations().unwrap();
    let finalized = additions
        .iter()
        .find_map(|publication| match &publication.state.value {
            LibraryStateEntry::BookAdd { value, .. } => Some(value),
            _ => None,
        })
        .unwrap();
    assert_eq!(finalized.record.title, "Book 7");
    assert_eq!(finalized.record.description, "Description");
    assert_eq!(finalized.record.contributors[0].name(), "Enriched author");
    assert_eq!(finalized.record.book.year, Some(2005));
    assert_eq!(additions.iter().filter(|p| matches!(p.state.value, LibraryStateEntry::BookAdd { .. })).count(), 1);
    assert!(additions.iter().any(|p| matches!(p.state.value, LibraryStateEntry::ReadingPosition { .. })));
    pull(&target, additions.iter().map(delivery).collect());
    assert_eq!(target.browse_book_detail(&hash).unwrap().book.author, "Enriched author");
    assert_eq!(target.browse_book_detail(&hash).unwrap().book.title, "Book 7");
    assert_eq!(target.browse_book_detail(&hash).unwrap().year, Some(2005));
    assert_eq!(target.reading_position(&hash).unwrap().as_deref(), Some("epubcfi(/6/2)"));
    assert!(target.enrichment_candidates(&[hash], i64::MAX).unwrap().is_empty());
    assert_eq!(target.enrichment_cover_source(&hash).unwrap().unwrap().id, "cover-7");
    assert_eq!(target.enrichment_next_retry().unwrap(), None);
    assert_eq!(target.enrichment_popularity(&hash).unwrap(), answer.popularity);
    assert_eq!(source.enrichment_popularity(&hash).unwrap(), answer.popularity);
}

fn complete_no_match(database: &Database, hash: &sync_common::BookId) {
    use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};
    let Some(candidate) = database.enrichment_candidates(&[*hash], 0).unwrap().pop() else { return };
    database.record_enrichment(hash, &candidate.request_key, &BookEnrichment { key: String::new(), status: BookEnrichmentStatus::NoMatch, record: None, toc: None, popularity: None, cover: None, detail: String::new() }, 0).unwrap();
}

#[cfg(feature = "scanner")]
#[test]
fn verified_audio_download_reuses_complete_synced_metadata_without_local_inspection() {
    use book_inspection::{AudiobookMetadata, InspectedBook};
    use book_model::{AudiobookChapter, AudiobookTrack, BookDetails, BookFormat, BookRecord, ChapterOrigin};
    use library_database::{ImportCommit, NativeBookAcknowledgement, NativeBookCompletion, NativeBookSnapshot};
    use library_model::PublishedStagedBook;

    for (format, complete) in [(BookFormat::M4b, true), (BookFormat::Mp3Folder, true), (BookFormat::Mp3Folder, false)] {
        let source = database();
        let path = std::env::temp_dir().join(format!("synced-audio-cache-{}.db", uuid::Uuid::new_v4()));
        let target = Database::open(&path).unwrap();
        target.initialize_library().unwrap();
        let hash = sync_common::BookId::new(&format!("{:064x}", 42));
        let tracks = if format == BookFormat::Mp3Folder && complete { vec![AudiobookTrack { name: "01.mp3".into(), start_ms: 0, end_ms: 1000, offset: 0, length: 100, archive_hash: Some(hash) }] } else { vec![] };
        source
            .commit_import(ImportCommit {
                parent_id: ROOT_DIR_ID,
                file_name: "Audio".into(),
                format,
                content_hash: hash,
                size_bytes: 100,
                inspection_version: book_inspection::inspection_version(format),
                inspection: Some(InspectedBook {
                    pdf: None,
                    toc: vec![],
                    metadata: BookRecord { metadata: BookDetails { title: "Audio".into(), ..Default::default() }, description: String::new() },
                    audiobook: Some(AudiobookMetadata {
                        title: "Audio".into(),
                        authors: vec![],
                        description: String::new(),
                        duration_ms: 1000,
                        chapters: vec![AudiobookChapter { title: "Chapter".into(), start_ms: 0, end_ms: 1000 }],
                        tracks,
                        chapter_origin: ChapterOrigin::FullAudiobookFallback,
                        recording: Default::default(),
                    }),
                }),
                published: PublishedStagedBook { name: "Audio".into(), relative_path: "/Audio".into(), published: false, fingerprint: None },
                request_thumbnail: false,
                restore_paths: vec![],
            })
            .unwrap();
        complete_no_match(&source, &hash);
        pull(&target, source.sync_publishable_mutations().unwrap().iter().map(delivery).collect());
        let metadata = target.audiobook_playback_metadata(&hash).unwrap().unwrap();
        assert_eq!(metadata.duration_ms, 1000);
        assert_eq!(metadata.chapters.len(), 1);
        let expected_titles = if format == BookFormat::Mp3Folder && complete { vec![("01.mp3".to_owned(), "Chapter".to_owned())] } else { vec![] };
        assert_eq!(target.audiobook_download_track_titles(&hash).unwrap(), expected_titles);
        let transfer = target.transfer_snapshot(&[hash]).unwrap();
        assert_eq!(transfer.placements[0].book.as_ref().unwrap().audiobook_track_titles, expected_titles);
        assert!(!target.has_import_inspection(&hash, format, book_inspection::inspection_version(format)).unwrap());
        assert!(target.scan_snapshot().unwrap().books.is_empty(), "synced membership alone does not verify local bytes");

        let relative_path = target.book_paths(&hash).unwrap().remove(0);
        let name = relative_path.file_name().to_owned();
        assert_eq!(
            target
                .complete_downloaded_book(
                    NativeBookCompletion {
                        snapshot: NativeBookSnapshot { content_hash: hash, directory: ROOT_DIR_ID, name: name.clone(), relative_path: relative_path.clone(), occupied_names: vec![], format, audiobook_track_titles: vec![] },
                        publication: PublishedStagedBook { name, relative_path: relative_path.to_string(), published: true, fingerprint: Some("42".into()) },
                        size_bytes: 100,
                        provider_file: None,
                    },
                    true
                )
                .unwrap(),
            NativeBookAcknowledgement::Applied
        );
        let expected = complete.then_some(format);
        let snapshot = target.scan_snapshot().unwrap();
        assert_eq!(snapshot.books.len(), 1);
        assert_eq!(snapshot.books[0].fingerprint, 42);
        assert_eq!(snapshot.books[0].inspection_version, None, "reusing synced facts does not fabricate a local inspection");
        assert_eq!(snapshot.books[0].cached_audiobook_format, expected);
        drop(target);

        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.scan_snapshot().unwrap().books[0].cached_audiobook_format, expected);
        let retained = reopened.audiobook_playback_metadata(&hash).unwrap().unwrap();
        assert_eq!(retained.duration_ms, metadata.duration_ms);
        assert_eq!(retained.chapters, metadata.chapters);
        assert_eq!(retained.tracks, metadata.tracks);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn a_shared_no_match_finishes_enrichment_and_a_late_answer_cannot_replace_it() {
    use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};

    let source = database();
    let target = database();
    let hash = import_book(&source, ROOT_DIR_ID, 8, "book.epub");
    pull(&target, source.sync_publishable_mutations().unwrap().iter().map(delivery).collect());
    acknowledge(&source);
    let candidate = source.enrichment_candidates(&[hash], 0).unwrap().pop().unwrap();
    let mut answer = BookEnrichment { key: String::new(), status: BookEnrichmentStatus::NoMatch, record: None, toc: None, popularity: None, cover: None, detail: String::new() };
    source.record_enrichment(&hash, &candidate.request_key, &answer, 0).unwrap();
    let original = source.sync_publishable_mutations().unwrap();
    pull(&target, original.iter().map(delivery).collect());
    answer.status = BookEnrichmentStatus::Found;
    answer.record = Some(candidate.record);
    source.record_enrichment(&hash, &candidate.request_key, &answer, 1).unwrap();
    assert_eq!(source.sync_publishable_mutations().unwrap(), original);
    pull(&target, original.iter().map(delivery).collect());
    assert!(target.enrichment_candidates(&[hash], i64::MAX).unwrap().is_empty());
}

#[test]
fn server_accepted_addition_replaces_a_competing_local_addition_even_with_an_older_version() {
    use book_model::{BookFormat, BookRecord};
    use library_replica::BookAddition;

    let source = database();
    let hash = import_book(&source, ROOT_DIR_ID, 9, "book.epub");
    complete_no_match(&source, &hash);
    let original = source.sync_publishable_mutations().unwrap();
    let candidate = original.iter().find(|p| matches!(p.state.value, LibraryStateEntry::BookAdd { .. })).unwrap();
    assert_eq!(candidate.state.version.changed_at, 0);
    let accepted = LibraryStateEntry::BookAdd {
        content_hash: hash,
        value: BookAddition {
            facts: library_replica::BookFacts { added_at: 1, format: BookFormat::Epub },
            record: BookRecord { metadata: library_replica::BookDetails { title: "Accepted".into(), ..Default::default() }, description: String::new() },
            navigation: None,
            pdf: None,
            recording: None,
            enrichment: Some(library_replica::EnrichmentResult::NoMatch),
        },
    }
    .publication(ReplicaId::from_u128(0), 0, ReplicaSeq::new(1).unwrap());
    assert!(accepted.state.version < candidate.state.version);
    pull(&source, vec![delivery(&accepted)]);
    assert_eq!(source.browse_book_detail(&hash).unwrap().book.title, "Accepted");
    let queued = source.sync_publishable_mutations().unwrap();
    let forwarded = queued.iter().find(|p| matches!(p.state.value, LibraryStateEntry::BookAdd { .. })).unwrap();
    assert_eq!(forwarded.state, accepted.state);
    assert_ne!(forwarded.publisher_seq, candidate.publisher_seq);
    source.sync_acknowledge_mutations(&original.iter().map(|p| p.publisher_seq).collect::<Vec<_>>()).unwrap();
    assert_eq!(source.sync_publishable_mutations().unwrap(), vec![forwarded.clone()]);
    assert_eq!(source.sync_enqueue_missing_state_cells(&[accepted.state.value.address()]).unwrap(), 0);
    acknowledge(&source);
    assert_eq!(source.sync_enqueue_missing_state_cells(&[accepted.state.value.address()]).unwrap(), 1);
    assert_eq!(source.sync_publishable_mutations().unwrap()[0].state, accepted.state);
}

#[test]
fn offline_draft_and_reading_progress_survive_reopening_before_enrichment() {
    let path = std::env::temp_dir().join(format!("book-draft-{}.sqlite3", uuid::Uuid::new_v4()));
    let source = Database::open(&path).unwrap();
    source.initialize_library().unwrap();
    source.set_asset_storage_policy(false, false).unwrap();
    let hash = import_book(&source, ROOT_DIR_ID, 10, "pending.epub");
    source.update_reading_position(&hash, "epubcfi(/6/2)", Some(0.5), None).unwrap();
    assert!(source.sync_publishable_mutations().unwrap().is_empty());
    drop(source);
    let source = Database::open(&path).unwrap();
    source.initialize_library().unwrap();
    assert_eq!(source.browse_book_detail(&hash).unwrap().book.title, "Book 10");
    assert_eq!(source.reading_position(&hash).unwrap().as_deref(), Some("epubcfi(/6/2)"));
    assert!(source.sync_publishable_mutations().unwrap().is_empty());
    complete_no_match(&source, &hash);
    let publications = source.sync_publishable_mutations().unwrap();
    assert_eq!(publications.iter().filter(|p| matches!(p.state.value, LibraryStateEntry::BookAdd { .. })).count(), 1);
    assert!(publications.iter().any(|p| matches!(p.state.value, LibraryStateEntry::ReadingPosition { .. })));
    drop(source);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn annotations_round_trip_register_projection_sync_and_worker_transport() {
    use book_model::{Annotation, AnnotationAnchor, AnnotationStyle, PdfAnnotationAnchor, PdfAnnotationRect};
    use client_runtime::wire::{decode_worker_message, encode_worker_message};

    let source = database();
    let target = database();
    source.set_asset_storage_policy(false, false).unwrap();
    target.set_asset_storage_policy(false, false).unwrap();
    let hash = import_book(&source, ROOT_DIR_ID, 100, "Annotated.epub");
    import_book(&target, ROOT_DIR_ID, 100, "Annotated.epub");
    complete_no_match(&source, &hash);
    complete_no_match(&target, &hash);
    acknowledge(&source);
    acknowledge(&target);
    for (index, anchor) in [AnnotationAnchor::epub_cfi("epubcfi(/6/2)"), AnnotationAnchor::Pdf(PdfAnnotationAnchor::new(3, vec![PdfAnnotationRect::new(0.1, 0.2, 0.3, 0.4)], None))].into_iter().enumerate() {
        let annotation = Annotation {
            id: format!("note-{index}"),
            content_hash: hash,
            anchor,
            exact_text: "Text".into(),
            style: AnnotationStyle::Underline,
            color: 0xffffff,
            note: "note".into(),
            created_at: 7,
            toc_ordinal: Some(5),
            progress: Some(42.5),
            modified_at: 9,
            deleted: false,
        };
        source.upsert_annotation(&annotation).unwrap();
    }
    let publications = source.sync_publishable_mutations().unwrap();
    assert_eq!(publications.len(), 2);
    pull(&target, publications.iter().map(delivery).collect());
    let source_rows = source.annotations(hash).unwrap();
    let target_rows = target.annotations(hash).unwrap();
    assert_eq!(target_rows.len(), 2);
    for (source_row, target_row) in source_rows.iter().zip(&target_rows) {
        assert_eq!(source_row, target_row);
    }
    let message = encode_worker_message(&target_rows).unwrap();
    let decoded: Vec<Annotation> = decode_worker_message(&message).unwrap();
    assert_eq!(decoded, target_rows);
}
