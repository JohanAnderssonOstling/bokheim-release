use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const WIFI_OFF_ICON_PATH: &str = "icons/wifi-off.svg";
const WIFI_OFF_ICON: &[u8] = include_bytes!("../../ui/design-tokens/assets/icon/wifi-off.svg");

const WIFI_ICON_PATH: &str = "icons/wifi.svg";
const WIFI_ICON: &[u8] = include_bytes!("../../ui/design-tokens/assets/icon/wifi.svg");

const APP_ICON_PATH: &str = "icons/bokheim.svg";
const APP_ICON: &[u8] = include_bytes!("../../ui/design-tokens/assets/icon/bokheim.svg");
const PHOSPHOR_LIGHT_ICONS: [(&str, &[u8]); 63] = [
    ("icons/alarm.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/alarm.svg")),
    ("icons/arrow-down.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/arrow-down.svg")),
    ("icons/arrow-left.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/arrow-left.svg")),
    ("icons/arrow-right.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/arrow-right.svg")),
    ("icons/book-open.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/book-open.svg")),
    ("icons/battery-charging.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/battery-charging.svg")),
    ("icons/battery-full.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/battery-full.svg")),
    ("icons/battery-low.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/battery-low.svg")),
    ("icons/battery-medium.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/battery-medium.svg")),
    ("icons/battery-warning.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/battery-warning.svg")),
    ("icons/bluetooth.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/bluetooth.svg")),
    ("icons/bluetooth-off.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/bluetooth-off.svg")),
    ("icons/check.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/check.svg")),
    ("icons/cloud-download.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/cloud-download.svg")),
    ("icons/copy.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/copy.svg")),
    ("icons/chevron-down.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/caret-down.svg")),
    ("icons/chevron-left.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/caret-left.svg")),
    ("icons/chevron-right.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/caret-right.svg")),
    ("icons/chevron-up.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/caret-up.svg")),
    ("icons/circle-x.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/x-circle.svg")),
    ("icons/close.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/x.svg")),
    ("icons/delete.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/trash.svg")),
    ("icons/ellipsis.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/dots-three.svg")),
    ("icons/ellipsis-vertical.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/dots-three-vertical.svg")),
    ("icons/eye.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/eye.svg")),
    ("icons/eye-off.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/eye-slash.svg")),
    ("icons/external-link.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/arrow-square-out.svg")),
    ("icons/folder.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/folder.svg")),
    ("icons/folder-open.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/folder-open.svg")),
    ("icons/globe.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/globe.svg")),
    ("icons/hard-drive.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/hard-drive.svg")),
    ("icons/headphones.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/headphones.svg")),
    ("icons/loader-circle.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/circle-notch.svg")),
    ("icons/lock.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/lock.svg")),
    ("icons/lock-open.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/lock-open.svg")),
    ("icons/menu.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/list.svg")),
    ("icons/panel-left.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/sidebar-simple.svg")),
    ("icons/panel-left-open.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/sidebar-simple.svg")),
    ("icons/panel-right-open.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/sidebar-simple-right.svg")),
    ("icons/power.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/power.svg")),
    ("icons/pause.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/pause.svg")),
    ("icons/play.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/play.svg")),
    ("icons/plus.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/plus.svg")),
    ("icons/redo-2.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/arrow-clockwise.svg")),
    ("icons/replace.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/pencil.svg")),
    ("icons/search.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/magnifying-glass.svg")),
    ("icons/settings.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/gear.svg")),
    ("icons/settings-2.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/sliders-horizontal.svg")),
    ("icons/star.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/star.svg")),
    ("icons/sun.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/sun.svg")),
    ("icons/undo-2.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/arrow-counter-clockwise.svg")),
    ("icons/user.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/user.svg")),
    ("icons/window-close.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/x.svg")),
    ("icons/navigation/house.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/house.svg")),
    ("icons/navigation/book-open.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/book-open.svg")),
    ("icons/navigation/folder.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/folder.svg")),
    ("icons/navigation/asterisk.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/asterisk.svg")),
    ("icons/navigation/tags.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/tag.svg")),
    ("icons/navigation/user.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/user.svg")),
    ("icons/navigation/trash-2.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/trash.svg")),
    ("icons/navigation/building-2.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/buildings.svg")),
    ("icons/navigation/circle-user.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/user-circle.svg")),
    ("icons/navigation/settings.svg", include_bytes!("../../ui/design-tokens/assets/phosphor-light/gear.svg")),
];

#[cfg(not(target_family = "wasm"))]
mod standard_assets {
    use super::*;

    pub struct Assets;

    pub(super) fn load(_assets: &Assets, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        gpui_component_assets::Assets.load(path)
    }

    pub(super) fn list(_assets: &Assets, path: &str) -> Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

#[cfg(target_family = "wasm")]
mod standard_assets {
    use super::*;

    /// Browser asset loading needs the stateful gpui-component fetch/cache
    /// source; Bokheim's own icons remain embedded in the WebAssembly module.
    pub struct Assets {
        standard: gpui_component_assets::Assets,
    }

    impl Assets {
        pub fn new(endpoint: impl Into<SharedString>) -> Self {
            Self { standard: gpui_component_assets::Assets::new(endpoint) }
        }
    }

    impl Default for Assets {
        fn default() -> Self {
            Self::new("")
        }
    }

    pub(super) fn load(assets: &Assets, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        assets.standard.load(path)
    }

    pub(super) fn list(assets: &Assets, path: &str) -> Result<Vec<SharedString>> {
        assets.standard.list(path)
    }
}

/// Headphones glyph for audiobook actions, in phosphor light like the rest of
/// the set. There is no such icon in the vendored bundle, so it rides the
/// override table under its own path instead of an `IconName` variant.
#[derive(Clone, Copy)]
pub struct Headphones;

impl gpui_component::IconNamed for Headphones {
    fn path(self) -> gpui::SharedString {
        "icons/headphones.svg".into()
    }
}

/// Bokheim assets layered over the target's standard gpui-component bundle.
pub use standard_assets::Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = PHOSPHOR_LIGHT_ICONS.iter().find(|(icon_path, _)| *icon_path == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if path == WIFI_OFF_ICON_PATH {
            return Ok(Some(Cow::Borrowed(WIFI_OFF_ICON)));
        }
        if path == WIFI_ICON_PATH {
            return Ok(Some(Cow::Borrowed(WIFI_ICON)));
        }
        if path == APP_ICON_PATH {
            return Ok(Some(Cow::Borrowed(APP_ICON)));
        }
        standard_assets::load(self, path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = standard_assets::list(self, path)?;
        assets.extend(PHOSPHOR_LIGHT_ICONS.iter().filter(|(icon_path, _)| icon_path.starts_with(path)).map(|(icon_path, _)| SharedString::from(*icon_path)));
        if WIFI_OFF_ICON_PATH.starts_with(path) {
            assets.push(WIFI_OFF_ICON_PATH.into());
        }
        if WIFI_ICON_PATH.starts_with(path) {
            assets.push(WIFI_ICON_PATH.into());
        }
        if APP_ICON_PATH.starts_with(path) {
            assets.push(APP_ICON_PATH.into());
        }
        Ok(assets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn phosphor_overrides_are_unique_valid_light_svgs() {
        let mut paths = HashSet::new();
        for (path, bytes) in PHOSPHOR_LIGHT_ICONS {
            assert!(paths.insert(path), "duplicate icon override: {path}");
            let svg = std::str::from_utf8(bytes).expect("Phosphor SVG must be UTF-8");
            assert!(svg.starts_with("<svg"), "invalid SVG override: {path}");
            assert!(svg.contains("viewBox=\"0 0 256 256\""), "unexpected Phosphor viewBox: {path}");
            assert!(svg.contains("currentColor"), "icon must inherit the GPUI text color: {path}");
        }
    }
}
