use super::*;
use crate::navigation::LibrarySession;
use gpui::{TestAppContext, VisualTestContext, WindowHandle, size};
use library_model::{DownloadProgress, LibraryFileTypeFilter};
use std::cell::{Cell, RefCell};

struct CardFixture {
    views: [Entity<BookCardElement>; 2],
    first: BookCardInteraction,
    width: Pixels,
}

impl Render for CardFixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().children(self.views.iter().enumerate().map(|(index, view)| {
            let interaction = if index == 0 { self.first } else { BookCardInteraction::default() };
            div().id(index).w(self.width).h(px(300.0)).child(BookCardView::present(view.clone(), interaction, window, cx))
        }))
    }
}

fn draw(window: WindowHandle<CardFixture>, cx: &TestAppContext) {
    VisualTestContext::from_window(window.into(), cx).update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn row() -> BookCardRow {
    BookCardRow {
        source: BookSource::Library { content_hash: BookId::from_bytes([7; 32]) },
        title: "A book title".into(),
        subtitle: Some("A subtitle".into()),
        author: "Writer".into(),
        progress: 0.0,
        downloaded: true,
        download_requested: false,
        format_category: LibraryFileTypeFilter::Book,
        audiobook_duration_ms: None,
        added_at: 0,
        source_directory: None,
    }
}

struct CoverWindowFixture {
    view: Entity<BookCardElement>,
    shown: bool,
}

impl Render for CoverWindowFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().when(self.shown, |root| root.child(self.view.clone().cached(StyleRefinement::default().size_full())))
    }
}

#[gpui::test]
async fn reader_entry_releases_hidden_covers_without_resetting_another_browse_window(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let mut cards = Vec::new();
    let mut images = Vec::new();
    for index in 0..3 {
        let book = BookCardRow { source: BookSource::Library { content_hash: BookId::from_bytes([index + 1; 32]) }, ..row() };
        let card = cx.new(|cx| BookCardView::new(book, library.clone(), Rc::new(|_, _, _| {}), false, false, cx));
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(2, 3))]));
        images.push(Arc::downgrade(&image));
        card.update(cx, |card, cx| {
            card.remember_cover(image, ThumbnailResolution::CardHighDensity, cx);
            card.cover_loaded_at = Some(Instant::now() - Duration::from_secs(1));
            card.load_cover_from(ThumbnailResolution::CardHighDensity, std::future::pending(), cx);
        });
        cards.push(card);
    }
    let outgoing_view = cx.update(|cx| BookCardView::presentation(cards[0].clone(), BookCardPresentation::Grid, cx));
    let other_view = cx.update(|cx| BookCardView::presentation(cards[1].clone(), BookCardPresentation::Grid, cx));
    let outgoing = cx.open_window(size(px(240.0), px(320.0)), |_, _| CoverWindowFixture { view: outgoing_view, shown: true });
    let other = cx.open_window(size(px(240.0), px(320.0)), |_, _| CoverWindowFixture { view: other_view.clone(), shown: true });
    let mut outgoing_visual = VisualTestContext::from_window(outgoing.into(), cx);
    let mut other_visual = VisualTestContext::from_window(other.into(), cx);
    for visual in [&mut outgoing_visual, &mut other_visual] {
        visual.update(|window, cx| { let _ = window.draw(cx); });
    }
    let before = cards[1].read_with(cx, |card, _| card.cover_loaded_at);
    let renders = other_view.read_with(cx, |view, _| view.renders);
    outgoing.update(cx, |view, window, cx| {
        view.shown = false;
        crate::evict_covers(window);
        cx.notify();
    }).unwrap();

    for _ in 0..4 {
        for visual in [&mut outgoing_visual, &mut other_visual] {
            visual.update(|window, cx| {
                let _ = window.draw(cx);
                window.simulate_next_frame(cx);
            });
        }
        cx.run_until_parked();
    }

    for index in [0, 2] {
        cards[index].read_with(cx, |card, _| {
            assert!(matches!(card.cover, CoverState::Pending));
            assert!(card.cover_task.is_none(), "hidden cover requests must be cancelled");
        });
        assert!(images[index].upgrade().is_none(), "hidden pixels must be released below the byte budget");
    }
    cards[1].read_with(cx, |card, _| {
        let displayed = card.cover.loaded().expect("the other window must keep its cover");
        assert!(Arc::ptr_eq(&displayed, &images[1].upgrade().unwrap()));
        assert_eq!(card.cover_loaded_at, before, "reader entry must not restart a cover fade");
        assert!(card.cover_task.is_some(), "displayed cover requests must keep running");
    });
    cx.update(|cx| {
        let key = cards[1].read(cx).cover_cache_key();
        let cached = cx.global_mut::<BrowseCovers>().get(&key).unwrap().0;
        assert!(Arc::ptr_eq(&cached, &images[1].upgrade().unwrap()), "displayed images must keep their shared lookup");
    });
    assert_eq!(other_view.read_with(cx, |view, _| view.renders), renders, "another browse window must keep its cached presentation");
}

#[gpui::test]
async fn cover_loads_decode_and_keep_existing_art_with_source_specific_retries(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg).encode(&[10, 20, 30], 1, 1, image::ExtendedColorType::Rgb8).unwrap();
    let sources = [
        row().source,
        BookSource::Catalog { content_hash: row().content_hash().unwrap() },
        BookSource::Opds { source: Uuid::new_v4(), entry: CardKey([2; 32]), downloadable: true, buyable: false },
        BookSource::ExternalCatalog { entry: CardKey([3; 32]), downloadable: true },
    ];
    for source in sources {
        for (result, keep_visible) in [(Ok(Some(jpeg.clone())), false), (Ok(None), true), (Ok(Some(b"broken JPEG".to_vec())), true), (Err(BackendError::message("offline")), false)] {
            let succeeded = result.as_ref().is_ok_and(|bytes| bytes.as_ref().is_some_and(|bytes| bytes == &jpeg));
            let card = cx.new(|cx| BookCardView::new(BookCardRow { source, ..row() }, library.clone(), Rc::new(|_, _, _| {}), false, false, cx));
            let retained = Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(2, 3))]));
            card.update(cx, |card, cx| {
                if keep_visible {
                    card.cover = CoverState::Loaded(Arc::downgrade(&retained), ThumbnailResolution::Browse);
                    card.cover_loaded_at = Some(Instant::now());
                }
                card.load_cover_from(ThumbnailResolution::CardHighDensity, async move { result }, cx);
            });
            let fade_started = card.read_with(cx, |card, _| card.cover_loaded_at);
            cx.condition(&card, |card, _| card.cover_task.is_none()).await;
            card.read_with(cx, |card, _| {
                if succeeded {
                    assert!(matches!(card.cover, CoverState::Loaded(_, ThumbnailResolution::CardHighDensity)));
                    assert!(card.cover_updates.is_none());
                } else {
                    assert_eq!(card.cover_updates.is_some(), matches!(source, BookSource::Library { .. } | BookSource::Catalog { .. }));
                    if keep_visible {
                        assert!(Arc::ptr_eq(&card.cover.loaded().unwrap(), &retained));
                        assert_eq!(card.cover_loaded_at, fade_started);
                    } else {
                        assert!(matches!(card.cover, CoverState::Unavailable));
                    }
                }
            });
        }
    }
}

#[gpui::test]
async fn late_cover_reads_keep_denser_shared_art_and_the_original_fade(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let first = cx.new(|cx| BookCardView::new(row(), library.clone(), Rc::new(|_, _, _| {}), false, false, cx));
    let second = cx.new(|cx| BookCardView::new(row(), library, Rc::new(|_, _, _| {}), false, false, cx));
    let image = || Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(2, 3))]));
    let (browse, dense, late) = (image(), image(), image());
    let started = Instant::now() - Duration::from_secs(1);
    first.update(cx, |card, cx| {
        card.remember_cover(browse, ThumbnailResolution::Browse, cx);
        card.cover_loaded_at = Some(started);
    });
    second.update(cx, |card, cx| card.remember_cover(dense.clone(), ThumbnailResolution::CardHighDensity, cx));
    first.update(cx, |card, cx| card.remember_cover(late.clone(), ThumbnailResolution::Browse, cx));
    first.read_with(cx, |card, _| {
        assert!(Arc::ptr_eq(&card.cover.loaded().unwrap(), &dense), "a late thumbnail must use the denser shared cover");
        assert!(matches!(card.cover, CoverState::Loaded(_, ThumbnailResolution::CardHighDensity)));
        assert_eq!(card.cover_loaded_at, Some(started), "upgrading shared art must not restart the fade");
    });
    second.update(cx, |card, cx| card.remember_cover(late, ThumbnailResolution::CardHighDensity, cx));
    second.read_with(cx, |card, _| assert!(Arc::ptr_eq(&card.cover.loaded().unwrap(), &dense), "the first completed read at a density keeps its shared image"));
}

#[gpui::test]
async fn placement_commands_preserve_folder_sources_and_flat_selection_order(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let received = Rc::new(RefCell::new(Vec::new()));
    let requests = received.clone();
    let events: BookCardEvents = Rc::new(move |event, _, _| {
        let BookCardEvent::Place(books, source, placement) = event else { panic!("expected a placement request") };
        requests.borrow_mut().push((books, source, placement));
    });
    let source = Uuid::new_v4();
    let book = BookCardRow { source_directory: Some(source), ..row() };
    let hash = book.library_file().unwrap();
    let window = cx.add_window(|_, _| gpui::Empty);
    let mut cards = Vec::new();
    for folder_commands in [true, false] {
        let card = cx.new(|cx| BookCardView::new(book.clone(), library.clone(), events.clone(), folder_commands, !folder_commands, cx));
        window.update(cx, |_, window, cx| BookCardView::run_overlay_command(&card, OverlayCommand::CopyToFolder, window, cx)).unwrap();
        if folder_commands {
            window.update(cx, |_, window, cx| BookCardView::run_overlay_command(&card, OverlayCommand::MoveToFolder, window, cx)).unwrap();
        }
        cards.push(card);
    }
    let second = BookId::from_bytes([8; 32]);
    let other = cx.new(|cx| BookCardView::new(BookCardRow { source: BookSource::Library { content_hash: second }, ..book }, library, events, false, true, cx));
    let copy = cx.update(|cx| BookCardView::selection_actions(&[cards[1].clone(), other, cards[1].clone()], cx).into_iter().find(|action| action.id == "copy").unwrap());
    window.update(cx, |_, window, cx| copy.run(window, cx)).unwrap();
    assert_eq!(*received.borrow(), [(vec![hash], Some(source), FolderPlacement::Copy), (vec![hash], Some(source), FolderPlacement::Move), (vec![hash], None, FolderPlacement::Copy), (vec![hash, second], None, FolderPlacement::Copy),]);
}

#[gpui::test]
async fn import_feedback_replaces_progress_and_marks_only_successful_adds(cx: &mut TestAppContext) {
    use crate::services::import_catalog_book_with_progress;
    use gpui_component::Root;
    use std::slice::from_ref;

    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let book = row();
    let hash = book.library_file().unwrap();
    let cached = cx.new(|cx| BookCardView::new(BookCardRow { source: BookSource::Catalog { content_hash: hash }, ..book.clone() }, library.clone(), Rc::new(|_, _, _| {}), false, false, cx));
    assert!(!cx.update(|cx| BookCardView::selection_actions(from_ref(&cached), cx)[0].is_disabled()));
    let window = cx.add_window(|window, cx| {
        let empty = cx.new(|_| gpui::Empty);
        Root::new(empty, window, cx)
    });
    let (app, backend) = (library.app().clone(), library.backend().clone());
    let task = window.update(cx, |_, window, cx| window.spawn(cx, async move |cx| import_catalog_book_with_progress(&app, &backend, book, ROOT_DIR_ID, Some("Added to library"), cx).await)).unwrap();
    assert_eq!(task.await.unwrap(), hash);
    window.update(cx, |root, _, cx| assert_eq!(root.notification.read(cx).notifications().len(), 1, "completion replaces its progress toast")).unwrap();
    assert!(cx.update(|cx| BookCardView::selection_actions(from_ref(&cached), cx)[0].is_disabled()), "an added book must no longer be offered by a cached catalog card");

    let unavailable = BookCardRow { source: BookSource::ExternalCatalog { entry: CardKey([12; 32]), downloadable: false }, ..row() };
    let key = unavailable.key();
    let (app, backend) = (library.app().clone(), library.backend().clone());
    let task = window.update(cx, |_, window, cx| window.spawn(cx, async move |cx| import_catalog_book_with_progress(&app, &backend, unavailable, ROOT_DIR_ID, None, cx).await)).unwrap();
    assert!(task.await.is_err());
    cx.update(|cx| assert!(!feed_book_added(key, cx), "failed imports must stay available for retry"));
    window.update(cx, |root, _, cx| assert_eq!(root.notification.read(cx).notifications().len(), 2, "failure replaces its own progress toast")).unwrap();
}

#[gpui::test]
async fn menu_commands_retain_their_error_target_until_the_backend_finishes(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let card = cx.new(|cx| BookCardView::new(row(), library, Rc::new(|_, _, _| {}), false, false, cx));
    let weak = card.downgrade();
    let (released, release) = async_channel::bounded(1);
    let _subscription = cx.update(|cx| {
        cx.observe_release(&card, move |_, _| {
            let _ = released.try_send(());
        })
    });
    cx.update(|cx| BookCardView::run_menu_action(card, BookCommand::RemoveDownload, vec![row().library_file().unwrap()], cx));
    assert!(weak.upgrade().is_some(), "closing a card must not cancel its error-reporting target");
    release.recv().await.unwrap();
    assert!(weak.upgrade().is_none(), "the completed command must release its card");
}

#[gpui::test]
async fn replacing_a_menu_disconnects_the_previous_dismiss_subscription(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let card = cx.new(|cx| BookCardView::new(row(), library, Rc::new(|_, _, _| {}), false, false, cx));
    let window = cx.add_window(|_, _| gpui::Empty);
    let mut menus = Vec::new();
    for _ in 0..2 {
        menus.push(
            window
                .update(cx, |_, window, cx| {
                    card.update(cx, |card, cx| {
                        card.open_books_menu(vec![card.book.clone()], Point::default(), window, cx);
                        card.context_menu.as_ref().unwrap().menu.clone()
                    })
                })
                .unwrap(),
        );
    }
    menus[0].update(cx, |_, cx| cx.emit(DismissEvent));
    card.read_with(cx, |card, _| assert_eq!(card.context_menu.as_ref().unwrap().menu, menus[1]));
    menus[1].update(cx, |_, cx| cx.emit(DismissEvent));
    card.read_with(cx, |card, _| assert!(card.context_menu.is_none()));
}

#[gpui::test]
async fn shared_book_commands_preserve_other_placements_and_stop_on_failed_eviction(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let backend = library.backend();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/library-backend/crates/book-inspection/tests/fixtures");
    assert!(backend.import_paths(ROOT_DIR_ID, ["back-matter-evidence.epub", "ocr-image-priority.epub"].map(|name| fixtures.join(name)).into(), false).await.unwrap().is_empty());
    let query = library_model::LibraryBrowseQuery {
        location: library_model::BrowseLocation::Books,
        search: String::new(),
        file_types: Vec::new(),
        chip_sort: Default::default(),
        book_sort: Default::default(),
        languages: Vec::new(),
        hide_finished: false,
    };
    let books = backend.browse_section_books(query, 0, 10).await.unwrap().books;
    assert_eq!(books.len(), 2);
    let hashes = books.iter().map(|book| book.library_file().unwrap()).collect::<Vec<_>>();
    for read in [true, false] {
        BookCommand::SetRead(read).execute(backend, hashes.clone()).await.unwrap();
        for &hash in &hashes {
            assert_eq!(backend.book_detail(hash).await.unwrap().book.progress >= FINISHED_PROGRESS_THRESHOLD, read);
        }
    }
    // Local-only books must retain their only copy when eviction fails.
    assert!(BookCommand::RemoveDownload.execute(backend, hashes.clone()).await.is_err());
    for &hash in &hashes {
        assert!(matches!(backend.download_state(hash).await.unwrap(), DownloadState::Downloaded));
    }
    backend.create_directory(ROOT_DIR_ID, "Keep".into()).await.unwrap();
    let keep = backend.folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Keep").unwrap().id;
    backend.copy_book_to_directory(hashes[0], Some(ROOT_DIR_ID), keep).await.unwrap();
    BookCommand::RemoveFromFolder(ROOT_DIR_ID).execute(backend, vec![hashes[0]]).await.unwrap();
    assert!(backend.trash().await.unwrap().books.is_empty(), "removing one placement must keep the book in its other folder");
    assert_eq!(backend.book_detail(hashes[0]).await.unwrap().source_directory_id, Some(keep.to_string()));
    BookCommand::TrashBooks.execute(backend, hashes.clone()).await.unwrap();
    assert_eq!(backend.trash().await.unwrap().books.len(), 2);
    backend.restore_book(hashes[0]).await.unwrap();
    BookCommand::TrashBook.execute(backend, vec![hashes[0]]).await.unwrap();
    assert_eq!(backend.trash().await.unwrap().books.len(), 2);
}

#[gpui::test]
async fn library_selection_download_only_enables_for_missing_books(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let available = cx.new(|cx| BookCardView::new(row(), library.clone(), Rc::new(|_, _, _| {}), false, false, cx));
    let missing_hash = BookId::from_bytes([8; 32]);
    let missing = cx.new(|cx| BookCardView::new(BookCardRow { source: BookSource::Library { content_hash: missing_hash }, downloaded: false, ..row() }, library.clone(), Rc::new(|_, _, _| {}), false, false, cx));
    let selected = [available, missing];
    assert!(!cx.update(|cx| BookCardView::download_selection_action(&selected, cx).unwrap().is_disabled()), "a mixed selection must offer Download");
    for state in [DownloadState::Queued, DownloadState::Downloading(DownloadProgress::ZERO), DownloadState::Downloaded] {
        library.updates().update(cx, |updates, cx| updates.set_download_state_for_test(missing_hash, state, cx));
        assert!(cx.update(|cx| BookCardView::download_selection_action(&selected, cx).unwrap().is_disabled()), "queued and complete books must not be queued again");
    }
    library.updates().update(cx, |updates, cx| updates.set_download_state_for_test(missing_hash, DownloadState::NotDownloaded, cx));
    assert!(!cx.update(|cx| BookCardView::download_selection_action(&selected, cx).unwrap().is_disabled()), "a removed or failed download must be available again");
    let catalog = cx.new(|cx| BookCardView::new(BookCardRow { source: BookSource::Catalog { content_hash: BookId::from_bytes([9; 32]) }, ..row() }, library, Rc::new(|_, _, _| {}), false, false, cx));
    assert!(cx.update(|cx| BookCardView::download_selection_action(&[catalog], cx).is_none()), "catalog imports use their own download operation");
}

#[gpui::test]
async fn play_opens_books_directly_and_shows_audiobook_detail_before_playback(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let window = cx.add_window(|_, _| gpui::Empty);
    let actions = Rc::new(RefCell::new(Vec::new()));
    let opened = actions.clone();
    let hash = row().library_file().unwrap();
    let library_id = *library.backend().id();
    let library = LibraryContext::new(
        Rc::new(library.app().clone()),
        Rc::new(library.backend().clone()),
        Rc::new(move |locator, title, target, _, _| {
            assert_eq!(*locator.library_id(), library_id);
            assert_eq!(locator.content_hash(), hash);
            assert_eq!(title, row().title);
            assert_eq!(target, None);
            opened.borrow_mut().push("open");
        }),
        library.updates(),
        library.session(),
        library.now_playing().clone(),
    );
    for format_category in [LibraryFileTypeFilter::Book, LibraryFileTypeFilter::Audiobook] {
        actions.borrow_mut().clear();
        let detail = actions.clone();
        let book = BookCardRow { format_category, ..row() };
        let card = cx.new(|cx| {
            BookCardView::new(
                book,
                library.clone(),
                Rc::new(move |event, _, _| {
                    let BookCardEvent::OpenDetail(book, _) = event else { panic!("expected book details") };
                    assert_eq!(book.format_category, LibraryFileTypeFilter::Audiobook);
                    detail.borrow_mut().push("detail");
                }),
                false,
                false,
                cx,
            )
        });
        window.update(cx, |_, window, cx| BookCardView::play(&card, window, cx)).unwrap();
        let expected = if format_category == LibraryFileTypeFilter::Audiobook { vec!["detail", "open"] } else { vec!["open"] };
        assert_eq!(*actions.borrow(), expected);
    }
}

#[gpui::test]
async fn cached_placements_reuse_neighbors_and_invalidate_interaction_rows_and_typography(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let queued = BookCardRow { downloaded: false, download_requested: true, ..row() };
    let card = cx.new(|cx| BookCardView::new(queued, library, Rc::new(|_, _, _| {}), false, false, cx));
    card.update(cx, |card, _| {
        card.cover = CoverState::Missing;
    });
    let views = cx.update(|cx| [BookCardView::presentation(card.clone(), BookCardPresentation::Grid, cx), BookCardView::presentation(card.clone(), BookCardPresentation::Section, cx)]);
    let window = cx.open_window(size(px(500.0), px(400.0)), |_, _| CardFixture { views: views.clone(), first: BookCardInteraction::default(), width: px(180.0) });
    draw(window, cx);
    let counts = |cx: &TestAppContext| cx.update(|cx| views.each_ref().map(|view| view.read(cx).renders));
    let initial = counts(cx);
    assert!(initial.iter().all(|&count| count > 0));
    assert_eq!(window.update(cx, |_, window, cx| window.simulate_next_frame(cx)).unwrap(), 0, "queued downloads must not animate");

    // A redraw of the parent must reuse both card subtrees.
    window.update(cx, |_, _, cx| cx.notify()).unwrap();
    draw(window, cx);
    assert_eq!(counts(cx), initial);

    // An animation/local change in one placement must leave its neighbor cached.
    views[0].update(cx, |_, cx| cx.notify());
    draw(window, cx);
    assert_eq!(counts(cx), [initial[0] + 1, initial[1]]);

    for interaction in [
        BookCardInteraction { selected: true, ..BookCardInteraction::default() },
        BookCardInteraction::default(),
        BookCardInteraction { focused: true, ..BookCardInteraction::default() },
        BookCardInteraction::default(),
        BookCardInteraction { marquee: true, ..BookCardInteraction::default() },
        BookCardInteraction::default(),
    ] {
        let before = counts(cx);
        window
            .update(cx, |fixture, _, cx| {
                fixture.first = interaction;
                cx.notify();
            })
            .unwrap();
        draw(window, cx);
        assert_eq!(counts(cx), [before[0] + 1, before[1]], "interaction must change the current frame, including toggling back");
    }

    let before = counts(cx);
    card.update(cx, |card, cx| {
        let mut row = card.book.clone();
        row.title = "Updated title".into();
        card.replace_row(row, cx);
    });
    draw(window, cx);
    assert_eq!(counts(cx), before.map(|count| count + 1), "model updates invalidate every placement of that book");

    let before = counts(cx);
    window
        .update(cx, |_, window, cx| {
            window.set_rem_size(px(24.0));
            cx.notify();
        })
        .unwrap();
    draw(window, cx);
    assert_eq!(counts(cx), before.map(|count| count + 1), "font scaling must invalidate fixed-size cards");

    let before = counts(cx);
    window
        .update(cx, |fixture, _, cx| {
            fixture.width = px(160.0);
            cx.notify();
        })
        .unwrap();
    draw(window, cx);
    assert_eq!(counts(cx), before.map(|count| count + 1), "resizing must remeasure the card");
}

#[gpui::test]
async fn progress_percentages_do_not_invalidate_cards_but_phase_changes_do(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let updates = library.updates();
    let card = cx.new(|cx| BookCardView::new(row(), library, Rc::new(|_, _, _| {}), false, false, cx));
    let notifications = Rc::new(Cell::new(0));
    let observed = notifications.clone();
    let _subscription = cx.update(|cx| cx.observe(&card, move |_, _| observed.set(observed.get() + 1)));
    let hash = row().content_hash().unwrap();
    for state in [DownloadState::Queued, DownloadState::Downloading(DownloadProgress::ZERO), DownloadState::Downloading(DownloadProgress::new(0.5).unwrap()), DownloadState::Downloaded] {
        updates.update(cx, |updates, cx| updates.set_download_state_for_test(hash, state, cx));
    }
    assert_eq!(notifications.get(), 3);
}

/// Measure presentation entities separately from book data, images and the
/// window's frame arena. Run alone with LD_PRELOAD=/usr/lib/libtcmalloc.so.
#[cfg(target_os = "linux")]
#[gpui::test]
#[ignore = "live-heap profiling; requires tcmalloc and PROFILE_CARD_COUNT"]
async fn profile_offscreen_presentation_views(cx: &mut TestAppContext) {
    use std::{
        ffi::{CStr, c_char, c_void},
        mem::size_of,
    };

    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    let symbol = unsafe { dlsym(std::ptr::null_mut(), c"MallocExtension_GetNumericProperty".as_ptr()) };
    assert!(!symbol.is_null(), "preload libtcmalloc.so for live allocation counters");
    let property: unsafe extern "C" fn(*const c_char, *mut usize) -> i32 = unsafe { std::mem::transmute(symbol) };
    let allocated = || {
        let mut bytes = 0;
        assert_ne!(unsafe { property(c"generic.current_allocated_bytes".as_ptr(), &mut bytes) }, 0);
        bytes
    };
    let heap_checkpoint = |label: &CStr| {
        if std::env::var_os("HEAPPROFILE").is_some() {
            let symbol = unsafe { dlsym(std::ptr::null_mut(), c"HeapProfilerDump".as_ptr()) };
            assert!(!symbol.is_null());
            let dump: unsafe extern "C" fn(*const c_char) = unsafe { std::mem::transmute(symbol) };
            unsafe { dump(label.as_ptr()) };
        }
    };
    let count: usize = std::env::var("PROFILE_CARD_COUNT").expect("set PROFILE_CARD_COUNT").parse().unwrap();
    assert!(count >= 2);
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    cx.update(|cx| cx.set_global(crate::stores::GeneratedCoversOnly(true)));
    let books: Vec<_> = (0..count)
        .map(|index| {
            let mut hash = [0; 32];
            hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
            cx.new(|cx| {
                BookCardView::new(BookCardRow { source: BookSource::Library { content_hash: BookId::from_bytes(hash) }, downloaded: false, download_requested: true, ..row() }, library.clone(), Rc::new(|_, _, _| {}), false, false, cx)
            })
        })
        .collect();
    // Warm the factory before taking the baseline. The same books stay alive
    // throughout; only their presentation entities are created and released.
    drop(cx.update(|cx| BookCardView::presentation(books[0].clone(), BookCardPresentation::Grid, cx)));
    cx.update(|_| {});
    let baseline = allocated();
    heap_checkpoint(c"book data only");
    let mut views: Vec<_> = cx.update(|cx| books.iter().map(|book| BookCardView::presentation(book.clone(), BookCardPresentation::Grid, cx)).collect());
    let created = allocated();
    heap_checkpoint(c"presentation views before rendering");
    assert!(cx.update(|cx| views.iter().all(|view| view.read(cx).renders == 0)));
    let visible = [views[0].clone(), views[1].clone()];
    let window = cx.open_window(size(px(500.0), px(400.0)), |_, _| CardFixture { views: visible, first: BookCardInteraction::default(), width: px(180.0) });
    draw(window, cx);
    let rendered = cx.update(|cx| views.iter().filter(|view| view.read(cx).renders > 0).count());
    assert_eq!(rendered, 2);
    let before_release = allocated();
    heap_checkpoint(c"two visible views before releasing offscreen views");
    views.truncate(2);
    cx.update(|_| {});
    let after_release = allocated();
    heap_checkpoint(c"offscreen views released; all book data retained");
    eprintln!(
        "PRESENTATION_PROFILE {}",
        serde_json::json!({
            "loaded_books": books.len(), "presentation_views_created": count,
            "rendered_views": rendered, "offscreen_views_released": count - rendered,
            "baseline_live_bytes": baseline, "with_presentations_live_bytes": created,
            "presentation_creation_delta_bytes": created as i64 - baseline as i64,
            "before_offscreen_release_live_bytes": before_release, "after_offscreen_release_live_bytes": after_release,
            "offscreen_release_delta_bytes": before_release as i64 - after_release as i64,
            "presentation_struct_bytes": size_of::<BookCardElement>(),
            "test_render_counter_bytes_per_view": size_of::<usize>(),
        })
    );
}
