use super::PdfReaderView;
use crate::shell::persistence::StoredPosition;
use crate::shell::tts::{self, TtsReader};
use gpui::{App, Context};
use reader_speech::{SpeechControl, SpeechEvent, SpeechPassage};
use std::time::Duration;
use ui_components::user_facing_error;

impl PdfReaderView {
    pub(super) fn tts_navigation_changed(&mut self, cx: &mut Context<Self>) {
        if !self.tts.playing || self.pdf.read(cx).navigation_revision() == self.tts_navigation_revision {
            return;
        }
        self.tts_navigation_revision = self.pdf.read(cx).navigation_revision();
        self.tts_request += 1;
        self.tts.navigated();
        self.pdf.update(cx, |pdf, cx| pdf.set_reading_highlight(None, cx));
        if !self.tts.paused {
            let timer = cx.background_executor().timer(Duration::from_millis(100));
            self.tts.navigation_task = Some(cx.spawn(async move |this, cx| {
                timer.await;
                let _ = this.update(cx, |reader, cx| {
                    if reader.tts.playing && !reader.tts.paused {
                        reader.start_tts(cx);
                    }
                });
            }));
        }
        cx.notify();
    }

    fn advance_tts_page(&mut self, cx: &mut Context<Self>) {
        if self.tts_page + 1 >= self.page_count {
            self.stop_tts(cx);
            self.tts.status = "Finished".into();
            return;
        }
        if self.tts.sleep.and_then(|plan| plan.chapter()).is_some_and(|chapter| self.toc.chapter_at(self.tts_page + 1) > chapter) {
            self.tts.finished = true;
            self.sleep_tts(cx);
            return;
        }
        self.tts_page += 1;
        let page = self.tts_page;
        self.pdf.update(cx, |pdf, cx| pdf.advance_reading_page(page, cx));
        self.start_tts(cx);
    }
}

impl TtsReader for PdfReaderView {
    fn narration_mut(&mut self) -> &mut tts::Narration {
        &mut self.tts
    }

    fn chapter_key(&self, cx: &App) -> usize {
        self.toc.chapter_at(self.pdf.read(cx).current_page())
    }

    fn chapter_label(&self, _cx: &App) -> String {
        self.toc.narration_chapter_label(self.tts_page)
    }

    fn skip_tts_element(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.tts_navigation_changed(cx);
        if !self.tts.playing || self.tts.needs_restart {
            return;
        }
        let Some(index) = tts::adjacent_item(self.tts.current.unwrap_or(0), self.tts_fragments.len(), forward) else { return };
        if self.tts.end_fragment.is_some_and(|end| index >= end) {
            self.sleep_tts(cx);
        }
        self.tts.skip_to(index);
        let rects = self.tts_fragments[index].clone();
        let page = self.tts_page;
        self.pdf.update(cx, |pdf, cx| pdf.follow_reading_fragment(page, rects, cx));
    }

    fn start_tts(&mut self, cx: &mut Context<Self>) {
        let continuation = self.tts.playing;
        if continuation && self.tts.service.as_ref().is_some_and(|service| !service.is_current()) {
            self.stop_tts(cx);
            return;
        }
        let current_page = self.pdf.read(cx).current_page();
        let Some(page) = self.toc.narration_start_page(current_page, self.page_count) else {
            self.stop_tts(cx);
            self.tts.status = "Finished".into();
            cx.notify();
            return;
        };
        if page != current_page {
            if self.tts.sleep.and_then(|plan| plan.chapter()).is_some_and(|chapter| self.toc.chapter_at(page) > chapter) {
                self.tts.finished = true;
                self.sleep_tts(cx);
                cx.notify();
                return;
            }
            self.pdf.update(cx, |pdf, cx| pdf.advance_reading_page(page, cx));
        }
        let pdf = self.pdf.read(cx);
        let position = pdf.full_page_position();
        self.tts_navigation_revision = pdf.navigation_revision();
        let Some(request) = pdf.reading_request(page) else {
            self.tts.status = "The document is still loading.".into();
            cx.notify();
            return;
        };
        self.tts_request += 1;
        let revision = self.tts_request;
        self.tts.navigated();
        self.tts.playing = true;
        self.tts.paused = false;
        self.tts.status = "Preparing this page…".into();
        self.tts_page = page;
        self.tts.navigation_task = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |reader, cx| {
                if revision != reader.tts_request || !reader.tts.playing || reader.tts.paused {
                    return;
                }
                if continuation && reader.tts.service.as_ref().is_some_and(|service| !service.is_current()) {
                    reader.stop_tts(cx);
                    return;
                }
                match result {
                    Ok(fragments) => {
                        let start = fragments.iter().position(|fragment| fragment.rects.iter().any(|rect| rect.bottom() >= position)).unwrap_or(fragments.len());
                        if fragments.is_empty() {
                            reader.stop_tts(cx);
                            reader.tts.status = "No readable text here. Scanned pages require OCR.".into();
                        } else {
                            // PDF fragments are geometry-based reading blocks.
                            let (rects, passages) = fragments.into_iter().map(|fragment| (fragment.rects, SpeechPassage::new(fragment.text, true))).unzip();
                            reader.tts_fragments = rects;
                            reader.tts.set_chapter(reader.chapter_label(cx));
                            match reader.tts.start(passages, start) {
                                Ok(Some(events)) => reader.tts.events_task = Some(tts::listen(events, cx)),
                                Ok(None) => {}
                                Err(error) => {
                                    reader.stop_tts(cx);
                                    reader.tts.status = user_facing_error(&error, "Read aloud could not continue. Check your audio output and try again.").to_owned();
                                }
                            }
                        }
                    }
                    Err(error) => {
                        reader.stop_tts(cx);
                        reader.tts.status = user_facing_error(&error.to_string(), "Read aloud could not continue. Check your audio output and try again.").to_owned();
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle_tts_pause(&mut self, cx: &mut Context<Self>) {
        if self.tts.paused {
            if self.tts.needs_restart {
                self.tts.paused = false;
                self.start_tts(cx);
            } else if self.tts.finished {
                self.tts.resume();
                self.advance_tts_page(cx);
            } else {
                self.tts.resume();
            }
        } else {
            self.tts.pause();
        }
        cx.notify();
    }

    fn stop_tts(&mut self, cx: &mut Context<Self>) {
        self.tts_request += 1;
        self.tts.stop();
        self.tts_fragments = Vec::new();
        self.pdf.update(cx, |pdf, cx| pdf.set_reading_highlight(None, cx));
        cx.notify();
    }

    fn speech_event(&mut self, event: SpeechEvent, cx: &mut Context<Self>) {
        self.tts_navigation_changed(cx);
        if !self.tts.accepts(&event) {
            return;
        }
        match event {
            SpeechEvent::Requested { .. } => return,
            SpeechEvent::Remote { command, .. } => {
                let item = match &command {
                    SpeechControl::Item { index, .. } => Some(*index),
                    _ => None,
                };
                self.remote_control(command, cx);
                if let Some(index) = item {
                    if let Some(fragment) = self.tts_fragments.get(index) {
                        let rects = fragment.clone();
                        let page = self.tts_page;
                        self.pdf.update(cx, |pdf, cx| pdf.follow_reading_fragment(page, rects, cx));
                    }
                }
            }
            SpeechEvent::Started { index, .. } => {
                if !self.tts.started_item(index) {
                    return;
                }
                if let Some(fragment) = self.tts_fragments.get(index) {
                    let rects = fragment.clone();
                    let page = self.tts_page;
                    if let Some(bounds) = rects.first() {
                        self.positions.record_narration(StoredPosition::pdf_page(page, bounds.top(), self.page_count), self.pdf.read(cx).navigation_revision());
                    }
                    self.pdf.update(cx, |pdf, cx| {
                        pdf.follow_reading_fragment(page, rects, cx);
                    });
                }
            }
            SpeechEvent::Finished { .. } => {
                if self.tts.paused {
                    self.tts.finished = true;
                } else {
                    self.advance_tts_page(cx);
                }
            }
            SpeechEvent::Failed { message, .. } => {
                self.stop_tts(cx);
                self.tts.status = user_facing_error(&message, "Read aloud could not continue. Check your audio output and try again.").to_owned();
            }
        }
        cx.notify();
    }
}
