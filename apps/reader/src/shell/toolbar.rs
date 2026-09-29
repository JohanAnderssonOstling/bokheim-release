//! Sidebar chrome shared by every reader surface.
//!
//! The sidebar is now the reader's only chrome: a header (back, title/progress
//! or inline search, search toggle) fixed above whatever tab strip and body
//! the caller has already built. There is no separate top toolbar and no
//! "toggle sidebar" button — `shell::chrome::ChromeState` hides and reveals
//! this whole element, and a mobile reader reaches it through the bottom bar
//! in `shell::page` instead.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, FocusHandle, Window};
use gpui_component::IconName;
use ui_components as components;
use super::state_panel::SidebarTab;


/// Operations provided by the entity that owns a reader sidebar.
pub(crate) trait SidebarTarget: Sized + 'static {
    #[cfg(feature = "kobo")]
    fn brightness_controls(&self) -> Entity<super::brightness::BrightnessControls>;
    fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>);
    fn hide_sidebar(&mut self, window: &Window, cx: &mut Context<Self>);
    fn search_is_open(&self) -> bool;
    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>);
}

/// The format-specific content placed into the common sidebar structure.
pub(crate) struct SidebarContent {
    title: String,
    progress_label: String,
    progress_detail: String,
    document_focus: Option<FocusHandle>,
    /// The three-tab strip (`state_panel::reader_sidebar_tabs`).
    tabs: AnyElement,
    /// The active tab's body: a TOC tree, annotation rows, or the settings
    /// panel — whichever `SidebarTab` is currently selected.
    body: AnyElement,
}

impl SidebarContent {
    pub(crate) fn new(title: String, progress_label: String, progress_detail: String, document_focus: Option<FocusHandle>, tabs: AnyElement, body: AnyElement) -> Self {
        Self { title, progress_label, progress_detail, document_focus, tabs, body }
    }
}

/// `owner` is already leased by GPUI during rendering. Read state through it;
/// the entity handle is only for callbacks that run after rendering completes.
pub(crate) fn reader_sidebar<C: SidebarTarget>(owner: &C, target: &Entity<C>, content: SidebarContent, search: Option<AnyElement>, theme: components::BrowserTheme, window: &Window) -> AnyElement {
    let library_target = target.clone();
    let sidebar_target = target.clone();
    let search_target = target.clone();
    let document_focus = content.document_focus;

    let heading = gpui::div().min_w_0().flex_1().flex().items_center().gap(gpui::px(components::SPACE_SM)).child(components::reader_toolbar_title(content.title)).child(components::reader_progress(
        content.progress_label,
        content.progress_detail,
        theme,
    ));

    let header = gpui::div()
        .flex_none()
        .w_full()
        .min_w_0()
        .h(gpui::rems(components::READER_TOOLBAR_SIZE_REM))
        .flex()
        .items_center()
        .bg(theme.page_bg)
        .when(!components::uses_mobile_navigation(window), |header| {
            header.child(components::reader_control_button_icon("reader-library", "Return to library", IconName::ArrowLeft, theme).on_click(move |_, window, cx| {
                library_target.update(cx, |this, cx| this.close_reader(window, cx));
            }))
        })
        .child(heading)
        .map(|header| {
            #[cfg(feature = "kobo")]
            let header = header.child(super::brightness::popover(owner.brightness_controls(), document_focus.as_ref(), theme, window));
            #[cfg(not(feature = "kobo"))]
            let header = {
                let _ = document_focus;
                header
            };
            header
        })
        .when(!components::uses_mobile_navigation(window), |header| {
            header.child(components::selectable_button(components::reader_control_button_icon("reader-search", if owner.search_is_open() { "Hide search" } else { "Search" }, IconName::Search, theme), owner.search_is_open(), theme).on_click(
                move |_, window, cx| {
                    search_target.update(cx, |this, cx| this.toggle_search(window, cx));
                },
            ))
        })
        .child(components::reader_control_button_icon("reader-sidebar-close", "Close sidebar", IconName::Close, theme).on_click(move |_, window, cx| {
            cx.stop_propagation();
            sidebar_target.update(cx, |this, cx| this.hide_sidebar(window, cx));
        }));

    let mobile = components::uses_mobile_navigation(window);
    gpui::div().size_full().min_h_0().min_w_0().flex().flex_col().bg(theme.page_bg).child(header).children(search).when(!mobile, |panel| panel.child(content.tabs)).child(gpui::div().flex_1().min_h_0().min_w_0().child(content.body)).into_any_element()
}

/// Mobile keeps its tab navigation at the bottom of the screen. It is shown
/// only while the reader controls are open; the mobile sidebar has no desktop
/// tab strip of its own.
pub(crate) fn reader_bottom_bar(
    id: &'static str,
    active: Option<SidebarTab>,
    contents_available: bool,
    search_active: bool,
    theme: components::BrowserTheme,
    on_select: std::rc::Rc<dyn Fn(SidebarTab, &mut Window, &mut gpui::App)>,
    on_search: std::rc::Rc<dyn Fn(&mut Window, &mut gpui::App)>,
) -> AnyElement {
    let mut bar = components::bottom_navigation(theme).id(id);
    bar = bar.child(components::bottom_navigation_item("reader-bottom-search", IconName::Search, "Search", search_active, theme).on_click(move |_, window, cx| on_search(window, cx)));
    for (index, (tab, label, icon)) in [
        (SidebarTab::Contents, "Contents", IconName::BookOpen),
        (SidebarTab::Annotations, "Annotations", IconName::File),
        (SidebarTab::Settings, "Settings", IconName::Settings),
    ].into_iter().enumerate() {
        if tab == SidebarTab::Contents && !contents_available {
            continue;
        }
        let on_select = on_select.clone();
        bar = bar.child(components::bottom_navigation_item((id, index), icon, label, Some(tab) == active, theme).on_click(move |_, window, cx| on_select(tab, window, cx)));
    }
    bar.into_any_element()
}

#[cfg(all(test, not(feature = "kobo")))]
mod tests {
    use super::*;

    struct TestReader {
        search_open: bool,
        draws: usize,
    }

    impl SidebarTarget for TestReader {
        fn close_reader(&mut self, _: &mut Window, _: &mut Context<Self>) {}
        fn hide_sidebar(&mut self, _: &Window, _: &mut Context<Self>) {}
        fn search_is_open(&self) -> bool {
            self.search_open
        }
        fn toggle_search(&mut self, _: &mut Window, _: &mut Context<Self>) {
            self.search_open = !self.search_open;
        }
    }

    impl gpui::Render for TestReader {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.draws += 1;
            let content = SidebarContent::new("Book".into(), "1 / 2".into(), String::new(), None, gpui::div().into_any_element(), gpui::div().into_any_element());
            reader_sidebar(self, &cx.entity(), content, None, components::BrowserTheme::default(), window)
        }
    }

    #[gpui::test]
    fn sidebar_can_be_built_while_its_reader_is_leased(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let reader = cx.add_window(|_, _| TestReader { search_open: false, draws: 0 });
        cx.update_window(reader.into(), |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        reader
            .update(cx, |reader, _, cx| {
                assert!(reader.draws > 0);
                reader.draws = 0;
                reader.search_open = true;
                cx.notify();
            })
            .unwrap();
        cx.update_window(reader.into(), |_, window, cx| window.draw(cx).clear(cx)).unwrap();
        reader.update(cx, |reader, _, _| assert!(reader.draws > 0)).unwrap();
    }
}
