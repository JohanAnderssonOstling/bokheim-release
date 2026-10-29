//! Provider login and the folder picker are omitted without the opt-in feature.

use super::Sidebar;
use gpui::{AnyElement, App, Context, Entity, Window};

pub(super) struct CloudLibraryDialog;

impl Sidebar {
    pub(super) fn close_cloud_library(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    pub(super) fn render_cloud_library(&self, _emitter: Entity<Self>, _window: &Window, _cx: &App) -> Option<AnyElement> {
        None
    }
}
