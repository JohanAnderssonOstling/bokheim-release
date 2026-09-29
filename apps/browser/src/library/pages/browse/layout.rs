//! Conversion from a listing to paginator groups.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, Entity, Pixels, Point, SharedString, WeakEntity, div, px, rems};
use library_model::BrowseRow;
use sync_common::DirId;
use ui_components as components;

use super::folder_actions::{FolderTargets, describe};
use super::listing::shows_download_mark;
use super::page::{BrowsePage, BrowseSplitSide, OpenTrash};
use super::section_card::SectionCard;
use crate::library::widgets::{BookCardView, PaginatorChild, PaginatorGroup, PaginatorGroupPolicy, PaginatorSizing, PaginatorWidthPolicy, book_card_policy, browse_section_card_policy};

/// What a drag or a menu on the child at `index` acts on: the whole selection
/// when that child is part of one, and otherwise just that child.
pub(super) fn acted_on(index: usize, selection: &BTreeSet<usize>) -> Vec<usize> {
    if selection.contains(&index) { selection.iter().copied().collect() } else { vec![index] }
}

/// The folders and books at `indices`, for the folder commands. Books are
/// declared after chips, so an index past the last chip is a book.
///
/// Built from the shared lists rather than read back off the page, which is
/// already borrowed while the listing is being drawn.
pub(super) fn folder_targets(indices: &[usize], chips: &[BrowseRow], chip_groups: &[library_model::BrowseChipGroup], books: &[Entity<BookCardView>], source: DirId, cx: &App) -> Option<FolderTargets> {
    let mut targets = FolderTargets { folders: Vec::new(), folder_parents: Default::default(), books: Vec::new(), source };
    for &index in indices {
        match chips.get(index) {
            Some(chip) => {
                // A chip that is not a directory — a subject, an author — has
                // no placement to change, so it is quietly left out.
                if let Ok(id) = DirId::parse_str(&chip.id) {
                    targets.folders.push(id);
                    if let Some(parent) = chip_groups.iter().find(|group| (group.start..group.end).contains(&index)).and_then(|group| DirId::parse_str(&group.parent.id).ok()) {
                        targets.folder_parents.insert(id, parent);
                    }
                }
            }
            None => {
                let Some(book) = books.get(index - chips.len()) else { continue };
                targets.books.push(book.read(cx).content_hash());
            }
        }
    }
    (!targets.is_empty()).then_some(targets)
}

/// Makes `element` a place to put things down in the folder `destination`:
/// files from the desktop are imported into it, and folders and books dragged
/// from the listing are moved or copied into it. Chips, breadcrumb segments and
/// the open space of the folder being browsed all take drops alike.
pub(super) fn folder_drop_target<E: ParentElement + InteractiveElement + Styled>(element: E, destination: DirId, name: SharedString, highlight: bool, page: WeakEntity<BrowsePage>, theme: components::BrowserTheme) -> E {
    let import_page = page.clone();
    let files_name = name.clone();
    let element = element
        // Says where the drag would land. Nested targets are drawn after the
        // one they sit in, so the innermost, which takes the drop, has the
        // last word.
        .drag_over::<FolderTargets>(move |style, targets, _, cx| {
            cx.set_global(DropHint(Some(Hint { destination, name: name.clone(), valid: targets.accepts(destination) })));
            style
        })
        .drag_over::<gpui::ExternalPaths>(move |style, _, _, cx| {
            cx.set_global(DropHint(Some(Hint { destination, name: files_name.clone(), valid: true })));
            style
        })
        .on_drop::<gpui::ExternalPaths>(move |paths, _, cx| {
            let paths = paths.paths().to_vec();
            let _ = import_page.update(cx, |page, cx| page.import_dropped_paths(Some(destination), paths, cx));
        })
        .on_drop::<FolderTargets>(move |targets, window, cx| {
            let targets = targets.clone();
            let _ = page.update(cx, |page, cx| page.drop_onto_folder(destination, targets, window, cx));
        })
        // Refusing a drop that cannot happen keeps the cursor from promising
        // one.
        .can_drop(move |dragged, _, _| dragged.is::<gpui::ExternalPaths>() || dragged.downcast_ref::<FolderTargets>().is_some_and(|targets| targets.accepts(destination)));
    // The outline is the target's last child, drawn after everything inside
    // it, so by then the innermost target under the pointer has said where the
    // drop goes, and only that one lights up.
    if highlight { element.relative().child(components::drop_highlight::<FolderTargets>(move |cx| lands_in(destination, cx), theme)) } else { element }
}

/// Whether the drag under way would land in `destination`, and be taken.
fn lands_in(destination: DirId, cx: &App) -> bool {
    cx.try_global::<DropHint>().and_then(|hint| hint.0.as_ref()).is_some_and(|hint| hint.destination == destination && hint.valid)
}

/// Where the drag under way would land, as the innermost folder under the
/// pointer reported it, and whether it would take the drop there. Targets set
/// it while they are drawn with the drag over them; the drag preview takes it
/// as it draws itself, so it names nothing once the pointer leaves every
/// folder.
struct DropHint(Option<Hint>);

#[derive(Clone)]
struct Hint {
    destination: DirId,
    name: SharedString,
    valid: bool,
}

impl gpui::Global for DropHint {}

/// The preview that follows the pointer during a drag.
///
/// A single item keeps its shape so it is immediately clear what is being
/// moved. Rectangular and multi-item drags use a compact icon grid so the
/// whole selection remains visible without obscuring the pointer.
struct DragPreview {
    items: Vec<DragPreviewItem>,
    label: SharedString,
    /// Where inside the dragged element the drag began. GPUI paints this view
    /// at `pointer - cursor_offset`; the small nudge keeps the ghost clear of
    /// the pointer while it is being carried.
    grab: Point<Pixels>,
    detailed_books: bool,
}

#[derive(Clone)]
pub(super) enum DragPreviewItem {
    Book(Entity<BookCardView>),
    Folder { icon: components::NavigationIcon, label: SharedString, path: Option<SharedString> },
}

impl gpui::Render for DragPreview {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let preview = if self.items.is_empty() {
            components::drag_preview(self.label.clone(), theme).into_any_element()
        } else if self.items.len() > 1 {
            // A rectangular/multi-selection should read as a group without
            // turning the drag ghost into a stack of full-size cards.
            div()
                .w(px(76.0))
                .flex()
                .flex_wrap()
                .gap(px(components::SPACE_XXS))
                .children(self.items.iter().map(|item| {
                    let icon = match item {
                        DragPreviewItem::Book(_) => components::NavigationIcon::BookOpen,
                        DragPreviewItem::Folder { icon, .. } => *icon,
                    };
                    div().w(px(24.0)).h(px(24.0)).flex().items_center().justify_center().rounded(px(components::RADIUS_SM)).bg(theme.accent_wash).child(components::navigation_icon(icon)).into_any_element()
                }))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_start()
                .gap(px(components::SPACE_XS))
                .children(self.items.iter().map(|item| {
                    match item {
                        DragPreviewItem::Book(book) => div()
                            .w(if self.detailed_books { px(components::BOOK_CARD_DETAIL_FIXED_WIDTH) + rems(components::BOOK_CARD_DETAIL_TEXT_WIDTH_REM).to_pixels(window.rem_size()) } else { px(components::BOOK_CARD_WIDTH) })
                            .flex_none()
                            .child(book.clone())
                            .into_any_element(),
                        DragPreviewItem::Folder { icon, label, path } => components::compact_hierarchy_item(format!("drag-preview-folder-{label}"), *icon, label.clone(), path.clone(), None, None, false, false, theme).into_any_element(),
                    }
                }))
                .into_any_element()
        };
        let hint = cx.try_global::<DropHint>().and_then(|hint| hint.0.clone());
        cx.set_global(DropHint(None));
        // Says where the drop would go, or that it would go nowhere; a drop it
        // cannot make also fades what is being carried.
        let caption = hint.as_ref().map(|hint| components::drag_preview(SharedString::from(if hint.valid { format!("{} → {}", self.label, hint.name) } else { format!("Can't drop in {}", hint.name) }), theme));
        let refused = hint.is_some_and(|hint| !hint.valid);
        // Keep the preview visible against the page while making it clear that
        // it is a transient copy rather than the original card.
        div()
            .pl(self.grab.x + px(DRAG_PREVIEW_NUDGE))
            .pt(self.grab.y + px(DRAG_PREVIEW_NUDGE))
            .flex()
            .flex_col()
            .items_start()
            .gap(px(components::SPACE_XS))
            .child(div().opacity(if refused { 0.3 } else { 0.58 }).child(preview))
            .children(caption)
    }
}

/// Lets `element` be picked up, carrying `targets`, with a preview of `items`
/// following the pointer.
pub(super) fn folder_drag_source<E: StatefulInteractiveElement>(element: E, targets: FolderTargets, items: Vec<DragPreviewItem>, detailed_books: bool) -> E {
    let label = SharedString::from(describe(&targets));
    element.on_drag(targets, move |_, grab, _, cx| cx.new(|_| DragPreview { items: items.clone(), label: label.clone(), grab, detailed_books }))
}

fn drag_preview_items(indices: &[usize], chips: &[BrowseRow], books: &[Entity<BookCardView>], chip_icon: components::NavigationIcon) -> Vec<DragPreviewItem> {
    indices
        .iter()
        .filter_map(|&index| match chips.get(index) {
            Some(chip) => DirId::parse_str(&chip.id).ok().map(|_| DragPreviewItem::Folder { icon: chip_icon, label: SharedString::from(chip.name.clone()), path: chip.path.clone().map(SharedString::from) }),
            None => books.get(index.checked_sub(chips.len())?).cloned().map(DragPreviewItem::Book),
        })
        .collect()
}

/// Distance from the pointer to the pill's corner.
const DRAG_PREVIEW_NUDGE: f32 = 12.0;

/// Chips draw the cursor, the selection and a drag's target from the state the
/// paginator hands each child as it draws it.
pub(super) fn paginator_groups(
    page: WeakEntity<BrowsePage>,
    chips: Rc<Vec<BrowseRow>>,
    chip_groups: &[library_model::BrowseChipGroup],
    chip_icon: components::NavigationIcon,
    books: Rc<Vec<Entity<BookCardView>>>,
    detailed_books: bool,
    cover_text: app_preferences::CoverText,
    // The folder being browsed. Present only for listings whose children are
    // real directories, which are the only ones that can be dragged.
    source: Option<DirId>,
    section_cards: &HashMap<String, Entity<SectionCard>>,
    show_trash: bool,
    open_trash: OpenTrash,
) -> Vec<PaginatorGroup> {
    let mut chip_children = Vec::with_capacity(chips.len());
    let mut book_children = Vec::with_capacity(books.len());

    for (chip_index, chip) in chips.iter().enumerate() {
        let width_label = SharedString::from(chip.name.clone());
        let child_count = Some(chip.book_count);
        let show_download = shows_download_mark(chip);
        let chip = chip.clone();
        let page = page.clone();
        let drag_chips = chips.clone();
        let drag_groups = chip_groups.to_vec();
        let drag_books = books.clone();
        let section_card = section_cards.get(&chip.id).cloned();
        // Chips previously carried a click handler only, so the paginator had
        // nothing to invoke for them. Enter and a click now share one path.
        let activate_page = page.clone();
        let activate_location = chip.id.clone();
        chip_children.push(
            PaginatorChild::new(move |state, window, cx| {
                let location = chip.id.clone();
                let split_location = location.clone();
                let open_page = page.clone();
                let split_page = page.clone();
                let menu_page = page.clone();
                let in_selection = state.selection.contains(&chip_index);
                let height = rems(components::COMPACT_HIERARCHY_ITEM_HEIGHT_REM).to_pixels(window.rem_size());
                // Chips are declared first, so a chip's flat child index is its
                // position in this list.
                //
                // A chip the band picked up is drawn like the cursor's chip: the
                // two never coexist, because moving the cursor drops the selection.
                let focused = state.cursor == Some(chip_index) || in_selection;
                let theme = components::browser_theme(cx);
                let download = show_download.then(|| {
                    let target = page.clone();
                    let location = chip.id.clone();
                    div()
                        .id(format!("chip-download-{location}"))
                        .debug_selector(|| format!("chip-download-{}", chip.id))
                        .flex_none()
                        .cursor_pointer()
                        .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Download remaining books to this device").build(window, cx))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let _ = target.update(cx, |page, cx| page.download_child(location.clone(), cx));
                        })
                        .child(components::download_glyph().text_color(theme.text))
                        .into_any_element()
                });
                let item = match &section_card {
                    None => {
                        let location = chip.id.clone();
                        components::compact_hierarchy_item(format!("browse-chip-{location}"), chip_icon, chip.name.clone(), chip.path.clone().map(SharedString::from), child_count, download, focused, state.marquee, theme)
                            .debug_selector(|| format!("browse-chip-{}", chip.id))
                            .on_click(move |event, window, cx| {
                                if event.modifiers().control && split_page.upgrade().is_some_and(|page| page.read(cx).can_split(window)) {
                                    let _ = split_page.update(cx, |page, cx| page.request_split_child(location.clone(), BrowseSplitSide::Left, cx));
                                    cx.stop_propagation();
                                } else {
                                    let _ = open_page.update(cx, |page, cx| page.open_child(location.clone(), window, cx));
                                }
                            }).into_any_element()
                    }
                    Some(card) => {
                        let title_location = chip.id.clone();
                        let title_split_location = title_location.clone();
                        let title_page = page.clone();
                        let title_split_page = page.clone();
                        let pager = card.read(cx).pager();
                        let band_index = card.read(cx).band_index();
                        let direct_book_count = card.read(cx).direct_book_count();
                        let title = components::section_card_heading(format!("browse-card-title-{location}"), chip.name.clone(), direct_book_count, pager.into_any_element(), focused, state.marquee)
                            .debug_selector(|| format!("browse-card-title-{}", chip.id))
                            .on_click(move |event, window, cx| {
                                if event.modifiers().control && title_split_page.upgrade().is_some_and(|page| page.read(cx).can_split(window)) {
                                    let _ = title_split_page.update(cx, |page, cx| page.request_split_child(title_split_location.clone(), BrowseSplitSide::Left, cx));
                                    cx.stop_propagation();
                                } else {
                                    let _ = title_page.update(cx, |page, cx| page.open_child(title_location.clone(), window, cx));
                                }
                            });
                        components::section_card_frame(theme).child(components::section_card_band(band_index, title)).child(card.clone()).into_any_element()
                    }
                };
                // A chip inside a selection leaves its secondary click to the
                // paginator, which opens one menu over everything selected. On its
                // own, a chip opens its own menu here: the chip's element is a
                // button, so the click is taken on this wrapper.
                // A section card fills the cell its policy sized; a chip is its
                // own height.
                let frame = div().id(SharedString::from(format!("browse-chip-frame-{}", chip.id))).w_full();
                let mut frame = if section_card.is_some() { frame.relative().h_full() } else { frame.h(height) }.child(item).on_aux_click(move |event, window, cx| {
                    if event.modifiers().control && event.is_secondary() && menu_page.upgrade().is_some_and(|page| page.read(cx).can_split(window)) {
                        let _ = menu_page.update(cx, |page, cx| page.request_split_child(split_location.clone(), BrowseSplitSide::Right, cx));
                        cx.stop_propagation();
                        return;
                    }
                    if !event.is_secondary() || in_selection {
                        return;
                    }
                    let _ = menu_page.update(cx, |page, cx| page.open_items_menu(chip_index, event.position(), window, cx));
                    cx.stop_propagation();
                });
                // Folders are both ends of a drag: a chip can be picked up, and a
                // chip is where anything picked up can be put down.
                if let Some(source) = source {
                    if let Ok(destination) = DirId::parse_str(&chip.id) {
                        frame = folder_drop_target(frame, destination, SharedString::from(chip.name.clone()), true, page.clone(), theme);
                    }
                    let indices = acted_on(chip_index, &state.selection);
                    if let Some(targets) = folder_targets(&indices, &drag_chips, &drag_groups, &drag_books, source, cx) {
                        frame = folder_drag_source(frame, targets, drag_preview_items(&indices, &drag_chips, &drag_books, chip_icon), detailed_books);
                    }
                }
                frame.into_any_element()
            })
            .with_intrinsic_width(move |window, cx| components::compact_hierarchy_item_width(chip_icon, width_label.clone(), child_count, show_download, window, cx))
            .with_activate(move |window, cx| {
                let _ = activate_page.update(cx, |page, cx| page.open_child(activate_location.clone(), window, cx));
            }),
        );
    }

    for (book_index, book) in books.iter().enumerate() {
        let prefetched_book = book.clone();
        let rendered_book = book.clone();
        let activated_book = book.clone();
        let drag_chips = chips.clone();
        let drag_groups = chip_groups.to_vec();
        let drag_books = books.clone();
        // Books are declared after chips, so a book's flat child index is its
        // position plus the chip count.
        let child_index = chips.len() + book_index;
        let child = PaginatorChild::new(move |state, _, cx| {
            // Identified because a drag source has to be a stateful element.
            let mut frame = div().id(SharedString::from(format!("browse-book-frame-{child_index}"))).w_full().h_full().min_w_0().child(rendered_book.clone());
            let indices = acted_on(child_index, &state.selection);
            if let Some(source) = source
                && let Some(targets) = folder_targets(&indices, &drag_chips, &drag_groups, &drag_books, source, cx)
            {
                frame = folder_drag_source(frame, targets, drag_preview_items(&indices, &drag_chips, &drag_books, chip_icon), detailed_books);
            }
            frame.into_any_element()
        })
        .with_activate(move |window, cx| BookCardView::activate(&activated_book, window, cx))
        .with_prepare(move |window, cx| BookCardView::prefetch_for_window(&prefetched_book, window, cx));
        book_children.push(child);
    }

    let chip_width = PaginatorWidthPolicy::pixels(px(1.0));
    let mut groups = Vec::with_capacity(2);
    if !chip_children.is_empty() {
        let policy = if section_cards.is_empty() {
            PaginatorGroupPolicy::new(chip_width, PaginatorSizing::rems(rems(components::COMPACT_HIERARCHY_ITEM_HEIGHT_REM)), px(components::COMPACT_HIERARCHY_ITEM_ROW_GAP))
            .with_gap(px(components::COMPACT_HIERARCHY_ITEM_ROW_GAP))
            .intrinsic()
            .flush_edges()
            .with_edge_inset(px(components::BOOK_CARD_PADDING))
        } else {
            browse_section_card_policy()
        };
        let mut children = chip_children.into_iter();
        let mut offset = 0;
        for section in chip_groups {
            if section.start > offset {
                groups.push(PaginatorGroup::new(policy, children.by_ref().take(section.start - offset)));
            }
            let label = section.parent.name.clone();
            let location = section.parent.id.clone();
            groups.push(PaginatorGroup::new(policy, children.by_ref().take(section.end - section.start)).with_header(PaginatorSizing::rems(rems(1.5)).with_pixels(px(components::SPACE_SM * 2.0)), move |_, cx| {
                let theme = components::browser_theme(cx);
                let location = location.clone();
                let selector = format!("browse-group-{}", location);
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .size_full()
                    .py(px(components::SPACE_SM))
                    .flex()
                    .items_center()
                    .gap(px(components::SPACE_SM))
                    .child(div().id("group-title").min_w_0().text_ellipsis().line_clamp(1).text_size(rems(components::TEXT_MD)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme.text).child(label.clone()))
                    .child(div().flex_1().h(px(1.0)).bg(theme.rule))
                    .into_any_element()
            }));
            offset = section.end;
        }
        let remaining = children.collect::<Vec<_>>();
        if !remaining.is_empty() {
            groups.push(PaginatorGroup::new(policy, remaining));
        }
    }
    if !book_children.is_empty() {
        groups.push(PaginatorGroup::new(book_card_policy(detailed_books, cover_text), book_children));
    }
    if show_trash {
        let trash_index = chips.len() + books.len();
        let open_from_click = open_trash.clone();
        let open_from_keyboard = open_trash;
        let card_policy = !section_cards.is_empty();
        let item = PaginatorChild::new(move |state, window, cx| {
            let theme = components::browser_theme(cx);
            let height = rems(components::COMPACT_HIERARCHY_ITEM_HEIGHT_REM).to_pixels(window.rem_size());
            let open = open_from_click.clone();
            if card_policy {
                let focused = state.cursor == Some(trash_index);
                div()
                    .id("browse-virtual-trash-card")
                    .relative()
                    .w_full()
                    .h_full()
                    .cursor_pointer()
                    .on_click(move |_, window, cx| open(window, cx))
                    .child(
                        components::section_card_frame(theme)
                            .child(div().h(rems(components::SECTION_CARD_HEADING_HEIGHT_REM)).flex().items_center().text_size(rems(components::TEXT_LG)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(if focused { theme.text_accent } else { theme.text }).child("Trash"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(components::SPACE_SM))
                                    .child(div().size(px(48.0)).rounded(px(components::RADIUS_MD)).flex().items_center().justify_center().bg(theme.accent_wash).text_color(theme.text_accent).child(components::navigation_icon(components::NavigationIcon::Trash2)))
                                    .child(div().text_size(rems(components::TEXT_SM)).text_color(theme.text_muted).child("Deleted books and folders")),
                            ),
                    )
                    .into_any_element()
            } else {
                let button = components::compact_hierarchy_item(
                    "browse-virtual-trash",
                    components::NavigationIcon::Trash2,
                    "Trash",
                    None,
                    None,
                    None,
                    state.cursor == Some(trash_index),
                    state.marquee,
                    theme,
                )
                .on_click(move |_, window, cx| open(window, cx));
                div().id("browse-virtual-trash-row").w_full().h(height).child(button).into_any_element()
            }
        })
        .with_activate(move |window, cx| {
            open_from_keyboard(window, cx);
        });
        let policy = if card_policy {
            browse_section_card_policy()
        } else {
            PaginatorGroupPolicy::new(PaginatorWidthPolicy::pixels(px(1.0)), PaginatorSizing::rems(rems(components::COMPACT_HIERARCHY_ITEM_HEIGHT_REM)), px(components::COMPACT_HIERARCHY_ITEM_ROW_GAP))
                .with_gap(px(components::COMPACT_HIERARCHY_ITEM_ROW_GAP))
                .intrinsic()
                .flush_edges()
                .with_edge_inset(px(components::BOOK_CARD_PADDING))
        };
        groups.push(PaginatorGroup::new(policy, [item]));
    }
    groups
}
