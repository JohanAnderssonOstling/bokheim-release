//! PDF reader action dispatch and chrome/sidebar transitions.

use crate::invalidation::notify;
use std::{cell::Cell, rc::Rc};

use gpui::{Context, Focusable, Window};

use crate::invalidation::Component;
use crate::shell::state_panel::{self, SidebarTab};
use crate::shell::toolbar::SidebarTarget;
use crate::shell::{chrome::ChromeState, input::ReaderSidebarCommands, state_panel::SidebarState};

use super::PdfReaderView;

impl ReaderSidebarCommands for PdfReaderView {
    fn sidebar_controls(&mut self) -> (&mut SidebarState, &mut ChromeState) {
        (&mut self.sidebar, &mut self.chrome)
    }
}

impl PdfReaderView {
    pub(super) fn open_search(&mut self, cx: &mut Context<Self>) {
        if self.search_match.is_none() {
            self.search_match = Some((0, 0));
            notify(cx, Component::PdfShell, "search_opened");
        }
    }
}

impl SidebarTarget for PdfReaderView {
    #[cfg(feature = "kobo")]
    fn device_controls(&self) -> Option<gpui::AnyView> {
        self.device_controls.clone()
    }

    fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
        self.tts.close();
        (self.close_reader)(window, cx);
    }

    fn hide_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_sidebar_controls(window, cx);
    }

    fn sidebar_focus(&self) -> gpui::FocusHandle {
        self.sidebar_focus.clone()
    }

    fn rail_focus(&self) -> gpui::FocusHandle {
        self.rail_focus.clone()
    }

    fn keyboard_visibility(&self) -> Rc<Cell<bool>> {
        self.settings_navigation.keyboard_visibility()
    }

    fn sidebar_tab(&self) -> SidebarTab {
        self.sidebar.effective_or_default(self.contents_available())
    }

    fn contents_available(&self) -> bool {
        self.toc.has_entries()
    }

    fn select_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        crate::shell::state_panel::select_tab(&mut self.sidebar, tab, cx);
        self.chrome.show(cx);
        if tab == SidebarTab::Search {
            self.open_search(cx);
            self.search_input.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.focus_sidebar_body(window, cx);
        cx.notify();
    }

    fn toggle_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        let outline_available = self.toc.has_entries();
        state_panel::toggle_tab(&mut self.sidebar, tab, outline_available, cx);
        self.chrome.show(cx);
        if self.sidebar.is_visible() && tab == SidebarTab::Search {
            self.open_search(cx);
            self.search_input.read(cx).focus_handle(cx).focus(window, cx);
        } else if self.sidebar.is_visible() {
            self.focus_sidebar_body(window, cx);
        } else {
            self.focus_document(window, cx);
        }
        notify(cx, Component::PdfShell, "mobile_sidebar_tab_changed");
    }

    fn focus_sidebar_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_tab() == SidebarTab::Contents {
            self.toc_tree.update(cx, |tree, cx| {
                if tree.selected_index().is_none() {
                    tree.set_selected_index(Some(0), cx);
                }
                tree.focus(window, cx);
            });
        } else if self.sidebar_tab() == SidebarTab::Search {
            self.search_input.read(cx).focus_handle(cx).focus(window, cx);
        } else {
            self.sidebar_focus.focus(window, cx);
            if self.sidebar_tab() == SidebarTab::Notes {
                window.focus_next(cx);
                if !self.sidebar_focus.contains_focused(window, cx) {
                    self.sidebar_focus.focus(window, cx);
                }
            }
        }
    }

    fn settings_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.handle_settings_key(key, window, cx)
    }

    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    fn tts_mark(&self) -> Option<ui_components::ReaderRailMark> {
        self.tts.available().then(|| self.tts.button_mark())
    }

    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    fn press_tts(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        crate::shell::tts::TtsReader::press_tts(self, cx);
    }
}
