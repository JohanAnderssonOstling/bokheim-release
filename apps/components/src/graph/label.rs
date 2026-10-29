//! The graph's typographic labels, for callers that draw nodes as text.
//!
//! The root, the path and the selection use the theme's display face in capitals,
//! spaced with hair spaces because gpui has no letter-spacing. Every other
//! label uses the selected interface face's small caps. Subtree counts follow
//! the name, raised, except on e-ink, where that size would not survive
//! dithering and the count sits on the baseline.
//!
//! A label is measured by laying out the element it draws, so the layout and
//! the drawing cannot disagree.

use gpui::prelude::FluentBuilder;
use gpui::{AvailableSpace, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels, SharedString, Size, Stateful, Styled, div, px, rems, size};
use gpui_component::Theme;

use super::layout::NodeRole;
use crate::{BrowserTheme, browser_theme};

const HAIR_SPACE: &str = "\u{200A}";
const E_INK: bool = cfg!(feature = "kobo");
/// A raised count's size and gap relative to its label; on e-ink the count sits on the baseline.
const COUNT_SCALE: f32 = if E_INK { 0.82 } else { 0.75 };
const COUNT_GAP: f32 = if E_INK { 5.0 } else { 2.0 };
const LABEL_SIZE_STEP: f32 = crate::TEXT_SM - crate::TEXT_XS;
/// The widest a label's text runs before it wraps, then is cut short on its second line.
const MAX_TEXT_WIDTH_REM: f32 = 12.5;
const MAX_LINES: usize = 2;
const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 8.0;

/// A label for `name` in `role`, filling the box the graph gives it.
pub fn graph_label(id: impl Into<ElementId>, role: NodeRole, name: &str, count: Option<usize>, compact: bool, theme: BrowserTheme, font_family: SharedString) -> Stateful<Div> {
    let color = match role {
        NodeRole::Selection => theme.text_accent,
        NodeRole::Context => theme.text_muted,
        NodeRole::Root | NodeRole::Path | NodeRole::Fan => theme.text,
    };
    let accent = theme.text_accent;
    div().id(id).size_full().flex().items_center().justify_center().cursor_pointer().text_color(color).hover(move |style| style.text_color(accent)).child(label_body(role, name, count, compact, theme, font_family))
}

/// The box [`graph_label`] needs for the same label.
pub fn graph_label_size(role: NodeRole, name: &str, count: Option<usize>, compact: bool, window: &mut gpui::Window, cx: &mut gpui::App) -> Size<Pixels> {
    let theme = browser_theme(cx);
    let mut body = label_body(role, name, count, compact, theme, Theme::global(cx).font_family.clone()).into_any_element();
    body.layout_as_root(size(AvailableSpace::MaxContent, AvailableSpace::MaxContent), window, cx)
}

fn label_body(role: NodeRole, name: &str, count: Option<usize>, compact: bool, theme: BrowserTheme, font_family: SharedString) -> Div {
    let text_size = label_rems(role, compact);
    let display = matches!(role, NodeRole::Root | NodeRole::Path | NodeRole::Selection);
    let shown = if display { name.to_uppercase().chars().map(String::from).collect::<Vec<_>>().join(HAIR_SPACE) } else { name.to_owned() };
    let text = div()
        .max_w(rems(MAX_TEXT_WIDTH_REM))
        .text_center()
        .line_clamp(MAX_LINES)
        .text_ellipsis()
        .when(display, |text| text.when_some(theme.display_font_family(), |text, family| text.font_family(family)).when(role == NodeRole::Selection, |text| text.font_weight(FontWeight::SEMIBOLD)))
        .when(!display, |text| text.font_family(font_family.clone()).font_features(crate::small_caps()))
        .child(shown);
    div()
        .px(px(PAD_X))
        .py(px(PAD_Y))
        .flex()
        .map(|row| if E_INK { row.items_end() } else { row.items_start() })
        .text_size(rems(text_size))
        .line_height_auto()
        .child(text)
        .children(count.map(|count| div().flex_none().pl(px(COUNT_GAP)).font_family(font_family).text_size(rems(text_size * COUNT_SCALE)).line_height_auto().text_color(theme.text_muted).child(count.to_string())))
}

/// Label sizes in rems, for phones and e-readers or for larger windows.
fn label_rems(role: NodeRole, compact: bool) -> f32 {
    match (role, compact) {
        (NodeRole::Root, false) => 1.25,
        (NodeRole::Path, false) => 0.9,
        (NodeRole::Selection, false) => 1.05,
        (NodeRole::Fan, false) => 0.9375 + LABEL_SIZE_STEP,
        (NodeRole::Context, false) => 0.875 + LABEL_SIZE_STEP,
        (NodeRole::Root, true) => 1.0,
        (NodeRole::Path, true) => 0.775,
        (NodeRole::Selection, true) => 0.9,
        (NodeRole::Fan, true) => 0.9375 + LABEL_SIZE_STEP,
        (NodeRole::Context, true) => 0.84375 + LABEL_SIZE_STEP,
    }
}
