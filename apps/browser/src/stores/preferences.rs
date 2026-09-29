//! Universal preference state.
//!
//! `backend` stores browsing preferences per account, not per library — the
//! getter takes no library id, and browsing changes enqueue to the signed-in
//! account. So this store lives at the root and is injected downward. Settings
//! edits it, and appearance refreshes all windows.

use app_preferences::{BrowsingPreferencePatch, BrowsingPreferences, CoverText};
use gpui::{App, Context, SharedString};
use ui_components::theme::apply_appearance;

use crate::services::AppServices;

/// The live "cover text" setting, read by every book card and by the pages
/// that size the row a card sits in.
///
/// A global rather than a value threaded down through the navigation stack
/// like `detailed`: `detailed` only ever reaches browse pages, but this
/// setting has to reach shelves too, and a card and the page around it can
/// each pick it up on their own `cx.observe_global` instead of every layer
/// between here and them needing a method just to pass it on.
pub(crate) struct CoverTextSetting(pub(crate) CoverText);

impl gpui::Global for CoverTextSetting {}

pub(crate) fn current_cover_text(cx: &App) -> CoverText {
    cx.try_global::<CoverTextSetting>().map(|setting| setting.0).unwrap_or_default()
}

pub(crate) struct Preferences {
    services: AppServices,
}

impl Preferences {
    pub(crate) fn new(services: AppServices, cx: &mut App) -> Self {
        let browsing = services.browsing_preferences.borrow().clone();
        apply_appearance(&browsing, cx);
        cx.set_global(CoverTextSetting(browsing.cover_text()));
        Self { services }
    }

    pub(crate) fn browsing(&self) -> BrowsingPreferences {
        self.services.browsing_preferences.borrow().clone()
    }

    pub(crate) fn interface_font_name(&self) -> SharedString {
        SharedString::from(app_preferences::INTERFACE_FONT_FAMILY)
    }

    pub(crate) fn ui_font_size_px(&self) -> u8 {
        self.services.browsing_preferences.borrow().ui_font_size_px()
    }

    /// Applies a browsing patch. Returns the error text on failure so the
    /// Settings page can surface it without this store knowing about pages.
    pub(crate) fn update_browsing(&mut self, patch: BrowsingPreferencePatch, cx: &mut Context<Self>) -> Result<(), String> {
        let previous = self.browsing();
        let mut browsing = previous.clone();
        patch.clone().apply(&mut browsing).map_err(|error| error.to_string())?;
        let appearance_changed = previous.theme() != browsing.theme() || previous.appearance_override() != browsing.appearance_override() || previous.ui_font_size_px() != browsing.ui_font_size_px();
        *self.services.browsing_preferences.borrow_mut() = browsing.clone();
        if appearance_changed {
            apply_appearance(&browsing, cx);
        }
        if previous.cover_text() != browsing.cover_text() {
            cx.set_global(CoverTextSetting(browsing.cover_text()));
        }
        let backend = self.services.backend.clone();
        cx.spawn(async move |_preferences, _cx| {
            if let Err(error) = backend.update_browsing_preference(patch).await {
                log::error!("failed to save browsing preference: {error}");
            }
        })
        .detach();
        cx.notify();
        Ok(())
    }
}
