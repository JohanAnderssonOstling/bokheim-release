use crate::epub::{LoadState, ReaderView};
use crate::shell::persistence::StoredPosition;
use crate::shell::tts::{self, TtsReader};
use gpui::{App, Context};
use skrift_column_paginator::cfi::{parse_cfi_spine_only, source_position_cfi};
use skrift_column_paginator::{ReadingTarget, RendererCommand};
use html_view_gpui::HtmlView;
use reader_speech::{FIRST_CHUNK_CHARS, SpeechChunk, SpeechControl, SpeechEvent, next_chunk_with_limit};
use std::time::Duration;
use ui_components::user_facing_error;

fn speech_chunk(view: &HtmlView, target: &ReadingTarget, glyph_start: u32, max_chars: usize) -> Option<SpeechChunk> {
    view.reading_chunk(target, glyph_start, max_chars, |text| next_chunk_with_limit(text, 0, max_chars)).map(|(fragment, next_glyph)| SpeechChunk { text: fragment.text, glyph_ranges: fragment.target.glyph_ranges, next_glyph })
}

/// A paused skip previews exactly the chunk that playback will start with.
fn follow_passage(view: &mut HtmlView, target: &ReadingTarget, cx: &mut Context<HtmlView>) {
    if let Some(chunk) = speech_chunk(view, target, 0, FIRST_CHUNK_CHARS) {
        view.follow_reading_target(ReadingTarget { element: target.element, glyph_ranges: chunk.glyph_ranges }, cx);
    }
}

impl ReaderView {
    pub(super) fn tts_document_failed(&mut self, message: &str, cx: &mut Context<Self>) {
        if self.tts.playing && (self.tts.needs_restart || self.tts.finished) {
            self.stop_tts(cx);
            self.tts.status = user_facing_error(message, "Could not read the next passage. Try starting read aloud again.").to_owned();
        }
    }

    pub(super) fn tts_navigation_changed(&mut self, cx: &mut Context<Self>) {
        let LoadState::Ready(view) = &self.load_state else { return };
        if view.read(cx).navigation_revision() == self.tts_navigation_revision {
            return;
        }
        self.tts_navigation_revision = view.read(cx).navigation_revision();
        if !self.tts.playing {
            return;
        }
        self.tts.navigated();
        self.with_renderer(cx, |view, cx| view.apply(RendererCommand::SetReadingTarget(None), cx));
        if !self.tts.paused {
            self.schedule_tts(cx);
        }
        cx.notify();
    }

    fn schedule_tts(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(Duration::from_millis(100));
        self.tts.navigation_task = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |reader, cx| {
                if reader.tts.playing && !reader.tts.paused {
                    if reader.tts.finished {
                        let ready = match &reader.load_state {
                            LoadState::Ready(view) => view.read(cx).reading_document(),
                            _ => None,
                        };
                        match ready {
                            Some(doc) if doc == reader.tts_doc => reader.advance_tts_document(cx),
                            Some(_) => reader.start_tts(cx),
                            None => reader.schedule_tts(cx),
                        }
                    } else {
                        reader.start_tts(cx);
                    }
                }
            });
        }));
    }

    fn advance_tts_document(&mut self, cx: &mut Context<Self>) {
        if let LoadState::Ready(view) = &self.load_state
            && view.read(cx).reading_document().is_none()
        {
            self.tts.finished = true;
            self.tts.status = "Waiting for navigation…".into();
            if !self.tts.paused {
                self.schedule_tts(cx);
            }
            return;
        }
        if self.tts_doc + 1 >= self.book.document_uris().len() {
            self.stop_tts(cx);
            self.tts.status = "Finished".into();
            return;
        }
        if self.tts.sleep.and_then(|plan| plan.chapter()).is_some_and(|chapter| self.toc.chapter_for_document(self.tts_doc + 1).unwrap_or(self.tts_doc + 1) > chapter) {
            self.tts.finished = true;
            self.sleep_tts(cx);
            return;
        }
        self.tts.navigated();
        self.tts.status = "Loading the next chapter…".into();
        self.with_renderer(cx, |view, cx| {
            view.apply(RendererCommand::SetReadingTarget(None), cx);
            view.advance_reading_document(cx);
        });
        if !self.tts.paused {
            self.schedule_tts(cx);
        }
    }
}

impl TtsReader for ReaderView {
    fn narration_mut(&mut self) -> &mut tts::Narration {
        &mut self.tts
    }

    fn chapter_key(&self, cx: &App) -> usize {
        let cfi = match &self.load_state {
            LoadState::Ready(view) => self.tts.current.and_then(|index| self.tts_fragments.get(index)).and_then(|fragment| view.read(cx).reading_target_cfi(fragment)).or_else(|| view.read(cx).current_cfi()),
            _ => None,
        };
        let cfi = cfi.or_else(|| self.reading_position.as_ref().map(|position| source_position_cfi(&position.source)));
        let cfi = cfi.as_deref();
        cfi.and_then(|cfi| self.toc.chapter_for_cfi(cfi)).or_else(|| cfi.and_then(parse_cfi_spine_only)).unwrap_or(self.tts_doc)
    }

    fn chapter_label(&self, cx: &App) -> String {
        let key = self.chapter_key(cx);
        let number = self.toc.chapter_ordinals().iter().position(|chapter| *chapter == key).unwrap_or(self.tts_doc) + 1;
        self.toc.title_for_ordinal(key).filter(|title| !title.trim().is_empty()).map_or_else(|| format!("Chapter {number}"), |title| format!("Chapter {number} · {title}"))
    }

    fn update_tts_chapter_limit(&mut self, cx: &mut Context<Self>) {
        let end = self.tts.sleep.and_then(|plan| plan.chapter()).and_then(|chapter| {
            let LoadState::Ready(view) = &self.load_state else { return None };
            let view = view.read(cx);
            self.tts_fragments.iter().position(|fragment| view.reading_target_cfi(fragment).as_deref().and_then(|cfi| self.toc.chapter_for_cfi(cfi)).unwrap_or(self.tts_doc) > chapter)
        });
        self.tts.limit_to(end);
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
        let target = self.tts_fragments[index].clone();
        self.with_renderer(cx, |view, cx| follow_passage(view, &target, cx));
    }

    fn start_tts(&mut self, cx: &mut Context<Self>) {
        if self.tts.playing && self.tts.service.as_ref().is_some_and(|service| !service.is_current()) {
            self.stop_tts(cx);
            return;
        }
        if let Some(reason) = self.tts.unavailable_reason.clone() {
            self.stop_tts(cx);
            self.tts.status = reason;
            return;
        }
        let LoadState::Ready(view) = &self.load_state else { return };
        let Some((doc, _, glyph_start, mut fragments)) = view.read(cx).reading_items() else {
            self.tts.playing = true;
            self.tts.needs_restart = true;
            self.tts.status = "Loading the chapter…".into();
            if !self.tts.paused {
                self.schedule_tts(cx);
            }
            cx.notify();
            return;
        };
        fragments.retain(|fragment| !view.read(cx).reading_target_cfi(fragment).is_some_and(|cfi| self.toc.narration_excludes_cfi(&cfi)));
        let start = fragments.iter().position(|fragment| fragment.glyph_ranges.last().is_some_and(|range| range.end > glyph_start)).unwrap_or(fragments.len());
        self.tts_doc = doc;
        self.tts_navigation_revision = view.read(cx).navigation_revision();
        self.tts_fragments = fragments;
        if self.tts_fragments.is_empty() {
            self.tts.playing = true;
            self.tts.paused = false;
            self.advance_tts_document(cx);
            cx.notify();
            return;
        }
        self.tts.current = Some(start);
        self.tts.set_chapter(self.chapter_label(cx));
        match self.tts.start_items(self.tts_fragments.len(), start, glyph_start) {
            Ok(Some(events)) => self.tts.events_task = Some(tts::listen(events, cx)),
            Ok(None) => {}
            Err(error) => {
                self.tts.stop();
                self.tts.status = user_facing_error(&error, "Read aloud could not continue. Check your audio output and try again.").to_owned();
            }
        }
        self.update_tts_chapter_limit(cx);
        cx.notify();
    }

    fn toggle_tts_pause(&mut self, cx: &mut Context<Self>) {
        if self.tts.paused {
            if self.tts.needs_restart {
                self.tts.paused = false;
                self.start_tts(cx);
            } else if self.tts.finished {
                self.tts.resume();
                self.advance_tts_document(cx);
            } else {
                self.tts.resume();
            }
        } else {
            self.tts.pause();
        }
        cx.notify();
    }

    fn stop_tts(&mut self, cx: &mut Context<Self>) {
        self.tts.stop();
        self.tts_fragments = Vec::new();
        self.with_renderer(cx, |view, cx| view.apply(RendererCommand::SetReadingTarget(None), cx));
        cx.notify();
    }

    fn speech_event(&mut self, event: SpeechEvent, cx: &mut Context<Self>) {
        self.tts_navigation_changed(cx);
        if !self.tts.accepts(&event) {
            return;
        }
        match event {
            SpeechEvent::Remote { command, .. } => {
                let item = match &command {
                    SpeechControl::Item { index, .. } => Some(*index),
                    _ => None,
                };
                self.remote_control(command, cx);
                if let Some(index) = item {
                    if let Some(target) = self.tts_fragments.get(index).cloned() {
                        self.with_renderer(cx, |view, cx| follow_passage(view, &target, cx));
                    }
                }
            }
            SpeechEvent::Requested { generation, index, glyph_start, max_chars } => {
                if let Some(target) = self.tts_fragments.get(index)
                    && let LoadState::Ready(view) = &self.load_state
                    && let Some(service) = &self.tts.service
                {
                    let chunk = speech_chunk(view.read(cx), target, glyph_start, max_chars);
                    service.provide_chunk(generation, index, glyph_start, chunk);
                }
                return;
            }
            SpeechEvent::Started { index, glyph_ranges, .. } => {
                self.tts.current = Some(index);
                self.tts.status = if self.tts.paused { "Paused" } else { "Reading" }.into();
                if let Some(fragment) = self.tts_fragments.get(index) {
                    let target = ReadingTarget { element: fragment.element, glyph_ranges };
                    if let LoadState::Ready(view) = &self.load_state
                        && let Some(cfi) = view.read(cx).reading_target_cfi(&target)
                        && let Some(position) = &self.reading_position
                    {
                        self.reading_positions.record_narration(StoredPosition::epub(cfi, position.fraction).within(self.toc.active_link()), view.read(cx).navigation_revision());
                    }
                    self.with_renderer(cx, |view, cx| view.follow_reading_target(target, cx));
                }
            }
            SpeechEvent::Finished { .. } => {
                if self.tts.end_fragment.is_some() && self.tts.sleep.and_then(|plan| plan.chapter()).is_some() {
                    self.tts.finished = true;
                    self.sleep_tts(cx);
                } else if self.tts.paused {
                    self.tts.finished = true;
                } else {
                    self.advance_tts_document(cx);
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, px, size};
    use skrift_column_paginator::{RendererInitialConfig, ResourceProvider};
    use std::{io, sync::Arc};

    struct PassageBook;

    impl ResourceProvider for PassageBook {
        fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
            if !self.exists(uri) {
                return Err(io::ErrorKind::NotFound.into());
            }
            let rest = "The rest of this long passage continues onto later lines and pages. ".repeat(50);
            Ok(format!("<html><body><p style=\"height: 600px\">Before.</p><p>A café <em>starts</em> this passage. {rest}</p></body></html>").into_bytes())
        }

        fn exists(&self, uri: &str) -> bool {
            uri == "book.html"
        }

        fn resolve(&self, _: &str, href: &str) -> String {
            href.to_owned()
        }

        fn list_html_candidates(&self, _: &str) -> io::Result<Vec<String>> {
            Ok(vec!["book.html".into()])
        }
    }

    #[gpui::test]
    fn passage_skip_and_playback_paint_the_same_opening_highlight(cx: &mut TestAppContext) {
        let (view, visual) = cx.add_window_view(|window, cx| HtmlView::from_provider(Arc::new(PassageBook), vec!["book.html".into()], 0, None, RendererInitialConfig { max_column_count: Some(1), ..Default::default() }, window, cx));
        visual.simulate_resize(size(px(600.0), px(400.0)));
        visual.run_until_parked();
        let unhighlighted = visual.update(|window, _| window.painted_quads().into_iter().map(|quad| quad.bounds).collect::<Vec<_>>());
        let (opening, revision) = view.update(visual, |view, cx| {
            let (_, _, _, items) = view.reading_items().unwrap();
            let passage = &items[1];
            let chunk = speech_chunk(view, passage, 0, FIRST_CHUNK_CHARS).unwrap();
            assert_eq!(chunk.text, "A café starts this passage.");
            assert!(chunk.next_glyph.is_some(), "the passage must extend beyond the opening chunk");
            assert_ne!(chunk.glyph_ranges, passage.glyph_ranges);
            let revision = view.navigation_revision();
            let position = view.current_cfi();
            follow_passage(view, passage, cx);
            assert_ne!(view.current_cfi(), position, "skipping must reveal a passage on a later page");
            (ReadingTarget { element: passage.element, glyph_ranges: chunk.glyph_ranges }, revision)
        });
        visual.run_until_parked();
        let preview = visual.update(|window, _| window.painted_quads().into_iter().map(|quad| quad.bounds).collect::<Vec<_>>());
        assert_ne!(preview, unhighlighted, "skipping while paused must visibly highlight the destination");
        let position = view.update(visual, |view, cx| {
            assert_eq!(view.navigation_revision(), revision, "following speech must not count as manual navigation");
            let position = view.current_cfi();
            view.follow_reading_target(opening, cx);
            position
        });
        visual.run_until_parked();
        let playing = visual.update(|window, _| window.painted_quads().into_iter().map(|quad| quad.bounds).collect::<Vec<_>>());
        assert_eq!(preview, playing, "starting audio must preserve the skip preview's highlight geometry");
        view.read_with(visual, |view, _| assert_eq!(view.current_cfi(), position, "starting audio must not jump to a different page"));
    }
}
