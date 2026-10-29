//! Reader preferences shared by all reader surfaces.
//!
//! [`ReaderSettings`] is mutable user state that is persisted on every change.

use std::rc::Rc;
use pdf_view_gpui::ReadingRulerStyle as PdfReadingRulerStyle;

use crate::cjk::CjkFont;
use html::pipeline::ReaderStyleOverrides;
use html::style::Hyphens;

use app_preferences::{DEFAULT_COLUMN_GAP_PX, DEFAULT_READER_MARGIN_PX, DEFAULT_WORD_SPACING_PERCENT, DEFAULT_PARAGRAPH_SPACING_EM, DEFAULT_FIRST_LINE_INDENT_EM, ReaderFontWeight, ReaderFirstLineIndent, ReaderFontFamily, ReaderHyphenation, ReaderPreferencePatch, ReaderTextAlignment};

/// The shared, synchronized description of how a PDF's layout is decided. The
/// reader carries `pdf_page_count` and `pdf_zoom` alongside it, so each mode
/// keeps the value it holds fixed.
pub use app_preferences::PdfZoomMode as PdfZoomModePreference;
use gpui::{App, Global};

/// A host can register device fonts in the background. Closing the sender
/// releases every book waiting to create its text shaper.
pub struct SystemFontsLoading(pub async_channel::Receiver<()>);

impl Global for SystemFontsLoading {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderFontRole {
    Serif,
    SansSerif,
    Monospace,
    Cjk,
}

impl ReaderFontRole {
    pub(crate) const ALL: [Self; 3] = [Self::Serif, Self::SansSerif, Self::Monospace];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Serif => "Serif",
            Self::SansSerif => "Sans serif",
            Self::Monospace => "Monospace",
            Self::Cjk => "CJK",
        }
    }

    fn generic(self) -> &'static str {
        match self {
            Self::Serif => "serif",
            Self::SansSerif => "sans-serif",
            Self::Monospace => "monospace",
            Self::Cjk => "sans-serif",
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Serif => 0,
            Self::SansSerif => 1,
            Self::Monospace => 2,
            Self::Cjk => 3,
        }
    }
}

/// Device-local typeface choice applied only to CJK text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CjkFontChoice {
    #[default]
    #[serde(alias = "bundled")]
    Publisher,
    Custom,
}

/// Which sidebar panel a reader last had open. TTS is not a panel: it plays
/// in a dock under the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SidebarTab {
    #[default]
    Contents,
    Notes,
    Search,
    #[serde(other)]
    Settings,
}

/// How long narration pauses between sentences and between passages.
/// `Natural` keeps the voice's own timing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NarrationPauses {
    Short,
    #[default]
    Natural,
    Long,
    Longer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NarrationVoice {
    George,
    #[default]
    #[serde(other)]
    Alba,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReaderPreferences {
    /// Reading guides are a local display preference and do not create book annotations.
    #[serde(default)]
    pub reading_ruler: html_view_gpui::ReadingRulerStyle,
    #[serde(default = "default_reading_ruler_style")]
    pub reading_ruler_last_style: html_view_gpui::ReadingRulerStyle,
    #[serde(default = "default_reading_ruler_lines")]
    pub reading_ruler_lines: usize,
    /// PDF guides are device-local and independent of the HTML ruler.
    #[serde(default)]
    pub pdf_reading_ruler: PdfReadingRulerStyle,
    #[serde(default = "default_pdf_reading_ruler_style")]
    pub pdf_reading_ruler_last_style: PdfReadingRulerStyle,
    #[serde(default = "default_reading_ruler_lines")]
    pub pdf_reading_ruler_lines: usize,
    pub font_size: f32,
    pub column_width: f64,
    #[serde(default = "default_min_line_width_ch")]
    pub min_line_width_ch: f64,
    #[serde(default = "default_max_line_width_ch")]
    pub max_line_width_ch: f64,
    pub scale: f64,
    pub toc_width: f32,
    #[serde(default)]
    pub active_sidebar_tab: SidebarTab,
    /// Whether the reader controls were hidden when last used.
    #[serde(default)]
    pub chrome_hidden: bool,
    /// Whether a sidebar tab is open, independently of mobile controls.
    #[serde(default = "default_sidebar_open")]
    pub sidebar_open: bool,
    #[serde(default)]
    pub font_family: ReaderFontFamily,
    /// Installed font names belong to this device and are not synchronized.
    #[serde(default)]
    pub system_font_family: Option<String>,
    #[serde(default)]
    pub system_sans_serif_font_family: Option<String>,
    #[serde(default)]
    pub system_monospace_font_family: Option<String>,
    /// None migrates the original custom-font settings, which had no profile switch.
    #[serde(default)]
    pub custom_fonts_enabled: Option<bool>,
    #[serde(default)]
    pub cjk_font_choice: CjkFontChoice,
    #[serde(default)]
    pub system_cjk_font_family: Option<String>,
    #[serde(default)]
    pub text_alignment: ReaderTextAlignment,
    #[serde(default = "default_line_height")]
    pub line_height: f32,
    /// False preserves the book's computed line heights and the saved custom value.
    #[serde(default = "default_override_line_height")]
    pub override_line_height: bool,
    #[serde(default = "default_paragraph_spacing_em")]
    pub paragraph_spacing_em: f32,
    #[serde(default)]
    pub override_paragraph_spacing: bool,
    #[serde(default = "default_first_line_indent_em")]
    pub first_line_indent_em: f32,
    #[serde(default, alias = "override_first_line_indent")]
    pub first_line_indent: ReaderFirstLineIndent,
    #[serde(default)]
    pub font_weight: ReaderFontWeight,
    #[serde(default = "default_word_spacing_percent")]
    pub word_spacing_percent: f32,
    #[serde(default)]
    pub hyphenation: ReaderHyphenation,
    #[serde(default = "default_reader_margin")]
    pub horizontal_margin_px: f32,
    #[serde(default = "default_reader_margin")]
    pub vertical_margin_px: f32,
    #[serde(default = "default_column_gap")]
    pub min_column_gap_px: f32,
    /// Set by the application from a user-facing setting, but not yet honoured
    /// by the native reader, which derives its column count from the viewport.
    #[serde(default = "default_max_column_count")]
    pub max_column_count: u8,
    /// Fixed-page spacing is saved independently for PDF, comics, and HTML.
    #[serde(default)]
    pub pdf_horizontal_margin_px: f32,
    #[serde(default)]
    pub pdf_vertical_margin_px: f32,
    #[serde(default)]
    pub pdf_page_gap_px: f32,
    #[serde(default)]
    pub comic_horizontal_margin_px: f32,
    #[serde(default)]
    pub comic_vertical_margin_px: f32,
    #[serde(default)]
    pub comic_page_gap_px: f32,
    #[serde(default = "default_pdf_page_count")]
    pub pdf_page_count: u8,
    #[serde(default = "default_pdf_trim_margins")]
    pub pdf_trim_margins: bool,
    /// Which end of the PDF layout the reader holds fixed. `pdf_page_count`
    /// carries the page count for [`PdfZoomModePreference::FitWidth`], and
    /// `pdf_zoom` the zoom for [`PdfZoomModePreference::Fixed`].
    #[serde(default)]
    pub pdf_zoom_mode: PdfZoomModePreference,
    /// Pixels per PDF point.
    #[serde(default = "default_pdf_zoom")]
    pub pdf_zoom: f32,
    /// How fast narration plays. Kept with the reader settings rather than with
    /// the book: it is a property of how someone listens, not of what they are
    /// listening to.
    #[serde(default = "default_audiobook_speed")]
    pub audiobook_speed: f64,
    /// Kept on this device only; the backend does not synchronize it.
    #[serde(default)]
    pub narration_pauses: NarrationPauses,
    /// The bundled narration voice selected on this device.
    #[serde(default)]
    pub narration_voice: NarrationVoice,
}

impl Default for ReaderPreferences {
    fn default() -> Self {
        Self {
            reading_ruler: html_view_gpui::ReadingRulerStyle::Off,
            reading_ruler_last_style: default_reading_ruler_style(),
            reading_ruler_lines: default_reading_ruler_lines(),
            pdf_reading_ruler: PdfReadingRulerStyle::Off,
            pdf_reading_ruler_last_style: default_pdf_reading_ruler_style(),
            pdf_reading_ruler_lines: default_reading_ruler_lines(),
            font_size: 20.0,
            column_width: 600.0,
            min_line_width_ch: default_min_line_width_ch(),
            max_line_width_ch: default_max_line_width_ch(),
            scale: 1.0,
            toc_width: 300.0,
            active_sidebar_tab: SidebarTab::default(),
            chrome_hidden: false,
            sidebar_open: default_sidebar_open(),
            font_family: ReaderFontFamily::default(),
            system_font_family: None,
            system_sans_serif_font_family: None,
            system_monospace_font_family: None,
            custom_fonts_enabled: None,
            cjk_font_choice: CjkFontChoice::default(),
            system_cjk_font_family: None,
            text_alignment: ReaderTextAlignment::default(),
            line_height: default_line_height(),
            override_line_height: default_override_line_height(),
            paragraph_spacing_em: DEFAULT_PARAGRAPH_SPACING_EM,
            override_paragraph_spacing: false,
            first_line_indent_em: DEFAULT_FIRST_LINE_INDENT_EM,
            first_line_indent: ReaderFirstLineIndent::Book,
            font_weight: ReaderFontWeight::Normal,
            word_spacing_percent: DEFAULT_WORD_SPACING_PERCENT,
            hyphenation: ReaderHyphenation::Book,
            horizontal_margin_px: default_reader_margin(),
            vertical_margin_px: default_reader_margin(),
            min_column_gap_px: DEFAULT_COLUMN_GAP_PX,
            max_column_count: default_max_column_count(),
            pdf_horizontal_margin_px: 0.0,
            pdf_vertical_margin_px: 0.0,
            pdf_page_gap_px: 0.0,
            comic_horizontal_margin_px: 0.0,
            comic_vertical_margin_px: 0.0,
            comic_page_gap_px: 0.0,
            pdf_page_count: default_pdf_page_count(),
            pdf_trim_margins: default_pdf_trim_margins(),
            pdf_zoom_mode: PdfZoomModePreference::default(),
            pdf_zoom: default_pdf_zoom(),
            audiobook_speed: default_audiobook_speed(),
            narration_pauses: NarrationPauses::default(),
            narration_voice: NarrationVoice::default(),
        }
    }
}

fn default_pdf_reading_ruler_style() -> PdfReadingRulerStyle {
    PdfReadingRulerStyle::Band
}

fn default_reading_ruler_lines() -> usize {
    1
}

fn default_reading_ruler_style() -> html_view_gpui::ReadingRulerStyle {
    html_view_gpui::ReadingRulerStyle::Band
}

fn default_reader_margin() -> f32 {
    DEFAULT_READER_MARGIN_PX
}

fn default_column_gap() -> f32 {
    DEFAULT_COLUMN_GAP_PX
}

fn default_sidebar_open() -> bool {
    !cfg!(any(feature = "mobile", feature = "kobo")) && !ui_components::touch_ui()
}

fn default_pdf_trim_margins() -> bool {
    true
}

fn default_min_line_width_ch() -> f64 {
    45.0
}

fn default_max_line_width_ch() -> f64 {
    65.0
}

fn default_pdf_zoom() -> f32 {
    1.0
}

fn default_pdf_page_count() -> u8 {
    2
}

fn default_paragraph_spacing_em() -> f32 { DEFAULT_PARAGRAPH_SPACING_EM }
fn default_first_line_indent_em() -> f32 { DEFAULT_FIRST_LINE_INDENT_EM }

fn default_word_spacing_percent() -> f32 {
    DEFAULT_WORD_SPACING_PERCENT
}

fn default_line_height() -> f32 {
    1.5
}

fn default_override_line_height() -> bool {
    true
}

fn default_max_column_count() -> u8 {
    2
}

fn default_audiobook_speed() -> f64 {
    1.0
}

impl ReaderPreferences {
    pub(crate) fn saved_cjk_font_family(&self) -> Option<&str> {
        self.system_cjk_font_family.as_deref().filter(|family| !family.trim().is_empty() && *family != ui_components::CJK_FONT_FAMILY)
    }

    pub(crate) fn custom_font_family(&self, role: ReaderFontRole) -> Option<&str> {
        match role {
            ReaderFontRole::Serif => self.system_font_family.as_deref(),
            ReaderFontRole::SansSerif => self.system_sans_serif_font_family.as_deref(),
            ReaderFontRole::Monospace => self.system_monospace_font_family.as_deref(),
            ReaderFontRole::Cjk => self.system_cjk_font_family.as_deref(),
        }
    }

    pub(crate) fn set_custom_font_family(&mut self, role: ReaderFontRole, family: String) {
        match role {
            ReaderFontRole::Serif => self.system_font_family = Some(family),
            ReaderFontRole::SansSerif => self.system_sans_serif_font_family = Some(family),
            ReaderFontRole::Monospace => self.system_monospace_font_family = Some(family),
            ReaderFontRole::Cjk => {
                self.system_cjk_font_family = Some(family);
                self.cjk_font_choice = CjkFontChoice::Custom;
                return;
            }
        }
        self.custom_fonts_enabled = Some(true);
    }

    pub(crate) fn has_custom_fonts(&self) -> bool {
        ReaderFontRole::ALL.into_iter().any(|role| self.custom_font_family(role).is_some())
    }

    pub(crate) fn custom_fonts_active(&self) -> bool {
        self.custom_fonts_enabled.unwrap_or_else(|| self.has_custom_fonts())
    }

    fn custom_font_override(&self, role: ReaderFontRole) -> Option<String> {
        if !self.custom_fonts_active() {
            return None;
        }
        self.custom_font_family(role).map(|family| format!("\"{}\", {}", family.replace('\\', "\\\\").replace('"', "\\\""), role.generic()))
    }

    /// Applies the portion of reader state synchronized by the backend while
    /// preserving UI-only state such as the active sidebar tab and its width.
    pub fn apply_synchronized(&mut self, saved: &app_preferences::ReaderPreferences) {
        self.font_size = saved.font_size_px();
        self.column_width = saved.max_line_width_px();
        self.min_line_width_ch = saved.min_line_width_ch();
        self.max_line_width_ch = saved.max_line_width_ch();
        self.scale = saved.zoom();
        self.line_height = saved.line_height();
        self.override_line_height = saved.override_line_height();
        self.paragraph_spacing_em = saved.paragraph_spacing_em();
        self.override_paragraph_spacing = saved.override_paragraph_spacing();
        self.first_line_indent_em = saved.first_line_indent_em();
        self.first_line_indent = saved.first_line_indent();
        self.font_weight = saved.font_weight();
        self.word_spacing_percent = saved.word_spacing_percent();
        self.hyphenation = saved.hyphenation();
        self.font_family = saved.font_family();
        self.text_alignment = saved.text_alignment();
        self.max_column_count = saved.max_column_count();
        self.horizontal_margin_px = saved.horizontal_margin_px();
        self.vertical_margin_px = saved.vertical_margin_px();
        self.min_column_gap_px = saved.min_column_gap_px();
        self.pdf_zoom_mode = saved.pdf_zoom_mode();
        self.pdf_page_count = saved.pdf_page_count();
        self.pdf_zoom = saved.pdf_zoom();
        self.pdf_horizontal_margin_px = saved.pdf_horizontal_margin_px();
        self.pdf_vertical_margin_px = saved.pdf_vertical_margin_px();
        self.pdf_page_gap_px = saved.pdf_page_gap_px();
        self.comic_horizontal_margin_px = saved.comic_horizontal_margin_px();
        self.comic_vertical_margin_px = saved.comic_vertical_margin_px();
        self.comic_page_gap_px = saved.comic_page_gap_px();
        self.pdf_trim_margins = saved.pdf_trim_margins();
        self.audiobook_speed = saved.audiobook_speed();
    }

    /// Produces the backend preference updates represented by a UI change.
    pub fn synchronized_patches(&self, next: &Self) -> Vec<ReaderPreferencePatch> {
        let mut patches = Vec::new();
        if self.font_size != next.font_size {
            patches.push(ReaderPreferencePatch::FontSizePx(next.font_size));
        }
        if self.column_width != next.column_width {
            patches.push(ReaderPreferencePatch::MaxLineWidthPx(next.column_width));
        }
        if self.min_line_width_ch != next.min_line_width_ch {
            patches.push(ReaderPreferencePatch::MinLineWidthCh(next.min_line_width_ch));
        }
        if self.max_line_width_ch != next.max_line_width_ch {
            patches.push(ReaderPreferencePatch::MaxLineWidthCh(next.max_line_width_ch));
        }
        if self.scale != next.scale {
            patches.push(ReaderPreferencePatch::Zoom(next.scale));
        }
        if self.line_height != next.line_height {
            patches.push(ReaderPreferencePatch::LineHeight(next.line_height));
        }
        if self.override_line_height != next.override_line_height {
            patches.push(ReaderPreferencePatch::OverrideLineHeight(next.override_line_height));
        }
        if self.paragraph_spacing_em != next.paragraph_spacing_em {
            patches.push(ReaderPreferencePatch::ParagraphSpacingEm(next.paragraph_spacing_em));
        }
        if self.override_paragraph_spacing != next.override_paragraph_spacing {
            patches.push(ReaderPreferencePatch::OverrideParagraphSpacing(next.override_paragraph_spacing));
        }
        if self.first_line_indent_em != next.first_line_indent_em {
            patches.push(ReaderPreferencePatch::FirstLineIndentEm(next.first_line_indent_em));
        }
        if self.first_line_indent != next.first_line_indent {
            patches.push(ReaderPreferencePatch::FirstLineIndent(next.first_line_indent));
        }
        if self.hyphenation != next.hyphenation {
            patches.push(ReaderPreferencePatch::Hyphenation(next.hyphenation));
        }
        if self.word_spacing_percent != next.word_spacing_percent {
            patches.push(ReaderPreferencePatch::WordSpacingPercent(next.word_spacing_percent));
        }
        if self.font_weight != next.font_weight {
            patches.push(ReaderPreferencePatch::FontWeight(next.font_weight));
        }
        if self.font_family != next.font_family {
            patches.push(ReaderPreferencePatch::FontFamily(next.font_family));
        }
        if self.text_alignment != next.text_alignment {
            patches.push(ReaderPreferencePatch::TextAlignment(next.text_alignment));
        }
        if self.max_column_count != next.max_column_count {
            patches.push(ReaderPreferencePatch::MaxColumnCount(next.max_column_count));
        }
        if self.horizontal_margin_px != next.horizontal_margin_px {
            patches.push(ReaderPreferencePatch::HorizontalMarginPx(next.horizontal_margin_px));
        }
        if self.vertical_margin_px != next.vertical_margin_px {
            patches.push(ReaderPreferencePatch::VerticalMarginPx(next.vertical_margin_px));
        }
        if self.min_column_gap_px != next.min_column_gap_px {
            patches.push(ReaderPreferencePatch::MinColumnGapPx(next.min_column_gap_px));
        }
        if self.pdf_zoom_mode != next.pdf_zoom_mode {
            patches.push(ReaderPreferencePatch::PdfZoomMode(next.pdf_zoom_mode));
        }
        if self.pdf_page_count != next.pdf_page_count {
            patches.push(ReaderPreferencePatch::PdfPageCount(next.pdf_page_count));
        }
        if self.pdf_zoom != next.pdf_zoom {
            patches.push(ReaderPreferencePatch::PdfZoom(next.pdf_zoom));
        }
        if self.pdf_horizontal_margin_px != next.pdf_horizontal_margin_px {
            patches.push(ReaderPreferencePatch::PdfHorizontalMarginPx(next.pdf_horizontal_margin_px));
        }
        if self.pdf_vertical_margin_px != next.pdf_vertical_margin_px {
            patches.push(ReaderPreferencePatch::PdfVerticalMarginPx(next.pdf_vertical_margin_px));
        }
        if self.pdf_page_gap_px != next.pdf_page_gap_px {
            patches.push(ReaderPreferencePatch::PdfPageGapPx(next.pdf_page_gap_px));
        }
        if self.comic_horizontal_margin_px != next.comic_horizontal_margin_px {
            patches.push(ReaderPreferencePatch::ComicHorizontalMarginPx(next.comic_horizontal_margin_px));
        }
        if self.comic_vertical_margin_px != next.comic_vertical_margin_px {
            patches.push(ReaderPreferencePatch::ComicVerticalMarginPx(next.comic_vertical_margin_px));
        }
        if self.comic_page_gap_px != next.comic_page_gap_px {
            patches.push(ReaderPreferencePatch::ComicPageGapPx(next.comic_page_gap_px));
        }
        if self.pdf_trim_margins != next.pdf_trim_margins {
            patches.push(ReaderPreferencePatch::PdfTrimMargins(next.pdf_trim_margins));
        }
        if self.audiobook_speed != next.audiobook_speed {
            patches.push(ReaderPreferencePatch::AudiobookSpeed(next.audiobook_speed));
        }
        patches
    }

    pub(crate) fn apply_cjk_font(&self, overrides: &mut ReaderStyleOverrides, cjk: Option<CjkFont>) {
        if cjk.is_none() {
            return;
        }
        let embedded = format!("\"{}\"", ui_components::CJK_FONT_FAMILY);
        overrides.cjk_font_fallback = Some(embedded.clone());
        overrides.cjk_font_family = match self.cjk_font_choice {
            CjkFontChoice::Publisher => None,
            CjkFontChoice::Custom => Some(self.system_cjk_font_family.as_deref().filter(|family| !family.trim().is_empty()).map(|family| format!("\"{}\"", family.replace('\\', "\\\\").replace('"', "\\\""))).unwrap_or(embedded)),
        };
    }

    pub(crate) fn style_overrides_for_book(&self, cjk: Option<CjkFont>, theme: Option<html::pipeline::ThemeAdaptation>, link_foreground: u32) -> ReaderStyleOverrides {
        let mut overrides = self.style_overrides(theme, link_foreground);
        self.apply_cjk_font(&mut overrides, cjk);
        overrides
    }

    pub(crate) fn style_overrides(&self, theme: Option<html::pipeline::ThemeAdaptation>, link_foreground: u32) -> html::pipeline::ReaderStyleOverrides {
        let family = match self.font_family {
            ReaderFontFamily::Publisher => None,
            ReaderFontFamily::Serif => Some("NV Garamond, serif".to_owned()),
            ReaderFontFamily::SansSerif => Some("Raleway, sans-serif".to_owned()),
        };
        let family = if self.custom_fonts_active() { None } else { family };
        let alignment = match self.text_alignment {
            ReaderTextAlignment::Publisher => None,
            ReaderTextAlignment::Start => Some(html::pipeline::TextAlign::Left),
            ReaderTextAlignment::Justify => Some(html::pipeline::TextAlign::Justify),
        };
        // `foreground` stays unset on purpose: it replaces every run's colour,
        // which flattens a book rather than adapting it. `theme` is the
        // one that re-lights instead of overwriting.
        html::pipeline::ReaderStyleOverrides {
            hyphens: match self.hyphenation {
                ReaderHyphenation::Book => None,
                ReaderHyphenation::Off => Some(Hyphens::None),
                ReaderHyphenation::On => Some(Hyphens::Auto),
            },
            font_weight_adjustment: self.font_weight.adjustment(),
            word_spacing_factor: (self.word_spacing_percent != DEFAULT_WORD_SPACING_PERCENT).then_some(self.word_spacing_percent / 100.0),
            font_family: family,
            cjk_font_family: None,
            cjk_font_fallback: None,
            serif_font_family: self.custom_font_override(ReaderFontRole::Serif),
            sans_serif_font_family: self.custom_font_override(ReaderFontRole::SansSerif),
            monospace_font_family: self.custom_font_override(ReaderFontRole::Monospace),
            text_align: alignment,
            line_height: self.override_line_height.then_some(self.line_height),
            paragraph_spacing_em: self.override_paragraph_spacing.then_some(self.paragraph_spacing_em),
            first_line_indent_em: match self.first_line_indent {
                ReaderFirstLineIndent::Book => None,
                ReaderFirstLineIndent::Off => Some(0.0),
                ReaderFirstLineIndent::Custom => Some(self.first_line_indent_em),
            },
            // The font-size control scales publisher typography from the CSS
            // regular size, including fixed sizes and nested relative sizes.
            canonical_font_size: Some(16.0),
            minimum_font_size: None,
            foreground: None,
            link_foreground: Some(link_foreground),
            background: None,
            strip_root_spacing: true,
            theme,
        }
    }
}

/// The two ends of the reader's lightness range, taken from the application
/// theme, so a book's colours are re-lit into the page it is drawn on.
pub(crate) fn library_theme_adaptation(cx: &App) -> html::pipeline::ThemeAdaptation {
    let theme = ui_components::browser_theme(cx);
    let opaque = |colour: gpui::Hsla| u32::from(gpui::Rgba::from(colour)) | 0xff;
    html::pipeline::ThemeAdaptation { ink: opaque(theme.text), surface: opaque(theme.page_bg) }
}

pub(crate) fn library_link_foreground(cx: &App) -> u32 {
    u32::from(gpui::Rgba::from(ui_components::browser_theme(cx).accent)) | 0xff
}

pub(crate) fn library_paint_palette(cx: &App) -> skrift_column_paginator::ReaderPaintPalette {
    let theme = ui_components::browser_theme(cx);
    // A paint-time foreground replaces the computed color of every text run,
    // including colors explicitly supplied by the book. Leave it unset
    // so EPUB styles reach the painter; the reader theme still owns the canvas.
    skrift_column_paginator::ReaderPaintPalette { foreground: None, background: Some(u32::from(gpui::Rgba::from(theme.page_bg))) }
}

/// The reader's highlights, in the two spellings its renderers use.
///
/// What the colours *are* is [`ui_components::InteractionHighlights`]; this is
/// only the conversion, kept here because the two renderer colour types are the
/// reader's business and not the design system's.
pub(crate) fn library_interaction_palette(cx: &App) -> skrift_column_paginator::InteractionPalette {
    let highlights = ui_components::InteractionHighlights::from_theme(ui_components::browser_theme(cx));
    let colour = |[r, g, b, a]: [u8; 4]| skrift_column_paginator::Color::rgba8(r, g, b, a);
    skrift_column_paginator::InteractionPalette {
        selection: colour(highlights.selection),
        search_match: colour(highlights.search_match),
        active_search_match: colour(highlights.active_search_match),
        annotation: colour(highlights.annotation),
        active_annotation: colour(highlights.active_annotation),
        reading: colour(highlights.reading),
    }
}

pub(crate) fn library_interaction_style(cx: &App) -> pdf_reader_core::ReaderInteractionStyle {
    let highlights = ui_components::InteractionHighlights::from_theme(ui_components::browser_theme(cx));
    let colour = |[r, g, b, a]: [u8; 4]| pdf_reader_core::RgbaColor::new(r, g, b, a);
    pdf_reader_core::ReaderInteractionStyle {
        selection: colour(highlights.selection),
        search_match: colour(highlights.search_match),
        active_search_match: colour(highlights.active_search_match),
        annotation: colour(highlights.annotation),
        active_annotation: colour(highlights.active_annotation),
        reading: colour(highlights.reading),
    }
}

/// Persists the full preferences locally and the changed synchronized fields
/// remotely. The reader computes patches while it owns both versions.
pub type SaveReaderSettings = Rc<dyn Fn(ReaderPreferences, Vec<ReaderPreferencePatch>, &mut App)>;

/// User state shared by every reader surface, persisted through [`Self::save`]
/// whenever it changes.
pub(crate) struct ReaderSettings {
    pub(crate) preferences: ReaderPreferences,
    pub(crate) save: SaveReaderSettings,
}

impl Global for ReaderSettings {}

impl ReaderSettings {
    /// Applies `update` to the shared preferences and persists them.
    ///
    /// Returns whether anything actually changed; a no-op update neither
    /// persists nor reports a change, so callers can use the result to decide
    /// whether to repaint.
    pub(crate) fn update(cx: &mut App, update: impl FnOnce(&mut ReaderPreferences)) -> bool {
        let Some((preferences, patches, save)) = ({
            let settings = cx.global_mut::<Self>();
            let previous = settings.preferences.clone();
            update(&mut settings.preferences);
            (previous != settings.preferences).then(|| (settings.preferences.clone(), previous.synchronized_patches(&settings.preferences), settings.save.clone()))
        }) else {
            return false;
        };
        save(preferences, patches, cx);
        true
    }

    pub(crate) fn preferences(cx: &App) -> &ReaderPreferences {
        &cx.global::<Self>().preferences
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[test]
    fn pdf_ruler_preferences_are_local_independent_and_migrate_older_profiles() {
        let mut defaults = ReaderPreferences::default();
        let synchronized = app_preferences::ReaderPreferences::default();
        defaults.apply_synchronized(&synchronized);
        let preferences = ReaderPreferences {
            reading_ruler: html_view_gpui::ReadingRulerStyle::Underline,
            pdf_reading_ruler: PdfReadingRulerStyle::Focus,
            pdf_reading_ruler_last_style: PdfReadingRulerStyle::Band,
            pdf_reading_ruler_lines: 3,
            ..defaults.clone()
        };
        assert!(defaults.synchronized_patches(&preferences).is_empty());
        let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&preferences).unwrap()).unwrap();
        restored.apply_synchronized(&synchronized);
        assert_eq!(restored, preferences);
        restored.pdf_reading_ruler = PdfReadingRulerStyle::Off;
        let saved = serde_json::to_value(&restored).unwrap();
        let disabled: ReaderPreferences = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(disabled.pdf_reading_ruler_last_style, PdfReadingRulerStyle::Band);
        assert_eq!(disabled.reading_ruler, html_view_gpui::ReadingRulerStyle::Underline);
        let mut legacy = saved;
        for field in ["pdf_reading_ruler", "pdf_reading_ruler_last_style", "pdf_reading_ruler_lines"] { legacy.as_object_mut().unwrap().remove(field); }
        let migrated: ReaderPreferences = serde_json::from_value(legacy).unwrap();
        assert_eq!(migrated.pdf_reading_ruler, PdfReadingRulerStyle::Off);
        assert_eq!(migrated.pdf_reading_ruler_last_style, PdfReadingRulerStyle::Band);
        assert_eq!(migrated.pdf_reading_ruler_lines, 1);
        assert_eq!(migrated.reading_ruler, html_view_gpui::ReadingRulerStyle::Underline);
    }

    #[test]
    fn pdf_and_comic_spacing_round_trip_without_changing_html_spacing() {
        let mut previous = ReaderPreferences::default();
        previous.apply_synchronized(&app_preferences::ReaderPreferences::default());
        let next = ReaderPreferences {
            pdf_horizontal_margin_px: 8.0, pdf_vertical_margin_px: 16.0, pdf_page_gap_px: 24.0,
            comic_horizontal_margin_px: 32.0, comic_vertical_margin_px: 40.0, comic_page_gap_px: 48.0,
            ..previous.clone()
        };
        let patches = previous.synchronized_patches(&next);
        assert_eq!(patches.len(), 6);
        let mut backend = app_preferences::ReaderPreferences::default();
        for patch in patches { patch.apply(&mut backend).unwrap(); }
        let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&next).unwrap()).unwrap();
        restored.apply_synchronized(&backend);
        assert_eq!(restored, next);
        assert_eq!(restored.horizontal_margin_px, previous.horizontal_margin_px);
        assert_eq!(restored.vertical_margin_px, previous.vertical_margin_px);
        assert_eq!(restored.min_column_gap_px, previous.min_column_gap_px);
        let mut legacy = serde_json::to_value(&next).unwrap();
        for field in ["pdf_horizontal_margin_px", "pdf_vertical_margin_px", "pdf_page_gap_px", "comic_horizontal_margin_px", "comic_vertical_margin_px", "comic_page_gap_px"] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        assert_eq!(serde_json::from_value::<ReaderPreferences>(legacy).unwrap(), previous);
    }

    #[test]
    fn hyphenation_choices_reach_the_renderer_and_survive_backend_updates() {
        let previous = ReaderPreferences::default();
        let mut legacy = serde_json::to_value(&previous).unwrap();
        legacy.as_object_mut().unwrap().remove("hyphenation");
        assert_eq!(serde_json::from_value::<ReaderPreferences>(legacy).unwrap().hyphenation, ReaderHyphenation::Book);
        for choice in ReaderHyphenation::ALL {
            let mut preferences = previous.clone();
            preferences.hyphenation = choice;
            let mut backend = app_preferences::ReaderPreferences::default();
            for patch in previous.synchronized_patches(&preferences) { patch.apply(&mut backend).unwrap(); }
            let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&preferences).unwrap()).unwrap();
            restored.apply_synchronized(&backend);
            assert_eq!(restored.hyphenation, choice);
            assert_eq!(restored.style_overrides(None, 0).hyphens, match choice {
                ReaderHyphenation::Book => None,
                ReaderHyphenation::Off => Some(Hyphens::None),
                ReaderHyphenation::On => Some(Hyphens::Auto),
            });
        }
    }

    #[test]
    fn first_line_indent_off_removes_indentation_without_losing_custom_value() {
        let mut current = ReaderPreferences::default();
        current.first_line_indent_em = 2.1;
        current.first_line_indent = ReaderFirstLineIndent::Custom;
        let mut backend = app_preferences::ReaderPreferences::default();
        backend.set_first_line_indent_em(2.1).unwrap();
        backend.set_first_line_indent(ReaderFirstLineIndent::Custom);
        for (mode, expected) in [(ReaderFirstLineIndent::Off, Some(0.0)), (ReaderFirstLineIndent::Book, None), (ReaderFirstLineIndent::Custom, Some(2.1))] {
            let mut next = current.clone();
            next.first_line_indent = mode;
            let patches = current.synchronized_patches(&next);
            assert_eq!(patches, vec![ReaderPreferencePatch::FirstLineIndent(mode)]);
            for patch in patches { patch.apply(&mut backend).unwrap(); }
            let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&next).unwrap()).unwrap();
            restored.apply_synchronized(&backend);
            assert_eq!(restored.first_line_indent, mode);
            assert_eq!(restored.first_line_indent_em, 2.1);
            assert_eq!(restored.style_overrides(None, 0).first_line_indent_em, expected);
            current = restored;
        }
        for (enabled, expected) in [(false, ReaderFirstLineIndent::Book), (true, ReaderFirstLineIndent::Custom)] {
            let mut legacy = serde_json::to_value(&current).unwrap();
            legacy.as_object_mut().unwrap().remove("first_line_indent");
            legacy["override_first_line_indent"] = serde_json::json!(enabled);
            let restored: ReaderPreferences = serde_json::from_value(legacy).unwrap();
            assert_eq!(restored.first_line_indent, expected);
            assert_eq!(restored.first_line_indent_em, 2.1);
        }
    }

    #[test]
    fn paragraph_controls_reach_layout_and_preserve_custom_values_in_book_mode() {
        let previous = ReaderPreferences::default();
        let mut legacy = serde_json::to_value(&previous).unwrap();
        for field in ["paragraph_spacing_em", "override_paragraph_spacing", "first_line_indent_em", "first_line_indent"] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        let migrated: ReaderPreferences = serde_json::from_value(legacy).unwrap();
        assert_eq!(migrated.paragraph_spacing_em, 1.0);
        assert_eq!(migrated.first_line_indent_em, 1.5);
        assert_eq!(migrated.style_overrides(None, 0).paragraph_spacing_em, None);
        assert_eq!(migrated.style_overrides(None, 0).first_line_indent_em, None);
        let mut custom = previous.clone();
        custom.paragraph_spacing_em = 0.7;
        custom.first_line_indent_em = 2.1;
        custom.override_paragraph_spacing = true;
        custom.first_line_indent = ReaderFirstLineIndent::Custom;
        let mut backend = app_preferences::ReaderPreferences::default();
        for patch in previous.synchronized_patches(&custom) { patch.apply(&mut backend).unwrap(); }
        let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&custom).unwrap()).unwrap();
        restored.apply_synchronized(&backend);
        assert_eq!(restored.style_overrides(None, 0).paragraph_spacing_em, Some(0.7));
        assert_eq!(restored.style_overrides(None, 0).first_line_indent_em, Some(2.1));
        let mut book = custom.clone();
        book.override_paragraph_spacing = false;
        book.first_line_indent = ReaderFirstLineIndent::Book;
        for patch in custom.synchronized_patches(&book) { patch.apply(&mut backend).unwrap(); }
        restored.apply_synchronized(&backend);
        assert_eq!(restored.paragraph_spacing_em, 0.7);
        assert_eq!(restored.first_line_indent_em, 2.1);
        assert_eq!(restored.style_overrides(None, 0).paragraph_spacing_em, None);
        assert_eq!(restored.style_overrides(None, 0).first_line_indent_em, None);
    }

    #[test]
    fn word_spacing_reaches_the_renderer_and_survives_backend_updates() {
        let previous = ReaderPreferences::default();
        let mut legacy = serde_json::to_value(&previous).unwrap();
        legacy.as_object_mut().unwrap().remove("word_spacing_percent");
        assert_eq!(serde_json::from_value::<ReaderPreferences>(legacy).unwrap().word_spacing_percent, 100.0);
        assert_eq!(previous.style_overrides(None, 0).word_spacing_factor, None);
        let mut preferences = previous.clone();
        preferences.word_spacing_percent = 120.0;
        let mut backend = app_preferences::ReaderPreferences::default();
        for patch in previous.synchronized_patches(&preferences) { patch.apply(&mut backend).unwrap(); }
        let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&preferences).unwrap()).unwrap();
        restored.apply_synchronized(&backend);
        assert_eq!(restored.style_overrides(None, 0).word_spacing_factor, Some(1.2));
    }

    #[test]
    fn font_weight_choices_reach_the_renderer_and_survive_backend_updates() {
        let previous = ReaderPreferences::default();
        for choice in ReaderFontWeight::ALL {
            let mut preferences = previous.clone();
            preferences.font_weight = choice;
            let mut backend = app_preferences::ReaderPreferences::default();
            for patch in previous.synchronized_patches(&preferences) {
                patch.apply(&mut backend).unwrap();
            }
            let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&preferences).unwrap()).unwrap();
            restored.apply_synchronized(&backend);
            assert_eq!(restored.style_overrides(None, 0).font_weight_adjustment, choice.adjustment());
        }
    }

    #[test]
    fn publisher_line_height_survives_reload_and_backend_preference_updates() {
        let mut preferences = ReaderPreferences::default();
        preferences.line_height = 1.8;
        let mut legacy = serde_json::to_value(&preferences).unwrap();
        legacy.as_object_mut().unwrap().remove("override_line_height");
        let previous: ReaderPreferences = serde_json::from_value(legacy).unwrap();
        assert!(previous.override_line_height);
        assert_eq!(previous.style_overrides(None, 0).line_height, Some(1.8));

        preferences.override_line_height = false;
        let patches = previous.synchronized_patches(&preferences);
        assert_eq!(patches, vec![ReaderPreferencePatch::OverrideLineHeight(false)]);
        let mut saved = app_preferences::ReaderPreferences::default();
        saved.set_line_height(1.8).unwrap();
        for patch in patches {
            patch.apply(&mut saved).unwrap();
        }
        let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&preferences).unwrap()).unwrap();
        restored.apply_synchronized(&saved);
        assert!(!restored.override_line_height);
        assert_eq!(restored.line_height, 1.8);
        assert_eq!(restored.style_overrides(None, 0).line_height, None);

        restored.override_line_height = true;
        assert_eq!(restored.style_overrides(None, 0).line_height, Some(1.8));
    }

    #[test]
    fn legacy_pdf_direction_overrides_do_not_survive_ui_preferences_reload() {
        let defaults = ReaderPreferences::default();
        let mut saved = serde_json::to_value(&defaults).unwrap();
        saved["pdf_reading_direction"] = serde_json::json!("right_to_left");
        saved["pdf_right_to_left"] = serde_json::json!(true);
        let restored: ReaderPreferences = serde_json::from_value(saved).unwrap();
        assert_eq!(restored, defaults);
        assert!(defaults.synchronized_patches(&restored).is_empty());
        let saved = serde_json::to_value(restored).unwrap();
        assert!(saved.get("pdf_reading_direction").is_none());
        assert!(saved.get("pdf_right_to_left").is_none());
    }

    #[test]
    fn ruler_settings_survive_local_reload_without_synchronization() {
        let synchronized = app_preferences::ReaderPreferences::default();
        let mut defaults = ReaderPreferences::default();
        defaults.apply_synchronized(&synchronized);
        let preferences = ReaderPreferences { reading_ruler: html_view_gpui::ReadingRulerStyle::Focus, reading_ruler_last_style: html_view_gpui::ReadingRulerStyle::Underline, reading_ruler_lines: 3, ..defaults.clone() };
        assert!(defaults.synchronized_patches(&preferences).is_empty());
        let mut restored: ReaderPreferences = serde_json::from_str(&serde_json::to_string(&preferences).unwrap()).unwrap();
        restored.apply_synchronized(&synchronized);
        assert_eq!(restored.reading_ruler, preferences.reading_ruler);
        assert_eq!(restored.reading_ruler_last_style, preferences.reading_ruler_last_style);
        assert_eq!(restored.reading_ruler_lines, 3);
        assert!(defaults.synchronized_patches(&restored).is_empty());
        restored.reading_ruler = html_view_gpui::ReadingRulerStyle::Off;
        let disabled: ReaderPreferences = serde_json::from_str(&serde_json::to_string(&restored).unwrap()).unwrap();
        assert_eq!(disabled.reading_ruler_last_style, preferences.reading_ruler_last_style);
        assert!(defaults.synchronized_patches(&disabled).is_empty());
    }

    #[test]
    fn system_font_is_local_and_survives_reload_and_synchronized_updates() {
        let mut preferences = ReaderPreferences::default();
        preferences.system_font_family = Some("Noto Serif".into());
        assert!(ReaderPreferences::default().synchronized_patches(&preferences).is_empty());
        let saved = serde_json::to_value(&preferences).unwrap();
        let mut restored: ReaderPreferences = serde_json::from_value(saved.clone()).unwrap();
        restored.apply_synchronized(&app_preferences::ReaderPreferences::default());
        assert_eq!(restored.system_font_family.as_deref(), Some("Noto Serif"));
        assert_eq!(restored.style_overrides(None, 0).font_family, None);
        assert_eq!(restored.style_overrides(None, 0).serif_font_family.as_deref(), Some("\"Noto Serif\", serif"));
        let mut legacy = saved;
        legacy.as_object_mut().unwrap().remove("system_font_family");
        assert_eq!(serde_json::from_value::<ReaderPreferences>(legacy).unwrap().system_font_family, None);
    }

    #[test]
    fn cjk_profile_is_local_and_independent_of_latin_fonts() {
        let mut preferences = ReaderPreferences::default();
        preferences.set_custom_font_family(ReaderFontRole::Serif, "NV Garamond".into());
        let latin = preferences.style_overrides_for_book(None, None, 0);
        let book = preferences.style_overrides_for_book(Some(CjkFont::Japanese), None, 0);
        assert_eq!(book.font_family, latin.font_family);
        assert_eq!(book.serif_font_family, latin.serif_font_family);
        assert_eq!(book.cjk_font_family, None);
        assert_eq!(book.cjk_font_fallback.as_deref(), Some("\"Droid Sans Fallback\""));
        let before = preferences.clone();
        preferences.set_custom_font_family(ReaderFontRole::Cjk, "A, B's \"CJK\"".into());
        assert!(before.synchronized_patches(&preferences).is_empty());
        assert_eq!(preferences.style_overrides_for_book(None, None, 0), latin);
        let custom = preferences.style_overrides_for_book(Some(CjkFont::Korean), None, 0);
        assert_eq!(custom.serif_font_family, latin.serif_font_family);
        assert_eq!(custom.font_family, latin.font_family);
        assert_eq!(custom.cjk_font_family.as_deref(), Some("\"A, B's \\\"CJK\\\"\""));
        let mut restored: ReaderPreferences = serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        restored.apply_synchronized(&app_preferences::ReaderPreferences::default());
        assert_eq!(restored.cjk_font_choice, CjkFontChoice::Custom);
        assert_eq!(restored.system_cjk_font_family, preferences.system_cjk_font_family);
        restored.cjk_font_choice = CjkFontChoice::Publisher;
        let publisher = restored.style_overrides_for_book(Some(CjkFont::Japanese), None, 0);
        assert_eq!(publisher.cjk_font_family, None);
        assert!(publisher.cjk_font_fallback.is_some());
        assert_eq!(restored.system_cjk_font_family, preferences.system_cjk_font_family);
    }

    #[test]
    fn legacy_settings_default_to_book_cjk_and_empty_custom_uses_embedded_fallback() {
        let mut saved = serde_json::to_value(ReaderPreferences::default()).unwrap();
        saved.as_object_mut().unwrap().remove("cjk_font_choice");
        saved.as_object_mut().unwrap().remove("system_cjk_font_family");
        let mut preferences: ReaderPreferences = serde_json::from_value(saved).unwrap();
        assert_eq!(preferences.cjk_font_choice, CjkFontChoice::Publisher);
        assert_eq!(serde_json::from_str::<CjkFontChoice>("\"bundled\"").unwrap(), CjkFontChoice::Publisher);
        preferences.set_custom_font_family(ReaderFontRole::Cjk, "  ".into());
        assert!(!preferences.custom_fonts_active());
        let overrides = preferences.style_overrides_for_book(Some(CjkFont::Japanese), None, 0);
        assert_eq!(overrides.cjk_font_family.as_deref(), Some("\"Droid Sans Fallback\""));
        assert_eq!(overrides.cjk_font_fallback.as_deref(), Some("\"Droid Sans Fallback\""));
    }

    #[test]
    fn custom_profile_selects_immediately_and_keeps_fonts_when_using_presets() {
        let mut preferences = ReaderPreferences::default();
        preferences.custom_fonts_enabled = Some(true);
        assert!(preferences.custom_fonts_active());
        assert_eq!(preferences.style_overrides(None, 0).font_family, None);
        preferences.set_custom_font_family(ReaderFontRole::SansSerif, "Noto Serif".into());
        preferences.custom_fonts_enabled = Some(false);
        preferences.font_family = ReaderFontFamily::Serif;
        let overrides = preferences.style_overrides(None, 0);
        assert_eq!(overrides.font_family.as_deref(), Some("NV Garamond, serif"));
        assert_eq!(overrides.sans_serif_font_family, None);
        preferences.custom_fonts_enabled = Some(true);
        assert_eq!(preferences.style_overrides(None, 0).sans_serif_font_family.as_deref(), Some("\"Noto Serif\", sans-serif"));
        let restored: ReaderPreferences = serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert!(restored.custom_fonts_active());
        assert_eq!(restored.custom_font_family(ReaderFontRole::SansSerif), Some("Noto Serif"));
    }

    #[test]
    fn system_font_names_are_quoted_without_changing_their_identity() {
        let preferences = ReaderPreferences { system_font_family: Some("A, B's \"Serif\" \\ Font".into()), ..Default::default() };
        assert_eq!(preferences.style_overrides(None, 0).serif_font_family.as_deref(), Some(r#""A, B's \"Serif\" \\ Font", serif"#));
    }

    #[test]
    fn all_three_custom_roles_are_independent_local_preferences() {
        let mut preferences = ReaderPreferences::default();
        preferences.set_custom_font_family(ReaderFontRole::Serif, "Noto Sans".into());
        preferences.set_custom_font_family(ReaderFontRole::SansSerif, "Noto Serif".into());
        preferences.set_custom_font_family(ReaderFontRole::Monospace, "Raleway".into());
        assert!(ReaderPreferences::default().synchronized_patches(&preferences).is_empty());
        let restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(&preferences).unwrap()).unwrap();
        let overrides = restored.style_overrides(None, 0);
        assert_eq!(overrides.font_family, None);
        assert_eq!(overrides.serif_font_family.as_deref(), Some("\"Noto Sans\", serif"));
        assert_eq!(overrides.sans_serif_font_family.as_deref(), Some("\"Noto Serif\", sans-serif"));
        assert_eq!(overrides.monospace_font_family.as_deref(), Some("\"Raleway\", monospace"));
    }

    #[gpui::test]
    fn persistence_receives_only_the_fields_changed_by_each_update(cx: &mut TestAppContext) {
        let saves = cx.new(|_| Vec::new());
        let target = saves.clone();
        cx.update(|cx| {
            cx.set_global(ReaderSettings {
                preferences: ReaderPreferences::default(),
                save: Rc::new(move |preferences, patches, cx| {
                    assert_eq!(ReaderSettings::preferences(cx), &preferences);
                    target.update(cx, |saves, _| saves.push((preferences, patches)));
                }),
            });
            assert!(!ReaderSettings::update(cx, |_| {}));
            assert!(ReaderSettings::update(cx, |preferences| preferences.font_size += 1.0));
            assert!(ReaderSettings::update(cx, |preferences| preferences.column_width += 10.0));
            assert!(ReaderSettings::update(cx, |preferences| preferences.chrome_hidden = true));
            assert!(!ReaderSettings::update(cx, |preferences| preferences.chrome_hidden = true));
        });
        saves.read_with(cx, |saves, _| {
            assert_eq!(saves.len(), 3);
            assert_eq!(saves[0].1, vec![ReaderPreferencePatch::FontSizePx(saves[0].0.font_size)]);
            assert_eq!(saves[1].1, vec![ReaderPreferencePatch::MaxLineWidthPx(saves[1].0.column_width)]);
            assert!(saves[2].0.chrome_hidden);
            assert!(saves[2].1.is_empty(), "local state must not generate synchronized patches");
        });
    }
}
