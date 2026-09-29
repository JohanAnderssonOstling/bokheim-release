use gpui::prelude::*;
use gpui::{ElementId, SharedString, px};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Disableable, Icon, IconName, Sizable};

use crate::{BrowserTheme, SPACE_MD, TEXT_SM, TOPBAR_ACTION_HEIGHT_REM};

/// Common entry point for interactive controls.
///
/// This is the shared base for all button-like controls, so pointer behavior
/// stays consistent even when call sites use different variants.
pub fn base_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer().hover(|style| style.bg(BrowserTheme::default().hover))
}

/// Square, with a light border distinct from the background.
///
/// `.secondary()` is deliberately absent: it filled the button with
/// `colors.secondary`, which the theme sets to the same value as the page
/// background, so the fill asserted a surface that was never visible. The
/// border is the whole control, and it sits on the quiet tier — `border` is
/// where hover travels to, not where the button rests.
pub(crate) fn outlined_button(id: impl Into<ElementId>, theme: BrowserTheme) -> Button {
    base_button(id).outline().border_color(theme.rule)
}

/// A borderless, 2rem square icon action on the page background, with a
/// 1rem icon and ink text rather than an accent — the standard size and
/// shape shared by every square icon button in the app (square corners, to
/// match every other native surface and control). Semantic wrappers own any
/// contextual state colour (hover, selection) on top of this baseline.
pub fn outlined_icon_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: impl Into<Icon>, theme: BrowserTheme) -> Button {
    base_button(id).ghost().text_color(theme.text).bg(theme.page_bg).icon(icon).size(gpui::rems(2.0)).tooltip(label)
}

/// A compact neutral outlined text button.
///
/// Consumers may add an icon, selected state, or contextual geometry without
/// duplicating the native secondary-outline treatment.
pub fn outlined_text_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    base_button(id).label(label).with_size(gpui_component::Size::Small).secondary().outline()
}

/// A compact primary action used inside lists, cards, and review panels.
pub fn compact_primary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    base_button(id).label(label).primary().small()
}

/// A compact secondary action used beside compact primary actions.
pub fn compact_secondary_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    base_button(id).label(label).secondary().outline().small()
}

/// The way out of a dialog, at the size of the primary action it stands beside.
///
/// [`outlined_text_button`] is deliberately compact — it is a control inside
/// chrome, sized against toolbars — and a dialog's Cancel is not that button:
/// paired with a default-size primary, it reads as the lesser of two controls
/// that should be equals.
pub fn dialog_cancel_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    base_button(id).label(label).secondary().outline()
}

pub fn primary_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: impl Into<gpui_component::Icon>, disabled: bool) -> Button {
    base_button(id).label(label).icon(icon).primary().h(gpui::rems(TOPBAR_ACTION_HEIGHT_REM)).px(px(SPACE_MD)).disabled(disabled)
}

pub fn secondary_action_button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon: IconName, disabled: bool) -> Button {
    base_button(id).label(label).icon(icon).secondary().outline().h(gpui::rems(TOPBAR_ACTION_HEIGHT_REM)).px(px(SPACE_MD)).disabled(disabled)
}

/// A text action that carries no frame of its own, for the way out of a form
/// rather than the way through it. It has no icon: an icon would give it the
/// weight the shape exists to avoid.
pub fn link_button(id: impl Into<ElementId>, label: impl Into<SharedString>, theme: BrowserTheme, disabled: bool) -> Button {
    base_button(id).label(label).ghost().with_size(gpui_component::Size::Small).px(px(0.0)).text_color(theme.text_accent).text_size(gpui::rems(TEXT_SM)).disabled(disabled)
}
