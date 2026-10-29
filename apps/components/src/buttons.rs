use gpui::prelude::*;
use gpui::{ElementId, SharedString, px, rems};
use crate::Button;
use gpui_component::{Disableable, Icon, IconName, Sizable, Size};

use crate::{BrowserTheme, SPACE_MD, TEXT_SM, topbar_action_height_rem};

/// Common entry point for interactive controls.
///
/// The application button component owns hover styling; this shared entry
/// point also gives every control the same pointer behavior.
pub fn base_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer()
}

/// Square action with a quiet border and the page background.
pub(crate) fn outlined_button(id: impl Into<ElementId>, theme: BrowserTheme) -> Button {
    base_button(id).border_color(theme.rule)
}

/// A borderless square icon action, `topbar_action_height_rem()` on a side (2rem,
/// or the touch target on phones), with a transparent background and
/// a 1rem icon and normal text — the standard size and
/// shape shared by every square icon button in the app (square corners, to
/// match every other native surface and control). Semantic wrappers own any
/// contextual state colour (hover, selection) on top of this baseline.
pub fn outlined_icon_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: impl Into<Icon>) -> Button {
    base_button(id).ghost().icon(icon).size(rems(topbar_action_height_rem())).tooltip(label)
}

/// A compact neutral outlined text button.
///
/// Consumers may add an icon, selected state, or contextual geometry without
/// duplicating the bordered treatment.
pub fn outlined_text_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    base_button(id).label(label).with_size(Size::Small)
}

/// Deleting something for good: the standard icon button wearing `danger`, an
/// icon where the actions beside it that keep things (Restore, Save) are words.
pub fn delete_icon_button(id: impl Into<ElementId>, label: impl Into<SharedString>, theme: BrowserTheme) -> Button {
    outlined_icon_button(id, label, IconName::Delete).text_color(theme.danger)
}

/// The way out of a dialog, at the size of the primary action it stands beside.
///
/// [`outlined_text_button`] is deliberately compact — it is a control inside
/// chrome, sized against toolbars — and a dialog's Cancel is not that button:
/// paired with a default-size primary, it reads as the lesser of two controls
/// that should be equals.
pub fn dialog_cancel_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    let label: SharedString = label.into();
    let tooltip = crate::shortcut_tooltip(label.clone(), "escape");
    base_button(id).label(label).tooltip(tooltip)
}

pub fn primary_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: impl Into<Icon>, disabled: bool) -> Button {
    secondary_action_button(id, label, icon, disabled).primary()
}

pub fn secondary_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: impl Into<Icon>, disabled: bool) -> Button {
    base_button(id).label(label).icon(icon).h(rems(topbar_action_height_rem())).px(px(SPACE_MD)).disabled(disabled)
}

/// A text action that carries no frame of its own, for the way out of a form
/// rather than the way through it. It has no icon: an icon would give it the
/// weight the shape exists to avoid.
pub fn link_button(id: impl Into<ElementId>, label: impl Into<SharedString>, disabled: bool) -> Button {
    base_button(id).label(label).ghost().with_size(Size::Small).px(px(0.0)).text_size(rems(TEXT_SM)).disabled(disabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Hsla, Render, TestAppContext, Window, canvas, div, rgb};
    use std::{cell::Cell, rc::Rc};

    struct DisabledButtonTest {
        disabled_ink: Rc<Cell<Option<Hsla>>>,
        enabled_ink: Rc<Cell<Option<Hsla>>>,
        clicks: Rc<Cell<usize>>,
    }

    impl Render for DisabledButtonTest {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().flex().children([true, false].map(|disabled| {
                let ink = if disabled { self.disabled_ink.clone() } else { self.enabled_ink.clone() };
                let clicks = self.clicks.clone();
                let id = if disabled { "disabled-custom-button" } else { "enabled-custom-button" };
                base_button(id)
                    .ghost()
                    .size(px(40.0))
                    .text_color(rgb(0xcc0000))
                    .bg(rgb(0x00cc00))
                    .disabled(disabled)
                    .debug_selector(move || id.to_owned())
                    .child(canvas(move |_, window, _| ink.set(Some(window.text_style().color)), |_, _, _, _| {}).size(px(16.0)))
                    .on_click(move |_, _, _| clicks.set(clicks.get() + 1))
            }))
        }
    }

    #[gpui::test]
    fn disabled_buttons_override_custom_ink_and_do_not_activate(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let disabled_ink = Rc::new(Cell::new(None));
        let enabled_ink = Rc::new(Cell::new(None));
        let clicks = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let disabled_ink = disabled_ink.clone();
            let enabled_ink = enabled_ink.clone();
            let clicks = clicks.clone();
            move |_, _| DisabledButtonTest { disabled_ink, enabled_ink, clicks }
        });
        cx.run_until_parked();
        let disabled_color = cx.update(|window, cx| {
            let _ = window.draw(cx);
            crate::browser_theme(cx).text_muted.opacity(0.5)
        });
        assert_eq!(disabled_ink.get(), Some(disabled_color), "caller colors must not override the disabled foreground");
        assert_eq!(enabled_ink.get(), Some(rgb(0xcc0000).into()), "enabled controls keep their custom colors");
        let disabled = cx.debug_bounds("disabled-custom-button").unwrap();
        cx.simulate_click(disabled.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(clicks.get(), 0);
        let enabled = cx.debug_bounds("enabled-custom-button").unwrap();
        cx.simulate_click(enabled.center(), Default::default());
        cx.run_until_parked();
        assert_eq!(clicks.get(), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod password_tests {
        use gpui::{AppContext, Context, Entity, Focusable, IntoElement, KeyLocation, KeyUpEvent, Keystroke, Render, TestAppContext, VisualTestContext, Window};
        use gpui_component::input::{Input, InputState};

        struct PasswordFixture {
            input: Entity<InputState>,
        }

        impl Render for PasswordFixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                Input::new(&self.input).mask_toggle()
            }
        }

        fn draw(cx: &mut VisualTestContext) {
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        fn press(cx: &mut VisualTestContext, key: &str) {
            cx.simulate_keystrokes(key);
            cx.simulate_event(KeyUpEvent { keystroke: Keystroke::parse(key).unwrap(), key_location: KeyLocation::Standard });
            draw(cx);
        }

        #[gpui::test]
        fn password_reveal_is_keyboard_accessible_persistent_and_tracks_mask_resets(cx: &mut TestAppContext) {
            cx.update(gpui_component::init);
            let (root, cx) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    let input = cx.new(|cx| InputState::new(window, cx).masked(true));
                    input.update(cx, |input, cx| input.set_value("example password", window, cx));
                    input.read(cx).focus_handle(cx).focus(window, cx);
                    PasswordFixture { input }
                });
                gpui_component::Root::new(view, window, cx)
            });
            let input = cx.update(|_, cx| {
                let view = root.read(cx).view().clone().downcast::<PasswordFixture>().ok().unwrap();
                view.read(cx).input.clone()
            });
            draw(cx);
            press(cx, "tab");
            press(cx, "enter");
            assert!(!input.read_with(cx, |input, _| input.is_masked()), "revealing survives key release and redraw");
            press(cx, "enter");
            assert!(input.read_with(cx, |input, _| input.is_masked()));
            press(cx, "enter");
            assert!(!input.read_with(cx, |input, _| input.is_masked()));
            // Account-flow completion re-masks the field without a window callback.
            input.update(cx, |input, cx| input.set_masked(true, cx));
            draw(cx);
            press(cx, "enter");
            input.read_with(cx, |input, _| {
                assert!(!input.is_masked(), "the button follows the input's actual mask after a reset");
                assert_eq!(input.value().as_str(), "example password");
            });
        }
    }
}
