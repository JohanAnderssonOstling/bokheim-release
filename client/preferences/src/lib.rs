use serde::{Deserialize, Serialize};
use std::fmt;

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
    /// Neutral greys and true-white cards, navy accent. Not a tinted paper —
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
            Self::Cool => "Neutral greys and white cards with a navy accent.",
        }
    }
}

/// The interface is set in one family, and it is a serif.
///
/// The app is a library: hierarchy is carried by size, colour and space rather
/// than by weight, and the type scale in `ui-components` is calibrated to this
/// face's x-height.
pub const INTERFACE_FONT_FAMILY: &str = "Libertinus Serif";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsingPreferences {
    theme: ApplicationTheme,
    #[serde(default)]
    appearance_override: Option<ResolvedAppearance>,
    ui_font_size_px: UiFontSize,
    library_view: LibraryView,
    cover_text: CoverText,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryView {
    #[default]
    Covers,
    Detailed,
}

impl LibraryView {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Covers => "Covers",
            Self::Detailed => "Detailed",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Covers => "A grid of cover art.",
            Self::Detailed => "Rows with the blurb and actions beside the cover.",
        }
    }
}

impl Default for BrowsingPreferences {
    fn default() -> Self {
        Self { theme: ApplicationTheme::default(), appearance_override: None, ui_font_size_px: UiFontSize(UI_FONT_SIZE_DEFAULT), library_view: LibraryView::Covers, cover_text: CoverText::Always }
    }
}

impl BrowsingPreferences {
    pub fn theme(&self) -> ApplicationTheme {
        self.theme
    }

    pub fn appearance_override(&self) -> Option<ResolvedAppearance> {
        self.appearance_override
    }

    pub fn ui_font_size_px(&self) -> u8 {
        self.ui_font_size_px.get()
    }
    pub fn library_view(&self) -> LibraryView {
        self.library_view
    }
    pub fn cover_text(&self) -> CoverText {
        self.cover_text
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
    Theme(ApplicationTheme),
    Palette(ApplicationTheme, ResolvedAppearance),
    UiFontSizePx(u8),
    LibraryView(LibraryView),
    CoverText(CoverText),
}

impl BrowsingPreferencePatch {
    pub fn apply(self, preferences: &mut BrowsingPreferences) -> Result<(), PreferenceValueError> {
        match self {
            Self::Theme(value) => preferences.theme = value,
            Self::Palette(theme, appearance) => {
                preferences.theme = theme;
                preferences.appearance_override = Some(appearance);
            }
            Self::UiFontSizePx(value) => preferences.ui_font_size_px = UiFontSize::new(value)?,
            Self::LibraryView(value) => preferences.library_view = value,
            Self::CoverText(value) => preferences.cover_text = value,
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
    flow: ReaderFlow,
    font_family: ReaderFontFamily,
    text_alignment: ReaderTextAlignment,
    hyphenation: bool,
    max_column_count: ColumnCount,
    #[serde(default = "default_reader_spacing")]
    horizontal_margin_px: ReaderSpacing,
    #[serde(default = "default_reader_spacing")]
    vertical_margin_px: ReaderSpacing,
    #[serde(default = "default_reader_spacing")]
    min_column_gap_px: ReaderSpacing,
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

const fn default_audiobook_speed() -> AudiobookSpeed {
    AudiobookSpeed(1.0)
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
        if (UI_FONT_SIZE_MIN..=UI_FONT_SIZE_MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(PreferenceValueError("UI font size"))
        }
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
                if value.is_finite() && ($minimum..=$maximum).contains(&value) {
                    Ok(Self(value))
                } else {
                    Err(PreferenceValueError($label))
                }
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
bounded_float!(PdfZoom, f32, "PDF zoom", PDF_ZOOM_MIN, PDF_ZOOM_MAX);
bounded_float!(AudiobookSpeed, f64, "audiobook speed", AUDIOBOOK_SPEED_MIN, AUDIOBOOK_SPEED_MAX);
bounded_float!(ReaderSpacing, f32, "reader spacing", READER_SPACING_MIN, READER_SPACING_MAX);

const fn default_reader_spacing() -> ReaderSpacing {
    ReaderSpacing(0.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct ColumnCount(u8);

impl ColumnCount {
    fn new(value: u8) -> Result<Self, PreferenceValueError> {
        // Only a floor: how many columns fit is a question about the width
        // available, which no contract here is in a position to answer.
        if value >= COLUMN_COUNT_MIN {
            Ok(Self(value))
        } else {
            Err(PreferenceValueError("column count"))
        }
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
        if (PDF_PAGE_COUNT_MIN..=PDF_PAGE_COUNT_MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(PreferenceValueError("PDF page count"))
        }
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
            flow: ReaderFlow::Paginated,
            font_family: ReaderFontFamily::Publisher,
            text_alignment: ReaderTextAlignment::Publisher,
            hyphenation: true,
            max_column_count: ColumnCount(2),
            horizontal_margin_px: default_reader_spacing(),
            vertical_margin_px: default_reader_spacing(),
            min_column_gap_px: default_reader_spacing(),
            pdf_zoom_mode: PdfZoomMode::FitWidth,
            pdf_page_count: PdfPageCount(1),
            pdf_zoom: PdfZoom(1.0),
            pdf_trim_margins: true,
            audiobook_speed: default_audiobook_speed(),
        }
    }
}

impl ReaderPreferences {
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
    pub fn flow(self) -> ReaderFlow {
        self.flow
    }
    pub fn font_family(self) -> ReaderFontFamily {
        self.font_family
    }
    pub fn text_alignment(self) -> ReaderTextAlignment {
        self.text_alignment
    }
    pub fn hyphenation(self) -> bool {
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
    pub fn set_flow(&mut self, value: ReaderFlow) {
        self.flow = value;
    }
    pub fn set_font_family(&mut self, value: ReaderFontFamily) {
        self.font_family = value;
    }
    pub fn set_text_alignment(&mut self, value: ReaderTextAlignment) {
        self.text_alignment = value;
    }
    pub fn set_hyphenation(&mut self, value: bool) {
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
    Hyphenation(bool),
    MaxColumnCount(u8),
    HorizontalMarginPx(f32),
    VerticalMarginPx(f32),
    MinColumnGapPx(f32),
    PdfZoomMode(PdfZoomMode),
    PdfPageCount(u8),
    PdfZoom(f32),
    PdfTrimMargins(bool),
    AudiobookSpeed(f64),
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
    use super::*;

    #[test]
    fn local_preference_patches_keep_the_serialized_shape() {
        let browsing = BrowsingPreferencePatch::LibraryView(LibraryView::Detailed);
        assert_eq!(serde_json::to_value(browsing).unwrap(), serde_json::json!({ "preference": "library_view", "value": "detailed" }));

        let palette = BrowsingPreferencePatch::Palette(ApplicationTheme::Cool, ResolvedAppearance::Dark);
        assert_eq!(serde_json::to_value(palette).unwrap(), serde_json::json!({ "preference": "palette", "value": ["cool", "dark"] }));

        let reader = ReaderPreferencePatch::PdfZoomMode(PdfZoomMode::FitHeight);
        assert_eq!(serde_json::to_value(reader).unwrap(), serde_json::json!({ "preference": "pdf_zoom_mode", "value": "fit_height" }));

        let count = ReaderPreferencePatch::PdfPageCount(3);
        assert_eq!(serde_json::to_value(count).unwrap(), serde_json::json!({ "preference": "pdf_page_count", "value": 3 }));
    }

    #[test]
    fn incomplete_profiles_are_rejected() {
        assert!(serde_json::from_str::<BrowsingPreferences>("{}").is_err());
        assert!(serde_json::from_str::<ReaderPreferences>("{}").is_err());
    }

    #[test]
    fn palette_patch_saves_both_choices_and_old_profiles_follow_system() {
        let mut saved = serde_json::to_value(BrowsingPreferences::default()).unwrap();
        saved.as_object_mut().unwrap().remove("appearance_override");
        let mut preferences: BrowsingPreferences = serde_json::from_value(saved).unwrap();
        assert_eq!(preferences.appearance_override(), None);
        BrowsingPreferencePatch::Palette(ApplicationTheme::Cool, ResolvedAppearance::Dark).apply(&mut preferences).unwrap();
        assert_eq!(preferences.theme(), ApplicationTheme::Cool);
        assert_eq!(preferences.appearance_override(), Some(ResolvedAppearance::Dark));
        let restored: BrowsingPreferences = serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
        assert_eq!(restored.appearance_override(), Some(ResolvedAppearance::Dark));
    }

    #[test]
    fn older_reader_profiles_get_zero_spacing_and_new_values_are_bounded() {
        let mut saved = serde_json::to_value(ReaderPreferences::default()).unwrap();
        let object = saved.as_object_mut().unwrap();
        object.remove("horizontal_margin_px");
        object.remove("vertical_margin_px");
        object.remove("min_column_gap_px");
        let restored: ReaderPreferences = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.horizontal_margin_px(), 0.0);
        assert_eq!(restored.vertical_margin_px(), 0.0);
        assert_eq!(restored.min_column_gap_px(), 0.0);

        let mut updated = restored;
        ReaderPreferencePatch::MinColumnGapPx(80.0).apply(&mut updated).unwrap();
        assert_eq!(updated.min_column_gap_px(), 80.0);
        assert!(ReaderPreferencePatch::HorizontalMarginPx(f32::NAN).apply(&mut updated).is_err());
        assert!(ReaderPreferencePatch::VerticalMarginPx(READER_SPACING_MAX + 1.0).apply(&mut updated).is_err());
    }
}
