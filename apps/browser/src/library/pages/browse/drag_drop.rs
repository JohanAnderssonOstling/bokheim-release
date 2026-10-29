//! Shared folder drag sources, destinations and previews.

use std::any::Any;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, ApplicationDrag, Entity, ExternalDragPayload, ExternalPaths, Pixels, Point, SharedString, WindowId, div, px};
use gpui_component::Theme;
use library_model::{BookCardRow, BookSource};
use sync_common::DirId;
use ui_components as components;
use uuid::Uuid;

use super::cards::{CardEvent, CardEvents};
use super::folder_actions::{FolderTargets, describe};
use super::listing::BrowseItem;
use crate::library::widgets::{BookCardView, PaginatorDragPreview, PaginatorState};

/// A drop into a folder, regardless of where the drag began.
#[derive(Clone)]
pub(super) enum FolderDrop {
    Files(Vec<PathBuf>),
    Items(LibraryDrag),
    Catalog(Vec<BookCardRow>),
}

/// Catalog entries import into the destination instead of moving a placement.
#[derive(Clone)]
pub(super) enum BrowseDrag {
    Library(LibraryDrag),
    Catalog(Vec<BookCardRow>),
}

/// Library identity accompanies folder IDs when a drag crosses windows.
#[derive(Clone)]
pub(super) struct LibraryDrag {
    pub library_id: Uuid,
    pub targets: FolderTargets,
}

impl LibraryDrag {
    pub(super) fn accepts(&self, library_id: Uuid, destination: DirId) -> bool {
        if self.targets.is_empty() || self.targets.folders.contains(&sync_common::ROOT_DIR_ID) {
            return false;
        }
        if self.library_id == library_id {
            return self.targets.source.is_some() && self.targets.accepts(destination);
        }
        cfg!(not(target_arch = "wasm32"))
    }
}

/// Adapts GPUI's typed drag payloads to the folder's shared drop behavior.
trait FolderDrag: 'static {
    fn accepts(&self, library_id: Uuid, destination: DirId) -> bool;
    fn contents(&self) -> FolderDrop;
}

impl FolderDrag for BrowseDrag {
    fn accepts(&self, library_id: Uuid, destination: DirId) -> bool {
        match self {
            Self::Library(drag) => drag.accepts(library_id, destination),
            Self::Catalog(books) => !books.is_empty() && books.iter().all(|book| importable_source(book.source)),
        }
    }

    fn contents(&self) -> FolderDrop {
        match self {
            Self::Library(drag) => FolderDrop::Items(drag.clone()),
            Self::Catalog(books) => FolderDrop::Catalog(books.clone()),
        }
    }
}

impl FolderDrag for ExternalPaths {
    fn accepts(&self, _: Uuid, _: DirId) -> bool {
        true
    }

    fn contents(&self) -> FolderDrop {
        FolderDrop::Files(self.paths().to_vec())
    }
}

fn accepts_drop(dragged: &dyn Any, library_id: Uuid, destination: DirId) -> bool {
    if let Some(targets) = dragged.downcast_ref::<BrowseDrag>() { targets.accepts(library_id, destination) } else { dragged.downcast_ref::<ExternalPaths>().is_some_and(|paths| paths.accepts(library_id, destination)) }
}

/// Registers the same hint and event handling for either GPUI payload type.
fn bind_folder_drop<T: FolderDrag, E: InteractiveElement>(element: E, library_id: Uuid, destination: DirId, name: SharedString, events: CardEvents) -> E {
    element
        // Nested targets are drawn after their parent, so the innermost
        // target has the last word about where the drag would land.
        .drag_over::<T>(move |style, dragged, window, cx| {
            cx.set_global(DropHint(Some(Hint { window: window.window_handle().window_id(), library_id, destination, name: name.clone(), valid: dragged.accepts(library_id, destination) })));
            style
        })
        .on_drop::<T>(move |dragged, window, cx| events(CardEvent::DropOnto(destination, dragged.contents()), window, cx))
}

/// Query pages import cross-library items into root, and also accept external files.
pub(super) fn import_drop_target<E: InteractiveElement>(element: E, library_id: Uuid, destination: DirId, name: SharedString, events: CardEvents) -> E {
    let element = bind_folder_drop::<BrowseDrag, _>(element, library_id, destination, name.clone(), events.clone());
    bind_folder_drop::<ExternalPaths, _>(element, library_id, destination, name, events).can_drop(move |dragged, _, _| {
        dragged.is::<ExternalPaths>()
            || dragged.downcast_ref::<BrowseDrag>().is_some_and(|drag| match drag {
                BrowseDrag::Library(items) => items.library_id != library_id && items.accepts(library_id, destination),
                BrowseDrag::Catalog(_) => drag.accepts(library_id, destination),
            })
    })
}

/// Chips, breadcrumbs and open space all accept files to import or library
/// items to move/copy, with the same destination and highlight behavior.
pub(super) fn folder_drop_target<E: ParentElement + InteractiveElement + Styled>(element: E, library_id: Uuid, destination: DirId, name: SharedString, highlight: bool, events: CardEvents, theme: components::BrowserTheme) -> E {
    let element = bind_folder_drop::<BrowseDrag, _>(element, library_id, destination, name.clone(), events.clone());
    let element = bind_folder_drop::<ExternalPaths, _>(element, library_id, destination, name, events).can_drop(move |dragged, _, _| accepts_drop(dragged, library_id, destination));
    // Draw the outline last, after nested targets have reported their hint.
    if highlight { element.relative().child(components::drop_highlight::<BrowseDrag>(move |cx| lands_in(library_id, destination, cx), theme)) } else { element }
}

/// Whether the drag under way would land in `destination`, and be taken.
fn lands_in(library_id: Uuid, destination: DirId, cx: &App) -> bool {
    cx.try_global::<DropHint>().and_then(|hint| hint.0.as_ref()).is_some_and(|hint| hint.library_id == library_id && hint.destination == destination && hint.valid)
}

/// Where the drag under way would land, as the innermost folder under the
/// pointer reported it, and whether it would take the drop there. Targets set
/// it while they are drawn with the drag over them; the drag preview takes it
/// as it draws itself, so it names nothing once the pointer leaves every
/// folder.
#[derive(Default)]
struct DropHint(Option<Hint>);

struct Hint {
    window: WindowId,
    library_id: Uuid,
    destination: DirId,
    name: SharedString,
    valid: bool,
}

impl gpui::Global for DropHint {}

/// The preview that follows the pointer during a drag.
///
/// Measured appearances retain the source items' layout and dimensions.
struct DragPreview {
    items: Vec<DragPreviewItem>,
    label: SharedString,
    /// Where inside the dragged element the drag began. GPUI paints this view
    /// at `pointer - cursor_offset`; the small nudge keeps the ghost clear of
    /// the pointer while it is being carried.
    grab: Point<Pixels>,
}

#[derive(Clone)]
pub(super) enum DragPreviewItem {
    Snapshot(PaginatorDragPreview),
    Book(Entity<BookCardView>),
    Folder { icon: components::NavigationIcon, label: SharedString, path: Option<SharedString> },
}

impl gpui::Render for DragPreview {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let preview = if self.items.is_empty() {
            components::drag_preview(self.label.clone(), theme).into_any_element()
        } else {
            div()
                .flex()
                .items_start()
                .gap(px(components::SPACE_XS))
                .children(self.items.iter().map(|item| match item {
                    DragPreviewItem::Snapshot(preview) => preview.render(window, cx).into_any_element(),
                    DragPreviewItem::Book(book) => div().w(px(components::BOOK_CARD_WIDTH)).flex_none().child(book.clone()).into_any_element(),
                    DragPreviewItem::Folder { icon, label, path } => components::compact_hierarchy_item(format!("drag-preview-folder-{label}"), *icon, label.clone(), path.clone(), None, None, false, false, false, theme).into_any_element(),
                }))
                .into_any_element()
        };
        let hint = cx.default_global::<DropHint>().0.take().filter(|hint| hint.window == window.window_handle().window_id());
        // Says where the drop would go, or that it would go nowhere; a drop it
        // cannot make also fades what is being carried.
        let caption = hint.as_ref().map(|hint| components::drag_preview(SharedString::from(if hint.valid { format!("{} → {}", self.label, hint.name) } else { format!("Can't drop in {}", hint.name) }), theme));
        // Drag overlays live outside the page root's inherited typography.
        let application_theme = Theme::global(cx);
        div()
            .font_family(application_theme.font_family.clone())
            .text_size(application_theme.font_size)
            .pl(self.grab.x + px(DRAG_PREVIEW_NUDGE))
            .pt(self.grab.y + px(DRAG_PREVIEW_NUDGE))
            .flex()
            .flex_col()
            .items_start()
            .gap(px(components::SPACE_XS))
            .child(preview)
            .children(caption)
    }
}

/// Lets `element` be picked up, carrying `targets`, with a preview of `items`
/// following the pointer.
pub(super) fn folder_drag_source<E: StatefulInteractiveElement>(element: E, targets: BrowseDrag, items: Vec<DragPreviewItem>) -> E {
    let label = SharedString::from(match &targets {
        BrowseDrag::Library(drag) => describe(&drag.targets),
        BrowseDrag::Catalog(books) => {
            if books.len() == 1 {
                "1 book".to_owned()
            } else {
                format!("{} books", books.len())
            }
        }
    });
    element
        .on_drag(targets, move |_, grab, _, cx| {
            cx.set_global(DropHint(None));
            cx.new(|_| DragPreview { items: items.clone(), label: label.clone(), grab })
        })
        .external_drag_payload::<BrowseDrag>(|_, _, _| Some(ExternalDragPayload::Application(ApplicationDrag::new())))
}

/// What a drag carries out of the folder `source`, and the preview it shows.
pub(super) type DragPayload = (BrowseDrag, Vec<DragPreviewItem>);

/// The drag a book card starts, from the paginator state and the card's id.
pub(super) type BookDrag = Rc<dyn Fn(&PaginatorState, &SharedString, &App) -> Option<DragPayload>>;

/// Existing placements can move; a complete selection of downloadable catalog
/// books imports into the destination instead.
pub(super) fn drag_payload(items: &[BrowseItem], source: Option<DirId>, library_id: Uuid, chip_icon: components::NavigationIcon, cx: &App) -> Option<DragPayload> {
    let (books, preview): (Vec<_>, Vec<_>) = items
        .iter()
        .filter_map(BrowseItem::book)
        .filter_map(|book| {
            let row = book.read(cx).detail_source().0;
            importable_source(row.source).then_some((row, DragPreviewItem::Book(book.clone())))
        })
        .unzip();
    if !books.is_empty() {
        if books.len() != items.len() {
            return None;
        }
        return Some((BrowseDrag::Catalog(books), preview));
    }
    let items = items.iter().filter(|item| source.is_some() || matches!(item, BrowseItem::Book(_)));
    let mut targets = FolderTargets::from_items(items.clone(), source.unwrap_or(sync_common::ROOT_DIR_ID), cx)?;
    if source.is_none() {
        targets.source = None;
    }
    let preview = items
        .map(|item| match item {
            BrowseItem::Child { row, .. } => DragPreviewItem::Folder { icon: chip_icon, label: row.name.clone().into(), path: row.path.clone().map(SharedString::from) },
            BrowseItem::Book(book) => DragPreviewItem::Book(book.clone()),
        })
        .collect();
    Some((BrowseDrag::Library(LibraryDrag { library_id, targets }), preview))
}

fn importable_source(source: BookSource) -> bool {
    matches!(source, BookSource::Catalog { .. } | BookSource::Opds { downloadable: true, .. } | BookSource::ExternalCatalog { downloadable: true, .. })
}

const DRAG_PREVIEW_NUDGE: f32 = 12.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_drags_accept_library_destinations_and_exclude_books_without_downloads() {
        use library_model::{CardKey, LibraryFileTypeFilter};
        use sync_common::{BookId, ROOT_DIR_ID};

        let book = |source| BookCardRow {
            source,
            title: "Catalog book".into(),
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
        let library = Uuid::from_u128(1);
        let folder = Uuid::from_u128(2);
        let entry = CardKey([7; 32]);
        let source = Uuid::from_u128(3);
        for source in [BookSource::Catalog { content_hash: BookId::from_bytes([4; 32]) }, BookSource::ExternalCatalog { entry, downloadable: true }, BookSource::Opds { source, entry, downloadable: true, buyable: false }] {
            let drag = BrowseDrag::Catalog(vec![book(source)]);
            assert!(drag.accepts(library, ROOT_DIR_ID));
            assert!(drag.accepts(library, folder));
            let FolderDrop::Catalog(books) = drag.contents() else { panic!("catalog books must bypass placement choices") };
            assert_eq!(books[0].source, source);
        }
        assert!(!BrowseDrag::Catalog(Vec::new()).accepts(library, ROOT_DIR_ID));
        for source in [BookSource::Library { content_hash: BookId::from_bytes([4; 32]) }, BookSource::ExternalCatalog { entry, downloadable: false }, BookSource::Opds { source, entry, downloadable: false, buyable: true }] {
            assert!(!BrowseDrag::Catalog(vec![book(source)]).accepts(library, ROOT_DIR_ID));
        }
    }

    struct SameFolderDrop {
        frames: Rc<std::cell::Cell<usize>>,
    }

    impl gpui::Render for SameFolderDrop {
        fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            self.frames.set(self.frames.get() + 1);
            let library = Uuid::from_u128(1);
            let source = Uuid::from_u128(2);
            let dragged = LibraryDrag { library_id: library, targets: FolderTargets { folders: vec![Uuid::from_u128(3)], source: Some(source), ..Default::default() } };
            let target = folder_drop_target(div().id("same-folder").size_full(), library, source, "Current folder".into(), false, Rc::new(|_, _, _| panic!("a no-op drop must not move anything")), components::browser_theme(cx));
            folder_drag_source(target, BrowseDrag::Library(dragged), Vec::new())
        }
    }

    #[gpui::test]
    fn dropping_in_the_current_folder_clears_the_preview(cx: &mut gpui::TestAppContext) {
        use gpui::{MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, point};
        use std::cell::Cell;

        cx.update(gpui_component::init);
        let frames = Rc::new(Cell::new(0));
        let (_, visual) = cx.add_window_view(|_, _| SameFolderDrop { frames: frames.clone() });
        visual.run_until_parked();
        visual.simulate_event(MouseDownEvent { position: point(px(20.0), px(20.0)), button: MouseButton::Left, ..Default::default() });
        visual.simulate_event(MouseMoveEvent { position: point(px(50.0), px(50.0)), pressed_button: Some(MouseButton::Left), ..Default::default() });
        visual.run_until_parked();
        assert!(visual.update(|_, cx| cx.has_active_drag()));
        let dragged_frames = frames.get();
        visual.simulate_event(MouseUpEvent { position: point(px(50.0), px(50.0)), button: MouseButton::Left, ..Default::default() });
        visual.run_until_parked();
        assert!(!visual.update(|_, cx| cx.has_active_drag()));
        assert!(frames.get() > dragged_frames, "rejected drops must redraw to erase the preview");
    }

    #[test]
    fn cross_library_drag_from_all_books_requires_a_different_library() {
        let library_id = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let drag = LibraryDrag { library_id, targets: FolderTargets { books: vec![sync_common::BookId::from_bytes([1; 32])], ..Default::default() } };
        assert!(!drag.accepts(library_id, sync_common::ROOT_DIR_ID));
        assert_eq!(drag.accepts(other, sync_common::ROOT_DIR_ID), cfg!(not(target_arch = "wasm32")));
    }

    #[test]
    fn cross_library_drag_rejects_empty_selections_and_library_roots() {
        let library_id = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let mut drag = LibraryDrag { library_id, targets: FolderTargets::default() };
        assert!(!drag.accepts(other, sync_common::ROOT_DIR_ID));
        drag.targets.folders.push(sync_common::ROOT_DIR_ID);
        assert!(!drag.accepts(other, sync_common::ROOT_DIR_ID));
    }

    #[test]
    fn cross_library_drag_accepts_identical_folder_ids_without_local_no_op_checks() {
        let library_id = Uuid::from_u128(1);
        let other_library_id = Uuid::from_u128(2);
        let source = Uuid::from_u128(3);
        let destination = Uuid::from_u128(4);
        let drag = LibraryDrag { library_id, targets: FolderTargets { source: Some(source), books: vec![sync_common::BookId::from_bytes([1; 32])], ..Default::default() } };
        assert!(drag.accepts(library_id, destination));
        assert_eq!(drag.accepts(other_library_id, destination), cfg!(not(target_arch = "wasm32")));
        assert_eq!(drag.accepts(other_library_id, source), cfg!(not(target_arch = "wasm32")));
        assert!(!drag.accepts(library_id, source));
    }
}
