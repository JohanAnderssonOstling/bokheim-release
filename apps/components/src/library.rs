use crate::Button;
use crate::{
    BOOK_CARD_SUBTITLE_SIZE, BOOK_COVER_BADGE_INSET, BOOK_COVER_RADIUS, COMPACT_HIERARCHY_ITEM_CONTENT_GAP, COMPACT_HIERARCHY_ITEM_HORIZONTAL_PADDING, COMPACT_HIERARCHY_ITEM_TEXT_SIZE_REM, CONTENT_INSET, DOWNLOAD_ACTION_SIZE_REM,
    NavigationIcon, SPACE_MD, TEXT_LG, TEXT_MD,
};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, BoxShadow, ClickEvent, ContentMask, Corners, Div, ElementId, ImageSource, ObjectFit, Pixels, RenderImage, SharedString, Stateful, StyledImage, Window, black, canvas, div, fill, img,
    linear_color_stop, linear_gradient, point, px, size,
};
use gpui_component::spinner::Spinner;
use gpui_component::{Icon, IconName, Selectable, Sizable, box_shadow};
use library_model::FINISHED_PROGRESS_THRESHOLD;
use std::sync::Arc;
use unicode_normalization::UnicodeNormalization;

use crate::{
    BOOK_CARD_COVER_CONTENT_GAP, BOOK_CARD_TEXT_LINES, BOOK_CARD_TITLE_HEIGHT_REM, BOOK_CARD_TITLE_LINE_HEIGHT_EM, BOOK_CARD_TITLE_SIZE, BOOK_CARD_TITLE_WEIGHT, BOOK_COVER_ASPECT_RATIO, BrowserTheme, RADIUS_MD, RADIUS_SM, SPACE_SM,
    SPACE_XS, TEXT_SM, TEXT_XS,
};

/// Cover, then the text block. The two levels each have one gap: `SPACE_MD`
/// separates the art from the words, `SPACE_XS` separates the words from each
/// other. Previously the wider first gap was a margin on the title fighting the
/// column's own gap, which meant two spacing mechanisms in one component.
/// Selected cards use the accent with its contrasting text colour. The
/// keyboard cursor gets an accent outline around the entire card,
/// independently of selection.
///
/// Selected cards hover to `accent_hover` so the highlight remains visible
/// under the pointer. Plain cards use the translucent hover wash.
///
/// Disable hover while selecting a band or when a section's outer keyboard
/// frame already paints hover for the whole cell.
pub fn book_card(id: impl Into<ElementId>, focused: bool, selected: bool, hover_enabled: bool, theme: BrowserTheme) -> Stateful<Div> {
    let hovered = if selected { theme.accent_hover } else { theme.hover };
    div()
        .id(id)
        .relative()
        .w_full()
        .min_w_0()
        .flex_none()
        .flex()
        .flex_col()
        // Reserve the outline inside the existing padding so moving the
        // cursor does not resize covers or alter pagination.
        .border_2()
        .border_color(if focused || selected { theme.accent } else { gpui::transparent_black() })
        .p(px((crate::BOOK_CARD_PADDING - 2.0).max(0.0)))
        .gap(px(BOOK_CARD_COVER_CONTENT_GAP))
        .rounded(px(RADIUS_MD))
        .when(selected, |card| card.bg(theme.accent).text_color(theme.accent_text))
        .when(hover_enabled, |card| card.hover(move |style| style.bg(hovered)))
}

/// The grid cell a card sits in, at the row's uniform reserved height.
///
/// Carries no background of its own — a card's cover is boxed at its own
/// real proportions rather than the placeholder's, so `book_card` (which does
/// carry the focus tint) comes out shorter or taller than the cell. This cell
/// is plain space around it. The default `justify_end` pins grid and section
/// cards to the bottom edge; graph covers override it to align at the top.
pub fn book_card_cell() -> Div {
    div().relative().w_full().h_full().min_w_0().flex().flex_col().justify_end()
}

/// `aspect_ratio` is the cover's own width-over-height once its art is
/// decoded, so the box takes the real cover's proportions instead of every
/// book being forced into the same frame. Callers without a decoded image yet
/// pass `BOOK_COVER_ASPECT_RATIO` as a placeholder.
pub fn book_cover( aspect_ratio: f32) -> Div {
    div().relative().w_full().min_w_0().aspect_ratio(aspect_ratio)
}

pub fn book_cover_frame(theme: BrowserTheme, aspect_ratio: f32) -> Div {
    crate::clickable_surface(theme).group(BOOK_COVER_GROUP).relative().w_full().min_w_0().aspect_ratio(aspect_ratio)
}

const BOOK_COVER_GROUP: &str = "book-cover";

/// A centered transport action revealed while the pointer is over the cover.
pub fn book_cover_play_button(id: impl Into<ElementId>, theme: BrowserTheme) -> Stateful<Div> {
    book_cover_badge_plate(theme)
        .id(id)
        .absolute()
        .left(gpui::relative(0.5))
        .top(gpui::relative(0.5))
        .ml(gpui::rems(-1.25))
        .mt(gpui::rems(-1.25))
        .size(gpui::rems(2.5))
        .rounded_full()
        .cursor_pointer()
        .invisible()
        .group_hover(BOOK_COVER_GROUP, |style| style.visible())
        .hover(move |style| style.bg(theme.page_bg.blend(theme.hover)))
        .child(Icon::empty().path("icons/play-fill.svg").size(gpui::rems(1.25)).text_color(theme.text))
}

/// A compact length mark that also identifies an audiobook by its headphones.
pub fn book_cover_audiobook_length(duration_ms: Option<u64>, theme: BrowserTheme) -> Div {
    let length = match duration_ms.filter(|duration| *duration > 0) {
        Some(duration) => {
            let total_minutes = (duration / 60_000).max(1);
            match (total_minutes / 60, total_minutes % 60) {
                (0, minutes) => format!("{minutes} m"),
                (hours, 0) => format!("{hours} h"),
                (hours, minutes) => format!("{hours} h {minutes} m"),
            }
        }
        None => "Unknown".to_owned(),
    };
    div()
        .absolute()
        .left(px(8.0))
        .bottom(px(8.0))
        .flex()
        .items_center()
        .gap(px(5.0))
        .px(px(7.0))
        .py(px(5.0))
        .bg(theme.page_bg)
        .border_1()
        .border_color(theme.rule)
        .text_color(theme.text)
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(Icon::new(crate::Headphones).size(px(11.0)))
        .child(length)
}

/// Cover art scaled to fit its frame without cropping.
///
/// Scaled rather than cropped: a cover's own proportions are part of what it
/// is, and a square audiobook cover cropped to a paperback's 2:3 would lose
/// its edges rather than just sit in a taller frame than most books need.
///
/// A small neutral shadow lifts the jacket without making a larger coloured
/// rectangle around it. The outline itself stays invisible except when
/// focus or selection picks it out in the accent colour — a hover border
/// read as a stray black line rather than a lift.
/// A shaded strip down the binding edge gives the front board a little shape.
///
/// Only the outer edge rounds. A jacket's spine side is a flat fold against
/// the pages, not a curve, so rounding it along with the outer corners would
/// contradict the spine shade and hinge sitting right next to it.
pub fn book_cover_image(id: impl Into<SharedString>, source: impl Into<ImageSource>, opacity: f32, focused: bool, theme: BrowserTheme) -> AnyElement {
    cover_with_art(div().relative().size_full(), source.into(), opacity, focused, None, theme).id(ElementId::from(id.into())).into_any_element()
}

/// Loaded library cover: the clickable frame also owns the artwork and its
/// decoration, so no additional aspect-ratio or artwork div is needed.
pub fn book_cover_with_image(image: Arc<RenderImage>, aspect_ratio: f32, opacity: f32, progress: Option<f32>, theme: BrowserTheme) -> Div {
    cover_with_art(book_cover_frame(theme, aspect_ratio), image.into(), opacity, false, progress, theme)
}

fn cover_with_art(frame: Div, source: ImageSource, opacity: f32, focused: bool, progress: Option<f32>, theme: BrowserTheme) -> Div {
    let retained = match &source {
        ImageSource::Render(image) => Some(image.clone()),
        _ => None,
    };
    frame
        .overflow_hidden()
        .rounded_r(px(BOOK_COVER_RADIUS))
        .shadow(book_cover_shadow(theme, false))
        .hover(move |style| style.shadow(book_cover_shadow(theme, true)))
        .when(focused, |cover| cover.shadow(book_cover_shadow(theme, true)))
        // Padding keeps the old artwork's 1px inset without changing the
        // containing block used to anchor the frame's absolute controls.
        .p(px(1.0))
        .child(img(source).size_full().object_fit(ObjectFit::Contain).opacity(opacity).rounded_r(px(BOOK_COVER_RADIUS)))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    if !bounds.intersects(&window.content_mask().bounds) {
                        return;
                    }
                    if let Some(image) = retained {
                        // Cached cards must keep their visible pixels alive even
                        // when the shared browse cache drops its reference.
                        window.with_global_id("visible-cover-pixels".into(), |id, window| {
                            window.with_element_state(id, |_: Option<Arc<RenderImage>>, _| ((), image));
                        });
                    }
                    if !cfg!(feature = "kobo") {
                        paint_cover_binding(bounds.inset(px(1.0)), window);
                    }
                    if focused {
                        window.paint_quad(gpui::outline(bounds, theme.accent, gpui::BorderStyle::default()).corner_radii(Corners {
                            top_left: px(0.0),
                            top_right: px(BOOK_COVER_RADIUS),
                            bottom_right: px(BOOK_COVER_RADIUS),
                            bottom_left: px(0.0),
                        }));
                    }
                    if let Some(progress) = progress.filter(|progress| *progress < FINISHED_PROGRESS_THRESHOLD) {
                        paint_cover_progress(bounds, progress, theme, window);
                    }
                },
            )
            .absolute()
            .inset_0()
            .size_full(),
        )
        .when_some(progress.filter(|progress| *progress >= FINISHED_PROGRESS_THRESHOLD), |cover, progress| cover.children(book_cover_progress(progress, theme)))
}

/// The palette's general overlay shadow can be gold in dark mode. Covers
/// instead cast a short neutral shadow; the alpha still follows the theme so
/// e-ink keeps its zero-shadow treatment.
fn book_cover_shadow(theme: BrowserTheme, lifted: bool) -> Vec<BoxShadow> {
    let mut color = black();
    color.a = theme.shadow.a * if lifted { 1.2 } else { 0.8 };
    let (offset, blur) = if lifted { (4.0, 12.0) } else { (2.0, 6.0) };
    vec![box_shadow(px(0.0), px(offset), px(blur), px(0.0), color)]
}

/// Paint the spine and recessed hinge above the art without decorative divs.
fn paint_cover_binding(bounds: Bounds<Pixels>, window: &mut Window) {
    window.paint_quad(fill(Bounds::new(bounds.origin, size(px(crate::BOOK_COVER_SPINE_WIDTH), bounds.size.height)), linear_gradient(90.0, linear_color_stop(black().opacity(0.35), 0.0), linear_color_stop(gpui::transparent_black(), 1.0))));
    // Four adjacent bands retain the original five-stop hinge gradient.
    let stops = [gpui::transparent_black(), gpui::black().opacity(0.12), gpui::black().opacity(0.28), gpui::white().opacity(0.14), gpui::transparent_black()];
    for (index, pair) in stops.windows(2).enumerate() {
        let band = Bounds::new(bounds.origin + point(px(10.0 + index as f32), px(0.0)), size(px(1.0), bounds.size.height));
        window.paint_quad(fill(band, linear_gradient(90.0, linear_color_stop(pair[0], 0.0), linear_color_stop(pair[1], 1.0))));
    }
}

fn paint_cover_progress(bounds: Bounds<Pixels>, progress: f32, theme: BrowserTheme, window: &mut Window) {
    let fraction = (progress / 100.0).clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return;
    }
    let scrim = Bounds::new(point(bounds.left(), bounds.bottom() - px(crate::BOOK_COVER_PROGRESS_SCRIM_HEIGHT)), size(bounds.size.width, px(crate::BOOK_COVER_PROGRESS_SCRIM_HEIGHT)));
    if !cfg!(feature = "kobo") {
        window.paint_quad(fill(scrim, linear_gradient(0.0, linear_color_stop(black().opacity(0.6), 0.0), linear_color_stop(gpui::transparent_black(), 1.0))));
    }
    let track = Bounds::new(point(bounds.left(), bounds.bottom() - px(crate::BOOK_COVER_PROGRESS_HEIGHT)), size(bounds.size.width, px(crate::BOOK_COVER_PROGRESS_HEIGHT)));
    if cfg!(feature = "kobo") {
        window.paint_quad(fill(track, theme.page_bg));
    }
    window.paint_quad(fill(Bounds::new(track.origin, size(track.size.width * fraction, track.size.height)), theme.accent));
}

/// The reader's place on the cover: a checkmark when finished, otherwise a
/// progress bar along the bottom edge after reading has started.
///
/// The bar's scrim keeps it legible over artwork. E-ink uses a page-coloured
/// track because its black accent would disappear into the scrim.
pub fn book_cover_progress(progress: f32, theme: BrowserTheme) -> Option<AnyElement> {
    if progress >= FINISHED_PROGRESS_THRESHOLD {
        return Some(
            book_cover_badge_plate(theme)
                .absolute()
                .right(px(BOOK_COVER_BADGE_INSET))
                .bottom(px(BOOK_COVER_BADGE_INSET))
                .border_color(theme.accent)
                .bg(theme.accent)
                .text_color(theme.accent_text)
                .child(Icon::new(IconName::Check).size(gpui::rems(crate::DOWNLOAD_ICON_SIZE_REM)))
                .into_any_element(),
        );
    }
    let fraction = (progress / 100.0).clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return None;
    }
    Some(canvas(|_, _, _| (), move |bounds, _, window, _| paint_cover_progress(bounds, progress, theme, window)).absolute().inset_0().size_full().into_any_element())
}

/// Fixed book-cloth colours shared by generated covers and section headings.
/// They stay the same when the reader switches themes.
const LIBRARY_GRADIENTS: [(u32, u32); 12] = [
    (0x794e5c, 0x5b3b45), // burgundy
    (0x8b453f, 0x68342f), // brick
    (0x955341, 0x703e31), // terracotta
    (0x806335, 0x604a28), // amber
    (0x666b37, 0x4d5029), // olive
    (0x5c7045, 0x455434), // sage
    (0x48715a, 0x365544), // forest
    (0x3d7374, 0x2e5657), // teal
    (0x4b698e, 0x384f6b), // blue
    (0x566777, 0x414d59), // slate
    (0x62618a, 0x4a4968), // indigo
    (0x795278, 0x5b3e5a), // plum
];

/// A repeatable palette choice for a folder name or canonical subject path.
/// Keep the palette order and this hash fixed once colours are in use: changing
/// either would recolour existing folders and subjects.
pub fn section_card_band_index(kind: NavigationIcon, key: &str) -> usize {
    let mut hash = 0xcbf29ce484222325_u64;
    let namespace = match kind {
        NavigationIcon::Folder => b'F',
        NavigationIcon::User => b'A',
        NavigationIcon::House => b'H',
        _ => b'S',
    };
    hash = (hash ^ u64::from(namespace)).wrapping_mul(0x100000001b3);
    let normalized = key.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().nfc().collect::<String>();
    for byte in normalized.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    // Avalanche the FNV result so neighbouring names do not favour nearby
    // palette slots, including when the slot count has small factors.
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51afd7ed558ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ceb9fe1a85ec53);
    hash ^= hash >> 33;
    // Reduce before converting to usize so 32- and 64-bit devices agree.
    (hash % LIBRARY_GRADIENTS.len() as u64) as usize
}

#[cfg(not(feature = "kobo"))]
fn section_card_band_colors(index: usize) -> (gpui::Hsla, gpui::Hsla) {
    let (from, to) = LIBRARY_GRADIENTS[index % LIBRARY_GRADIENTS.len()];
    (gpui::rgb(from).into(), gpui::rgb(to).into())
}

/// Grounds for generated covers: ten hues spaced in OKLCH at alternating
/// lightness, plus indigo, charcoal, raspberry and teal. The closest pair
/// (moss and forest) is 0.065 apart in OKLab. Keep the order fixed once in
/// use: it decides which colour every coverless book gets.
#[cfg(not(feature = "kobo"))]
const COVER_GROUNDS: [u32; 14] = [
    0x461307, // oxblood
    0x503401, // bronze
    0x344e1d, // moss
    0x0d401e, // forest
    0x00291a, // racing green
    0x013741, // peacock
    0x05436c, // sapphire
    0x00183f, // navy
    0x3e315f, // amethyst
    0x3e1332, // mulberry
    0x261d57, // indigo
    0x23171a, // charcoal
    0x7b3548, // raspberry
    0x055c5c, // teal
];
#[cfg(not(feature = "kobo"))]
const COVER_GOLD: u32 = 0xc9a24a;
#[cfg(not(feature = "kobo"))]
const COVER_INK: u32 = 0xefe9dc;

/// E-ink shows 16 greys, so hues would collapse into a few near-blacks, and a
/// dark ground is the most ink to drive and to clear on every refresh. Every
/// Kobo cover is plain white paper with black lines and type; variety comes
/// from the motif and ornament instead.
#[cfg(feature = "kobo")]
const COVER_GROUNDS: [u32; 1] = [0xffffff];
#[cfg(feature = "kobo")]
const COVER_GOLD: u32 = 0x000000;
#[cfg(feature = "kobo")]
const COVER_INK: u32 = 0x000000;

#[derive(Clone, Copy)]
enum CoverOrnament {
    Bars,
    Lozenge,
    Chevrons,
}

/// The line pattern behind the frame. Rays fall from the top; chevrons,
/// lattice and hexagons cover the whole background; ziggurat and skyline stand
/// at the name plate and reach up to the title panel.
#[derive(Clone, Copy)]
enum CoverMotif {
    Rays,
    Chevrons,
    Ziggurat,
    Lattice,
    Skyline,
    Hexagons,
}

/// The art deco design a coverless book gets. Each choice reads its own digit
/// of `variant`, so they vary independently: ground (desktop only), motif and
/// ornament — 14 × 6 × 3 = 252 covers on desktop, 6 × 3 = 18 on Kobo. Keep the
/// order of every list fixed once in use: it decides which cover a book gets.
#[derive(Clone, Copy)]
struct CoverDesign {
    ground: u32,
    motif: CoverMotif,
    ornament: CoverOrnament,
}

impl CoverDesign {
    fn new(variant: u64) -> Self {
        let mut digits = variant;
        let mut digit = |radix: usize| {
            let value = digits % radix as u64;
            digits /= radix as u64;
            value as usize
        };
        let ground = COVER_GROUNDS[digit(COVER_GROUNDS.len())];
        let motif = [CoverMotif::Rays, CoverMotif::Chevrons, CoverMotif::Ziggurat, CoverMotif::Lattice, CoverMotif::Skyline, CoverMotif::Hexagons][digit(6)];
        let ornament = [CoverOrnament::Bars, CoverOrnament::Lozenge, CoverOrnament::Chevrons][digit(3)];
        Self { ground, motif, ornament }
    }
}

/// The book's own words on a generated cover; the author is already uppercase.
struct CoverLettering {
    title: SharedString,
    subtitle: Option<SharedString>,
    author: SharedString,
}

/// Shapes `text` in Raleway to wrap at `width`, keeping at most `lines` lines.
fn shape_cover_text(text: &SharedString, font: gpui::Font, color: gpui::Hsla, font_size: f32, width: f32, lines: usize, window: &Window) -> Vec<gpui::WrappedLine> {
    let run = gpui::TextRun { len: text.len(), font, color, background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_text(text.clone(), px(font_size), &[run], Some(px(width)), Some(lines)).map(|shaped| shaped.into_iter().collect()).unwrap_or_default()
}

fn cover_text_height(lines: &[gpui::WrappedLine], line_height: f32) -> f32 {
    lines.iter().map(|line| f32::from(line.size(px(line_height)).height)).sum()
}

/// Paints shaped lines centred in a column `width` wide starting at `origin`.
fn paint_cover_text(lines: &[gpui::WrappedLine], origin: gpui::Point<Pixels>, width: f32, line_height: f32, window: &mut Window, cx: &mut App) {
    let mut y = origin.y;
    for line in lines {
        let at = gpui::point(origin.x, y);
        let _ = line.paint(at, px(line_height), gpui::TextAlign::Center, Some(gpui::Bounds::new(at, size(px(width), px(line_height)))), window, cx);
        y += line.size(px(line_height)).height;
    }
}

/// The ornament above the title, hanging from `top_centre`. `unit` is one
/// percent of the cover's width, the unit every measurement here uses.
fn paint_cover_ornament(ornament: CoverOrnament, top_centre: gpui::Point<Pixels>, unit: f32, line: f32, window: &mut Window) {
    let gold = gpui::rgb(COVER_GOLD);
    let at = |x: f32, y: f32| top_centre + gpui::point(px(x * unit), px(y * unit));
    let polylines = match ornament {
        CoverOrnament::Bars => {
            for (index, length) in [16.0_f32, 11.0, 6.0].into_iter().enumerate() {
                let origin = top_centre + gpui::point(px(-length * unit / 2.0), px(index as f32 * (line + 1.5 * unit)));
                window.paint_quad(gpui::fill(gpui::Bounds::new(origin, size(px(length * unit), px(line))), gold));
            }
            return;
        }
        // A rule, a diamond between gaps, and a rule.
        CoverOrnament::Lozenge => vec![vec![at(-11.4, 2.4), at(-4.4, 2.4)], vec![at(0.0, 0.0), at(2.4, 2.4), at(0.0, 4.8), at(-2.4, 2.4), at(0.0, 0.0)], vec![at(4.4, 2.4), at(11.4, 2.4)]],
        // Two nested chevrons, the inner one lower and narrower.
        CoverOrnament::Chevrons => vec![vec![at(-8.0, 4.025), at(0.0, 0.575), at(8.0, 4.025)], vec![at(-4.8, 4.6), at(0.0, 2.444), at(4.8, 4.6)]],
    };
    let mut builder = gpui::PathBuilder::stroke(px(line));
    for polyline in polylines {
        builder.add_polygon(&polyline, false);
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, gold);
    }
}

/// The motif inside the frame, as one stroked path. `panel_bottom` and
/// `plate_top` are the title panel's lower edge and the name plate's upper
/// edge, from the top of the cover: ziggurat and skyline stand between them.
///
/// On Kobo the lines are black hairlines one device pixel wide. Library page
/// turns use the A2 waveform, which thresholds every pixel at 160, so a grey
/// motif would vanish on each swipe and reappear at the next greyscale
/// cleanup; black looks the same under both. On desktop they are
/// half-strength gold, one logical pixel wide.
fn paint_cover_pattern(design: CoverDesign, bounds: gpui::Bounds<Pixels>, line: f32, panel_bottom: f32, plate_top: f32, window: &mut Window) {
    #[cfg(feature = "kobo")]
    let (stroke, colour): (f32, gpui::Hsla) = (1.0 / window.scale_factor(), gpui::rgb(COVER_INK).into());
    #[cfg(not(feature = "kobo"))]
    let (stroke, colour): (f32, gpui::Hsla) = (1.0, gpui::Hsla::from(gpui::rgb(COVER_GOLD)).opacity(0.5));
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let across = |percent: f32| width * percent / 100.0;
    let (left, right) = (across(4.0) + line, width - across(4.0) - line);
    let (top, bottom) = (height * 0.04 + line, height * 0.96 - line);
    let centre = width / 2.0;
    let at = |x: f32, y: f32| bounds.origin + gpui::point(px(x), px(y));
    let inside = gpui::Bounds::new(at(left, top), size(px(right - left), px(bottom - top)));
    window.with_content_mask(Some(gpui::ContentMask { bounds: inside }), |window| {
        let mut builder = gpui::PathBuilder::stroke(px(stroke));
        let mut segment = |from: (f32, f32), to: (f32, f32)| builder.add_polygon(&[at(from.0, from.1), at(to.0, to.1)], false);
        match design.motif {
            // Lines fanning down from just above the cover, 5° apart.
            CoverMotif::Rays => {
                let (origin, length) = (-height * 0.04, height * 1.2);
                let mut angle: f32 = 0.4;
                while angle < 180.0 {
                    let direction = angle.to_radians();
                    segment((centre, origin), (centre + length * direction.cos(), origin + length * direction.sin()));
                    angle += 5.0;
                }
            }
            // Stacked chevrons pointing up, laid out from the top so the first
            // one's point sits on the frame instead of being clipped through.
            CoverMotif::Chevrons => {
                let mut apex = top;
                while apex < bottom {
                    builder.add_polygon(&[at(left, apex + across(14.0)), at(centre, apex), at(right, apex + across(14.0))], false);
                    apex += across(6.0);
                }
            }
            // A stepped pyramid as wide as the name plate, standing on it with
            // its flat top meeting the title panel, drawn like contour lines:
            // each inner outline is the outer one moved in and down by the
            // same distance, so gaps stay equal and treads keep their width.
            // A tier whose inset height falls below the base is skipped and a
            // tier whose treads would meet becomes the top, so the outlines
            // carry on until the whole pyramid is filled.
            CoverMotif::Ziggurat => {
                let tiers = 5;
                let rise = (plate_top - panel_bottom) / tiers as f32;
                let (start, end) = (across(14.0), width - across(14.0));
                let tread = (end - start) / (2 * tiers + 4) as f32;
                let inset = across(3.0);
                for nest in 0..200 {
                    if rise <= 0.0 {
                        break;
                    }
                    let offset = nest as f32 * inset;
                    let (mut rising, mut falling) = (Vec::new(), Vec::new());
                    for tier in 1..=tiers {
                        let y = plate_top - tier as f32 * rise + offset;
                        if y >= plate_top {
                            continue;
                        }
                        let (riser_left, riser_right) = (start + (tier - 1) as f32 * tread + offset, end - (tier - 1) as f32 * tread - offset);
                        if riser_right - riser_left < inset {
                            break;
                        }
                        if rising.is_empty() {
                            rising.push(at(riser_left, plate_top));
                            falling.push(at(riser_right, plate_top));
                        }
                        rising.push(at(riser_left, y));
                        falling.push(at(riser_right, y));
                        let (tread_left, tread_right) = (start + tier as f32 * tread + offset, end - tier as f32 * tread - offset);
                        if tread_right - tread_left < inset {
                            break;
                        }
                        rising.push(at(tread_left, y));
                        falling.push(at(tread_right, y));
                    }
                    if rising.is_empty() {
                        break;
                    }
                    falling.reverse();
                    rising.extend(falling);
                    builder.add_polygon(&rising, false);
                }
            }
            // A diamond lattice of continuous diagonals.
            CoverMotif::Lattice => {
                let span = bottom - top;
                let mut offset = left - span;
                while offset < right + span {
                    segment((offset, bottom), (offset + span, top));
                    segment((offset, bottom), (offset - span, top));
                    offset += across(9.0);
                }
            }
            // Pinstripes in mirrored pairs around one centre line, which alone
            // reaches the title panel. The spacing, about 4% of the width,
            // divides the half width exactly, so the frame stands one space
            // from the outermost line; each pair is 3.5% of the height shorter,
            // so the steps keep their proportion on any cover.
            CoverMotif::Skyline => {
                let half_width = (right - left) / 2.0;
                let pairs = ((half_width / across(4.0)).round() as i32 - 1).max(0);
                let spacing = half_width / (pairs + 1) as f32;
                let tallest = bottom - panel_bottom;
                for pair in 0..=pairs {
                    let tall = (tallest - pair as f32 * height * 0.035).max(0.0);
                    let x = pair as f32 * spacing;
                    segment((centre + x, bottom), (centre + x, bottom - tall));
                    if pair > 0 {
                        segment((centre - x, bottom), (centre - x, bottom - tall));
                    }
                }
            }
            // A honeycomb of flat-topped hexagons centred on the middle column.
            CoverMotif::Hexagons => {
                let radius = across(5.0);
                let (row_step, column_step) = (3.0_f32.sqrt() * radius, 1.5 * radius);
                let mut column = -((centre - left) / column_step).ceil() as i32 - 1;
                while column as f32 * column_step < right - centre + column_step {
                    let x = centre + column as f32 * column_step;
                    let mut y = top - row_step + if column % 2 != 0 { row_step / 2.0 } else { 0.0 };
                    while y < bottom + row_step {
                        let corners: Vec<_> = (0..6).map(|side| side as f32 * std::f32::consts::PI / 3.0).map(|angle| at(x + radius * angle.cos(), y + radius * angle.sin())).collect();
                        builder.add_polygon(&corners, true);
                        y += row_step;
                    }
                    column += 1;
                }
            }
        }
        if let Ok(path) = builder.build() {
            window.paint_path(path, colour);
        }
    });
}

/// Frame, motif, title panel and name plate, painted straight into `bounds`;
/// the title is always on top and the author at the bottom. Measurements
/// follow the cover's width like the HTML mock's `cqw` and positions follow
/// its height, so a square audiobook cover keeps the same proportions as a
/// book's. The ground is the host's background.
fn paint_generated_cover(design: CoverDesign, bounds: gpui::Bounds<Pixels>, lettering: Option<&CoverLettering>, window: &mut Window, cx: &mut App) {
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let across = |percent: f32| width * percent / 100.0;
    let down = |percent: f32| height * percent / 100.0;
    let line = (width * 0.0045).max(1.0);
    let gold: gpui::Hsla = gpui::rgb(COVER_GOLD).into();
    let ground: gpui::Hsla = gpui::rgb(design.ground).into();
    let ink: gpui::Hsla = gpui::rgb(COVER_INK).into();
    let rect = |x: f32, y: f32, w: f32, h: f32| gpui::Bounds::new(bounds.origin + gpui::point(px(x), px(y)), size(px(w), px(h)));
    let ruled = |area: gpui::Bounds<Pixels>, fill: gpui::Hsla| gpui::quad(area, px(0.0), fill, px(line), gold, gpui::BorderStyle::Solid);

    window.paint_quad(ruled(rect(across(4.0), down(4.0), width - 2.0 * across(4.0), height - 2.0 * down(4.0)), gpui::transparent_black()));
    let Some(lettering) = lettering else {
        // Unlettered: the motif alone, with room where the panels would be.
        paint_cover_pattern(design, bounds, line, down(40.0), height - down(9.0), window);
        return;
    };
    let raleway = gpui::font("Raleway");

    // The title panel: a ruled box with a second rule standing off it. Both
    // are filled, so the pattern stops at the outer rule.
    let (stand_off, pad_x, pad_top, pad_bottom, gap) = (across(1.4), across(6.0), across(7.0), across(8.0), across(3.0));
    let outer_x = across(10.6);
    let outer_width = width - 2.0 * outer_x;
    let inner_x = outer_x + line + stand_off;
    let inner_width = outer_width - 2.0 * (line + stand_off);
    let text_x = inner_x + line + pad_x;
    let text_width = inner_width - 2.0 * (line + pad_x);
    let (title_size, subtitle_size) = (across(8.4), across(4.6));
    let (title_line, subtitle_line) = (title_size * 1.18, subtitle_size * 1.25);
    let title = shape_cover_text(&lettering.title, raleway.clone(), ink, title_size, text_width, 5, window);
    let italic = gpui::Font { style: gpui::FontStyle::Italic, ..raleway.clone() };
    // Softened on desktop; solid black on Kobo, where grey thresholds away on page turns.
    let subtitle_colour = if cfg!(feature = "kobo") { ink } else { ink.opacity(0.8) };
    let subtitle = lettering.subtitle.as_ref().map(|subtitle| shape_cover_text(subtitle, italic, subtitle_colour, subtitle_size, text_width, 2, window)).unwrap_or_default();
    let ornament_height = match design.ornament {
        CoverOrnament::Bars => 3.0 * line + 2.0 * across(1.5),
        CoverOrnament::Lozenge => across(4.8),
        CoverOrnament::Chevrons => across(4.6),
    };
    let (title_height, subtitle_height) = (cover_text_height(&title, title_line), cover_text_height(&subtitle, subtitle_line));
    let content_height = ornament_height + gap + title_height + if subtitle.is_empty() { 0.0 } else { gap + subtitle_height };
    let inner_height = 2.0 * line + pad_top + content_height + pad_bottom;
    let outer_height = inner_height + 2.0 * (line + stand_off);
    let outer_y = down(12.0);
    let inner_y = outer_y + line + stand_off;

    // The name plate is laid out before anything is painted: the motif goes
    // underneath both panels, and the ziggurat stands on the plate. Long
    // names wrap onto a second line rather than being cut off.
    let (plate_x, pad) = (across(14.0), across(2.0));
    let plate_width = width - 2.0 * plate_x;
    let author_width = plate_width - 2.0 * (line + pad);
    let author_size = across(3.3).max(5.0);
    let author_line = author_size * 1.35;
    let bold = gpui::Font { weight: gpui::FontWeight::BOLD, ..raleway };
    // Up to four lines: a book with several authors wraps rather than being cut off.
    let author = shape_cover_text(&lettering.author, bold, ink, author_size, author_width, 4, window);
    let plate_height = 2.0 * line + across(2.4) + cover_text_height(&author, author_line) + pad;
    let plate_y = height - down(9.0) - plate_height;
    let plate_top = if author.is_empty() { height - down(9.0) } else { plate_y };
    paint_cover_pattern(design, bounds, line, outer_y + outer_height, plate_top, window);

    window.paint_quad(ruled(rect(outer_x, outer_y, outer_width, outer_height), ground));
    window.paint_quad(ruled(rect(inner_x, inner_y, inner_width, inner_height), ground));
    let mut y = inner_y + line + pad_top;
    paint_cover_ornament(design.ornament, bounds.origin + gpui::point(px(width / 2.0), px(y)), across(1.0), line, window);
    y += ornament_height + gap;
    paint_cover_text(&title, bounds.origin + gpui::point(px(text_x), px(y)), text_width, title_line, window, cx);
    y += title_height + gap;
    paint_cover_text(&subtitle, bounds.origin + gpui::point(px(text_x), px(y)), text_width, subtitle_line, window, cx);

    if author.is_empty() {
        return;
    }
    window.paint_quad(ruled(rect(plate_x, plate_y, plate_width, plate_height), ground));
    paint_cover_text(&author, bounds.origin + gpui::point(px(plate_x + line + pad), px(plate_y + line + across(2.4))), author_width, author_line, window, cx);
}

/// The generated jacket: one element whose canvas paints the whole design at
/// the cover's real size, so the type scales with the card. Always 1.6 times
/// as tall as it is wide, audiobooks included.
fn generated_cover(frame: Div, design: CoverDesign, lettering: Option<CoverLettering>) -> Div {
    frame.child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                // Clip only the art; controls attached to the frame retain
                // their own hit areas, including the larger touch targets.
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    window.paint_quad(fill(bounds, gpui::rgb(design.ground)).corner_radii(Corners { top_left: px(0.0), top_right: px(BOOK_COVER_RADIUS), bottom_right: px(BOOK_COVER_RADIUS), bottom_left: px(0.0) }));
                    paint_generated_cover(design, bounds, lettering.as_ref(), window, cx);
                });
            },
        )
        .absolute()
        .inset_0()
        .size_full(),
    )
}

/// The generated jacket without its text, for a cover too small to letter.
pub fn book_thumbnail_generated(variant: u64) -> Div {
    generated_cover(book_cover( BOOK_COVER_ASPECT_RATIO).overflow_hidden().rounded_r(px(BOOK_COVER_RADIUS)), CoverDesign::new(variant), None)
}

/// A cover for a book with no available image, also shown while its image loads.
/// Cards and detail pages use it regardless of the cover-text setting.
///
/// `variant` picks the design — pass something stable per book (its content
/// hash or source-entry identity) so the same book always generates the same
/// cover. Painted directly from quads, shaped text and paths, nothing
/// rasterized, so no cover keeps a bitmap in memory.
pub fn book_cover_generated(title: impl Into<SharedString>, subtitle: Option<&str>, author: &str, variant: u64) -> Div {
    generated_cover(book_cover( BOOK_COVER_ASPECT_RATIO).overflow_hidden().rounded_r(px(BOOK_COVER_RADIUS)), CoverDesign::new(variant), Some(cover_lettering(title, subtitle, author)))
}

/// A clickable generated cover, with its art painted directly in the frame.
pub fn book_cover_with_generated_art(title: impl Into<SharedString>, subtitle: Option<&str>, author: &str, variant: u64, theme: BrowserTheme) -> Div {
    generated_cover(book_cover_frame(theme, BOOK_COVER_ASPECT_RATIO), CoverDesign::new(variant), Some(cover_lettering(title, subtitle, author)))
}

fn cover_lettering(title: impl Into<SharedString>, subtitle: Option<&str>, author: &str) -> CoverLettering {
    let subtitle = subtitle.map(str::trim).filter(|value| !value.is_empty()).map(SharedString::from);
    let author = SharedString::from(author.trim().to_uppercase());
    CoverLettering { title: title.into(), subtitle, author }
}

/// No colour of its own: the title inherits from the card, which is what turns
/// it to `accent_text` when the cell fills with the accent.
///
/// Takes only the lines it needs rather than reserving the full
/// title-and-subtitle height itself — `book_card_title_row` reserves that
/// budget on this same container when used below a cover.
pub fn book_card_title() -> Div {
    div().w_full().min_w_0().flex_none().text_size(gpui::rems(BOOK_CARD_TITLE_SIZE)).line_height(gpui::relative(BOOK_CARD_TITLE_LINE_HEIGHT_EM)).font_weight(BOOK_CARD_TITLE_WEIGHT)
}

/// Share three lines, giving the title priority while reserving one subtitle
/// line. Measure at the actual card width so a one-line title leaves two
/// subtitle lines, including after a font or window-size change.
pub fn book_title_lines(title: impl Into<SharedString>, subtitle: Option<&str>) -> impl IntoElement {
    let title = title.into();
    let subtitle = subtitle.map(str::trim).filter(|value| !value.is_empty()).map(SharedString::from);
    canvas(
        move |bounds, window, cx| {
            let mut content = book_title_content(title, subtitle, bounds.size.width, window, cx).into_any_element();
            content.prepaint_as_root(bounds.origin, size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::MaxContent), window, cx);
            content
        },
        |_, mut content, window, cx| content.paint(window, cx),
    )
    .w_full()
    .h(gpui::rems(BOOK_CARD_TITLE_HEIGHT_REM))
}

fn book_title_content(title: SharedString, subtitle: Option<SharedString>, width: Pixels, window: &mut Window, cx: &mut App) -> Div {
    let has_title = !title.trim().is_empty();
    let title_limit = BOOK_CARD_TEXT_LINES - usize::from(subtitle.is_some());
    let title_element = || div().w_full().min_w_0().flex_none().text_ellipsis().line_clamp(title_limit).child(title.clone());
    // The line count only determines how much space a subtitle receives.
    let title_lines = if has_title && subtitle.is_some() {
        let mut measured = title_element().into_any_element();
        let height = measured.layout_as_root(size(AvailableSpace::Definite(width), AvailableSpace::MaxContent), window, cx).height;
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.pixel_snap(style.line_height.to_pixels(font_size.into(), window.rem_size()));
        (f32::from(height) / f32::from(line_height)).round().max(1.0) as usize
    } else {
        0
    };
    div().w_full().min_w_0().flex().flex_col().when(has_title, |content| content.child(title_element())).children(subtitle.map(|subtitle| {
        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .text_size(gpui::rems(BOOK_CARD_SUBTITLE_SIZE))
            .line_height(gpui::relative(BOOK_CARD_TITLE_LINE_HEIGHT_EM))
            .font_weight(gpui::FontWeight::NORMAL)
            .text_ellipsis()
            .line_clamp(BOOK_CARD_TEXT_LINES.saturating_sub(title_lines).max(1))
            .child(subtitle)
    }))
}

#[cfg(test)]
mod book_title_tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext};

    struct TitleBudget;

    impl Render for TitleBudget {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            book_card_title().child(canvas(
                |_, window, cx| {
                    let long = "A long book title or subtitle that needs many lines of text ".repeat(12);
                    for rem in [16.0, 24.0] {
                        window.set_rem_size(px(rem));
                        let title_height = window.pixel_snap(px(rem * BOOK_CARD_TITLE_SIZE * BOOK_CARD_TITLE_LINE_HEIGHT_EM));
                        let subtitle_height = window.pixel_snap(px(rem * BOOK_CARD_SUBTITLE_SIZE * BOOK_CARD_TITLE_LINE_HEIGHT_EM));
                        for width in [px(120.0), px(200.0)] {
                            for (title, subtitle, title_lines, subtitle_lines) in [
                                (long.as_str(), None, 3, 0),
                                (long.as_str(), Some(long.as_str()), 2, 1),
                                ("Book", Some(long.as_str()), 1, 2),
                                ("", Some(long.as_str()), 0, 3),
                                ("   ", Some(long.as_str()), 0, 3),
                                (long.as_str(), Some("Brief"), 2, 1),
                                ("Book", None, 1, 0),
                                ("", None, 0, 0),
                            ] {
                                let mut content = book_title_content(title.into(), subtitle.map(SharedString::from), width, window, cx).into_any_element();
                                let measured = content.layout_as_root(size(AvailableSpace::Definite(width), AvailableSpace::MaxContent), window, cx);
                                assert_eq!(measured.height, title_height * title_lines as f32 + subtitle_height * subtitle_lines as f32, "title={title:?}, subtitle={subtitle:?}, width={width:?}, rem={rem}");
                                let mut row = book_card_title_row(title, subtitle).into_any_element();
                                let measured = row.layout_as_root(size(AvailableSpace::Definite(width), AvailableSpace::MaxContent), window, cx);
                                assert_eq!(measured, size(width, px(rem * BOOK_CARD_TITLE_HEIGHT_REM)), "short or blank titles must retain the full cell budget");
                            }
                        }
                    }
                },
                |_, _, _, _| {},
            ))
        }
    }

    #[gpui::test]
    fn book_titles_share_three_lines_at_different_widths_and_font_sizes(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| TitleBudget);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

/// A card's title row: title and subtitle across the card's whole width.
///
/// The download control used to sit beside them, which took width from only
/// the cards whose books were not on the device — so titles in one row wrapped
/// at different places. It is on the cover now; see
/// [`book_cover_download_badge`].
/// Reserves the paginator's full title budget even when the text is shorter.
pub fn book_card_title_row(title: impl Into<SharedString>, subtitle: Option<&str>) -> Div {
    book_card_title().h(gpui::rems(BOOK_CARD_TITLE_HEIGHT_REM)).child(book_title_lines(title, subtitle))
}

/// The mark of a book that is not on this device, in its cover's top-right
/// corner — the one the progress line, the audiobook tag and the spine leave
/// free — and the control that fetches it.
///
/// An opaque theme surface and a contrasting glyph keep the control readable
/// over any cover art, including black ink on white paper on Kobo. `pending`
/// swaps the glyph for a spinner in the same place, so nothing moves when the mark is pressed; the
/// caller only makes the idle mark clickable.
pub fn book_cover_download_badge(id: impl Into<ElementId>, pending: bool, theme: BrowserTheme) -> Stateful<Div> {
    book_cover_action_badge(id, pending, download_glyph(), theme)
}

/// Waiting for transfer policy or another download requires no animation.
pub fn book_cover_queued_badge(id: impl Into<ElementId>, theme: BrowserTheme) -> Stateful<Div> {
    book_cover_badge(id, Icon::empty().path("icons/hourglass.svg").size(gpui::rems(crate::DOWNLOAD_ICON_SIZE_REM)).text_color(theme.text), false, theme)
}

/// The same corner for a catalog book: adding it to the library, which
/// downloads it.
pub fn book_cover_add_badge(id: impl Into<ElementId>, pending: bool, theme: BrowserTheme) -> Stateful<Div> {
    book_cover_action_badge(id, pending, download_glyph(), theme)
}

/// The same corner for a feed book its feed sells: opening its store page.
pub fn book_cover_buy_badge(id: impl Into<ElementId>, theme: BrowserTheme) -> Stateful<Div> {
    book_cover_action_badge(id, false, buy_glyph().size(gpui::rems(crate::DOWNLOAD_ICON_SIZE_REM)), theme)
}

/// The same corner for a catalog book the library already holds: a mark,
/// not an action.
pub fn book_cover_in_library_badge(id: impl Into<ElementId>, theme: BrowserTheme) -> Stateful<Div> {
    book_cover_badge(id, Icon::new(IconName::Check).size(gpui::rems(crate::DOWNLOAD_ICON_SIZE_REM)).text_color(theme.text), false, theme)
}

fn book_cover_action_badge(id: impl Into<ElementId>, pending: bool, glyph: Icon, theme: BrowserTheme) -> Stateful<Div> {
    let content = if pending { Spinner::new().icon(IconName::LoaderCircle).xsmall().color(theme.text).into_any_element() } else { glyph.text_color(theme.text).into_any_element() };
    book_cover_badge(id, content, !pending, theme)
}

const BOOK_COVER_BADGE_GROUP: &str = "book-cover-badge";

/// The pointer target is the plate itself. Touch keeps a larger transparent
/// target around the same inset plate, so it needs its own layout container.
fn book_cover_badge(id: impl Into<ElementId>, content: impl IntoElement, hover_enabled: bool, theme: BrowserTheme) -> Stateful<Div> {
    let plate = book_cover_badge_plate(theme).child(content);
    let plate_hover = theme.page_bg.blend(theme.hover);
    if crate::touch_ui() {
        div()
            .id(id)
            .group(BOOK_COVER_BADGE_GROUP)
            .absolute()
            .flex()
            .items_start()
            .justify_end()
            .top_0()
            .right_0()
            .min_w(gpui::rems(crate::TOUCH_TARGET_REM))
            .min_h(gpui::rems(crate::TOUCH_TARGET_REM))
            .p(px(BOOK_COVER_BADGE_INSET))
            .when(hover_enabled, |badge| badge.cursor_pointer())
            .child(plate.when(hover_enabled, |plate| plate.group_hover(BOOK_COVER_BADGE_GROUP, move |style| style.bg(plate_hover))))
    } else {
        plate.id(id).group(BOOK_COVER_BADGE_GROUP).absolute().top(px(BOOK_COVER_BADGE_INSET)).right(px(BOOK_COVER_BADGE_INSET)).when(hover_enabled, |badge| badge.cursor_pointer().hover(move |style| style.bg(plate_hover)))
    }
}

fn book_cover_badge_plate(theme: BrowserTheme) -> Div {
    div().size(gpui::rems(DOWNLOAD_ACTION_SIZE_REM)).flex().items_center().justify_center().rounded(px(RADIUS_SM)).border_1().border_color(theme.rule).bg(theme.page_bg).text_color(theme.text)
}

/// The cloud-and-arrow glyph every download action carries, on a book's cover
/// and on a folder or subject chip alike.
pub fn download_glyph() -> Icon {
    Icon::empty().path("icons/cloud-download.svg").size(gpui::rems(crate::DOWNLOAD_ICON_SIZE_REM))
}

/// Buying a book at its store.
pub fn buy_glyph() -> Icon {
    Icon::empty().path("icons/shopping-bag.svg")
}

/// The pill that follows the pointer while items are being dragged. It names
/// what is being carried rather than miniaturising it: a half-transparent copy
/// of three book covers reads as a rendering fault, and a count does not.
pub fn drag_preview(label: SharedString, theme: BrowserTheme) -> Div {
    div().px(px(SPACE_SM)).h(px(28.0)).flex().items_center().rounded(px(RADIUS_SM)).bg(theme.accent).text_color(theme.accent_text).text_size(gpui::rems(TEXT_XS)).child(label)
}

/// The folder trail in the topbar: the root, then the path, on one line, folded
/// to the width the topbar leaves it — whole crumbs into one "…", from the
/// middle out, the current folder never (see [`fold_trail`](crate::fold_trail)).
/// Names are never cut: a cut name named nothing.
///
/// Should even "… › current" not fit, the start is clipped under the topbar's
/// edge rather than the folder the trail is for.
///
/// `labels` are the crumbs' names, root first, for the fold's tooltip.
/// Crumb padding is the only space in the trail — there is no gap — so the
/// label, its hit area and its distance from the chevron move together: change
/// it in [`folder_breadcrumb_item`].
pub fn folder_breadcrumb(crumbs: Vec<Stateful<Div>>, labels: Vec<SharedString>, theme: BrowserTheme) -> crate::FoldedTrail {
    let crumbs = crumbs.into_iter().map(IntoElement::into_any_element).collect();
    crate::folded_trail("folder-breadcrumb", crumbs, move || folder_breadcrumb_separator(theme).into_any_element(), move |folded| folder_breadcrumb_fold(&labels, folded, theme).into_any_element()).keep_end().w_full()
}

/// The chevron between two crumbs, at the size and colour the toolkit's
/// breadcrumb drew it.
fn folder_breadcrumb_separator(theme: BrowserTheme) -> Icon {
    Icon::new(IconName::ChevronRight).text_color(theme.text_muted).size_3p5()
}

/// The folded crumbs, as one "…" that names them when the pointer rests on it.
/// When the root is among them it takes the root's inset.
fn folder_breadcrumb_fold(labels: &[SharedString], folded: std::ops::Range<usize>, theme: BrowserTheme) -> Stateful<Div> {
    let hidden = SharedString::from(labels[folded.clone()].iter().map(|label| label.as_ref()).collect::<Vec<&str>>().join(" › "));
    let fold =
        div().id("folder-breadcrumb-fold").flex_none().whitespace_nowrap().text_size(gpui::rems(TEXT_MD)).px(px(SPACE_XS)).py(px(SPACE_XS)).text_color(theme.text_muted).when(folded.start == 0, |fold| fold.pl(px(CONTENT_INSET))).child("…");
    if hidden.is_empty() {
        return fold;
    }
    fold.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(hidden.clone()).build(window, cx))
}

/// The trail recedes and the current folder does not: ancestors are muted, the
/// last crumb keeps full ink. Weight used to carry this, but the interface has
/// no semibold face — see the type scale in `tokens.rs`.
///
/// The horizontal padding is `SPACE_XS` rather than the `CONTENT_INSET` the
/// rest of the browser is inset by, and it is the only thing between a label
/// and its chevron — the trail has no gap. A chevron is a separator, not a
/// neighbour: it belongs to the pair of crumbs around it, and at the inset it
/// reads as an item of its own.
///
/// `index` is the crumb's place in the trail, root first. Every crumb but
/// the current one leads somewhere, which the page attaches.
pub fn folder_breadcrumb_item(index: usize, label: impl Into<SharedString>, current: bool, theme: BrowserTheme) -> Stateful<Div> {
    // One face for the whole trail: the display face at this size read thinner
    // than the interface face beside it, so the current folder looked lighter
    // than its ancestors. Ink against muted carries "current" on its own.
    div()
        .id(("folder-breadcrumb-crumb", index))
        .flex_none()
        .whitespace_nowrap()
        .text_size(gpui::rems(TEXT_MD))
        .px(px(SPACE_XS))
        .py(px(SPACE_XS))
        .text_color(if current { theme.text } else { theme.text_muted })
        .when(!current, |crumb| crumb.cursor_pointer())
        .child(label.into())
}

/// The first crumb, set on the left edge of the covers below it.
///
/// Nothing above it carries a horizontal inset — the topbar and the column it
/// sits in are both unpadded — so this padding is the whole of the distance
/// between the rail and the trail, and `CONTENT_INSET` is where a book cover
/// starts: the card grid is flush with the column and the cover sits one
/// `BOOK_CARD_PADDING` inside its card. The two are the same token for exactly
/// this reason. Only the leading edge takes it; the trailing edge keeps the
/// tighter padding the chevrons are spaced on.
pub fn folder_breadcrumb_root_item(label: impl Into<SharedString>, current: bool, theme: BrowserTheme) -> Stateful<Div> {
    folder_breadcrumb_item(0, label, current, theme).pl(px(CONTENT_INSET))
}

/// The content that determines a hierarchy item's natural width.
#[derive(Clone)]
pub struct HierarchyItemWidth {
    pub icon: NavigationIcon,
    pub label: SharedString,
    pub count: Option<usize>,
    pub show_download: bool,
}

impl HierarchyItemWidth {
    /// Uses the rendered chip's text and icon layout so font changes affect
    /// wrapping, including the action slot only when it is shown.
    pub fn measure(&self, window: &mut Window, cx: &mut App) -> Pixels {
        let mut content = div()
            .flex()
            .items_center()
            .whitespace_nowrap()
            .text_size(gpui::rems(COMPACT_HIERARCHY_ITEM_TEXT_SIZE_REM))
            .gap(px(COMPACT_HIERARCHY_ITEM_CONTENT_GAP))
            .child(crate::navigation_icon(self.icon))
            .child(div().flex_none().child(self.label.clone()))
            .children(self.count.map(|count| div().flex_none().child(count.to_string())))
            .when(self.show_download, |content| content.child(div().w(gpui::rems(DOWNLOAD_ACTION_SIZE_REM)).flex_none()))
            .into_any_element();
        let measured = content.layout_as_root(gpui::size(gpui::AvailableSpace::MaxContent, gpui::AvailableSpace::MaxContent), window, cx);
        measured.width + px(COMPACT_HIERARCHY_ITEM_HORIZONTAL_PADDING * 2.0 + 2.0)
    }
}

/// A compact hierarchy item for mixed paginator pages.
///
/// This function owns presentation only. Feature code remains responsible for
/// attaching click handlers and navigation behavior.
pub fn compact_hierarchy_item(
    id: impl Into<ElementId>, icon: NavigationIcon, label: impl Into<SharedString>, path: Option<SharedString>, count: Option<usize>, download: Option<AnyElement>, selected: bool, cursor: bool, marquee: bool, theme: BrowserTheme,
) -> Button {
    let label = label.into();
    let text = hierarchy_item_text(label, path, theme);
    // A selected chip and the chip under the keyboard cursor are both the
    // keyboard's tier, `accent_wash`, and keep every foreground they had; see
    // the ladder in `tokens.rs`. The pointer lifts a washed chip to
    // `accent_wash_hover` and a plain one to the light `hover`.
    let washed = selected || cursor;
    let hovered = if washed { theme.accent_wash_hover } else { theme.hover };
    let base = if washed { theme.accent_wash } else { theme.page_bg };
    let button = crate::base_button(id).ghost().selected(selected).bg(base).text_color(theme.text).hover(move |style| if marquee { style.bg(base) } else { style.bg(hovered) });
    button
        .w_full()
        .max_w_full()
        .min_w_0()
        .h_full()
        .p_0()
        .px(px(COMPACT_HIERARCHY_ITEM_HORIZONTAL_PADDING))
        .text_size(gpui::rems(COMPACT_HIERARCHY_ITEM_TEXT_SIZE_REM))
        .gap(px(COMPACT_HIERARCHY_ITEM_CONTENT_GAP))
        .rounded(px(0.0))
        .border_1()
        .border_color(theme.rule)
        .child(crate::navigation_icon(icon).text_color(theme.text_accent))
        .child(text)
        .children(count.map(|count| div().flex_none().text_color(theme.text_muted).child(count.to_string())))
        .when_some(download, |content, download| content.child(div().size(gpui::rems(DOWNLOAD_ACTION_SIZE_REM)).flex_none().flex().items_center().justify_center().child(download)))
}

fn hierarchy_item_text(label: SharedString, path: Option<SharedString>, theme: BrowserTheme) -> Div {
    // Stretch text across a column so it receives a definite truncation width;
    // a row can keep the text's intrinsic width even when its parent shrinks.
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .justify_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(label)
        .children(path.map(|path| div().absolute().top_0().left_0().right_0().h(gpui::relative(0.5)).min_w_0().flex().flex_col().justify_center().text_ellipsis_start().text_size(gpui::rems(0.5)).text_color(theme.text_muted).child(path)))
}

#[cfg(test)]
mod hierarchy_item_tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, rems};
    use std::{cell::RefCell, rc::Rc};

    type MeasuredBounds = Rc<RefCell<Vec<Bounds<Pixels>>>>;

    struct TextFixture {
        width: f32,
        rem: f32,
        path: Option<SharedString>,
        regions: MeasuredBounds,
        text: MeasuredBounds,
    }

    impl Render for TextFixture {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(px(self.rem));
            let regions = self.regions.clone();
            let text = self.text.clone();
            div()
                .w(px(self.width))
                .h(px(40.0))
                .flex()
                .text_size(rems(COMPACT_HIERARCHY_ITEM_TEXT_SIZE_REM))
                .on_children_prepainted(move |bounds, _, _| *regions.borrow_mut() = bounds)
                .child(hierarchy_item_text("A very long folder label".into(), self.path.clone(), BrowserTheme::default()).on_children_prepainted(move |bounds, _, _| *text.borrow_mut() = bounds))
                .child(div().w(px(30.0)).h_full().flex_none())
        }
    }

    #[gpui::test]
    fn narrow_hierarchy_text_preserves_clipping_alignment_and_action_space(cx: &mut TestAppContext) {
        for width in [70.0, 180.0] {
            for rem in [16.0, 24.0] {
                for path in [None, Some(SharedString::from("A library/a parent folder/a long path"))] {
                    let regions: MeasuredBounds = Default::default();
                    let text: MeasuredBounds = Default::default();
                    let has_path = path.is_some();
                    let _window = cx.open_window(size(px(width), px(40.0)), |_, _| TextFixture { width, rem, path, regions: regions.clone(), text: text.clone() });
                    let regions = regions.borrow();
                    let text = text.borrow();
                    let region = regions[0];
                    assert_eq!(regions[1].size.width, px(30.0), "the action slot must not shrink");
                    assert_eq!(region.right(), regions[1].left());
                    assert!(text[0].right() <= region.right(), "the label must truncate inside its allotted width");
                    assert!((f32::from(text[0].center().y - region.center().y)).abs() <= 0.5, "the label stays centered even when a path is shown");
                    if has_path {
                        assert_eq!(text[1].origin, region.origin);
                        assert_eq!(text[1].size, size(region.size.width, region.size.height * 0.5));
                    }
                }
            }
        }
    }
}

/// A gentle shadow for a container card. Its outer grid cell paints this
/// before clipping the card's contents, so the shadow can reach the gaps.
pub fn section_card_shadow(theme: BrowserTheme) -> Vec<BoxShadow> {
    let mut color = black();
    color.a = theme.shadow.a * if theme.page_bg.l < 0.5 { 1.0 } else { 0.32 };
    vec![box_shadow(px(0.0), px(2.0), px(8.0), px(0.0), color)]
}

/// A section card's tile, sharing the page background.
/// `washed` — selected, or under the keyboard cursor — fills it with the
/// keyboard's `accent_wash`, the same mark a folder chip takes; see the ladder
/// in `tokens.rs`.
pub fn section_card_frame(washed: bool, theme: BrowserTheme) -> Div {
    div().absolute().inset_0().min_w_0().flex().flex_col().bg(if washed { theme.accent_wash } else { theme.page_bg }).when(cfg!(feature = "kobo"), |frame| frame.border_1().border_color(gpui::black()))
}

/// Full-width colour behind a section card's heading. On Kobo it is not
/// filled: the outer frame encloses it, and a black rule closes it below.
pub fn section_card_band(index: usize, heading: impl IntoElement) -> Div {
    let band = div().flex_none().min_w_0().py(gpui::rems(crate::SECTION_CARD_HEADING_PADDING_REM)).px(px(SPACE_MD));
    #[cfg(feature = "kobo")]
    {
        let _ = index;
        band.bg(gpui::white()).border_b_1().border_color(gpui::black()).child(heading)
    }
    #[cfg(not(feature = "kobo"))]
    {
        let (from, to) = section_card_band_colors(index);
        band.bg(gpui::linear_gradient(120.0, gpui::linear_color_stop(from, 0.0), gpui::linear_color_stop(to, 1.0))).child(heading)
    }
}

/// The area below a section card's heading. Kobo uses the card's outer border.
pub fn section_card_content(theme: BrowserTheme) -> Div {
    div().flex_1().min_h_0().min_w_0().flex().flex_col().px(px(SPACE_MD - 1.0)).border_1().border_color(theme.rule).when(cfg!(feature = "kobo"), |content| content.border_0())
}

/// A section card's title, total and paging controls on a shared colour band.
/// Only the title's text area points, underlines and opens the section.
///
/// A heading that `opens` nothing — a shelf of Home — neither points nor
/// underlines.
pub fn section_card_heading(
    id: impl Into<ElementId>, title: impl Into<SharedString>, count: Option<usize>, pager: Option<AnyElement>, opens: bool, focused: bool, marquee: bool, theme: BrowserTheme, on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(gpui::rems(crate::SECTION_CARD_HEADING_HEIGHT_REM))
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(SPACE_SM))
        // Light on the colour band; black on Kobo's unfilled one.
        .text_color(if cfg!(feature = "kobo") { theme.text } else { gpui::rgb(0xf2efe8).into() })
        .child(
            div().flex_1().min_w_0().flex().child(
                div()
                    .id("title")
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .when_some(theme.display_font_family(), |text, family| text.font_family(family))
                    .text_size(gpui::rems(TEXT_LG))
                    .when(focused && !cfg!(feature = "kobo"), |title| title.text_color(gpui::white()))
                    .when(opens && !marquee, |title| title.cursor_pointer().hover(|style| style.underline()).on_click(on_open))
                    .child(title.into()),
            ),
        )
        .children(count.map(|count| div().flex_none().child(count.to_string())))
        .children(pager)
}

/// One child in a section card's list, with its navigation icon and a quiet
/// count pill, when it has a count. The whole row is the hover and click target.
pub fn section_card_subject(id: impl Into<ElementId>, icon: NavigationIcon, label: impl Into<SharedString>, count: Option<usize>, theme: BrowserTheme) -> Stateful<Div> {
    div()
        .id(id)
        .group("section-card-subject")
        .min_h(gpui::rems(crate::SECTION_CARD_SUBJECT_ROW_HEIGHT_REM))
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(SPACE_SM))
        .mx(px(-SPACE_XS))
        .px(px(SPACE_XS))
        .cursor_pointer()
        .text_size(gpui::rems(TEXT_MD))
        .line_height_auto()
        .hover(move |style| style.bg(theme.hover))
        .child(crate::navigation_icon(icon).size(gpui::rems(0.8)).text_color(theme.text_accent))
        .child(div().id("label").flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.into()))
        .children(count.map(|count| div().flex_none().min_w(gpui::rems(1.6)).px(px(SPACE_XS)).text_center().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).bg(theme.hover).child(count.to_string())))
}

/// The body shared by container cards and book shelves.
pub fn section_card_body() -> Div {
    // Cells provide the full vertical inset. Offset the content border so it
    // does not add another pixel above or below their padding. Kobo's body
    // stays inside the frame so it cannot paint over the black outline.
    div().flex_1().min_h_0().min_w_0().my(px(if cfg!(feature = "kobo") { 0.0 } else { -1.0 })).flex().gap(px(crate::SECTION_CARD_COLUMN_GAP))
}

/// Paging control in a container or book shelf heading.
pub fn section_card_cover_arrow(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: IconName, enabled: bool) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .size(gpui::rems(1.5))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |arrow| arrow.cursor_pointer())
        .border_1()
        // Translucent white on the colour band; outlined black on Kobo's white one.
        .when(!cfg!(feature = "kobo"), |arrow| {
            arrow.text_color(gpui::rgb(0xf2efe8)).bg(gpui::white().opacity(0.12)).border_color(gpui::white().opacity(0.28)).when(enabled, |arrow| arrow.hover(|style| style.bg(gpui::white().opacity(0.22))))
        })
        .when(cfg!(feature = "kobo"), |arrow| arrow.text_color(gpui::black()).bg(gpui::white()).border_color(gpui::black()))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
        .child(Icon::new(icon).size(px(12.0)))
}

/// Marks the place a drag will land: an accent outline and a faint wash,
/// shown while a drag of `T`, or of files from the desktop, is over it and
/// `lands_here` says so. It is laid over the target's own content, so it shows
/// on a button as well as on a plain row, and it takes no clicks of its own.
/// The target it sits in has to be `relative`.
pub fn drop_highlight<T: 'static>(lands_here: impl Fn(&gpui::App) -> bool + 'static, theme: BrowserTheme) -> Div {
    let accent = theme.accent;
    let wash = theme.accent.opacity(0.08);
    let lands_here = std::rc::Rc::new(lands_here);
    let lands_here_files = lands_here.clone();
    div()
        .absolute()
        .inset_0()
        .drag_over::<T>(move |style, _, _, cx| if lands_here(cx) { style.border_2().border_color(accent).bg(wash) } else { style })
        .drag_over::<gpui::ExternalPaths>(move |style, _, _, cx| if lands_here_files(cx) { style.border_2().border_color(accent).bg(wash) } else { style })
}
