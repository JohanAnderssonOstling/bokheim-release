//! The application palette, and the two things derived from it.
//!
//! Colour lives here rather than in `app-preferences` because it is a
//! rendering concern: the preference crate is linked by the backend, which has
//! no business holding ninety-odd colour literals. What crosses that boundary
//! — and what would cross the wire if the theme ever synced between devices —
//! is the theme and optional appearance choice. An older client then draws
//! its own tested palette instead of rendering hexes no contrast test on that
//! machine has ever seen.
//!
//! The flow is one-directional:
//!
//! ```text
//! ApplicationPalette  ->  BrowserTheme  ->  gpui_component::Theme
//!    (what a theme is)     (what we draw)    (what the vendored widgets read)
//! ```
//!
//! [`BrowserTheme`] used to be reconstructed by reading `gpui_component::Theme`
//! back out after writing it, which made that struct a lossy transport between
//! two of our own types: three tokens ended up aliased to one colour and
//! `warning_bg` was written but never read back. It is now derived from the
//! palette by [`BrowserTheme::from_palette`], and the gpui-component theme is
//! written *from* it purely to dress the vendored widgets — never read back.

use std::rc::Rc;

use app_preferences::{ApplicationTheme, BrowsingPreferences, ResolvedAppearance, UiFontFamily};
use gpui::{App, Hsla, px};
use gpui_component::menu::ChoiceRowColors;
use gpui_component::{Theme, ThemeTokens};

/// One theme at one appearance.
///
/// Most surfaces share `surface_bg` and are told apart by their edge.
/// `raised_bg` is the one deliberate second fill: bright browse cards against
/// a slightly darker page. `hover_overlay`
/// carries alpha (`0xRRGGBBAA`) so one value reads correctly wherever it
/// lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplicationPalette {
    pub surface_bg: u32,
    pub raised_bg: u32,
    pub ink_text: u32,
    pub subtle_text: u32,
    pub accent: u32,
    /// Highlighted text and icons on a neutral surface. Gold in both light
    /// themes and in the shared dark palette.
    pub text_accent: u32,
    /// The accent under the pointer and while pressed. Held as palette values
    /// rather than derived at paint time because the direction differs by
    /// appearance — light darkens, dark lightens — and because the previous
    /// theme aliased all three to `accent`, leaving primary buttons with no
    /// hover or press feedback at all.
    pub accent_hover: u32,
    pub accent_active: u32,
    /// Foreground for anything sitting *on* the accent — a filled primary
    /// button, a selected segment. Its own value, not a surface that happens
    /// to contrast.
    pub accent_text: u32,
    pub border: u32,
    /// A boundary that separates without asserting: row dividers, card edges,
    /// the rail's edge against content.
    ///
    /// `border` sits at 3.2:1 or above, which is right for something
    /// interactive and far too loud for decoration — one value on every
    /// hairline is what made the interface read as uniformly emphatic. This is
    /// the quiet tier, and `border` becomes where hover travels to.
    pub rule: u32,
    /// The light wash under the pointer: the accent at a low alpha, in every
    /// appearance, so hovering reads as a lighter step of `active_highlight`
    /// — the keyboard's wash — rather than as a grey of its own.
    pub hover_overlay: u32,
    /// The keyboard's wash: the cursor and the selection. See the ladder in
    /// `tokens.rs`.
    pub active_highlight: u32,
    /// `active_highlight` under the pointer. Its own value rather than a step
    /// off it: see the generator's ladder, which owns both the size of the step
    /// and its direction.
    pub active_highlight_hover: u32,
    pub danger_text: u32,
    pub danger_bg: u32,
    /// State that needs a decision but is not a failure: a library holding
    /// unsynced changes, bytes reserved by an upload in flight. Separate from
    /// `accent` so "fine" and "look at this" never render the same, and from
    /// `danger_*` so a pending transfer does not read as an error.
    pub warning_text: u32,
    pub warning_bg: u32,
    /// Categorical colours for library storage, shared by the meter and legend.
    pub usage_segments: [u32; 5],
}

#[cfg(feature = "kobo")]
mod kobo;
mod palettes;

/// The palette table lives in `theme/palettes.rs`. The application reads its
/// literals without doing colour arithmetic at paint time.
pub use palettes::palette;

/// The selected square shown by both Browse and Reader settings.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SelectedPalette(pub ApplicationTheme, pub ResolvedAppearance);

impl gpui::Global for SelectedPalette {}

/// Saves palette selections through the application's preference store.
pub trait PaletteSelectionService {
    fn select_palette(&self, theme: ApplicationTheme, appearance: ResolvedAppearance, cx: &mut App);
}

/// Installed by the browser preference store so reader controls save through
/// the same backend path as Browse settings.
pub struct PaletteSelectionHandler(pub Rc<dyn PaletteSelectionService>);

impl gpui::Global for PaletteSelectionHandler {}

pub fn select_palette(theme: ApplicationTheme, appearance: ResolvedAppearance, cx: &mut App) {
    if let Some(handler) = cx.try_global::<PaletteSelectionHandler>().map(|handler| handler.0.clone()) {
        handler.select_palette(theme, appearance, cx);
    }
}

fn platform_palette(theme: ApplicationTheme, appearance: ResolvedAppearance) -> ApplicationPalette {
    #[cfg(feature = "kobo")]
    {
        let _ = (theme, appearance);
        kobo::PALETTE
    }
    #[cfg(not(feature = "kobo"))]
    {
        palette(theme, appearance)
    }
}

/// The palette as the interface consumes it: gpui colours, named for what they
/// are used for rather than for where they came from.
///
/// Stored as a global and read through [`browser_theme`]. Deriving it once per
/// appearance change rather than per call also means a component cannot see a
/// half-applied theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrowserTheme {
    /// Interface typography, shared by text and prominent display headings.
    pub ui_font_family: UiFontFamily,
    /// Display headings inherit the interface face when a custom font is active.
    pub custom_ui_font: bool,
    /// The surface. Every region — the page, a card, a popover, a panel — is
    /// this colour by default, and is told apart by its `rule` edge and, if it
    /// floats, by `overlay_shadow`.
    pub page_bg: Hsla,
    /// A brighter second fill for borderless browse-section cards.
    pub raised_bg: Hsla,
    pub border: Hsla,
    /// The quiet boundary: dividers, card edges, a region's edge against
    /// content. `border` is the interactive tier and is where hover travels to;
    /// using it for decoration is what made every hairline read as emphatic.
    pub rule: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub accent: Hsla,
    /// Highlighted text and icons on a neutral surface.
    pub text_accent: Hsla,
    /// Foreground for anything sitting *on* `accent`. Needed the moment a
    /// surface inverts to the accent instead of merely bordering with it —
    /// `text` is unreadable there, and `page_bg` only happens to work in light.
    pub accent_text: Hsla,
    /// The keyboard's tier: the cursor and the selection, which is transient
    /// however it was made. Also the value chosen in a settings control. What
    /// it means is set by the ladder in `tokens.rs`; pointer hover is the
    /// lighter `hover`.
    ///
    /// Distinct from `accent_hover`, which is the accent *itself* under the
    /// pointer. The two are both "hover", but from different starting surfaces
    /// — a wash over a solid accent would be invisible, and darkening a neutral
    /// row to `accent_hover` would be far too heavy. Named `accent_wash` rather
    /// than `accent_soft` so that difference is legible at the call site.
    pub accent_wash: Hsla,
    /// `accent_wash` under the pointer, and the reason it cannot be an overlay.
    ///
    /// gpui's hover style *replaces* `background` rather than painting over it,
    /// so hovering a selected row with the translucent `hover` composites that
    /// 5% black against the page and throws the wash away — the row stops
    /// looking selected at exactly the moment the pointer is on it.
    ///
    /// This is the wash pushed further from the page instead, which keeps the
    /// two hovers apart: hovering a selection has to look unlike hovering
    /// anything else, or the pointer erases the one distinction the grid is
    /// making.
    pub accent_wash_hover: Hsla,
    /// The accent under the pointer. Held on the theme because a filled accent
    /// cannot hover with `accent_wash`.
    pub accent_hover: Hsla,
    /// The accent while a selected or primary control is pressed.
    pub accent_active: Hsla,
    /// The light wash under the pointer — the accent at a low alpha.
    /// Translucent: composited over whichever surface it lands on. Solid hover
    /// colours were the reason hover was invisible on the page background.
    pub hover: Hsla,
    pub shadow: Hsla,
    pub overlay_scrim: Hsla,
    pub danger: Hsla,
    /// Surface behind `danger` text. An error is not an accent surface; without
    /// this the only tinted background to hand was `accent_wash`, which is what
    /// the audiobook player reached for.
    pub danger_surface: Hsla,
    /// State that needs a decision but is not a failure. Kept apart from
    /// `accent`, so a settled page carries no state colour at all, and from
    /// `danger`, so a pending transfer does not read as an error.
    pub warning: Hsla,
    /// The tint `warning` is set on. The palette has always carried it; the old
    /// round trip through `gpui_component::Theme` wrote it and never read it
    /// back, so warning text had no surface of its own to sit on.
    pub warning_surface: Hsla,
    /// Distinct library colours with contrast against the page in this appearance.
    pub usage_segments: [Hsla; 5],
}

impl gpui::Global for BrowserTheme {}

impl From<BrowserTheme> for ChoiceRowColors {
    fn from(theme: BrowserTheme) -> Self {
        Self { foreground: theme.text, disabled_foreground: theme.text_muted, selected_background: theme.accent, selected_foreground: theme.accent_text, hover_background: theme.hover }
    }
}

impl BrowserTheme {
    /// Derives every colour from the palette, with the default interface face.
    pub fn from_palette(palette: ApplicationPalette) -> Self {
        let colour = |value: u32| -> Hsla { gpui::rgb(value).into() };
        let mut shadow = colour(palette.border);
        shadow.a = if cfg!(feature = "kobo") { 0.0 } else { 0.25 };
        let mut overlay_scrim = colour(palette.ink_text);
        overlay_scrim.a = 0.5;
        Self {
            ui_font_family: UiFontFamily::default(),
            custom_ui_font: false,
            page_bg: colour(palette.surface_bg),
            raised_bg: colour(palette.raised_bg),
            border: colour(palette.border),
            rule: colour(palette.rule),
            text: colour(palette.ink_text),
            text_muted: colour(palette.subtle_text),
            accent: colour(palette.accent),
            text_accent: colour(palette.text_accent),
            accent_text: colour(palette.accent_text),
            accent_wash: colour(palette.active_highlight),
            accent_wash_hover: colour(palette.active_highlight_hover),
            accent_hover: colour(palette.accent_hover),
            accent_active: colour(palette.accent_active),
            hover: gpui::rgba(palette.hover_overlay).into(),
            shadow,
            overlay_scrim,
            danger: colour(palette.danger_text),
            danger_surface: colour(palette.danger_bg),
            warning: colour(palette.warning_text),
            warning_surface: colour(palette.warning_bg),
            usage_segments: palette.usage_segments.map(colour),
        }
    }

    pub const fn display_font_family(self) -> Option<&'static str> {
        if self.custom_ui_font {
            return None;
        }
        Some(match self.ui_font_family {
            UiFontFamily::Serif => crate::DISPLAY_FONT_FAMILY,
            UiFontFamily::SansSerif => self.ui_font_family.family_name(),
        })
    }
}

impl Default for BrowserTheme {
    /// The default theme in light. Only reached before the first
    /// [`apply_appearance`], which happens during startup.
    fn default() -> Self {
        Self::from_palette(platform_palette(ApplicationTheme::default(), ResolvedAppearance::Light))
    }
}

/// The active colours and typography, copied together.
pub fn browser_theme(cx: &App) -> BrowserTheme {
    cx.try_global::<BrowserTheme>().copied().unwrap_or_default()
}

/// The platform appearance used until the user selects a theme square.
/// On Linux this comes from the XDG desktop portal's `color-scheme`; Kobo
/// always reports light for its e-ink display.
pub fn resolved_appearance(cx: &App) -> ResolvedAppearance {
    #[cfg(feature = "kobo")]
    {
        let _ = cx;
        ResolvedAppearance::Light
    }
    #[cfg(not(feature = "kobo"))]
    match cx.window_appearance() {
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark => ResolvedAppearance::Dark,
        gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => ResolvedAppearance::Light,
    }
}

/// Resolves the preferences against the platform, installs the derived
/// [`BrowserTheme`], dresses the vendored widgets from it and repaints.
///
/// The single point where appearance becomes visible. Nothing else writes
/// `gpui_component::Theme`.
pub fn apply_appearance(preferences: &BrowsingPreferences, cx: &mut App) {
    let resolved = preferences.appearance_override().unwrap_or_else(|| resolved_appearance(cx));
    let mut theme = BrowserTheme::from_palette(platform_palette(preferences.theme(), resolved));
    theme.ui_font_family = preferences.ui_font_family();
    theme.custom_ui_font = preferences.custom_ui_font_active();
    cx.set_global(SelectedPalette(preferences.theme(), resolved));
    cx.set_global(theme);

    let font_family = crate::configured_ui_font_name(preferences, cx).to_owned();
    let application_theme = Theme::global_mut(cx);
    application_theme.notification.placement = gpui::Anchor::BottomRight;
    #[cfg(not(feature = "kobo"))]
    {
        application_theme.mode = match resolved {
            ResolvedAppearance::Light => gpui_component::ThemeMode::Light,
            ResolvedAppearance::Dark => gpui_component::ThemeMode::Dark,
        };
    }
    #[cfg(feature = "kobo")]
    {
        application_theme.mode = gpui_component::ThemeMode::Light;
        application_theme.shadow = false;
        application_theme.scrollbar_show = gpui_component::scroll::ScrollbarShow::Always;
        application_theme.colors = kobo::widget_colours(theme);
    }
    application_theme.font_family = font_family.into();
    application_theme.font_size = px(preferences.ui_font_size_px() as f32);
    application_theme.radius = px(0.0);
    application_theme.radius_lg = px(0.0);
    apply_widget_colours(theme, application_theme);
    application_theme.tokens = ThemeTokens::from(application_theme.colors);
    cx.refresh_windows();
}

/// Dresses `gpui-component`'s widgets in our theme.
///
/// One-way. `gpui-component` speaks the shadcn vocabulary, where `primary` is
/// the brand colour and `accent` is the muted highlight behind a hovered or
/// selected row. Its `accent` receives our shared neutral `hover`, while
/// its `primary` receives our solid `accent` for current choices.
fn apply_widget_colours(theme: BrowserTheme, application: &mut Theme) {
    let colors = &mut application.colors;
    colors.background = theme.page_bg;
    colors.foreground = theme.text;
    colors.border = theme.border;
    colors.accent = theme.hover;
    colors.accent_foreground = theme.text;
    colors.group_box = theme.page_bg;
    colors.group_box_foreground = theme.text;
    colors.input = theme.rule;
    colors.muted = theme.hover;
    colors.muted_foreground = theme.text_muted;
    colors.popover = theme.page_bg;
    colors.popover_foreground = theme.text;
    colors.primary = theme.accent;
    colors.primary_active = theme.accent_active;
    colors.primary_hover = theme.accent_hover;
    colors.primary_foreground = theme.accent_text;
    colors.secondary = theme.page_bg;
    colors.secondary_active = theme.accent_wash;
    colors.secondary_hover = theme.hover;
    colors.secondary_foreground = theme.text;
    colors.button = theme.page_bg;
    colors.button_active = theme.accent_wash;
    colors.button_hover = theme.hover;
    colors.button_foreground = theme.text;
    colors.button_primary = theme.accent;
    colors.button_primary_active = theme.accent_active;
    colors.button_primary_hover = theme.accent_hover;
    colors.button_primary_foreground = theme.accent_text;
    colors.button_secondary = theme.page_bg;
    colors.button_secondary_active = theme.accent_wash;
    colors.button_secondary_hover = theme.hover;
    colors.button_secondary_foreground = theme.text;
    colors.caret = theme.accent;
    colors.list = theme.page_bg;
    colors.list_active = theme.accent_wash;
    colors.list_active_border = theme.rule;
    colors.list_even = theme.page_bg;
    colors.list_head = theme.page_bg;
    colors.list_hover = theme.hover;
    colors.selection = theme.accent.opacity(f32::from(SELECTION_ALPHA) / 255.0);
    colors.sidebar = theme.page_bg;
    colors.sidebar_accent = theme.accent_wash;
    colors.sidebar_accent_foreground = theme.text_accent;
    colors.sidebar_border = theme.rule;
    colors.sidebar_foreground = theme.text;
    colors.sidebar_primary = theme.accent;
    colors.sidebar_primary_foreground = theme.accent_text;
    colors.title_bar = theme.page_bg;
    colors.title_bar_border = theme.rule;
    // The scrim behind a dialog, the same one our own modals lay down, so a
    // confirmation dims the page the way the navigation modal does.
    colors.overlay = theme.overlay_scrim;
    // gpui-component treats the semantic colour itself as the readable
    // foreground for alerts and derives a soft background from it. The old
    // assignment inverted these roles, rendering the pale tint as alert text.
    colors.danger = theme.danger;
    colors.danger_foreground = theme.danger_surface;
    // A destructive button, such as a dialog's "Delete library": the danger
    // text colour as its fill, lettered in the danger surface. Left unset it
    // kept gpui-component's stock red in every palette.
    colors.button_danger = theme.danger;
    colors.button_danger_hover = theme.danger.opacity(0.88);
    colors.button_danger_active = theme.danger.opacity(0.76);
    colors.button_danger_foreground = theme.danger_surface;
    colors.warning = theme.warning;
    colors.warning_foreground = theme.warning_surface;
    colors.ring = theme.accent;
}

/// Selection, search hits and annotations, as raw `[r, g, b, a]` channels.
///
/// Lives here rather than in the reader because it is a palette question —
/// what a selection *is* — while the reader owns only the conversion into
/// whichever colour type its renderer speaks. Both readers used to hold five
/// hardcoded light-mode constants instead, with the setters that would have
/// replaced them never called from anywhere: every theme and both appearances
/// drew the same pale green and amber, and in dark the page swallowed them.
///
/// Three roles from three tokens: selection is the accent, and search hits and
/// annotations are both the warning amber — which is what the old constants
/// were too, since a highlighter is yellow in every theme. Each pair separates
/// by weight rather than hue, because a hit and a note rarely land on the same
/// word while an active one always has to win against its own passive form.
/// The passage being read aloud is a lighter step of the selection's accent.
///
/// On e-ink the amber is a grey, and a search hit at the annotation's weight
/// reads as one. There hits are the accent at steps darker than any
/// annotation or narration highlight paints, the active one still light
/// enough to read black text through.
///
/// Alphas rather than blends: a highlight is laid over text the book
/// drew, so it has to stay translucent whatever is underneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InteractionHighlights {
    pub selection: [u8; 4],
    pub search_match: [u8; 4],
    pub active_search_match: [u8; 4],
    pub annotation: [u8; 4],
    pub active_annotation: [u8; 4],
    pub reading: [u8; 4],
}

const SELECTION_ALPHA: u8 = 96;
#[cfg(not(feature = "kobo"))]
const SEARCH_MATCH_ALPHA: u8 = 88;
#[cfg(not(feature = "kobo"))]
const ACTIVE_SEARCH_MATCH_ALPHA: u8 = 152;
#[cfg(feature = "kobo")]
const SEARCH_MATCH_ALPHA: u8 = 120;
#[cfg(feature = "kobo")]
const ACTIVE_SEARCH_MATCH_ALPHA: u8 = 150;
const ANNOTATION_ALPHA: u8 = 72;
const ACTIVE_ANNOTATION_ALPHA: u8 = 136;
const READING_ALPHA: u8 = 56;

/// A theme colour laid over the page at `alpha`.
fn highlight(colour: Hsla, alpha: u8) -> [u8; 4] {
    let rgba = gpui::Rgba::from(colour);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [channel(rgba.r), channel(rgba.g), channel(rgba.b), alpha]
}

impl InteractionHighlights {
    pub fn from_theme(theme: BrowserTheme) -> Self {
        let search = if cfg!(feature = "kobo") { theme.accent } else { theme.warning };
        Self {
            selection: highlight(theme.accent, SELECTION_ALPHA),
            search_match: highlight(search, SEARCH_MATCH_ALPHA),
            active_search_match: highlight(search, ACTIVE_SEARCH_MATCH_ALPHA),
            annotation: highlight(theme.warning, ANNOTATION_ALPHA),
            active_annotation: highlight(theme.warning, ACTIVE_ANNOTATION_ALPHA),
            reading: highlight(theme.accent, READING_ALPHA),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn popup_and_sheet_choice_colors_match_every_palette() {
        for theme in ApplicationTheme::ALL {
            for appearance in [ResolvedAppearance::Light, ResolvedAppearance::Dark] {
                let sheet = BrowserTheme::from_palette(palette(theme, appearance));
                let mut popup = Theme::default();
                apply_widget_colours(sheet, &mut popup);
                popup.tokens = ThemeTokens::from(popup.colors);
                assert_eq!(ChoiceRowColors::from(sheet), ChoiceRowColors::from(&popup), "choice styling diverged for {theme:?} / {appearance:?}");
            }
        }
    }

    #[test]
    fn selected_button_families_keep_heavy_accent() {
        use gpui::{Fill, Styled};
        use gpui_component::IconName;

        for palette in ApplicationTheme::ALL {
            for appearance in [ResolvedAppearance::Light, ResolvedAppearance::Dark] {
                let theme = BrowserTheme::from_palette(crate::theme::palette(palette, appearance));
                let buttons = [
                    crate::settings_choice_button("choice", "Choice", true, theme),
                    crate::browser_settings_header_button("action", "Action", true, theme),
                    crate::nav_rail_item("rail", IconName::Settings, "Settings", true, theme),
                    crate::bottom_navigation_item("bottom", IconName::Settings, "Settings", true, theme),
                    crate::library_switcher_item("library", "Library", true, false, theme),
                    crate::reader_annotation_action_button("annotation", "Edit", true, theme),
                ];
                for mut button in buttons {
                    let style = button.style();
                    assert_eq!(style.background, Some(Fill::from(if cfg!(feature = "kobo") { theme.page_bg } else { theme.accent })));
                    assert_eq!(style.text.color, Some(if cfg!(feature = "kobo") { theme.text } else { theme.accent_text }));
                }
            }
        }
    }

    #[gpui::test]
    fn text_selection_highlight_stays_translucent(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            apply_appearance(&BrowsingPreferences::default(), cx);
            let selection = Theme::global(cx).selection;
            assert!(selection.a > 0.0 && selection.a < 1.0, "selection is painted over text and must keep the letters visible");
        });
    }
}
