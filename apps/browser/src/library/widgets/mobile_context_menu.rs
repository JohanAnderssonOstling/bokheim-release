//! Shared placement for library context menus on mobile screens.

use gpui::{Anchor, AnyElement, Entity, IntoElement, ParentElement, Pixels, Point, Window, anchored, deferred, point, px};
use gpui_component::menu::PopupMenu;
use ui_components as components;

pub(crate) fn size(window: &Window) -> Option<(Pixels, Pixels)> {
    if !components::uses_mobile_navigation(window) {
        return None;
    }
    let safe = window.insets().safe_area;
    let viewport = window.viewport_size();
    let available_width = (f32::from(viewport.width - safe.left - safe.right)).max(1.0);
    let available_height = (f32::from(viewport.height - safe.top - safe.bottom)).max(1.0);
    Some((px(available_width.min(560.0)), px(available_height * 0.8)))
}

pub(crate) fn configure(menu: PopupMenu, mobile_size: Option<(Pixels, Pixels)>) -> PopupMenu {
    match mobile_size {
        Some((width, height)) => menu.min_w(width).max_w(width).max_h(height).scrollable(true).touch_friendly(true),
        None => menu,
    }
}

pub(crate) fn render(menu: Entity<PopupMenu>, position: Point<Pixels>, window: &Window) -> AnyElement {
    if let Some((width, _)) = size(window) {
        let safe = window.insets().safe_area;
        let viewport = window.viewport_size();
        let available_width = viewport.width - safe.left - safe.right;
        let centered_left = safe.left + px((f32::from(available_width - width) / 2.0).max(0.0));
        deferred(anchored().position(point(centered_left, viewport.height - safe.bottom)).anchor(Anchor::BottomLeft).snap_to_window().child(menu)).with_priority(1).into_any_element()
    } else {
        deferred(anchored().position(position).snap_to_window_with_margin(px(8.0)).anchor(Anchor::TopLeft).child(menu)).with_priority(1).into_any_element()
    }
}
