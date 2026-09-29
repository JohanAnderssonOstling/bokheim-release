//! PDF implementations of shared navigation commands.

use gpui::{Context, Focusable, Window};
use ui_components as components;

use crate::invalidation::Component;
use crate::shell::input::CommandTarget;
use crate::shell::search::SearchBarTarget;
use crate::shell::state_panel::{self, SidebarTab};
use crate::shell::toolbar::SidebarTarget;
use crate::{CloseSearch, DecreaseScale, FocusReaderSearch, IncreaseScale, NextLine, NextPage, PreviousLine, PreviousPage, ReturnToLibrary, ToggleSearch, ToggleToc};

use super::{PdfReaderView, ZOOM_STEP};

impl CommandTarget<PreviousPage> for PdfReaderView {
    fn execute(&mut self, _: &PreviousPage, window: &mut Window, cx: &mut Context<Self>) {
        if components::uses_mobile_navigation(window) {
            self.chrome.hide(cx);
        }
        if self.pdf.read(cx).can_previous_page() {
            self.pdf.update(cx, |pdf, cx| pdf.previous_page(cx));
        }
    }
}

impl CommandTarget<NextPage> for PdfReaderView {
    fn execute(&mut self, _: &NextPage, window: &mut Window, cx: &mut Context<Self>) {
        if components::uses_mobile_navigation(window) {
            self.chrome.hide(cx);
        }
        if self.pdf.read(cx).can_next_page() {
            self.pdf.update(cx, |pdf, cx| pdf.next_page(cx));
        }
    }
}

impl CommandTarget<PreviousLine> for PdfReaderView {
    fn execute(&mut self, _: &PreviousLine, window: &mut Window, cx: &mut Context<Self>) {
        if components::uses_mobile_navigation(window) {
            self.chrome.hide(cx);
        }
        self.pdf.update(cx, |pdf, cx| pdf.previous_item(cx));
    }
}

impl CommandTarget<NextLine> for PdfReaderView {
    fn execute(&mut self, _: &NextLine, window: &mut Window, cx: &mut Context<Self>) {
        if components::uses_mobile_navigation(window) {
            self.chrome.hide(cx);
        }
        self.pdf.update(cx, |pdf, cx| pdf.next_item(cx));
    }
}

impl CommandTarget<ToggleToc> for PdfReaderView {
    fn execute(&mut self, _: &ToggleToc, _: &mut Window, cx: &mut Context<Self>) {
        self.chrome.show(cx);
        state_panel::select_tab(&mut self.sidebar, SidebarTab::Contents, cx);
        crate::invalidation::notify(cx, Component::PdfShell, "toc_visibility_changed");
    }
}

impl CommandTarget<FocusReaderSearch> for PdfReaderView {
    fn execute(&mut self, _: &FocusReaderSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome.show(cx);
        if components::uses_mobile_navigation(window) && self.sidebar.close() {
            crate::invalidation::notify(cx, Component::PdfShell, "sidebar_closed_for_search");
        }
        self.open_search(cx);
        self.search_input.read(cx).focus_handle(cx).focus(window, cx);
    }
}

impl CommandTarget<ToggleSearch> for PdfReaderView {
    fn execute(&mut self, _: &ToggleSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.chrome.show(cx);
        if self.search_match.is_some() {
            self.search_close(window, cx);
        } else {
            self.open_search(cx);
            if !components::uses_mobile_navigation(window) || !self.sidebar.is_visible() {
                self.search_input.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
    }
}

impl CommandTarget<CloseSearch> for PdfReaderView {
    fn execute(&mut self, _: &CloseSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search_close(window, cx);
    }
}

impl CommandTarget<IncreaseScale> for PdfReaderView {
    fn execute(&mut self, _: &IncreaseScale, _: &mut Window, cx: &mut Context<Self>) {
        self.adjust_zoom(ZOOM_STEP, cx);
    }
}

impl CommandTarget<DecreaseScale> for PdfReaderView {
    fn execute(&mut self, _: &DecreaseScale, _: &mut Window, cx: &mut Context<Self>) {
        self.adjust_zoom(-ZOOM_STEP, cx);
    }
}

/// Leaving the book is one behaviour with two entry points: this and the
/// toolbar's library button both go through `close_reader`.
impl CommandTarget<ReturnToLibrary> for PdfReaderView {
    fn execute(&mut self, _: &ReturnToLibrary, window: &mut Window, cx: &mut Context<Self>) {
        self.close_reader(window, cx);
    }
}
