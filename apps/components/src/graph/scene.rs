//! Draws column and radial graphs on an infinite canvas.
//!
//! The root, the path and the selection are Bodoni Moda capitals, spaced with
//! hair spaces because gpui has no letter-spacing (Bodoni's hair space is 0.1 em).
//! Every other label is in Libertinus small caps. Subtree counts follow the last line, except on
//! e-ink, where that size would not survive dithering.

use gpui::prelude::FluentBuilder;
use gpui::{
    App, BorderStyle, Entity, FontFeatures, FontStyle, FontWeight, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, SharedString, Styled, TextRun, Window, canvas, div, point, px, quad, rems, size,
};

use super::layout::{GraphLayout, GraphLayoutKind, LabelMetrics, LabelRole, PlacedLabel};
use crate::{BrowserTheme, InfiniteCanvas, InfiniteCanvasView, browser_theme, infinite_canvas};

const DISPLAY_FAMILY: &str = crate::DISPLAY_FONT_FAMILY;
const TEXT_FAMILY: &str = "Libertinus Serif";
const HAIR_SPACE: &str = "\u{200A}";
const LINE_HEIGHT: f32 = 1.12;
const E_INK: bool = cfg!(feature = "kobo");
/// A raised count's size and gap relative to its label; on e-ink the count sits on the baseline.
const COUNT_SCALE: f32 = if E_INK { 0.82 } else { 0.75 };
const COUNT_GAP: f32 = if E_INK { 5.0 } else { 2.0 };
const LABEL_SIZE_STEP: f32 = crate::TEXT_SM - crate::TEXT_XS;

/// Label sizes in rems for one viewport class.
#[derive(Clone, Copy, Debug)]
pub struct GraphType {
    pub root: f32,
    pub path: f32,
    pub selection: f32,
    pub fan: f32,
    pub context: f32,
}

impl GraphType {
    pub fn desktop() -> Self {
        Self { root: 1.25, path: 0.9, selection: 1.05, fan: 0.9375 + LABEL_SIZE_STEP, context: 0.875 + LABEL_SIZE_STEP }
    }

    /// Phones and e-readers.
    pub fn compact() -> Self {
        Self { root: 1.0, path: 0.775, selection: 0.9, fan: 0.9375 + LABEL_SIZE_STEP, context: 0.84375 + LABEL_SIZE_STEP }
    }

    fn size(&self, role: LabelRole, window: &Window) -> Pixels {
        let rem = match role {
            LabelRole::Root => self.root,
            LabelRole::Path => self.path,
            LabelRole::Selection => self.selection,
            LabelRole::Fan => self.fan,
            LabelRole::Context => self.context,
        };
        rems(rem).to_pixels(window.rem_size())
    }
}

/// Measures labels with the fonts the scene draws them in.
pub struct GraphMetrics<'a> {
    pub window: &'a Window,
    pub sizes: GraphType,
}

impl GraphMetrics<'_> {
    fn width(&self, text: &str, font: gpui::Font, size: Pixels) -> f32 {
        let run = TextRun { len: text.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
        f32::from(self.window.text_system().shape_line(SharedString::from(text.to_string()), size, &[run], None).width)
    }
}

impl LabelMetrics for GraphMetrics<'_> {
    fn text_width(&self, text: &str, role: LabelRole) -> f32 {
        self.width(&shown(text, role), font(role), self.sizes.size(role, self.window))
    }

    fn count_width(&self, count: usize, role: LabelRole) -> f32 {
        self.width(&count.to_string(), count_font(), self.sizes.size(role, self.window) * COUNT_SCALE) + COUNT_GAP
    }

    fn line_height(&self, role: LabelRole) -> f32 {
        f32::from(self.sizes.size(role, self.window)) * LINE_HEIGHT
    }
}

/// The graph on `canvas`: edges, then labels, in world coordinates. Chain the
/// canvas handlers (`on_tap` and the rest) onto the result.
pub fn graph_scene(canvas: &Entity<InfiniteCanvas>, layout: &GraphLayout, departing: Option<(&GraphLayout, f32)>, hovered: Option<usize>, sizes: GraphType, window: &Window, cx: &App) -> InfiniteCanvasView {
    let theme = browser_theme(cx);
    let zoom = canvas.read(cx).camera.zoom;
    let mut view = infinite_canvas(canvas);
    if let Some((departing, opacity)) = departing {
        view = view.child(edges(departing, theme, zoom, opacity))
            .children(departing.labels.iter().map(|label| label_element(label, false, sizes, theme, window, opacity)));
    }
    view.child(edges(layout, theme, zoom, 1.0))
        .children(layout.labels.iter().map(|label| label_element(label, hovered == Some(label.node), sizes, theme, window, 1.0)))
}

/// Curved connectors that keep their screen width at any zoom.
fn edges(layout: &GraphLayout, theme: BrowserTheme, zoom: f32, opacity: f32) -> impl IntoElement {
    let edges = layout.edges.clone();
    let radial = layout.kind == GraphLayoutKind::AdaptiveRadial;
    let (width, path_width): (f32, f32) = if E_INK { (1.0, 1.8) } else { (1.0, 1.5) };
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            for edge in edges.iter() {
                let (stroke, color) = if edge.on_path { (path_width, theme.text_muted) } else { (width, theme.rule) };
                let color = color.opacity(edge.opacity * opacity);
                if radial {
                    let mut builder = PathBuilder::stroke(px(stroke / zoom));
                    builder.move_to(bounds.origin + point(px(edge.start.x), px(edge.start.y)));
                    builder.line_to(bounds.origin + point(px(edge.end.x), px(edge.end.y)));
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, color);
                    }
                    continue;
                }
                if (edge.end.y - edge.start.y).abs() <= 0.5 {
                    let left = edge.start.x.min(edge.end.x);
                    let right = edge.start.x.max(edge.end.x);
                    let thickness = stroke.max(1.0_f32) / zoom;
                    let bar = gpui::Bounds::new(
                        bounds.origin + point(px(left), px(edge.start.y - thickness / 2.0)),
                        size(px(right - left), px(thickness)),
                    );
                    window.paint_quad(quad(bar, px(0.0), color, px(0.0), color, BorderStyle::Solid));
                    continue;
                }
                let mut builder = PathBuilder::stroke(px(stroke / zoom));
                builder.move_to(bounds.origin + point(px(edge.start.x), px(edge.start.y)));
                let middle = (edge.start.x + edge.end.x) / 2.0;
                builder.cubic_bezier_to(
                    bounds.origin + point(px(edge.end.x), px(edge.end.y)),
                    bounds.origin + point(px(middle), px(edge.start.y)),
                    bounds.origin + point(px(middle), px(edge.end.y)),
                );
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn label_element(label: &PlacedLabel, hovered: bool, sizes: GraphType, theme: BrowserTheme, window: &Window, opacity: f32) -> impl IntoElement {
    let size = sizes.size(label.role, window);
    let color = if hovered { theme.text_accent } else { match label.role {
        LabelRole::Selection => theme.text_accent,
        LabelRole::Context => theme.text_muted,
        LabelRole::Root | LabelRole::Path | LabelRole::Fan => theme.text,
    } };
    let last = label.lines.len() - 1;
    div()
        .absolute()
        .left(px(label.center.x - label.text_width / 2.0))
        .top(px(label.center.y - label.text_height / 2.0))
        .w(px(label.text_width))
        .flex()
        .flex_col()
        .items_center()
        .font(font(label.role))
        .font_features(font_features(label.role))
        .text_size(size)
        .line_height(size * LINE_HEIGHT)
        .text_color(color)
        .opacity(label.opacity * opacity)
        .children(label.lines.iter().enumerate().map(|(i, line)| {
            let row = div().flex().whitespace_nowrap().child(shown(line, label.role));
            match label.count.filter(|_| i == last) {
                Some(count) => row
                    .when(E_INK, |row| row.items_end())
                    .child(div().pl(px(COUNT_GAP)).font(count_font()).text_size(size * COUNT_SCALE).line_height(size * COUNT_SCALE).text_color(theme.text_muted).child(count.to_string())),
                None => row,
            }
        }))
}

/// A label's text as drawn: display roles in capitals spaced with hair spaces.
fn shown(text: &str, role: LabelRole) -> String {
    match role {
        LabelRole::Root | LabelRole::Path | LabelRole::Selection => {
            let upper = text.to_uppercase();
            let chars: Vec<String> = upper.chars().map(String::from).collect();
            chars.join(HAIR_SPACE)
        }
        LabelRole::Fan | LabelRole::Context => text.to_string(),
    }
}

fn font(role: LabelRole) -> gpui::Font {
    let (family, weight, style, features) = match role {
        LabelRole::Selection => (DISPLAY_FAMILY, FontWeight::SEMIBOLD, FontStyle::Normal, FontFeatures::default()),
        LabelRole::Root | LabelRole::Path => (DISPLAY_FAMILY, FontWeight::NORMAL, FontStyle::Normal, FontFeatures::default()),
        LabelRole::Fan | LabelRole::Context => (TEXT_FAMILY, FontWeight::NORMAL, FontStyle::Normal, crate::small_caps()),
    };
    gpui::Font { family: family.into(), features, fallbacks: None, weight, style }
}

fn font_features(role: LabelRole) -> FontFeatures {
    match role {
        LabelRole::Fan | LabelRole::Context => crate::small_caps(),
        LabelRole::Root | LabelRole::Path | LabelRole::Selection => FontFeatures::default(),
    }
}

fn count_font() -> gpui::Font {
    gpui::Font { family: TEXT_FAMILY.into(), features: FontFeatures::default(), fallbacks: None, weight: FontWeight::NORMAL, style: FontStyle::Normal }
}
