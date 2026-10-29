//! Narration through native offline voices or the browser's speech service.
mod chunks;
mod protocol;

pub use chunks::{FIRST_CHUNK_CHARS, MAX_CHUNK_CHARS, next_chunk, next_chunk_in_tree, next_chunk_with_limit};
pub use protocol::{Pauses, SpeechChunk, SpeechControl, SpeechEvent, SpeechPassage};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::*;
