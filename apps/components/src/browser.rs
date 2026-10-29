use crate::{CONTENT_INSET, NAV_RAIL_ICON_SIZE_REM, NAV_RAIL_LABEL_SIZE, navigation_modal_row_height_rem, topbar_height_rem};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Bounds, Div, ElementId, Entity, FocusHandle, Focusable, FontWeight, MouseButton, ObjectFit, Pixels, ScrollHandle, SharedString, StyledImage, div, img, px, rems};
use crate::Button;
use gpui_component::input::InputState;
use gpui_component::{Icon, IconName};

use crate::{
    BrowserTheme, RADIUS_MD, RADIUS_SM, SPACE_LG, SPACE_MD, SPACE_SM, SPACE_XL, SPACE_XS, SPACE_XXS, Scrollable, ScrollableElement, TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XS, THEME_PICKER_WIDTH_REM, TITLE_MD, TOPBAR_ACTION_HORIZONTAL_PADDING,
    settings_control_size_rem, topbar_action_gap, topbar_action_height_rem,
};

/// A navigation destination, in the sidebar rail and the bottom bar alike: its
/// glyph (`NAV_RAIL_ICON_SIZE_REM` tall) over its name. The whole tile is the
/// button, and the current one takes the shared selection highlight. The
/// caller sizes the tile and sets the label's text, which the label inherits.
pub(crate) fn navigation_tile(id: impl Into<ElementId>, glyph: impl IntoElement, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    let button = crate::base_button(id)
        .ghost()
        .min_w_0()
        .p_0()
        .rounded(px(RADIUS_MD))
        .child(
            div()
                .size_full()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_XXS))
                .child(div().flex_none().child(glyph))
                .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_center().child(label.into())),
        );
    crate::selectable_button(button, selected, theme)
}

/// Keep the standard rail width until its actual interface labels need more.
pub fn navigation_rail_width<'a>(labels: impl IntoIterator<Item = &'a str>, window: &gpui::Window, cx: &gpui::App) -> Pixels {
    let normal = window.rem_size() * crate::NAV_RAIL_SIZE_REM;
    let required = labels.into_iter().map(|label| crate::interface_label_width(label, NAV_RAIL_LABEL_SIZE, window, cx) + px(2.0 * SPACE_XS)).max().unwrap_or(normal);
    normal.max(required).min((window.viewport_size().width * 0.35).max(normal))
}

/// A rail destination using the shared selected-button treatment.
pub fn nav_rail_item(id: impl Into<ElementId>, icon: impl Into<Icon>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    navigation_tile(id, Icon::new(icon).size(gpui::rems(NAV_RAIL_ICON_SIZE_REM)), label, selected, theme)
        .w_full()
        .h(gpui::rems(crate::NAV_RAIL_SIZE_REM))
        .flex_none()
        .px(px(SPACE_XS))
        .text_size(gpui::rems(NAV_RAIL_LABEL_SIZE))
}

/// A full-width bar along the bottom of the window, ruled off from the page
/// above: the navigation bar, and the audiobook dock.
pub fn bottom_bar(theme: BrowserTheme) -> Div {
    div().w_full().flex_none().flex().border_t_1().border_color(theme.rule).bg(theme.page_bg).text_color(theme.text)
}

/// Contextual commands stay at the bottom of their browse pane and wrap on
/// narrow screens without displacing the selection count or clear button.
pub fn selection_footer(theme: BrowserTheme) -> Div {
    bottom_bar(theme).min_w_0().items_center().px(px(SPACE_SM)).py(px(SPACE_XS)).gap(px(SPACE_SM))
}

pub fn bottom_navigation(theme: BrowserTheme) -> Div {
    bottom_bar(theme).h(gpui::rems(crate::BOTTOM_NAV_HEIGHT_REM)).flex_row().items_stretch()
}

pub fn bottom_navigation_item(id: impl Into<ElementId>, icon: impl Into<Icon>, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    bottom_navigation_tile(id, Icon::new(icon).size(gpui::rems(NAV_RAIL_ICON_SIZE_REM)), label, selected, theme)
}

/// A bottom bar cell: the navigation tile, sharing the bar's width with the
/// others and as tall as the bar.
pub(crate) fn bottom_navigation_tile(id: impl Into<ElementId>, glyph: impl IntoElement, label: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Button {
    navigation_tile(id, glyph, label, selected, theme).h_full().flex_1().text_size(gpui::rems(TEXT_XS))
}

/// A modal list sized from its fixed-height header and rows, capped only by
/// the viewport space inside the modal overlay. The caller supplies a
/// scrollable body beneath the fixed header.
///
/// The height is stated rather than left to the contents because the list
/// scrolls, and a scroll container needs a definite height. It is worked out at
/// the window's `rem_size`: the header and rows are rems, the gaps and padding
/// are pixels, and a height reckoned at a fixed 16px rem cut the rows off at
/// any larger UI font.
pub fn navigation_modal_list(item_count: usize, rem_size: Pixels, theme: BrowserTheme) -> Div {
    let rem = f32::from(rem_size);
    let rows = (item_count + 1) as f32 * navigation_modal_row_height_rem() * rem;
    let gaps = item_count as f32 * SPACE_XS + SPACE_MD * 2.0;
    let height = rows + gaps + 2.0; // Include the surface's top and bottom borders.
    crate::modal_surface(theme).w(gpui::rems(crate::NAVIGATION_MODAL_WIDTH_REM)).max_w_full().h(px(height)).max_h_full().flex().flex_col()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavigationIcon {
    House,
    BookOpen,
    Books,
    Folder,
    Tags,
    User,
    Trash2,
    Building2,
    Settings,
    Storefront,
}

pub fn navigation_icon(icon: NavigationIcon) -> Icon {
    let path = match icon {
        NavigationIcon::House => "icons/navigation/house.svg",
        NavigationIcon::BookOpen => "icons/navigation/book-open.svg",
        NavigationIcon::Books => "icons/navigation/books.svg",
        NavigationIcon::Folder => "icons/navigation/folder.svg",
        NavigationIcon::Tags => "icons/navigation/tags.svg",
        NavigationIcon::User => "icons/navigation/user.svg",
        NavigationIcon::Trash2 => "icons/navigation/trash-2.svg",
        NavigationIcon::Building2 => "icons/navigation/building-2.svg",
        NavigationIcon::Settings => "icons/navigation/settings.svg",
        NavigationIcon::Storefront => "icons/storefront.svg",
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
        .child(div().w_full().min_w_0().h(gpui::rems(topbar_action_height_rem())).flex_1().child(gpui::Styled::h(gpui_component::input::Input::new(state).cleanable(true), gpui::rems(topbar_action_height_rem()))))
}

/// Keep the URL editor mounted for keyboard focus, covering it with a centered
/// site identity until the user activates the field.
pub fn browser_address_control(state: &Entity<InputState>, active: bool, name: impl Into<SharedString>, favicon: Option<&str>, theme: BrowserTheme) -> Div {
    let state = state.clone();
    browser_search_control(&state).relative().when(!active, move |control| {
        let fallback = move || Icon::new(IconName::Globe).size(rems(TEXT_SM)).text_color(theme.text_muted).into_any_element();
        let glyph = div()
            .flex_none()
            .size(rems(TEXT_SM))
            .when_some(favicon, |glyph, url| glyph.child(img(url.to_owned()).size_full().object_fit(ObjectFit::Contain).with_fallback(fallback).with_loading(fallback)))
            .when(favicon.is_none(), |glyph| glyph.child(fallback()));
        control.child(
            div()
                .id("browser-site-identity")
                .debug_selector(|| "browser-site-identity".into())
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .min_w_0()
                .overflow_hidden()
                .px(px(SPACE_SM))
                .border_1()
                .border_color(theme.border)
                .rounded(px(RADIUS_SM))
                .bg(theme.page_bg)
                .text_color(theme.text)
                .text_size(rems(TEXT_SM))
                .cursor_text()
                .occlude()
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.prevent_default();
                    state.update(cx, |state, cx| {
                        state.focus_handle(cx).focus(window, cx);
                        state.set_selected_range(0..state.value().len(), cx);
                    });
                })
                .child(
                    div()
                        .debug_selector(|| "browser-site-identity-content".into())
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(SPACE_XS))
                        .min_w_0()
                        .max_w_full()
                        .child(glyph)
                        .child(div().debug_selector(|| "browser-site-name".into()).min_w_0().text_ellipsis().whitespace_nowrap().child(name.into())),
                ),
        )
    })
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

pub fn library_switcher_group() -> Div {
    // Reserve the tile's full height so it stays above Home and inside the rail.
    div().relative().w_full().h(gpui::rems(crate::NAV_RAIL_SIZE_REM)).flex_none().flex().items_center().gap(px(SPACE_SM))
}

/// The default bar along the top of a page. The page colour is what lets it
/// lie over content that scrolls or pages under it. In the library
/// it carries no underline: the breadcrumb and the actions already mark the
/// boundary. See [`crate::nav_rail_destinations`].
pub fn topbar(theme: BrowserTheme) -> Div {
    div().w_full().min_w_0().h(gpui::rems(topbar_height_rem())).flex_none().flex().flex_row().items_center().gap(px(SPACE_MD)).bg(theme.page_bg)
}

/// The reader's single-row topbar, sized to its controls without extra vertical
/// inset and ruled off from the book under it.
pub fn ruled_topbar(theme: BrowserTheme) -> Div {
    topbar(theme).h_auto().min_h(gpui::rems(topbar_action_height_rem())).px(px(SPACE_XS)).border_b_1().border_color(theme.rule)
}

pub fn browser_topbar_left() -> Div {
    div().flex_1().min_w_0().flex().items_center().justify_start()
}

pub fn mobile_back_button() -> Button {
    crate::outlined_icon_button("mobile-navigation-back", crate::shortcut_tooltip("Back", "escape"), IconName::ArrowLeft).debug_selector(|| "mobile-navigation-back".into())
}

/// Shared navigation heading for browsing, book loading, and reading. `back`
/// is `None` on native mobile, where the OS back button/gesture already does
/// the same thing — see `has_native_back_button`.
pub fn mobile_navigation_heading(title: impl Into<SharedString>, back: Option<Button>, theme: BrowserTheme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(SPACE_SM))
        .pl(px(CONTENT_INSET))
        .children(back)
        .child(div().flex_1().min_w_0().truncate().when_some(theme.display_font_family(), |text, family| text.font_family(family)).text_size(gpui::rems(TEXT_MD)).child(title.into()))
}

/// A navigation heading whose title opens a menu, made by
/// `mobile_navigation_menu_title`.
pub fn mobile_navigation_menu_heading(title: impl IntoElement, back: Option<Button>) -> Div {
    div().flex_1().min_w_0().flex().items_center().gap(px(SPACE_SM)).pl(px(CONTENT_INSET)).children(back).child(div().flex_1().min_w_0().flex().child(title))
}

/// A heading title that opens a menu: the title as it always reads, with a
/// chevron saying it can be changed. Pulled left by its own padding so the
/// text lines up with the plain titles of other pages.
pub fn mobile_navigation_menu_title(id: impl Into<ElementId>, title: impl Into<SharedString>, theme: BrowserTheme) -> Button {
    crate::base_button(id).ghost().min_w_0().h(gpui::rems(topbar_action_height_rem())).px(px(SPACE_XS)).ml(px(-SPACE_XS)).child(
        div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .child(div().min_w_0().truncate().when_some(theme.display_font_family(), |text, family| text.font_family(family)).text_size(gpui::rems(TEXT_MD)).child(title.into()))
            .child(Icon::new(IconName::ChevronDown).flex_none().text_color(theme.text_muted)),
    )
}

pub fn mobile_navigation_bar(title: impl Into<SharedString>, back: Option<Button>, theme: BrowserTheme) -> Div {
    topbar(theme).child(mobile_navigation_heading(title, back, theme))
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
    div().flex_none().min_w_0().flex().items_center().justify_end().gap(px(topbar_action_gap())).pr(px(CONTENT_INSET))
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

/// Centred empty state with actions but no explanatory subheader.
pub fn browser_actionable_empty(title: impl Into<SharedString>, actions: impl IntoElement, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .min_h(px(240.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(SPACE_SM))
        .text_center()
        .child(div().when_some(theme.display_font_family(), |text, family| text.font_family(family)).text_size(gpui::rems(TITLE_MD)).child(title.into()))
        .child(div().mt(px(SPACE_XS)).child(actions))
}

pub fn library_switcher_trigger(label: impl Into<SharedString>, theme: BrowserTheme) -> Button {
    nav_rail_item("library-switcher-trigger", navigation_icon(NavigationIcon::Books), label, false, theme).font_weight(FontWeight::NORMAL).text_color(theme.text_accent)
}

pub fn library_switcher_panel() -> Div {
    div().w_full().flex().flex_col().gap(px(SPACE_XS))
}

pub fn library_switcher_header(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().h(gpui::rems(navigation_modal_row_height_rem())).w_full().flex().items_center().justify_between().pl(px(SPACE_SM)).pb(px(SPACE_XS)).text_size(gpui::rems(TEXT_MD)).text_color(theme.text).child(title.into())
}

/// The keyboard cursor uses the light accent, shared by every square icon
/// action in the library switcher list.
fn library_switcher_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: IconName, cursor: bool, theme: BrowserTheme) -> Button {
    crate::outlined_icon_button(id, crate::shortcut_tooltip(label, "enter"), icon).flex_none().when(cursor, |button| button.bg(theme.accent_wash))
}

pub fn library_switcher_add_button(cursor: bool, theme: BrowserTheme) -> Button {
    library_switcher_action_button("library-switcher-add", "Add library", IconName::Plus, cursor, theme)
}

/// The library in use shares the selected-button highlight. The keyboard
/// cursor adds a light highlight and deepens it on the selected library.
pub fn library_switcher_item(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, cursor: bool, theme: BrowserTheme) -> Button {
    let label = label.into();
    let button = crate::base_button(id)
        .ghost()
        .when(cursor && !selected, |button| button.bg(theme.accent_wash))
        .flex_1()
        .min_w_0()
        .h(gpui::rems(navigation_modal_row_height_rem()))
        .px(px(SPACE_SM))
        .justify_start()
        .overflow_hidden()
        .text_size(gpui::rems(TEXT_SM))
        .child(div().w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label));
    crate::selectable_button(button, selected, theme).when(selected && cursor && !cfg!(feature = "kobo"), |button| button.bg(theme.accent_hover))
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
        .text_size(gpui::rems(TEXT_SM))
        .font_weight(crate::TOPBAR_ACTION_LABEL_WEIGHT)
        .h(gpui::rems(topbar_action_height_rem()))
        .px(px(TOPBAR_ACTION_HORIZONTAL_PADDING))
        .rounded(px(RADIUS_MD))
}

/// A topbar action that destroys something for good, told apart from the
/// actions beside it by its colour.
pub fn topbar_danger_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: IconName, theme: BrowserTheme) -> Button {
    topbar_action_button(id, label, icon, theme).text_color(theme.danger)
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

/// The column the sections stack in, with the same air below the last section
/// as above the first, and an inset from the edges of narrow windows.
///
/// Sections are open — a heading and its rows, no box — so the space between
/// them is what separates one from the next, and it is wider than the space
/// inside one.
pub fn browser_settings_content() -> Div {
    div().w_full().max_w(px(640.0)).flex().flex_col().gap(px(SPACE_XL)).px(px(SPACE_MD)).py(px(SPACE_LG))
}

pub fn browser_settings_row(title: impl Into<SharedString>, description: impl Into<SharedString>, control: impl IntoElement, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .min_h(px(76.0))
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
    div().w_full().flex().flex_col().gap(px(SPACE_XL))
}

/// A block of settings: a heading and the rows under it, with no box.
///
/// Each group used to be a bordered region in a narrow column, which in a
/// wide window was a strip of boxes, and a box around one row was mostly
/// border. Now the heading names the section and every row opens with a rule,
/// so the first rule doubles as the heading's underline.
pub fn browser_settings_group() -> Div {
    div().w_full().flex().flex_col()
}

/// A titled block, its heading flush with the rows under it.
pub fn browser_settings_fieldset(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    browser_settings_group().child(browser_settings_header(title, theme))
}

/// A group's content that is not a setting row — who is signed in, a form, a
/// message — spaced below its heading without an underline.
pub fn browser_settings_block() -> Div {
    div().w_full().py(px(SPACE_SM)).flex().flex_col().gap(px(SPACE_SM))
}

/// Something the page needs you to act on, set apart from the settings by a
/// tint rather than drawn as one more group of them.
pub fn browser_settings_banner(theme: BrowserTheme) -> Div {
    div().w_full().bg(theme.accent_wash).border_l(px(3.0)).border_color(theme.accent)
}

/// A titled block whose header carries a control of its own, on the right.
///
/// For the one group whose subject is a thing you act on rather than a list of
/// settings: the account. Its control — sign in, or who is signed in — is not a
/// setting inside the group, it is what the group is about, so it sits in the
/// header beside the name instead of becoming the first row.
pub fn browser_settings_fieldset_with_actions(title: impl Into<SharedString>, actions: impl IntoElement, theme: BrowserTheme) -> Div {
    browser_settings_group().child(browser_settings_header(title, theme).child(div().flex_none().flex().flex_row().items_center().gap(px(SPACE_SM)).child(actions)))
}

/// A group's header bar: on the rows' line, and at least as tall as a header
/// button, so a group whose header has none is exactly as tall as one whose
/// header does.
fn browser_settings_header(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .w_full()
        .py(px(SPACE_SM))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(SPACE_SM))
        .child(browser_settings_legend(title, theme).flex_none().w_auto().min_h(gpui::rems(settings_control_size_rem())).flex().items_center())
}

/// An action in a settings group's header.
///
/// The same size as the controls in the rows below it, because it is read as
/// one of them; `primary` highlights it, for the one of a pair a returning user
/// almost always wants.
pub fn browser_settings_header_button(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool, theme: BrowserTheme) -> Button {
    let button = crate::base_button(id)
        .label(label)
        .ghost()
        .h(gpui::rems(settings_control_size_rem()))
        .px(px(SPACE_MD))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_color(if primary { theme.accent } else { theme.border })
        .text_size(gpui::rems(TEXT_XS));
    crate::selectable_button(button, primary, theme).when(primary && cfg!(feature = "kobo"), |button| button.border_1())
}

/// The name a settings group is read by: a section heading in the display
/// face, as on the book detail page. It was muted small caps, quieter than the
/// controls under it, so the eye reached the controls before it knew which
/// section it was in.
fn browser_settings_legend(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().when_some(theme.display_font_family(), |text, family| text.font_family(family)).text_size(gpui::rems(TEXT_LG)).text_color(theme.text).child(title.into())
}

/// What went wrong, said inside the group it went wrong in.
///
/// A failure belongs next to the control that caused it. One alert at the foot
/// of the page reported a theme that would not save from below the library
/// list, where nobody flipping a theme is looking.
pub fn browser_settings_group_error(text: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().w_full().py(px(SPACE_SM)).border_t_1().border_color(theme.danger).flex().flex_row().gap(px(SPACE_SM)).text_size(gpui::rems(TEXT_XS)).text_color(theme.danger).child(text.into())
}

/// Shared choice styling, sized to align with the application theme picker.
pub fn browser_settings_button_group(theme: BrowserTheme) -> Div {
    crate::settings_choice_button_group(theme).w(gpui::rems(THEME_PICKER_WIDTH_REM)).max_w_full()
}

/// One setting: its name on the left, its control on the right.
///
/// Rows have a leading separator; callers remove it from the first row so
/// section headings remain unruled.
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
        .py(px(SPACE_SM))
        .border_t_1()
        .border_color(theme.rule)
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(SPACE_SM))
        .child(div().flex_none().max_w_full().text_size(gpui::rems(TEXT_XS)).text_color(theme.text_muted).child(title.into()))
        .child(choices)
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
    crate::outlined_text_button(id, path).h(gpui::rems(settings_control_size_rem())).px(px(SPACE_SM)).rounded(px(RADIUS_SM)).border_color(theme.border).text_size(gpui::rems(TEXT_XS))
}

/// Show normal Windows paths and abbreviate Unix home-directory prefixes;
/// keep the actual path for opening the folder and for the tooltip.
pub fn library_path_label(path: &Path) -> String {
    #[cfg(target_os = "windows")]
    let path = dunce::simplified(path);
    #[cfg(not(target_os = "windows"))]
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        let home = std::path::PathBuf::from(home);
        if home.is_absolute()
            && let Ok(relative) = path.strip_prefix(&home)
        {
            return if relative.as_os_str().is_empty() { "~".to_owned() } else { Path::new("~").join(relative).display().to_string() };
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, Context, Modifiers, Render, ScrollDelta, ScrollWheelEvent, TestAppContext, Window, point};
    use gpui_component::Root;

    struct RailHarness {
        rem_size: Pixels,
    }

    impl Render for RailHarness {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(self.rem_size);
            let theme = BrowserTheme::default();
            let switcher = library_switcher_group().debug_selector(|| "test-switcher-group".into()).child(library_switcher_trigger("Library", theme).debug_selector(|| "test-switcher-button".into()));
            let mut destinations = crate::nav_rail_destinations();
            for (index, label) in ["Home", "Books", "Folders", "Subjects", "Authors", "Catalog", "Settings"].into_iter().enumerate() {
                destinations = destinations.child(nav_rail_item(("test-rail", index), IconName::BookOpen, label, index == 0, theme).debug_selector(move || format!("test-rail-{index}")));
            }
            div().w(rems(crate::NAV_RAIL_SIZE_REM)).h(px(240.0)).flex().debug_selector(|| "test-rail-viewport".into()).child(crate::sidebar(theme).child(switcher).child(destinations))
        }
    }

    #[gpui::test]
    fn switcher_does_not_overlap_home_and_short_rails_scroll_to_settings(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for rem_size in [16.0, 24.0] {
            let (_, visual) = cx.add_window_view(|window, cx| {
                let content = cx.new(|_| RailHarness { rem_size: px(rem_size) });
                Root::new(content, window, cx)
            });
            visual.run_until_parked();
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let viewport = visual.debug_bounds("test-rail-viewport").unwrap();
            let group = visual.debug_bounds("test-switcher-group").unwrap();
            let switcher = visual.debug_bounds("test-switcher-button").unwrap();
            let home = visual.debug_bounds("test-rail-0").unwrap();
            assert_eq!(switcher.size.height, px(crate::NAV_RAIL_SIZE_REM * rem_size));
            assert_eq!(group.size.height, switcher.size.height);
            assert!(switcher.top() >= viewport.top());
            assert!(home.top() >= switcher.bottom());
            visual.simulate_event(ScrollWheelEvent { position: viewport.center(), delta: ScrollDelta::Pixels(point(px(0.0), px(-10_000.0))), modifiers: Modifiers::default(), ..Default::default() });
            visual.run_until_parked();
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let settings = visual.debug_bounds("test-rail-6").unwrap();
            assert!(settings.top() >= viewport.top());
            assert!(settings.bottom() <= viewport.bottom());
            assert!(settings.left() >= viewport.left());
            assert!(settings.right() <= viewport.right());
        }
    }
}
