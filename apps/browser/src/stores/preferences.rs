//! Universal preference state.
//!
//! The backend stores browsing preferences in local application state, shared
//! across libraries and windows. One application store owns the live value.
//! Settings edits it, and appearance refreshes all windows.

use app_preferences::{BrowsingPreferencePatch, BrowsingPreferences, CatalogGrouping, CoverText};
use gpui::{App, AppContext, Context, Entity, SharedString, Task};
use std::rc::Rc;
use ui_components::theme::apply_appearance;

use crate::services::AppServices;

/// The live "cover text" setting, read by every book card and by the pages
/// that size the row a card sits in.
///
/// A global rather than a value threaded down through the navigation stack:
/// it has to reach browse pages and shelves alike, and a card and the page
/// around it can each pick it up on their own `cx.observe_global` instead of
/// every layer between here and them needing a method just to pass it on.
pub(crate) struct CoverTextSetting(pub(crate) CoverText);

impl gpui::Global for CoverTextSetting {}

pub(crate) fn current_cover_text(cx: &App) -> CoverText {
    cx.try_global::<CoverTextSetting>().map(|setting| setting.0).unwrap_or_default()
}

/// The live "generated covers only" setting, a global for the same reason as
/// [`CoverTextSetting`]: every card and detail page reads it.
pub(crate) struct GeneratedCoversOnly(pub(crate) bool);

impl gpui::Global for GeneratedCoversOnly {}

pub(crate) fn generated_covers_only(cx: &App) -> bool {
    cx.try_global::<GeneratedCoversOnly>().is_some_and(|setting| setting.0)
}

/// The store itself, for a page that saves a choice of its own — the
/// catalog's grouping — without every layer above it passing the store on.
struct PreferencesStore(Entity<Preferences>);

impl gpui::Global for PreferencesStore {}

/// The catalog's grouping as last chosen.
pub(crate) fn current_catalog_grouping(cx: &App) -> CatalogGrouping {
    cx.try_global::<PreferencesStore>().map(|store| store.0.read(cx).browsing().catalog_grouping()).unwrap_or_default()
}

/// Saves the catalog's grouping, as Settings saves its choices.
pub(crate) fn save_catalog_grouping(grouping: CatalogGrouping, cx: &mut App) {
    let Some(store) = cx.try_global::<PreferencesStore>().map(|store| store.0.clone()) else { return };
    store.update(cx, |preferences, cx| {
        if let Err(error) = preferences.update_browsing(BrowsingPreferencePatch::CatalogGrouping(grouping), cx) {
            log::error!("failed to save the catalog grouping: {error}");
        }
    });
}

pub(crate) fn current_catalog_languages(cx: &App) -> Option<Vec<String>> {
    cx.try_global::<PreferencesStore>().and_then(|store| store.0.read(cx).browsing().catalog_languages().map(<[String]>::to_vec))
}

pub(crate) fn save_catalog_languages(languages: Option<Vec<String>>, cx: &mut App) {
    let Some(store) = cx.try_global::<PreferencesStore>().map(|store| store.0.clone()) else { return };
    store.update(cx, |preferences, cx| {
        if let Err(error) = preferences.update_browsing(BrowsingPreferencePatch::CatalogLanguages(languages), cx) {
            log::error!("failed to save catalog languages: {error}");
        }
    });
}

pub(crate) struct Preferences {
    backend: Rc<app::AppClient>,
    browsing: BrowsingPreferences,
}

impl Preferences {
    /// Initialized once for the application; every window observes this owner.
    pub(crate) fn shared(services: &AppServices, cx: &mut App) -> Entity<Self> {
        if let Some(store) = cx.try_global::<PreferencesStore>() {
            return store.0.clone();
        }
        if let Err(error) = ui_components::register_fonts(cx) {
            log::warn!("failed to install interface fonts: {error}");
        }
        let browsing = services.startup.browsing_preferences.clone();
        ui_components::apply_keyboard_shortcuts(browsing.keyboard_shortcuts(), cx);
        apply_appearance(&browsing, cx);
        cx.set_global(CoverTextSetting(browsing.cover_text()));
        cx.set_global(GeneratedCoversOnly(browsing.generated_covers_only()));
        let backend = services.backend.clone();
        #[cfg(feature = "kobo")]
        cx.set_global(gpui_kobo::KoboKeyboardLayouts(browsing.keyboard_layouts().to_vec()));
        let store = cx.new(|cx| {
            #[cfg(target_os = "android")]
            cx.observe_global::<ui_components::SystemFontFamilies>(|preferences: &mut Self, cx| {
                apply_appearance(&preferences.browsing, cx);
                cx.notify();
            })
            .detach();
            #[cfg(not(target_os = "android"))]
            let _ = cx;
            Self { backend, browsing }
        });
        cx.set_global(PreferencesStore(store.clone()));
        let shortcut_preferences = store.clone();
        ui_components::configure_shortcut_saving(
            move |shortcuts, cx| {
                let saved = shortcut_preferences.update(cx, |preferences, cx| preferences.save_keyboard_shortcuts(shortcuts, cx));
                cx.spawn(async move |_| saved.await.map_err(|error| Box::new(error) as ui_components::ShortcutSaveError))
            },
            cx,
        );
        // The keyboard's layout chooser updates the global; save its selection.
        #[cfg(feature = "kobo")]
        cx.observe_global::<gpui_kobo::KoboKeyboardLayouts>(|cx| {
            let layouts = cx.global::<gpui_kobo::KoboKeyboardLayouts>().0.clone();
            let Some(store) = cx.try_global::<PreferencesStore>().map(|store| store.0.clone()) else { return };
            if store.read(cx).browsing.keyboard_layouts() == layouts.as_slice() {
                return;
            }
            store.update(cx, |preferences, cx| {
                if let Err(error) = preferences.update_browsing(BrowsingPreferencePatch::KeyboardLayouts(layouts), cx) {
                    log::error!("failed to save keyboard layouts: {error}");
                }
            });
        })
        .detach();
        store
    }

    pub(crate) fn browsing(&self) -> BrowsingPreferences {
        self.browsing.clone()
    }

    pub(crate) fn interface_font_name(&self, cx: &App) -> SharedString {
        SharedString::from(ui_components::configured_ui_font_name(&self.browsing, cx).to_owned())
    }

    pub(crate) fn ui_font_size_px(&self) -> u8 {
        self.browsing.ui_font_size_px()
    }

    pub(crate) fn save_keyboard_shortcuts(&mut self, shortcuts: app_preferences::KeyboardShortcuts, cx: &mut Context<Self>) -> Task<Result<(), app::BackendError>> {
        let backend = self.backend.clone();
        cx.spawn(async move |preferences, cx| {
            let saved = backend.update_browsing_preference(BrowsingPreferencePatch::KeyboardShortcuts(shortcuts)).await?;
            preferences
                .update(cx, |preferences, cx| {
                    let shortcuts = saved.keyboard_shortcuts().clone();
                    ui_components::apply_keyboard_shortcuts(&shortcuts, cx);
                    let _ = BrowsingPreferencePatch::KeyboardShortcuts(shortcuts).apply(&mut preferences.browsing);
                    cx.notify();
                })
                .map_err(|error| app::BackendError::message(error.to_string()))
        })
    }

    /// Commit this backend-enforced setting only after acknowledgement. The
    /// caller owns any window-specific presentation of a failed save.
    pub(crate) fn save_metered_transfers(&mut self, enabled: bool, cx: &mut Context<Self>) -> Task<Result<(), app::BackendError>> {
        let backend = self.backend.clone();
        cx.spawn(async move |preferences, cx| {
            let saved = backend.update_browsing_preference(BrowsingPreferencePatch::AllowMeteredTransfers(enabled)).await?;
            preferences
                .update(cx, |preferences, cx| {
                    let _ = BrowsingPreferencePatch::AllowMeteredTransfers(saved.allow_metered_transfers()).apply(&mut preferences.browsing);
                    cx.notify();
                })
                .map_err(|error| app::BackendError::message(error.to_string()))
        })
    }

    /// Applies a browsing patch. Returns the error text on failure so the
    /// Settings page can surface it without this store knowing about pages.
    pub(crate) fn update_browsing(&mut self, patch: BrowsingPreferencePatch, cx: &mut Context<Self>) -> Result<(), String> {
        if let BrowsingPreferencePatch::AllowMeteredTransfers(enabled) = patch {
            let saved = self.save_metered_transfers(enabled, cx);
            cx.spawn(async move |_, _| {
                if let Err(error) = saved.await {
                    log::error!("failed to save the transfer setting: {error}");
                }
            })
            .detach();
            return Ok(());
        }
        let previous = self.browsing();
        let mut browsing = previous.clone();
        patch.clone().apply(&mut browsing).map_err(|error| error.to_string())?;
        let appearance_changed = previous.theme() != browsing.theme()
            || previous.appearance_override() != browsing.appearance_override()
            || previous.ui_font_size_px() != browsing.ui_font_size_px()
            || previous.ui_font_family() != browsing.ui_font_family()
            || previous.custom_ui_font_active() != browsing.custom_ui_font_active()
            || previous.custom_ui_font_family() != browsing.custom_ui_font_family();
        self.browsing = browsing.clone();
        if previous.keyboard_shortcuts() != browsing.keyboard_shortcuts() {
            ui_components::apply_keyboard_shortcuts(browsing.keyboard_shortcuts(), cx);
        }
        if appearance_changed {
            apply_appearance(&browsing, cx);
        }
        if previous.cover_text() != browsing.cover_text() {
            cx.set_global(CoverTextSetting(browsing.cover_text()));
        }
        if previous.generated_covers_only() != browsing.generated_covers_only() {
            cx.set_global(GeneratedCoversOnly(browsing.generated_covers_only()));
        }
        #[cfg(feature = "kobo")]
        if cx.global::<gpui_kobo::KoboKeyboardLayouts>().0.as_slice() != browsing.keyboard_layouts() {
            cx.set_global(gpui_kobo::KoboKeyboardLayouts(browsing.keyboard_layouts().to_vec()));
        }
        let backend = self.backend.clone();
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
