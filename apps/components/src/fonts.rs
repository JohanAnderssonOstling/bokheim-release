//! Interface and book fonts shared by every application platform.

use std::borrow::Cow;

use app_preferences::{BrowsingPreferences, UiFontFamily};
use gpui::{App, AsyncApp, Global, Pixels, Result, TextRun, Window, px};
use gpui_component::Theme;

/// A bundled face, also available to native browser DOM controls.
pub struct InterfaceFontFace {
    pub data: &'static [u8],
    pub weight: u16,
    pub style: &'static str,
}

const GARAMOND: [InterfaceFontFace; 4] = [
    InterfaceFontFace { data: include_bytes!("../../desktop-gpui/assets/fonts/NV_Garamond-Regular.ttf"), weight: 400, style: "normal" },
    InterfaceFontFace { data: include_bytes!("../../desktop-gpui/assets/fonts/NV_Garamond-Bold.ttf"), weight: 700, style: "normal" },
    InterfaceFontFace { data: include_bytes!("../../desktop-gpui/assets/fonts/NV_Garamond-Italic.ttf"), weight: 400, style: "italic" },
    InterfaceFontFace { data: include_bytes!("../../desktop-gpui/assets/fonts/NV_Garamond-BoldItalic.ttf"), weight: 700, style: "italic" },
];
const RALEWAY: [InterfaceFontFace; 4] = [
    InterfaceFontFace { data: include_bytes!("../../desktop-gpui/assets/fonts/Raleway-Regular.ttf"), weight: 400, style: "normal" },
    InterfaceFontFace { data: include_bytes!("../../desktop-gpui/assets/fonts/Raleway-Bold.ttf"), weight: 700, style: "normal" },
    InterfaceFontFace { data: include_bytes!("../../desktop-gpui/assets/fonts/Raleway-Italic.ttf"), weight: 400, style: "italic" },
    InterfaceFontFace { data: include_bytes!("../../desktop-gpui/assets/fonts/Raleway-BoldItalic.ttf"), weight: 700, style: "italic" },
];

pub fn interface_font_faces(family: UiFontFamily) -> &'static [InterfaceFontFace] {
    match family {
        UiFontFamily::Serif => &GARAMOND,
        UiFontFamily::SansSerif => &RALEWAY,
    }
}

/// Avoid resolving a saved Android font before background registration finishes:
/// GPUI caches failed font lookups as well as successful ones.
pub fn configured_ui_font_name<'a>(preferences: &'a BrowsingPreferences, cx: &App) -> &'a str {
    #[cfg(target_os = "android")]
    if let Some(family) = preferences.custom_ui_font_family()
        && cx.try_global::<SystemFontFamilies>().is_some_and(|fonts| !fonts.0.iter().any(|available| available == family))
    {
        return preferences.ui_font_family().family_name();
    }
    #[cfg(not(target_os = "android"))]
    let _ = cx;
    preferences.ui_font_name()
}

/// Measure interface labels with the active face, including its real glyph advances.
pub fn interface_label_width(label: &str, size_rem: f32, window: &Window, cx: &App) -> Pixels {
    let mut font = window.text_style().font();
    font.family = Theme::global(cx).font_family.clone();
    let run = TextRun { len: label.len(), font, ..Default::default() };
    let line = window.text_system().shape_line(label.to_owned().into(), window.rem_size() * size_rem, &[run], None);
    px(f32::from(line.width).ceil())
}

/// Space for equal-width choices without clipping their labels. Insets are the
/// total horizontal padding and borders within each choice.
pub fn equal_choice_group_width(labels: &[&str], size_rem: f32, inset: f32, gap: f32, window: &Window, cx: &App) -> Pixels {
    let widest = labels.iter().map(|label| interface_label_width(label, size_rem, window, cx)).max().unwrap_or(px(0.0));
    (widest + px(inset)) * labels.len() as f32 + px(gap * labels.len().saturating_sub(1) as f32)
}

struct FontsRegistered;

impl Global for FontsRegistered {}

/// A host-provided catalog of installed families, without GPUI's synthetic
/// fallback aliases. Android supplies this after registering device fonts.
pub struct SystemFontFamilies(pub Vec<String>);

impl Global for SystemFontFamilies {}

/// Registers the interface, book and serif display faces once per application.
pub fn register_fonts(cx: &mut App) -> Result<()> {
    if cx.has_global::<FontsRegistered>() {
        return Ok(());
    }
    let mut fonts: Vec<Cow<'static, [u8]>> = GARAMOND.iter().chain(RALEWAY.iter()).map(|face| Cow::Borrowed(face.data)).collect();
    fonts.extend([Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/BodoniModa-Regular.ttf").as_slice()), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/BodoniModa-SemiBold.ttf").as_slice())]);
    cx.text_system().add_fonts(fonts)?;
    cx.set_global(FontsRegistered);
    Ok(())
}

struct CjkFontsRegistered;
impl Global for CjkFontsRegistered {}

/// Compact common CJK coverage; users can select an installed regional face.
pub const CJK_FONT_FAMILY: &str = "Droid Sans Fallback";

#[cfg(not(target_arch = "wasm32"))]
const CJK_FONT_BYTES: &[u8] = include_bytes!("../../desktop-gpui/assets/fonts/DroidSansFallback.ttf");

/// Register fallback before constructing a book shaper. Native uses the bundled
/// face; web loads the same face on demand through the shared font cache.
pub async fn ensure_cjk_fonts(cx: &mut AsyncApp) -> Result<(), String> {
    cx.wait_for_update_slot().await;
    if cx.update(|cx| cx.has_global::<CjkFontsRegistered>()) {
        return Ok(());
    }
    let bytes = load_cjk_font().await?;
    cx.wait_for_update_slot().await;
    cx.update(move |cx| register_cjk_font_data(bytes, cx)).map_err(|error| error.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
async fn load_cjk_font() -> Result<Cow<'static, [u8]>, String> {
    Ok(Cow::Borrowed(CJK_FONT_BYTES))
}

#[cfg(target_arch = "wasm32")]
async fn load_cjk_font() -> Result<Cow<'static, [u8]>, String> {
    use js_sys::{Reflect, global};
    use wasm_bindgen::JsValue;

    let url = Reflect::get(&global(), &JsValue::from_str("__BOKHEIM_CJK_FONT_URL__"))
        .ok()
        .and_then(|value| value.as_string())
        .ok_or("bundled CJK font URL is unavailable")?;
    html_gpui::backend::load_remote_font(&url).await.map(Cow::Owned)
}

/// Synchronous native entry point for hosts that already own an App context.
#[cfg(not(target_arch = "wasm32"))]
pub fn register_cjk_fonts(cx: &mut App) -> Result<()> {
    register_cjk_font_data(Cow::Borrowed(CJK_FONT_BYTES), cx)
}

fn register_cjk_font_data(bytes: Cow<'static, [u8]>, cx: &mut App) -> Result<()> {
    if cx.has_global::<CjkFontsRegistered>() {
        return Ok(());
    }
    cx.text_system().add_fonts(vec![bytes])?;
    if let Some(fonts) = cx.try_global::<SystemFontFamilies>() {
        let mut families = fonts.0.clone();
        if !families.iter().any(|available| available == CJK_FONT_FAMILY) {
            families.push(CJK_FONT_FAMILY.to_owned());
        }
        cx.set_global(SystemFontFamilies(families));
    }
    cx.set_global(CjkFontsRegistered);
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod cjk_tests {
    use super::*;

    #[gpui::test]
    async fn cjk_registration_is_separate_and_idempotent(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_fonts(cx).unwrap();
            assert!(!cx.has_global::<CjkFontsRegistered>());
            cx.set_global(SystemFontFamilies(vec!["Latin font".into()]));
        });
        let mut async_cx = cx.to_async();
        ensure_cjk_fonts(&mut async_cx).await.unwrap();
        ensure_cjk_fonts(&mut async_cx).await.unwrap();
        cx.update(|cx| {
            register_cjk_fonts(cx).unwrap();
            assert!(cx.has_global::<CjkFontsRegistered>());
            assert_eq!(cx.global::<SystemFontFamilies>().0, vec!["Latin font", CJK_FONT_FAMILY]);
        });
    }
}
