//! EPUB implementations of shared navigation commands.

use crate::invalidation::notify;
use gpui::{Context, Window};
use html_view_gpui::HtmlPageSnapshot;
use ui_components::PageTurn;

use crate::epub::{LoadState, ReaderView};
use crate::invalidation::Component;
use crate::shell::input::CommandTarget;
use crate::shell::state_panel::SidebarTab;
use crate::shell::toolbar::SidebarTarget;
use crate::{
    CloseSearch, DecreaseColumnWidth, DecreaseFont, DecreaseScale, FocusReaderSearch, IncreaseColumnWidth, IncreaseFont, IncreaseScale, NextLine, NextPage, NextSection, NextSidebarTab, PreviousLine, PreviousPage, PreviousSection,
    PreviousSidebarTab, ReturnToLibrary, ToggleFullscreen, ToggleSearch, ToggleSidebar, ToggleToc,
};

impl CommandTarget<PreviousPage> for ReaderView {
    fn execute(&mut self, _: &PreviousPage, window: &mut Window, cx: &mut Context<Self>) {
        self.turn_page(false, window, cx);
    }
}

impl ReaderView {
    /// Every page navigation source shares one transition, including taps,
    /// keys, wheel bursts and completed Android swipes.
    pub(super) fn turn_page(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_selection(cx);
        self.chrome.hide_for_motion(window, cx);
        let LoadState::Ready(renderer) = &self.load_state else { return };
        let renderer = renderer.clone();
        let outgoing = PageTurn::<HtmlPageSnapshot>::enabled(cx).then(|| renderer.update(cx, |view, _| view.page_snapshot())).flatten();
        let moved = renderer.update(cx, |renderer, cx| if forward { renderer.next_page(cx) } else { renderer.previous_page(cx) });
        if moved {
            let drag = self.swipe_preview.take_offset();
            self.page_turn = outgoing.and_then(|outgoing| PageTurn::new(outgoing, forward, drag, self.page_size.unwrap_or(window.viewport_size()), cx));
        } else {
            // The first/last page is a rejected turn, just like a short drag.
            self.swipe_preview.settle();
        }
        cx.notify();
    }

    pub(crate) fn native_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.image_preview.take().is_some() || self.footnote.take().is_some() || self.context_sheet.take().is_some() {
            cx.notify();
        } else if self.marks.selected().is_some() {
            self.close_annotation_popup(cx);
            self.focus_document(window, cx);
        } else if !self.dismiss_selection(cx) {
            if self.sidebar.is_visible() && self.chrome.is_visible() {
                self.hide_sidebar(window, cx);
            } else {
                self.close_reader(window, cx);
            }
        }
    }
}

impl CommandTarget<NextPage> for ReaderView {
    fn execute(&mut self, _: &NextPage, window: &mut Window, cx: &mut Context<Self>) {
        self.turn_page(true, window, cx);
    }
}

impl CommandTarget<PreviousLine> for ReaderView {
    fn execute(&mut self, _: &PreviousLine, window: &mut Window, cx: &mut Context<Self>) {
        self.page_turn = None;
        self.swipe_preview.clear();
        self.chrome.hide_for_motion(window, cx);
        self.dismiss_selection(cx);
        self.with_renderer(cx, |renderer, cx| {
            if !renderer.step_reading_ruler(false, cx) {
                renderer.previous_line(cx);
            }
        });
    }
}

impl CommandTarget<NextLine> for ReaderView {
    fn execute(&mut self, _: &NextLine, window: &mut Window, cx: &mut Context<Self>) {
        self.page_turn = None;
        self.swipe_preview.clear();
        self.chrome.hide_for_motion(window, cx);
        self.dismiss_selection(cx);
        self.with_renderer(cx, |renderer, cx| {
            if !renderer.step_reading_ruler(true, cx) {
                renderer.next_line(cx);
            }
        });
    }
}

impl CommandTarget<PreviousSection> for ReaderView {
    fn execute(&mut self, _: &PreviousSection, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_selection(cx);
        self.page_turn = None;
        self.swipe_preview.clear();
        self.chrome.hide_for_motion(window, cx);
        self.with_renderer(cx, |renderer, cx| renderer.previous_section(cx));
    }
}

impl CommandTarget<NextSection> for ReaderView {
    fn execute(&mut self, _: &NextSection, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_selection(cx);
        self.page_turn = None;
        self.swipe_preview.clear();
        self.chrome.hide_for_motion(window, cx);
        self.with_renderer(cx, |renderer, cx| renderer.next_section(cx));
    }
}

impl CommandTarget<NextSidebarTab> for ReaderView {
    fn execute(&mut self, _: &NextSidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.chrome.cycled_sidebar_tab(self.sidebar.is_visible(), self.sidebar_tab(), self.contents_available(), 1, window);
        self.chrome.show(cx);
        self.keyboard_visibility().set(!ui_components::uses_mobile_reader_controls(window));
        self.select_sidebar_tab(tab, window, cx);
    }
}

impl CommandTarget<PreviousSidebarTab> for ReaderView {
    fn execute(&mut self, _: &PreviousSidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.chrome.cycled_sidebar_tab(self.sidebar.is_visible(), self.sidebar_tab(), self.contents_available(), -1, window);
        self.chrome.show(cx);
        self.keyboard_visibility().set(!ui_components::uses_mobile_reader_controls(window));
        self.select_sidebar_tab(tab, window, cx);
    }
}

impl CommandTarget<ToggleSidebar> for ReaderView {
    fn execute(&mut self, _: &ToggleSidebar, window: &mut Window, cx: &mut Context<Self>) {
        let chrome_visible = self.chrome.is_visible();
        let visible = chrome_visible && (!crate::shell::chrome::sidebar_overlays_document(window) || self.sidebar.is_visible());
        if visible {
            self.sidebar.close(cx);
            self.chrome.hide(cx);
            self.focus_document(window, cx);
        } else {
            self.sidebar.reopen(cx);
            self.chrome.show(cx);
            self.focus_sidebar_body(window, cx);
        }
        notify(cx, Component::ReaderShell, "sidebar_toggled");
    }
}

impl CommandTarget<ToggleToc> for ReaderView {
    fn execute(&mut self, _: &ToggleToc, window: &mut Window, cx: &mut Context<Self>) {
        self.select_sidebar_tab(SidebarTab::Contents, window, cx);
    }
}

impl CommandTarget<FocusReaderSearch> for ReaderView {
    fn execute(&mut self, _: &FocusReaderSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.select_sidebar_tab(SidebarTab::Search, window, cx);
    }
}

impl CommandTarget<ToggleSearch> for ReaderView {
    fn execute(&mut self, _: &ToggleSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.select_sidebar_tab(SidebarTab::Search, window, cx);
    }
}

impl CommandTarget<CloseSearch> for ReaderView {
    fn execute(&mut self, _: &CloseSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_sidebar(window, cx);
    }
}

impl CommandTarget<IncreaseFont> for ReaderView {
    fn execute(&mut self, _: &IncreaseFont, _: &mut Window, cx: &mut Context<Self>) {
        self.adjust_font_size(1.0, cx);
    }
}

impl CommandTarget<DecreaseFont> for ReaderView {
    fn execute(&mut self, _: &DecreaseFont, _: &mut Window, cx: &mut Context<Self>) {
        self.adjust_font_size(-1.0, cx);
    }
}

// EPUB has no zoom control: font size covers reflowable text, and
// CommonReaderCommands still requires these two be implemented.
impl CommandTarget<IncreaseScale> for ReaderView {
    fn execute(&mut self, _: &IncreaseScale, _: &mut Window, _: &mut Context<Self>) {}
}

impl CommandTarget<DecreaseScale> for ReaderView {
    fn execute(&mut self, _: &DecreaseScale, _: &mut Window, _: &mut Context<Self>) {}
}

impl CommandTarget<IncreaseColumnWidth> for ReaderView {
    fn execute(&mut self, _: &IncreaseColumnWidth, window: &mut Window, cx: &mut Context<Self>) {
        self.adjust_column_width(40.0, window, cx);
    }
}

impl CommandTarget<DecreaseColumnWidth> for ReaderView {
    fn execute(&mut self, _: &DecreaseColumnWidth, window: &mut Window, cx: &mut Context<Self>) {
        self.adjust_column_width(-40.0, window, cx);
    }
}

impl CommandTarget<ToggleFullscreen> for ReaderView {
    fn execute(&mut self, _: &ToggleFullscreen, window: &mut Window, _: &mut Context<Self>) {
        window.toggle_fullscreen();
    }
}

/// Leaving the book is one behaviour with two entry points: this and the
/// toolbar's library button both go through `close_reader`.
impl CommandTarget<ReturnToLibrary> for ReaderView {
    fn execute(&mut self, _: &ReturnToLibrary, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dismiss_selection(cx) {
            self.close_reader(window, cx);
        }
    }
}
