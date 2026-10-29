use super::*;
use crate::navigation::LibrarySession;
use gpui::{Bounds, ListOffset, Pixels, TestAppContext, VisualTestContext};
use gpui_component::{Root, Theme};

struct TrashFixture(Entity<TrashPage>);

impl Render for TrashFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().flex_col().child(self.0.clone())
    }
}

fn bounds(cx: &mut VisualTestContext, selector: String) -> Option<Bounds<Pixels>> {
    // GPUI's test lookup accepts only static selectors. The test creates a
    // fixed handful of these, independent of the number of library items.
    cx.debug_bounds(Box::leak(selector.into_boxed_str()))
}

fn book(index: usize, deleted_at: i64) -> TrashedBook {
    let mut hash = [0; 32];
    hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
    TrashedBook { content_hash: BookId::from_bytes(hash), title: format!("Book {index}"), author: "Writer".into(), format: "EPUB".into(), deleted_at }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

#[test]
fn rows_group_and_filter_original_indices_without_copying_metadata() {
    let now = 20 * DAY_MS;
    let contents = LibraryTrashView {
        books: vec![book(0, now - 8 * DAY_MS), book(1, now - 1_000), book(2, now - 2 * DAY_MS)],
        folders: vec![TrashedFolder { id: DirId::from_u128(42), name: "Folder".into(), parent_name: None, deleted_at: now - 500 }],
    };
    let mut list = TrashList::default();
    list.update(&contents, "", now);
    assert_eq!(list.rows, [TrashRow::Group(0), TrashRow::Folder(0), TrashRow::Book(1), TrashRow::Group(1), TrashRow::Book(2), TrashRow::Group(2), TrashRow::Book(0)]);
    list.invalidate();
    list.update(&contents, "  BoOk 0  ", now);
    assert_eq!(list.rows, [TrashRow::Group(2), TrashRow::Book(0)]);
    assert_eq!(list.rows[1].item(&contents).unwrap().key(), TrashKey::Book(contents.books[0].content_hash));
    list.invalidate();
    list.update(&contents, "no matches", now);
    assert!(list.rows.is_empty());
    assert!(std::mem::size_of::<TrashRow>() <= 24, "off-screen rows must remain compact descriptions");
}

#[test]
fn time_boundaries_regroup_rows_and_ordinary_repaints_preserve_scroll() {
    let contents = LibraryTrashView { books: vec![book(0, 0)], folders: Vec::new() };
    let mut list = TrashList::default();
    list.update(&contents, "", DAY_MS - 1);
    list.state.scroll_to(ListOffset { item_ix: 1, offset_in_item: px(5.0) });
    list.update(&contents, "", DAY_MS - 1);
    assert_eq!(list.state.logical_scroll_top().offset_in_item, px(5.0));
    list.update(&contents, "", DAY_MS);
    assert_eq!(list.rows, [TrashRow::Group(1), TrashRow::Book(0)]);
    list.update(&contents, "", 7 * DAY_MS);
    assert_eq!(list.rows, [TrashRow::Group(2), TrashRow::Book(0)]);
}

#[gpui::test]
async fn reader_entry_preserves_displayed_trash_covers_and_requests(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let now = now_ms();
    let book = book(0, now);
    let hash = book.content_hash;
    let page = cx.new(|cx| {
        let mut page = TrashPage::new(library, cx);
        page.request.take();
        page.contents = Some(LibraryTrashView { books: vec![book], folders: Vec::new() });
        page
    });
    let fixture = cx.new(|_| TrashFixture(page.clone()));
    let (_, visual) = cx.add_window_view(|window, cx| Root::new(fixture, window, cx));
    draw(visual);
    let image = Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(2, 3))]));
    page.update(visual, |page, cx| {
        track_image(&image, cx);
        page.covers.insert(hash, Some(Arc::downgrade(&image)));
        page.cover_request_serial += 1;
        let task = cx.spawn(async |_, _| std::future::pending::<()>().await);
        page.cover_requests.insert(hash, (page.cover_request_serial, task));
    });
    visual.update(|_, cx| crate::library::evict_covers(cx));
    page.read_with(visual, |page, _| {
        assert!(Arc::ptr_eq(&page.covers[&hash].as_ref().unwrap().upgrade().unwrap(), &image));
        assert!(page.wanted_covers.contains(&hash));
        assert!(page.cover_requests.contains_key(&hash), "reader entry must not cancel another displayed view's requests");
    });
}

#[gpui::test]
async fn large_trash_only_builds_nearby_rows_and_actions_keep_their_ids_across_filters(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let updates = library.updates();
    cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
    cx.update(|cx| cx.set_global(crate::stores::GeneratedCoversOnly(true)));
    let now = now_ms();
    let view = cx.new(|cx| {
        let mut page = TrashPage::new(library, cx);
        page.request.take();
        page.contents = Some(LibraryTrashView { books: (0..10_000).map(|index| book(index, now - index as i64)).collect(), folders: Vec::new() });
        page
    });
    let fixture = cx.new(|_| TrashFixture(view.clone()));
    let (_, visual) = cx.add_window_view(|window, cx| Root::new(fixture, window, cx));
    draw(visual);
    let first = TrashKey::Book(book(0, now).content_hash);
    let last = TrashKey::Book(book(9_999, now).content_hash);
    assert!(bounds(visual, format!("trash-row-{first}")).is_some());
    assert!(bounds(visual, format!("trash-row-{last}")).is_none());
    view.read_with(visual, |page, _| {
        assert!(page.wanted_covers.len() < 50);
        assert!(page.covers.len() < 50);
        assert!(page.cover_requests.is_empty());
        assert!(page.list.state.max_offset_for_scrollbar().y > px(100_000.0));
    });
    let row_height = bounds(visual, format!("trash-row-{first}")).unwrap().size.height;
    let rem = visual.update(|window, _| window.rem_size());
    visual.update(|window, cx| {
        Theme::global_mut(cx).font_size = rem * 2.0;
        window.refresh();
    });
    draw(visual);
    assert!(bounds(visual, format!("trash-row-{first}")).unwrap().size.height > row_height, "font scaling must invalidate cached row heights");
    visual.update(|window, cx| {
        Theme::global_mut(cx).font_size = rem;
        window.refresh();
    });
    draw(visual);
    let checkbox = bounds(visual, format!("trash-select-{first}")).unwrap();
    visual.simulate_click(checkbox.center(), Default::default());
    view.read_with(visual, |page, _| assert!(page.selection.contains(&first)));
    view.update(visual, |page, cx| {
        page.list.state.scroll_to(ListOffset { item_ix: page.list.rows.len() - 1, offset_in_item: px(0.0) });
        cx.notify();
    });
    draw(visual);
    assert!(bounds(visual, format!("trash-row-{last}")).is_some());
    assert!(bounds(visual, format!("trash-row-{first}")).is_none());
    view.read_with(visual, |page, _| assert!(page.covers.len() < 50, "scrolling must release lookups outside the current range"));
    visual.update(|window, cx| {
        view.update(cx, |page, cx| {
            page.search_input(window, cx).read(cx).focus_handle(cx).focus(window, cx);
        });
    });
    visual.run_until_parked();
    visual.simulate_input("  BoOk 9999  ");
    draw(visual);
    let group = visual.debug_bounds("trash-select-group-0").unwrap();
    visual.simulate_click(group.center(), Default::default());
    draw(visual);
    view.read_with(visual, |page, _| {
        assert_eq!(page.selection, HashSet::from([first, last]), "group selection must respect the filter and preserve hidden selections");
        assert_eq!(page.group_keys(0), [last]);
    });
    let restore = bounds(visual, format!("trash-restore-{last}")).unwrap();
    visual.simulate_click(restore.center(), Default::default());
    view.read_with(visual, |page, _| assert_eq!(page.selection, HashSet::from([first]), "the visible Restore handler must act on the original book ID"));

    visual.update(|window, cx| {
        view.update(cx, |page, cx| page.search_input(window, cx).read(cx).focus_handle(cx).focus(window, cx));
    });
    visual.run_until_parked();
    visual.simulate_keystrokes("ctrl-a backspace");
    draw(visual);
    view.read_with(visual, |page, _| {
        assert_eq!(page.list.rows.len(), 10_001, "clearing the input restores every item");
        assert_eq!(page.selection, HashSet::from([first]));
    });

    // Exercise the bounded queue before asynchronous reads complete, then
    // leave the viewport and cancel every obsolete request.
    visual.update(|_, cx| cx.set_global(crate::stores::GeneratedCoversOnly(false)));
    view.update(visual, |page, cx| {
        page.request_visible_covers((0..100).map(|index| book(index, now).content_hash).collect(), cx);
        assert_eq!(page.cover_requests.len(), MAX_COVER_REQUESTS);
        page.request_visible_covers(HashSet::new(), cx);
        assert!(page.cover_requests.is_empty());
        assert!(page.covers.is_empty());
    });
    visual.update(|_, cx| cx.set_global(crate::stores::GeneratedCoversOnly(true)));
    draw(visual);
    view.read_with(visual, |page, _| assert!(!page.wanted_covers.is_empty()));
    visual.update(|window, cx| {
        window.replace_root(cx, |_, _| gpui::Empty);
    });
    draw(visual);
    view.read_with(visual, |page, _| {
        assert!(page.wanted_covers.is_empty(), "a cached, hidden Trash page must release its cover work");
        assert!(page.cover_requests.is_empty());
        assert!(page.covers.is_empty());
        assert_eq!(page.contents.as_ref().unwrap().books.len(), 10_000);
    });
}

#[gpui::test]
async fn refreshing_a_hidden_trash_page_cancels_its_previous_cover_work(cx: &mut TestAppContext) {
    let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_directory, _session, library) = LibrarySession::test_context(cx).await;
    let updates = library.updates();
    cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
    let now = now_ms();
    // Keep the page cached without mounting a window. Its old contents and
    // cover queue must be invalidated by completion, without requiring render.
    let page = cx.new(|cx| {
        let mut page = TrashPage::new(library, cx);
        page.request.take();
        page.contents = Some(LibraryTrashView { books: (0..100).map(|index| book(index, now)).collect(), folders: Vec::new() });
        page.request_visible_covers((0..100).map(|index| book(index, now).content_hash).collect(), cx);
        assert_eq!(page.cover_requests.len(), MAX_COVER_REQUESTS);
        page
    });
    page.update(cx, |page, cx| page.refresh(cx));
    cx.condition(&page, |page, _| page.contents.as_ref().is_some_and(|contents| contents.books.is_empty())).await;
    page.read_with(cx, |page, _| {
        assert!(page.wanted_covers.is_empty(), "a refreshed hidden page must forget its old viewport");
        assert!(page.cover_requests.is_empty(), "obsolete reads must not continue while the page is hidden");
        assert!(page.covers.is_empty());
    });
}
