#![cfg_attr(windows, windows_subsystem = "windows")]

use bokheim_kobo_installer::{DeviceDetection, DeviceMonitor, EjectOutcome, KoboDevice, detect_kobo_devices, eject_device, install_archive_with_progress, validate_archive};
use gpui::{App, Bounds, Context, FontWeight, SharedString, Task, Window, WindowBounds, WindowOptions, div, prelude::*, px, relative, rgb, size};
use kobo_package::{RELEASE_API, newer_release};
use semver::Version;
use std::borrow::Cow;
use std::io::ErrorKind;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use update_protocol::copy_verified;

const KOBO_ARCHIVE: &[u8] = include_bytes!(env!("BOKHEIM_KOBO_ARCHIVE"));
const VERSION: &str = env!("BOKHEIM_VERSION");
const PREVIEW_ONLY: bool = option_env!("BOKHEIM_INSTALLER_PREVIEW").is_some();
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
    MountFailed(String),
    Ready,
    NickelMenuMissing,
    Unsupported(String),
    Installing,
    Installed(String),
    Failed(String),
}

struct Installer {
    devices: Vec<KoboDevice>,
    mount_error: Option<String>,
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

fn fetch_newer_package() -> Result<Option<Vec<u8>>, String> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout_read(Duration::from_secs(30)).timeout_write(Duration::from_secs(30)).build();
    let response = agent.get(RELEASE_API).set("Accept", "application/vnd.github+json").set("User-Agent", "Bokheim-Kobo-Installer").call().map_err(|error| error.to_string())?;
    let installed = Version::parse(VERSION).map_err(|error| error.to_string())?;
    let Some(release) = newer_release(response.into_reader(), &installed)? else {
        return Ok(None);
    };
    let response = agent.get(&release.url).set("Accept", "application/octet-stream").set("User-Agent", "Bokheim-Kobo-Installer").call().map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    copy_verified(response.into_reader(), &mut bytes, &release.artifact())?;
    validate_archive(&bytes).map_err(|error| error.to_string())?;
    Ok(Some(bytes))
}

impl Installer {
    fn new(cx: &mut Context<Self>) -> Self {
        let monitor = cx.spawn(async move |this, cx| {
            let mut device_monitor = DeviceMonitor::default();
            loop {
                let installing = match this.update(cx, |this, cx| {
                    let installing = matches!(this.status, InstallerStatus::Installing);
                    if installing {
                        cx.notify();
                    }
                    installing
                }) {
                    Ok(installing) => installing,
                    Err(_) => break,
                };
                if !installing {
                    let (updated_monitor, detected) = cx.background_executor().spawn(async move {
                        let detected = if PREVIEW_ONLY {
                            DeviceDetection { devices: detect_kobo_devices(), mount_error: None }
                        } else {
                            device_monitor.poll()
                        };
                        (device_monitor, detected)
                    }).await;
                    device_monitor = updated_monitor;
                    if this.update(cx, |this, cx| {
                        if matches!(this.status, InstallerStatus::Installing) { return; }
                        if detected.devices != this.devices || detected.mount_error != this.mount_error {
                            this.devices = detected.devices;
                            if let Some(error) = &detected.mount_error {
                                eprintln!("Automatic Kobo mount failed: {error}");
                            }
                            this.mount_error = detected.mount_error;
                            if !matches!(this.status, InstallerStatus::Installed(_)) || !this.devices.is_empty() {
                                this.status = this.detected_status();
                            }
                            cx.notify();
                        }
                    }).is_err() { break; }
                }
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
                if !matches!(this.status, InstallerStatus::Installing | InstallerStatus::Installed(_)) {
                    this.status = this.detected_status();
                }
                cx.notify();
            });
        });

        Self { devices: Vec::new(), mount_error: None, status: InstallerStatus::Monitoring, _monitor: monitor, _update_check: update_check, install_task: None, package: KoboPackage::Embedded, checking_for_update: true, progress: Arc::default() }
    }

    fn detected_status(&self) -> InstallerStatus {
        if let Some(device) = self.devices.first() {
            let compatible = device.profile().and_then(|profile| kobo_package::validate_device(self.package.bytes(), kobo_package::Policy::Usb, profile));
            if let Err(error) = compatible { return InstallerStatus::Unsupported(error.to_string()); }
        }
        match self.devices.first() {
            Some(device) if device.nickel_menu_detected() => InstallerStatus::Ready,
            Some(_) => InstallerStatus::NickelMenuMissing,
            None => match &self.mount_error {
                Some(error) => InstallerStatus::MountFailed(error.clone()),
                None => InstallerStatus::Monitoring,
            },
        }
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        if PREVIEW_ONLY || matches!(self.status, InstallerStatus::Installing) {
            return;
        }
        let Some(device) = self.devices.first().cloned() else {
            return;
        };
        if matches!(self.detected_status(), InstallerStatus::Unsupported(_)) {
            self.status = self.detected_status();
            cx.notify();
            return;
        }

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
            .map_err(|error| {
                eprintln!("Kobo installation failed: {error}");
                match error.kind() {
                    ErrorKind::StorageFull => "There isn't enough free space on your Kobo. Free up space, then try installing again.",
                    ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => "The installer can't save files to your Kobo. Check that your computer can write to it, then try again.",
                    ErrorKind::InvalidData => "The installation package couldn't be read. Download the installer again and retry.",
                    _ => "Installation couldn't finish. Reconnect your Kobo via USB and try again.",
                }.to_owned()
            })?;
            let message = match eject_device(&root) {
                Ok(EjectOutcome::Ejected) => "Installed and ejected. Disconnect the Kobo, let it restart, then tap Bokheim.".to_owned(),
                Ok(EjectOutcome::Unmounted) => "Installed and safely unmounted. Disconnect the Kobo, let it restart, then tap Bokheim.".to_owned(),
                Err(error) => {
                    eprintln!("Automatic Kobo eject failed: {error}");
                    "Installed. Use your computer's eject option to safely remove your Kobo, then disconnect it and let it restart.".to_owned()
                },
            };
            Ok::<_, String>(message)
        });
        self.install_task = Some(cx.spawn(async move |this, cx| {
            let result = installation.await;
            let _ = this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(message) => InstallerStatus::Installed(message),
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
            InstallerStatus::MountFailed(_) => "Kobo detected — mount required".into(),
            InstallerStatus::Ready | InstallerStatus::NickelMenuMissing => "✓  Kobo eReader".into(),
            InstallerStatus::Installing => {
                let total = self.progress.total_bytes.load(Ordering::Relaxed);
                let copied = self.progress.copied_bytes.load(Ordering::Relaxed);
                let percent = if total == 0 { 0 } else { (copied.saturating_mul(100) / total).min(100) };
                format!("Installing Bokheim — {percent}%").into()
            }
            InstallerStatus::Installed(_) => "✓  Installed".into(),
            InstallerStatus::Failed(_) => "Installation failed".into(),
            InstallerStatus::Unsupported(_) => "Kobo or package is incompatible".into(),
        }
    }

    fn status_detail(&self) -> Option<SharedString> {
        match &self.status {
            InstallerStatus::MountFailed(_) => Some("Your computer couldn't open the Kobo automatically. Open it in your file manager, then return here to install Bokheim.".into()),
            InstallerStatus::Installed(message) | InstallerStatus::Failed(message) | InstallerStatus::Unsupported(message) => Some(message.clone().into()),
            _ => None,
        }
    }
}

impl gpui::Render for Installer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_install = !PREVIEW_ONLY && !self.devices.is_empty() && !self.checking_for_update && !matches!(self.status, InstallerStatus::Installing | InstallerStatus::Unsupported(_));
        let status_color = match self.status {
            InstallerStatus::Failed(_) | InstallerStatus::MountFailed(_) | InstallerStatus::Unsupported(_) => rgb(DANGER),
            InstallerStatus::Ready | InstallerStatus::NickelMenuMissing | InstallerStatus::Installed(_) => rgb(ACCENT),
            InstallerStatus::Monitoring | InstallerStatus::Installing => rgb(MUTED),
        };
        let progress = match self.status {
            InstallerStatus::Installing => {
                let total = self.progress.total_bytes.load(Ordering::Relaxed);
                let copied = self.progress.copied_bytes.load(Ordering::Relaxed);
                if total == 0 { 0.0 } else { (copied as f32 / total as f32).clamp(0.0, 1.0) }
            }
            InstallerStatus::Installed(_) => 1.0,
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
            .font_family("NV Garamond")
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
            .when_some(self.status_detail(), |view, detail| view.child(div().mt_3().text_size(px(13.0)).text_color(status_color).child(detail)))
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(520.0), px(260.0)), cx);
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/NV_Garamond-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/NV_Garamond-Bold.ttf")),
                Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/NV_Garamond-Italic.ttf")),
                Cow::Borrowed(include_bytes!("../../desktop-gpui/assets/fonts/NV_Garamond-BoldItalic.ttf")),
            ])
            .expect("failed to register embedded interface fonts");
        cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), titlebar: Some(gpui::TitlebarOptions { title: Some("Bokheim".into()), ..Default::default() }), focus: true, ..Default::default() }, |_, cx| {
            cx.new(Installer::new)
        })
        .expect("failed to open the Kobo installer window");
        cx.activate(true);
    });
}
