//! UI binding for the shared backend update state. The host supplies actions
//! only after its preparation and startup/installer lifecycle are available.
use gpui::{App, Context};
use std::rc::Rc;
pub use update_client::coordinator::{UpdateAction, UpdateView};

pub type UpdateActionHandler = Rc<dyn Fn(UpdateAction, &mut App)>;

pub struct UpdateControls {
    pub(crate) view: UpdateView,
    pub(crate) action: UpdateActionHandler,
}

impl UpdateControls {
    pub fn new(view: UpdateView, action: UpdateActionHandler) -> Self {
        Self { view, action }
    }

    /// Backend notifications must enter through the platform's normal UI event
    /// bridge. In particular a browser worker must not re-enter a GPUI render.
    pub fn apply(&mut self, view: UpdateView, cx: &mut Context<Self>) {
        if self.view != view {
            self.view = view;
            cx.notify();
        }
    }
}

pub(crate) fn needed_space(bytes: u64) -> String {
    // Round up by at most one decimal MB; never exaggerate a 1.1 GB deficit to 2 GB.
    if bytes >= 1_000_000 { format!("{} MB", bytes.div_ceil(1_000_000)) } else { format!("{} KB", bytes.div_ceil(1_000)) }
}
