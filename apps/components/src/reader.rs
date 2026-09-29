use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Div, Element, ElementId, FontWeight, GlobalElementId, Image, ImageSource, Img, LayoutId, MouseButton, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, RenderOnce, ScrollHandle, SharedString, Size, Stateful, Style,
    StyledImage, Window, div, img, px,
};
use gpui_component::IconName;
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::menu::PopupMenu;
use gpui_component::popover::Popover;
use gpui_component::{ElementExt, Sizable};

use crate::{BrowserTheme, RADIUS_MD, RADIUS_SM, SETTINGS_CONTROL_SIZE_REM, SPACE_MD, SPACE_SM, SPACE_XS, SPACE_XXS, ScrollableElement, TEXT_SM, TEXT_XS, base_button, browser_theme, menu_item, outlined_text_button, scroll::Scrollable};

/// The toolbar and its controls grow together with the UI font. The same 2rem
/// step as `TOPBAR_ACTION_HEIGHT_REM`, kept under its own name because the
/// toolbar's height feeds its own geometry math below.
pub const READER_TOOLBAR_SIZE_REM: f32 = crate::TOPBAR_ACTION_HEIGHT_REM;
pub fn reader_toolbar_size_px(window: &Window) -> f32 {
    f32::from(gpui::rems(READER_TOOLBAR_SIZE_REM).to_pixels(window.rem_size()))
}
/// Height of the reader's mobile bottom bar. Shares the library shell's
/// bottom-nav height rather than inventing a second size for the same kind of
/// row.
pub const READER_BOTTOM_BAR_HEIGHT: f32 = crate::BOTTOM_NAV_HEIGHT_REM * 16.0;
const READER_SETTINGS_POPOVER_GAP: f32 = 4.0;
const READER_SETTINGS_POPOVER_WINDOW_MARGIN: f32 = 8.0;
const READER_SETTINGS_MAX_WIDTH: f32 = 400.0;
const READER_SETTINGS_MAX_HEIGHT: f32 = 620.0;
const READER_SETTINGS_PADDING: f32 = 16.0;
const READER_SETTINGS_GAP: f32 = 13.0;

pub fn reader_toolbar(theme: BrowserTheme) -> Div {
    div().h(gpui::rems(READER_TOOLBAR_SIZE_REM)).flex_none().w_full().min_w_0().px(px(crate::CONTENT_INSET)).flex().items_center().gap(px(SPACE_SM)).bg(theme.page_bg).border_b_1().border_color(theme.rule)
}

/// `.ghost()`, not `outlined_icon_button`: the header's square icon buttons
/// (back, close, search) sit borderless against the header's own bottom
/// border, not individually outlined.
pub fn reader_control_button_icon(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: IconName, theme: BrowserTheme) -> Button {
    base_button(id).ghost().icon(icon).size(gpui::rems(READER_TOOLBAR_SIZE_REM)).rounded(px(RADIUS_SM)).px_0().bg(theme.page_bg).tooltip(label)
}

/// Chapter title beside progress, yielding space to the fixed toolbar controls.
pub fn reader_toolbar_title(title: impl Into<SharedString>) -> Div {
    div().flex_initial().min_w_0().max_w(px(520.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_align(gpui::TextAlign::Left).text_size(gpui::rems(TEXT_SM)).child(title.into())
}

pub fn reader_choice_button(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    // Use the same selected-button path as the reader's active contents row:
    // solid accent and inverted ink. `ButtonCustomVariant` is superseded by
    // ButtonGroup's segment styling, which was why this previously rendered
    // as the pale accent wash instead.
    // The group below owns the joined outline and segment edges. These are
    // deliberately plain children, with only the selected child receiving
    // reader-specific colour refinement.
    let base = base_button(id).label(label).with_size(gpui_component::Size::Small).h(gpui::rems(SETTINGS_CONTROL_SIZE_REM)).flex_1().rounded(px(RADIUS_SM)).px(px(SPACE_XS));
    crate::selectable_button(base, selected, theme).when(selected, |button| button.font_weight(FontWeight::SEMIBOLD))
}

pub fn reader_choice_button_group(id: impl Into<ElementId>, buttons: impl IntoIterator<Item = Button>) -> ButtonGroup {
    // Full width so the group's right edge lands on the steppers' right edge
    // in the rows below; the flex_1 buttons stretch to fill it.
    ButtonGroup::new(id).children(buttons).outline().w_full()
}

pub fn reader_match_count(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().flex_none().min_w(gpui::rems(3.25)).text_align(gpui::TextAlign::Right).text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(text.into())
}

fn reader_settings_trigger(theme: BrowserTheme) -> gpui_component::button::Button {
    reader_control_button_icon("reader-settings-trigger", "Aa", IconName::Settings2, theme)
}

/// How far through the book, as a single percentage.
///
/// Deliberately just the number: the toolbar is permanent chrome over the page,
/// and location and section counts are detail the tooltip can carry. The fixed
/// width keeps the title beside it from reflowing on every page turn.
pub fn reader_progress(label: impl Into<SharedString>, detail: impl Into<SharedString>, theme: BrowserTheme) -> Stateful<Div> {
    div()
        .id("reader-progress")
        .flex_none()
        .w(gpui::rems(2.75))
        .px(px(SPACE_XS))
        .text_align(gpui::TextAlign::Right)
        .text_size(gpui::rems(TEXT_SM))
        .text_color(theme.text_muted)
        .tooltip({
            let detail = detail.into();
            move |window, cx| gpui_component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
        })
        .child(label.into())
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

/// The border between sidebar and document is drawn once, by the sidebar's
/// own outer container (`reader_sidebar` in `shell/toolbar.rs`) — a tab body
/// sits inside that border, not beside it, so it draws none of its own.
pub fn reader_toc(theme: BrowserTheme) -> Div {
    div().size_full().min_h_0().min_w_0().flex().flex_col().gap(px(SPACE_XXS)).bg(theme.page_bg).border_t_1().border_color(theme.rule).on_scroll_wheel(|_, _, cx| cx.stop_propagation())
}

/// Scrollable reader-sidebar panel: full size, themed surface, own scroll.
fn reader_scroll_panel(theme: BrowserTheme) -> Scrollable<Div> {
    div().size_full().min_h_0().min_w_0().overflow_y_scrollbar().flex().flex_col().bg(theme.page_bg)
}

pub fn reader_state_panel(theme: BrowserTheme) -> Scrollable<Div> {
    reader_scroll_panel(theme)
}

/// The reader settings tab body: the same sections `reader_settings_dropdown`
/// draws as a popover, but full width/height for its tab rather than a fixed,
/// shadowed panel — it lives inside the sidebar's border, not floating over
/// it.
pub fn reader_settings_tab_body(theme: BrowserTheme) -> Scrollable<Div> {
    // Groups carry their own content padding, while their borders and fills
    // reach the sidebar edges like the Contents and Notes rows.
    reader_scroll_panel(theme)
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
        .hover(|style| style.bg(theme.hover))
        .when(selected, |card| card.bg(theme.accent_wash))
}

pub fn reader_annotation_group_heading(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().min_w_0().pt(px(12.0)).pb(px(SPACE_XS)).px(px(12.0)).text_size(gpui::rems(TEXT_XS)).font_features(crate::small_caps()).text_color(theme.text_muted).child(title.into())
}

pub fn reader_annotation_color_bar(color: &str) -> Div {
    div().absolute().left(px(6.0)).top(px(12.0)).bottom(px(12.0)).w(px(3.0)).bg(reader_annotation_color(color))
}

pub fn reader_state_quote(text: impl Into<SharedString>) -> Div {
    div().text_size(gpui::rems(TEXT_SM)).line_height(gpui::relative(1.35)).italic().child(text.into())
}

pub fn reader_state_note(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(text.into())
}

/// Smallest note field, in the absence of a note. Below two rows the box reads
/// as a single-line field and invites a single line back.
pub const READER_NOTE_MIN_ROWS: usize = 2;
/// The note grows to fit what is written in it; this is only the ceiling past
/// which the *panel* — placed against the room its anchor leaves — takes over
/// and scrolls. High enough that the panel is always the first to run out.
pub const READER_NOTE_MAX_ROWS: usize = 40;

/// Compact non-modal annotation editor. Its caller supplies absolute placement
/// relative to the document viewport and appends the action rows.
///
/// No quote: the panel is placed clear of the passage it belongs to, so the
/// passage is on screen, highlighted, a few millimetres away. Repeating it here
/// would be labelling something the reader can already see — and it cost the
/// panel two or three lines of the page it covers.
///
/// The note takes its height from its content. Only when the panel reaches the
/// height its anchor allows does anything scroll, and then it is the panel, so
/// a note is never a small scrolling box inside a panel with room to spare.
pub fn reader_annotation_editor(note: impl IntoElement, theme: BrowserTheme) -> Scrollable<Stateful<Div>> {
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
        .overflow_y_scrollbar()
        .occlude()
        .p(px(SPACE_MD))
        .flex()
        .flex_col()
        .gap(px(SPACE_SM))
        .rounded(px(RADIUS_MD))
        .border_1()
        .border_color(theme.rule)
        .bg(theme.page_bg)
        .shadow(crate::overlay_shadow(theme))
        .child(div().w_full().flex_none().p(px(SPACE_XS)).rounded(px(RADIUS_SM)).border_1().border_color(theme.rule).bg(theme.page_bg).child(note))
}

/// Editor controls run at `SETTINGS_CONTROL_SIZE_REM`, the shared settings
/// geometry, rather than a third name for the same 2rem step.

/// The highlight palette, in hue order.
///
/// Eight, which is what one row of swatches holds at the editor's width, and
/// far enough round the wheel that neighbours are still told apart. Hue order
/// rather than lightness: past four colours a reader looks for "the green one"
/// by where green falls on a spectrum, not by how light it is.
///
/// All eight sit at a similar lightness so none of them buries the words it is
/// laid over — which is also why they collapse on a greyscale screen. There,
/// the style row is the distinction that survives.
///
/// The values are canonical: an annotation stores the hex it was given, so this
/// is the one place a colour may be named, and both the swatch and the renderer
/// read it from here.
pub const READER_ANNOTATION_COLORS: [(&str, &str); 8] = [("Yellow", "#f6c945"), ("Orange", "#fb923c"), ("Red", "#f87171"), ("Pink", "#f472b6"), ("Purple", "#c084fc"), ("Blue", "#60a5fa"), ("Teal", "#2dd4bf"), ("Green", "#4ade80")];

/// The default a new annotation takes, and the fallback for an unreadable one,
/// in the two spellings the readers need: EPUB stores and parses hex strings,
/// the PDF overlay packs channels into an integer. `colors_agree_on_the_default`
/// keeps the pair honest.
pub const READER_ANNOTATION_DEFAULT_COLOR: &str = READER_ANNOTATION_COLORS[0].1;
pub const READER_ANNOTATION_DEFAULT_RGB: u32 = 0xf6c945;

/// Parses one of [`READER_ANNOTATION_COLORS`] for display.
pub fn reader_annotation_color(hex: &str) -> gpui::Hsla {
    let parsed = hex.strip_prefix('#').and_then(|value| u32::from_str_radix(value, 16).ok()).filter(|_| hex.len() == 7);
    gpui::rgb(parsed.unwrap_or(0xf6c945)).into()
}

/// The group the swatches sit in: one bordered strip, like a choice group.
pub fn reader_annotation_color_group() -> Div {
    div().flex_none().flex().flex_row().items_center().gap(px(SPACE_XXS)).p(px(SPACE_XXS)).rounded(px(RADIUS_SM)).border_1().border_color(gpui::transparent_black())
}

/// One highlight colour, shown rather than named.
///
/// The circle is the colour itself, so nothing here is a word: the name goes to
/// the tooltip, where a reader who needs it can ask. The chosen one is picked
/// out by the accent wash behind it rather than by a tick, which would have to
/// sit on top of the colour it is confirming.
pub fn reader_annotation_color_swatch(id: impl Into<ElementId>, hex: &'static str, name: &'static str, selected: bool, theme: BrowserTheme) -> Stateful<Div> {
    div()
        .id(id)
        .size(gpui::rems(SETTINGS_CONTROL_SIZE_REM))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(RADIUS_SM))
        .cursor_pointer()
        .when(selected, |swatch| swatch.bg(theme.accent_wash))
        .when(!selected, |swatch| swatch.hover(move |style| style.bg(theme.hover)))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(name).build(window, cx))
        .child(div().size(px(16.0)).rounded_full().border_1().border_color(theme.rule).bg(reader_annotation_color(hex)))
}

/// A compact text action in the annotation editor.
pub fn reader_annotation_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    crate::selectable_button(outlined_text_button(id, label).h(gpui::rems(SETTINGS_CONTROL_SIZE_REM)).rounded(px(RADIUS_SM)).border_color(theme.rule).bg(theme.page_bg), selected, theme)
}

/// Discarding an annotation, kept apart from the actions that keep it: an icon
/// where the others are words, and the only thing in the panel wearing `danger`.
pub fn reader_annotation_delete_button(id: impl Into<ElementId>, theme: BrowserTheme) -> Button {
    crate::outlined_icon_button(id, "Delete annotation", IconName::Delete, theme).text_color(theme.danger)
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
    div().w_full().flex().flex_col().gap(px(SPACE_XS)).px(px(crate::CONTENT_INSET)).pt(px(SPACE_MD)).pb(px(SPACE_MD)).border_b_1().border_color(theme.rule)
}

pub fn reader_setting_row(label: impl Into<SharedString>) -> Div {
    div().w_full().min_h(gpui::rems(2.375)).flex().items_center().gap(px(SPACE_SM)).child(div().w(gpui::rems(7.0)).max_w(px(112.0)).flex_none().text_size(gpui::rems(TEXT_SM)).child(label.into())).child(div().flex_1())
}

/// A button-group row: a small-caps label above a full-width choice group,
/// rather than the stepper row's side-by-side label-and-value layout.
pub fn reader_setting_row_smallcaps(label: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().flex().flex_col().gap(px(SPACE_XXS)).child(div().text_size(gpui::rems(TEXT_XS)).font_features(crate::small_caps()).text_color(theme.text_muted).child(label.into()))
}

/// The query field. Grows with the bar instead of sitting at a fixed 260px:
/// the search bar spans the window, and a stranded box with dead space beside
/// it looks like a control that failed to lay out.
pub fn reader_search_box(theme: BrowserTheme) -> Div {
    div().flex_1().min_w_0().h(gpui::rems(READER_TOOLBAR_SIZE_REM)).overflow_hidden().rounded(px(RADIUS_SM)).border_1().border_color(theme.border).bg(theme.page_bg)
}

/// The search row: the toolbar's own shape, carrying the query field instead
/// of document controls. (The toolbar's `min_w_0` comes along; inside a
/// full-width row it can only help.)
pub fn reader_search_bar(theme: BrowserTheme) -> Div {
    reader_toolbar(theme)
}

pub fn reader_footnote_panel(theme: BrowserTheme) -> Scrollable<Div> {
    div().max_h(px(240.0)).flex_none().overflow_y_scrollbar().px(px(SPACE_MD)).py(px(SPACE_MD)).flex().flex_col().gap(px(SPACE_SM)).bg(theme.page_bg).border_t_1().border_color(theme.rule)
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
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = browser_theme(cx);
        let dismiss = self.on_dismiss;
        crate::modal_overlay().child(crate::modal_scrim("reader-modal-scrim", theme).on_click(move |_, window, cx| dismiss(window, cx))).child(reader_modal_panel(self.size.width, self.size.height, theme).child(self.content))
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

    pub fn selection(mut self, on_highlight: impl Fn(&mut Window, &mut App) + 'static, on_cite: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self = self.begin_section();
        self.menu = self.menu.item(menu_item("Highlight", move |_, window, cx| on_highlight(window, cx))).item(menu_item("Cite", move |_, window, cx| on_cite(window, cx)));
        self
    }

    /// The PDF reader has no citation format, but it shares the annotation
    /// workflow with EPUB: a selection becomes a highlight whose editor holds
    /// the optional note.
    pub fn pdf_selection(mut self, on_highlight: impl Fn(&mut Window, &mut App) + 'static, on_copy: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self = self.begin_section();
        self.menu = self.menu.item(menu_item("Highlight", move |_, window, cx| on_highlight(window, cx))).item(menu_item("Copy", move |_, window, cx| on_copy(window, cx)));
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

#[cfg(test)]
mod annotation_color_tests {
    use super::*;

    #[test]
    fn colors_agree_on_the_default() {
        assert_eq!(READER_ANNOTATION_DEFAULT_COLOR, READER_ANNOTATION_COLORS[0].1);
        assert_eq!(format!("#{READER_ANNOTATION_DEFAULT_RGB:06x}"), READER_ANNOTATION_DEFAULT_COLOR);
    }

    /// Every colour must be a full `#rrggbb`: the EPUB parser reads `#rgb` as
    /// shorthand, and a swatch that means one thing there and another in a PDF
    /// is worse than no swatch.
    #[test]
    fn every_color_is_a_full_hex_triplet() {
        for (name, hex) in READER_ANNOTATION_COLORS {
            assert_eq!(hex.len(), 7, "{name} is {hex}");
            assert!(hex.starts_with('#') && hex[1..].chars().all(|digit| digit.is_ascii_hexdigit()), "{name} is {hex}");
        }
    }
}

#[cfg(test)]
mod image_popup_tests {
    use super::image_popup_panel_size;

    #[test]
    fn panel_tracks_reader_bounds_and_image_aspect_ratio() {
        let landscape = image_popup_panel_size(1000.0, 700.0, Some((1600.0, 900.0)));
        let portrait = image_popup_panel_size(1000.0, 700.0, Some((900.0, 1600.0)));
        let constrained = image_popup_panel_size(420.0, 320.0, Some((1600.0, 900.0)));

        assert!(landscape.0 > landscape.1, "a landscape image should produce a wider dialog");
        assert!(portrait.1 > portrait.0, "a portrait image should produce a taller dialog");
        assert!(constrained.0 <= 372.0 && constrained.1 <= 272.0, "the dialog must remain within the reader inset");
    }
}

pub fn reader_document_body() -> Div {
    div().relative().size_full().min_h_0().min_w_0().flex().flex_col().overflow_hidden()
}

/// Invisible strip along the top edge that brings a hidden toolbar back.
///
/// Kept thin so it barely overlaps the document: a reader who is selecting text
/// near the top of the page should not keep summoning the chrome.
pub fn reader_chrome_reveal() -> Stateful<Div> {
    div().id("reader-chrome-reveal").absolute().top_0().left_0().right_0().h(px(SPACE_SM))
}

/// Invisible strip along the left edge that brings a hidden sidebar back.
///
/// The sidebar is now the reader's only chrome and sits on the left, so this
/// is where a pointer-driven reader reaches to bring it back — `top_0`'s
/// counterpart for a reader whose chrome no longer lives along the top edge.
pub fn reader_chrome_reveal_left() -> Stateful<Div> {
    div().id("reader-chrome-reveal-left").absolute().top_0().bottom_0().left_0().w(px(SPACE_SM))
}

pub fn reader_content() -> Div {
    div().flex_1().min_h_0()
}

/// The appearance popover, anchored to the end of the reader toolbar.
///
/// `document_focus` is where the popover should put keyboard focus while it is
/// open. A popover always focuses something on open, and nothing in this panel
/// takes keyboard input — it is buttons throughout — so pointing it at the
/// document leaves paging keys working while settings are being adjusted.
/// Without this the popover focuses itself and the reader's navigation keys go
/// dead until the document is clicked again.
pub fn reader_settings_popover(document_focus: Option<&gpui::FocusHandle>, theme: BrowserTheme, window: &Window) -> Popover {
    // The trigger is vertically centered in the toolbar. Top-anchored GPUI
    // popovers begin at the trigger's top edge, so move this one past the
    // trigger and the remainder of the toolbar before drawing the panel.
    let top_offset = reader_toolbar_size_px(window) + READER_SETTINGS_POPOVER_GAP;
    Popover::new("reader-settings").anchor(gpui::Anchor::TopRight).top(px(top_offset)).p_0().when_some(document_focus, |popover, focus| popover.track_focus(focus)).trigger(reader_settings_trigger(theme).tooltip("Reader settings"))
}

#[cfg(test)]
mod reader_settings_geometry_tests {
    use super::*;

    #[test]
    fn settings_panel_begins_below_the_toolbar() {
        let toolbar_size = READER_TOOLBAR_SIZE_REM * 16.0;
        let trigger_top = 0.0;
        let top_offset = toolbar_size + READER_SETTINGS_POPOVER_GAP;

        assert_eq!(trigger_top, 0.0, "reader controls must meet the top edge");
        assert_eq!(trigger_top + top_offset, toolbar_size + READER_SETTINGS_POPOVER_GAP);
    }
}

const READER_SIDEBAR_MIN_WIDTH: f32 = 260.0;
const READER_SIDEBAR_MAX_WIDTH: f32 = 520.0;

pub fn clamp_reader_sidebar_width(width: f32) -> f32 {
    width.clamp(READER_SIDEBAR_MIN_WIDTH, READER_SIDEBAR_MAX_WIDTH)
}

struct ReaderSplitState {
    width: f32,
    bounds: Bounds<Pixels>,
    resizing: bool,
}

impl ReaderSplitState {
    fn new(width: f32) -> Self {
        Self { width: clamp_reader_sidebar_width(width), bounds: Bounds::default(), resizing: false }
    }
}

/// A reader-specific two-pane layout whose sidebar has an absolute pixel width.
///
/// Unlike a proportional resizable panel group, window resizing never changes
/// the sidebar width. Only dragging the divider updates it.
#[derive(IntoElement)]
pub struct ReaderSplit {
    sidebar: gpui::AnyElement,
    document: gpui::AnyElement,
    width: f32,
    on_resize: Rc<dyn Fn(f32, &mut Window, &mut App)>,
}

impl ReaderSplit {
    pub fn on_resize(mut self, callback: impl Fn(f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_resize = Rc::new(callback);
        self
    }
}

impl RenderOnce for ReaderSplit {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = browser_theme(cx);
        let requested_width = clamp_reader_sidebar_width(self.width);
        let state = window.use_keyed_state("reader-layout", cx, |_, _| ReaderSplitState::new(requested_width));
        state.update(cx, |state, _| {
            if !state.resizing && (state.width - requested_width).abs() >= 0.5 {
                state.width = requested_width;
            }
        });
        let width = state.read(cx).width;

        let start_resize = state.clone();
        let track_bounds = state.clone();
        div()
            .id("reader-layout")
            .relative()
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .overflow_hidden()
            .child(div().h_full().w(px(width)).min_w(px(width)).max_w(px(width)).flex_none().min_h_0().overflow_hidden().child(self.sidebar))
            .child(div().h_full().min_h_0().min_w_0().flex_1().overflow_hidden().child(self.document))
            // The divider is the sidebar's only outer boundary. Keep it a
            // single `theme.rule` line, matching the sidebar header, tabs,
            // and controls; the former sidebar border plus this handle made
            // a visibly heavier two-tone seam.
            .child(div().id("reader-resize-handle").absolute().top_0().left(px(width - 4.0)).h_full().w(px(9.0)).cursor_col_resize().flex().justify_center().child(div().h_full().w(px(1.0)).bg(theme.rule)).on_mouse_down(
                MouseButton::Left,
                move |_, _, cx| {
                    cx.stop_propagation();
                    start_resize.update(cx, |state, cx| {
                        state.resizing = true;
                        cx.notify();
                    });
                },
            ))
            .on_prepaint(move |bounds, _, cx| {
                track_bounds.update(cx, |state, _| state.bounds = bounds);
            })
            .child(ReaderSplitEventSink { state, on_resize: self.on_resize })
    }
}

struct ReaderSplitEventSink {
    state: gpui::Entity<ReaderSplitState>,
    on_resize: Rc<dyn Fn(f32, &mut Window, &mut App)>,
}

impl IntoElement for ReaderSplitEventSink {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ReaderSplitEventSink {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), None, cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: Bounds<Pixels>, _: &mut Self::RequestLayoutState, _: &mut Window, _: &mut App) -> Self::PrepaintState {}

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&gpui::InspectorElementId>, _: Bounds<Pixels>, _: &mut Self::RequestLayoutState, _: &mut Self::PrepaintState, window: &mut Window, _cx: &mut App) {
        let move_state = self.state.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if !phase.bubble() || !move_state.read(cx).resizing {
                return;
            }
            let bounds = move_state.read(cx).bounds;
            let width = clamp_reader_sidebar_width(f32::from(event.position.x - bounds.left()));
            move_state.update(cx, |state, cx| {
                if (state.width - width).abs() >= 0.5 {
                    state.width = width;
                    cx.notify();
                }
            });
        });

        let end_state = self.state.clone();
        let on_resize = self.on_resize.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
            if !phase.bubble() || !end_state.read(cx).resizing {
                return;
            }
            let width = end_state.read(cx).width;
            end_state.update(cx, |state, cx| {
                state.resizing = false;
                cx.notify();
            });
            on_resize(width, window, cx);
        });
    }
}

pub fn reader_split(sidebar: impl IntoElement, document: impl IntoElement, sidebar_width: f32) -> ReaderSplit {
    ReaderSplit { sidebar: sidebar.into_any_element(), document: document.into_any_element(), width: sidebar_width, on_resize: Rc::new(|_, _, _| {}) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_sidebar_width_is_clamped_only_to_its_user_range() {
        assert_eq!(clamp_reader_sidebar_width(100.0), READER_SIDEBAR_MIN_WIDTH);
        assert_eq!(clamp_reader_sidebar_width(360.0), 360.0);
        assert_eq!(clamp_reader_sidebar_width(900.0), READER_SIDEBAR_MAX_WIDTH);
    }
}
