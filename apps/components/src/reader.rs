use crate::{CONTENT_INSET, topbar_action_height_rem};
use std::rc::Rc;
use std::sync::Arc;

use crate::{Button, Interactive};
use gpui::prelude::*;
use gpui::{AnyElement, App, ClickEvent, Div, ElementId, Image, ImageSource, Img, MouseButton, ObjectFit, Pixels, RenderOnce, Role, ScrollHandle, SharedString, Size, Stateful, StyledImage, Window, div, img, px};
use gpui_component::IconName;
use gpui_component::checkbox::Checkbox;
use gpui_component::menu::PopupMenu;

use crate::{BrowserTheme, RADIUS_MD, RADIUS_SM, SPACE_MD, SPACE_SM, SPACE_XS, SPACE_XXS, ScrollableElement, TEXT_SM, TEXT_XS, browser_theme, menu_item, outlined_text_button, scroll::Scrollable, settings_control_size_rem};

/// The toolbar and its controls grow together with the UI font.
pub fn reader_toolbar_size_px(window: &Window) -> f32 {
    f32::from(gpui::rems(topbar_action_height_rem()).to_pixels(window.rem_size()))
}
const READER_SETTINGS_POPOVER_GAP: f32 = 4.0;
const READER_SETTINGS_POPOVER_WINDOW_MARGIN: f32 = 8.0;
const READER_SETTINGS_MAX_WIDTH: f32 = 400.0;
const READER_SETTINGS_MAX_HEIGHT: f32 = 620.0;
const READER_SETTINGS_PADDING: f32 = 16.0;
const READER_SETTINGS_GAP: f32 = 13.0;

pub fn reader_choice_button_group(id: impl Into<ElementId>, buttons: impl IntoIterator<Item = Button>, theme: BrowserTheme) -> Stateful<Div> {
    // Full width so the group's right edge lands on the steppers' right edge
    // in the rows below; the flex_1 buttons stretch to fill it.
    crate::settings_choice_button_group(theme).id(id).children(buttons).w_full()
}

pub fn reader_match_count(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().flex_none().min_w(gpui::rems(3.25)).text_align(gpui::TextAlign::Right).text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(text.into())
}

fn reader_document_surface(theme: BrowserTheme) -> Div {
    div().size_full().min_h_0().min_w_0().bg(theme.page_bg).overflow_hidden()
}

pub fn reader_document_message(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    reader_document_surface(theme).flex().items_center().justify_center().text_color(theme.text_muted).text_size(gpui::rems(TEXT_SM)).child(text.into())
}

pub fn reader_pdf_scroll_surface(scroll_handle: &ScrollHandle, theme: BrowserTheme, cx: &mut App) -> Stateful<Div> {
    crate::register_page_scroll_handle("pdf-reader-scroll", scroll_handle.clone(), cx);
    reader_document_surface(theme).id("pdf-reader-scroll").track_scroll(scroll_handle).flex().items_start().justify_start()
}

/// The split divider and tab strip own the surrounding borders; the TOC body
/// adds none of its own.
pub fn reader_toc(theme: BrowserTheme) -> Div {
    div().size_full().min_h_0().min_w_0().flex().flex_col().gap(px(SPACE_XXS)).bg(theme.page_bg).on_scroll_wheel(|_, _, cx| cx.stop_propagation())
}

/// Space for the scrollbar's 16px track, including its thumb insets.
pub const READER_SCROLLBAR_GUTTER_PX: f32 = SPACE_MD;

/// Scrollable reader-sidebar panel: full size, themed surface, own scroll.
pub fn reader_state_panel(theme: BrowserTheme) -> Scrollable<Div> {
    div().size_full().min_h_0().min_w_0().pr(px(READER_SCROLLBAR_GUTTER_PX)).overflow_y_scrollbar().flex().flex_col().bg(theme.page_bg)
}

/// The reader settings tab body: the same sections `reader_settings_dropdown`
/// draws as a popover, but full width/height for its tab rather than a fixed,
/// shadowed panel — it lives inside the sidebar's border, not floating over
/// it.
pub fn reader_settings_tab_body(theme: BrowserTheme) -> Scrollable<Div> {
    // Groups provide SPACE_MD on both sides. Settings retain their own scroll
    // handle and keyboard navigation without drawing a scrollbar or gutter.
    reader_state_panel(theme).pr_0().hide_scrollbar()
}

pub fn reader_state_card(id: impl Into<ElementId>, selected: bool, theme: BrowserTheme) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .relative()
        .flex()
        .flex_col()
        .gap(px(SPACE_XS))
        .pt(px(10.0))
        .pb(px(10.0))
        .pl(px(16.0))
        .pr(px(12.0))
        .border_b_1()
        .border_color(theme.rule)
        .bg(theme.page_bg)
        .hover(move |style| style.bg(if selected { theme.accent_wash_hover } else { theme.hover }))
        .when(selected, |card| card.bg(theme.accent_wash))
}

pub fn reader_annotation_group_heading(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().min_w_0().pt(px(12.0)).pb(px(SPACE_XS)).px(px(12.0)).text_size(gpui::rems(TEXT_XS)).font_features(crate::small_caps()).text_color(theme.text_muted).child(title.into())
}

pub fn reader_annotation_color_bar(color: u32) -> Div {
    div().absolute().left(px(6.0)).top(px(12.0)).bottom(px(12.0)).w(px(3.0)).bg(reader_annotation_color(color))
}

pub fn reader_state_quote(text: impl IntoElement, theme: BrowserTheme) -> Div {
    div().w_full().min_w_0().whitespace_normal().line_clamp(2).text_ellipsis().text_size(gpui::rems(TEXT_SM)).line_height(gpui::relative(1.35)).text_color(theme.text_muted).italic().child(text)
}

pub fn reader_state_note(text: impl IntoElement, theme: BrowserTheme) -> Div {
    div().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(text)
}

/// Smallest note field, in the absence of a note. Below two rows the box reads
/// as a single-line field and invites a single line back.
pub const READER_NOTE_MIN_ROWS: usize = 2;
/// The note grows to fit what is written in it; this is only the ceiling past
/// which the *panel* — placed against the room its anchor leaves — takes over
/// and scrolls. High enough that the panel is always the first to run out.
pub const READER_NOTE_MAX_ROWS: usize = 40;

/// Compact non-modal annotation editor. Its caller supplies absolute placement
/// relative to the document viewport and provides the action rows.
///
/// No quote: the panel is placed clear of the passage it belongs to, so the
/// passage is on screen, highlighted, a few millimetres away. Repeating it here
/// would be labelling something the reader can already see — and it cost the
/// panel two or three lines of the page it covers.
///
/// The note takes its height from its content. Only when the panel reaches the
/// height its anchor allows does anything scroll, and then it is the panel, so
/// a note is never a small scrolling box inside a panel with room to spare.
pub fn reader_annotation_editor(note: impl IntoElement, controls: impl IntoElement, scroll: &ScrollHandle, theme: BrowserTheme) -> Stateful<Div> {
    use gpui_component::scroll::ScrollableElement as _;

    // The panel is `modal_surface` plus editor geometry: an absolute `min_0`
    // pair and `relative` come along, neither of which moves an
    // absolutely-placed, content-sized panel.
    crate::modal_surface(theme)
        .id("reader-annotation-editor")
        .w(px(360.0))
        .max_w_full()
        // A scroll surface is full height unless it is told otherwise, and this
        // one is placed against a point in the page: it should be as tall as
        // the note in it, up to the room its anchor leaves.
        .h_auto()
        .occlude()
        .flex()
        .flex_col()
        .rounded(px(RADIUS_MD))
        .border_1()
        .border_color(theme.rule)
        .bg(theme.page_bg)
        .shadow(crate::overlay_shadow(theme))
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        // The scroll surface has neither padding nor a border, so its
        // scrollbar overlay matches the viewport exactly. Padding belongs to
        // the content; adding it to the overlay's bounds creates false overflow.
        .child(
            div().id("reader-annotation-scroll-area").w_full().min_h_0().flex_shrink(1.0).overflow_y_scroll().track_scroll(scroll).vertical_scrollbar(scroll).flex().flex_col().child(
                div()
                    .w_full()
                    .flex_none()
                    .p(px(SPACE_MD))
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_SM))
                    .child(div().w_full().flex_none().debug_selector(|| "reader-annotation-note-row".into()).p(px(SPACE_XS)).rounded(px(RADIUS_SM)).border_1().border_color(theme.rule).bg(theme.page_bg).child(note))
                    .child(controls),
            ),
        )
}

/// Editor controls run at `settings_control_size_rem()`, the shared settings
/// geometry, rather than a third name for the same 2rem step.

/// The annotation palette, in hue order.
///
/// Eight, which is what one row of swatches holds at the editor's width, and
/// far enough round the wheel that neighbours are still told apart. Hue order
/// rather than lightness: past four colours a reader looks for "the green one"
/// by where green falls on a spectrum, not by how light it is.
///
/// Restrained saturation keeps the colours subtle over text. Warm red and
/// cooler pink, and violet purple and blue, keep adjacent choices distinct.
/// On a greyscale screen, the style row provides the distinction.
///
/// The values are canonical: an annotation stores the `0xRRGGBB` it was given, so this
/// is the one place a colour may be named, and both the swatch and the renderer
/// read it from here.
pub const READER_ANNOTATION_COLORS: [(&str, u32); 8] = [("Yellow", 0xe0bb4b), ("Orange", 0xe09a57), ("Red", 0xd57868), ("Pink", 0xc880b2), ("Purple", 0x9d80c9), ("Blue", 0x6e9dcb), ("Teal", 0x57aaa1), ("Green", 0x94ad65)];

/// The colour a new annotation takes.
pub const READER_ANNOTATION_DEFAULT_COLOR: u32 = READER_ANNOTATION_COLORS[0].1;

/// What an annotation paints in, from the `0xRRGGBB` it stores.
///
/// E-ink lands the palette on three neighbouring greys, so there every
/// annotation paints black and the editor offers no colours. The stored colour
/// is untouched and still shows wherever the annotation syncs to.
pub const fn reader_annotation_paint_color(color: u32) -> u32 {
    if cfg!(feature = "kobo") { 0x000000 } else { color }
}

/// How opaque a highlight is over the text it marks. On e-ink, black at this
/// weight is a light grey, lighter than a search hit.
pub const READER_ANNOTATION_HIGHLIGHT_ALPHA: u8 = if cfg!(feature = "kobo") { 56 } else { 96 };

/// An annotation's `0xRRGGBB` for display.
pub fn reader_annotation_color(color: u32) -> gpui::Hsla {
    gpui::rgb(reader_annotation_paint_color(color)).into()
}

/// A full-width palette with evenly spaced swatches.
pub fn reader_annotation_color_group() -> Div {
    div().w_full().min_w_0().flex_none().flex().flex_row().items_center().justify_between()
}

/// One highlight colour, shown rather than named.
///
/// The selected colour is indicated by the swatch's background tint.
pub fn reader_annotation_color_swatch(id: impl Into<ElementId>, color: u32, name: &'static str, selected: bool, theme: BrowserTheme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || format!("reader-annotation-color-{name}"))
        .w(gpui::rems(settings_control_size_rem()))
        .h(gpui::rems(settings_control_size_rem()))
        .min_w(px(20.0))
        .flex_shrink_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_SM))
        .cursor_pointer()
        .when(selected, |swatch| swatch.bg(theme.accent_wash))
        .hover(move |style| style.bg(theme.hover))
        .focus_visible(move |style| style.bg(theme.accent_wash))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(if selected { format!("{name} (selected)") } else { name.to_owned() }).build(window, cx))
        .child(
            div()
                .size(px(16.0))
                .max_w_full()
                .flex_none()
                .rounded_full()
                .border_1()
                .border_color(theme.border)
                .bg(reader_annotation_color(color))
                .when(selected, |circle| circle.debug_selector(move || format!("reader-annotation-color-selected-{name}"))),
        )
}

/// A compact text action in the annotation editor.
pub fn reader_annotation_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    crate::selectable_button(outlined_text_button(id, label).h(gpui::rems(settings_control_size_rem())).rounded(px(RADIUS_SM)).border_color(theme.rule), selected, theme)
}

/// Width of the disclosure column. Doubles as the row's left inset and as the
/// per-depth indent step, so a nested title starts exactly where its parent's
/// title does, one chevron further in.
pub fn reader_settings_dropdown(window: &Window, theme: BrowserTheme) -> Scrollable<Div> {
    let available_height = (f32::from(window.viewport_size().height) - reader_toolbar_size_px(window) - READER_SETTINGS_POPOVER_GAP - READER_SETTINGS_POPOVER_WINDOW_MARGIN).max(1.0);
    let max_height = available_height.min(READER_SETTINGS_MAX_HEIGHT);
    div()
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .w(gpui::rems(READER_SETTINGS_MAX_WIDTH / 16.0))
        .max_h(px(max_height))
        .overflow_y_scrollbar()
        .hide_scrollbar()
        .rounded(px(RADIUS_MD))
        .border_1()
        .border_color(theme.rule)
        .shadow(crate::overlay_shadow(theme))
        .p(px(READER_SETTINGS_PADDING))
        .flex()
        .flex_col()
        .gap(px(READER_SETTINGS_GAP))
        .bg(theme.page_bg)
}

pub fn reader_settings_section(label: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    reader_settings_group(theme).child(div().pb(px(SPACE_XXS)).text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(label.into()))
}

/// A settings group with no heading text, delineated by a border instead —
/// for panels where the surrounding tabs or layout already make each group's
/// purpose clear.
pub fn reader_settings_group(theme: BrowserTheme) -> Div {
    // Matching top/bottom padding keeps the first and last row equidistant
    // from the group's own edges, so the group reads as centred rather than
    // hugging its border. Horizontal padding lives here rather than on the
    // tab body, so the border-bottom below it spans full width, edge to edge
    // with the sidebar's own borders.
    div().w_full().min_w_0().flex().flex_col().gap(px(SPACE_XS)).p(px(SPACE_MD)).border_b_1().border_color(theme.rule)
}

pub fn reader_setting_row(label: impl Into<SharedString>) -> Div {
    div().w_full().min_h(gpui::rems(2.375)).flex().items_center().gap(px(SPACE_SM)).child(div().w(gpui::rems(7.0)).max_w(px(112.0)).flex_none().whitespace_nowrap().text_size(gpui::rems(TEXT_SM)).child(label.into())).child(div().flex_1())
}

/// A full-width checkbox row with its label on the left and check on the right.
pub fn reader_setting_checkbox(id: impl Into<ElementId>, label: impl Into<SharedString>, checked: bool, theme: BrowserTheme) -> Checkbox {
    let label: SharedString = label.into();
    Checkbox::new(id).label(label).checked(checked).w_full().min_h(gpui::rems(2.375)).flex_row_reverse().items_center().text_size(gpui::rems(TEXT_SM)).text_color(theme.text)
}

/// A button-group row: a small-caps label above a full-width choice group,
/// rather than the stepper row's side-by-side label-and-value layout.
pub fn reader_setting_row_smallcaps(label: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().flex().flex_col().gap(px(SPACE_XXS)).child(div().text_size(gpui::rems(TEXT_XS)).font_features(crate::small_caps()).text_color(theme.text_muted).child(label.into()))
}

/// The places the reader's chrome leads to. Each has one icon, used by the
/// desktop rail and the phone's bars alike.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReaderDestination {
    Library,
    Contents,
    Notes,
    Search,
    Settings,
    Tts,
}

impl ReaderDestination {
    pub fn icon(self) -> gpui_component::Icon {
        gpui_component::Icon::empty().path(match self {
            Self::Library => "icons/arrow-left.svg",
            Self::Contents => "icons/menu.svg",
            Self::Notes => "icons/replace.svg",
            Self::Search => "icons/search.svg",
            Self::Settings => "icons/text-aa.svg",
            Self::Tts => "icons/headphones.svg",
        })
    }

    /// Settings uses live text so its Aa follows the interface typeface.
    fn glyph(self, size_rem: f32, theme: BrowserTheme) -> AnyElement {
        if self == Self::Settings {
            div()
                .size(gpui::rems(size_rem))
                .flex()
                .items_center()
                .justify_center()
                .when(!theme.custom_ui_font, |text| text.font_family(theme.ui_font_family.family_name()))
                .text_size(gpui::rems(size_rem))
                .line_height_auto()
                .child("Aa")
                .into_any_element()
        } else {
            self.icon().size(gpui::rems(size_rem)).into_any_element()
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Contents => "Contents",
            Self::Notes => "Notes",
            Self::Search => "Search",
            Self::Settings => "Settings",
            Self::Tts => "TTS",
        }
    }

    /// Keep keyboard hints consistent across the rail and mobile controls.
    pub fn tooltip(self, label: impl Into<SharedString>, cx: &gpui::App) -> SharedString {
        let label = label.into();
        let shortcut = match self {
            Self::Library => "backspace",
            Self::Contents => return crate::configured_shortcut_tooltip(label, "reader.contents", cx),
            Self::Search => return crate::configured_shortcut_tooltip(label, "reader.search", cx),
            Self::Settings => return crate::configured_shortcut_tooltip(label, "reader.settings", cx),
            Self::Notes | Self::Tts => return label,
        };
        crate::shortcut_tooltip(label, shortcut)
    }
}

/// A sidebar panel's own header: its name and a close button.
pub fn reader_panel_header(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .flex_none()
        .w_full()
        .min_w_0()
        .h(gpui::rems(2.75))
        .flex()
        .items_center()
        .gap(px(SPACE_SM))
        .pl(px(SPACE_MD))
        .pr(px(SPACE_XS))
        .border_b_1()
        .border_color(theme.rule)
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_features(crate::small_caps()).child(title.into()))
}

/// The handle at the top of a phone sheet, which a downward swipe closes.
pub fn reader_sheet_grab(theme: BrowserTheme) -> Div {
    div().flex_none().w_full().pt(px(SPACE_SM)).pb(px(SPACE_XS)).flex().justify_center().child(div().w(px(36.0)).h(px(4.0)).rounded_full().bg(theme.rule))
}

/// Book and current chapter on one line, joined as "Title: Chapter".
pub fn reader_top_bar_title(title: impl Into<SharedString>, chapter: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    let title = title.into();
    let chapter = chapter.into();
    let title = title.trim();
    let chapter = chapter.trim();
    let heading = if title.is_empty() {
        chapter.to_owned()
    } else if chapter.is_empty() || chapter == title {
        title.to_owned()
    } else {
        format!("{title}: {chapter}")
    };
    div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_center().text_size(gpui::rems(TEXT_SM)).text_color(theme.text).child(heading)
}

/// On a phone the only place reading progress is shown.
pub fn reader_top_bar_progress(label: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().flex_none().pl(px(SPACE_XS)).text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(label.into())
}

/// A phone bar action below the page: the library's bottom bar item over the
/// destination's glyph. Reading aloud marks its glyph with the rail's activity
/// dot, and tints it while it plays.
pub fn reader_bottom_bar_item(id: impl Into<ElementId>, destination: ReaderDestination, mark: ReaderRailMark, theme: BrowserTheme, cx: &gpui::App) -> Button {
    let glyph = div()
        .relative()
        .when(mark == ReaderRailMark::Active, |glyph| glyph.text_color(theme.accent))
        .child(destination.glyph(crate::NAV_RAIL_ICON_SIZE_REM, theme))
        .children(reader_activity_dot(mark, theme).map(|dot| dot.top(px(-2.0)).right(px(-6.0))));
    crate::browser::bottom_navigation_tile(id, glyph, destination.label(), mark == ReaderRailMark::Selected, theme).tooltip(destination.tooltip(destination.label(), cx))
}

/// Width of the reader's persistent icon rail. The panel's resizable width
/// is measured beside it.
pub const READER_RAIL_WIDTH: f32 = 52.0;
const READER_RAIL_ITEM_SIZE: f32 = 40.0;
const READER_PROGRESS_RING_SIZE: f32 = 28.0;
const READER_PROGRESS_RING_STROKE: f32 = 3.0;

/// The sidebar's column of destinations: navigation at the top, tools at the
/// foot, ruled off from the panel beside it.
pub fn reader_rail(theme: BrowserTheme) -> Div {
    div().w(px(READER_RAIL_WIDTH)).flex_none().h_full().min_h_0().flex().flex_col().items_center().gap(px(SPACE_XS)).py(px(SPACE_SM)).border_r_1().border_color(theme.rule).bg(theme.page_bg)
}

/// Pushes the rail's tools to its foot.
pub fn reader_rail_spacer() -> Div {
    div().flex_1().min_h_0()
}

/// What a rail item shows besides its icon.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReaderRailMark {
    None,
    /// The open panel: solid accent with contrasting text.
    Selected,
    /// Something running: an accent icon with a filled dot.
    Active,
    /// Something paused: a hollow dot.
    Paused,
}

/// One rail destination: an icon, named by its tooltip.
pub fn reader_rail_item(id: impl Into<ElementId>, destination: ReaderDestination, label: impl Into<SharedString>, mark: ReaderRailMark, theme: BrowserTheme, cx: &gpui::App) -> Stateful<Div> {
    let label = destination.tooltip(label, cx);
    let ink = match mark {
        ReaderRailMark::Selected => theme.text,
        ReaderRailMark::Active => theme.accent,
        ReaderRailMark::None | ReaderRailMark::Paused => theme.text_muted,
    };
    div()
        .id(id)
        .relative()
        .flex_none()
        .size(px(READER_RAIL_ITEM_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_SM))
        .cursor_pointer()
        .text_color(ink)
        .selectable(mark == ReaderRailMark::Selected, theme)
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
        .child(destination.glyph(1.25, theme))
        .children(reader_activity_dot(mark, theme).map(|dot| dot.top(px(7.0)).right(px(7.0))))
}

/// The dot that says something is running (filled) or paused (hollow), for
/// the caller to place over the item it marks.
fn reader_activity_dot(mark: ReaderRailMark, theme: BrowserTheme) -> Option<Div> {
    let dot = div().absolute().size(px(7.0)).rounded_full();
    match mark {
        ReaderRailMark::Active => Some(dot.bg(theme.accent)),
        ReaderRailMark::Paused => Some(dot.border_1().border_color(theme.text_muted)),
        ReaderRailMark::None | ReaderRailMark::Selected => None,
    }
}

/// How far through the book, as a ring with the percentage inside. On
/// desktop this is the one place reading progress is shown.
pub fn reader_progress_ring(fraction: f32, label: impl Into<SharedString>, detail: impl Into<SharedString>, theme: BrowserTheme) -> Stateful<Div> {
    let fraction = fraction.clamp(0.0, 1.0);
    let detail = detail.into();
    let ring = gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = (READER_PROGRESS_RING_SIZE - READER_PROGRESS_RING_STROKE) / 2.0;
            // A circle drawn as a polyline: fine enough at this size, and the
            // same stroke builder the graph view uses.
            let arc = |from: f32, to: f32, color: gpui::Hsla, window: &mut Window| {
                let steps = ((to - from) * 64.0).ceil().max(1.0) as usize;
                let point_at = |turn: f32| {
                    let angle = (turn - 0.25) * std::f32::consts::TAU;
                    center + gpui::point(px(radius * angle.cos()), px(radius * angle.sin()))
                };
                let mut builder = gpui::PathBuilder::stroke(px(READER_PROGRESS_RING_STROKE));
                builder.move_to(point_at(from));
                for step in 1..=steps {
                    builder.line_to(point_at(from + (to - from) * step as f32 / steps as f32));
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            };
            arc(0.0, 1.0, theme.rule, window);
            if fraction > 0.0 {
                arc(0.0, fraction, theme.accent, window);
            }
        },
    )
    .absolute()
    .size_full();
    div()
        .id("reader-progress-ring")
        .relative()
        .flex_none()
        .size(px(READER_PROGRESS_RING_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(9.0))
        .text_color(theme.text_muted)
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(detail.clone()).build(window, cx))
        .child(ring)
        .child(label.into())
}

/// One setting's choices as equal-width picture tiles in a row.
pub fn reader_tile_group(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).w_full().min_w_0().flex().flex_row().gap(px(SPACE_SM))
}

/// A choice drawn as a picture of its effect above its name, using the shared
/// selected appearance for interactive controls.
pub fn reader_picture_tile(id: impl Into<ElementId>, picture: impl IntoElement, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SPACE_XXS))
        .pt(px(SPACE_XS))
        .pb(px(SPACE_XS))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(theme.rule)
        .bg(theme.page_bg)
        .text_color(if selected { theme.text } else { theme.text_muted })
        .cursor_pointer()
        .selectable(selected, theme)
        .child(picture)
        .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_center().text_size(gpui::rems(TEXT_XS)).child(label.into()))
}

/// Minimum width for two independent stepper targets and a readable value.
pub fn reader_numeric_tile_width_rem() -> f32 {
    2.0 * settings_control_size_rem() + 3.25
}

fn numeric_tile_stepper(button: Button, selected: bool, theme: BrowserTheme) -> Button {
    // Kobo marks selection with a border and retains the paper background.
    let filled = selected && !cfg!(feature = "kobo");
    let foreground = if filled { theme.accent_text } else { theme.text };
    let hovered = if filled { theme.accent_hover } else { theme.hover };
    button.text_color(foreground).hover(move |style| style.bg(hovered))
}

/// A custom choice with its saved value beneath the lines icon. The outer
/// surface groups three controls; stepper callbacks stop click propagation so
/// changing a value never also activates the central selection control.
pub fn reader_numeric_tile(id: &'static str, value: impl Into<SharedString>, selected: bool, decrement: Button, increment: Button, theme: BrowserTheme, select: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    let value = value.into();
    div()
        .id(id)
        .role(Role::Group)
        .debug_selector(move || id.into())
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(theme.rule)
        .bg(theme.page_bg)
        .text_color(if selected { theme.text } else { theme.text_muted })
        .selectable(selected, theme)
        .child(numeric_tile_stepper(decrement, selected, theme))
        .child(
            div()
                .id((id, 0usize))
                .role(Role::Button)
                .debug_selector(move || format!("{id}-value"))
                .on_click(select)
                .aria_label(format!("Use custom value: {value}"))
                .aria_selected(selected)
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(SPACE_XXS))
                .py(px(SPACE_XS))
                .cursor_pointer()
                .child(reader_tile_picture("icons/reader-settings/line-spacing-normal.svg"))
                .child(div().text_size(gpui::rems(TEXT_XS)).whitespace_nowrap().child(value)),
        )
        .child(numeric_tile_stepper(increment, selected, theme))
}

/// The drawing on a settings tile, from the `icons/reader-settings/` set.
pub fn reader_tile_picture(path: &'static str) -> gpui_component::Icon {
    gpui_component::Icon::empty().path(path).size(gpui::rems(2.25))
}

/// A typeface tile's sample: "Aa" set in the face it selects. `None` is the
/// book's own face, which the panel can only suggest with the interface serif.
pub fn reader_typeface_sample(family: Option<&'static str>) -> Div {
    div().h(gpui::rems(2.25)).flex().items_center().text_size(gpui::rems(1.5)).when_some(family, |sample, family| sample.font_family(family)).child("Aa")
}

/// The query field fills the space beside the search controls.
pub fn reader_search_box(theme: BrowserTheme) -> Div {
    div().flex_1().min_w_0().h(gpui::rems(topbar_action_height_rem())).overflow_hidden().rounded(px(RADIUS_SM)).border_1().border_color(theme.border).bg(theme.page_bg)
}

/// Search shares the sidebar's inset, with space above and below its controls.
pub fn reader_search_bar(theme: BrowserTheme) -> Div {
    div().flex_none().w_full().min_w_0().px(px(CONTENT_INSET)).py(px(SPACE_SM)).flex().items_center().gap(px(SPACE_SM)).bg(theme.page_bg).border_b_1().border_color(theme.rule)
}

pub fn reader_footnote_panel(theme: BrowserTheme) -> Scrollable<Div> {
    div().max_h(px(240.0)).flex_none().occlude().overflow_y_scrollbar().px(px(SPACE_MD)).py(px(SPACE_MD)).flex().flex_col().gap(px(SPACE_SM)).bg(theme.page_bg).border_t_1().border_color(theme.rule)
}

fn reader_image_preview(source: impl Into<ImageSource>) -> Img {
    img(source).size_full().object_fit(ObjectFit::Contain)
}

fn reader_modal_panel(width: Pixels, height: Pixels, theme: BrowserTheme) -> Div {
    crate::modal_surface(theme).w(width).h(height).max_w_full().max_h_full().p(px(SPACE_MD)).flex().flex_col().gap(px(SPACE_SM))
}

type ReaderPopupCallback = Rc<dyn Fn(&mut Window, &mut App)>;

/// Shared centered reader-modal shell. Specialized popups provide only their
/// content, requested size, and dismissal behavior.
#[derive(IntoElement)]
struct ReaderModal {
    size: Size<Pixels>,
    content: gpui::AnyElement,
    on_dismiss: ReaderPopupCallback,
}

impl ReaderModal {
    fn new(size: Size<Pixels>, content: impl IntoElement) -> Self {
        Self { size, content: content.into_any_element(), on_dismiss: Rc::new(|_, _| {}) }
    }

    fn on_dismiss(mut self, callback: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Rc::new(callback);
        self
    }
}

impl RenderOnce for ReaderModal {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = browser_theme(cx);
        let dismiss = self.on_dismiss;
        crate::anchor_to_window(
            crate::modal_overlay().child(crate::modal_scrim("reader-modal-scrim", theme).on_click(move |_, window, cx| dismiss(window, cx))).child(reader_modal_panel(self.size.width, self.size.height, theme).child(self.content)),
            window,
        )
    }
}

/// Reader-specific semantic sections layered on GPUI Component's native
/// `PopupMenu`. This keeps labels, ordering, and separators consistent while
/// retaining the toolkit's focus and keyboard behavior.
pub struct ReaderContextMenu {
    menu: PopupMenu,
    has_section: bool,
}

impl ReaderContextMenu {
    pub fn new(menu: PopupMenu) -> Self {
        Self { menu, has_section: false }
    }

    fn begin_section(mut self) -> Self {
        if self.has_section {
            self.menu = self.menu.separator();
        }
        self.has_section = true;
        self
    }

    /// A PDF selection can become an annotation or be copied.
    pub fn pdf_selection(mut self, on_annotate: impl Fn(&mut Window, &mut App) + 'static, on_copy: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self = self.begin_section();
        self.menu = self.menu.item(menu_item("Annotation", move |_, window, cx| on_annotate(window, cx))).item(menu_item("Copy", move |_, window, cx| on_copy(window, cx)));
        self
    }

    pub fn table(mut self, on_markdown: impl Fn(&mut Window, &mut App) + 'static, on_unstyled_html: impl Fn(&mut Window, &mut App) + 'static, on_styled_html: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self = self.begin_section();
        self.menu = self
            .menu
            .item(menu_item("Copy Table as Markdown", move |_, window, cx| on_markdown(window, cx)))
            .item(menu_item("Copy Table as Unstyled HTML", move |_, window, cx| on_unstyled_html(window, cx)))
            .item(menu_item("Copy Table as Styled HTML", move |_, window, cx| on_styled_html(window, cx)));
        self
    }

    pub fn image(mut self, on_copy: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self = self.begin_section();
        self.menu = self.menu.item(menu_item("Copy Image", move |_, window, cx| on_copy(window, cx)));
        self
    }

    pub fn finish(self) -> PopupMenu {
        self.menu
    }
}

fn image_popup_panel_size(reader_width: f32, reader_height: f32, intrinsic: Option<(f32, f32)>) -> (f32, f32) {
    const OVERLAY_INSET: f32 = 48.0;
    const PANEL_HORIZONTAL_CHROME: f32 = 24.0;
    const PANEL_VERTICAL_CHROME: f32 = 92.0;
    const MIN_PANEL_WIDTH: f32 = 280.0;
    const MIN_PANEL_HEIGHT: f32 = 180.0;
    const MAX_PANEL_WIDTH: f32 = 1100.0;
    const MAX_PANEL_HEIGHT: f32 = 820.0;

    let available_width = (reader_width - OVERLAY_INSET).max(1.0).min(MAX_PANEL_WIDTH);
    let available_height = (reader_height - OVERLAY_INSET).max(1.0).min(MAX_PANEL_HEIGHT);
    let image_limit_width = (available_width - PANEL_HORIZONTAL_CHROME).max(1.0);
    let image_limit_height = (available_height - PANEL_VERTICAL_CHROME).max(1.0);
    let (image_width, image_height) = intrinsic.filter(|(width, height)| width.is_finite() && height.is_finite() && *width > 0.0 && *height > 0.0).unwrap_or((4.0, 3.0));
    let scale = (image_limit_width / image_width).min(image_limit_height / image_height);
    let displayed_width = image_width * scale;
    let displayed_height = image_height * scale;
    ((displayed_width + PANEL_HORIZONTAL_CHROME).clamp(MIN_PANEL_WIDTH.min(available_width), available_width), (displayed_height + PANEL_VERTICAL_CHROME).clamp(MIN_PANEL_HEIGHT.min(available_height), available_height))
}

/// Centered, responsive image viewer used by reader surfaces.
#[derive(IntoElement)]
pub struct ReaderImagePopup {
    uri: SharedString,
    image: Arc<Image>,
    reader_size: Size<Pixels>,
    on_copy: ReaderPopupCallback,
    on_dismiss: ReaderPopupCallback,
}

impl ReaderImagePopup {
    pub fn new(uri: impl Into<SharedString>, image: Arc<Image>, reader_size: Size<Pixels>) -> Self {
        Self { uri: uri.into(), image, reader_size, on_copy: Rc::new(|_, _| {}), on_dismiss: Rc::new(|_, _| {}) }
    }

    pub fn on_copy(mut self, callback: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_copy = Rc::new(callback);
        self
    }

    pub fn on_dismiss(mut self, callback: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Rc::new(callback);
        self
    }
}

impl RenderOnce for ReaderImagePopup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = browser_theme(cx);
        let intrinsic = self.image.clone().use_render_image(window, cx).map(|rendered| {
            let size = rendered.size(0);
            (size.width.0 as f32, size.height.0 as f32)
        });
        let (panel_width, panel_height) = image_popup_panel_size(f32::from(self.reader_size.width), f32::from(self.reader_size.height), intrinsic);
        let copy = self.on_copy;
        let dismiss_modal = self.on_dismiss.clone();
        let dismiss_button = self.on_dismiss;
        let content = div().size_full().min_h_0().min_w_0().flex().flex_col().gap(px(SPACE_SM)).child(reader_state_note(self.uri, theme)).child(reader_content().child(reader_image_preview(self.image))).child(
            crate::action_row()
                .child(crate::topbar_action_button("reader-image-copy", "Copy image", IconName::Copy, theme).on_click(move |_, window, cx| copy(window, cx)))
                .child(crate::topbar_action_button("reader-image-close", "Close", IconName::Close, theme).on_click(move |_, window, cx| dismiss_button(window, cx))),
        );

        ReaderModal::new(gpui::size(px(panel_width), px(panel_height)), content).on_dismiss(move |window, cx| dismiss_modal(window, cx))
    }
}

pub fn reader_document_body() -> Div {
    div().relative().size_full().min_h_0().min_w_0().flex().flex_col().overflow_hidden()
}

pub fn reader_content() -> Div {
    div().flex_1().min_h_0()
}

#[cfg(all(test, feature = "mobile"))]
mod numeric_tile_tests;
