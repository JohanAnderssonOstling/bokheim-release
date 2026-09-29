//! PDF reader action dispatch and chrome/sidebar transitions.

use gpui::{Context, Window};

use crate::invalidation::Component;
use crate::shell::state_panel::{self, SidebarTab};
use crate::shell::toolbar::SidebarTarget;

use super::PdfReaderView;

impl PdfReaderView {
    pub(super) fn open_search(&mut self, cx: &mut Context<Self>) {
        if self.search_match.is_none() {
            self.search_match = Some((0, 0));
            crate::invalidation::notify(cx, Component::PdfShell, "search_opened");
        }
    }

    /// The contents/annotations/settings tab strip shared with the EPUB reader.
    pub(super) fn render_sidebar_tabs(&self, active: SidebarTab, outline_available: bool, theme: ui_components::BrowserTheme, cx: &mut Context<Self>) -> gpui::AnyElement {
        state_panel::reader_sidebar_tabs("pdf-sidebar-tabs", &cx.entity(), active, outline_available, theme, |tab, this, cx| {
            state_panel::select_tab(&mut this.sidebar, tab, cx);
            crate::invalidation::notify(cx, Component::PdfShell, "sidebar_tab_changed");
        })
    }
}

impl SidebarTarget for PdfReaderView {
    #[cfg(feature = "kobo")]
    fn brightness_controls(&self) -> gpui::Entity<crate::shell::brightness::BrightnessControls> {
        self.brightness.clone()
    }

    fn close_reader(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        (self.close_reader)(window, cx);
    }

    fn hide_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) {
        if ui_components::uses_mobile_navigation(window) {
            if self.sidebar.close() {
                crate::invalidation::notify(cx, Component::PdfShell, "sidebar_closed");
            }
        } else {
            self.chrome.hide(cx);
        }
    }

    fn search_is_open(&self) -> bool {
        self.search_match.is_some()
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::shell::input::CommandTarget::execute(self, &crate::ToggleSearch, window, cx);
    }
}
