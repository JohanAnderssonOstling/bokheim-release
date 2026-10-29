use gpui::prelude::*;
use gpui::{Anchor, AnyElement, Context, Entity, IntoElement, div};
use gpui_component::menu::{DropdownMenu as _, PopupMenu};
use gpui_component::{Icon, IconName};
use ui_components as components;

use super::page::BrowsePage;

use library_model::{BrowseBookSort, BrowseChipSort, BrowseLocation, FeedFacetGroup, LibraryBrowseQuery, LibraryFileTypeFilter, LibraryFormatCount, LibraryLanguageCount};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BrowseOptionsSelection {
    pub(crate) search: String,
    pub(super) formats: Vec<LibraryFileTypeFilter>,
    pub(super) chip_sort: BrowseChipSort,
    pub(super) book_sort: BrowseBookSort,
    pub(super) languages: Vec<String>,
    pub(super) hide_finished: bool,
    /// The catalog grouped by author rather than by subject.
    pub(super) by_author: bool,
}

impl BrowseOptionsSelection {
    pub(crate) fn for_controls(controls: &BrowseControls) -> Self {
        Self {
            search: String::new(),
            formats: Vec::new(),
            chip_sort: controls.collection_sorts.first().copied().unwrap_or_default(),
            book_sort: controls.book_sorts.first().copied().unwrap_or_default(),
            languages: Vec::new(),
            hide_finished: false,
            by_author: false,
        }
    }

    pub(crate) fn backend_query(&self, location: BrowseLocation, include_search: bool) -> LibraryBrowseQuery {
        LibraryBrowseQuery {
            location,
            search: if include_search { self.search.clone() } else { String::new() },
            file_types: self.formats.clone(),
            chip_sort: self.chip_sort,
            book_sort: self.book_sort,
            languages: super::languages::query_codes(&self.languages),
            hide_finished: self.hide_finished,
        }
    }

    /// Applies a choice from the sort and filter controls, reporting whether
    /// it changed anything. A choice the controls do not offer is ignored.
    pub(crate) fn apply(&mut self, event: BrowseOptionsEvent, controls: &BrowseControls) -> bool {
        match event {
            BrowseOptionsEvent::SelectChipSort(sort) => controls.collection_sorts.contains(&sort) && std::mem::replace(&mut self.chip_sort, sort) != sort,
            BrowseOptionsEvent::SelectBookSort(sort) => controls.book_sorts.contains(&sort) && std::mem::replace(&mut self.book_sort, sort) != sort,
            BrowseOptionsEvent::ToggleFormat(format) => toggle(&mut self.formats, format, &controls.formats),
            BrowseOptionsEvent::ToggleLanguage(language) => toggle(&mut self.languages, language, &controls.languages),
            BrowseOptionsEvent::ToggleHideFinished => {
                if !controls.hide_finished {
                    return false;
                }
                self.hide_finished = !self.hide_finished;
                true
            }
            BrowseOptionsEvent::ShowByAuthor(by_author) => controls.groupings && std::mem::replace(&mut self.by_author, by_author) != by_author,
            // Not a choice the selection keeps: the navigator opens the page.
            BrowseOptionsEvent::OpenFeedPage(_) => false,
        }
    }

    /// Whether a filter is narrowing the books. The search is not one: it has
    /// its own button. Nor is the grouping, which chooses what the page is, or
    /// a sort, which orders the books without hiding any.
    fn is_filtered(&self) -> bool {
        !self.formats.is_empty() || self.hide_finished
    }
}

/// Toggles `choice` in `selected`, or clears `selected` for `None`, the "any"
/// row. Reports whether anything changed.
fn toggle<T: PartialEq>(selected: &mut Vec<T>, choice: Option<T>, offered: &[Facet<T>]) -> bool {
    let Some(choice) = choice else {
        let changed = !selected.is_empty();
        selected.clear();
        return changed;
    };
    if !offered.iter().any(|facet| facet.value == choice) {
        return false;
    }
    match selected.iter().position(|item| *item == choice) {
        Some(index) => {
            selected.remove(index);
        }
        None => selected.push(choice),
    }
    true
}

/// A filter choice. `None` is the "any" row, which clears the filter.
#[derive(Clone)]
pub(crate) enum BrowseOptionsEvent {
    SelectChipSort(BrowseChipSort),
    SelectBookSort(BrowseBookSort),
    ToggleFormat(Option<LibraryFileTypeFilter>),
    ToggleLanguage(Option<String>),
    ToggleHideFinished,
    /// The catalog by author (`true`) or by subject.
    ShowByAuthor(bool),
    /// One of a feed's own sort or filter choices: the feed page it names.
    OpenFeedPage(BrowseLocation),
}

/// One value a filter offers, and how many books in the listing have it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Facet<T> {
    value: T,
    label: String,
    count: u32,
}

/// A filter is worth offering only where it would narrow the listing: it has
/// more than one value, or a value no book here has.
fn offered<T>(facets: Vec<Facet<T>>) -> Vec<Facet<T>> {
    if facets.len() > 1 || facets.iter().any(|facet| facet.count == 0) { facets } else { Vec::new() }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BrowseControls {
    /// Offers to hide finished books; catalog books have no reading history.
    pub(crate) hide_finished: bool,
    /// The catalog offers to group its books by subject or by author.
    pub(crate) groupings: bool,
    pub(crate) collection_sorts: Vec<BrowseChipSort>,
    pub(crate) book_sorts: Vec<BrowseBookSort>,
    formats: Vec<Facet<LibraryFileTypeFilter>>,
    languages: Vec<Facet<String>>,
    /// A feed page's own choices; `None` on a library page.
    feed: Option<Vec<FeedFacetGroup>>,
}

impl BrowseControls {
    pub(crate) fn library() -> Self {
        Self { hide_finished: true, groupings: false, collection_sorts: BrowseChipSort::ALL.to_vec(), book_sorts: BrowseBookSort::ALL.to_vec(), formats: Vec::new(), languages: Vec::new(), feed: None }
    }

    /// A feed page offers only the feed's own choices: the library's sorts and
    /// filters do not reach a page fetched from the feed.
    pub(crate) fn feed(facets: Vec<FeedFacetGroup>) -> Self {
        Self { hide_finished: false, groupings: false, collection_sorts: Vec::new(), book_sorts: Vec::new(), formats: Vec::new(), languages: Vec::new(), feed: Some(facets) }
    }

    pub(crate) fn library_with_facets(language_counts: &[LibraryLanguageCount], format_counts: &[LibraryFormatCount]) -> Self {
        let languages = super::languages::choices(language_counts).into_iter().map(|(value, label, count)| Facet { value, label, count }).collect();
        let formats = format_counts.iter().map(|count| Facet { value: count.format, label: count.format.label().into(), count: count.book_count }).collect();
        Self { formats: offered(formats), languages, ..Self::library() }
    }
}

/// Sort and filter choices use the same popup-menu surface as the Add action
/// on wide pointer-driven windows. Compact windows and touch targets use
/// `browse_options_sheet` below.
/// `menu_anchor` is the menu's corner at the trigger.
pub(super) fn browse_options_control(page: Entity<BrowsePage>, selection: &BrowseOptionsSelection, compact: bool, menu_anchor: Anchor, theme: components::BrowserTheme) -> AnyElement {
    // A filter narrowing the books is state the page holds, so the trigger
    // takes the solid accent while one is on — the same mark as an open search.
    let filtered = selection.is_filtered();
    let trigger = components::selectable_button(components::outlined_icon_button("browse-options", "Sort & filter", IconName::Settings2), filtered, theme);

    if compact || components::touch_ui() || cfg!(feature = "kobo") {
        return trigger.on_click(move |_, _, cx| page.update(cx, |page, cx| page.toggle_browse_options_sheet(cx))).into_any_element();
    }

    trigger
        .dropdown_menu_with_anchor(menu_anchor, move |menu, _, cx| options_menu(menu, page.clone(), false, cx))
        .into_any_element()
}

pub(super) fn browse_language_control(page: Entity<BrowsePage>, controls: &BrowseControls, selection: &BrowseOptionsSelection, compact: bool, anchor: Anchor) -> Option<AnyElement> {
    if controls.feed.is_some() || (controls.languages.is_empty() && selection.languages.is_empty()) {
        return None;
    }
    Some(components::outlined_icon_button("browse-languages", "Languages", Icon::empty().path("icons/translate.svg"))
        .dropdown_menu_with_anchor(anchor, move |menu, _, cx| options_menu(menu.touch_friendly(compact || components::touch_ui()), page.clone(), true, cx))
        .into_any_element())
}

fn options_menu(menu: PopupMenu, page: Entity<BrowsePage>, languages_only: bool, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let (controls, selection) = page.read(cx).options_state(cx);
    let sections = if languages_only {
        vec![OptionSection { divided: false, title: Some("Languages".into()), options: controls.language_options(&selection) }]
    } else {
        controls.options(&selection)
    };
    let mut menu = menu.scrollable(true).dismiss_on_select(controls.feed.is_some());
    for section in sections {
        if section.divided {
            menu = menu.separator();
        }
        if let Some(title) = section.title {
            menu = menu.item(components::menu_section(title));
        }
        for option in section.options {
            let page = page.clone();
            let target = cx.weak_entity();
            menu = menu.item(components::menu_item(option.label, move |_, window, cx| {
                page.update(cx, |page, cx| page.select_option(option.event.clone(), cx));
                let page = page.clone();
                let target = target.clone();
                // Rebuild after the click handler returns so the open menu reads
                // the current selection without updating its own borrowed entity.
                window.defer(cx, move |window, cx| {
                    if let Some(target) = target.upgrade() {
                        target.update(cx, |menu, cx| menu.rebuild(window, cx, |menu, _, cx| options_menu(menu, page, languages_only, cx)));
                    }
                });
            }).checked(option.checked));
        }
    }
    menu
}

/// The compact counterpart to `browse_options_control`'s dropdown: the same
/// sections, events, and read of `controls`/`selection`, laid out as full-width
/// rows in a bottom sheet instead of a small anchored popup, since the popup
/// menu widget used above has no bottom-sheet mode of its own.
pub(super) fn browse_options_sheet(page: Entity<BrowsePage>, controls: BrowseControls, selection: BrowseOptionsSelection, theme: components::BrowserTheme) -> AnyElement {
    let mut rows = div().flex().flex_col();

    for section in controls.options(&selection) {
        if section.divided {
            rows = rows.child(components::bottom_sheet_divider(theme));
        }
        if let Some(title) = section.title {
            rows = rows.child(components::bottom_sheet_section(title, theme));
        }
        for option in section.options {
            let page = page.clone();
            rows = rows.child(components::bottom_sheet_row(option.id, option.label, option.checked, theme).on_click(move |_, _, cx| {
                cx.stop_propagation();
                page.update(cx, |page, cx| page.select_option(option.event.clone(), cx));
            }));
        }
    }

    let header_dismiss_target = page.clone();
    let header = components::bottom_sheet_header("Sort & filter", theme).child(
        components::bottom_sheet_close_button("browse-options-sheet-close", theme).on_click(move |_, _, cx| header_dismiss_target.update(cx, |page, cx| page.close_browse_options_sheet(cx))),
    );

    let sheet = components::bottom_sheet_surface(theme).id("browse-options-sheet").child(header).child(div().id("browse-options-sheet-rows").flex_1().min_h_0().overflow_y_scroll().child(rows));

    components::bottom_sheet_overlay().child(components::modal_scrim("browse-options-sheet-scrim", theme).on_click(move |_, _, cx| page.update(cx, |page, cx| page.close_browse_options_sheet(cx)))).child(sheet).into_any_element()
}

/// Choices are data shared by desktop menus and mobile sheets, not operations.
struct OptionRow {
    id: String,
    label: String,
    checked: bool,
    event: BrowseOptionsEvent,
}
struct OptionSection {
    /// Ruled off from what is above it; only a leading "Show by" or "Sort
    /// books" is not.
    divided: bool,
    title: Option<String>,
    options: Vec<OptionRow>,
}

impl BrowseControls {
    fn options(&self, selection: &BrowseOptionsSelection) -> Vec<OptionSection> {
        use BrowseOptionsEvent::*;
        if let Some(groups) = &self.feed {
            return groups
                .iter()
                .enumerate()
                .map(|(index, group)| OptionSection {
                    divided: index > 0,
                    title: Some(group.title.clone()),
                    options: group.choices.iter().map(|choice| OptionRow { id: format!("feed-facet-{}", choice.location), label: choice.title.clone(), checked: choice.active, event: OpenFeedPage(choice.location) }).collect(),
                })
                .collect();
        }
        // The grouping comes first: it decides what the rest sorts and filters.
        [
            (Some("Show by"), if self.groupings { vec![ShowByAuthor(false), ShowByAuthor(true)] } else { Vec::new() }),
            (Some("Sort books"), self.book_sorts.iter().copied().map(SelectBookSort).collect::<Vec<_>>()),
            (Some("Sort collections"), self.collection_sorts.iter().copied().map(SelectChipSort).collect()),
            (Some("Format"), facet_events(&self.formats, &selection.formats, ToggleFormat)),
            (None, self.hide_finished.then_some(ToggleHideFinished).into_iter().collect()),
        ]
        .into_iter()
        .filter(|(_, events)| !events.is_empty())
        .enumerate()
        .map(|(index, (title, events))| OptionSection { divided: index > 0 || !matches!(title, Some("Show by" | "Sort books")), title: title.map(str::to_owned), options: events.into_iter().map(|event| self.option(event, selection)).collect() })
        .collect()
    }

    fn language_options(&self, selection: &BrowseOptionsSelection) -> Vec<OptionRow> {
        facet_events(&self.languages, &selection.languages, BrowseOptionsEvent::ToggleLanguage)
            .into_iter().map(|event| self.option(event, selection)).collect()
    }

    fn option(&self, event: BrowseOptionsEvent, selection: &BrowseOptionsSelection) -> OptionRow {
        use BrowseOptionsEvent::*;
        let (id, label, checked) = match event.clone() {
            SelectBookSort(sort) => (format!("sheet-book-sort-{}", sort.label()), sort.label().into(), sort == selection.book_sort),
            SelectChipSort(sort) => (format!("sheet-chip-sort-{}", sort.label()), sort.label().into(), sort == selection.chip_sort),
            ToggleFormat(None) => ("sheet-format-All".into(), "All".into(), selection.formats.is_empty()),
            ToggleFormat(Some(format)) => (format!("sheet-format-{}", format.label()), facet_label(&self.formats, format), selection.formats.contains(&format)),
            ToggleLanguage(None) => ("sheet-language-any".into(), "Any".into(), selection.languages.is_empty()),
            ToggleLanguage(Some(language)) => {
                let checked = selection.languages.contains(&language);
                let label = facet_label(&self.languages, language.clone());
                (format!("sheet-language-{language}"), label, checked)
            }
            ToggleHideFinished => ("sheet-hide-finished".into(), "Hide finished".into(), selection.hide_finished),
            ShowByAuthor(false) => ("sheet-show-by-subject".into(), "Subject".into(), !selection.by_author),
            ShowByAuthor(true) => ("sheet-show-by-author".into(), "Author".into(), selection.by_author),
            OpenFeedPage(_) => unreachable!("a feed's choices are rows of their own"),
        };
        OptionRow { id, label, checked, event }
    }
}

/// A filter's rows: "any" and then each value offered. A filter no longer
/// offered keeps its "any" row while something is still selected in it, so
/// the selection can be cleared.
fn facet_events<T: Clone>(facets: &[Facet<T>], selected: &[T], event: fn(Option<T>) -> BrowseOptionsEvent) -> Vec<BrowseOptionsEvent> {
    if facets.is_empty() && selected.is_empty() {
        return Vec::new();
    }
    std::iter::once(event(None)).chain(facets.iter().map(|facet| event(Some(facet.value.clone())))).collect()
}

fn facet_label<T: PartialEq>(facets: &[Facet<T>], value: T) -> String {
    facets.iter().find(|facet| facet.value == value).map_or_else(String::new, |facet| format!("{} ({})", facet.label, facet.count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_choices_follow_offered_defaults_and_ignore_unoffered_actions() {
        let mut controls = BrowseControls::library();
        controls.collection_sorts = vec![BrowseChipSort::DefaultOrder, BrowseChipSort::BookCount];
        controls.book_sorts = vec![BrowseBookSort::DefaultOrder, BrowseBookSort::Alphabetical];
        let mut selection = BrowseOptionsSelection::for_controls(&controls);
        let query = selection.backend_query(BrowseLocation::Books, true);
        assert_eq!(query.chip_sort, BrowseChipSort::DefaultOrder);
        assert_eq!(query.book_sort, BrowseBookSort::DefaultOrder);
        assert!(!selection.apply(BrowseOptionsEvent::SelectBookSort(BrowseBookSort::DefaultOrder), &controls));
        assert!(!selection.apply(BrowseOptionsEvent::SelectChipSort(BrowseChipSort::DefaultOrder), &controls));
        assert!(selection.apply(BrowseOptionsEvent::SelectBookSort(BrowseBookSort::Alphabetical), &controls));
        assert!(selection.apply(BrowseOptionsEvent::SelectChipSort(BrowseChipSort::BookCount), &controls));
        let checked = controls.options(&selection).into_iter().flat_map(|section| section.options).filter(|row| row.checked).map(|row| row.label).collect::<Vec<_>>();
        assert_eq!(checked, ["Alphabetical", "Book count"]);
        let snapshot = selection.clone();
        assert!(!selection.apply(BrowseOptionsEvent::SelectBookSort(BrowseBookSort::Popularity), &controls));
        assert!(!selection.apply(BrowseOptionsEvent::SelectChipSort(BrowseChipSort::Popularity), &controls));
        assert_eq!(selection, snapshot);

        let feed = BrowseControls::feed(Vec::new());
        let mut selection = BrowseOptionsSelection::for_controls(&feed);
        let snapshot = selection.clone();
        let query = selection.backend_query(BrowseLocation::Books, true);
        assert_eq!(query.chip_sort, BrowseChipSort::Alphabetical);
        assert_eq!(query.book_sort, BrowseBookSort::Alphabetical);
        assert!(!selection.apply(BrowseOptionsEvent::SelectBookSort(BrowseBookSort::DefaultOrder), &feed));
        assert!(!selection.apply(BrowseOptionsEvent::SelectChipSort(BrowseChipSort::BookCount), &feed));
        assert_eq!(selection, snapshot);
        assert!(feed.options(&selection).is_empty());
    }

    #[test]
    fn catalog_languages_allow_deselecting_english_and_any() {
        let controls = BrowseControls::library_with_facets(&[
            LibraryLanguageCount { language: "en".into(), book_count: 100 },
            LibraryLanguageCount { language: "sv".into(), book_count: 1522 },
            LibraryLanguageCount { language: "he".into(), book_count: 2 },
        ], &[]);
        let mut selection = BrowseOptionsSelection::for_controls(&controls);
        selection.languages = vec!["en".into(), "sv".into()];
        assert!(selection.apply(BrowseOptionsEvent::ToggleLanguage(Some("en".into())), &controls));
        assert_eq!(selection.languages, ["sv"]);
        for location in [BrowseLocation::Catalog(0), BrowseLocation::CatalogAuthor(None)] {
            assert_eq!(selection.backend_query(location, true).languages, ["sv", "swe"]);
        }
        assert!(controls.options(&selection).iter().all(|section| section.title.as_deref() != Some("Language")));
        let rows = controls.language_options(&selection);
        assert!(rows.iter().any(|row| row.id == "sheet-language-en" && row.label == "English (100)" && !row.checked));
        assert!(rows.iter().any(|row| row.label.starts_with("Hebrew")));
        assert!(selection.apply(BrowseOptionsEvent::ToggleLanguage(Some("en".into())), &controls));
        assert_eq!(selection.backend_query(BrowseLocation::Catalog(0), true).languages, ["en", "eng", "sv", "swe"]);
        assert!(selection.apply(BrowseOptionsEvent::ToggleLanguage(Some("sv".into())), &controls));
        assert_eq!(selection.languages, ["en"]);
        assert!(selection.apply(BrowseOptionsEvent::ToggleLanguage(None), &controls));
        assert!(selection.backend_query(BrowseLocation::Catalog(0), true).languages.is_empty());
        assert!(selection.apply(BrowseOptionsEvent::ToggleLanguage(Some("sv".into())), &controls));
        assert_eq!(selection.backend_query(BrowseLocation::Catalog(0), true).languages, ["sv", "swe"]);
        let rows = controls.language_options(&selection);
        assert!(rows.iter().any(|row| row.label.starts_with("Hebrew")));
        assert!(rows.iter().any(|row| row.id == "sheet-language-en" && !row.checked));
        assert!(rows.iter().all(|row| row.label != "Automatic"));
    }

    #[test]
    fn library_languages_reuse_aliases_without_requiring_english() {
        let controls = BrowseControls::library_with_facets(&[
            LibraryLanguageCount { language: "eng".into(), book_count: 10 },
            LibraryLanguageCount { language: "swe".into(), book_count: 5 },
        ], &[]);
        let mut selection = BrowseOptionsSelection::for_controls(&controls);
        assert!(selection.apply(BrowseOptionsEvent::ToggleLanguage(Some("sv".into())), &controls));
        assert_eq!(selection.backend_query(BrowseLocation::Books, true).languages, ["sv", "swe"]);
    }
}
