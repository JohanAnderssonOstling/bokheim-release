//! The floating note editor an annotation opens, and where it is placed.
//!
//! Both paginated readers put the same panel over the same kind of layout: a
//! document split into equal columns beneath a toolbar, with an optional
//! sidebar to its left. The panel is centred on the column that was clicked
//! and takes whichever vertical side has more room. Only the actions differ
//! between the readers, so they are supplied by the caller.

use gpui::prelude::*;
use std::rc::Rc;

use gpui::{AnyElement, App, Context, Entity, Pixels, Point, SharedString, Window, px};
use gpui_component::input::{Input, InputState};
use ui_components as components;

use book_model::AnnotationStyle;
use library_backend::ReaderAnnotation;

const WIDTH: f32 = 360.0;
const MAX_HEIGHT: f32 = 420.0;

/// Distance kept between the panel and the edges of the document area.
const INSET: f32 = 12.0;

/// Distance kept between the panel and the point it describes.
const GAP: f32 = 10.0;

/// Creates the note input used by every annotation editor.
///
/// Auto-grow rather than a fixed box: the field is as tall as what has been
/// written in it. Its ceiling is high enough never to be the binding one — the
/// panel reaches the room its anchor allows first, and scrolls there.
pub(crate) fn note_input(window: &mut Window, cx: &mut Context<InputState>) -> InputState {
    InputState::new(window, cx).placeholder("Add a note").auto_grow(components::READER_NOTE_MIN_ROWS, components::READER_NOTE_MAX_ROWS)
}

/// Where to place the panel, in pixels relative to the document area.
pub(crate) struct Geometry {
    left: f32,
    /// Offset from whichever edge the panel is anchored to, named by `above`.
    edge_offset: f32,
    max_height: f32,
    /// Whether the panel sits above the anchor rather than below it.
    above: bool,
}

/// The width the document occupies once the sidebar has taken its share.
pub(crate) fn document_width(viewport_width: f32, sidebar_width: f32) -> f32 {
    (viewport_width - sidebar_width).max(1.0)
}

/// The passage the panel is about, in window coordinates.
///
/// A box rather than a point: a selection is as tall as the lines it crosses,
/// and a panel placed against the pointer that made it covers everything above
/// that pointer. The panel is placed clear of the whole box, which is also what
/// lets it leave the quoted text out — the reader can see the passage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Passage {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

impl Passage {
    pub(crate) fn from_bounds(bounds: gpui::Bounds<Pixels>) -> Self {
        Self { left: f32::from(bounds.origin.x), right: f32::from(bounds.origin.x + bounds.size.width), top: f32::from(bounds.origin.y), bottom: f32::from(bounds.origin.y + bounds.size.height) }
    }

    /// A passage whose extent is unknown, standing on the point that opened the
    /// panel. Placement then clears that point rather than the whole passage,
    /// which is the old behaviour and the best a pointer position supports.
    pub(crate) fn from_point(point: Point<Pixels>) -> Self {
        Self { left: f32::from(point.x), right: f32::from(point.x), top: f32::from(point.y), bottom: f32::from(point.y) }
    }

    fn center_x(self) -> f32 {
        (self.left + self.right) / 2.0
    }
}

/// Places the panel clear of `passage`.
///
/// `passage` is in window coordinates, so the sidebar and toolbar are taken off
/// it to reach document coordinates. Without one — an annotation opened from
/// the sidebar rather than clicked — the panel goes in the first column, a
/// third of the way down.
///
/// The side with more room wins, and the panel is anchored to that edge of the
/// passage rather than to its middle: above means "ending `GAP` above the first
/// line", below means "starting `GAP` under the last".
pub(crate) fn geometry(passage: Option<Passage>, viewport_width: f32, viewport_height: f32, sidebar_width: f32, toolbar_height: f32, column_count: usize) -> Geometry {
    let document_width = document_width(viewport_width, sidebar_width);
    let document_height = (viewport_height - toolbar_height).max(1.0);
    let column_count = column_count.max(1) as f32;
    let column_width = document_width / column_count;

    let passage = passage.map(|passage| Passage { left: passage.left - sidebar_width, right: passage.right - sidebar_width, top: passage.top - toolbar_height, bottom: passage.bottom - toolbar_height }).unwrap_or(Passage {
        left: 0.0,
        right: column_width,
        top: document_height / 3.0,
        bottom: document_height / 3.0,
    });
    let center_x = passage.center_x().clamp(0.0, document_width - f32::EPSILON);
    let top = passage.top.clamp(0.0, document_height);
    let bottom = passage.bottom.clamp(top, document_height);

    let column = (center_x / column_width).floor().min(column_count - 1.0);
    let column_center = (column + 0.5) * column_width;
    let panel_width = WIDTH.min((document_width - INSET * 2.0).max(1.0));
    let maximum_left = (document_width - panel_width).max(0.0);
    let side_inset = INSET.min(maximum_left / 2.0);
    let left = (column_center - panel_width / 2.0).clamp(side_inset, (maximum_left - side_inset).max(side_inset));

    let room_above = (top - GAP - INSET).max(1.0);
    let room_below = (document_height - bottom - GAP - INSET).max(1.0);
    let above = room_above > room_below;
    Geometry { left, edge_offset: if above { document_height - top + GAP } else { bottom + GAP }, max_height: (if above { room_above } else { room_below }).min(MAX_HEIGHT), above }
}

/// One button in the editor's action rows.
struct Action {
    id: SharedString,
    label: SharedString,
    /// Whether this is the state the annotation is already in. A style row that
    /// cannot say which style is on is a row of four questions.
    selected: bool,
    on_click: Box<dyn Fn(&mut Window, &mut App) + 'static>,
}

impl Action {
    fn new(id: impl Into<SharedString>, label: impl Into<SharedString>, on_click: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { id: id.into(), label: label.into(), selected: false, on_click: Box::new(on_click) }
    }

    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

/// One highlight colour. The swatch is the colour, so the name is only what the
/// tooltip says and what a screen reader hears.
struct ColorAction {
    id: SharedString,
    hex: &'static str,
    name: &'static str,
    selected: bool,
    on_click: Box<dyn Fn(&mut Window, &mut App) + 'static>,
}

/// Everything the editor can do, grouped the way it is drawn.
///
/// Only the two choices. There is no Save — a note is written as it is typed
/// and committed when the panel closes — and no Close, because clicking the
/// book is how a reader puts the panel down. Deleting is the sidebar's, where
/// the annotation can be seen in the company of the others.
struct EditorActions {
    styles: Vec<Action>,
    colors: Vec<ColorAction>,
}

/// The common actions used by both readers.
///
/// Readers provide the save action because applying an annotation to EPUB text
/// and a PDF page remains format-specific. Keeping the choices here prevents
/// their labels, colours, or styles from drifting apart.
///
/// The colours come from [`ui_components::READER_ANNOTATION_COLORS`], which is
/// also what the swatch and the renderers read, so a colour is named in exactly
/// one place.
fn standard_actions(id_prefix: &'static str, current: &ReaderAnnotation, mut save: impl FnMut(String, &'static str, Option<AnnotationStyle>, Option<&'static str>) -> Action) -> EditorActions {
    let id = |suffix: &str| format!("{id_prefix}-{suffix}");
    let styles = [(AnnotationStyle::Highlight, "Highlight", "highlight"), (AnnotationStyle::Underline, "Underline", "underline"), (AnnotationStyle::Squiggly, "Squiggly", "squiggly"), (AnnotationStyle::Strikethrough, "Strike", "strike")]
        .into_iter()
        .map(|(style, label, suffix)| {
            let selected = current.style == style;
            save(id(suffix), label, Some(style), None).selected(selected)
        })
        .collect();
    let colors = components::READER_ANNOTATION_COLORS
        .iter()
        .map(|(name, hex)| {
            let action = save(id(&name.to_lowercase()), name, None, Some(hex));
            ColorAction { id: action.id, hex, name, selected: current.color.eq_ignore_ascii_case(hex), on_click: action.on_click }
        })
        .collect();
    EditorActions { styles, colors }
}

/// Builds the editor panel, absolutely placed by `geometry`.
///
/// Two rows, quietest last: how the passage is marked, and in which colour.
/// `dismiss` runs when a click lands outside the panel, which is the only way
/// it closes.
fn editor(geometry: Geometry, note: &Entity<InputState>, actions: EditorActions, dismiss: impl Fn(&mut Window, &mut App) + 'static, theme: components::BrowserTheme) -> AnyElement {
    let mut panel = components::reader_annotation_editor(Input::new(note).appearance(false).w_full(), theme)
        .absolute()
        .left(px(geometry.left))
        .max_h(px(geometry.max_height))
        .when(geometry.above, |panel| panel.bottom(px(geometry.edge_offset)))
        .when(!geometry.above, |panel| panel.top(px(geometry.edge_offset)))
        // The panel floats over the document, which treats a click as "put the
        // cursor here" and would clear the very selection being annotated.
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        // Anywhere else is "done": the note is committed and the panel goes.
        .on_mouse_down_out(move |_, window, cx| dismiss(window, cx));
    let button = |action: Action| {
        let on_click = action.on_click;
        components::reader_annotation_action_button(action.id, action.label, action.selected, theme).on_click(move |_, window, cx| on_click(window, cx))
    };

    let mut styles = components::action_row();
    for action in actions.styles {
        styles = styles.child(button(action));
    }
    panel = panel.child(styles);

    let mut colors = components::reader_annotation_color_group();
    for color in actions.colors {
        let on_click = color.on_click;
        colors = colors.child(components::reader_annotation_color_swatch(color.id, color.hex, color.name, color.selected, theme).on_click(move |_, window, cx| on_click(window, cx)));
    }
    panel.child(colors).into_any_element()
}

/// Builds the standard editor and connects its format-specific effects.
///
/// `close` runs on a click outside the panel, and the note is written back
/// first: a reader who typed a note and turned the page has saved it, because
/// nothing in the panel ever asked them to.
pub(crate) fn standard_editor<C: 'static>(
    id_prefix: &'static str, target: &Entity<C>, annotation: &ReaderAnnotation, geometry: Geometry, note: &Entity<InputState>, theme: components::BrowserTheme,
    save: impl Fn(Option<AnnotationStyle>, Option<&'static str>, &mut C, &mut Context<C>) + 'static, close: impl Fn(&mut Window, &mut C, &mut Context<C>) + 'static,
) -> AnyElement {
    let save = Rc::new(save);
    let save_target = target.clone();
    let save_for_dismiss = save.clone();
    let save_action = move |id: String, label: &'static str, style: Option<AnnotationStyle>, color: Option<&'static str>| {
        let save = save.clone();
        let target = save_target.clone();
        Action::new(id, label, move |_, cx| {
            let style = style.clone();
            target.update(cx, |this, cx| save(style, color, this, cx));
        })
    };

    let dismiss_target = target.clone();
    let dismiss = move |window: &mut Window, cx: &mut App| {
        dismiss_target.update(cx, |this, cx| {
            // Neither style nor colour changes here; this is the note going in.
            save_for_dismiss(None, None, this, cx);
            close(window, this, cx);
        });
    };

    editor(geometry, note, standard_actions(id_prefix, annotation, save_action), dismiss, theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    #[test]
    fn the_panel_centers_on_the_column_that_was_clicked() {
        let left = geometry(Some(Passage::from_point(point(px(100.0), px(200.0)))), 1200.0, 800.0, 0.0, 58.0, 2);
        let right = geometry(Some(Passage::from_point(point(px(900.0), px(200.0)))), 1200.0, 800.0, 0.0, 58.0, 2);

        assert_eq!(left.left, 120.0);
        assert_eq!(right.left, 720.0);
    }

    #[test]
    fn the_panel_takes_the_roomier_vertical_side() {
        let near_top = geometry(Some(Passage::from_point(point(px(120.0), px(180.0)))), 1200.0, 800.0, 0.0, 58.0, 3);
        let near_bottom = geometry(Some(Passage::from_point(point(px(1020.0), px(700.0)))), 1200.0, 800.0, 0.0, 58.0, 3);

        assert!(!near_top.above, "there is more room below an anchor near the top");
        assert!(near_bottom.above);
        assert!(near_top.max_height <= MAX_HEIGHT && near_bottom.max_height <= MAX_HEIGHT);
    }

    #[test]
    fn a_narrow_document_still_places_the_panel_inside_itself() {
        let narrow = geometry(Some(Passage::from_point(point(px(10.0), px(200.0)))), 200.0, 800.0, 0.0, 58.0, 1);

        assert!(narrow.left >= 0.0, "the panel must not start left of the document");
        assert!(narrow.left <= 200.0, "nor beyond its right edge");
    }

    #[test]
    fn the_sidebar_and_toolbar_are_taken_off_a_window_anchor() {
        let with_chrome = geometry(Some(Passage::from_point(point(px(400.0), px(258.0)))), 1200.0, 800.0, 200.0, 58.0, 1);
        let without_chrome = geometry(Some(Passage::from_point(point(px(200.0), px(200.0)))), 1000.0, 742.0, 0.0, 0.0, 1);

        assert_eq!(with_chrome.left, without_chrome.left);
        assert_eq!(with_chrome.edge_offset, without_chrome.edge_offset);
    }

    /// The point a selection ends at is on its last line, so a panel placed
    /// against that point sits over every line above it. Against the box, the
    /// panel clears the whole passage — which is what lets the quote go.
    #[test]
    fn the_panel_clears_the_whole_passage_not_just_the_pointer() {
        // Four lines low on the page, so both placements go above and the only
        // difference is which line they clear.
        let passage = Passage::from_bounds(gpui::Bounds::from_corners(point(px(100.0), px(500.0)), point(px(500.0), px(700.0))));
        let mouse_up = Passage::from_point(point(px(500.0), px(700.0)));

        let against_box = geometry(Some(passage), 1200.0, 800.0, 0.0, 58.0, 1);
        let against_point = geometry(Some(mouse_up), 1200.0, 800.0, 0.0, 58.0, 1);

        assert!(against_box.above);
        assert!(against_point.above);
        // Measured from the document's bottom edge, so a larger offset is a
        // panel whose lower edge sits higher up the page.
        assert!(against_box.edge_offset > against_point.edge_offset, "the panel must end above the passage's first line ({}), not above the pointer on its last ({})", against_box.edge_offset, against_point.edge_offset);
        assert_eq!(against_box.edge_offset, 800.0 - 58.0 - (500.0 - 58.0) + GAP);
    }

    #[test]
    fn a_passage_with_room_below_puts_the_panel_under_its_last_line() {
        let passage = Passage::from_bounds(gpui::Bounds::from_corners(point(px(100.0), px(100.0)), point(px(500.0), px(200.0))));

        let placed = geometry(Some(passage), 1200.0, 800.0, 0.0, 58.0, 1);

        assert!(!placed.above);
        assert_eq!(placed.edge_offset, 200.0 - 58.0 + GAP, "the panel starts under the passage's last line");
    }

    #[test]
    fn an_unanchored_panel_lands_in_the_first_column() {
        let unanchored = geometry(None, 1200.0, 800.0, 0.0, 58.0, 3);
        let first_column = geometry(Some(Passage::from_point(point(px(200.0), px(58.0 + (800.0 - 58.0) / 3.0)))), 1200.0, 800.0, 0.0, 58.0, 3);

        assert_eq!(unanchored.left, first_column.left);
    }
}
