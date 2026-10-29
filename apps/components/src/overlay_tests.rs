use gpui::prelude::*;
use gpui::{Context, Entity, Render, ScrollDelta, ScrollWheelEvent, TestAppContext, Window, anchored, div, point, px};
use gpui_component::menu::{PopupMenu, PopupMenuItem};

struct MenuSurface {
    menu: Entity<PopupMenu>,
    centered: bool,
}

impl Render for MenuSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = div().debug_selector(|| "test-popup".into()).child(self.menu.clone());
        let overlay = if self.centered { gpui_component::popover::centered_overlay(menu, window, cx) } else { anchored().position(point(px(8.0), px(8.0))).child(menu) };
        div().size_full().child(overlay)
    }
}

#[gpui::test]
fn menus_fit_their_contents_and_scroll_only_at_the_viewport_limit(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for centered in [false, true] {
        for count in [2, 20, 100] {
            let (_, cx) = cx.add_window_view(move |window, cx| {
                let menu = PopupMenu::build(window, cx, |mut menu, _, _| {
                    menu = menu.scrollable(true);
                    for index in 0..count {
                        menu = menu.item(PopupMenuItem::element(move |_, _| div().h(px(26.0)).flex_none().debug_selector(move || format!("test-row-{index}")).child(format!("Choice {index}"))));
                    }
                    menu
                });
                MenuSurface { menu, centered }
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                _ = window.draw(cx);
            });
            let menu = cx.debug_bounds("test-popup").unwrap();
            let viewport = cx.update(|window, _| window.viewport_size());
            assert!(menu.size.height <= viewport.height - px(16.0));
            if centered {
                assert!((f32::from(menu.center().y - viewport.height / 2.0)).abs() < 1.0);
                assert!((f32::from(menu.center().x - viewport.width / 2.0)).abs() < 1.0);
            }
            match count {
                2 => assert!(menu.size.height < px(100.0)),
                20 => assert!(menu.size.height > px(450.0)),
                _ => {}
            }
            let initial_row = cx.debug_bounds("test-row-0").unwrap();
            cx.simulate_event(ScrollWheelEvent { position: menu.center(), delta: ScrollDelta::Pixels(point(px(0.0), px(-80.0))), ..Default::default() });
            cx.run_until_parked();
            cx.update(|window, cx| {
                _ = window.draw(cx);
            });
            let scrolled_row = cx.debug_bounds("test-row-0").unwrap();
            if count == 100 {
                assert!(scrolled_row.top() < initial_row.top());
            } else {
                assert_eq!(scrolled_row.top(), initial_row.top());
            }
        }
    }
}
