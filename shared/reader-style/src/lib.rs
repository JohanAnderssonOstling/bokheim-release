//! Renderer-neutral visual tokens shared by EPUB and PDF readers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgbaColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl RgbaColor {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn packed_rgba(self) -> u32 {
        u32::from_be_bytes([self.r, self.g, self.b, self.a])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReaderInteractionStyle {
    pub selection: RgbaColor,
    pub search_match: RgbaColor,
    pub active_search_match: RgbaColor,
    pub annotation: RgbaColor,
    pub active_annotation: RgbaColor,
}

impl ReaderInteractionStyle {
    pub const LIBRARY_LIGHT: Self = Self {
        selection: RgbaColor::new(151, 188, 159, 112),
        search_match: RgbaColor::new(232, 193, 78, 116),
        active_search_match: RgbaColor::new(190, 116, 55, 164),
        annotation: RgbaColor::new(236, 204, 91, 104),
        active_annotation: RgbaColor::new(196, 126, 57, 148),
    };
}

impl Default for ReaderInteractionStyle {
    fn default() -> Self {
        Self::LIBRARY_LIGHT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_color_preserves_rgba_channel_order() {
        assert_eq!(RgbaColor::new(0x12, 0x34, 0x56, 0x78).packed_rgba(), 0x12345678);
    }

    #[test]
    fn interaction_roles_remain_visually_distinct() {
        let style = ReaderInteractionStyle::default();
        assert_ne!(style.selection, style.search_match);
        assert_ne!(style.search_match, style.active_search_match);
        assert_ne!(style.annotation, style.active_annotation);
    }
}
