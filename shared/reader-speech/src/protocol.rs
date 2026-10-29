//! Text and events shared by native and browser narration.
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum SpeechEvent {
    Requested { generation: u64, index: usize, glyph_start: u32, max_chars: usize },
    Started { generation: u64, index: usize, glyph_ranges: Vec<Range<u32>> },
    Finished { generation: u64 },
    Failed { generation: u64, message: String },
    Remote { generation: u64, command: SpeechControl },
}

#[derive(Clone, Debug)]
pub enum SpeechControl {
    Pause,
    Resume,
    Stop,
    Item { generation: u64, index: usize },
}

/// Original fragment text plus the reader's block boundary. Multiple fragments
/// can belong to one paragraph or list item without introducing extra pauses.
#[derive(Clone, Debug)]
pub struct SpeechPassage {
    pub(crate) text: Arc<str>,
    pub(crate) ends_block: bool,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) position: Option<ChunkPosition>,
}

/// A bounded piece of original text and its source position, extracted together
/// by the reader. `next_glyph` is absent at the logical item's end.
#[derive(Debug)]
pub struct SpeechChunk {
    pub text: String,
    pub glyph_ranges: Vec<Range<u32>>,
    pub next_glyph: Option<u32>,
}

#[derive(Clone, Debug)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct ChunkPosition {
    pub(crate) glyph_ranges: Vec<Range<u32>>,
    pub(crate) next_glyph: Option<u32>,
}

impl SpeechPassage {
    pub fn new(text: impl Into<String>, ends_block: bool) -> Self {
        Self {
            text: text.into().into(),
            ends_block,
            #[cfg(not(target_arch = "wasm32"))]
            position: None,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn from_chunk(chunk: SpeechChunk) -> Self {
        Self { text: chunk.text.into(), ends_block: chunk.next_glyph.is_none(), position: Some(ChunkPosition { glyph_ranges: chunk.glyph_ranges, next_glyph: chunk.next_glyph }) }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn ends_block(&self) -> bool {
        self.ends_block
    }
}

/// Pause lengths applied during synthesis. `sentence_scale` stretches or
/// shortens the pauses the voice makes inside a passage; `passage_gap` adds
/// seconds of silence after each passage, shortened by the reading speed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pauses {
    pub sentence_scale: f32,
    pub passage_gap: f32,
}

impl Pauses {
    pub const NATURAL: Self = Self { sentence_scale: 1.0, passage_gap: 0.0 };
}
