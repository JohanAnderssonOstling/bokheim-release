//! Bounded reuse of completed synthesis, independent of playback generations.
use super::{AudioChunk, Voice};
use sherpa_onnx::GenerationConfig;
use std::collections::VecDeque;
use std::time::Duration;

#[derive(PartialEq, Eq)]
pub(super) struct AudioKey {
    text: String,
    voice: Voice,
    speed: u32,
    silence_scale: u32,
}

impl AudioKey {
    pub(super) fn new(text: &str, voice: Voice, config: &GenerationConfig) -> Self {
        Self { text: text.into(), voice, speed: config.speed.to_bits(), silence_scale: config.silence_scale.to_bits() }
    }
}

struct Entry {
    key: AudioKey,
    chunks: Vec<AudioChunk>,
    duration: Duration,
    bytes: usize,
}

pub(super) struct AudioCache {
    entries: VecDeque<Entry>,
    duration: Duration,
    bytes: usize,
    max_duration: Duration,
    max_bytes: usize,
}

impl Default for AudioCache {
    fn default() -> Self {
        Self { entries: VecDeque::new(), duration: Duration::ZERO, bytes: 0, max_duration: Duration::from_secs(180), max_bytes: 32 * 1024 * 1024 }
    }
}

impl AudioCache {
    pub(super) fn get(&mut self, key: &AudioKey) -> Option<Vec<AudioChunk>> {
        let index = self.entries.iter().position(|entry| entry.key == *key)?;
        let entry = self.entries.remove(index)?;
        let chunks = entry.chunks.clone();
        self.entries.push_back(entry);
        Some(chunks)
    }

    pub(super) fn insert(&mut self, key: AudioKey, chunks: Vec<AudioChunk>) {
        if chunks.is_empty() || !chunks.last().is_some_and(|chunk| chunk.last) {
            return;
        }
        let duration: Duration = chunks.iter().map(|chunk| Duration::from_secs_f64(chunk.samples.len() as f64 / f64::from(chunk.rate.get()))).sum();
        let bytes = chunks.iter().map(|chunk| chunk.samples.len() * size_of::<f32>()).sum::<usize>() + key.text.len();
        // An unusually large passage must not evict the entire useful cache.
        if duration > self.max_duration || bytes > self.max_bytes {
            return;
        }
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            let old = self.entries.remove(index).expect("matching cache entry");
            self.duration -= old.duration;
            self.bytes -= old.bytes;
        }
        while self.duration + duration > self.max_duration || self.bytes + bytes > self.max_bytes {
            let old = self.entries.pop_front().expect("nonempty cache exceeds budget");
            self.duration -= old.duration;
            self.bytes -= old.bytes;
        }
        self.duration += duration;
        self.bytes += bytes;
        self.entries.push_back(Entry { key, chunks, duration, bytes });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;
    use std::sync::Arc;

    fn key(text: &str) -> AudioKey {
        AudioKey::new(text, Voice::Alba, &GenerationConfig::default())
    }

    fn chunks() -> Vec<AudioChunk> {
        vec![AudioChunk { samples: Arc::new(vec![0.25; 10]), rate: NonZeroU32::new(10).unwrap(), last: true }]
    }

    #[test]
    fn reuse_requires_matching_text_voice_speed_and_sentence_pauses() {
        let mut cache = AudioCache::default();
        let samples = chunks();
        let allocation = samples[0].samples.clone();
        cache.insert(key("First."), samples);
        let hit = cache.get(&key("First.")).unwrap();
        assert!(Arc::ptr_eq(&allocation, &hit[0].samples), "reuse must not copy PCM");
        assert!(cache.get(&key("Other.")).is_none());
        assert!(cache.get(&AudioKey::new("First.", Voice::George, &GenerationConfig::default())).is_none());
        let speed = GenerationConfig { speed: 1.5, ..Default::default() };
        assert!(cache.get(&AudioKey::new("First.", Voice::Alba, &speed)).is_none());
        let pauses = GenerationConfig { silence_scale: 1.5, ..Default::default() };
        assert!(cache.get(&AudioKey::new("First.", Voice::Alba, &pauses)).is_none());
    }

    #[test]
    fn duration_and_memory_budgets_evict_least_recently_used_audio() {
        for (max_duration, max_bytes) in [(Duration::from_secs(2), usize::MAX), (Duration::MAX, 100)] {
            let mut cache = AudioCache { max_duration, max_bytes, ..Default::default() };
            cache.insert(key("One."), chunks());
            cache.insert(key("Two."), chunks());
            assert!(cache.get(&key("One.")).is_some());
            cache.insert(key("Three."), chunks());
            assert!(cache.get(&key("Two.")).is_none());
            assert!(cache.get(&key("One.")).is_some());
            assert!(cache.get(&key("Three.")).is_some());
            assert!(cache.duration <= max_duration);
            assert!(cache.bytes <= max_bytes);
        }
    }

    #[test]
    fn incomplete_or_oversized_synthesis_does_not_displace_completed_audio() {
        let mut cache = AudioCache { max_duration: Duration::from_secs(1), ..Default::default() };
        cache.insert(key("Complete."), chunks());
        let mut partial = chunks();
        partial[0].last = false;
        cache.insert(key("Partial."), partial);
        cache.insert(key("Oversized."), vec![AudioChunk { samples: Arc::new(vec![0.25; 11]), rate: NonZeroU32::new(10).unwrap(), last: true }]);
        assert!(cache.get(&key("Complete.")).is_some());
        assert!(cache.get(&key("Partial.")).is_none());
        assert!(cache.get(&key("Oversized.")).is_none());
    }
}
