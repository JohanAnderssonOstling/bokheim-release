#![cfg_attr(windows, windows_subsystem = "windows")]

use bokheim_kobo_installer::{KoboDevice, detect_kobo_devices, install_archive_with_progress, validate_archive};
use gpui::{App, Bounds, Context, FontWeight, SharedString, Task, Window, WindowBounds, WindowOptions, div, prelude::*, px, relative, rgb, size};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const KOBO_ARCHIVE: &[u8] = include_bytes!(env!("BOKHEIM_KOBO_ARCHIVE"));
const VERSION: &str = env!("BOKHEIM_VERSION");
const PREVIEW_ONLY: bool = option_env!("BOKHEIM_INSTALLER_PREVIEW").is_some();
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/JohanAnderssonOstling/bokheim-release/releases/latest";
const KOBO_ASSET_NAME: &str = "Bokheim-Kobo-Libra2-armv7.tar.gz";
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
const SURFACE: u32 = 0xf5f1e8;
const RAISED: u32 = 0xfffcf6;
const TEXT: u32 = 0x1f2220;
const MUTED: u32 = 0x65675d;
const ACCENT: u32 = 0x486754;
const BORDER: u32 = 0x8b7e5b;
const DANGER: u32 = 0x9b312a;
const PROGRESS: u32 = 0xe1e6d8;

enum InstallerStatus {
    Monitoring,
    Ready,
    NickelMenuMissing,
    Installing,
    Installed,
    Failed(String),
}

struct Installer {
    devices: Vec<KoboDevice>,
    status: InstallerStatus,
    _monitor: Task<()>,
    _update_check: Task<()>,
    install_task: Option<Task<()>>,
    package: KoboPackage,
    checking_for_update: bool,
    progress: Arc<SharedInstallProgress>,
}

#[derive(Default)]
struct SharedInstallProgress {
    copied_bytes: AtomicU64,
    total_bytes: AtomicU64,
}

#[derive(Clone)]
enum KoboPackage {
    Embedded,
    Downloaded(Arc<[u8]>),
}

impl KoboPackage {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Embedded => KOBO_ARCHIVE,
            Self::Downloaded(bytes) => bytes,
        }
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

fn fetch_newer_package() -> Result<Option<Vec<u8>>, String> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout_read(Duration::from_secs(30)).timeout_write(Duration::from_secs(30)).build();
    let response = agent.get(LATEST_RELEASE_API).set("Accept", "application/vnd.github+json").set("User-Agent", "Bokheim-Kobo-Installer").call().map_err(|error| error.to_string())?;
    let release: GithubRelease = serde_json::from_reader(response.into_reader()).map_err(|error| error.to_string())?;
    let available = Version::parse(release.tag_name.trim_start_matches('v')).map_err(|error| error.to_string())?;
    let embedded = Version::parse(VERSION).map_err(|error| error.to_string())?;
    if available <= embedded {
        return Ok(None);
    }

    let asset = release.assets.into_iter().find(|asset| asset.name == KOBO_ASSET_NAME).ok_or_else(|| format!("{KOBO_ASSET_NAME} is missing from the latest release"))?;
    let expected_digest = asset.digest.as_deref().and_then(|digest| digest.strip_prefix("sha256:")).ok_or_else(|| format!("{KOBO_ASSET_NAME} has no SHA-256 digest"))?;
    let response = agent.get(&asset.browser_download_url).set("Accept", "application/octet-stream").set("User-Agent", "Bokheim-Kobo-Installer").call().map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    response.into_reader().take(MAX_DOWNLOAD_BYTES + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err("the downloaded Kobo package is unexpectedly large".to_owned());
    }
    let actual_digest = hex::encode(Sha256::digest(&bytes));
    if actual_digest != expected_digest {
        return Err("the downloaded Kobo package failed its SHA-256 check".to_owned());
    }
    validate_archive(&bytes).map_err(|error| error.to_string())?;
    Ok(Some(bytes))
}

impl Installer {
    fn new(cx: &mut Context<Self>) -> Self {
        let monitor = cx.spawn(async move |this, cx| {
            loop {
                let installing = match this.update(cx, |this, cx| {
                    let detected = detect_kobo_devices();
                    if detected != this.devices && !matches!(this.status, InstallerStatus::Installing) {
                        this.devices = detected;
                        this.status = this.detected_status();
                        cx.notify();
                    }
                    let installing = matches!(this.status, InstallerStatus::Installing);
                    if installing {
                        cx.notify();
                    }
                    installing
                }) {
                    Ok(installing) => installing,
                    Err(_) => break,
                };
                cx.background_executor().timer(if installing { Duration::from_millis(50) } else { Duration::from_secs(1) }).await;
            }
        });

        let update_check = cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async { fetch_newer_package() }).await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(Some(package)) = result {
                    this.package = KoboPackage::Downloaded(package.into());
                }
                this.checking_for_update = false;
                cx.notify();
            });
        });

        Self { devices: Vec::new(), status: InstallerStatus::Monitoring, _monitor: monitor, _update_check: update_check, install_task: None, package: KoboPackage::Embedded, checking_for_update: true, progress: Arc::default() }
    }

    fn detected_status(&self) -> InstallerStatus {
        match self.devices.first() {
            Some(device) if device.nickel_menu_detected() => InstallerStatus::Ready,
            Some(_) => InstallerStatus::NickelMenuMissing,
            None => InstallerStatus::Monitoring,
        }
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        if PREVIEW_ONLY || matches!(self.status, InstallerStatus::Installing) {
            return;
        }
        let Some(device) = self.devices.first().cloned() else {
            return;
        };

        self.status = InstallerStatus::Installing;
        self.progress.copied_bytes.store(0, Ordering::Relaxed);
        self.progress.total_bytes.store(0, Ordering::Relaxed);
        cx.notify();
        let root = device.root().to_owned();
        let package = self.package.clone();
        let progress = self.progress.clone();
        let installation = cx.background_executor().spawn(async move {
            install_archive_with_progress(&root, package.bytes(), |update| {
                progress.total_bytes.store(update.total_bytes, Ordering::Relaxed);
                progress.copied_bytes.store(update.copied_bytes, Ordering::Relaxed);
            })
            .map_err(|error| error.to_string())
        });
        self.install_task = Some(cx.spawn(async move |this, cx| {
            let result = installation.await;
            let _ = this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(_) => InstallerStatus::Installed,
                    Err(error) => InstallerStatus::Failed(error),
                };
                this.install_task = None;
                cx.notify();
            });
        }));
    }

    fn status_text(&self) -> SharedString {
        match &self.status {
            InstallerStatus::Monitoring => "Connect your Kobo via USB".into(),
            InstallerStatus::Ready | InstallerStatus::NickelMenuMissing => "✓  Kobo eReader".into(),
            InstallerStatus::Installing => {
                let total = self.progress.total_bytes.load(Ordering::Relaxed);
                let copied = self.progress.copied_bytes.load(Ordering::Relaxed);
                let percent = if total == 0 { 0 } else { (copied.saturating_mul(100) / total).min(100) };
                format!("Installing Bokheim — {percent}%").into()
            }
            InstallerStatus::Installed => "✓  Installed. Find Bokheim Kobo in NickelMenu.".into(),
            InstallerStatus::Failed(error) => format!("Installation failed: {error}").into(),
        }
    }
}

impl gpui::Render for Installer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_install = !PREVIEW_ONLY && !self.devices.is_empty() && !self.checking_for_update && !matches!(self.status, InstallerStatus::Installing);
        let status_color = match self.status {
            InstallerStatus::Failed(_) => rgb(DANGER),
            InstallerStatus::Ready | InstallerStatus::NickelMenuMissing | InstallerStatus::Installed => rgb(ACCENT),
            InstallerStatus::Monitoring | InstallerStatus::Installing => rgb(MUTED),
        };
        let progress = match self.status {
            InstallerStatus::Installing => {
                let total = self.progress.total_bytes.load(Ordering::Relaxed);
                let copied = self.progress.copied_bytes.load(Ordering::Relaxed);
                if total == 0 { 0.0 } else { (copied as f32 / total as f32).clamp(0.0, 1.0) }
            }
            InstallerStatus::Installed => 1.0,
            _ => 0.0,
        };
        let install_button = div()
            .id("install-bokheim")
            .h_full()
            .px(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(ACCENT))
            .text_color(rgb(0xfffaf2))
            .text_size(px(13.0))
            .font_weight(FontWeight::SEMIBOLD)
            .opacity(if can_install { 1.0 } else { 0.45 })
            .when(can_install, |button| button.cursor_pointer().hover(|style| style.bg(rgb(ACCENT))))
            .on_click(cx.listener(|this, _, _, cx| this.install(cx)))
            .child(if self.checking_for_update {
                "Checking…"
            } else if matches!(self.status, InstallerStatus::Installing) {
                "Installing…"
            } else {
                "Install Bokheim"
            });

        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .p_6()
            .justify_center()
            .bg(rgb(SURFACE))
            .text_color(rgb(TEXT))
            .font_family("Libertinus Serif")
            .child(div().mb_4().text_size(px(21.0)).font_weight(FontWeight::BOLD).child("Bokheim Kobo Installation"))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .h(px(58.0))
                    .gap_3()
                    .child(
                        div()
                            .relative()
                            .overflow_hidden()
                            .min_w_0()
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(RAISED))
                            .child(div().absolute().left_0().top_0().h_full().w(relative(progress)).bg(rgb(PROGRESS)))
                            .child(div().relative().min_w_0().w_full().px_4().line_clamp(1).text_size(px(14.0)).text_color(status_color).child(self.status_text())),
                    )
                    .child(install_button.flex_none()),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(520.0), px(190.0)), cx);
        cx.text_system()
            .add_fonts(vec![Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/LibertinusSerif-Regular.otf")), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/LibertinusSerif-Bold.otf")), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/LibertinusSerif-Italic.otf")), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/BodoniModa-Regular.ttf")), Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/BodoniModa-SemiBold.ttf"))])
            .expect("failed to register embedded Libertinus fonts");
        cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), titlebar: Some(gpui::TitlebarOptions { title: Some("Bokheim".into()), ..Default::default() }), focus: true, ..Default::default() }, |_, cx| {
            cx.new(Installer::new)
        })
        .expect("failed to open the Kobo installer window");
        cx.activate(true);
    });
}
