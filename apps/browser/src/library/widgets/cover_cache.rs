//! Weak book-to-cover lookups. The shared image cache owns retained pixels.

use gpui::RenderImage;
use library_backend::ThumbnailResolution;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Weak};

pub(super) struct CoverCache<K> {
    entries: HashMap<K, (Weak<RenderImage>, ThumbnailResolution)>,
}

impl<K> Default for CoverCache<K> {
    fn default() -> Self {
        Self { entries: HashMap::new() }
    }
}

impl<K: Eq + Hash> CoverCache<K> {
    pub(super) fn get(&mut self, key: &K) -> Option<(Arc<RenderImage>, ThumbnailResolution)> {
        let (image, resolution) = self.entries.get(key)?;
        if let Some(image) = image.upgrade() {
            return Some((image, *resolution));
        }
        self.entries.remove(key);
        None
    }

    #[cfg(test)]
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(super) fn insert(&mut self, key: K, image: &Arc<RenderImage>, resolution: ThumbnailResolution) {
        // Prune metadata for images expired by shared pressure eviction.
        self.entries.retain(|_, (image, _)| image.strong_count() > 0);
        self.entries.insert(key, (Arc::downgrade(image), resolution));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Frame, RgbaImage};

    fn image() -> Arc<RenderImage> {
        Arc::new(RenderImage::new(vec![Frame::new(RgbaImage::new(1, 1))]))
    }

    #[test]
    fn lookups_do_not_pin_pixels_and_expired_metadata_is_pruned() {
        let mut cache = CoverCache::default();
        let first = image();
        cache.insert(1, &first, ThumbnailResolution::Browse);
        assert_eq!(Arc::strong_count(&first), 1);
        assert_eq!(cache.get(&1).unwrap().1, ThumbnailResolution::Browse);
        drop(first);
        assert!(cache.get(&1).is_none());
        assert!(cache.entries.is_empty());
        let stale = image();
        cache.insert(2, &stale, ThumbnailResolution::Browse);
        drop(stale);
        let next = image();
        cache.insert(3, &next, ThumbnailResolution::CardHighDensity);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.get(&3).unwrap().1, ThumbnailResolution::CardHighDensity);
    }

    #[test]
    fn replacement_and_invalidation_leave_external_owners_intact() {
        let mut cache = CoverCache::default();
        let low = image();
        let high = image();
        cache.insert(1, &low, ThumbnailResolution::Browse);
        cache.insert(1, &high, ThumbnailResolution::CardHighDensity);
        assert!(Arc::ptr_eq(&cache.get(&1).unwrap().0, &high));
        cache.clear();
        assert!(cache.get(&1).is_none());
        assert_eq!(Arc::strong_count(&low), 1);
        assert_eq!(Arc::strong_count(&high), 1);
    }
}
