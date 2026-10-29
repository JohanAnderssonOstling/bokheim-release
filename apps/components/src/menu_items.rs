use gpui::prelude::*;
use gpui::{App, ClickEvent, ElementId, Hsla, IntoElement, MouseButton, Pixels, SharedString, Window, div, px};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use std::{cell::Cell, rc::Rc};

use crate::{TEXT_SM, browser_theme};

#[derive(Clone, Copy)]
struct SplitActionStyle {
    text: Hsla,
    accent: Hsla,
    accent_text: Hsla,
    radius: Pixels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitSide {
    Left,
    Right,
}

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
    let item = PopupMenuItem::element(move |_, _| {
        let row = div().flex().flex_1().items_center().gap(px(6.0)).text_size(gpui::rems(TEXT_SM));

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
/// Up/Down selects this row, Left/Right chooses a half, and Enter activates it.
pub fn menu_item_with_split_actions(left_label: impl Into<SharedString>, right_label: impl Into<SharedString>, on_click: impl Fn(SplitSide, &mut Window, &mut App) + 'static) -> PopupMenuItem {
    let left_label = left_label.into();
    let right_label = right_label.into();
    let on_click = Rc::new(on_click);

    let side = Rc::new(Cell::new(SplitSide::Left));
    let render_side = side.clone();
    let select_side = side.clone();
    let activate = on_click.clone();
    PopupMenuItem::element_with_selection(move |selected, _, cx| {
        let theme = browser_theme(cx);
        // Matches the color, radius and text-swap every other row uses on
        // hover — the framework's own accent tokens, not this app's
        // `BrowserTheme`, since that is what actually paints a plain row's
        // highlight.
        let style = SplitActionStyle { text: theme.text, accent: *cx.theme().tokens.accent, accent_text: cx.theme().accent_foreground, radius: cx.theme().radius.min(px(8.0)) };
        div()
            .flex()
            .flex_1()
            .items_stretch()
            .child(split_action_half("menu-split-action-left", SplitSide::Left, left_label.clone(), style, selected && render_side.get() == SplitSide::Left, render_side.clone(), on_click.clone()))
            .child(div().flex_none().w(px(1.0)).my(px(2.0)).bg(theme.rule))
            .child(split_action_half("menu-split-action-right", SplitSide::Right, right_label.clone(), style, selected && render_side.get() == SplitSide::Right, render_side.clone(), on_click.clone()))
            .into_any_element()
    })
    .icon(IconName::ArrowLeft)
    .on_select_side(move |right, _, _| select_side.set(if right { SplitSide::Right } else { SplitSide::Left }))
    .on_click(move |_, window, cx| activate(side.get(), window, cx))
}

fn split_action_half<F>(id: impl Into<ElementId>, side: SplitSide, label: SharedString, style: SplitActionStyle, selected: bool, active_side: Rc<Cell<SplitSide>>, on_click: Rc<F>) -> impl IntoElement
where
    F: Fn(SplitSide, &mut Window, &mut App) + 'static,
{
    let SplitActionStyle { text, accent, accent_text, radius } = style;
    div()
        .id(id.into())
        .flex_1()
        .flex()
        .items_center()
        .when(side == SplitSide::Right, |row| row.justify_end().ml(px(6.0)))
        .when(side == SplitSide::Left, |row| row.mr(px(6.0)))
        .gap(px(6.0))
        .py(px(4.0))
        .rounded(radius)
        .cursor_pointer()
        .text_size(gpui::rems(TEXT_SM))
        .text_color(text)
        .when(selected, |row| row.bg(accent).text_color(accent_text))
        .hover(move |style| style.bg(accent).text_color(accent_text))
        .on_hover(move |hovered, window, _| {
            if *hovered && active_side.replace(side) != side {
                window.refresh();
            }
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_: &ClickEvent, window, cx| {
            cx.stop_propagation();
            on_click(side, window, cx);
        })
        .child(label)
        .when(side == SplitSide::Right, |row| row.child(Icon::new(IconName::ArrowRight).xsmall()))
}

/// Build a non-interactive popup menu section heading.
pub fn menu_section(label: impl Into<SharedString>) -> PopupMenuItem {
    PopupMenuItem::label(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Entity, Focusable, Render, TestAppContext};
    use gpui_component::menu::PopupMenu;

    struct MenuHost(Entity<PopupMenu>);

    impl Render for MenuHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.0.clone()
        }
    }

    #[gpui::test]
    fn choice_menu_navigation_skips_disabled_rows_and_waits_for_confirmation(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let chosen = Rc::new(Cell::new("Current"));
        let selected = chosen.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let menu = PopupMenu::build(window, cx, move |menu, _, _| {
                menu.item(menu_item("Disabled", |_, _, _| panic!("disabled choice must not activate")).disabled(true))
                    .item(menu_item_with_icon("Current", IconName::ArrowLeft, |_, _, _| {}).checked(true))
                    .item(menu_item("Other", move |_, _, _| selected.set("Other")))
            });
            menu.read(cx).focus_handle(cx).focus(window, cx);
            MenuHost(menu)
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("down down");
        assert_eq!(chosen.get(), "Current", "moving focus must not apply another choice");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(chosen.get(), "Other");
    }
}
