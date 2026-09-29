//! Shared window size classes for adaptive GPUI layouts.

use gpui::Window;

pub const MEDIUM_WINDOW_WIDTH: f32 = 600.0;
pub const EXPANDED_WINDOW_WIDTH: f32 = 840.0;

/// Mobile app navigation also serves compact browser windows. Kobo keeps its
/// device controls instead of the phone's Back/title header.
pub fn uses_mobile_navigation(window: &Window) -> bool {
    has_native_back_button() || (cfg!(target_arch = "wasm32") && WindowWidthClass::for_window(window).is_compact())
}

/// Native mobile builds have a real OS back button/gesture (wired to the same
/// dismiss/return actions as the on-screen one), so the mobile navigation
/// heading skips its own back button there. Compact-width web builds share
/// the mobile heading layout but have no such OS-level back to fall back on,
/// so they keep the on-screen button.
pub fn has_native_back_button() -> bool {
    cfg!(any(target_os = "android", target_os = "ios", all(feature = "mobile", not(feature = "kobo"))))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowWidthClass {
    Compact,
    Medium,
    Expanded,
}

impl WindowWidthClass {
    pub fn for_window(window: &Window) -> Self {
        Self::for_width(f32::from(window.viewport_size().width))
    }

    pub fn for_width(width: f32) -> Self {
        if width < MEDIUM_WINDOW_WIDTH {
            Self::Compact
        } else if width < EXPANDED_WINDOW_WIDTH {
            Self::Medium
        } else {
            Self::Expanded
        }
    }

    pub fn is_compact(self) -> bool {
        self == Self::Compact
    }
}
