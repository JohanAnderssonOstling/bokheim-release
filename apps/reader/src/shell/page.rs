//! Common reader page assembly and action routing.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FocusHandle, Window, div, px};
use ui_components as components;

use crate::shell::input::{self, CommonReaderCommands};
use crate::shell::state_panel::SidebarTab;
use crate::shell::toolbar::{SidebarTarget, reader_bottom_bar, reader_rail, reader_top_bar};
use crate::{CloseSearch, DecreaseScale, FocusReaderSearch, IncreaseScale, NextLine, NextPage, NextSidebarTab, OpenReaderSettings, PreviousLine, PreviousPage, PreviousSidebarTab, ReturnToLibrary, ToggleSearch, ToggleSidebar, ToggleToc};

/// The phone's chrome around the page.
pub(crate) struct MobileChrome {
    pub(crate) top_bar: Option<AnyElement>,
    pub(crate) bottom_bar: Option<AnyElement>,
    /// The TTS dock, while it is open.
    pub(crate) dock: Option<AnyElement>,
    /// A sheet covers the page and the dock.
    pub(crate) sheet: Option<AnyElement>,
    pub(crate) partial_sheet: bool,
}

/// What a reader hands the shell to lay out. The shell decides between the
/// desktop split and the phone's bars and sheets, so neither reader does.
pub(crate) struct ReaderSurface {
    pub(crate) document: AnyElement,
    /// The TTS dock, while it is open.
    pub(crate) dock: Option<AnyElement>,
    /// The sidebar panel, while one is showing.
    pub(crate) sidebar: Option<AnyElement>,
    pub(crate) active_tab: Option<SidebarTab>,
    pub(crate) chrome_visible: bool,
    pub(crate) document_ready: bool,
    pub(crate) loading_focus: FocusHandle,
    pub(crate) title: String,
    pub(crate) chapter: String,
    pub(crate) progress_label: String,
    pub(crate) progress_fraction: f32,
    pub(crate) progress_detail: String,
    pub(crate) bottom_bar_id: &'static str,
}

struct ReaderPage {
    document_ready: bool,
    loading_focus: FocusHandle,
    chrome_visible: bool,
    mobile_chrome: MobileChrome,
    content: AnyElement,
}

pub(crate) fn reader_surface<C: CommonReaderCommands + SidebarTarget>(owner: &C, surface: ReaderSurface, theme: components::BrowserTheme, window: &mut Window, cx: &mut Context<C>) -> Div {
    let ReaderSurface { document, dock, sidebar, active_tab, chrome_visible, document_ready, loading_focus, title, chapter, progress_label, progress_fraction, progress_detail, bottom_bar_id } = surface;
    let document = if window.supports_region_capture() { super::screenshot::document(document, cx.entity_id(), theme) } else { document };
    if !components::uses_mobile_reader_controls(window) {
        let rail = reader_rail(owner, &cx.entity(), active_tab.filter(|_| sidebar.is_some()), progress_fraction, progress_label, progress_detail, theme, window, cx);
        let document = div().size_full().min_h_0().min_w_0().flex().flex_col().child(document).children(dock).into_any_element();
        let content = super::chrome::desktop_content(rail.into_any_element(), sidebar, document, window, cx);
        let no_bars = MobileChrome { top_bar: None, bottom_bar: None, dock: None, sheet: None, partial_sheet: false };
        return reader_page(ReaderPage { document_ready, loading_focus, chrome_visible, mobile_chrome: no_bars, content }, theme, window, cx);
    }
    let target = cx.entity();
    let top_bar = chrome_visible.then(|| reader_top_bar(&target, title, chapter, progress_label, owner.available_tabs().contains(&SidebarTab::Search), theme, cx));
    let bottom_bar = chrome_visible.then(|| reader_bottom_bar(bottom_bar_id, owner, &target, active_tab, theme, cx));
    let mobile_chrome = MobileChrome { top_bar, bottom_bar, dock, sheet: sidebar, partial_sheet: active_tab == Some(SidebarTab::Settings) };
    reader_page(ReaderPage { document_ready, loading_focus, chrome_visible, mobile_chrome, content: document }, theme, window, cx)
}

fn reader_page<C: CommonReaderCommands>(page: ReaderPage, theme: components::BrowserTheme, window: &mut Window, cx: &mut Context<C>) -> Div {
    let ReaderPage { document_ready, loading_focus, chrome_visible, mobile_chrome, content } = page;
    input::install_reader_shortcuts(window, cx);
    let safe_area = window.insets().effective();
    let mobile = components::uses_mobile_reader_controls(window);

    let page = components::reader_app_page(window, cx, theme)
        .relative()
        .key_context("Reader")
        .when(!mobile, |page| page.pt(safe_area.top))
        .when(!document_ready, |page| page.track_focus(&loading_focus))
        .on_action(cx.listener(input::dispatch::<C, PreviousPage>))
        .on_action(cx.listener(input::dispatch::<C, NextPage>))
        .on_action(cx.listener(input::dispatch::<C, PreviousLine>))
        .on_action(cx.listener(input::dispatch::<C, NextLine>))
        .on_action(cx.listener(input::dispatch::<C, NextSidebarTab>))
        .on_action(cx.listener(input::dispatch::<C, PreviousSidebarTab>))
        .on_action(cx.listener(input::dispatch::<C, ToggleSidebar>))
        .on_action(cx.listener(input::dispatch::<C, ToggleToc>))
        .on_action(cx.listener(input::dispatch::<C, FocusReaderSearch>))
        .on_action(cx.listener(input::dispatch::<C, OpenReaderSettings>))
        .on_action(cx.listener(input::dispatch::<C, ToggleSearch>))
        .on_action(cx.listener(input::dispatch::<C, CloseSearch>))
        .on_action(cx.listener(input::dispatch::<C, IncreaseScale>))
        .on_action(cx.listener(input::dispatch::<C, DecreaseScale>))
        .on_action(cx.listener(input::dispatch::<C, ReturnToLibrary>));
    if !mobile {
        return page.child(components::reader_content().child(content));
    }

    // The bars cover the page rather than taking room from it, so showing and
    // hiding them never reflows the text. The dock does take room: while the
    // bars are hidden it sits under the page, and while they are shown it
    // rides above the bottom bar with an empty row of its height left in its
    // place, so the page keeps one height either way. The system's safe area
    // takes no room from the page: it changes as the bars show and hide, and
    // would reflow the text with them.
    let MobileChrome { top_bar, bottom_bar, dock, sheet, partial_sheet } = mobile_chrome;
    let sheet_open = sheet.is_some();
    let docked_in_bars = chrome_visible && dock.is_some();
    let (page_dock, bar_dock) = if docked_in_bars { (None, dock.filter(|_| !sheet_open)) } else { (dock, None) };
    let document =
        div().flex_1().min_h_0().flex().flex_col().child(components::reader_content().child(content)).children(page_dock).when(docked_in_bars, |document| document.child(div().flex_none().h(px(components::player_dock_height_px(window)))));
    let page = page.child(document).when(sheet_open && partial_sheet, |page| {
        // Keep the exposed page visible, but let the first tap only dismiss
        // the sheet instead of also following a link or turning a page.
        page.child(div().id("reader-sheet-dismiss").absolute().inset_0().occlude().on_scroll_wheel(|_, _, cx| cx.stop_propagation()).on_click(cx.listener(|reader, event, window, cx| {
            cx.stop_propagation();
            if super::chrome::is_page_tap(event) {
                input::dispatch(reader, &CloseSearch, window, cx);
            }
        })))
    });
    let page = page.children(sheet.map(|sheet| super::chrome::sidebar_sheet(sheet, partial_sheet, window)));
    let page = match top_bar.filter(|_| chrome_visible && !sheet_open) {
        Some(top_bar) => page.child(div().absolute().top_0().left_0().right_0().pt(safe_area.top).pl(safe_area.left).pr(safe_area.right).bg(theme.page_bg).occlude().child(top_bar)),
        None => page,
    };
    if bottom_bar.is_none() && bar_dock.is_none() {
        return page;
    }
    page.child(
        div()
            .id("reader-mobile-controls")
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .pb(safe_area.bottom)
            .pl(safe_area.left)
            .pr(safe_area.right)
            .bg(theme.page_bg)
            .flex()
            .flex_col()
            .occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .children(bar_dock)
            .children(bottom_bar),
    )
}

#[cfg(all(test, feature = "mobile"))]
mod tests {
    use std::any::Any;

    use super::*;
    use crate::shell::input::CommandTarget;
    use gpui::{AppContext, Entity, Render, TestAppContext, VisualTestContext, point, size};

    struct LayoutTest {
        focus: FocusHandle,
        chrome: bool,
        dock: bool,
        sheet: bool,
        partial: bool,
        document_downs: usize,
        document_clicks: usize,
    }

    impl<A: 'static> CommandTarget<A> for LayoutTest {
        fn execute(&mut self, action: &A, window: &mut Window, cx: &mut Context<Self>) {
            if (action as &dyn Any).is::<CloseSearch>() {
                self.sheet = false;
                self.focus.focus(window, cx);
                cx.notify();
            }
        }
    }

    impl Render for LayoutTest {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = components::BrowserTheme::default();
            let top_bar = self.chrome.then(|| components::ruled_topbar(theme).debug_selector(|| "test-top-bar".into()).into_any_element());
            let bottom_bar = self.chrome.then(|| components::bottom_navigation(theme).debug_selector(|| "test-bottom-bar".into()).into_any_element());
            let dock = self.dock.then(|| components::player_dock(div(), div(), div(), theme).debug_selector(|| "test-dock".into()).into_any_element());
            let sheet = self.sheet.then(|| div().size_full().debug_selector(|| "test-sheet".into()).bg(theme.page_bg).into_any_element());
            let content = components::reader_document_body()
                .id("test-document")
                .debug_selector(|| "test-document".into())
                .on_any_mouse_down(cx.listener(|view, _, _, _| view.document_downs += 1))
                .on_click(cx.listener(|view, _, _, _| view.document_clicks += 1))
                .into_any_element();
            reader_page(
                ReaderPage { document_ready: true, loading_focus: self.focus.clone(), chrome_visible: self.chrome, mobile_chrome: MobileChrome { top_bar, bottom_bar, dock, sheet, partial_sheet: self.partial }, content },
                theme,
                window,
                cx,
            )
        }
    }

    fn setup(cx: &mut TestAppContext, rem_size: f32) -> (Entity<LayoutTest>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| gpui_component::Theme::global_mut(cx).font_size = px(rem_size));
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| LayoutTest { focus: cx.focus_handle(), chrome: false, dock: true, sheet: false, partial: false, document_downs: 0, document_clicks: 0 });
            gpui_component::Root::new(view, window, cx)
        });
        let view = cx.update(|_, cx| root.read(cx).view().clone().downcast::<LayoutTest>().ok().unwrap());
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui::test]
    fn bars_overlay_the_document_and_sheets_cover_the_reserved_dock_space(cx: &mut TestAppContext) {
        for rem_size in [16.0, 24.0, 32.0] {
            let (view, cx) = setup(cx, rem_size);
            let reading = cx.debug_bounds("test-document").unwrap();
            let dock = cx.debug_bounds("test-dock").unwrap();
            assert_eq!(dock.size.height, px(5.0 * rem_size + 1.0));
            assert_eq!(reading.bottom(), dock.top());
            view.update(cx, |view, cx| {
                view.chrome = true;
                cx.notify();
            });
            cx.run_until_parked();
            assert_eq!(cx.debug_bounds("test-document").unwrap(), reading, "bars must not repaginate the book");
            let bottom_bar = cx.debug_bounds("test-bottom-bar").unwrap();
            assert_eq!(cx.debug_bounds("test-dock").unwrap().bottom(), bottom_bar.top());
            view.update(cx, |view, cx| {
                view.sheet = true;
                cx.notify();
            });
            cx.run_until_parked();
            assert!(cx.debug_bounds("test-dock").is_none());
            assert!(cx.debug_bounds("test-top-bar").is_none());
            assert_eq!(cx.debug_bounds("test-document").unwrap(), reading);
            let sheet = cx.debug_bounds("test-sheet").unwrap();
            assert_eq!(sheet.bottom(), bottom_bar.top(), "the sheet must cover the dock's reserved space");
            view.update(cx, |view, cx| {
                view.partial = true;
                cx.notify();
            });
            cx.run_until_parked();
            let partial = cx.debug_bounds("test-sheet").unwrap();
            assert!(partial.top() > sheet.top());
            assert_eq!(partial.bottom(), sheet.bottom());
        }
    }

    #[gpui::test]
    fn tapping_exposed_page_dismisses_sheet_without_activating_document(cx: &mut TestAppContext) {
        for viewport in [size(px(632.0), px(840.0)), size(px(840.0), px(632.0))] {
            for fraction in [0.1, 0.5, 0.9] {
                let (view, cx) = setup(cx, 16.0);
                cx.simulate_resize(viewport);
                view.update(cx, |view, cx| {
                    view.chrome = true;
                    view.sheet = true;
                    view.partial = true;
                    cx.notify();
                });
                cx.run_until_parked();
                let document = cx.debug_bounds("test-document").unwrap();
                let sheet = cx.debug_bounds("test-sheet").unwrap();
                cx.simulate_click(sheet.center(), Default::default());
                view.read_with(cx, |view, _| assert!(view.sheet, "tapping inside the sheet must keep it open"));

                let exposed = point(viewport.width * fraction, sheet.top() / 2.0);
                cx.simulate_click(exposed, Default::default());
                cx.run_until_parked();
                assert!(cx.debug_bounds("test-sheet").is_none());
                assert_eq!(cx.debug_bounds("test-document").unwrap(), document, "dismissing the sheet must not reflow the book");
                view.read_with(cx, |view, _| {
                    assert!(!view.sheet);
                    assert!(view.chrome, "dismissing the sheet keeps navigation visible");
                    assert_eq!((view.document_downs, view.document_clicks), (0, 0), "the dismissal tap must not reach the book");
                });

                cx.simulate_click(exposed, Default::default());
                view.read_with(cx, |view, _| assert_eq!((view.document_downs, view.document_clicks), (1, 1), "the next tap reaches the book again"));
            }
        }
    }
}
