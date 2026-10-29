use std::{cell::RefCell, rc::Rc};

use gpui::{
    Anchor, AnyElement, App, Context, DismissEvent, Element, ElementId, Entity, Focusable,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement, IntoElement,
    KeyBinding, LongPressEvent, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, StyleRefinement, Styled,
    Subscription, Window, actions, anchored, deferred, div, px,
};

use crate::menu::PopupMenu;

actions!(context_menu, [OpenContextMenu]);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", OpenContextMenu, Some("!PopupMenu && (!Dialog || Input)")),
        KeyBinding::new("shift-f10", OpenContextMenu, Some("!PopupMenu && (!Dialog || Input)")),
        KeyBinding::new("menu", OpenContextMenu, Some("!PopupMenu && (!Dialog || Input)")),
    ]);
}

type MenuBuilder = dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu;

/// A extension trait for adding a context menu to an element.
pub trait ContextMenuExt: InteractiveElement + ParentElement + Styled {
    /// Add a context menu to the element.
    ///
    /// This will changed the element to be `relative` positioned, and add a child `ContextMenu` element.
    /// Because the `ContextMenu` element is positioned `absolute`, it will not affect the layout of the parent element.
    #[track_caller]
    fn context_menu(
        mut self,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> ContextMenu<Self>
    where
        Self: Sized,
    {
        // The ID must be stable across renders, otherwise the element state
        // (open menu) is lost on every re-render.
        let caller = std::panic::Location::caller();
        let id = self
            .interactivity()
            .element_id
            .clone()
            .map(|id| ElementId::Name(format!("context-menu-{:?}", id).into()))
            .unwrap_or_else(|| ElementId::CodeLocation(*caller));
        ContextMenu::new(id, self).menu(f)
    }
}

impl<E: InteractiveElement + ParentElement + Styled> ContextMenuExt for E {}

/// A context menu opened by right-click or the context-menu keyboard shortcuts.
pub struct ContextMenu<E: ParentElement + Styled + Sized> {
    id: ElementId,
    element: Option<E>,
    menu: Option<Rc<MenuBuilder>>,
    keyboard_menu: Option<Rc<MenuBuilder>>,
    // This is not in use, just for style refinement forwarding.
    _ignore_style: StyleRefinement,
    anchor: Anchor,
}

impl<E: ParentElement + Styled> ContextMenu<E> {
    /// Create a new context menu with the given ID.
    pub fn new(id: impl Into<ElementId>, element: E) -> Self {
        Self {
            id: id.into(),
            element: Some(element),
            menu: None,
            keyboard_menu: None,
            anchor: Anchor::TopLeft,
            _ignore_style: StyleRefinement::default(),
        }
    }

    /// Build the context menu using the given builder function.
    #[must_use]
    fn menu<F>(mut self, builder: F) -> Self
    where
        F: Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    {
        self.menu = Some(Rc::new(builder));
        self
    }

    /// Use selection or keyboard focus to resolve menu contents instead of the pointer.
    #[must_use]
    pub fn keyboard_menu(mut self, builder: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static) -> Self {
        self.keyboard_menu = Some(Rc::new(builder));
        self
    }

    fn with_element_state<R>(
        &mut self,
        id: &GlobalElementId,
        window: &mut Window,
        cx: &mut App,
        f: impl FnOnce(&mut Self, &mut ContextMenuState, &mut Window, &mut App) -> R,
    ) -> R {
        window.with_optional_element_state::<ContextMenuState, _>(
            Some(id),
            |element_state, window| {
                let mut element_state = element_state.unwrap().unwrap_or_default();
                let result = f(self, &mut element_state, window, cx);
                (result, Some(element_state))
            },
        )
    }
}

impl<E: ParentElement + Styled> ParentElement for ContextMenu<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        if let Some(element) = &mut self.element {
            element.extend(elements);
        }
    }
}

impl<E: ParentElement + Styled> Styled for ContextMenu<E> {
    fn style(&mut self) -> &mut StyleRefinement {
        if let Some(element) = &mut self.element {
            element.style()
        } else {
            &mut self._ignore_style
        }
    }
}

impl<E: InteractiveElement + ParentElement + Styled + IntoElement + 'static> IntoElement for ContextMenu<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct ContextMenuSharedState {
    menu_view: Option<Entity<PopupMenu>>,
    open: bool,
    position: Point<Pixels>,
    _subscription: Option<Subscription>,
}

impl ContextMenuSharedState {
    fn open(state: Rc<RefCell<Self>>, builder: Option<Rc<MenuBuilder>>, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        // Keep the original focus when replacing an already open menu.
        let previous_focus_handle = window.focused(cx).and_then(|focused| {
            let state = state.borrow();
            match state.menu_view.as_ref() {
                Some(menu) if menu.read(cx).focus_handle == focused => menu.read(cx).previous_focus_handle.clone(),
                _ => Some(focused),
            }
        });
        {
            let mut state = state.borrow_mut();
            state.menu_view = None;
            state._subscription = None;
            state.position = position;
            state.open = true;
        }
        // Build after event dispatch, just as for a mouse-triggered menu.
        window.defer(cx, move |window, cx| {
            let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
                match &builder {
                    Some(build) => build(menu, window, cx),
                    None => menu,
                }
            });
            menu.update(cx, |menu, cx| menu.set_previous_focus(previous_focus_handle, cx));
            let subscription = window.subscribe(&menu, cx, {
                let state = state.clone();
                move |_, _: &DismissEvent, window, _cx| {
                    state.borrow_mut().open = false;
                    window.refresh();
                }
            });
            let mut state = state.borrow_mut();
            state.menu_view = Some(menu);
            state._subscription = Some(subscription);
            window.refresh();
        });
    }
}

pub struct ContextMenuState {
    element: Option<AnyElement>,
    shared_state: Rc<RefCell<ContextMenuSharedState>>,
}

impl Default for ContextMenuState {
    fn default() -> Self {
        Self {
            element: None,
            shared_state: Rc::new(RefCell::new(ContextMenuSharedState {
                menu_view: None,
                open: false,
                position: Default::default(),
                _subscription: None,
            })),
        }
    }
}

impl<E: InteractiveElement + ParentElement + Styled + IntoElement + 'static> Element for ContextMenu<E> {
    type RequestLayoutState = ContextMenuState;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let anchor = self.anchor;

        self.with_element_state(
            id.unwrap(),
            window,
            cx,
            |this, state: &mut ContextMenuState, window, cx| {
                let (position, open) = {
                    let shared_state = state.shared_state.borrow();
                    (shared_state.position, shared_state.open)
                };
                let menu_view = state.shared_state.borrow().menu_view.clone();
                let mut menu_element = None;
                if open {
                    let has_menu_item = menu_view
                        .as_ref()
                        .map(|menu| !menu.read(cx).is_empty())
                        .unwrap_or(false);

                    if has_menu_item {
                        if let Some(menu) = &menu_view {
                            if !menu.focus_handle(cx).contains_focused(window, cx) {
                                menu.focus_handle(cx).focus(window, cx);
                            }
                        }
                        let overlay = if cx.supports_touch_input() {
                            crate::popover::centered_overlay(div().children(menu_view), window, cx)
                        } else {
                            anchored()
                                .position(position)
                                .snap_to_window_with_margin(px(8.))
                                .anchor(anchor)
                                .children(menu_view)
                        };
                        menu_element = Some(
                            deferred(
                                anchored().child(
                                    div()
                                        .w(window.bounds().size.width)
                                        .h(window.bounds().size.height)
                                        .on_scroll_wheel(|_, _, cx| {
                                            cx.stop_propagation();
                                        })
                                        .child(overlay),
                                ),
                            )
                            .with_priority(1)
                            .into_any(),
                        );
                    }
                }

                let keyboard_state = state.shared_state.clone();
                let keyboard_builder = this.keyboard_menu.clone().or_else(|| this.menu.clone());
                let mut element = this
                    .element
                    .take()
                    .expect("Element should exists.")
                    .on_action(move |_: &OpenContextMenu, window, cx| {
                        ContextMenuSharedState::open(keyboard_state.clone(), keyboard_builder.clone(), window.mouse_position(), window, cx);
                    })
                    .children(menu_element)
                    .into_any_element();

                let layout_id = element.request_layout(window, cx);

                (
                    layout_id,
                    ContextMenuState {
                        element: Some(element),
                        ..Default::default()
                    },
                )
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if let Some(element) = &mut request_layout.element {
            element.prepaint(window, cx);
        }
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&gpui::GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(element) = &mut request_layout.element {
            element.paint(window, cx);
        }

        // Take the builder before setting up element state to avoid borrow issues
        let builder = self.menu.clone();

        self.with_element_state(
            id.unwrap(),
            window,
            cx,
            |_view, state: &mut ContextMenuState, window, _| {
                let shared_state = state.shared_state.clone();

                let hitbox = hitbox.clone();
                let press_state = shared_state.clone();
                let press_builder = builder.clone();
                let press_hitbox = hitbox.clone();
                window.on_long_press(move |event: &LongPressEvent, phase, window, cx| {
                    if phase.bubble() && press_hitbox.is_under_pointer(window) {
                        ContextMenuSharedState::open(press_state.clone(), press_builder.clone(), event.position, window, cx);
                        cx.stop_propagation();
                    }
                });
                // When right mouse click, to build content menu, and show it at the mouse position.
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase.bubble()
                        && event.button == MouseButton::Right
                        && hitbox.is_hovered(window)
                    {
                        ContextMenuSharedState::open(shared_state.clone(), builder.clone(), event.position, window, cx);
                    }
                });
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use gpui::{
        Context, FocusHandle, IntoElement, Render, TestAppContext, VisualTestContext, actions,
        point, px,
    };
    use std::cell::Cell;

    actions!(context_menu_test, [RemoveTab]);

    /// The regression shape: the action handler lives on the trigger's
    /// ancestor (like an action bar), which is NOT on the focus path while
    /// focus is in the content area.
    struct TestRoot {
        content_focus: FocusHandle,
        received: Rc<Cell<bool>>,
    }

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let received = self.received.clone();
            div()
                .size_full()
                .child(
                    div()
                        .id("content")
                        .h(px(40.))
                        .track_focus(&self.content_focus),
                )
                .child(
                    div()
                        .id("action-bar")
                        .h(px(60.))
                        .on_action(move |_: &RemoveTab, _, _| received.set(true))
                        .child(
                            div()
                                .id("tab")
                                .size_full()
                                .context_menu(|menu, _, _| menu.menu("Close", Box::new(RemoveTab))),
                        ),
                )
        }
    }

    struct KeyboardRoot {
        focus: FocusHandle,
        received: Rc<Cell<bool>>,
    }

    impl Render for KeyboardRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let received = self.received.clone();
            div()
                .id("keyboard-menu-trigger")
                .key_context("Root")
                .size_full()
                .track_focus(&self.focus)
                .on_action(move |_: &RemoveTab, _, _| received.set(true))
                .context_menu(|menu, _, _| menu.label("Pointer-only actions"))
                .keyboard_menu(|menu, _, _| menu.menu("Close", Box::new(RemoveTab)))
        }
    }

    #[gpui::test]
    fn keyboard_context_menu_shortcuts_open_menu_and_restore_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::default());
            super::init(cx);
            super::super::popup_menu::init(cx);
        });
        let received = Rc::new(Cell::new(false));
        let (root, cx) = cx.add_window_view({
            let received = received.clone();
            move |window, cx| {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                KeyboardRoot { focus, received }
            }
        });
        let focus = root.read_with(cx, |root, _| root.focus.clone());
        let cx: &mut VisualTestContext = cx;
        for shortcut in ["ctrl-enter", "shift-f10", "menu"] {
            received.set(false);
            cx.run_until_parked();
            cx.update(|window, cx| { _ = window.draw(cx); });
            cx.simulate_keystrokes(shortcut);
            cx.run_until_parked();
            cx.update(|window, cx| { _ = window.draw(cx); });
            cx.simulate_keystrokes("down enter");
            cx.run_until_parked();
            assert!(received.get(), "{shortcut} should open the same actionable menu");
            cx.update(|window, cx| {
                assert_eq!(window.focused(cx).as_ref(), Some(&focus));
            });
        }
    }

    #[gpui::test]
    fn action_bubbles_from_trigger_and_focus_restores_on_dismiss(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::default());
            super::super::popup_menu::init(cx);
        });

        let received = Rc::new(Cell::new(false));
        let (root, cx) = cx.add_window_view({
            let received = received.clone();
            move |window, cx| {
                let content_focus = cx.focus_handle();
                content_focus.focus(window, cx);
                TestRoot {
                    content_focus,
                    received,
                }
            }
        });
        let content_focus = root.read_with(cx, |root, _| root.content_focus.clone());
        let cx: &mut VisualTestContext = cx;
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        // Right-click inside the tab to open the context menu.
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Right,
            position: point(px(50.), px(70.)),
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        });
        // The menu entity is built in a deferred callback, then rendered
        // (which also focuses it) on the next draw.
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        // Select "Close" and confirm. Keyboard confirm and mouse click share
        // the same `confirm` path in `PopupMenu`.
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();

        // The action must reach the handler on the trigger's ancestor chain,
        // even though the action bar was never on the focus path.
        assert!(received.get());
        // And dismiss must restore focus to where it was before the menu
        // opened, keeping the dangling-focus fix (#2614).
        cx.update(|window, cx| {
            assert_eq!(window.focused(cx).as_ref(), Some(&content_focus));
        });
    }
}
