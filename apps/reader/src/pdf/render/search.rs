//! PDF search visibility and renderer navigation.

use gpui::{Context, Window};

use super::PdfReaderView;
use crate::invalidation::Component;
use crate::shell::search::SearchBarTarget;

impl SearchBarTarget for PdfReaderView {
    fn search_navigate(&mut self, direction: i8, cx: &mut Context<Self>) {
        self.pdf.update(cx, |pdf, cx| pdf.navigate_search(direction, cx));
    }

    fn search_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_match.is_none() && self.search_input.read(cx).value().is_empty() {
            return;
        }
        self.search_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.search_match = None;
        self.pdf.update(cx, |pdf, cx| pdf.search(String::new(), cx));
        self.focus_document(window, cx);
        crate::invalidation::notify(cx, Component::PdfShell, "search_closed");
    }
}
