/// Thumbnail requests name a display density, not a pixel size, so the stored
/// thumbnail widths stay owned by the thumbnail crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum ThumbnailResolution {
    Browse,
    HighDensity,
}

impl ThumbnailResolution {
    pub const fn width(self) -> u32 {
        match self {
            Self::Browse => thumbnail::BROWSE_THUMBNAIL_WIDTH,
            Self::HighDensity => thumbnail::THUMBNAIL_WIDTH,
        }
    }
}
