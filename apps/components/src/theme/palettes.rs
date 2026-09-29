//! Application palettes. Both light themes keep their own tint, while dark
//! appearance uses one black and gold palette for either saved theme.

use app_preferences::{ApplicationTheme, ResolvedAppearance};

use super::ApplicationPalette;

const DARK_SURFACE: u32 = 0x11100d;
const DARK_GOLD: u32 = 0xd3b271;

const fn over_channel(base: u32, layer: u32, alpha: u32, shift: u32) -> u32 {
    (((base >> shift) & 0xff) * (255 - alpha) + ((layer >> shift) & 0xff) * alpha + 127) / 255
}

/// Precompose a translucent layer because GPUI replaces, rather than layers,
/// a control's background on hover.
const fn over(base: u32, layer: u32, alpha: u32) -> u32 {
    (over_channel(base, layer, alpha, 16) << 16) | (over_channel(base, layer, alpha, 8) << 8) | over_channel(base, layer, alpha, 0)
}

pub const fn palette(theme: ApplicationTheme, appearance: ResolvedAppearance) -> ApplicationPalette {
    match (theme, appearance) {
        (ApplicationTheme::Warm, ResolvedAppearance::Light) => ApplicationPalette {
            surface_bg: 0xfdfaf5,
            raised_bg: 0xfdfdf8,
            ink_text: 0x22201d,
            subtle_text: 0x6b675f,
            accent: 0x466853,
            text_accent: 0x8a6a2b,
            accent_hover: 0x3d5c49,
            accent_active: 0x34503f,
            accent_text: 0xfffdfa,
            border: 0x8c7f64,
            rule: 0xc5bdac,
            hover_overlay: 0x0000000d,
            active_highlight: 0xd9e8de,
            active_highlight_hover: 0xc7ddcf,
            danger_text: 0xa13029,
            danger_bg: 0xfde6e3,
            warning_text: 0x895c00,
            warning_bg: 0xfaeecf,
        },
        (ApplicationTheme::Cool, ResolvedAppearance::Light) => ApplicationPalette {
            surface_bg: 0xfafdff,
            raised_bg: 0xffffff,
            ink_text: 0x262627,
            subtle_text: 0x65686a,
            accent: 0x224a7f,
            text_accent: 0x8a6a2b,
            accent_hover: 0x1b3f6f,
            accent_active: 0x14355f,
            accent_text: 0xfdfdfe,
            border: 0x798189,
            rule: 0xb9bec4,
            hover_overlay: 0x0000000d,
            active_highlight: 0xdbe4f1,
            active_highlight_hover: 0xcad8eb,
            danger_text: 0xa13029,
            danger_bg: 0xfde6e3,
            warning_text: 0x895c00,
            warning_bg: 0xfaeecf,
        },
        (ApplicationTheme::Warm | ApplicationTheme::Cool, ResolvedAppearance::Dark) => ApplicationPalette {
            surface_bg: DARK_SURFACE,
            raised_bg: 0x1b1915,
            ink_text: 0xefe6d0,
            subtle_text: 0xa69c86,
            accent: DARK_GOLD,
            text_accent: DARK_GOLD,
            accent_hover: over(DARK_GOLD, 0xffffff, 38),
            accent_active: over(DARK_GOLD, 0xffffff, 72),
            accent_text: DARK_SURFACE,
            border: over(DARK_SURFACE, DARK_GOLD, 136),
            rule: over(DARK_SURFACE, DARK_GOLD, 80),
            hover_overlay: (DARK_GOLD << 8) | 0x14,
            active_highlight: over(DARK_SURFACE, DARK_GOLD, 56),
            active_highlight_hover: over(DARK_SURFACE, DARK_GOLD, 72),
            danger_text: 0xdf8a83,
            danger_bg: 0x401511,
            warning_text: 0xd4a444,
            warning_bg: 0x362c10,
        },
    }
}
