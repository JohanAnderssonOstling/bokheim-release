//! The book overlay: what a book is, and what can be done with it, opened from
//! its card in place of a context menu.
//!
//! It says little on purpose — title, author, a few lines of the blurb, the
//! curated subjects — and leaves everything else to the book's own page. Its
//! width is its commands': the row of buttons sets it, and the text wraps to
//! what they leave.
//!
//! These functions style, lay out and measure. What a command does, and which
//! commands a book has, are the card's business.

use gpui::prelude::*;
use gpui::{AnyElement, App, AvailableSpace, Bounds, Div, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId, Pixels, Refineable, SharedString, Stateful, Style, StyleRefinement, Window, div, point, px, relative, rems, size};
use gpui_component::Icon;
use crate::Button;

use crate::{BrowserTheme, SPACE_MD, SPACE_SM, SPACE_XS, SPACE_XXS, TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XS, base_button, topbar_action_height_rem};

/// Never narrower than this, so a book with a single command — a catalog
/// book's Add — still has a column its blurb can be read in.
pub const BOOK_OVERLAY_MIN_WIDTH_REM: f32 = 22.0;
/// Enough of the blurb to say what the book is; the rest is on its page.
const DESCRIPTION_LINES: usize = 6;
const DESCRIPTION_LINE_HEIGHT: f32 = 1.5;
/// Space between the buttons of a row, which the width is measured with.
const COMMAND_GAP: f32 = SPACE_XS;

/// Places the overlay beside what it is placed over: to its right where the
/// window has room, else to its left, else centred over it — and always
/// inside the window, `margin` from its edges. Covers its parent (which must
/// be `relative()`) to learn where that is; draws only the overlay.
///
/// Wrap it in `deferred` so the overlay is drawn above the page.
pub struct BookOverlayBeside {
    style: StyleRefinement,
    overlay: AnyElement,
    gap: Pixels,
    margin: Pixels,
}

pub fn book_overlay_beside(overlay: impl IntoElement, gap: Pixels, margin: Pixels) -> BookOverlayBeside {
    BookOverlayBeside { style: StyleRefinement::default(), overlay: overlay.into_any_element(), gap, margin }.absolute().top_0().left_0().size_full()
}

impl Styled for BookOverlayBeside {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Element for BookOverlayBeside {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, beside: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) {
        let overlay = self.overlay.layout_as_root(size(AvailableSpace::MaxContent, AvailableSpace::MaxContent), window, cx);
        let viewport = window.viewport_size();
        let (gap, margin) = (self.gap, self.margin);
        let right = beside.right() + gap;
        let left = beside.left() - gap - overlay.width;
        let x = if right + overlay.width + margin <= viewport.width {
            right
        } else if left >= margin {
            left
        } else {
            beside.center().x - overlay.width / 2.0
        };
        let x = x.min(viewport.width - margin - overlay.width).max(margin);
        let y = beside.top().min(viewport.height - margin - overlay.height).max(margin);
        self.overlay.prepaint_at(point(x, y), window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        self.overlay.paint(window, cx);
    }
}

impl IntoElement for BookOverlayBeside {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The floating surface, at the width [`book_overlay_width`] gave it.
pub fn book_overlay_surface(id: impl Into<ElementId>, width: Pixels, theme: BrowserTheme) -> Stateful<Div> {
    crate::modal_surface(theme).id(id).w(width).flex().flex_col().overflow_hidden()
}

/// Title and byline on the left; opening the book on the right, level with
/// the title.
pub fn book_overlay_header() -> Div {
    div().w_full().min_w_0().flex().flex_row().items_start().gap(px(SPACE_MD)).px(px(SPACE_MD)).pt(px(SPACE_MD))
}

/// The column the title, byline and facts stack in. It takes what the
/// actions beside it leave, and wraps.
pub fn book_overlay_headline() -> Div {
    div().flex_1().min_w_0().flex().flex_col().gap(px(SPACE_XS))
}

/// Read or Listen, and New window, beside the title.
pub fn book_overlay_actions() -> Div {
    div().flex_none().flex().flex_row().items_center().gap(px(COMMAND_GAP))
}

/// The display face at the detail page's smaller title size.
pub fn book_overlay_title(title: impl Into<SharedString>, subtitle: Option<&str>, theme: BrowserTheme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_XS))
        .child(div().when_some(theme.display_font_family(), |text, family| text.font_family(family)).text_size(rems(TEXT_LG)).line_height_auto().text_color(theme.text).child(title.into()))
        .children(subtitle.filter(|subtitle| !subtitle.trim().is_empty()).map(|subtitle| div().text_size(rems(TEXT_MD)).text_color(theme.text_muted).child(subtitle.to_owned())))
}

/// "by" and the names after it, in the muted caption the detail page uses.
pub fn book_overlay_byline(theme: BrowserTheme) -> Div {
    div().flex().flex_row().flex_wrap().items_baseline().gap_x(px(SPACE_XS)).text_size(rems(TEXT_SM)).text_color(theme.text_muted)
}

/// Plain words in a byline: "by", "read by", the comma between names.
pub fn book_overlay_byline_text(text: impl Into<SharedString>) -> Div {
    div().flex_none().child(text.into())
}

/// A name that goes to that author's page.
pub fn book_overlay_name_link(id: impl Into<ElementId>, name: impl Into<SharedString>, theme: BrowserTheme) -> Stateful<Div> {
    div().id(id).min_w_0().cursor_pointer().text_color(theme.text_accent).hover(|style| style.underline()).child(name.into())
}

/// How long an audiobook runs, or that a catalog book is already held: facts
/// checked rather than read, separated by a middot.
pub fn book_overlay_facts(facts: impl IntoIterator<Item = SharedString>, theme: BrowserTheme) -> Div {
    let mut row = div().flex().flex_row().flex_wrap().items_baseline().gap(px(SPACE_SM)).text_size(rems(TEXT_XS)).text_color(theme.text_muted);
    for (index, fact) in facts.into_iter().enumerate() {
        if index > 0 {
            row = row.child(div().flex_none().text_color(theme.rule).child("·"));
        }
        row = row.child(div().flex_none().child(fact));
    }
    row
}

/// The blurb, the subjects and the way to the book's page.
pub fn book_overlay_body() -> Div {
    div().w_full().min_w_0().flex().flex_col().gap(px(SPACE_SM)).px(px(SPACE_MD)).pt(px(SPACE_SM)).pb(px(SPACE_MD))
}

/// The first lines of the blurb, stored as HTML, read as plain text and cut
/// with an ellipsis. `None` for a blurb of markup alone.
pub fn book_overlay_description(description: &str, theme: BrowserTheme) -> Option<Div> {
    let text = html::document::plain_text_from_fragment(description);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then(|| div().w_full().min_w_0().whitespace_normal().line_clamp(DESCRIPTION_LINES).text_ellipsis().text_size(rems(TEXT_SM)).line_height(relative(DESCRIPTION_LINE_HEIGHT)).text_color(theme.text).child(text))
}

/// The subjects, one path to a line.
pub fn book_overlay_subjects() -> Div {
    div().w_full().min_w_0().flex().flex_col().gap(px(SPACE_XXS))
}

/// "More details →", to the book's own page.
pub fn book_overlay_more_details(id: impl Into<ElementId>, theme: BrowserTheme) -> Stateful<Div> {
    div().id(id).flex_none().pt(px(SPACE_XS)).cursor_pointer().text_size(rems(TEXT_XS)).text_color(theme.text_accent).hover(|style| style.underline()).child("More details →")
}

/// The commands that are not about opening the book, in one row under a rule.
/// On a phone the row wraps; elsewhere the overlay is as wide as the row.
pub fn book_overlay_commands(wrap: bool, theme: BrowserTheme) -> Div {
    div().w_full().flex().flex_row().when(wrap, |row| row.flex_wrap()).items_center().gap(px(COMMAND_GAP)).px(px(SPACE_MD - SPACE_SM)).py(px(SPACE_SM)).border_t_1().border_color(theme.rule)
}

/// A command: icon and label, on no surface of its own until hovered.
pub fn book_overlay_command(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: impl Into<Icon>, theme: BrowserTheme) -> Button {
    base_button(id).ghost().label(label.into()).icon(icon.into()).text_color(theme.text).h(rems(topbar_action_height_rem())).px(px(SPACE_SM))
}

/// What `buttons` take side by side, gaps included. Measured from the
/// buttons themselves, so the label font and size decide it.
pub fn book_overlay_row_width(buttons: Vec<Button>, window: &mut Window, cx: &mut App) -> Pixels {
    let gaps = px(COMMAND_GAP) * buttons.len().saturating_sub(1) as f32;
    buttons.into_iter().map(|button| measure(button.into_any_element(), window, cx)).sum::<Pixels>() + gaps
}

/// The overlay's width: the widest of its button rows, `row_widths`, with the
/// padding around them; at least [`BOOK_OVERLAY_MIN_WIDTH_REM`]; at most `limit`.
pub fn book_overlay_width(row_widths: &[Pixels], limit: Pixels, window: &Window) -> Pixels {
    let widest = row_widths.iter().copied().max().unwrap_or_default();
    let minimum = rems(BOOK_OVERLAY_MIN_WIDTH_REM).to_pixels(window.rem_size());
    (widest + px(SPACE_MD * 2.0)).max(minimum).min(limit)
}

/// One subject path on one line, folded to the overlay's width as
/// [`fold_trail`](crate::fold_trail) folds: whole crumbs into one "…",
/// from the middle out, the subject itself last of all.
///
/// `crumb` receives each crumb by its index in `labels`, to make it go
/// somewhere.
pub fn book_overlay_subject(id: impl Into<SharedString>, labels: &[SharedString], theme: BrowserTheme, crumb: impl Fn(usize, Stateful<Div>) -> Stateful<Div>) -> crate::FoldedTrail {
    let id = id.into();
    let last = labels.len().saturating_sub(1);
    let crumbs = labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let text = crumb_text(label.clone(), theme).id((SharedString::from(format!("{id}-crumb")), index)).flex_none().cursor_pointer().hover(|style| style.underline());
            // Ancestors recede; the subject itself is the link colour.
            let text = if index == last { text.text_color(theme.text_accent) } else { text.text_color(theme.text_muted) };
            crumb(index, text).into_any_element()
        })
        .collect();
    let labels = labels.to_vec();
    crate::folded_trail(
        id.clone(),
        crumbs,
        move || separator(theme).into_any_element(),
        move |folded| {
            let hidden = labels[folded].iter().map(|label| label.as_ref()).collect::<Vec<&str>>().join(" › ");
            fold(hidden.into(), theme).into_any_element()
        },
    )
    .w_full()
}

fn crumb_text(label: SharedString, theme: BrowserTheme) -> Div {
    div().when(!theme.custom_ui_font, |text| text.font_family(theme.ui_font_family.family_name())).text_size(rems(TEXT_XS)).whitespace_nowrap().child(label)
}

fn separator(theme: BrowserTheme) -> Div {
    crumb_text("›".into(), theme).flex_none().px(px(SPACE_XS)).text_color(theme.rule)
}

/// The folded run, saying what it hides when the pointer rests on it.
fn fold(hidden: SharedString, theme: BrowserTheme) -> Stateful<Div> {
    let tooltip = hidden.clone();
    crumb_text("…".into(), theme)
        .id(SharedString::from(format!("subject-fold-{hidden}")))
        .flex_none()
        .text_color(theme.text_muted)
        .when(!hidden.is_empty(), |fold| fold.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)))
}

fn measure(mut element: AnyElement, window: &mut Window, cx: &mut App) -> Pixels {
    element.layout_as_root(size(AvailableSpace::MaxContent, AvailableSpace::MaxContent), window, cx).width
}
