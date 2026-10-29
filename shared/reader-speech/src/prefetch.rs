//! Bounded read-ahead for text extracted by the reader's UI thread.
use super::{FIRST_CHUNK_CHARS, MAX_CHUNK_CHARS, SpeechChunk, SpeechEvent, SpeechEvents, SpeechPassage};
use std::collections::VecDeque;

// Text duration is estimated until synthesis supplies the actual PCM duration.
// Keep a little more text ready than the audio queue's one-minute target.
const PREFETCH_SECONDS: f32 = 90.0;
const CHARS_PER_SECOND: f32 = 15.0;

fn estimated_chars(passage: &Option<SpeechPassage>) -> usize {
    // Charge empty items and tiny chunks a minimum duration as well, so an
    // empty document cannot fill an unbounded queue of extraction results.
    passage.as_ref().map_or(0, |passage| passage.text().chars().count()).max(4)
}

#[derive(Default)]
pub(super) struct TextPrefetch {
    ready: VecDeque<((usize, u32), Option<SpeechPassage>)>,
    requested: Option<(usize, u32)>,
    next: (usize, u32),
    opening_pending: bool,
    queued_chars: usize,
}

impl TextPrefetch {
    pub fn reset(&mut self, index: usize, glyph_start: u32) {
        self.ready.clear();
        self.requested = None;
        self.next = (index, glyph_start);
        self.opening_pending = true;
        self.queued_chars = 0;
    }

    pub fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }

    pub fn can_request(&self, end: usize, rate: f32) -> bool {
        let estimated_seconds = self.queued_chars as f32 / CHARS_PER_SECOND / rate;
        self.requested.is_none() && estimated_seconds < PREFETCH_SECONDS && self.next.0 < end
    }

    pub fn request(&mut self, generation: u64, end: usize, rate: f32, events: &SpeechEvents) {
        if self.can_request(end, rate) {
            let (index, glyph_start) = self.next;
            let max_chars = if self.opening_pending { FIRST_CHUNK_CHARS } else { MAX_CHUNK_CHARS };
            self.requested = Some(self.next);
            let _ = events.try_send(SpeechEvent::Requested { generation, index, glyph_start, max_chars });
        }
    }

    pub fn provide(&mut self, index: usize, glyph_start: u32, chunk: Option<SpeechChunk>) {
        if self.requested != Some((index, glyph_start)) {
            return;
        }
        self.next = match chunk.as_ref().and_then(|chunk| chunk.next_glyph) {
            Some(next) => (index, next),
            _ => (index + 1, 0),
        };
        if chunk.as_ref().is_some_and(|chunk| !chunk.text.trim().is_empty()) {
            self.opening_pending = false;
        }
        let passage = chunk.map(SpeechPassage::from_chunk);
        self.queued_chars += estimated_chars(&passage);
        self.ready.push_back(((index, glyph_start), passage));
        self.requested = None;
    }

    /// Distinguish a pending response from a reader item with no readable text.
    pub fn take(&mut self, index: usize, glyph_start: u32) -> Option<Option<SpeechPassage>> {
        if self.ready.front().is_some_and(|(position, _)| *position == (index, glyph_start)) {
            self.ready.pop_front().map(|(_, passage)| {
                self.queued_chars -= estimated_chars(&passage);
                passage
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(prefetch: &mut TextPrefetch, text: &str, rate: f32) -> usize {
        let (sender, events) = async_channel::unbounded();
        let sender = SpeechEvents::Direct(sender);
        let mut count = 0;
        while prefetch.can_request(10000, rate) {
            prefetch.request(1, 10000, rate, &sender);
            assert!(matches!(events.try_recv().unwrap(), SpeechEvent::Requested { index, .. } if index == count));
            prefetch.provide(count, 0, Some(SpeechChunk { text: text.into(), glyph_ranges: vec![], next_glyph: None }));
            count += 1;
        }
        assert!(count < 10000, "read-ahead must not extract the whole document");
        count
    }

    #[test]
    fn text_read_ahead_uses_estimated_duration_instead_of_a_passage_count() {
        let mut short = TextPrefetch::default();
        short.reset(0, 0);
        let short_count = fill(&mut short, "Short sentence.", 1.0);
        let mut long = TextPrefetch::default();
        long.reset(0, 0);
        let long_count = fill(&mut long, &"a".repeat(MAX_CHUNK_CHARS), 1.0);
        assert!(short_count > 3 && short_count > long_count * 10);
        for queue in [&short, &long] {
            let estimated = queue.queued_chars as f32 / CHARS_PER_SECOND;
            assert!(estimated >= PREFETCH_SECONDS);
            assert!(estimated < PREFETCH_SECONDS + MAX_CHUNK_CHARS as f32 / CHARS_PER_SECOND);
        }
        let mut faster = TextPrefetch::default();
        faster.reset(0, 0);
        assert_eq!(fill(&mut faster, "Short sentence.", 2.0), short_count * 2);
    }

    #[test]
    fn consuming_or_resetting_prefetched_text_releases_its_duration_budget() {
        let (sender, events) = async_channel::unbounded();
        let sender = SpeechEvents::Direct(sender);
        let mut prefetch = TextPrefetch::default();
        prefetch.reset(0, 0);
        let count = fill(&mut prefetch, "Sentence.", 1.0);
        prefetch.request(1, 10000, 1.0, &sender);
        assert!(events.try_recv().is_err());
        assert_eq!(prefetch.take(0, 0).unwrap().unwrap().text(), "Sentence.");
        prefetch.request(1, 10000, 1.0, &sender);
        assert!(matches!(events.try_recv().unwrap(), SpeechEvent::Requested { index, .. } if index == count));
        prefetch.reset(8, 50);
        prefetch.provide(count, 0, Some(SpeechChunk { text: "Old request.".into(), glyph_ranges: vec![], next_glyph: None }));
        assert!(!prefetch.has_ready());
        assert_eq!(prefetch.queued_chars, 0);
        prefetch.request(2, 10, 1.0, &sender);
        assert!(matches!(events.try_recv().unwrap(), SpeechEvent::Requested { index: 8, glyph_start: 50, max_chars: FIRST_CHUNK_CHARS, .. }));
    }

    #[test]
    fn empty_items_keep_the_short_opening_chunk_but_still_have_a_bounded_queue() {
        let (sender, events) = async_channel::unbounded();
        let sender = SpeechEvents::Direct(sender);
        let mut prefetch = TextPrefetch::default();
        prefetch.reset(0, 0);
        let mut count = 0;
        while prefetch.can_request(10000, 1.0) {
            prefetch.request(1, 10000, 1.0, &sender);
            assert!(matches!(events.try_recv().unwrap(), SpeechEvent::Requested { index, max_chars: FIRST_CHUNK_CHARS, .. } if index == count));
            prefetch.provide(count, 0, None);
            count += 1;
        }
        assert!(count > 3 && count < 10000);
    }
}
