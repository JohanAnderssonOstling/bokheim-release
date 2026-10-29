use serde::de::Error as DeserializeError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Device-local shortcut overrides, with one combination per action.
/// Missing actions retain their defaults; an empty list disables the shortcut.
/// Lists remain in the saved format for compatibility with older profiles.
pub type KeyboardShortcuts = BTreeMap<String, Vec<String>>;

/// A relative adjustment to every original publication weight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderFontWeight {
    Lightest,
    Lighter,
    #[default]
    Normal,
    Heavier,
    Heaviest,
}

impl ReaderFontWeight {
    pub const ALL: [Self; 5] = [Self::Lightest, Self::Lighter, Self::Normal, Self::Heavier, Self::Heaviest];

    pub const fn adjustment(self) -> i16 {
        match self {
            Self::Lightest => -200,
            Self::Lighter => -100,
            Self::Normal => 0,
            Self::Heavier => 100,
            Self::Heaviest => 200,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Lightest => "Lightest",
            Self::Lighter => "Lighter",
            Self::Normal => "Normal",
            Self::Heavier => "Heavier",
            Self::Heaviest => "Heaviest",
        }
    }
}

/// Whether to preserve the publication's rules or override discretionary hyphenation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderHyphenation {
    #[default]
    Book,
    Off,
    On,
}

impl ReaderHyphenation {
    pub const ALL: [Self; 3] = [Self::Book, Self::Off, Self::On];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Book => "Book",
            Self::Off => "Off",
            Self::On => "On",
        }
    }
}

impl<'de> Deserialize<'de> for ReaderHyphenation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HyphenationVisitor;
        impl serde::de::Visitor<'_> for HyphenationVisitor {
            type Value = ReaderHyphenation;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("book, off, on, or a legacy hyphenation boolean")
            }

            fn visit_bool<E: DeserializeError>(self, enabled: bool) -> Result<Self::Value, E> {
                Ok(if enabled { ReaderHyphenation::On } else { ReaderHyphenation::Off })
            }

            fn visit_str<E: DeserializeError>(self, value: &str) -> Result<Self::Value, E> {
                match value {
                    "book" => Ok(ReaderHyphenation::Book),
                    "off" => Ok(ReaderHyphenation::Off),
                    "on" => Ok(ReaderHyphenation::On),
                    _ => Err(E::unknown_variant(value, &["book", "off", "on"])),
                }
            }
        }
        deserializer.deserialize_any(HyphenationVisitor)
    }
}

fn normalize_keyboard_shortcuts(mut shortcuts: KeyboardShortcuts) -> KeyboardShortcuts {
    for keys in shortcuts.values_mut() {
        keys.truncate(1);
    }
    shortcuts
}

fn deserialize_keyboard_shortcuts<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<KeyboardShortcuts, D::Error> {
    KeyboardShortcuts::deserialize(deserializer).map(normalize_keyboard_shortcuts)
}

pub const FONT_SIZE_MIN: f32 = 8.0;
pub const FONT_SIZE_MAX: f32 = 64.0;
pub const LINE_WIDTH_MIN: f64 = 200.0;
pub const LINE_WIDTH_MAX: f64 = 1200.0;
pub const LINE_WIDTH_CH_MIN: f64 = 20.0;
pub const LINE_WIDTH_CH_MAX: f64 = 100.0;
pub const ZOOM_MIN: f64 = 0.5;
pub const ZOOM_MAX: f64 = 3.0;
pub const LINE_HEIGHT_MIN: f32 = 1.0;
pub const LINE_HEIGHT_MAX: f32 = 2.4;
pub const DEFAULT_WORD_SPACING_PERCENT: f32 = 100.0;
pub const WORD_SPACING_MIN_PERCENT: f32 = 50.0;
pub const WORD_SPACING_MAX_PERCENT: f32 = 200.0;
pub const WORD_SPACING_STEP_PERCENT: f32 = 10.0;
pub const DEFAULT_PARAGRAPH_SPACING_EM: f32 = 1.0;
pub const DEFAULT_FIRST_LINE_INDENT_EM: f32 = 1.5;
pub const PARAGRAPH_SPACING_MAX_EM: f32 = 3.0;
pub const FIRST_LINE_INDENT_MAX_EM: f32 = 4.0;
pub const PARAGRAPH_STEP_EM: f32 = 0.1;
pub const PDF_ZOOM_MIN: f32 = 0.05;
pub const PDF_ZOOM_MAX: f32 = 10.0;
pub const PDF_PAGE_COUNT_MIN: u8 = 1;
pub const PDF_PAGE_COUNT_MAX: u8 = 6;
pub const COLUMN_COUNT_MIN: u8 = 1;
pub const COLUMN_COUNT_MAX: u8 = 4;
pub const READER_SPACING_MIN: f32 = 0.0;
pub const READER_SPACING_MAX: f32 = 200.0;
/// The slowest and fastest narration the time-stretcher is asked for. Beyond
/// this range speech stops being speech.
pub const AUDIOBOOK_SPEED_MIN: f64 = 0.5;
pub const AUDIOBOOK_SPEED_MAX: f64 = 3.0;
pub const UI_FONT_SIZE_MIN: u8 = 12;
pub const UI_FONT_SIZE_MAX: u8 = 32;
pub const UI_FONT_SIZE_DEFAULT: u8 = 16;

/// Two light themes with distinct tints, plus a shared dark palette.
///
/// They were three — `GrecoFuturistic`, `LibrarySerif` and `ClassicWhite`,
/// labelled Sage, Sepia and Paper — and the names described neither what they
/// were nor what distinguished them: Sage was documented as "cool" while its
/// own neutrals sat at hue 87, a cream. What actually varies between them is
/// the hue of the neutral surfaces and of the accent in light appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationTheme {
    /// Cream surfaces, green accent.
    #[default]
    Warm,
    /// Neutral greys and true-white cards, muted slate accent. Not a tinted paper —
    /// the absence of a tint, for people who want the application to disappear
    /// behind the covers.
    Cool,
}

/// Which of a theme's two palettes to draw. Existing profiles follow the
/// platform until a specific palette is selected in settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedAppearance {
    #[default]
    Light,
    Dark,
}

impl ApplicationTheme {
    /// Every theme, in the order they are offered.
    pub const ALL: [Self; 2] = [Self::Warm, Self::Cool];

    pub const fn next(self) -> Self {
        match self {
            Self::Warm => Self::Cool,
            Self::Cool => Self::Warm,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Warm => "Warm",
            Self::Cool => "Cool",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Warm => "Cream surfaces with a muted green accent.",
            Self::Cool => "Neutral greys and white cards with a muted slate accent.",
        }
    }
}

/// The interface face, independent of the book's reading font.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiFontFamily {
    #[default]
    Serif,
    SansSerif,
}

impl UiFontFamily {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Serif => "Serif",
            Self::SansSerif => "Sans serif",
        }
    }

    pub const fn family_name(self) -> &'static str {
        match self {
            Self::Serif => "NV Garamond",
            Self::SansSerif => "Raleway",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsingPreferences {
    #[serde(default, deserialize_with = "deserialize_keyboard_shortcuts")]
    keyboard_shortcuts: KeyboardShortcuts,
    #[serde(default)]
    allow_metered_transfers: bool,
    theme: ApplicationTheme,
    #[serde(default)]
    appearance_override: Option<ResolvedAppearance>,
    ui_font_size_px: UiFontSize,
    #[serde(default)]
    ui_font_family: UiFontFamily,
    #[serde(default)]
    custom_ui_font_family: Option<String>,
    #[serde(default)]
    custom_ui_font_enabled: Option<bool>,
    cover_text: CoverText,
    /// Draw every book with its generated cover and never load cover art.
    /// Offered on Kobo, where decoding and holding cover images is the
    /// costliest part of a library page.
    #[serde(default)]
    generated_covers_only: bool,
    #[serde(default)]
    catalog_grouping: CatalogGrouping,
    /// None needs an initial default; Some(empty) explicitly shows any language.
    #[serde(default)]
    catalog_languages: Option<Vec<String>>,
    /// Current Kobo keyboard layout; legacy lists use the first known code.
    #[serde(default = "default_keyboard_layouts")]
    keyboard_layouts: Vec<String>,
}

fn default_keyboard_layouts() -> Vec<String> {
    vec!["SV".into()]
}

impl Default for BrowsingPreferences {
    fn default() -> Self {
        Self {
            keyboard_shortcuts: KeyboardShortcuts::new(),
            allow_metered_transfers: false,
            theme: ApplicationTheme::default(),
            appearance_override: None,
            ui_font_size_px: UiFontSize(UI_FONT_SIZE_DEFAULT),
            ui_font_family: UiFontFamily::default(),
            custom_ui_font_family: None,
            custom_ui_font_enabled: None,
            cover_text: CoverText::Always,
            generated_covers_only: false,
            catalog_grouping: CatalogGrouping::default(),
            catalog_languages: None,
            keyboard_layouts: default_keyboard_layouts(),
        }
    }
}

impl BrowsingPreferences {
    pub fn keyboard_shortcuts(&self) -> &KeyboardShortcuts {
        &self.keyboard_shortcuts
    }
    pub fn allow_metered_transfers(&self) -> bool {
        self.allow_metered_transfers
    }
    pub fn theme(&self) -> ApplicationTheme {
        self.theme
    }

    pub fn appearance_override(&self) -> Option<ResolvedAppearance> {
        self.appearance_override
    }

    pub fn ui_font_size_px(&self) -> u8 {
        self.ui_font_size_px.get()
    }
    pub fn ui_font_family(&self) -> UiFontFamily {
        self.ui_font_family
    }
    pub fn custom_ui_font_family(&self) -> Option<&str> {
        self.custom_ui_font_family.as_deref().filter(|family| !family.trim().is_empty())
    }
    pub fn custom_ui_font_active(&self) -> bool {
        self.custom_ui_font_family().is_some() && self.custom_ui_font_enabled.unwrap_or(true)
    }
    pub fn ui_font_name(&self) -> &str {
        if self.custom_ui_font_active() {
            self.custom_ui_font_family().unwrap()
        } else {
            self.ui_font_family.family_name()
        }
    }
    pub fn cover_text(&self) -> CoverText {
        self.cover_text
    }

    pub fn generated_covers_only(&self) -> bool {
        self.generated_covers_only
    }

    pub fn catalog_grouping(&self) -> CatalogGrouping {
        self.catalog_grouping
    }

    pub fn catalog_languages(&self) -> Option<&[String]> {
        self.catalog_languages.as_deref()
    }

    pub fn keyboard_layouts(&self) -> &[String] {
        &self.keyboard_layouts
    }
}

/// How the catalog page groups its books: by subject or by author.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogGrouping {
    #[default]
    Subject,
    Author,
}

impl CatalogGrouping {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Subject => "Subject",
            Self::Author => "Author",
        }
    }
}

/// Whether paragraph indentation follows the book, is disabled, or uses a saved value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderFirstLineIndent {
    #[default]
    Book,
    Off,
    Custom,
}

impl ReaderFirstLineIndent {
    pub const ALL: [Self; 3] = [Self::Book, Self::Off, Self::Custom];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Book => "Book",
            Self::Off => "Off",
            Self::Custom => "Custom",
        }
    }
}

impl<'de> Deserialize<'de> for ReaderFirstLineIndent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Mode { Book, Off, Custom }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Saved { Mode(Mode), Legacy(bool) }
        Ok(match Saved::deserialize(deserializer)? {
            Saved::Mode(Mode::Book) | Saved::Legacy(false) => Self::Book,
            Saved::Mode(Mode::Off) => Self::Off,
            Saved::Mode(Mode::Custom) | Saved::Legacy(true) => Self::Custom,
        })
    }
}

/// Whether a compact book card names its title under the cover, or leaves
/// that to the cover itself.
///
/// `CoverOnly` still needs every book to have something to read — a book with
/// no cover art gets a generated one with its title set on it, rather than
/// the plain placeholder, which under this setting would otherwise be the
/// only anonymous tile in the grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverText {
    #[default]
    Always,
    CoverOnly,
}

impl CoverText {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Always => "Always",
            Self::CoverOnly => "Cover only",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Always => "Title and subtitle under every cover.",
            Self::CoverOnly => "Covers carry their own title; a book with no cover art gets a generated one.",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "preference", content = "value", rename_all = "snake_case")]
pub enum BrowsingPreferencePatch {
    KeyboardShortcuts(KeyboardShortcuts),
    AllowMeteredTransfers(bool),
    Theme(ApplicationTheme),
    Palette(ApplicationTheme, ResolvedAppearance),
    UiFontSizePx(u8),
    UiFontFamily(UiFontFamily),
    CustomUiFontFamily(String),
    CoverText(CoverText),
    GeneratedCoversOnly(bool),
    CatalogGrouping(CatalogGrouping),
    CatalogLanguages(Option<Vec<String>>),
    KeyboardLayouts(Vec<String>),
}

impl BrowsingPreferencePatch {
    pub fn apply(self, preferences: &mut BrowsingPreferences) -> Result<(), PreferenceValueError> {
        match self {
            Self::KeyboardShortcuts(value) => preferences.keyboard_shortcuts = normalize_keyboard_shortcuts(value),
            Self::AllowMeteredTransfers(value) => preferences.allow_metered_transfers = value,
            Self::Theme(value) => preferences.theme = value,
            Self::Palette(theme, appearance) => {
                preferences.theme = theme;
                preferences.appearance_override = Some(appearance);
            }
            Self::UiFontSizePx(value) => preferences.ui_font_size_px = UiFontSize::new(value)?,
            Self::UiFontFamily(value) => {
                preferences.ui_font_family = value;
                preferences.custom_ui_font_enabled = Some(false);
            }
            Self::CustomUiFontFamily(value) => {
                preferences.custom_ui_font_family = Some(value);
                preferences.custom_ui_font_enabled = Some(true);
            }
            Self::CoverText(value) => preferences.cover_text = value,
            Self::GeneratedCoversOnly(value) => preferences.generated_covers_only = value,
            Self::CatalogGrouping(value) => preferences.catalog_grouping = value,
            Self::CatalogLanguages(value) => preferences.catalog_languages = value,
            Self::KeyboardLayouts(value) => preferences.keyboard_layouts = value,
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderFlow {
    #[default]
    Paginated,
    Scrolled,
}

/// How a PDF page's on-screen size is decided.
///
/// Every mode settles the same quantity — the zoom, in pixels per PDF point —
/// and they differ only in which end is held fixed. `pdf_page_count` carries
/// the count for [`PdfZoomMode::FitWidth`] and `pdf_zoom` the zoom for
/// [`PdfZoomMode::Fixed`], so switching modes and back restores what was set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfZoomMode {
    /// The page count is chosen and the zoom follows.
    #[default]
    FitWidth,
    /// The zoom is chosen and the page count follows.
    Fixed,
    /// A page's height fills the viewport, and the page count follows.
    FitHeight,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderFontFamily {
    #[default]
    Publisher,
    Serif,
    SansSerif,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderTextAlignment {
    #[default]
    Publisher,
    Start,
    Justify,
}

/// Complete reader settings consumed by every frontend. Account-wide fields
/// are synchronized separately; layout-sensitive fields remain in a local
/// device or web form-factor profile. Neither belongs in the library event log.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReaderPreferences {
    font_size_px: FontSize,
    max_line_width_px: LineWidth,
    #[serde(default = "default_min_line_width_ch")]
    min_line_width_ch: LineWidthCh,
    #[serde(default = "default_max_line_width_ch")]
    max_line_width_ch: LineWidthCh,
    zoom: Zoom,
    line_height: LineHeight,
    /// Keep the last custom value when publisher line spacing is selected.
    #[serde(default = "default_override_line_height")]
    override_line_height: bool,
    #[serde(default = "default_paragraph_spacing")]
    paragraph_spacing_em: ParagraphSpacing,
    #[serde(default)]
    override_paragraph_spacing: bool,
    #[serde(default = "default_first_line_indent")]
    first_line_indent_em: FirstLineIndent,
    #[serde(default, alias = "override_first_line_indent")]
    first_line_indent: ReaderFirstLineIndent,
    #[serde(default)]
    font_weight: ReaderFontWeight,
    #[serde(default = "default_word_spacing_percent")]
    word_spacing_percent: WordSpacingPercent,
    flow: ReaderFlow,
    font_family: ReaderFontFamily,
    text_alignment: ReaderTextAlignment,
    #[serde(default)]
    hyphenation: ReaderHyphenation,
    max_column_count: ColumnCount,
    #[serde(default = "default_reader_margin")]
    horizontal_margin_px: ReaderSpacing,
    #[serde(default = "default_reader_margin")]
    vertical_margin_px: ReaderSpacing,
    #[serde(default = "default_column_gap")]
    min_column_gap_px: ReaderSpacing,
    /// Fixed-page spacing is independent of the HTML reader and other formats.
    #[serde(default = "default_fixed_page_spacing")]
    pdf_horizontal_margin_px: ReaderSpacing,
    #[serde(default = "default_fixed_page_spacing")]
    pdf_vertical_margin_px: ReaderSpacing,
    #[serde(default = "default_fixed_page_spacing")]
    pdf_page_gap_px: ReaderSpacing,
    #[serde(default = "default_fixed_page_spacing")]
    comic_horizontal_margin_px: ReaderSpacing,
    #[serde(default = "default_fixed_page_spacing")]
    comic_vertical_margin_px: ReaderSpacing,
    #[serde(default = "default_fixed_page_spacing")]
    comic_page_gap_px: ReaderSpacing,
    /// Which end of the PDF layout is held fixed; the rest is derived.
    pdf_zoom_mode: PdfZoomMode,
    /// How many PDF pages sit side by side under [`PdfZoomMode::FitWidth`].
    pdf_page_count: PdfPageCount,
    /// Pixels per PDF point under [`PdfZoomMode::Fixed`].
    pdf_zoom: PdfZoom,
    /// Automatically removes uniform outer margins from rendered PDF pages.
    pdf_trim_margins: bool,
    /// How fast narration is played back. Defaulted rather than required, so a
    /// preferences file written before audiobooks still loads.
    #[serde(default = "default_audiobook_speed")]
    audiobook_speed: AudiobookSpeed,
}

const fn default_fixed_page_spacing() -> ReaderSpacing {
    ReaderSpacing(0.0)
}

const fn default_audiobook_speed() -> AudiobookSpeed {
    AudiobookSpeed(1.0)
}

const fn default_paragraph_spacing() -> ParagraphSpacing {
    ParagraphSpacing(DEFAULT_PARAGRAPH_SPACING_EM)
}

const fn default_first_line_indent() -> FirstLineIndent {
    FirstLineIndent(DEFAULT_FIRST_LINE_INDENT_EM)
}

const fn default_word_spacing_percent() -> WordSpacingPercent {
    WordSpacingPercent(DEFAULT_WORD_SPACING_PERCENT)
}

const fn default_override_line_height() -> bool {
    true
}

const fn default_min_line_width_ch() -> LineWidthCh {
    LineWidthCh(45.0)
}

const fn default_max_line_width_ch() -> LineWidthCh {
    LineWidthCh(65.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreferenceValueError(&'static str);

impl fmt::Display for PreferenceValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} is outside its supported finite range", self.0)
    }
}

impl std::error::Error for PreferenceValueError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct UiFontSize(u8);

impl UiFontSize {
    fn new(value: u8) -> Result<Self, PreferenceValueError> {
        if (UI_FONT_SIZE_MIN..=UI_FONT_SIZE_MAX).contains(&value) { Ok(Self(value)) } else { Err(PreferenceValueError("UI font size")) }
    }

    const fn get(self) -> u8 {
        self.0
    }
}

impl<'de> Deserialize<'de> for UiFontSize {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

macro_rules! bounded_float {
    ($name:ident, $primitive:ty, $label:literal, $minimum:expr, $maximum:expr) => {
        #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
        #[serde(transparent)]
        struct $name($primitive);

        impl $name {
            fn new(value: $primitive) -> Result<Self, PreferenceValueError> {
                if value.is_finite() && ($minimum..=$maximum).contains(&value) { Ok(Self(value)) } else { Err(PreferenceValueError($label)) }
            }

            const fn get(self) -> $primitive {
                self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(<$primitive>::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

bounded_float!(FontSize, f32, "font size", FONT_SIZE_MIN, FONT_SIZE_MAX);
bounded_float!(LineWidth, f64, "line width", LINE_WIDTH_MIN, LINE_WIDTH_MAX);
bounded_float!(LineWidthCh, f64, "line width in ch", LINE_WIDTH_CH_MIN, LINE_WIDTH_CH_MAX);
bounded_float!(Zoom, f64, "zoom", ZOOM_MIN, ZOOM_MAX);
bounded_float!(LineHeight, f32, "line height", LINE_HEIGHT_MIN, LINE_HEIGHT_MAX);
bounded_float!(WordSpacingPercent, f32, "word spacing", WORD_SPACING_MIN_PERCENT, WORD_SPACING_MAX_PERCENT);
bounded_float!(ParagraphSpacing, f32, "paragraph spacing", 0.0, PARAGRAPH_SPACING_MAX_EM);
bounded_float!(FirstLineIndent, f32, "first-line indentation", 0.0, FIRST_LINE_INDENT_MAX_EM);
bounded_float!(PdfZoom, f32, "PDF zoom", PDF_ZOOM_MIN, PDF_ZOOM_MAX);
bounded_float!(AudiobookSpeed, f64, "audiobook speed", AUDIOBOOK_SPEED_MIN, AUDIOBOOK_SPEED_MAX);
bounded_float!(ReaderSpacing, f32, "reader spacing", READER_SPACING_MIN, READER_SPACING_MAX);

/// Default space between ebook text and each edge of the reading surface.
pub const DEFAULT_READER_MARGIN_PX: f32 = 24.0;

const fn default_reader_margin() -> ReaderSpacing {
    ReaderSpacing(DEFAULT_READER_MARGIN_PX)
}

/// Default minimum space between columns of ebook text.
pub const DEFAULT_COLUMN_GAP_PX: f32 = 32.0;

const fn default_column_gap() -> ReaderSpacing {
    ReaderSpacing(DEFAULT_COLUMN_GAP_PX)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct ColumnCount(u8);

impl ColumnCount {
    fn new(value: u8) -> Result<Self, PreferenceValueError> {
        // Only a floor: how many columns fit is a question about the width
        // available, which no contract here is in a position to answer.
        if value >= COLUMN_COUNT_MIN { Ok(Self(value)) } else { Err(PreferenceValueError("column count")) }
    }

    const fn get(self) -> u8 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ColumnCount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct PdfPageCount(u8);

impl PdfPageCount {
    fn new(value: u8) -> Result<Self, PreferenceValueError> {
        if (PDF_PAGE_COUNT_MIN..=PDF_PAGE_COUNT_MAX).contains(&value) { Ok(Self(value)) } else { Err(PreferenceValueError("PDF page count")) }
    }

    const fn get(self) -> u8 {
        self.0
    }
}

impl<'de> Deserialize<'de> for PdfPageCount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl Default for ReaderPreferences {
    fn default() -> Self {
        Self {
            font_size_px: FontSize(20.0),
            max_line_width_px: LineWidth(600.0),
            min_line_width_ch: default_min_line_width_ch(),
            max_line_width_ch: default_max_line_width_ch(),
            zoom: Zoom(1.0),
            line_height: LineHeight(1.5),
            override_line_height: default_override_line_height(),
            paragraph_spacing_em: default_paragraph_spacing(),
            override_paragraph_spacing: false,
            first_line_indent_em: default_first_line_indent(),
            first_line_indent: ReaderFirstLineIndent::Book,
            font_weight: ReaderFontWeight::Normal,
            word_spacing_percent: default_word_spacing_percent(),
            flow: ReaderFlow::Paginated,
            font_family: ReaderFontFamily::Publisher,
            text_alignment: ReaderTextAlignment::Publisher,
            hyphenation: ReaderHyphenation::Book,
            max_column_count: ColumnCount(2),
            horizontal_margin_px: default_reader_margin(),
            vertical_margin_px: default_reader_margin(),
            min_column_gap_px: default_column_gap(),
            pdf_horizontal_margin_px: default_fixed_page_spacing(),
            pdf_vertical_margin_px: default_fixed_page_spacing(),
            pdf_page_gap_px: default_fixed_page_spacing(),
            comic_horizontal_margin_px: default_fixed_page_spacing(),
            comic_vertical_margin_px: default_fixed_page_spacing(),
            comic_page_gap_px: default_fixed_page_spacing(),
            pdf_zoom_mode: PdfZoomMode::FitWidth,
            pdf_page_count: PdfPageCount(1),
            pdf_zoom: PdfZoom(1.0),
            pdf_trim_margins: true,
            audiobook_speed: default_audiobook_speed(),
        }
    }
}

impl ReaderPreferences {
    pub fn paragraph_spacing_em(self) -> f32 { self.paragraph_spacing_em.get() }
    pub fn override_paragraph_spacing(self) -> bool { self.override_paragraph_spacing }
    pub fn first_line_indent_em(self) -> f32 { self.first_line_indent_em.get() }
    pub fn first_line_indent(self) -> ReaderFirstLineIndent { self.first_line_indent }

    pub fn set_paragraph_spacing_em(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.paragraph_spacing_em = ParagraphSpacing::new(value)?;
        Ok(())
    }
    pub fn set_override_paragraph_spacing(&mut self, value: bool) { self.override_paragraph_spacing = value; }
    pub fn set_first_line_indent_em(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.first_line_indent_em = FirstLineIndent::new(value)?;
        Ok(())
    }
    pub fn set_first_line_indent(&mut self, value: ReaderFirstLineIndent) { self.first_line_indent = value; }

    pub fn word_spacing_percent(self) -> f32 {
        self.word_spacing_percent.get()
    }

    pub fn set_word_spacing_percent(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.word_spacing_percent = WordSpacingPercent::new(value)?;
        Ok(())
    }

    pub fn font_weight(&self) -> ReaderFontWeight {
        self.font_weight
    }

    pub fn set_font_weight(&mut self, weight: ReaderFontWeight) {
        self.font_weight = weight;
    }

    pub fn font_size_px(self) -> f32 {
        self.font_size_px.get()
    }
    pub fn max_line_width_px(self) -> f64 {
        self.max_line_width_px.get()
    }
    pub fn min_line_width_ch(self) -> f64 {
        self.min_line_width_ch.get()
    }
    pub fn max_line_width_ch(self) -> f64 {
        self.max_line_width_ch.get()
    }
    pub fn zoom(self) -> f64 {
        self.zoom.get()
    }
    pub fn line_height(self) -> f32 {
        self.line_height.get()
    }
    pub fn override_line_height(&self) -> bool {
        self.override_line_height
    }
    pub fn flow(self) -> ReaderFlow {
        self.flow
    }
    pub fn font_family(self) -> ReaderFontFamily {
        self.font_family
    }
    pub fn text_alignment(self) -> ReaderTextAlignment {
        self.text_alignment
    }
    pub fn hyphenation(self) -> ReaderHyphenation {
        self.hyphenation
    }
    pub fn max_column_count(self) -> u8 {
        self.max_column_count.get()
    }
    pub fn horizontal_margin_px(self) -> f32 {
        self.horizontal_margin_px.get()
    }
    pub fn vertical_margin_px(self) -> f32 {
        self.vertical_margin_px.get()
    }
    pub fn min_column_gap_px(self) -> f32 {
        self.min_column_gap_px.get()
    }
    pub fn pdf_horizontal_margin_px(self) -> f32 {
        self.pdf_horizontal_margin_px.get()
    }
    pub fn pdf_vertical_margin_px(self) -> f32 {
        self.pdf_vertical_margin_px.get()
    }
    pub fn pdf_page_gap_px(self) -> f32 {
        self.pdf_page_gap_px.get()
    }
    pub fn comic_horizontal_margin_px(self) -> f32 {
        self.comic_horizontal_margin_px.get()
    }
    pub fn comic_vertical_margin_px(self) -> f32 {
        self.comic_vertical_margin_px.get()
    }
    pub fn comic_page_gap_px(self) -> f32 {
        self.comic_page_gap_px.get()
    }
    pub fn pdf_zoom_mode(self) -> PdfZoomMode {
        self.pdf_zoom_mode
    }
    pub fn pdf_page_count(self) -> u8 {
        self.pdf_page_count.get()
    }
    pub fn pdf_zoom(self) -> f32 {
        self.pdf_zoom.get()
    }
    pub fn audiobook_speed(self) -> f64 {
        self.audiobook_speed.get()
    }
    pub fn pdf_trim_margins(self) -> bool {
        self.pdf_trim_margins
    }

    pub fn set_font_size_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.font_size_px = FontSize::new(value)?;
        Ok(())
    }
    pub fn set_max_line_width_px(&mut self, value: f64) -> Result<(), PreferenceValueError> {
        self.max_line_width_px = LineWidth::new(value)?;
        Ok(())
    }
    pub fn set_min_line_width_ch(&mut self, value: f64) -> Result<(), PreferenceValueError> {
        let value = LineWidthCh::new(value)?;
        // Treat the endpoints as a movable range: honour the edited endpoint
        // and bring the other one along if it would otherwise cross it.
        if value.get() > self.max_line_width_ch.get() {
            self.max_line_width_ch = value;
        }
        self.min_line_width_ch = value;
        Ok(())
    }
    pub fn set_max_line_width_ch(&mut self, value: f64) -> Result<(), PreferenceValueError> {
        let value = LineWidthCh::new(value)?;
        // See `set_min_line_width_ch`: setting the maximum below the minimum
        // moves the minimum instead of rejecting or clamping the request.
        if value.get() < self.min_line_width_ch.get() {
            self.min_line_width_ch = value;
        }
        self.max_line_width_ch = value;
        Ok(())
    }
    pub fn set_zoom(&mut self, value: f64) -> Result<(), PreferenceValueError> {
        self.zoom = Zoom::new(value)?;
        Ok(())
    }
    pub fn set_line_height(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.line_height = LineHeight::new(value)?;
        Ok(())
    }
    pub fn set_override_line_height(&mut self, value: bool) {
        self.override_line_height = value;
    }
    pub fn set_flow(&mut self, value: ReaderFlow) {
        self.flow = value;
    }
    pub fn set_font_family(&mut self, value: ReaderFontFamily) {
        self.font_family = value;
    }
    pub fn set_text_alignment(&mut self, value: ReaderTextAlignment) {
        self.text_alignment = value;
    }
    pub fn set_hyphenation(&mut self, value: ReaderHyphenation) {
        self.hyphenation = value;
    }
    pub fn set_max_column_count(&mut self, value: u8) -> Result<(), PreferenceValueError> {
        self.max_column_count = ColumnCount::new(value)?;
        Ok(())
    }
    pub fn set_horizontal_margin_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.horizontal_margin_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_vertical_margin_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.vertical_margin_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_min_column_gap_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.min_column_gap_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_pdf_horizontal_margin_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.pdf_horizontal_margin_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_pdf_vertical_margin_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.pdf_vertical_margin_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_pdf_page_gap_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.pdf_page_gap_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_comic_horizontal_margin_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.comic_horizontal_margin_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_comic_vertical_margin_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.comic_vertical_margin_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_comic_page_gap_px(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.comic_page_gap_px = ReaderSpacing::new(value)?;
        Ok(())
    }
    pub fn set_pdf_zoom_mode(&mut self, value: PdfZoomMode) {
        self.pdf_zoom_mode = value;
    }
    pub fn set_pdf_page_count(&mut self, value: u8) -> Result<(), PreferenceValueError> {
        self.pdf_page_count = PdfPageCount::new(value)?;
        Ok(())
    }
    pub fn set_pdf_zoom(&mut self, value: f32) -> Result<(), PreferenceValueError> {
        self.pdf_zoom = PdfZoom::new(value)?;
        Ok(())
    }
    pub fn set_pdf_trim_margins(&mut self, value: bool) {
        self.pdf_trim_margins = value;
    }

    pub fn set_audiobook_speed(&mut self, value: f64) -> Result<(), PreferenceValueError> {
        self.audiobook_speed = AudiobookSpeed::new(value)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "preference", content = "value", rename_all = "snake_case")]
pub enum ReaderPreferencePatch {
    FontSizePx(f32),
    MaxLineWidthPx(f64),
    MinLineWidthCh(f64),
    MaxLineWidthCh(f64),
    Zoom(f64),
    LineHeight(f32),
    Flow(ReaderFlow),
    FontFamily(ReaderFontFamily),
    TextAlignment(ReaderTextAlignment),
    Hyphenation(ReaderHyphenation),
    MaxColumnCount(u8),
    HorizontalMarginPx(f32),
    VerticalMarginPx(f32),
    MinColumnGapPx(f32),
    PdfHorizontalMarginPx(f32),
    PdfVerticalMarginPx(f32),
    PdfPageGapPx(f32),
    ComicHorizontalMarginPx(f32),
    ComicVerticalMarginPx(f32),
    ComicPageGapPx(f32),
    PdfZoomMode(PdfZoomMode),
    PdfPageCount(u8),
    PdfZoom(f32),
    PdfTrimMargins(bool),
    AudiobookSpeed(f64),
    OverrideLineHeight(bool),
    ParagraphSpacingEm(f32),
    OverrideParagraphSpacing(bool),
    FirstLineIndentEm(f32),
    #[serde(alias = "override_first_line_indent")]
    FirstLineIndent(ReaderFirstLineIndent),
    FontWeight(ReaderFontWeight),
    WordSpacingPercent(f32),
}

impl ReaderPreferencePatch {
    pub fn apply(self, preferences: &mut ReaderPreferences) -> Result<(), PreferenceValueError> {
        match self {
            Self::FontSizePx(value) => preferences.set_font_size_px(value),
            Self::MaxLineWidthPx(value) => preferences.set_max_line_width_px(value),
            Self::MinLineWidthCh(value) => preferences.set_min_line_width_ch(value),
            Self::MaxLineWidthCh(value) => preferences.set_max_line_width_ch(value),
            Self::Zoom(value) => preferences.set_zoom(value),
            Self::LineHeight(value) => preferences.set_line_height(value),
            Self::OverrideLineHeight(value) => {
                preferences.set_override_line_height(value);
                Ok(())
            }
            Self::ParagraphSpacingEm(value) => preferences.set_paragraph_spacing_em(value),
            Self::FirstLineIndentEm(value) => preferences.set_first_line_indent_em(value),
            Self::OverrideParagraphSpacing(value) => {
                preferences.set_override_paragraph_spacing(value);
                Ok(())
            }
            Self::FirstLineIndent(value) => {
                preferences.set_first_line_indent(value);
                Ok(())
            }
            Self::WordSpacingPercent(value) => preferences.set_word_spacing_percent(value),
            Self::FontWeight(weight) => {
                preferences.set_font_weight(weight);
                Ok(())
            }
            Self::Flow(value) => {
                preferences.set_flow(value);
                Ok(())
            }
            Self::FontFamily(value) => {
                preferences.set_font_family(value);
                Ok(())
            }
            Self::TextAlignment(value) => {
                preferences.set_text_alignment(value);
                Ok(())
            }
            Self::Hyphenation(value) => {
                preferences.set_hyphenation(value);
                Ok(())
            }
            Self::MaxColumnCount(value) => preferences.set_max_column_count(value),
            Self::HorizontalMarginPx(value) => preferences.set_horizontal_margin_px(value),
            Self::VerticalMarginPx(value) => preferences.set_vertical_margin_px(value),
            Self::MinColumnGapPx(value) => preferences.set_min_column_gap_px(value),
            Self::PdfHorizontalMarginPx(value) => preferences.set_pdf_horizontal_margin_px(value),
            Self::PdfVerticalMarginPx(value) => preferences.set_pdf_vertical_margin_px(value),
            Self::PdfPageGapPx(value) => preferences.set_pdf_page_gap_px(value),
            Self::ComicHorizontalMarginPx(value) => preferences.set_comic_horizontal_margin_px(value),
            Self::ComicVerticalMarginPx(value) => preferences.set_comic_vertical_margin_px(value),
            Self::ComicPageGapPx(value) => preferences.set_comic_page_gap_px(value),
            Self::PdfZoomMode(value) => {
                preferences.set_pdf_zoom_mode(value);
                Ok(())
            }
            Self::PdfPageCount(value) => preferences.set_pdf_page_count(value),
            Self::PdfZoom(value) => preferences.set_pdf_zoom(value),
            Self::PdfTrimMargins(value) => {
                preferences.set_pdf_trim_margins(value);
                Ok(())
            }
            Self::AudiobookSpeed(value) => preferences.set_audiobook_speed(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BrowsingPreferencePatch, BrowsingPreferences, CoverText, DEFAULT_READER_MARGIN_PX, ReaderPreferencePatch, ReaderPreferences, UiFontFamily};

    #[test]
    fn fixed_page_spacing_is_independent_persisted_and_defaults_for_older_settings() {
        let original = ReaderPreferences::default();
        let patches = [
            ReaderPreferencePatch::PdfHorizontalMarginPx(8.0),
            ReaderPreferencePatch::PdfVerticalMarginPx(16.0),
            ReaderPreferencePatch::PdfPageGapPx(24.0),
            ReaderPreferencePatch::ComicHorizontalMarginPx(32.0),
            ReaderPreferencePatch::ComicVerticalMarginPx(40.0),
            ReaderPreferencePatch::ComicPageGapPx(48.0),
        ];
        let mut preferences = original;
        for patch in patches {
            let patch = serde_json::from_str::<ReaderPreferencePatch>(&serde_json::to_string(&patch).unwrap()).unwrap();
            patch.apply(&mut preferences).unwrap();
        }
        let saved = serde_json::to_value(preferences).unwrap();
        let restored: ReaderPreferences = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(restored, preferences);
        assert_eq!((restored.pdf_horizontal_margin_px(), restored.pdf_vertical_margin_px(), restored.pdf_page_gap_px()), (8.0, 16.0, 24.0));
        assert_eq!((restored.comic_horizontal_margin_px(), restored.comic_vertical_margin_px(), restored.comic_page_gap_px()), (32.0, 40.0, 48.0));
        assert_eq!(restored.horizontal_margin_px(), original.horizontal_margin_px());
        assert_eq!(restored.vertical_margin_px(), original.vertical_margin_px());
        assert_eq!(restored.min_column_gap_px(), original.min_column_gap_px());
        let mut legacy = saved;
        for field in ["pdf_horizontal_margin_px", "pdf_vertical_margin_px", "pdf_page_gap_px", "comic_horizontal_margin_px", "comic_vertical_margin_px", "comic_page_gap_px"] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        assert_eq!(serde_json::from_value::<ReaderPreferences>(legacy).unwrap(), original);
        assert!(ReaderPreferencePatch::PdfPageGapPx(f32::NAN).apply(&mut preferences).is_err());
        assert!(ReaderPreferencePatch::ComicVerticalMarginPx(201.0).apply(&mut preferences).is_err());
    }

    #[test]
    fn first_line_indent_modes_preserve_custom_value_and_migrate_legacy_overrides() {
        use super::ReaderFirstLineIndent;
        let mut preferences = ReaderPreferences::default();
        preferences.set_first_line_indent_em(2.1).unwrap();
        for mode in ReaderFirstLineIndent::ALL {
            let patch = ReaderPreferencePatch::FirstLineIndent(mode);
            let patch: ReaderPreferencePatch = serde_json::from_value(serde_json::to_value(patch).unwrap()).unwrap();
            patch.apply(&mut preferences).unwrap();
            let restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
            assert_eq!(restored.first_line_indent(), mode);
            assert_eq!(restored.first_line_indent_em(), 2.1);
        }
        for (enabled, expected) in [(false, ReaderFirstLineIndent::Book), (true, ReaderFirstLineIndent::Custom)] {
            let mut legacy = serde_json::to_value(preferences).unwrap();
            legacy.as_object_mut().unwrap().remove("first_line_indent");
            legacy["override_first_line_indent"] = serde_json::json!(enabled);
            let restored: ReaderPreferences = serde_json::from_value(legacy).unwrap();
            assert_eq!(restored.first_line_indent(), expected);
            assert_eq!(restored.first_line_indent_em(), 2.1);
            let patch: ReaderPreferencePatch = serde_json::from_value(serde_json::json!({ "preference": "override_first_line_indent", "value": enabled })).unwrap();
            patch.apply(&mut preferences).unwrap();
            assert_eq!(preferences.first_line_indent(), expected);
        }
        let mut invalid = serde_json::to_value(preferences).unwrap();
        invalid["first_line_indent"] = serde_json::json!("invalid");
        assert!(serde_json::from_value::<ReaderPreferences>(invalid).is_err());
    }

    #[test]
    fn paragraph_controls_default_to_book_and_round_trip_custom_values() {
        let mut legacy = serde_json::to_value(ReaderPreferences::default()).unwrap();
        for field in ["paragraph_spacing_em", "override_paragraph_spacing", "first_line_indent_em", "first_line_indent"] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        let mut preferences: ReaderPreferences = serde_json::from_value(legacy).unwrap();
        assert!(!preferences.override_paragraph_spacing());
        assert_eq!(preferences.first_line_indent(), super::ReaderFirstLineIndent::Book);
        assert_eq!(preferences.paragraph_spacing_em(), 1.0);
        assert_eq!(preferences.first_line_indent_em(), 1.5);
        for patch in [ReaderPreferencePatch::ParagraphSpacingEm(0.7), ReaderPreferencePatch::FirstLineIndentEm(2.1), ReaderPreferencePatch::OverrideParagraphSpacing(true), ReaderPreferencePatch::FirstLineIndent(super::ReaderFirstLineIndent::Custom)] {
            let decoded: ReaderPreferencePatch = serde_json::from_value(serde_json::to_value(patch).unwrap()).unwrap();
            decoded.apply(&mut preferences).unwrap();
        }
        let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
        assert_eq!(restored, preferences);
        ReaderPreferencePatch::OverrideParagraphSpacing(false).apply(&mut restored).unwrap();
        ReaderPreferencePatch::FirstLineIndent(super::ReaderFirstLineIndent::Book).apply(&mut restored).unwrap();
        assert_eq!(restored.paragraph_spacing_em(), 0.7);
        assert_eq!(restored.first_line_indent_em(), 2.1);
        for value in [-0.1, 10.0, f32::NAN, f32::INFINITY] {
            assert!(ReaderPreferencePatch::ParagraphSpacingEm(value).apply(&mut restored).is_err());
            assert!(ReaderPreferencePatch::FirstLineIndentEm(value).apply(&mut restored).is_err());
            assert_eq!(restored.paragraph_spacing_em(), 0.7);
            assert_eq!(restored.first_line_indent_em(), 2.1);
        }
        ReaderPreferencePatch::ParagraphSpacingEm(0.0).apply(&mut restored).unwrap();
        ReaderPreferencePatch::FirstLineIndentEm(0.0).apply(&mut restored).unwrap();
        assert_eq!(restored.paragraph_spacing_em(), 0.0);
        assert_eq!(restored.first_line_indent_em(), 0.0);
        let mut invalid = serde_json::to_value(restored).unwrap();
        invalid["first_line_indent_em"] = serde_json::json!(-1.0);
        assert!(serde_json::from_value::<ReaderPreferences>(invalid).is_err());
    }

    #[test]
    fn hyphenation_modes_preserve_legacy_preferences_and_round_trip() {
        use super::ReaderHyphenation;
        let mut saved = serde_json::to_value(ReaderPreferences::default()).unwrap();
        saved.as_object_mut().unwrap().remove("hyphenation");
        let mut preferences: ReaderPreferences = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(preferences.hyphenation(), ReaderHyphenation::Book);
        for choice in ReaderHyphenation::ALL {
            let patch = ReaderPreferencePatch::Hyphenation(choice);
            let decoded: ReaderPreferencePatch = serde_json::from_value(serde_json::to_value(patch).unwrap()).unwrap();
            decoded.apply(&mut preferences).unwrap();
            let restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
            assert_eq!(restored.hyphenation(), choice);
        }
        for (legacy, expected) in [(false, ReaderHyphenation::Off), (true, ReaderHyphenation::On)] {
            saved["hyphenation"] = serde_json::json!(legacy);
            assert_eq!(serde_json::from_value::<ReaderPreferences>(saved.clone()).unwrap().hyphenation(), expected);
            let patch: ReaderPreferencePatch = serde_json::from_value(serde_json::json!({"preference":"hyphenation", "value":legacy})).unwrap();
            patch.apply(&mut preferences).unwrap();
            assert_eq!(preferences.hyphenation(), expected);
        }
        saved["hyphenation"] = serde_json::json!("invalid");
        assert!(serde_json::from_value::<ReaderPreferences>(saved).is_err());
    }

    #[test]
    fn word_spacing_defaults_survive_reload_and_reject_invalid_values() {
        let mut legacy = serde_json::to_value(ReaderPreferences::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("word_spacing_percent");
        let mut preferences: ReaderPreferences = serde_json::from_value(legacy).unwrap();
        assert_eq!(preferences.word_spacing_percent(), 100.0);
        let patch = ReaderPreferencePatch::WordSpacingPercent(120.0);
        let decoded: ReaderPreferencePatch = serde_json::from_value(serde_json::to_value(patch).unwrap()).unwrap();
        decoded.apply(&mut preferences).unwrap();
        let saved = serde_json::to_value(preferences).unwrap();
        let restored: ReaderPreferences = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(restored.word_spacing_percent(), 120.0);
        for invalid in [0.0, 49.0, 201.0, f32::NAN, f32::INFINITY] {
            assert!(ReaderPreferencePatch::WordSpacingPercent(invalid).apply(&mut preferences).is_err());
            assert_eq!(preferences.word_spacing_percent(), 120.0);
        }
        let mut invalid_saved = saved;
        invalid_saved["word_spacing_percent"] = serde_json::json!(0);
        assert!(serde_json::from_value::<ReaderPreferences>(invalid_saved).is_err());
    }

    #[test]
    fn font_weight_choices_default_to_normal_and_survive_reload() {
        use super::ReaderFontWeight;
        let mut legacy = serde_json::to_value(ReaderPreferences::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("font_weight");
        let mut preferences: ReaderPreferences = serde_json::from_value(legacy).unwrap();
        assert_eq!(preferences.font_weight(), ReaderFontWeight::Normal);
        for choice in ReaderFontWeight::ALL {
            let patch = ReaderPreferencePatch::FontWeight(choice);
            let decoded: ReaderPreferencePatch = serde_json::from_value(serde_json::to_value(patch).unwrap()).unwrap();
            decoded.apply(&mut preferences).unwrap();
            let restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
            assert_eq!(restored.font_weight(), choice);
        }
    }

    #[test]
    fn line_height_override_preserves_legacy_profiles_and_saved_spacing() {
        let mut legacy = serde_json::to_value(ReaderPreferences::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("override_line_height");
        let mut preferences: ReaderPreferences = serde_json::from_value(legacy).unwrap();
        assert!(preferences.override_line_height());
        ReaderPreferencePatch::LineHeight(1.8).apply(&mut preferences).unwrap();
        let patch: ReaderPreferencePatch = serde_json::from_str(r#"{"preference":"override_line_height","value":false}"#).unwrap();
        patch.apply(&mut preferences).unwrap();
        let mut restored: ReaderPreferences = serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
        assert!(!restored.override_line_height());
        assert_eq!(restored.line_height(), 1.8);
        ReaderPreferencePatch::OverrideLineHeight(true).apply(&mut restored).unwrap();
        assert!(restored.override_line_height());
        assert_eq!(restored.line_height(), 1.8);
    }

    #[test]
    fn legacy_pdf_direction_overrides_are_ignored_when_loading_preferences() {
        for direction in ["document", "left_to_right", "right_to_left"] {
            let defaults = ReaderPreferences::default();
            let mut saved = serde_json::to_value(&defaults).unwrap();
            saved["pdf_reading_direction"] = serde_json::json!(direction);
            saved["pdf_right_to_left"] = serde_json::json!(true);
            let restored: ReaderPreferences = serde_json::from_value(saved).unwrap();
            assert_eq!(restored, defaults);
            let saved = serde_json::to_value(restored).unwrap();
            assert!(saved.get("pdf_reading_direction").is_none());
            assert!(saved.get("pdf_right_to_left").is_none());
        }
    }

    #[test]
    fn keyboard_shortcuts_preserve_legacy_profiles_and_disabled_actions() {
        let mut legacy = serde_json::to_value(BrowsingPreferences::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("keyboard_shortcuts");
        let mut preferences: BrowsingPreferences = serde_json::from_value(legacy).unwrap();
        assert!(preferences.keyboard_shortcuts().is_empty());
        let shortcuts = super::KeyboardShortcuts::from([("reader.search".into(), vec!["ctrl-shift-f".into(), "alt-f".into()]), ("browse.undo".into(), vec![])]);
        BrowsingPreferencePatch::KeyboardShortcuts(shortcuts.clone()).apply(&mut preferences).unwrap();
        let restored: BrowsingPreferences = serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert_eq!(restored.keyboard_shortcuts(), &super::normalize_keyboard_shortcuts(shortcuts));
        BrowsingPreferencePatch::KeyboardShortcuts(Default::default()).apply(&mut preferences).unwrap();
        assert!(preferences.keyboard_shortcuts().is_empty());
    }

    #[test]
    fn metered_transfer_preference_defaults_off_and_round_trips() {
        let mut legacy = serde_json::to_value(BrowsingPreferences::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("allow_metered_transfers");
        let mut preferences: BrowsingPreferences = serde_json::from_value(legacy).unwrap();
        assert!(!preferences.allow_metered_transfers());
        BrowsingPreferencePatch::AllowMeteredTransfers(true).apply(&mut preferences).unwrap();
        let restored: BrowsingPreferences = serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert!(restored.allow_metered_transfers());
        assert_eq!(restored, preferences);
    }

    #[test]
    fn custom_ui_font_round_trips_and_presets_preserve_the_saved_family() {
        let mut preferences = BrowsingPreferences::default();
        let mut legacy = serde_json::to_value(&preferences).unwrap();
        legacy.as_object_mut().unwrap().remove("custom_ui_font_family");
        assert_eq!(serde_json::from_value::<BrowsingPreferences>(legacy).unwrap(), preferences);
        BrowsingPreferencePatch::CustomUiFontFamily("Noto Serif".into()).apply(&mut preferences).unwrap();
        let restored: BrowsingPreferences = serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert_eq!(restored.ui_font_name(), "Noto Serif");
        assert_eq!(restored.custom_ui_font_family(), Some("Noto Serif"));
        assert!(restored.custom_ui_font_active());
        let mut legacy_custom = serde_json::to_value(&restored).unwrap();
        legacy_custom.as_object_mut().unwrap().remove("custom_ui_font_enabled");
        assert_eq!(serde_json::from_value::<BrowsingPreferences>(legacy_custom).unwrap().ui_font_name(), "Noto Serif");
        BrowsingPreferencePatch::UiFontFamily(UiFontFamily::SansSerif).apply(&mut preferences).unwrap();
        assert_eq!(preferences.custom_ui_font_family(), Some("Noto Serif"));
        assert!(!preferences.custom_ui_font_active());
        assert_eq!(preferences.ui_font_name(), UiFontFamily::SansSerif.family_name());
        let mut restored: BrowsingPreferences = serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert_eq!(restored.custom_ui_font_family(), Some("Noto Serif"));
        assert!(!restored.custom_ui_font_active());
        assert_eq!(restored.ui_font_name(), UiFontFamily::SansSerif.family_name());
        BrowsingPreferencePatch::CustomUiFontFamily(restored.custom_ui_font_family().unwrap().into()).apply(&mut restored).unwrap();
        assert_eq!(restored.ui_font_name(), "Noto Serif");
        assert!(restored.custom_ui_font_active());
    }

    #[test]
    fn ui_font_preferences_preserve_legacy_settings_and_serialized_changes() {
        let mut preferences = BrowsingPreferences::default();
        BrowsingPreferencePatch::UiFontSizePx(22).apply(&mut preferences).unwrap();
        BrowsingPreferencePatch::CoverText(CoverText::CoverOnly).apply(&mut preferences).unwrap();
        let mut legacy = serde_json::to_value(&preferences).unwrap();
        legacy.as_object_mut().unwrap().remove("ui_font_family");
        let mut restored: BrowsingPreferences = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored, preferences);
        assert_eq!(restored.ui_font_family(), UiFontFamily::Serif);

        let change: BrowsingPreferencePatch = serde_json::from_str(r#"{"preference":"ui_font_family","value":"sans_serif"}"#).unwrap();
        change.apply(&mut restored).unwrap();
        let saved = serde_json::to_value(&restored).unwrap();
        assert_eq!(saved["ui_font_family"], "sans_serif");
        let reloaded: BrowsingPreferences = serde_json::from_value(saved).unwrap();
        assert_eq!(reloaded, restored);
        assert_eq!(reloaded.ui_font_family(), UiFontFamily::SansSerif);
        assert_eq!(reloaded.ui_font_size_px(), 22);
        assert_eq!(reloaded.cover_text(), CoverText::CoverOnly);
    }

    #[test]
    fn catalog_language_preferences_preserve_unset_any_and_saved_choices() {
        let mut preferences = BrowsingPreferences::default();
        let mut legacy = serde_json::to_value(&preferences).unwrap();
        legacy.as_object_mut().unwrap().remove("catalog_languages");
        assert!(serde_json::from_value::<BrowsingPreferences>(legacy).unwrap().catalog_languages().is_none());
        for languages in [Some(vec!["en".into(), "sv".into()]), Some(vec![]), None] {
            BrowsingPreferencePatch::CatalogLanguages(languages.clone()).apply(&mut preferences).unwrap();
            let restored: BrowsingPreferences = serde_json::from_value(serde_json::to_value(&preferences).unwrap()).unwrap();
            assert_eq!(restored.catalog_languages(), languages.as_deref());
        }
    }

    #[test]
    fn legacy_preferences_default_missing_margins_but_preserve_explicit_zero() {
        let defaults = ReaderPreferences::default();
        assert_eq!(defaults.horizontal_margin_px(), DEFAULT_READER_MARGIN_PX);
        assert_eq!(defaults.vertical_margin_px(), DEFAULT_READER_MARGIN_PX);
        let mut saved = serde_json::to_value(&defaults).unwrap();
        let fields = saved.as_object_mut().unwrap();
        fields.remove("horizontal_margin_px");
        fields.remove("vertical_margin_px");
        let restored: ReaderPreferences = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(restored.horizontal_margin_px(), DEFAULT_READER_MARGIN_PX);
        assert_eq!(restored.vertical_margin_px(), DEFAULT_READER_MARGIN_PX);
        saved["horizontal_margin_px"] = serde_json::json!(0.0);
        saved["vertical_margin_px"] = serde_json::json!(0.0);
        let restored: ReaderPreferences = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.horizontal_margin_px(), 0.0);
        assert_eq!(restored.vertical_margin_px(), 0.0);
    }
}
