use gpui::prelude::*;
use gpui::{App, ClickEvent, ElementId, IntoElement, MouseButton, SharedString, Window, div, px};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};

use crate::{TEXT_SM, browser_theme};

/// Build a text-only popup menu item with the project's common layout.
pub fn menu_item<L, F>(label: L, on_click: F) -> PopupMenuItem
where
    L: Into<SharedString>,
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    menu_item_impl(label.into(), None, on_click)
}

/// Build a popup menu item with an icon and the project's common layout.
pub fn menu_item_with_icon<L, I, F>(label: L, icon: I, on_click: F) -> PopupMenuItem
where
    L: Into<SharedString>,
    I: Into<Icon> + 'static,
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    menu_item_impl(label.into(), Some(icon.into()), on_click)
}

fn menu_item_impl<F>(label: SharedString, icon: Option<Icon>, on_click: F) -> PopupMenuItem
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    let item = PopupMenuItem::element(move |_, cx| {
        let theme = browser_theme(cx);
        let row = div().flex().flex_1().items_center().gap(px(6.0)).text_size(gpui::rems(TEXT_SM)).text_color(theme.text);

        row.child(div().min_w_0().text_ellipsis().child(label.clone()))
    });
    let item = if let Some(icon) = icon { item.icon(icon) } else { item };
    item.on_click(on_click)
}

/// Build a popup menu row split into two equal, independently clickable
/// halves (e.g. "Open left" / "Open right"), separated by a rule so the pair
/// reads as two symmetric controls rather than one wide click target.
///
/// The left arrow is set as the item's own icon rather than drawn by hand:
/// menus with any iconed item reserve a left icon column on every row, so
/// only the framework's own icon slot lands in the same place other rows'
/// icons do. A hand-drawn arrow there would stack after that column instead
/// of in it.
///
/// Marked `disabled` at the item level: that's what a plain menu item does
/// for its own click and hover fill, and both would be wrong here — the row
/// has two handlers, not one, and highlighting the whole row on hover would
/// hide which half is about to fire. Each half draws its own hover instead.
pub fn menu_item_with_split_actions(left_label: impl Into<SharedString>, right_label: impl Into<SharedString>, on_left: impl Fn(&mut Window, &mut App) + 'static, on_right: impl Fn(&mut Window, &mut App) + 'static) -> PopupMenuItem {
    let left_label = left_label.into();
    let right_label = right_label.into();
    let on_left = std::rc::Rc::new(on_left);
    let on_right = std::rc::Rc::new(on_right);

    PopupMenuItem::element(move |_, cx| {
        let theme = browser_theme(cx);
        // Matches the color, radius and text-swap every other row uses on
        // hover — the framework's own accent tokens, not this app's
        // `BrowserTheme`, since that is what actually paints a plain row's
        // highlight.
        let accent = *cx.theme().tokens.accent;
        let accent_text = cx.theme().accent_foreground;
        let radius = cx.theme().radius.min(px(8.0));
        div()
            .flex()
            .flex_1()
            .items_stretch()
            .child(split_action_half("menu-split-action-left", None, left_label.clone(), theme, accent, accent_text, radius, on_left.clone()))
            .child(div().flex_none().w(px(1.0)).my(px(2.0)).bg(theme.rule))
            .child(split_action_half("menu-split-action-right", Some(IconName::ArrowRight), right_label.clone(), theme, accent, accent_text, radius, on_right.clone()))
            .into_any_element()
    })
    .icon(IconName::ArrowLeft)
    .disabled(true)
}

#[allow(clippy::too_many_arguments)]
fn split_action_half(
    id: impl Into<ElementId>, trailing_arrow: Option<IconName>, label: SharedString, theme: crate::BrowserTheme, accent: gpui::Hsla, accent_text: gpui::Hsla, radius: gpui::Pixels, on_click: std::rc::Rc<dyn Fn(&mut Window, &mut App)>,
) -> impl IntoElement {
    div()
        .id(id.into())
        .flex_1()
        .flex()
        .items_center()
        .when(trailing_arrow.is_some(), |row| row.justify_end().ml(px(6.0)))
        .when(trailing_arrow.is_none(), |row| row.mr(px(6.0)))
        .gap(px(6.0))
        .py(px(4.0))
        .rounded(radius)
        .cursor_pointer()
        .text_size(gpui::rems(TEXT_SM))
        .text_color(theme.text)
        .hover(move |style| style.bg(accent).text_color(accent_text))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_: &ClickEvent, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
        .child(label)
        .children(trailing_arrow.map(|icon| Icon::new(icon).xsmall()))
}

/// Build a non-interactive popup menu section heading.
pub fn menu_section(label: impl Into<SharedString>) -> PopupMenuItem {
    PopupMenuItem::label(label)
}
