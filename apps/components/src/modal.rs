use gpui::prelude::*;
use gpui::{Anchored, App, Div, ElementId, FontWeight, KeyDownEvent, MouseButton, SharedString, Stateful, Window, div, px, rems};

use gpui_component::menu::{ChoiceRowColors, choice_row, choice_row_checkmark};
use gpui_component::{Icon, IconName, tooltip::Tooltip};

use crate::{BrowserTheme, RADIUS_MD, SPACE_MD, SPACE_SM, SPACE_XXS, TEXT_MD, TEXT_XS};

/// The shared full-window layer for a centered modal. The caller supplies the
/// scrim and surface so it can retain its own identity and dismissal behavior.
///
/// On its own it fills its parent, so callers pin it to the window with
/// [`anchor_to_window`].
pub fn modal_overlay() -> Div {
    crate::absolute_full_size().flex().items_center().justify_center().p(px(crate::SPACE_MD)).on_key_down(contain_modal_key)
}

/// Child controls handle bindings first. Contain remaining non-text keys so
/// navigation cannot reach the page underneath. Printable input must continue
/// to the platform input handler; Escape keeps the caller's dismissal path.
fn contain_modal_key(event: &KeyDownEvent, _: &mut Window, cx: &mut App) {
    if event.keystroke.key != "escape" && event.keystroke.key_char.as_deref().is_none_or(|text| text.chars().all(char::is_control)) {
        cx.stop_propagation();
    }
}

/// Pins a modal layer to the window's origin at the window's size, so the
/// popup it centers is centered in the window wherever in the tree it is
/// rendered. Left to fill its parent, a dialog opened from one pane of a split
/// browse view, or from the sidebar, centered on that pane and dimmed only it.
///
/// On Kobo the layer ends above the virtual keyboard, as the application
/// content does, so a dialog's text field is never under the keys.
pub fn anchor_to_window(overlay: Div, window: &Window) -> Anchored {
    let viewport = window.viewport_size();
    #[cfg(feature = "kobo")]
    let viewport = gpui::size(viewport.width, viewport.height - gpui_kobo::keyboard_height(window));
    let insets = window.insets().effective();
    gpui::anchored().position(gpui::point(insets.left, insets.top)).child(overlay.w((viewport.width - insets.left - insets.right).max(px(0.0))).h((viewport.height - insets.top - insets.bottom).max(px(0.0))))
}

/// A centered modal anchored to the window. Callers choose when it paints.
pub fn centered_modal(scrim: impl IntoElement, content: impl IntoElement, window: &Window) -> Anchored {
    anchor_to_window(modal_overlay().occlude().child(scrim).child(content), window)
}

/// A modal's pointer-blocking backdrop.
pub fn modal_scrim(id: impl Into<ElementId>, theme: BrowserTheme) -> Stateful<Div> {
    crate::absolute_full_size().id(id).occlude().bg(theme.overlay_scrim).cursor_default()
}

/// The common elevated surface for dialogs, editors, and other modal content.
pub fn modal_surface(theme: BrowserTheme) -> Div {
    div().relative().min_h_0().min_w_0().occlude().rounded(px(RADIUS_MD)).border_1().border_color(theme.rule).bg(theme.page_bg).shadow(crate::overlay_shadow(theme))
}

/// A form dialog's padded column, containing pointer presses within its surface.
/// Callers supply its width and controls; compact forms can override the gap.
pub fn modal_form_surface(theme: BrowserTheme) -> Div {
    modal_surface(theme).max_w_full().p(px(SPACE_MD)).flex().flex_col().gap(px(SPACE_MD)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// Font selection uses the same centered surface on the reader and browse
/// settings. Its list fills the available height instead of sizing to results.
pub fn font_picker_surface(window: &Window, theme: BrowserTheme) -> Div {
    let viewport = window.viewport_size();
    let insets = window.insets().effective();
    let width = (f32::from(viewport.width - insets.left - insets.right) - 2.0 * SPACE_MD).clamp(1.0, 640.0);
    let height = ((f32::from(viewport.height - insets.top - insets.bottom) - 2.0 * SPACE_MD) * 0.9).clamp(1.0, 720.0);
    modal_surface(theme).w(px(width)).h(px(height)).p(px(SPACE_MD)).flex().flex_col().gap(px(SPACE_SM)).overflow_hidden()
}

/// A font sample is a choice row, with the name set in its own family.
/// Shared by application and reader font pickers so selection and hover agree.
pub fn font_picker_row(id: impl Into<ElementId>, family: impl Into<SharedString>, selected: bool, theme: BrowserTheme) -> Stateful<Div> {
    let family = family.into();
    let sample = div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(family.clone()).text_size(rems(TEXT_MD)).line_height(gpui::relative(1.4)).child(family);
    bottom_sheet_row(id, sample, selected, theme).w_full().flex_none().min_h(rems(crate::settings_control_size_rem()))
}

/// A viewport-centered font selector with backdrop and Escape dismissal.
pub fn font_picker_overlay(content: impl IntoElement, window: &Window, theme: BrowserTheme, on_dismiss: impl Fn(&mut Window, &mut App) + 'static) -> gpui::Deferred {
    let dismiss = std::rc::Rc::new(on_dismiss);
    let scrim_dismiss = dismiss.clone();
    gpui::deferred(anchor_to_window(
        modal_overlay()
            .occlude()
            .on_key_down(move |event, window, cx| {
                if event.keystroke.key == "escape" {
                    dismiss(window, cx);
                    cx.stop_propagation();
                }
            })
            .child(modal_scrim("font-picker-scrim", theme).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, window, cx| {
                scrim_dismiss(window, cx);
                cx.stop_propagation();
            }))
            .child(content),
        window,
    ))
    .with_priority(3)
}

/// The widest a confirmation dialog grows.
const CONFIRMATION_DIALOG_MAX_WIDTH: f32 = 448.0;

/// A confirmation, sized the same wherever one is asked: up to
/// [`CONFIRMATION_DIALOG_MAX_WIDTH`], and never closer to the window's edges
/// than a modal's gutter.
///
/// It is gpui-component's in-window dialog rather than the platform's: that
/// one is drawn by the system in the system's colours, while this one is
/// dressed from the palette like every other widget, scrim included.
pub fn confirmation_dialog(dialog: gpui_component::dialog::AlertDialog, window: &gpui::Window) -> gpui_component::dialog::AlertDialog {
    dialog.width(px((f32::from(window.viewport_size().width) - 2.0 * SPACE_MD).clamp(1.0, CONFIRMATION_DIALOG_MAX_WIDTH)))
}

/// A confirmation that is a single line: the question, then its two answers.
///
/// Passed to the dialog as its footer, with nothing above it. The dialog's own
/// minimum height and the gap it keeps between a header and a footer would
/// otherwise leave an empty band over the row, so both are cleared.
pub fn confirmation_dialog_row(
    dialog: gpui_component::dialog::AlertDialog, question: impl Into<SharedString>, cancel: crate::Button, confirm: crate::Button, theme: BrowserTheme,
) -> gpui_component::dialog::AlertDialog {
    dialog.min_h_0().gap_0().footer(
        div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(SPACE_SM))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(rems(TEXT_MD)).font_weight(FontWeight::SEMIBOLD).text_color(theme.text).child(question.into()))
            .child(cancel)
            .child(confirm),
    )
}

/// The shared full-window layer for a bottom sheet: like `modal_overlay`, but
/// the surface it wraps sits flush against the bottom edge instead of
/// floating centered. The caller supplies the scrim and surface, same as
/// `modal_overlay`, so it keeps its own identity and dismissal behavior.
pub fn bottom_sheet_overlay() -> Div {
    crate::absolute_full_size().flex().flex_col().justify_end().on_key_down(contain_modal_key)
}

/// The sheet's own surface: full width, capped height with its own scroll,
/// and a click-swallowing guard so a tap inside doesn't fall through to the
/// scrim below it and dismiss the sheet it landed on.
pub fn bottom_sheet_surface(theme: BrowserTheme) -> Div {
    div().w_full().flex_none().flex().flex_col().max_h(gpui::relative(0.8)).occlude().bg(theme.page_bg).border_t_1().border_color(theme.border).shadow(crate::overlay_shadow(theme)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A bottom sheet's title row. Callers wanting a trailing close button (or
/// nothing at all) append it with `.child(...)`, since `justify_between`
/// leaves room for one without requiring every sheet to have one.
pub fn bottom_sheet_header(title: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px(px(SPACE_MD))
        .py(px(SPACE_SM + 4.0))
        .border_b_1()
        .border_color(theme.rule)
        .child(div().text_size(rems(TEXT_MD)).font_weight(FontWeight::SEMIBOLD).text_color(theme.text).child(title.into()))
}

/// A bottom sheet's close button, for the end of `bottom_sheet_header`: the
/// muted glyph, centred in a touch target on a phone.
pub fn bottom_sheet_close_button(id: impl Into<ElementId>, theme: BrowserTheme) -> Stateful<Div> {
    let tooltip = crate::shortcut_tooltip("Close", "escape");
    div()
        .id(id)
        .flex_none()
        .cursor_pointer()
        .text_color(theme.text_muted)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .when(crate::touch_ui(), |button| button.size(rems(crate::TOUCH_TARGET_REM)).flex().items_center().justify_center())
        .child(Icon::new(IconName::Close).size(px(16.0)))
}

/// A small muted heading over a group of sheet rows.
pub fn bottom_sheet_section(label: impl Into<SharedString>, theme: BrowserTheme) -> Div {
    div().flex_none().min_w_0().px(px(SPACE_SM)).pt(px(SPACE_SM)).pb(px(SPACE_XXS)).text_size(rems(TEXT_XS)).font_weight(FontWeight::SEMIBOLD).text_color(theme.text_muted).child(label.into())
}

pub fn bottom_sheet_divider(theme: BrowserTheme) -> Div {
    div().flex_none().h(px(1.0)).my(px(SPACE_XXS)).bg(theme.rule)
}

/// One full-width, touch-sized choice in a bottom sheet; `checked` marks the
/// current one.
pub fn bottom_sheet_row(id: impl Into<ElementId>, label: impl IntoElement, checked: bool, theme: BrowserTheme) -> Stateful<Div> {
    let colors = theme.into();
    choice_row(checked, false, false, colors)
        .id(id)
        .flex_none()
        .min_w_0()
        .cursor_pointer()
        .min_h(px(48.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(SPACE_SM))
        .px(px(SPACE_MD))
        .py(px(SPACE_SM))
        .text_size(rems(TEXT_MD))
        .child(div().flex_1().min_w_0().whitespace_normal().child(label))
        .when(checked, |row| row.child(choice_row_checkmark(colors)))
}

/// Descriptive text follows the same selected foreground as the row label.
pub fn bottom_sheet_row_caption(label: impl Into<SharedString>, checked: bool, theme: BrowserTheme) -> Div {
    crate::caption_line(label, theme).when(checked, |caption| caption.text_color(ChoiceRowColors::from(theme).selected_foreground))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Entity, Focusable, Render, Subscription, TestAppContext};
    use gpui_component::input::{Input, InputEvent, InputState};

    struct ModalTest {
        input: Entity<InputState>,
        sheet: bool,
        submitted: usize,
        background_keys: Vec<String>,
        _subscription: Subscription,
    }

    impl Render for ModalTest {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let input = Input::new(&self.input);
            let overlay = if self.sheet {
                bottom_sheet_overlay().child(input).into_any_element()
            } else {
                let theme = crate::browser_theme(cx);
                centered_modal(modal_scrim("test-scrim", theme), modal_form_surface(theme).child(input), window).into_any_element()
            };
            div().size_full().on_key_down(cx.listener(|view, event: &KeyDownEvent, _, _| view.background_keys.push(event.keystroke.key.clone()))).child(overlay)
        }
    }

    #[gpui::test]
    fn modal_and_sheet_keep_navigation_keys_without_breaking_typing_or_escape(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for sheet in [false, true] {
            let (root, cx) = cx.add_window_view(|window, cx| {
                let view = cx.new(|cx| {
                    let input = cx.new(|cx| InputState::new(window, cx));
                    input.read(cx).focus_handle(cx).focus(window, cx);
                    let subscription = cx.subscribe(&input, |view: &mut ModalTest, _, event, _| {
                        if matches!(event, InputEvent::PressEnter { .. }) {
                            view.submitted += 1;
                        }
                    });
                    ModalTest { input, sheet, submitted: 0, background_keys: Vec::new(), _subscription: subscription }
                });
                gpui_component::Root::new(view, window, cx)
            });
            let view = cx.update(|_, cx| root.read(cx).view().clone().downcast::<ModalTest>().ok().unwrap());
            cx.simulate_keystrokes("a b c enter pagedown");
            view.read_with(cx, |view, cx| {
                assert_eq!(view.input.read(cx).value().as_str(), "abc");
                assert_eq!(view.submitted, 1);
                assert!(!view.background_keys.iter().any(|key| matches!(key.as_str(), "enter" | "pagedown")));
            });
            cx.simulate_keystrokes("escape");
            view.read_with(cx, |view, _| assert_eq!(view.background_keys.last().map(String::as_str), Some("escape")));
        }
    }
}
