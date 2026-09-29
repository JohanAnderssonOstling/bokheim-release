//! One place to report a failed action to the user.

use gpui::{App, SharedString, Window};
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::notification::Notification;

/// Namespaces every browser notification. The ids below are unique across the
/// crate, so one namespace is enough to collapse repeats of the same failure.
struct BrowserNotification;

/// Reports a failed action. Failures stay on screen until dismissed.
pub(crate) fn notify_error(id: &'static str, message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::error(message.into()).id1::<BrowserNotification>(id).autohide(false), cx);
}

/// Reports an action that partly succeeded.
pub(crate) fn notify_warning(id: &'static str, message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::warning(message.into()).id1::<BrowserNotification>(id).autohide(false), cx);
}

/// Confirms an action that succeeded and left no trace on screen — an undo,
/// which puts things back where the reader was not necessarily looking. It
/// autohides: a confirmation the reader has to dismiss costs more than it says.
pub(crate) fn notify_info(id: &'static str, message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::info(message.into()).id1::<BrowserNotification>(id), cx);
}

/// Confirms a library change the reader can take back, with the Undo right
/// there. Undo is the same action Ctrl+Z dispatches, so it undoes the last
/// library action wherever it was taken. It still autohides.
pub(crate) fn notify_undoable(id: &'static str, message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    let notification = Notification::info(message.into())
        .id1::<BrowserNotification>(id)
        .action(|_, _, cx| {
            let notification = cx.entity();
            Button::new("undo").label("Undo").ghost().on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(crate::UndoLibraryAction), cx);
                notification.update(cx, |notification, cx| notification.dismiss(window, cx));
            })
        })
        // An action turns autohide off; a confirmation should still leave.
        .autohide(true);
    window.push_notification(notification, cx);
}
