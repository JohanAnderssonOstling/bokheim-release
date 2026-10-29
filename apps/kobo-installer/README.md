# Bokheim Kobo Installer

This small desktop application installs the release Kobo package onto a supported
Kobo connected as USB storage. The verified `tar.gz` package is embedded in
the Windows executable and Linux AppImage at build time; users do not select or
download a second payload.

The Linux AppImage uses the same glibc 2.17 release SDK and ELF checks as the
desktop app. Build it with `python3 scripts/linux/toolchain.py -- cargo build
--release --manifest-path apps/kobo-installer/Cargo.toml`, then run
`apps/kobo-installer/linux/build-appimage.sh`. See
[`../desktop-gpui/linux/README.md`](../desktop-gpui/linux/README.md) for SDK and
AppImage tool requirements.

The installer:

- attempts to mount an unmounted removable USB volume labeled `KOBOeReader`
  on Linux, then detects mounted volumes containing Kobo's `.kobo` marker directory;
- displays the exact mount path and requires an explicit **Install Bokheim**
  click;
- accepts package entries only below `.adds` and `Bokheim`;
- identifies the model from `.kobo/version` and checks the package's supported
  model IDs before writing files;
- replaces individual files through temporary and backup files; and
- verifies that the launcher, FBInk, executable, PDFium runtime, and NickelMenu entry exist
  after extraction;
- queues the pinned NickelMenu v0.6.0 build with Bokheim direct-launch support; and
- attempts to safely eject the Kobo when installation finishes.

With a package supporting on-device updates, open **Settings → Kobo → Check
for updates** on the reader. Bokheim downloads the latest Kobo release over
Wi-Fi, verifies its published size and SHA-256 digest, and stages its files.
**Restart and install** closes the application before the launcher replaces
the program and starts it again. The packaged version must match the release
tag. Books, settings, databases, and optional SSH tools are excluded. This
flow keeps the existing NickelMenu installation and Kobo firmware.

A complete program-file backup is saved before replacement. If installation
is interrupted, the firmware shell restores it before the next launch. This
recovers installation failures; it does not roll back database migrations
after a successfully installed release. Older launchers need one USB install
of a package with on-device update support.

Tapping **Bokheim** in Kobo's bottom navigation opens Bokheim immediately.
The button uses Bokheim's logo, with gray and black versions for its normal
and active states. The installer embeds both icons even for older app packages.
The installer supplies a small NickelMenu extension and replaces older Bokheim
menu configuration. Configuration errors fall back to the regular menu.
Firmware before 4.23.15505 keeps the original menu behavior. See the
[extension build instructions](../desktop-gpui/kobo/nickelmenu-direct/README.md).

On the reader, **Settings → Kobo → Uninstall Bokheim** asks for confirmation,
then removes the program and its menu entry and returns to Kobo's built-in
reader. Books and settings are kept, as are other NickelMenu configurations
and separately installed developer SSH tools. This requires a Kobo package
built with uninstall support; older launchers report that a reinstall is needed.

NickelMenu installs when the Kobo disconnects and restarts. Firmware must be
4.6 or later in the 4.x series. An existing unrelated firmware update is
preserved and blocks installation until that update finishes.

## Sleep and battery diagnostics

Pressing the hardware power button sleeps Bokheim. The on-screen power menu
still offers shutdown, restart, and return to Kobo. Automatic sleep follows
Kobo's **Settings → Energy saving and privacy → Automatically go to sleep after**
choice, including **Never**. Change it in Kobo and reopen Bokheim to apply it.
Bokheim reads `[PowerOptions] AutoSleepMinutes` from
`/mnt/onboard/.kobo/Kobo/Kobo eReader.conf` at launch. It uses five minutes if
the file or setting is missing or invalid and does not rewrite Kobo's settings.
Automatic sleep waits while an audiobook is playing or buffering.
Pausing playback starts a fresh idle period using the configured time. The power
button and sleep cover still request sleep during listening. A failed sleep attempt retries
after one minute; interacting with the reader cancels that retry.

Pausing an audiobook closes its Bluetooth audio output. Press Play to reconnect
and continue from the confirmed position. Device sleep releases audio decoders
and pending source loads before switching Bluetooth and Wi-Fi off. Bluetooth's
previous power state is restored after a confirmed wake; audio stays paused.
Automatic Wi-Fi reconnect stops after 45 seconds if it cannot connect.
Wi-Fi status follows supplicant events, kernel link/address notifications, and
control-socket replacement events. A stable connected or disconnected radio has
no periodic status timer. Connection setup, scans and DHCP keep their bounded
checks; firmware without the required event support falls back to a 30-second
recovery check while Wi-Fi is enabled. Radio-off and sleep have no status timer.
Automatic library network
work waits for a connection, and repeated failed passes back off from 30 seconds
to a maximum of 15 minutes. Reconnecting or requesting a remote refresh wakes
the network workers immediately.
Account maintenance and notification connections also wait while Wi-Fi is
offline. Pending torrents release their network engine while offline, and failed
downloads back off instead of polling progress every two seconds. A stuck audio
helper is killed after its two-second drain deadline. The battery indicator
shares the power manager's samples instead of running a separate polling timer.
Completed torrents do not seed in the background on Kobo; explicit downloads
remain available.

Resource sampling and the text-shaping benchmark are disabled during normal
reading. To collect resource samples for a development session, launch with
`BOKHEIM_KOBO_RESOURCE_MONITOR=1 /bin/sh /mnt/onboard/.adds/bokheim/run-monitored.sh`.
This enables both the shell resource log and `KOBO_RUNTIME_RESOURCE` entries
in the application log. Set `BOKHEIM_WESTERN_SHAPING_BENCHMARK=1` separately
when comparing text-shaping paths. Both diagnostics add work and should stay
disabled when measuring normal battery use.

## Device profiles and verification

The shared `kobo-profile` catalogue expands Plato's advertised supported-device
list into 26 hardware profiles: Touch B/C, Glo, Aura, Aura H2O, Glo HD, Aura ONE and Limited
Edition, Aura Edition 2 and v2, Aura H2O Edition 2 and v2, Clara HD, Forma and
32GB, Nia, Libra H2O, Elipsa, Sage, Libra 2, Clara 2E, Elipsa 2E, Libra Colour,
Clara BW, Clara Colour, and Clara BW TPV. The numeric version-marker ID takes
precedence over product names, which several hardware revisions share. Unknown
models and contradictory identities are rejected.

Page buttons are optional: touch navigation remains available on buttonless
models. The profile selects screen-density scaling, the touch protocol and rotation mapping, frontlight
driver, and available brightness, warmth, auto-rotation and Bluetooth controls.
FBInk handles framebuffer identification and display refresh.
Bluetooth availability follows Kobo's [compatible eReader list](https://help.kobo.com/hc/en-us/articles/4406284800023-Troubleshoot-Bluetooth-pairing-on-your-Kobo-eReader).

The universal package uses ARMv7 with NEON, a private glibc 2.23 runtime built
for Linux 2.6.32, and pinned FBInk sources covering the listed models. Firmware
libraries remain untouched. Legacy packages without a model declaration are
accepted only on Libra 2. Releases retain the old Libra 2 asset name as an
identical copy of the universal package so existing updaters can migrate.

Profiles describe implemented support; they are not evidence that every model
has passed physical-device testing. In particular, an old-kernel libc ABI note
does not establish that all Rust and application syscalls work on an old kernel.
Old devices still need kernel and performance testing. Colour displays currently
use the application's grayscale rendering path.

For startup diagnostics, run `desktop-gpui-kobo --check-device` on the reader.
It reads the firmware and model marker, prints the detected profile, and exits
before opening the display or application databases. This also checks the real
marker parser under ARM QEMU; `--no-display --device-model` bypasses that parser.

Build and capture the real application on the host:

```sh
cargo build --release -p desktop-gpui --no-default-features --features kobo --bin desktop-gpui-kobo
python3 apps/desktop-gpui/scripts/test-kobo-profiles.py
```

The runner uses separate application data for each model and writes PGM frames,
logs and a JSON report under `target/kobo-profile-verification`. Add
`--models 310 376 388 390` for a smaller sample. Touch protocol, lighting and
package compatibility tests run in the shared suite. Headless captures test
layout and capability selection. QEMU user mode can additionally exercise the
ARM binary and bundled loader; it uses the host kernel and does not emulate
Kobo hardware. Neither method validates e-ink waveforms, real touch coordinates,
sensor orientation, suspend, battery use or performance on a physical reader.

On a Linux host with QEMU user emulation, extract the package and supply the
private interpreter at its compiled path inside a QEMU prefix:

```sh
arm_root="$PWD/target/kobo-arm-verification"
mkdir -p "$arm_root"
tar -xzf apps/kobo-installer/package/Bokheim-Kobo-armv7.tar.gz -C "$arm_root"
mkdir -p "$arm_root/sysroot/mnt/onboard/.adds/bokheim"
ln -sfn "$arm_root/.adds/bokheim/runtime" "$arm_root/sysroot/mnt/onboard/.adds/bokheim/runtime"
python3 apps/desktop-gpui/scripts/test-kobo-profiles.py \
  --binary "$arm_root/.adds/bokheim/desktop-gpui-kobo" \
  --qemu "$(command -v qemu-arm-static)" --qemu-root "$arm_root/sysroot" \
  --output target/kobo-arm-profiles
```

This runs all declared profiles and restricts CPU instructions to Cortex-A8
while retaining the host's kernel. Add `--models 310 388` for a smaller sample.
Each run requires newly generated, nonblank frames at the model's exact screen
resolution. A successful JSON report is written only after every selected
profile passes; a failed run removes any previous success report.

Automatic eject uses `udisksctl` on Linux, `diskutil` on macOS, and volume
control calls on Windows. If host permissions or open files prevent eject,
the installer reports success and asks for manual safe eject. It does not
request elevated privileges. If only USB power-off fails after a successful
unmount, it reports that the device is safely unmounted.

On Linux, connection monitoring runs in the background and attempts mounting
once per connection using `udisksctl`. A permission failure asks the user to
mount manually. The monitor remembers mounted volumes so it cannot remount
the Kobo after eject. Windows and macOS use the operating system's normal
automatic mounting.

## Local development

The sibling `GPUI-Fork` checkout must include the Kobo input-profile, density
scaling and capture-settling changes in `crates/gpui_kobo`. Include those fork
changes and update the pinned fork revision when preparing a source release.

Set `BOKHEIM_KOBO_ARCHIVE` to a package produced by
`apps/desktop-gpui/scripts/package-kobo.sh` and `BOKHEIM_VERSION` to the desktop
version before building the graphical executable.

```sh
BOKHEIM_KOBO_ARCHIVE=/absolute/path/to/desktop-gpui-kobo.tar.gz \
BOKHEIM_VERSION=0.1.0 \
cargo run --manifest-path apps/kobo-installer/Cargo.toml
```

The detection layer also accepts `BOKHEIM_KOBO_MOUNT` for development with a
temporary directory containing a `.kobo` directory.

After installing NickelMenu and enabling SSH separately, developers can deploy
over Wi-Fi while the Kobo is awake and Bokheim is closed:

```sh
BOKHEIM_KOBO_SSH_PORT=2222 apps/desktop-gpui/scripts/deploy-kobo-wifi.sh \
  root@192.168.1.123 /absolute/path/to/desktop-gpui-kobo.tar.gz
```

Use the server's actual port and an authorized SSH key. Trust its host key
through a normal SSH connection first. This script streams the package over
SSH, verifies its checksum and installed files, and preserves the one-entry
NickelMenu configuration. It requires neither SFTP nor a remote `scp` binary.

For a standalone development SSH server, build the pinned, statically linked
Dropbear with `sh apps/desktop-gpui/scripts/prepare-kobo-ssh.sh`. Copy the
resulting `target/desktop-gpui-kobo/ssh` directory into `.adds/bokheim/ssh` over
USB, and put the computer's SSH **public** key in `ssh/authorized_keys`. The
latest `run-monitored.sh` starts this optional server before launching Bokheim.
Create an empty `ssh/start-only` file to make the first tap start SSH while
leaving Nickel open; subsequent taps launch Bokheim normally. Eject and unplug
USB, enable Wi-Fi, then tap the existing Bokheim Kobo entry. This adds no menu
entries or firmware update. It must be started again after a reboot.

Dropbear listens on port 2222, with password authentication disabled at build
time. The bootstrap copies keys to `/etc/bokheim-ssh` with private permissions;
FAT storage cannot enforce SSH key permissions. It generates a device host key
there unless a unique, pre-provisioned `ssh/hostkey-seed` is supplied. That seed
is removed after installation. Verify the host fingerprint before trusting the
connection. Startup errors appear in `.adds/bokheim/ssh/start.log`; server logs
are in `/etc/bokheim-ssh/server.log`. Keep the Kobo awake for SSH access.
Set `BOKHEIM_KOBO_SSH_KEY=/path/to/private-key` for Wi-Fi deployment if the key
is not one of the SSH client's default identities.

To opt into persistent development SSH, copy the current `ssh/*.sh` scripts
alongside the provisioned Dropbear executable. Inspect `/proc/1/exe` and
`/etc/inittab` over SSH to confirm the Kobo uses BusyBox init, then run:

```sh
/bin/sh /mnt/onboard/.adds/bokheim/ssh/install-service.sh
```

Alternatively, with a configured and trusted SSH alias, install from the host:

```sh
python3 apps/desktop-gpui/scripts/install-kobo-ssh-service.py bokheim-kobo
```

The host helper inspects init, saves `/etc/inittab` locally, transfers and
verifies the scripts, and confirms init is supervising the service.

This explicitly changes `/etc/inittab`, adding one `respawn` action for
`/etc/bokheim-ssh/supervise.sh`. The original boot configuration is saved in
`/etc/bokheim-ssh/inittab.before-service`. The service keeps its executable and
keys on the internal filesystem, starts at boot, and restarts Dropbear if it
exits. It preserves an already-running server and its sessions during setup.
The launcher recognizes either server location. Service installation reloads
BusyBox init without rebooting; see the upstream [inittab format](https://github.com/mirror/busybox/blob/master/examples/inittab)
and [init implementation](https://github.com/mirror/busybox/blob/master/init/init.c).
Sleep and disabled Wi-Fi still make the reader unreachable. To disable boot
startup, remove the exact `::respawn:/bin/sh /etc/bokheim-ssh/supervise.sh` line
from `/etc/inittab` and reboot when convenient. This service is an explicit
developer setup step; the normal reader installer does not enable it.

## Releases

The Kobo package itself is built on a maintainer machine, not in CI. It uses
the pinned Bootlin ARMv7 glibc toolchain and bundles its runtime alongside
PDFium so the reader can load PDFium without replacing firmware libraries.
The built package is committed to the repository at
`package/Bokheim-Kobo-armv7.tar.gz`, and the `Kobo release` workflow
embeds whatever is committed there.

Whenever the Kobo application changes, rebuild and commit the package before
running the workflow:

```sh
apps/desktop-gpui/scripts/refresh-kobo-package.sh
git add apps/kobo-installer/package/Bokheim-Kobo-armv7.tar.gz
git commit -m "Refresh the committed Kobo package"
```

The script cross-compiles the package, applies the checks listed above, and
replaces the committed archive only once they pass. Note that each refresh
adds a new copy of the archive to the repository's history permanently.
