//! The player dock along the bottom of a window, shared by audiobook playback
//! and reader narration.
//!
//! Wide docks keep the transport centred between what is playing on the left
//! and the remaining tools on the right. Compact narration docks use a flat
//! row with equal space between every control and at both ends.

use gpui::prelude::*;
use gpui::{App, DispatchPhase, Div, ElementId, HitboxBehavior, SharedString, TouchEvent, TouchPhase, Window, canvas, div, px, rems};
use crate::Button;

use crate::{BrowserTheme, FlexItem, SPACE_SM, SPACE_XS, SPACE_XXS};

/// Dock and cover size in rems, scaling with the interface font.
pub const PLAYER_DOCK_HEIGHT_REM: f32 = 5.0;
pub const PLAYER_DOCK_TEXT_SIZE_REM: f32 = 1.0;
pub const PLAYER_DOCK_CAPTION_SIZE_REM: f32 = 0.75;
/// The progress line is drawn thin but has to be easy to hit, so its hit area
/// is taller than the line inside it.
pub const PLAYER_DOCK_TRACK_HIT_HEIGHT_REM: f32 = 1.0;
const PROGRESS_LINE_HEIGHT: f32 = 3.0;

/// Attach inside a positioned scrub track. Only an unobscured track can begin
/// a touch scrub; its owner tracks the contact id through release/cancellation,
/// including when the finger moves outside the track.
pub fn playback_scrub_touch_listener(listener: impl Fn(&TouchEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            window.on_touch(move |event, phase, window, cx| {
                if phase == DispatchPhase::Capture && (event.phase != TouchPhase::Started || hitbox.is_under_pointer(window)) {
                    listener(event, window, cx);
                }
            });
        },
    )
    .absolute()
    .size_full()
}

/// Total dock height in pixels, including its fixed top border.
pub fn player_dock_height_px(window: &Window) -> f32 {
    f32::from(rems(PLAYER_DOCK_HEIGHT_REM).to_pixels(window.rem_size())) + 1.0
}

pub fn player_dock(leading: impl IntoElement, transport: impl IntoElement, trailing: impl IntoElement, theme: BrowserTheme) -> Div {
    crate::bottom_bar(theme).occlude().relative().child(
        crate::row(0.0)
            .gap(rems(SPACE_SM / 16.0))
            .fill()
            .h(rems(PLAYER_DOCK_HEIGHT_REM))
            .pr(rems(SPACE_SM / 16.0))
            .child(crate::row(0.0).gap(rems(SPACE_SM / 16.0)).fill().h_full().child(leading))
            .child(transport)
            .child(crate::row(0.0).gap(rems(SPACE_XS / 16.0)).fill().h_full().justify_end().child(trailing)),
    )
}

/// A compact dock with the same free space between each control and at both ends.
pub fn player_dock_evenly(controls: impl IntoIterator<Item = impl IntoElement>, theme: BrowserTheme) -> Div {
    crate::bottom_bar(theme).occlude().relative().child(crate::row(0.0).w_full().h(rems(PLAYER_DOCK_HEIGHT_REM)).justify_evenly().children(controls))
}

/// The transport buttons, with an optional caption over them.
pub fn player_dock_transport(caption: Option<SharedString>, buttons: impl IntoIterator<Item = impl IntoElement>, theme: BrowserTheme) -> Div {
    crate::column(0.0)
        .gap(rems(SPACE_XXS / 16.0))
        .flex_none()
        .items_center()
        .children(caption.map(|caption| crate::caption_line(caption, theme).text_size(rems(PLAYER_DOCK_CAPTION_SIZE_REM)).line_height_auto()))
        .child(crate::row(0.0).gap(rems(SPACE_XXS / 16.0)).children(buttons))
}

/// A row of dock tools.
pub fn player_dock_tools() -> Div {
    crate::row(0.0).gap(rems(SPACE_XS / 16.0))
}

/// A dock tool that shows a value instead of an icon (the speed, the sleep
/// timer's time left), as tall as the icon buttons beside it.
pub fn player_dock_text_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    // Keep the dock label independent of dropdown menu typography.
    let label = label.into();
    crate::playback_secondary_button(crate::base_button(id).ghost()).child(div().flex_none().text_size(rems(crate::TEXT_SM)).line_height_auto().child(label))
}

/// The chapter's progress as a thin line, for the top edge of a dock.
pub fn player_dock_progress_line(fraction: f32, theme: BrowserTheme) -> Div {
    crate::progress_track(theme).h(px(PROGRESS_LINE_HEIGHT)).child(crate::progress_fill(fraction, theme))
}

/// The progress line inside its taller hit area along the dock's top edge.
/// The caller makes it scrubbable.
pub fn player_dock_track(fraction: f32, theme: BrowserTheme) -> Div {
    crate::top_edge().h(rems(PLAYER_DOCK_TRACK_HIT_HEIGHT_REM)).child(player_dock_progress_line(fraction, theme))
}
