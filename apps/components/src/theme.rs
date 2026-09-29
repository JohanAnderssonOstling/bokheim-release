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

use app_preferences::{ApplicationTheme, BrowsingPreferences, ResolvedAppearance};
use gpui::{App, Hsla, px};
use gpui_component::{Theme, ThemeTokens};

/// One theme at one appearance.
///
/// Most surfaces share `surface_bg` and are told apart by their edge.
/// `raised_bg` is the one deliberate second fill: a browse-page section that
/// wants to read as raised off the page without floating. `hover_overlay`
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
    pub hover_overlay: u32,
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

/// Installed by the browser preference store so reader controls save through
/// the same backend path as Browse settings.
pub struct PaletteSelectionHandler(pub Rc<dyn Fn(ApplicationTheme, ResolvedAppearance, &mut App)>);

impl gpui::Global for PaletteSelectionHandler {}

pub fn select_palette(theme: ApplicationTheme, appearance: ResolvedAppearance, cx: &mut App) {
    if let Some(handler) = cx.try_global::<PaletteSelectionHandler>().map(|handler| handler.0.clone()) {
        handler(theme, appearance, cx);
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
    /// The surface. Every region — the page, a card, a popover, a panel — is
    /// this colour by default, and is told apart by its `rule` edge and, if it
    /// floats, by `overlay_shadow`.
    pub page_bg: Hsla,
    /// A second fill, lighter than `page_bg` in both appearances, for a region
    /// that wants to read as raised off the page without floating — see
    /// `paginator::element`'s group panels.
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
    /// A pale accent tint laid over a neutral surface: hover on a list row, a
    /// tab, a navigation destination.
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
    /// The accent while pressed. Only the vendored widgets reach for it today;
    /// it is on the theme because the palette distinguishes it and dropping it
    /// here would mean a control could never acknowledge a press.
    pub accent_active: Hsla,
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
}

impl gpui::Global for BrowserTheme {}

impl BrowserTheme {
    /// Total in both directions: every palette token is spent, and every field
    /// here comes from one. The two alpha derivations are the only arithmetic.
    pub fn from_palette(palette: ApplicationPalette) -> Self {
        let colour = |value: u32| -> Hsla { gpui::rgb(value).into() };
        let mut shadow = colour(palette.border);
        shadow.a = if cfg!(feature = "kobo") { 0.0 } else { 0.25 };
        let mut overlay_scrim = colour(palette.ink_text);
        overlay_scrim.a = 0.5;
        Self {
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
        }
    }
}

impl Default for BrowserTheme {
    /// The default theme in light. Only reached before the first
    /// [`apply_appearance`], which happens during startup.
    fn default() -> Self {
        Self::from_palette(platform_palette(ApplicationTheme::default(), ResolvedAppearance::Light))
    }
}

/// The active theme. Cheap: a copy of a struct of colours.
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
    let theme = BrowserTheme::from_palette(platform_palette(preferences.theme(), resolved));
    cx.set_global(SelectedPalette(preferences.theme(), resolved));
    cx.set_global(theme);

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
    application_theme.font_family = app_preferences::INTERFACE_FONT_FAMILY.into();
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
/// selected row — which is why `accent` here receives `accent_wash` and not
/// `accent`. That mapping was always right; what was wrong was reading it back
/// out afterwards under our own names.
fn apply_widget_colours(theme: BrowserTheme, application: &mut Theme) {
    let colors = &mut application.colors;
    colors.background = theme.page_bg;
    colors.foreground = theme.text;
    colors.border = theme.border;
    colors.accent = theme.accent_wash;
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
    colors.selection = theme.accent_wash;
    colors.sidebar = theme.page_bg;
    colors.sidebar_accent = theme.accent_wash;
    colors.sidebar_accent_foreground = theme.text_accent;
    colors.sidebar_border = theme.rule;
    colors.sidebar_foreground = theme.text;
    colors.sidebar_primary = theme.accent;
    colors.sidebar_primary_foreground = theme.accent_text;
    colors.title_bar = theme.page_bg;
    colors.title_bar_border = theme.rule;
    // gpui-component treats the semantic colour itself as the readable
    // foreground for alerts and derives a soft background from it. The old
    // assignment inverted these roles, rendering the pale tint as alert text.
    colors.danger = theme.danger;
    colors.danger_foreground = theme.danger_surface;
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
}

const SELECTION_ALPHA: u8 = 96;
const SEARCH_MATCH_ALPHA: u8 = 88;
const ACTIVE_SEARCH_MATCH_ALPHA: u8 = 152;
const ANNOTATION_ALPHA: u8 = 72;
const ACTIVE_ANNOTATION_ALPHA: u8 = 136;

/// A theme colour laid over the page at `alpha`.
fn highlight(colour: Hsla, alpha: u8) -> [u8; 4] {
    let rgba = gpui::Rgba::from(colour);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [channel(rgba.r), channel(rgba.g), channel(rgba.b), alpha]
}

impl InteractionHighlights {
    pub fn from_theme(theme: BrowserTheme) -> Self {
        Self {
            selection: highlight(theme.accent, SELECTION_ALPHA),
            search_match: highlight(theme.warning, SEARCH_MATCH_ALPHA),
            active_search_match: highlight(theme.warning, ACTIVE_SEARCH_MATCH_ALPHA),
            annotation: highlight(theme.warning, ANNOTATION_ALPHA),
            active_annotation: highlight(theme.warning, ACTIVE_ANNOTATION_ALPHA),
        }
    }

    #[cfg(test)]
    fn roles(self) -> [[u8; 4]; 5] {
        [self.selection, self.search_match, self.active_search_match, self.annotation, self.active_annotation]
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    use app_preferences::ApplicationTheme;

    fn highlights(theme: ApplicationTheme, appearance: ResolvedAppearance) -> InteractionHighlights {
        InteractionHighlights::from_theme(BrowserTheme::from_palette(palette(theme, appearance)))
    }

    /// The five roles have to stay tellable apart — a hit that draws like the
    /// note under it is no signal at all.
    #[test]
    fn every_interaction_role_is_distinct() {
        for theme in ApplicationTheme::ALL {
            for appearance in [ResolvedAppearance::Light, ResolvedAppearance::Dark] {
                let roles = highlights(theme, appearance).roles();
                for (index, role) in roles.iter().enumerate() {
                    for other in &roles[index + 1..] {
                        assert_ne!(role, other, "{theme:?}/{appearance:?}: two interaction roles draw identically");
                    }
                }
            }
        }
    }

    /// The regression this replaced: one set of constants for every theme, and
    /// the light ones at that.
    #[test]
    fn highlights_follow_the_theme_and_the_appearance() {
        assert_ne!(highlights(ApplicationTheme::Cool, ResolvedAppearance::Light), highlights(ApplicationTheme::Cool, ResolvedAppearance::Dark), "dark draws the light palette's highlights");
        assert_ne!(highlights(ApplicationTheme::Cool, ResolvedAppearance::Light), highlights(ApplicationTheme::Warm, ResolvedAppearance::Light), "two themes draw the same highlights");
    }

    /// Translucent, always: a highlight sits over text somebody else drew.
    #[test]
    fn no_highlight_is_opaque() {
        for role in highlights(ApplicationTheme::Warm, ResolvedAppearance::Light).roles() {
            assert!(role[3] > 0 && role[3] < 200, "alpha {} does not read as a highlight", role[3]);
        }
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;
    use app_preferences::ApplicationTheme;

    fn channel(value: f64) -> f64 {
        if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    }

    fn luminance(colour: u32) -> f64 {
        let component = |shift: u32| channel(f64::from((colour >> shift) & 0xff) / 255.0);
        0.2126 * component(16) + 0.7152 * component(8) + 0.0722 * component(0)
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    fn palettes() -> Vec<(String, ApplicationPalette)> {
        let mut out = Vec::new();
        for theme in ApplicationTheme::ALL {
            for appearance in [ResolvedAppearance::Light, ResolvedAppearance::Dark] {
                out.push((format!("{theme:?}/{appearance:?}"), palette(theme, appearance)));
            }
        }
        out
    }

    /// WCAG 1.4.11 sets 3:1 for a boundary that carries meaning. Card and panel
    /// definition rests entirely on these 1px borders, and they used to sit at
    /// 1.3:1 — visible only if you already knew where to look.
    #[test]
    fn borders_separate_from_every_surface_they_touch() {
        for (name, palette) in palettes() {
            for (surface, label) in [(palette.surface_bg, "surface_bg"), (palette.raised_bg, "raised_bg")] {
                let ratio = contrast(palette.border, surface);
                assert!(ratio >= 3.0, "{name}: border against {label} is {ratio:.2}:1, under the 3:1 floor for UI boundaries");
            }
        }
    }

    #[test]
    fn body_text_and_muted_text_pass_wcag_aa() {
        for (name, palette) in palettes() {
            for (surface, label) in [(palette.surface_bg, "surface_bg"), (palette.raised_bg, "raised_bg")] {
                let ink = contrast(palette.ink_text, surface);
                assert!(ink >= 7.0, "{name}: body text on {label} is {ink:.2}:1");
                let muted = contrast(palette.subtle_text, surface);
                assert!(muted >= 4.5, "{name}: muted text on {label} is {muted:.2}:1");
                let accent = contrast(palette.accent, surface);
                assert!(accent >= 4.5, "{name}: accent on {label} is {accent:.2}:1");
                // Warning text is set directly on a surface as often as it is on
                // its own tint — the verdict stripe and the state pills.
                let warning = contrast(palette.warning_text, surface);
                assert!(warning >= 4.5, "{name}: warning on {label} is {warning:.2}:1");
            }
            let danger = contrast(palette.danger_text, palette.danger_bg);
            assert!(danger >= 4.5, "{name}: danger text is {danger:.2}:1");
            let warning = contrast(palette.warning_text, palette.warning_bg);
            assert!(warning >= 4.5, "{name}: warning text is {warning:.2}:1");
        }
    }

    /// With one fill, a card is its edge. `rule` stopped being decoration the
    /// day the second surface was removed, so its lower bound is now what keeps
    /// a card, a popover and a panel visible at all.
    /// `rule` is the quiet tier: visible as a boundary, but never competing
    /// with `border`, which is reserved for interactive edges and hover.
    #[test]
    fn the_rule_is_visible_but_quieter_than_the_border() {
        for (name, palette) in palettes() {
            let rule = contrast(palette.rule, palette.surface_bg);
            let border = contrast(palette.border, palette.surface_bg);
            assert!(rule >= 1.4, "{name}: rule is only {rule:.2}:1 against the surface — every card edge is invisible");
            assert!(rule <= 2.2, "{name}: rule is {rule:.2}:1 — too loud for decoration");
            assert!(border > rule, "{name}: border {border:.2}:1 must outrank rule {rule:.2}:1");
        }
    }

    /// Whatever sits on a filled accent must be readable on it. This used to
    /// rely on a surface tone happening to contrast; `accent_text` is now its
    /// own value, which is what makes this an assertion rather than luck.
    #[test]
    fn accent_text_is_readable_on_the_accent() {
        for (name, palette) in palettes() {
            let ratio = contrast(palette.accent_text, palette.accent);
            assert!(ratio >= 4.5, "{name}: accent_text on accent is {ratio:.2}:1");
        }
    }

    /// Supporting text on a filled accent — the author line of a focused book
    /// card — uses `accent_wash` so it steps back from the title.
    ///
    /// Asserted for the light palettes only. In the dark ones the accent is
    /// light and `accent_wash` is a dark tint that lands close behind it, which
    /// is a known and accepted gap.
    #[test]
    fn soft_accent_text_is_readable_on_the_accent_in_light_palettes() {
        for theme in ApplicationTheme::ALL {
            let colours = palette(theme, ResolvedAppearance::Light);
            let ratio = contrast(colours.active_highlight, colours.accent);
            assert!(ratio >= 4.5, "{theme:?} light: accent_wash on accent is {ratio:.2}:1");
        }
    }

    /// A primary button has to acknowledge the pointer. These were once all
    /// aliased to `accent`, which left it visually inert on hover and press.
    #[test]
    fn the_accent_moves_under_the_pointer_and_on_press() {
        for (name, palette) in palettes() {
            let hover = contrast(palette.accent_hover, palette.accent);
            let active = contrast(palette.accent_active, palette.accent);
            assert!(hover > 1.08, "{name}: accent_hover is {hover:.2}:1 from accent — no feedback");
            assert!(active > hover, "{name}: press must read as further than hover");
            assert!(hover < 1.5, "{name}: accent_hover is {hover:.2}:1 — reads as a different colour");
        }
    }

    /// Hover is an overlay rather than a solid so one value works on every
    /// surface. A fully opaque value would mean it had been written as a solid
    /// colour by mistake, which reads as a colour change rather than a hover.
    #[test]
    fn the_hover_overlay_is_translucent() {
        for (name, palette) in palettes() {
            let alpha = palette.hover_overlay & 0xff;
            assert!(alpha > 0 && alpha < 0x40, "{name}: hover alpha {alpha:#04x} is not a subtle overlay");
        }
    }

    #[test]
    fn light_and_dark_are_actually_light_and_dark() {
        for theme in ApplicationTheme::ALL {
            let light = palette(theme, ResolvedAppearance::Light);
            let dark = palette(theme, ResolvedAppearance::Dark);
            assert!(luminance(light.surface_bg) > 0.5, "{theme:?} light background is not light");
            assert!(luminance(dark.surface_bg) < 0.1, "{theme:?} dark background is not dark");
        }
    }
}

#[cfg(all(test, feature = "kobo"))]
mod kobo_tests {
    use super::*;

    #[test]
    fn kobo_ignores_all_saved_themes_and_dark_appearance() {
        for theme in ApplicationTheme::ALL {
            for appearance in [ResolvedAppearance::Light, ResolvedAppearance::Dark] {
                assert_eq!(platform_palette(theme, appearance), kobo::PALETTE);
            }
        }
        assert_eq!(BrowserTheme::default().page_bg, gpui::rgb(0xffffff).into());
    }

    #[test]
    fn kobo_palette_is_grayscale_with_readable_control_states() {
        let p = kobo::PALETTE;
        for colour in [p.surface_bg, p.ink_text, p.subtle_text, p.accent, p.accent_hover, p.accent_active, p.accent_text, p.border, p.rule, p.hover_overlay >> 8, p.active_highlight, p.danger_text, p.danger_bg, p.warning_text, p.warning_bg]
        {
            assert_eq!((colour >> 16) & 255, (colour >> 8) & 255);
            assert_eq!((colour >> 8) & 255, colour & 255);
            assert_eq!((colour & 255) % 17, 0, "Use native 16-level grayscale values");
        }
        assert_eq!(p.ink_text, 0);
        assert_eq!(p.surface_bg, 0xffffff);
        assert!(p.subtle_text <= 0x555555);
        assert!(p.accent_active <= 0x777777);
        assert_ne!(p.active_highlight, p.surface_bg);
        assert_eq!(p.hover_overlay & 255, 255);
    }
}
