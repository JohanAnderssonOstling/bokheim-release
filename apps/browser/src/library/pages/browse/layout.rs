//! Conversion from a listing to paginator groups.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, Bounds, Div, Entity, MouseButton, Pixels, Point, SharedString, Stateful, Window, div, px, rems};
use gpui_component::menu::OpenContextMenu;
use library_model::{BrowseHeadingState, BrowsePageState, BrowseRow};
use sync_common::DirId;
use ui_components as components;

use super::cards::{CardEvent, CardEvents};
use super::clipboard::FocusedBookClipboard;
use super::drag_drop::{BookDrag, DragPreviewItem, drag_payload, folder_drag_source, folder_drop_target};
use super::listing::{BrowseItem, BrowseItems, BrowseKind, book_id, chip_id, is_author_heading, resolve_items, shown_count, shows_download_mark};
use super::page::BrowseSplitSide;
use super::section_card::SectionCard;
use crate::library::widgets::{
    BookCardInteraction, BookCardPresentation, BookCardView, PaginatorChild, PaginatorChildren, PaginatorDragPreview, PaginatorGroup, PaginatorGroupPolicy, PaginatorSizing, PaginatorSlots, PaginatorState, PaginatorWidthPolicy,
    book_card_policy, browse_folder_card_policy, browse_section_card_policy, browse_shelf_card_policy,
};
use crate::{CopyBooks, CutBooks};

/// A child opens, splits, shows its resolved menu, and carries folder placements.
#[derive(Clone, Default)]
pub(super) struct ChildInteraction {
    pub selected: bool,
    pub marquee: bool,
    pub draggable: bool,
    pub drag_preview: Option<PaginatorDragPreview>,
}

pub(super) fn child_frame(
    element: impl IntoElement, row: &BrowseRow, events: CardEvents, library_id: uuid::Uuid, source: Option<DirId>, items: Vec<BrowseItem>, interaction: ChildInteraction, chip_icon: components::NavigationIcon,
    theme: components::BrowserTheme, cx: &App,
) -> Stateful<Div> {
    let frame = div().id(format!("browse-chip-frame-{}", row.id)).w_full().h_full().child(element);
    child_interactions(frame, row, events, library_id, source, items, interaction, chip_icon, theme, cx)
}

/// Attach interactions to the visible entry itself when its cell has padding.
pub(super) fn child_interactions(
    frame: Stateful<Div>, row: &BrowseRow, events: CardEvents, library_id: uuid::Uuid, source: Option<DirId>, items: Vec<BrowseItem>, interaction: ChildInteraction, chip_icon: components::NavigationIcon, theme: components::BrowserTheme,
    cx: &App,
) -> Stateful<Div> {
    let location = row.id;
    let split_location = location;
    let open_events = events.clone();
    let menu_events = events.clone();
    let menu_items = items.clone();
    let long_press_events = events.clone();
    let long_press_items = items.clone();
    let mut frame = frame
        // The page opens its background/selection menu on right-button down.
        // Claim this press before the later auxiliary click opens our menu.
        .on_mouse_down(MouseButton::Right, move |event, window, cx| {
            if !interaction.selected || (event.modifiers.control && crate::split_panes_available(window)) {
                cx.stop_propagation();
            }
        })
        .on_click(move |event, window, cx| {
            if interaction.marquee {
                cx.stop_propagation();
                return;
            }
            let event = if event.modifiers().control && crate::split_panes_available(window) { CardEvent::SplitChild(location, BrowseSplitSide::Left) } else { CardEvent::OpenChild(location) };
            open_events(event, window, cx);
            cx.stop_propagation();
        })
        .on_aux_click(move |event, window, cx| {
            if !event.is_secondary() {
                return;
            }
            let split = event.modifiers().control && crate::split_panes_available(window);
            if split {
                menu_events(CardEvent::SplitChild(split_location, BrowseSplitSide::Right), window, cx);
            } else if interaction.selected {
                return;
            } else {
                menu_events(CardEvent::ItemsMenu(menu_items.clone(), event.position()), window, cx);
            }
            cx.stop_propagation();
        })
        .on_long_press(move |event, window, cx| {
            if !interaction.selected {
                long_press_events(CardEvent::ItemsMenu(long_press_items.clone(), event.position), window, cx);
                cx.stop_propagation();
            }
        });
    if let Some(source) = source {
        if let library_model::BrowseLocation::Folder(destination) = row.id {
            frame = folder_drop_target(frame, library_id, destination, row.name.clone().into(), true, events, theme);
        }
        if interaction.draggable
            && let Some((targets, preview)) = drag_payload(&items, Some(source), library_id, chip_icon, cx)
        {
            let preview = interaction.drag_preview.map_or(preview, |snapshot| vec![DragPreviewItem::Snapshot(snapshot)]);
            frame = folder_drag_source(frame, targets, preview);
        }
    }
    frame
}

pub(super) fn download_mark(row: &BrowseRow, events: CardEvents, theme: components::BrowserTheme) -> Option<gpui::AnyElement> {
    shows_download_mark(row).then(|| {
        let location = row.id;
        div()
            .id(format!("chip-download-{location}"))
            .debug_selector(move || format!("chip-download-{location}"))
            .flex_none()
            .cursor_pointer()
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Download remaining books to this device").build(window, cx))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click({
                let location = row.id;
                move |_, window, cx| {
                    cx.stop_propagation();
                    events(CardEvent::Download(location), window, cx);
                }
            })
            .child(components::download_glyph().text_color(theme.text))
            .into_any_element()
    })
}

/// Independently focusable contents use the same activation and menu as the pointer.
pub(super) fn keyboard_item(frame: Stateful<Div>, activate: impl Fn(&mut Window, &mut App) + 'static, menu: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static, cx: &App) -> Stateful<Div> {
    let bounds = Rc::new(Cell::new(Bounds::default()));
    let menu_bounds = bounds.clone();
    let theme = components::browser_theme(cx);
    frame
        .relative()
        .focusable()
        .tab_index(0)
        .key_context("BrowseItem")
        .border_1()
        .border_color(gpui::transparent_black())
        .focus(move |style| style.border_color(theme.accent))
        .child(components::measured_bounds(bounds))
        .on_action(move |_: &OpenContextMenu, window, cx| menu(menu_bounds.get().bottom_left(), window, cx))
        .on_key_down(move |event, window, cx| {
            if event.keystroke.modifiers != gpui::Modifiers::default() {
                return;
            }
            match event.keystroke.key.as_str() {
                "enter" => activate(window, cx),
                "left" | "up" => window.focus_prev(cx),
                "right" | "down" => window.focus_next(cx),
                _ => return,
            }
            cx.stop_propagation();
        })
}

/// The three book rails share identity, activation and cover preparation.
pub(super) fn book_child(
    item_id: SharedString, book: Entity<BookCardView>, presentation: BookCardPresentation, interaction: impl Fn(&PaginatorState, &SharedString, &App) -> BookCardInteraction + 'static, drag: Option<BookDrag>, cx: &mut App,
) -> PaginatorChild {
    let frame_id = item_id.clone();
    let rendered = book.clone();
    let activated = book.clone();
    let view = BookCardView::presentation(book.clone(), presentation, cx);
    let child = PaginatorChild::new(move |state, window, cx| {
        let interaction = interaction(state, &frame_id, cx);
        if state.preview {
            return div()
                .w_full()
                .h_full()
                .min_w_0()
                .when(presentation == BookCardPresentation::Grid, |frame| frame.overflow_hidden())
                .when(presentation != BookCardPresentation::Grid, |frame| frame.relative().border_1().border_color(gpui::transparent_black()))
                .child(BookCardView::preview(&rendered, presentation, interaction, window, cx))
                .into_any_element();
        }
        let card = BookCardView::present(view.clone(), interaction, window, cx);
        let mut frame = div().id(frame_id.clone()).w_full().h_full().min_w_0().when(presentation == BookCardPresentation::Grid, |frame| frame.overflow_hidden()).child(card);
        if presentation != BookCardPresentation::Grid {
            let activate = rendered.clone();
            let menu = rendered.clone();
            frame = keyboard_item(frame, move |window, cx| BookCardView::activate(&activate, window, cx), move |position, window, cx| BookCardView::open_items_menu(std::slice::from_ref(&menu), position, window, cx), cx);
            let copy = rendered.clone();
            let cut = rendered.clone();
            frame = frame.on_action(move |_: &CopyBooks, window, cx| dispatch_book_clipboard(&copy, false, window, cx)).on_action(move |_: &CutBooks, window, cx| dispatch_book_clipboard(&cut, true, window, cx));
        }
        if presentation == BookCardPresentation::Section {
            let theme = components::browser_theme(cx);
            frame = frame.when(!interaction.marquee, |frame| frame.hover(move |style| style.bg(theme.hover))).when(!interaction.selected, |frame| frame.on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation()));
        }
        if let Some((targets, preview)) = drag.as_ref().and_then(|drag| drag(state, &frame_id, cx)) {
            let preview = state.drag_preview(&frame_id).map_or(preview, |snapshot| vec![DragPreviewItem::Snapshot(snapshot)]);
            frame = folder_drag_source(frame, targets, preview);
        }
        frame.into_any_element()
    })
    .with_id(item_id)
    .prevent_selection_start()
    .with_activate(move |window, cx| BookCardView::activate(&activated, window, cx))
    .with_prepare(move |window, cx| BookCardView::prefetch_for_window(&book, window, cx));
    if presentation == BookCardPresentation::Grid { child.with_own_cell() } else { child }
}

fn dispatch_book_clipboard(book: &Entity<BookCardView>, cut: bool, window: &mut Window, cx: &mut App) {
    let book = book.read(cx);
    if book.can_drag()
        && let Some(id) = book.content_hash()
    {
        window.dispatch_action(Box::new(FocusedBookClipboard { book: id, source: book.source_directory(), cut }), cx);
    }
}

pub(super) fn paginator_groups(
    events: CardEvents,
    library_id: uuid::Uuid,
    chips: Rc<Vec<BrowseRow>>,
    kind: BrowseKind,
    books: Rc<Vec<Entity<BookCardView>>>,
    // The folder being browsed. Present only for listings whose children are
    // real directories, which are the only ones that can be dragged.
    source: Option<DirId>,
    section_cards: &HashMap<library_model::BrowseLocation, Entity<SectionCard>>,
    items: BrowseItems,
    page: Option<&BrowsePageState>,
    book_indices: &BTreeMap<usize, Entity<BookCardView>>,
    heading_rows: &BTreeMap<usize, BrowseRow>,
    cx: &mut App,
) -> Vec<PaginatorGroup> {
    let cover_text = crate::stores::current_cover_text(cx);
    let chip_icon = kind.child_icon();
    // Home's shelves are fixed rows rather than places to go: their cards span
    // the page, and their headings do not open anything.
    let shelves = kind == BrowseKind::Home;
    // A listing's children are section cards or plain chips. Both appear only at
    // the catalog's root, where the feeds are chips beside the subjects' cards.
    let section_ids: HashSet<_> = page.into_iter().flat_map(|page| page.section_ids.iter().copied()).collect();

    let headings = page.and_then(|page| page.headings.as_ref());
    let rows = outer_heading_rows(&chips, headings, heading_rows);
    let children = rows.map(|chip| {
        let Some(chip) = chip else {
            let placeholder = PaginatorChild::new(|_, _, cx| div().size_full().bg(components::browser_theme(cx).hover).into_any_element());
            if headings.is_some_and(|headings| headings.cards) {
                return (true, placeholder);
            } else {
                return (false, placeholder.with_intrinsic_width(components::HierarchyItemWidth { icon: chip_icon, label: "Loading authors…".into(), count: None, show_download: false }));
            }
        };
        let chip_icon = if matches!(chip.id, library_model::BrowseLocation::Store(_)) { components::NavigationIcon::Storefront } else { chip_icon };
        let width_label = SharedString::from(chip.name.clone());
        let child_count = shown_count(&chip);
        let show_download = shows_download_mark(&chip);
        let item_id = chip_id(chip.id);
        let rendered_id = item_id.clone();
        let chip = chip.clone();
        let events = events.clone();
        let drag_items = items.clone();
        let section_card = section_cards.get(&chip.id).cloned();
        let is_card = section_card.is_some() || section_ids.contains(&chip.id);
        // Chips previously carried a click handler only, so the paginator had
        // nothing to invoke for them. Enter and a click now share one path.
        let activate_events = events.clone();
        let activate_location = chip.id;
        let child = PaginatorChild::new(move |state, _, cx| {
            let focused = state.is_focused(&rendered_id);
            let selected = state.is_selected(&rendered_id);
            let theme = components::browser_theme(cx);
            let selected_items = resolve_items(&state.target_ids(&rendered_id), &drag_items);
            let header_drag = (is_card && !state.preview)
                .then(|| drag_payload(&selected_items, source, library_id, chip_icon, cx))
                .flatten()
                .map(|(targets, preview)| (targets, state.drag_preview(&rendered_id).map_or(preview, |snapshot| vec![DragPreviewItem::Snapshot(snapshot)])));
            let item = match &section_card {
                None if is_card => div().size_full().p_4().bg(theme.hover).child(chip.name.clone()).into_any_element(),
                None => components::compact_hierarchy_item(
                    format!("browse-chip-{}", chip.id),
                    chip_icon,
                    chip.name.clone(),
                    chip.path.clone().map(SharedString::from),
                    child_count,
                    download_mark(&chip, events.clone(), theme),
                    selected,
                    focused,
                    state.marquee,
                    theme,
                )
                .debug_selector(|| format!("browse-chip-{}", chip.id))
                .into_any_element(),
                Some(card) => SectionCard::present(card.clone(), focused, selected, state.marquee, header_drag, state.preview).into_any_element(),
            };
            if shelves || state.preview {
                return div().relative().size_full().child(item).into_any_element();
            }
            let interaction = ChildInteraction { selected, marquee: state.marquee, draggable: !is_card, drag_preview: state.drag_preview(&rendered_id) };
            let frame = child_frame(item, &chip, events.clone(), library_id, source, selected_items, interaction, chip_icon, theme, cx);
            let frame = if is_card { frame.relative() } else { frame.h(rems(components::COMPACT_HIERARCHY_ITEM_HEIGHT_REM)) };
            frame.into_any_element()
        })
        .with_id(item_id)
        .with_intrinsic_width(components::HierarchyItemWidth { icon: chip_icon, label: width_label, count: child_count, show_download });
        let child = if shelves { child } else { child.with_activate(move |window, cx| activate_events(CardEvent::OpenChild(activate_location), window, cx)) };
        (is_card, child)
    });
    let chip_children = children.filter_map(|(is_card, child)| (!is_card).then(|| child.clone()));
    let card_children = children.filter_map(|(is_card, child)| (*is_card).then(|| child.clone()));

    let book_children = direct_book_children(books, page, book_indices, BookCardPresentation::Grid, Some((items, source, library_id, chip_icon)), cx);

    let chip_width = PaginatorWidthPolicy::pixels(px(1.0));
    let mut groups = Vec::with_capacity(3);
    if !chip_children.is_empty() {
        let policy = PaginatorGroupPolicy::new(chip_width, PaginatorSizing::rems(rems(components::COMPACT_HIERARCHY_ITEM_HEIGHT_REM)), px(components::COMPACT_HIERARCHY_ITEM_ROW_GAP))
            .with_gap(px(components::COMPACT_HIERARCHY_ITEM_ROW_GAP))
            .intrinsic()
            .flush_edges()
            .with_edge_inset(px(components::BOOK_CARD_PADDING));
        groups.push(PaginatorGroup::from_slots(policy, chip_children));
    }
    if !card_children.is_empty() {
        let policy = if shelves {
            browse_shelf_card_policy(cover_text)
        } else if kind == BrowseKind::Folder {
            browse_folder_card_policy(cover_text)
        } else {
            browse_section_card_policy(cover_text)
        };
        groups.push(PaginatorGroup::from_slots(policy, card_children));
    }
    if !book_children.is_empty() {
        groups.push(PaginatorGroup::from_slots(book_card_policy(cover_text), book_children));
    }
    groups
}

/// Reserve unloaded author positions without allocating names or card views.
fn outer_heading_rows(chips: &[BrowseRow], headings: Option<&BrowseHeadingState>, loaded: &BTreeMap<usize, BrowseRow>) -> PaginatorSlots<Option<BrowseRow>> {
    if let Some(headings) = headings {
        let mut rows: PaginatorSlots<_> = chips.iter().filter(|chip| !is_author_heading(chip.id)).cloned().map(Some).collect();
        let entries = loaded.iter().filter(|(index, _)| **index < headings.total).map(|(&index, row)| (index, Some(row.clone()))).collect();
        rows.append(PaginatorSlots::sparse(headings.total, None, entries));
        rows
    } else {
        chips.iter().cloned().map(Some).collect()
    }
}

#[cfg(test)]
mod heading_tests {
    use super::*;
    use library_model::BrowseLocation;

    #[cfg(all(not(target_arch = "wasm32"), not(feature = "kobo")))]
    #[gpui::test]
    fn folder_context_menu_claims_right_press_before_the_page_menu(cx: &mut gpui::TestAppContext) {
        use crate::library::widgets::{CursorMove, Paginator};
        use gpui::{Modifiers, MouseButton, VisualTestContext, size};
        use std::cell::RefCell;

        cx.update(gpui_component::init);
        let menus = Rc::new(RefCell::new(Vec::new()));
        let recorded = menus.clone();
        let events: CardEvents = Rc::new(move |event, _, _| match event {
            CardEvent::ItemsMenu(_, _) => recorded.borrow_mut().push("folder"),
            CardEvent::SplitChild(_, BrowseSplitSide::Right) => recorded.borrow_mut().push("split"),
            _ => panic!("unexpected folder activation"),
        });
        let selected = Rc::new(Cell::new(false));
        let child_selected = selected.clone();
        let child = PaginatorChild::new(move |_, _, cx| {
            let row = BrowseRow { id: BrowseLocation::Folder(DirId::from_u128(1)), name: "Folder".into(), path: None, book_count: 0, downloaded_book_count: 0 };
            child_frame(
                div().size_full(),
                &row,
                events.clone(),
                uuid::Uuid::nil(),
                None,
                vec![BrowseItem::Child { row: row.clone(), parent: None }],
                ChildInteraction { selected: child_selected.get(), ..Default::default() },
                BrowseKind::Folder.child_icon(),
                components::browser_theme(cx),
                cx,
            )
            .debug_selector(|| "context-folder".into())
            .into_any_element()
        })
        .with_id("folder");
        let policy = PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(200.0)), PaginatorSizing::pixels(px(100.0)), px(0.0)).with_maximum_width(PaginatorWidthPolicy::pixels(px(200.0))).pack_start();
        let recorded = menus.clone();
        let window = cx.open_window(size(px(1000.0), px(400.0)), |_, _| Paginator::new("folder-context-test", [PaginatorGroup::new(policy, [child])]).selectable().on_context_menu(move |_, _, _, _| recorded.borrow_mut().push("page")));
        let flow = window.root(cx).unwrap();
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        // A nested folder must still claim its press when its enclosing card
        // is selected and the outer paginator would otherwise open a menu.
        flow.update(visual, |flow, cx| {
            flow.move_and_toggle_selection(CursorMove::Down, cx);
        });
        for (child_selected, control, expected) in [(false, false, "folder"), (true, false, "page"), (true, true, "split")] {
            let expected = if control && !crate::native_windows_available() { "page" } else { expected };
            selected.set(child_selected);
            flow.update(visual, |_, cx| cx.notify());
            visual.run_until_parked();
            let position = visual.debug_bounds("context-folder").unwrap().center();
            let modifiers = Modifiers { control, ..Default::default() };
            visual.simulate_mouse_down(position, MouseButton::Right, modifiers);
            if expected != "page" {
                assert!(menus.borrow().is_empty(), "the page menu must not run before the folder handles the click");
            }
            visual.simulate_mouse_up(position, MouseButton::Right, modifiers);
            visual.run_until_parked();
            assert_eq!(menus.borrow_mut().drain(..).collect::<Vec<_>>(), [expected]);
        }
        let background = gpui::point(px(900.0), px(300.0));
        visual.simulate_mouse_down(background, MouseButton::Right, Modifiers::default());
        visual.simulate_mouse_up(background, MouseButton::Right, Modifiers::default());
        assert_eq!(menus.borrow_mut().drain(..).collect::<Vec<_>>(), ["page"], "empty browse space must still open its page menu once");
    }
    #[cfg(all(not(target_arch = "wasm32"), not(feature = "kobo")))]
    #[gpui::test]
    async fn catalog_books_can_be_selected_and_dragged_to_a_library_in_bulk(cx: &mut gpui::TestAppContext) {
        use super::super::drag_drop::{BrowseDrag, FolderDrop, import_drop_target};
        use crate::library::widgets::Paginator;
        use crate::navigation::LibrarySession;
        use gpui::{MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, point};
        use library_model::{BookCardRow, BookSource, LibraryFileTypeFilter};
        use std::cell::RefCell;
        use sync_common::BookId;

        struct CatalogSurface {
            flow: Entity<Paginator>,
            events: CardEvents,
        }

        impl gpui::Render for CatalogSurface {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                import_drop_target(div().size_full().child(self.flow.clone()), uuid::Uuid::from_u128(777), sync_common::ROOT_DIR_ID, "Target library".into(), self.events.clone())
            }
        }

        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        cx.update(|cx| {
            cx.set_global(crate::stores::GeneratedCoversOnly(true));
            cx.set_global(crate::stores::CoverTextSetting(app_preferences::CoverText::CoverOnly));
        });
        let library_id = *library.backend().id();
        let books = Rc::new(
            (0..3)
                .map(|index| {
                    let row = BookCardRow {
                        source: BookSource::Catalog { content_hash: BookId::from_bytes([index + 10; 32]) },
                        title: format!("Catalog book {index}"),
                        subtitle: None,
                        author: "Writer".into(),
                        progress: 0.0,
                        downloaded: false,
                        download_requested: false,
                        format_category: LibraryFileTypeFilter::Book,
                        audiobook_duration_ms: None,
                        added_at: 0,
                        source_directory: None,
                    };
                    BookCardView::create_in_grid(row, library.clone(), Rc::new(|_, _, _| {}), false, false, cx)
                })
                .collect::<Vec<_>>(),
        );
        let hash = BookId::from_bytes([250; 32]);
        let mut row = books[0].read_with(cx, |book, _| book.detail_source().0);
        row.source = BookSource::Library { content_hash: hash };
        let library_book = BookCardView::create_in_grid(row, library.clone(), Rc::new(|_, _, _| {}), false, false, cx);
        let folder_id = DirId::from_u128(778);
        let folder = BrowseItem::Child { row: BrowseRow { id: BrowseLocation::Folder(folder_id), name: "Folder".into(), path: None, book_count: 0, downloaded_book_count: 0 }, parent: None };
        cx.update(|cx| {
            let selection = [folder.clone(), BrowseItem::Book(library_book.clone())];
            let icon = BrowseKind::Folder.child_icon();
            let (BrowseDrag::Library(flat), preview) = drag_payload(&selection, None, library_id, icon, cx).unwrap() else { panic!("library books must carry placements") };
            assert_eq!(flat.library_id, library_id);
            assert_eq!(flat.targets.source, None);
            assert_eq!(flat.targets.books, [hash]);
            assert!(flat.targets.folders.is_empty(), "flat listings must exclude folder chips from the drag");
            assert!(matches!(preview.as_slice(), [DragPreviewItem::Book(book)] if book == &library_book));

            let (BrowseDrag::Library(nested), preview) = drag_payload(&selection, Some(sync_common::ROOT_DIR_ID), library_id, icon, cx).unwrap() else { panic!("folder listings must carry placements") };
            assert_eq!(nested.targets.source, Some(sync_common::ROOT_DIR_ID));
            assert_eq!(nested.targets.books, [hash]);
            assert_eq!(nested.targets.folders, [folder_id]);
            assert!(matches!(preview.as_slice(), [DragPreviewItem::Folder { label, .. }, DragPreviewItem::Book(book)] if label.as_ref() == "Folder" && book == &library_book), "preview order must follow selection order");

            assert!(drag_payload(std::slice::from_ref(&folder), None, library_id, icon, cx).is_none());
            for companion in [folder, BrowseItem::Book(library_book.clone())] {
                let mixed = [BrowseItem::Book(books[0].clone()), companion];
                assert!(drag_payload(&mixed, Some(sync_common::ROOT_DIR_ID), library_id, icon, cx).is_none(), "mixed catalog and placement selections must be rejected as a whole");
            }
        });
        let items: BrowseItems = Rc::new(cx.update(|cx| books.iter().map(|book| (book_id(book.read(cx).key(), None), BrowseItem::Book(book.clone()))).collect()));
        let menus = Rc::new(RefCell::new(Vec::new()));
        let recorded = menus.clone();
        let flow = cx.new(|cx| {
            let this: gpui::WeakEntity<Paginator> = cx.weak_entity();
            let groups = paginator_groups(Rc::new(|_, _, _| {}), library_id, Rc::new(Vec::new()), BrowseKind::Catalog, books.clone(), None, &HashMap::new(), items.clone(), None, &BTreeMap::new(), &BTreeMap::new(), cx);
            Paginator::new("catalog-selection", groups).selectable().on_context_menu(move |id, position, window, cx| {
                let (Some(id), Some(flow)) = (id, this.upgrade()) else { return };
                let books = resolve_items(&flow.read(cx).target_ids(&id), &items)
                    .into_iter()
                    .filter_map(|item| match item {
                        BrowseItem::Book(book) => Some(book),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                *recorded.borrow_mut() = books.iter().map(|book| book.read(cx).key()).collect();
                BookCardView::open_items_menu(&books, position, window, cx);
            })
        });
        let drops = Rc::new(RefCell::new(Vec::new()));
        let recorded_drops = drops.clone();
        let events: CardEvents = Rc::new(move |event, _, _| {
            let CardEvent::DropOnto(destination, FolderDrop::Catalog(books)) = event else { panic!("catalog drags must import books directly") };
            assert_eq!(destination, sync_common::ROOT_DIR_ID);
            recorded_drops.borrow_mut().push(books.iter().map(BookCardRow::key).collect::<Vec<_>>());
        });
        let surface = cx.new(|_| CatalogSurface { flow: flow.clone(), events });
        let (_, visual) = cx.add_window_view(|window, cx| gpui_component::Root::new(surface, window, cx));
        visual.run_until_parked();
        let (first, second, empty) = visual.update(|_, cx| {
            let first = flow.read(cx).child_bounds(0).unwrap();
            (first.center(), flow.read(cx).child_bounds(1).unwrap().center(), point(first.left() + px(5.0), first.bottom() + px(5.0)))
        });
        visual.simulate_event(MouseDownEvent { position: first, button: MouseButton::Left, ..Default::default() });
        visual.simulate_event(MouseMoveEvent { position: empty, pressed_button: Some(MouseButton::Left), ..Default::default() });
        visual.run_until_parked();
        assert!(visual.update(|_, cx| cx.has_active_drag()), "a catalog book must start a drag");
        visual.simulate_event(MouseUpEvent { position: empty, button: MouseButton::Left, ..Default::default() });
        visual.run_until_parked();
        assert_eq!(drops.borrow_mut().drain(..).collect::<Vec<_>>(), vec![vec![visual.update(|_, cx| books[0].read(cx).key())]]);
        assert!(visual.update(|_, cx| flow.read(cx).selection().is_empty()), "selection bands must start outside book previews");

        visual.simulate_event(MouseDownEvent { position: empty, button: MouseButton::Left, ..Default::default() });
        visual.simulate_event(MouseMoveEvent { position: second, pressed_button: Some(MouseButton::Left), ..Default::default() });
        visual.run_until_parked();
        visual.simulate_event(MouseUpEvent { position: second, button: MouseButton::Left, ..Default::default() });
        visual.run_until_parked();
        assert_eq!(visual.update(|_, cx| flow.read(cx).selection().iter().copied().collect::<Vec<_>>()), vec![0, 1]);
        visual.simulate_event(MouseDownEvent { position: first, button: MouseButton::Left, ..Default::default() });
        visual.simulate_event(MouseMoveEvent { position: empty, pressed_button: Some(MouseButton::Left), ..Default::default() });
        visual.run_until_parked();
        assert!(visual.update(|_, cx| cx.has_active_drag()));
        visual.simulate_event(MouseUpEvent { position: empty, button: MouseButton::Left, ..Default::default() });
        visual.run_until_parked();
        let expected = visual.update(|_, cx| books.iter().take(2).map(|book| book.read(cx).key()).collect::<Vec<_>>());
        assert_eq!(drops.borrow_mut().drain(..).collect::<Vec<_>>(), vec![expected.clone()], "dropping the selection must import every selected catalog book and no unselected books");
        visual.simulate_event(MouseDownEvent { position: first, button: MouseButton::Right, ..Default::default() });
        visual.run_until_parked();
        assert_eq!(*menus.borrow(), expected, "the download menu must receive every selected book and no unselected books");
    }

    #[test]
    fn sparse_author_headings_preserve_distant_positions_and_leave_feed_chips_first() {
        let row = |id| BrowseRow { id: BrowseLocation::CatalogAuthor(Some(uuid::Uuid::from_u128(id))), name: format!("Writer {id}"), path: None, book_count: 2, downloaded_book_count: 0 };
        let feed = BrowseRow { id: BrowseLocation::Store(uuid::Uuid::nil()), name: "Store".into(), path: None, book_count: 0, downloaded_book_count: 0 };
        let first = row(1);
        let distant = row(28);
        let loaded = BTreeMap::from([(0, first.clone()), (27, distant.clone())]);
        for cards in [false, true] {
            let headings = BrowseHeadingState { total: 30, revision: "one".into(), cards };
            let rows = outer_heading_rows(&[first.clone(), distant.clone(), feed.clone()], Some(&headings), &loaded);
            assert_eq!(rows.len(), 31);
            assert_eq!(rows[0], Some(feed.clone()));
            assert_eq!(rows[1], Some(first.clone()));
            assert!((2..28).map(|index| &rows[index]).all(Option::is_none));
            assert_eq!(rows[28], Some(distant.clone()));
            assert!((29..rows.len()).map(|index| &rows[index]).all(Option::is_none));
            let huge = outer_heading_rows(&[feed.clone()], Some(&BrowseHeadingState { total: 1_000_000, ..headings }), &loaded);
            let mut visited = 0;
            let _ = huge.map(|_| {
                visited += 1;
            });
            assert_eq!(visited, 4, "a feed, two loaded authors and one repeated placeholder");
        }
        let rows = outer_heading_rows(&[feed.clone()], None, &loaded);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], Some(feed));
    }
}

/// Unloaded slots retain their positions; loaded books retain their identity.
pub(super) fn direct_book_children(
    books: Rc<Vec<Entity<BookCardView>>>, page: Option<&BrowsePageState>, indices: &BTreeMap<usize, Entity<BookCardView>>, presentation: BookCardPresentation,
    drag_context: Option<(BrowseItems, Option<DirId>, uuid::Uuid, components::NavigationIcon)>, cx: &mut App,
) -> PaginatorChildren {
    let mut build = |book: &Entity<BookCardView>| {
        let drag = drag_context.as_ref().map(|(items, source, library_id, icon)| {
            let (items, source, library_id, icon) = (items.clone(), *source, *library_id, *icon);
            Rc::new(move |state: &PaginatorState, id: &SharedString, cx: &App| drag_payload(&resolve_items(&state.target_ids(id), &items), source, library_id, icon, cx)) as BookDrag
        });
        let id = book_id(book.read(cx).key(), book.read(cx).source_directory());
        book_child(id, book.clone(), presentation, move |state, id, _| BookCardInteraction { focused: state.is_focused(id), selected: state.is_selected(id), marquee: state.marquee }, drag, cx)
    };
    if let Some(page) = page {
        let loaded = indices.iter().filter(|(index, _)| **index < page.book_total).map(|(&index, book)| (index, build(book))).collect();
        PaginatorChildren::sparse(page.book_total, PaginatorChild::new(|_, _, _| div().size_full().into_any_element()), loaded)
    } else {
        books.iter().map(build).collect()
    }
}
