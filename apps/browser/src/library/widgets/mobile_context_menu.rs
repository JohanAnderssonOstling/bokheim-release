//! Shared lifetime and placement for library context menus.

use gpui::{Anchor, AnyElement, Context, Entity, Focusable, IntoElement, ParentElement, Pixels, Point, Subscription, Window, anchored, deferred, point, px};
use gpui_component::menu::PopupMenu;
use ui_components as components;

/// The single context menu over items, open space, or a pending drop.
pub(crate) struct ContextMenu {
    pub(crate) menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _subscription: Subscription,
}

impl ContextMenu {
    /// The owner supplies its dismissal policy and the guard that keeps it subscribed.
    pub(crate) fn new<T: 'static>(position: Point<Pixels>, window: &mut Window, cx: &mut Context<T>, items: impl FnOnce(PopupMenu) -> PopupMenu, subscribe: impl FnOnce(&Entity<PopupMenu>, &Window, &mut Context<T>) -> Subscription) -> Self {
        let mobile_size = size(window);
        let menu = PopupMenu::build(window, cx, move |menu, _, _| configure(items(menu), mobile_size));
        let subscription = subscribe(&menu, window, cx);
        menu.focus_handle(cx).focus(window, cx);
        Self { menu, position, _subscription: subscription }
    }

    pub(crate) fn render(&self, window: &Window) -> AnyElement {
        if let Some((width, _)) = size(window) {
            let safe = window.insets().effective();
            let viewport = window.viewport_size();
            let available_width = viewport.width - safe.left - safe.right;
            let centered_left = safe.left + px((f32::from(available_width - width) / 2.0).max(0.0));
            deferred(anchored().position(point(centered_left, viewport.height - safe.bottom)).anchor(Anchor::BottomLeft).snap_to_window().child(self.menu.clone())).with_priority(1).into_any_element()
        } else {
            deferred(anchored().position(self.position).snap_to_window_with_margin(px(8.0)).anchor(Anchor::TopLeft).child(self.menu.clone())).with_priority(1).into_any_element()
        }
    }
}

fn size(window: &Window) -> Option<(Pixels, Pixels)> {
    if !cfg!(feature = "kobo") && !components::touch_ui() && !components::uses_mobile_navigation(window) {
        return None;
    }
    let safe = window.insets().effective();
    let viewport = window.viewport_size();
    let available_width = (f32::from(viewport.width - safe.left - safe.right)).max(1.0);
    let available_height = (f32::from(viewport.height - safe.top - safe.bottom)).max(1.0);
    Some((px(available_width.min(560.0)), px(available_height * 0.8)))
}

fn configure(menu: PopupMenu, mobile_size: Option<(Pixels, Pixels)>) -> PopupMenu {
    match mobile_size {
        Some((width, height)) => menu.min_w(width).max_w(width).max_h(height).scrollable(true).touch_friendly(true),
        None => menu.touch_friendly(components::touch_ui()),
    }
}
