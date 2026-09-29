use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Bounds, Div, ElementId, FocusHandle, FontWeight, Pixels, ScrollHandle, SharedString, div, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Icon, IconName};

use crate::{
    BrowserTheme, RADIUS_MD, RADIUS_SM, SETTINGS_CONTROL_GAP, SETTINGS_CONTROL_SIZE_REM, SPACE_MD, SPACE_SM, SPACE_XL, SPACE_XS, SPACE_XXS, Scrollable, ScrollableElement, TAB_UNDERLINE_HEIGHT, TEXT_MD, TEXT_SM, TEXT_XS, THEME_PICKER_WIDTH_REM,
    TITLE_MD, TOPBAR_ACTION_GAP, TOPBAR_ACTION_HEIGHT_REM, TOPBAR_ACTION_HORIZONTAL_PADDING,
};

// Button installs its own hover style during render. Setting a Div hover
// override here would cause GPUI to panic when these navigation controls render.
pub fn nav_rail_item(id: impl Into<ElementId>, icon: impl Into<Icon>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    let label = label.into();
    crate::base_button(id)
        .ghost()
        .w_full()
        .h(gpui::rems(crate::NAV_RAIL_SIZE_REM))
        .flex_none()
        .p_0()
        .px(px(SPACE_XS))
        .rounded(px(RADIUS_MD))
        .text_color(theme.text)
        .when(selected, |button| button.primary().bg(theme.accent).text_color(theme.accent_text))
        .child(
            div()
                .size_full()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_XXS))
                .child(Icon::new(icon).size(gpui::rems(crate::NAV_RAIL_ICON_SIZE_REM)))
                .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_center().text_size(gpui::rems(crate::NAV_RAIL_LABEL_SIZE)).font_features(crate::small_caps()).child(label)),
        )
}

/// A full-width bar along the bottom of the window, ruled off from the page
/// above: the navigation bar, and the audiobook dock.
pub fn bottom_bar(theme: BrowserTheme) -> Div {
    div().w_full().flex_none().flex().border_t_1().border_color(theme.rule).bg(theme.page_bg).text_color(theme.text)
}

pub fn bottom_navigation(theme: BrowserTheme) -> Div {
    bottom_bar(theme).h(gpui::rems(crate::BOTTOM_NAV_HEIGHT_REM)).flex_row().items_stretch()
}

pub fn bottom_navigation_item(id: impl Into<ElementId>, icon: impl Into<Icon>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    let label = label.into();
    crate::base_button(id).ghost().h_full().flex_1().min_w_0().p_0().rounded(px(0.0)).text_color(theme.text_muted).when(selected, |button| button.primary().bg(theme.accent).text_color(theme.accent_text)).tooltip(label.clone()).child(
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SPACE_XXS))
            .child(Icon::new(icon).size(gpui::rems(crate::NAV_RAIL_ICON_SIZE_REM)))
            .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_center().text_size(gpui::rems(TEXT_XS)).child(label)),
    )
}

pub fn more_navigation_overlay() -> Div {
    crate::absolute_full_size().flex().items_end().justify_center()
}

/// A modal list sized from its fixed-height header and rows.
/// Sized to its contents up to a cap, then scrolls.
///
/// The height is stated rather than left to the contents because the list
/// scrolls, and a scroll container needs a definite height. It is worked out at
/// the window's `rem_size`: the header and rows are rems, the gaps and padding
/// are pixels, and a height reckoned at a fixed 16px rem cut the rows off at
/// any larger UI font.
pub fn navigation_modal_list(item_count: usize, rem_size: Pixels, theme: BrowserTheme) -> Scrollable<Div> {
    let rem = f32::from(rem_size);
    let rows = (item_count + 1) as f32 * crate::NAVIGATION_MODAL_ROW_HEIGHT_REM * rem;
    let gaps = item_count as f32 * SPACE_XS + SPACE_MD * 2.0;
    let height = (rows + gaps).min(crate::NAVIGATION_MODAL_MAX_HEIGHT_REM * rem);
    crate::modal_surface(theme).w(gpui::rems(crate::NAVIGATION_MODAL_WIDTH_REM)).max_w_full().h(px(height)).max_h_full().overflow_y_scrollbar()
}

pub fn more_navigation_header(theme: BrowserTheme) -> Div {
    div().w_full().h(gpui::rems(crate::NAVIGATION_MODAL_ROW_HEIGHT_REM)).flex_none().flex().items_center().px(px(SPACE_SM)).text_size(gpui::rems(TEXT_MD)).text_color(theme.text).child("More")
}

pub fn more_navigation_item(id: impl Into<ElementId>, icon: impl Into<Icon>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    let label = label.into();
    crate::base_button(id)
        .ghost()
        .w_full()
        .h(gpui::rems(crate::NAVIGATION_MODAL_ROW_HEIGHT_REM))
        .flex_none()
        .px(px(SPACE_SM))
        .justify_start()
        .rounded(px(RADIUS_MD))
        .text_color(theme.text)
        .when(selected, |button| button.primary().bg(theme.accent).text_color(theme.accent_text))
        .tooltip(label.clone())
        .child(div().size_full().min_w_0().flex().items_center().gap(px(SPACE_SM)).child(Icon::new(icon)).child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavigationIcon {
    House,
    BookOpen,
    Folder,
    Tags,
    User,
    Trash2,
    Building2,
    Settings,
}

pub fn navigation_icon(icon: NavigationIcon) -> Icon {
    let path = match icon {
        NavigationIcon::House => "icons/navigation/house.svg",
        NavigationIcon::BookOpen => "icons/navigation/book-open.svg",
        NavigationIcon::Folder => "icons/navigation/folder.svg",
        NavigationIcon::Tags => "icons/navigation/tags.svg",
        NavigationIcon::User => "icons/navigation/user.svg",
        NavigationIcon::Trash2 => "icons/navigation/trash-2.svg",
        NavigationIcon::Building2 => "icons/navigation/building-2.svg",
        NavigationIcon::Settings => "icons/navigation/settings.svg",
    };
    Icon::empty().path(path)
}

/// A centered topbar input that searches or filters.
///
/// Individual pages may add a contextual placeholder through their
/// `InputState`. `cleanable` gives a filter a way out that does not involve
/// selecting and deleting its text.
pub fn browser_search_control(state: &gpui::Entity<gpui_component::input::InputState>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .justify_center()
        .child(div().w_full().min_w_0().h(gpui::rems(TOPBAR_ACTION_HEIGHT_REM)).flex_1().child(gpui::Styled::h(gpui_component::input::Input::new(state).cleanable(true), gpui::rems(TOPBAR_ACTION_HEIGHT_REM))))
}

/// The rail and the page, side by side.
///
/// `items_stretch` is load-bearing: row children that declare no height of
/// their own must fill the cross axis, or they lay out at zero height and
/// nothing paints. The rail escaped that only because it sets `h_full`
/// explicitly; every page went through `library_section_content`, which does
/// not, and so rendered blank.
pub fn browser_body() -> Div {
    div().flex_1().min_h_0().flex().flex_row().items_stretch()
}

pub fn library_section_content() -> Div {
    div().flex_1().min_w_0().min_h_0().flex().flex_col()
}

pub fn library_switcher_group(theme: BrowserTheme) -> Div {
    let _ = theme;
    div().relative().w_full().h(gpui::rems(crate::TOPBAR_HEIGHT_REM)).flex_none().flex().items_center().gap(px(SPACE_SM))
}

/// The topbar carries neither fill nor underline. Its old `bg(theme.page_bg)`
/// was a no-op — `surface` and `page_bg` are the same colour — and the
/// underline that replaced it was marking a boundary the breadcrumb and the
/// actions already make obvious. See [`crate::nav_rail_destinations`].
pub fn browser_topbar(theme: BrowserTheme) -> Div {
    let _ = theme;
    div().w_full().h(gpui::rems(crate::TOPBAR_HEIGHT_REM)).flex_none().flex().flex_row().items_center().gap(px(SPACE_MD))
}

pub fn browser_topbar_left() -> Div {
    div().flex_1().min_w_0().flex().items_center().justify_start()
}

pub fn mobile_back_button(theme: BrowserTheme) -> Button {
    crate::outlined_icon_button("mobile-navigation-back", "Back", IconName::ArrowLeft, theme).debug_selector(|| "mobile-navigation-back".into())
}

/// Shared navigation heading for browsing, book loading, and reading. `back`
/// is `None` on native mobile, where the OS back button/gesture already does
/// the same thing — see `has_native_back_button`.
pub fn mobile_navigation_heading(title: impl Into<SharedString>, back: Option<Button>) -> Div {
    div().flex_1().min_w_0().flex().items_center().gap(px(SPACE_SM)).pl(px(crate::CONTENT_INSET)).children(back).child(div().flex_1().min_w_0().truncate().font_family(crate::DISPLAY_FONT_FAMILY).text_size(gpui::rems(crate::TEXT_MD)).child(title.into()))
}

pub fn mobile_navigation_bar(title: impl Into<SharedString>, back: Option<Button>, theme: BrowserTheme) -> Div {
    browser_topbar(theme).bg(theme.page_bg).child(mobile_navigation_heading(title, back))
}

/// The actions end where the covers do.
///
/// The card grid is flush with the content column, so the rightmost cover stops
/// one `BOOK_CARD_PADDING` short of the column's edge. Without the same inset
/// here the last button overhangs that edge by exactly a card's padding, and the
/// column reads as having two different right margins — one for the chrome and
/// one for the content. `CONTENT_INSET` is the same token the breadcrumb's first
/// crumb uses on the other side.
pub fn browser_topbar_right() -> Div {
    div().flex_none().min_w_0().flex().items_center().justify_end().gap(px(TOPBAR_ACTION_GAP)).pr(px(crate::CONTENT_INSET))
}

/// A page that names itself rather than trailing a breadcrumb, with a muted
/// count beside the title. Set at the breadcrumb's size so a page carrying one
/// of these and a page carrying a trail have the same topbar height.
pub fn browser_page_title(title: impl Into<SharedString>, detail: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .min_w_0()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(px(SPACE_SM))
        .text_size(gpui::rems(crate::TEXT_LG))
        .child(div().flex_none().font_family(crate::DISPLAY_FONT_FAMILY).child(title.into()))
        .child(div().min_w_0().text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(detail.into()))
}

/// Centred, quiet status text for a section that is still loading.
///
/// Deliberately plainer than [`browser_actionable_empty`]: a page that is
/// waiting has nothing for the reader to act on, and an empty-state heading
/// would assert something not yet known to be true.
pub fn browser_loading_message(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().min_h(px(240.0)).flex().items_center().justify_center().text_center().text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(text.into())
}

/// A one-line muted note standing in for content: "no matching titles",
/// "searching the library…". Quieter than [`browser_actionable_empty`], which
/// asserts a heading and offers an action.
pub fn browser_inline_note(text: impl Into<SharedString>, theme: BrowserTheme) -> gpui_component::label::Label {
    gpui_component::label::Label::new(text.into()).w_full().px(px(SPACE_SM)).py(px(SPACE_XS)).text_color(theme.text_muted)
}

pub fn browser_actionable_empty(title: impl Into<SharedString>, description: impl Into<SharedString>, actions: impl IntoElement, theme: BrowserTheme) -> Div {
    browser_actionable_empty_parts(title.into(), Some(description.into()), actions, theme)
}

/// Centred empty state with actions but no explanatory subheader.
pub fn browser_actionable_empty_without_description(title: impl Into<SharedString>, actions: impl IntoElement, theme: BrowserTheme) -> Div {
    browser_actionable_empty_parts(title.into(), None, actions, theme)
}

fn browser_actionable_empty_parts(title: SharedString, description: Option<SharedString>, actions: impl IntoElement, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .min_h(px(240.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(SPACE_SM))
        .text_center()
        .child(div().font_family(crate::DISPLAY_FONT_FAMILY).text_size(gpui::rems(TITLE_MD)).child(title))
        .children(description.map(|description| div().text_size(gpui::rems(TEXT_SM)).text_color(theme.text_muted).child(description)))
        .child(div().mt(px(SPACE_XS)).child(actions))
}

/// The bar a tab strip and its actions share, above the panel they belong to.
///
/// Its height is stated rather than taken from the tallest child. The actions
/// beside the tabs come and go — a lane with one page has no paging arrows — and
/// a row sized by its contents moved the tabs every time the reader switched
/// lane. `items_stretch` then gives each tab the full height, which is what puts
/// every underline on the bar's bottom edge, flush with the panel's border.
/// The gap here outranks the one inside [`browser_tab_strip`] on purpose: with
/// both at `SPACE_MD` the rightmost tab and the leftmost action were the same
/// distance apart as two tabs, and the bar read as one row of six things.
pub fn browser_tab_bar() -> Div {
    div().w_full().h(gpui::rems(TOPBAR_ACTION_HEIGHT_REM)).flex_none().flex().flex_row().items_stretch().gap(px(SPACE_XL))
}

/// The tabs themselves, at the left of a [`browser_tab_bar`].
pub fn browser_tab_strip() -> Div {
    div().flex_1().h_full().flex().flex_row().items_stretch()
}

/// Controls sharing the bar with the tabs. They centre on it; the tabs do not,
/// because a tab is as tall as the bar.
pub fn browser_tab_bar_actions() -> Div {
    div().flex_none().h_full().flex().flex_row().items_center().gap(px(SPACE_SM))
}

/// One tab: its own text, underlined when it is the one on show.
///
/// Not a segmented button. A filled segment reads as a control sitting above the
/// content; a tab is a label the panel is attached to, so the only marks it
/// carries are colour and the rule under the word itself. The rule is drawn on
/// every tab and merely goes transparent when unselected, so selecting one moves
/// nothing.
///
/// An optional count is baselined with the label inside a box that
/// centres on the bar: they are two sizes of the same line, and centring them
/// separately would leave the count riding above the word it belongs to.
pub fn browser_tab(id: impl Into<ElementId>, label: impl Into<SharedString>, count: impl Into<Option<usize>>, selected: bool, theme: BrowserTheme) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .h_full()
        .flex_1()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_b(px(TAB_UNDERLINE_HEIGHT))
        .border_color(if selected { theme.accent } else { gpui::transparent_black() })
        .text_size(gpui::rems(TEXT_SM))
        .font_features(crate::small_caps())
        .text_color(if selected { theme.text } else { theme.text_muted })
        .hover(move |style| style.text_color(theme.text))
        .child(div().flex().flex_row().items_baseline().gap(px(SPACE_XS)).child(label.into()).children(count.into().map(|count| div().flex_none().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(count.to_string()))))
}

/// The list a tab strip is attached to: one border around the whole thing.
///
/// Its entries are separated by [`browser_panel_row`]'s own rule and stand
/// directly on one another, so the list reads as a single bordered body under
/// the selected tab rather than as a column of floating boxes each carrying
/// four edges of their own.
pub fn browser_panel_list(theme: BrowserTheme) -> Div {
    div().w_full().flex_1().min_h_0().min_w_0().overflow_hidden().flex().flex_col().border_1().border_color(theme.rule).bg(theme.page_bg)
}

/// One entry in a [`browser_panel_list`]. It draws the rule under itself and
/// nothing else: the list supplies the outer edges, and the entries carry no
/// gap between them for a border to float in.
pub fn browser_panel_row(theme: BrowserTheme) -> Div {
    div().w_full().border_b_1().border_color(theme.rule).bg(theme.page_bg)
}

pub fn library_switcher_trigger(label: impl Into<SharedString>, theme: BrowserTheme) -> Button {
    let label = label.into();
    crate::base_button("library-switcher-trigger")
        .ghost()
        .w_full()
        .h(gpui::rems(crate::TOPBAR_HEIGHT_REM))
        .flex_none()
        .p_0()
        .overflow_hidden()
        .text_size(gpui::rems(crate::NAV_RAIL_LABEL_SIZE))
        .font_weight(FontWeight::NORMAL)
        .text_color(theme.text_accent)
        .child(
            div()
                .size_full()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_XXS))
                .child(navigation_icon(NavigationIcon::BookOpen).size(gpui::rems(crate::NAV_RAIL_ICON_SIZE_REM)))
                .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_center().font_features(crate::small_caps()).child(label)),
        )
}

pub fn library_switcher_panel() -> Div {
    div().w_full().flex().flex_col().gap(px(SPACE_XS))
}

pub fn library_switcher_header(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().h(gpui::rems(crate::NAVIGATION_MODAL_ROW_HEIGHT_REM)).w_full().flex().items_center().justify_between().pl(px(SPACE_SM)).pb(px(SPACE_XS)).text_size(gpui::rems(TEXT_MD)).text_color(theme.text).child(title.into())
}

/// `cursor` is the keyboard's transient position — the same tier `contents_row`
/// uses for its own cursor, `accent_wash` — not a place the app considers
/// itself "in", which is what the solid accent is reserved for. Shared by
/// every square icon action in the library switcher list.
fn library_switcher_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: IconName, cursor: bool, theme: BrowserTheme) -> Button {
    crate::outlined_icon_button(id, label, icon, theme).flex_none().when(cursor, |button| button.bg(theme.accent_wash))
}

pub fn library_switcher_add_button(cursor: bool, theme: BrowserTheme) -> Button {
    library_switcher_action_button("library-switcher-add", "Add library", IconName::Plus, cursor, theme)
}

/// `selected` is the library actually in use, so it carries the solid accent —
/// the same weight nav uses for "where you are now". `cursor` is only the
/// keyboard's transient position, so it takes `accent_wash` instead, the same
/// tier `contents_row` uses for its own cursor — outranked by `selected`,
/// since a wash over the solid accent would be invisible anyway.
pub fn library_switcher_item(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, cursor: bool, theme: BrowserTheme) -> Button {
    let label = label.into();
    crate::base_button(id)
        .ghost()
        .when(cursor && !selected, |button| button.bg(theme.accent_wash))
        .when(selected, |button| button.bg(theme.accent).text_color(theme.accent_text))
        .flex_1()
        .min_w_0()
        .h(gpui::rems(crate::NAVIGATION_MODAL_ROW_HEIGHT_REM))
        .px(px(SPACE_SM))
        .justify_start()
        .overflow_hidden()
        .text_size(gpui::rems(TEXT_SM))
        .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
}

pub fn library_switcher_row(library: impl IntoElement, actions: impl IntoElement) -> Div {
    div().w_full().flex().items_center().gap(px(SPACE_XS)).child(library).child(actions)
}

pub fn library_switcher_menu_button(id: impl Into<ElementId>, label: impl Into<SharedString>, cursor: bool, theme: BrowserTheme) -> Button {
    library_switcher_action_button(id, format!("Manage {}", label.into()), IconName::EllipsisVertical, cursor, theme)
}

pub fn topbar_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: IconName, theme: BrowserTheme) -> Button {
    crate::outlined_button(id, theme)
        .label(label)
        .icon(icon)
        .text_color(theme.text_accent)
        .text_size(gpui::rems(TEXT_SM))
        .font_weight(crate::TOPBAR_ACTION_LABEL_WEIGHT)
        .h(gpui::rems(TOPBAR_ACTION_HEIGHT_REM))
        .px(px(TOPBAR_ACTION_HORIZONTAL_PADDING))
        .rounded(px(RADIUS_MD))
}

/// Base surface for a clickable row or card.
pub fn clickable_surface(theme: BrowserTheme) -> Div {
    div().cursor_pointer().hover(move |style| style.bg(theme.hover))
}

pub fn browser_settings_page() -> Scrollable<Div> {
    crate::full_size_column().items_center().overflow_y_scrollbar()
}

/// The settings surface, scrolling through a handle its page owns rather than
/// one of its own, so a control reached with the keyboard can be brought into
/// view by the page that knows where its sections are.
pub fn browser_settings_page_with_scroll(scroll: &ScrollHandle) -> Scrollable<Div> {
    crate::full_size_column().items_center().overflow_y_scrollbar_with_handle(scroll)
}

/// One block of settings, measured and focusable so keyboard navigation can
/// tell which block it has moved into and scroll that block into view.
///
/// The handle is not a tab stop: focus belongs to the controls inside, and the
/// block is only ever a place, not a destination.
pub fn browser_settings_section(focus: &FocusHandle, bounds: Rc<Cell<Bounds<Pixels>>>, child: impl IntoElement) -> Div {
    div().w_full().relative().track_focus(focus).child(crate::measured_bounds(bounds)).child(child)
}

/// The column the groups stack in, with the same air below the last group as
/// above the first, so a page scrolled to its end does not stop on a border.
pub fn browser_settings_content() -> Div {
    div().w_full().max_w(px(580.0)).flex().flex_col().gap(px(SPACE_MD)).py(px(SPACE_MD))
}

pub fn browser_settings_row(title: impl Into<SharedString>, description: impl Into<SharedString>, control: impl IntoElement, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .min_h(px(76.0))
        .px(px(16.0))
        .py(px(12.0))
        .flex()
        .items_center()
        .gap(px(20.0))
        .border_t_1()
        .border_color(theme.rule)
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(SPACE_XS)).child(div().text_size(gpui::rems(TEXT_MD)).child(title.into())).child(div().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(description.into())))
        .child(control)
}

pub fn browser_settings_sections() -> Div {
    div().w_full().flex().flex_col().gap(px(SPACE_MD))
}

/// A block of settings, drawn as a bordered region.
///
/// The interface has one surface, so a region is its edge — there is no second
/// fill to lift a group off the page. Groups were previously separated by
/// whitespace alone, which worked while a `GroupBox` fill stood behind them and
/// stopped working the moment that fill became the page colour.
///
/// One edge, around the outside. The rows inside are separated by their own
/// padding rather than by dividers: the border is what says "these belong
/// together", and a hairline between every row says it a second time.
pub fn browser_settings_group(theme: BrowserTheme) -> Div {
    div().w_full().flex().flex_col().overflow_hidden().rounded(px(RADIUS_SM)).border_1().border_color(theme.rule)
}

/// A titled block. The title is a header on the group rather than a floating
/// label above it, so it is inside the region it names.
///
/// Its header is the same bar a group with actions has, empty on the right:
/// the title was once a bare line in the group's corner, flush against the
/// border while every row under it and the account's title were inset.
pub fn browser_settings_fieldset(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    browser_settings_group(theme).child(browser_settings_header(title, theme))
}

/// A titled block whose header carries a control of its own, on the right.
///
/// For the one group whose subject is a thing you act on rather than a list of
/// settings: the account. Its control — sign in, or who is signed in — is not a
/// setting inside the group, it is what the group is about, so it sits in the
/// header beside the name instead of becoming the first row.
pub fn browser_settings_fieldset_with_actions(title: impl Into<SharedString>, actions: impl IntoElement, theme: BrowserTheme) -> Div {
    browser_settings_group(theme).child(browser_settings_header(title, theme).child(div().flex_none().flex().flex_row().items_center().gap(px(SPACE_SM)).child(actions)))
}

/// A group's header bar: inset like the rows under it, so the title starts on
/// their line, and at least as tall as a header button, so a group whose header
/// has none is exactly as tall as one whose header does.
fn browser_settings_header(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .px(px(SPACE_MD))
        .py(px(SPACE_SM))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(SPACE_SM))
        .child(browser_settings_legend(title, theme).flex_none().w_auto().min_h(gpui::rems(SETTINGS_CONTROL_SIZE_REM)).flex().items_center())
}

/// An action in a settings group's header.
///
/// The same size as the controls in the rows below it, because it is read as
/// one of them; `primary` fills it, for the one of a pair a returning user
/// almost always wants.
pub fn browser_settings_header_button(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool, theme: BrowserTheme) -> Button {
    crate::base_button(id)
        .label(label)
        .ghost()
        .h(gpui::rems(SETTINGS_CONTROL_SIZE_REM))
        .px(px(SPACE_MD))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(if primary { theme.accent } else { theme.border })
        .bg(if primary { theme.accent } else { theme.page_bg })
        .text_color(if primary { theme.accent_text } else { theme.text })
        .text_size(gpui::rems(TEXT_XS))
}

/// The name a settings group is read by.
fn browser_settings_legend(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().text_size(gpui::rems(crate::TEXT_MD)).text_color(theme.text_muted).font_weight(FontWeight::SEMIBOLD).font_features(crate::small_caps()).child(title.into())
}

/// What went wrong, said inside the group it went wrong in.
///
/// A failure belongs next to the control that caused it. One alert at the foot
/// of the page reported a theme that would not save from below the library
/// list, where nobody flipping a theme is looking.
pub fn browser_settings_group_error(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().px(px(SPACE_MD)).py(px(SPACE_SM)).border_t_1().border_color(theme.danger).flex().flex_row().gap(px(SPACE_SM)).text_size(gpui::rems(TEXT_XS)).text_color(theme.danger).child(text.into())
}

/// The segmented control a choice row carries.
///
/// The fill is what shows through the one-pixel gaps between segments, so it is
/// the separator rather than a background: `rule`, the quiet tier, like every
/// other divider in the interface. It was `border`, which is the tier reserved
/// for a control's own edge — three and a half to one against the surface,
/// drawn between every pair of segments.
///
/// It matters more than a separator normally would, because it is also what an
/// unselected segment falls back to if it does not paint its own surface.
pub fn browser_settings_button_group(theme: BrowserTheme) -> Div {
    div().w(gpui::rems(THEME_PICKER_WIDTH_REM)).max_w_full().flex().flex_row().gap(px(SETTINGS_CONTROL_GAP)).overflow_hidden().rounded(px(RADIUS_SM)).border_1().border_color(theme.border).bg(theme.rule)
}

/// One setting: its name on the left, its control on the right.
///
/// No divider of its own — the group's border is the only edge, and rows are
/// told apart by their padding.
///
/// The row wraps rather than squashing. Several of the controls it carries are
/// bounded — a choice group is up to the theme picker's width, and a stepper
/// has its own fixed width — so on a narrow window there is no width
/// left to take from them, and the name is what gets crushed instead. The name
/// therefore keeps its own size and the control drops to a line of its own when
/// the two no longer fit. No width class is consulted: the row wraps at exactly
/// the width its own contents stop fitting at, which is the right width whatever
/// the window is doing.
pub fn browser_settings_compact_row(title: impl Into<SharedString>, choices: impl IntoElement, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .px(px(SPACE_MD))
        .py(px(SPACE_SM))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(SPACE_SM))
        .child(div().flex_none().max_w_full().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(title.into()))
        .child(choices)
}

pub fn browser_settings_choice_button(id: impl Into<ElementId>, label: impl Into<SharedString>, checked: bool, theme: BrowserTheme) -> Button {
    // Button installs its own hover style during render; use the variant's
    // hover treatment to avoid registering the style twice.
    // `ghost` paints its own background from the variant, so the explicit
    // `bg` below is what actually decides an unselected segment's colour: the
    // surface, the same as the row it sits on.
    crate::base_button(id)
        .label(label)
        .ghost()
        .when(checked, |button| button.bg(theme.accent).text_color(theme.accent_text))
        .when(!checked, |button| button.bg(theme.page_bg))
        .flex_1()
        .min_w_0()
        .h(gpui::rems(SETTINGS_CONTROL_SIZE_REM))
        .px(px(SPACE_SM))
        .rounded(px(0.0))
        .border_0()
        .bg(if checked { theme.accent } else { theme.page_bg })
        .text_color(if checked { theme.accent_text } else { theme.text })
        .text_size(gpui::rems(TEXT_XS))
}

/// The control for a setting whose value *is* a path: the path is the button's
/// label, so the row reads "Default save location — ~/Books" and clicking the
/// value is what changes it.
///
/// A path is long and a settings column is not, so this one grows to the space
/// available and truncates from the left, keeping the leaf folder — the part
/// that identifies it — rather than the root, which is the same for every path
/// a user is choosing between.
pub fn browser_settings_path_button(id: impl Into<ElementId>, path: impl Into<SharedString>, theme: BrowserTheme) -> Button {
    crate::outlined_text_button(id, path).h(gpui::rems(SETTINGS_CONTROL_SIZE_REM)).px(px(SPACE_SM)).rounded(px(RADIUS_SM)).border_color(theme.border).text_color(theme.text).text_size(gpui::rems(TEXT_XS))
}

/// Abbreviate only a complete home-directory prefix; keep the actual path for
/// opening the folder and for the tooltip.
pub fn library_path_label(path: &std::path::Path) -> String {
    let (variable, alias) = if cfg!(target_os = "windows") { ("USERPROFILE", "\\") } else { ("HOME", "~") };
    if let Some(home) = std::env::var_os(variable).filter(|home| !home.is_empty()) {
        let home = std::path::PathBuf::from(home);
        if home.is_absolute()
            && let Ok(relative) = path.strip_prefix(&home)
        {
            return if relative.as_os_str().is_empty() { alias.to_owned() } else { std::path::Path::new(alias).join(relative).display().to_string() };
        }
    }
    path.display().to_string()
}
