//! Cached pages shared by every library selection.

use gpui::{AnyView, AppContext, Context, Entity};

use super::{BrowserShell, GlobalRoute, Libraries};
use crate::stores::Preferences;
use crate::universal::settings::SettingsPage;
use crate::universal::sync::SyncPage;

/// Owns the dependencies its pages are built from, so the shell only has to say
/// which page it wants.
pub(super) struct GlobalPages {
    updates: Option<Entity<crate::services::UpdateControls>>,
    preferences: Entity<Preferences>,
    libraries: Entity<Libraries>,
    sync_page: Entity<SyncPage>,
    settings: Option<Entity<SettingsPage>>,
}

impl GlobalPages {
    #[cfg(target_arch = "wasm32")]
    pub(super) fn apply_sync_event(&self, event: crate::SyncEvent, cx: &mut Context<BrowserShell>) {
        self.sync_page.update(cx, |page, cx| page.apply_event(event, cx));
    }
    pub(super) fn new(updates: Option<Entity<crate::services::UpdateControls>>, preferences: Entity<Preferences>, libraries: Entity<Libraries>, sync_page: Entity<SyncPage>) -> Self {
        Self { updates, preferences, libraries, sync_page, settings: None }
    }

    /// Returns the page for `route`, building it on first use.
    pub(super) fn page(&mut self, route: GlobalRoute, cx: &mut Context<BrowserShell>) -> AnyView {
        match route {
            GlobalRoute::Settings => self.settings_page(cx).into(),
        }
    }

    /// Creates the page before the shell renders it. Page creation is kept out
    /// of render and performs no focus or other window operations.
    pub(super) fn prepare(&mut self, route: GlobalRoute, cx: &mut Context<BrowserShell>) {
        match route {
            GlobalRoute::Settings => {
                drop(self.settings_page(cx));
                // The shared state was created at app startup. Refresh when
                // entering Settings so completed imports update local usage.
                self.sync_page.update(cx, |page, cx| page.schedule_refresh(cx));
            }
        }
    }

    fn settings_page(&mut self, cx: &mut Context<BrowserShell>) -> Entity<SettingsPage> {
        if let Some(page) = &self.settings {
            return page.clone();
        }
        let page = cx.new(|cx| SettingsPage::new(self.updates.clone(), self.sync_page.clone(), self.preferences.clone(), self.libraries.clone(), cx));
        self.settings = Some(page.clone());
        page
    }
}
