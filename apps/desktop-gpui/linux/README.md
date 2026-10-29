# Linux packaging

## Automatic desktop integration

On each normal launch, the AppImage automatically checks its KDE/GNOME
application-menu entry, command link and icon against the file's current
absolute location. It updates missing or outdated registration files and
continues running from that location. No confirmation, `sudo`, separate
installer or copy of the AppImage is needed. The menu entry and automatic
updater use the AppImage that was launched.

If the AppImage is moved or renamed, launching it once at its new location
repairs the application-menu entry and `~/.local/bin/bokheim` link. Until that
launch, the old entry points to the old location. When multiple AppImages are
kept, registration follows the one most recently launched without modifying
the other files. Unchanged registration files and caches are left alone.
Registration is serialized across concurrent launches. If registration fails,
the AppImage continues running normally.

Metadata checks, update preparation and update trials bypass registration.
Development binaries and native package installations do not register
themselves. Set `BOKHEIM_DISABLE_DESKTOP_INTEGRATION=1` to suppress registration
when deliberately running a portable AppImage. The launcher uses Bash,
coreutils and util-linux `flock`; registration needs no FUSE operations beyond
those used to launch the AppImage itself.

## Optional command-line installer

Linux releases also include `install.sh` for command-line installation before
the first GUI launch. It downloads the latest stable x86-64 release, verifies its SHA-256
checksum, and installs the AppImage, application-menu entry and icon for the
current user. Run it without `sudo`:

```sh
curl --fail --location --output install.sh \
  https://github.com/JohanAnderssonOstling/bokheim-release/releases/latest/download/install.sh
bash install.sh
```

For a specific release, use `bash install.sh --version 0.1.10`. To install a
previously downloaded AppImage without network access, keep its published
checksum beside it as `Bokheim-x86_64.AppImage.sha256` and run:

```sh
bash install.sh --appimage ./Bokheim-x86_64.AppImage
```

The stable application path is
`${XDG_DATA_HOME:-$HOME/.local/share}/bokheim/application/Bokheim.AppImage`.
The installer adds `~/.local/bin/bokheim` and a desktop entry with an absolute
application path, so KDE/GNOME search works even when `~/.local/bin` is absent
from the desktop session's `PATH`. Set `BOKHEIM_BIN_DIR` to change the command
directory. Existing command and desktop entry files are retained with a
`.before-install` suffix before replacement. Re-running the installer replaces
the application while preserving libraries and settings. The installed file
remains an AppImage, compatible with the existing application updater.

The installer does not install distribution packages. Normal AppImage launches
need FUSE support; on systems without it, use
`APPIMAGE_EXTRACT_AND_RUN=1 ~/.local/bin/bokheim` to extract and run without
mounting. The optional browser still needs the system WebKitGTK runtime
described below. Installation extracts only the menu icon and does not open a
GUI window or require FUSE.

## AppImage

Linux desktop and Kobo installer releases target glibc 2.17. The main app,
optional WebKitGTK browser helper, bundled native libraries and AppImage runtime
are checked against that ceiling.

The release tooling builds an x86-64 AppImage and its SHA-256 checksum.
GitHub can store these artifacts, while in-app update discovery uses the signed
Bokheim feed. The package does not embed a separate zsync update endpoint.

Build the release binary and package it with versioned releases of
`linuxdeploy` and `appimagetool` available on `PATH`. The build script prepares
the checksum-pinned Zig 0.15.2/native SDK automatically; it does not use the
host's glibc headers or C++ runtime for the main executable. Install Python
3.11+, `cpio`, `zstd`, `pkg-config`, `file`, `binutils`, and the native GPUI
development packages on the build host. Docker and an old distribution are not
required. SDK downloads and generated files stay in `.linux-toolchain/`; set
`BOKHEIM_LINUX_SDK` to use another cache directory. `--offline` requires those
verified archives to have been downloaded already.

```sh
python3 apps/desktop-gpui/scripts/build.py
apps/desktop-gpui/linux/appimage/build-appimage.sh
```

Google Drive is an optional Cargo feature and is disabled by default, including
GitHub releases. To include it in a custom desktop build (Linux, macOS or Windows),
run `python3 apps/desktop-gpui/scripts/build.py --google-drive`, or add
`--features google-drive` when building `desktop-gpui` directly with Cargo.
Drive connection controls and the folder picker are hidden when the feature
is disabled. OAuth client configuration is still required for sign-in.

The webview helper uses GPUI for browser controls and WebKitGTK 4.1 for
webpages. Browser windows embed WebKitGTK using X11, including XWayland in a
Wayland session; OPDS verification uses a GTK window with native Wayland
support. Build hosts need
`libgtk-3-dev`, `libwebkit2gtk-4.1-dev` and `libdbus-1-dev`. The AppImage does not bundle
WebKitGTK: the helper is copied without linuxdeploy and uses the system's
`libwebkit2gtk-4.1` at runtime, so WebKit security updates come from the
distribution. Without it, the rest of Bokheim works: browser verification
reports that WebKitGTK is missing, and stores are not offered.

The browser helper is optional in the AppImage. By default packaging includes
the executable beside `bokheim` if it exists. `BOKHEIM_BROWSER=required` makes a
missing helper an error; an explicit `OPDS_BROWSER` path also requires that file.
To build and package without a browser (and without WebKitGTK development
packages), use both options so a stale helper is not accidentally included:

```sh
python3 apps/desktop-gpui/scripts/build.py --without-browser
BOKHEIM_BROWSER=disabled apps/desktop-gpui/linux/appimage/build-appimage.sh
```

The browser helper uses the same glibc 2.17 SDK as the main app. Host
`pkg-config` supplies GTK/WebKit headers and versions; an isolated library
directory exposes the required GUI libraries to the linker without adding the
host's libc search directory. Only the helper allows unresolved references
inside system shared libraries during linking; its own glibc imports are still
audited against 2.17. To build the helper separately, run:

```sh
python3 scripts/linux/browser-toolchain.py
```

Set `OPDS_BROWSER=/path/to/bokheim-opds-webview` when packaging a separately
built helper. Only that executable is copied; its system libraries are not
bundled. The helper's process restores the system library path from before
AppRun, so the main app's compatibility libraries do not override WebKitGTK's
dependencies. The executable's glibc 2.17 ceiling does not provide WebKitGTK on
older distributions: a compatible system GTK 3, WebKitGTK 4.1 and libsoup 3
runtime is still required.

The SDK pins its GNU C++ headers and runtime together. Packaging includes a
fallback C++ runtime for older hosts, but prefers a usable system runtime so
modern graphics drivers retain their matching C++ ABI and X11 libraries. The
same fallback directory includes the SDK's XCB core/XKB and xkbcommon libraries;
XCB graphics-driver extensions are not bundled. Fontconfig, FreeType, ALSA, WebKitGTK and graphics
drivers come from the distribution. A 2.17 glibc baseline does not remove their
own API and hardware requirements; old kernels and drivers still need runtime
validation.

The Linux Kobo installer uses the same SDK and packaging checks:

```sh
python3 scripts/linux/toolchain.py -- cargo build --release --manifest-path apps/kobo-installer/Cargo.toml
apps/kobo-installer/linux/build-appimage.sh
```

At runtime the app probes the helper once per launch, with a five-second timeout.
A missing helper, incompatible glibc, absent WebKitGTK or failed probe hides the
browser button and stores. Store windows also require X11/XWayland. The probe
checks that the helper can load its runtime; actual window creation can still
fail and is handled when opening a browser. Ordinary OPDS requests and the
reader do not use the helper. Feeds requiring browser verification remain
unavailable when the helper cannot run.

The embedded browser uses a GTK-owned native surface parented inside the
GPUI window. GTK handles its native focus events directly; the helper routes
keyboard focus between the page and address bar and restores page focus after
switching windows. The GTK surface follows the GPUI viewport when resizing.
Address history appears in a dropdown attached to the shared search-field
component. While it is open, a native clipping surface reveals that GPUI
surface and routes clicks to it without changing the webpage's viewport. Its
shape is independent of GTK's painting. Closing the
dropdown restores the full native surface.

Browser windows receive the application's browsing preferences at launch and
use the shared palette, fonts, and toolbar components. Since the helper runs
in a separate process, its theme remains that launch snapshot. On Linux the
same light/dark preference is exposed natively to websites through
`prefers-color-scheme`; websites choose their own styling in response. Download
notifications reuse the main application’s toasts at the bottom right, including
the filename, active transfers, completion, and failure. On Linux, downloads
with a reported size show a native progress bar and percentage; unknown sizes
keep the loading indicator. Successful downloads
auto-dismiss; failures stay visible until dismissed. On Linux, native page
clipping keeps toasts and address suggestions visible together without resizing
the webpage.
Back and Forward use native history state to disable unavailable navigation.
The link context menu omits “Download Linked File”. Opening a link in a new
window creates another GPUI browser window with the same profile, preferences,
and download handling; the original page stays open.

Magnet links and `.torrent` downloads start the library's Rust torrent worker.
It downloads all files with default networking, then moves the completed folder
into the library for its normal scan. Torrent metadata and book associations
stay in the device's library database. Seeding follows whole-folder moves made
in the app; the book detail actions stop or resume seeding, or remove the torrent
while keeping its files. Transfers run while the application is open and resume
when the library is reopened.

HTTP and torrent transfers use the same download notifications and progress bars.
Each torrent has a separate entry, updated once per second. Downloaded `.torrent`
metadata hands its notification to the payload transfer; completion reports
whether seeding continues. Both kinds show the same waiting and error states.

Settings → Transfers has one "Allow automatic transfers on metered connections"
setting, off by default. Automatic uploads, cover/catalog updates and completed
torrents' seeding wait and resume when allowed. Opening and reading books,
explicit downloads from the library, catalog, OPDS or browser, and approved app
updates proceed on metered connections. Pending user-requested downloads retain
this exemption across retries and restarts. Linux uses NetworkManager's reported
cost; unavailable cost information is treated as unknown and allowed.

The globe button, store links, and feed verification share the original globe
browser profile under `opds-webview/stores` in the application's data directory.
Cookies and website storage persist between launches. On Linux, cookies from
older per-site profiles are imported once into `cookies.sqlite`, preserving
their expiration and security attributes. SQLite keeps concurrent windows from
overwriting each other's saved cookies; the old cookie files are retained.

The focus regression uses a local page and an isolated X11 display. With
Python GTK 3 bindings, `xdotool`, and Xvfb installed, run it after building
the helper:

```sh
xvfb-run -a python3 apps/opds-webview/tests/native_focus.py --display-env
xvfb-run -a python3 apps/opds-webview/tests/profile.py --display-env
xvfb-run -a python3 apps/opds-webview/tests/responsiveness.py --display-env
```

The responsiveness regression hangs a page with an infinite JavaScript loop
and checks address editing, navigation, Back, Forward, Reload, and shutdown.

The UI regression also needs ffmpeg, tesseract with English language data, and
Python Pillow. It checks dropdown selection with both mouse and keyboard,
unchanged page dimensions, the native color-scheme hint, toast lifecycle,
simultaneous overlays, and navigation availability using a local server:

```sh
xvfb-run -a python3 apps/opds-webview/tests/ui.py --display-env --appearance dark
xvfb-run -a python3 apps/opds-webview/tests/ui.py --display-env --appearance light
```

The packaging script writes `Bokheim-x86_64.AppImage`,
its `.sha256` file and `install.sh` to `release/`. Set `RELEASE_DIRECTORY`, `DESKTOP_BINARY`,
`LINUXDEPLOY`, or `APPIMAGETOOL` to override the defaults.
Packaging and local release checks inspect the AppImage runtime and every
packaged ELF file. The runtime, main executable and bundled libraries must
require no newer than glibc 2.17. The optional
`usr/bin/bokheim-opds-webview` is audited separately against the same ceiling.

Update discovery is enabled only when the build contains a valid public signing
key. Settings shows one row when an update is available. Explicit Update consent
persists approval and stages verified packages; activation waits for a later
launch. Restart is an optional explicit action. Database/taxonomy changes and the
AppImage replacement use a recoverable startup transaction.

The `APPIMAGE` environment variable identifies the package that can be replaced.
Without it, the host can offer compatible taxonomy updates only. Package launchers
can also disable application replacement using `BOKHEIM_DISABLE_SELF_UPDATE=1`;
the AUR package must set this variable. Installation permissions and actual storage
deficits appear only when action is required. See `servers/updates/README.md` for
public-key configuration, release preparation and signed feed publication.

The hosted platform workflows check out the companion GPUI-Fork, HtmlEngine, and
HtmlViewCore repositories beside this repository because the Cargo workspace
currently uses sibling paths. Their pinned revisions must be updated whenever
Bokheim starts depending on newer companion commits.

## Arch Linux (AUR)

The [`aur` directory](aur/README.md) contains the `bokheim-bin` recipe for the
public Linux AppImage. It installs an extracted copy with desktop integration,
declares the host libraries including WebKitGTK, and leaves application updates
to the package manager. See that guide for local builds, release updates, and
the corresponding source availability still needed before publication. Bokheim
uses GPL-3.0-or-later; both the AppImage and AUR package include its license text
and copyright notice.

## Flatpak

Flatpak builds use the `flatpak` Cargo feature. It keeps PDFium out of the
executable and loads the application-private library installed at
`/app/lib/libpdfium.so`.

The Flatpak manifest must fetch the pinned PDFium archive declared in
`shared/pdfium/artifacts.toml`, verify its SHA-256 checksum, and make the
archive available to Cargo without network access:

```sh
python3 shared/pdfium/prepare.py --target x86_64-unknown-linux-gnu \
  --archive /run/build/bokheim/pdfium-linux-x64.tgz --offline
python3 scripts/linux/toolchain.py --offline -- cargo build --locked --release -p desktop-gpui --features flatpak
cargo build --locked --release -p opds-webview
```

The Flatpak source inputs must also include the checksum-pinned SDK archives
from `scripts/linux/toolchain.json` in `.linux-toolchain/downloads/`. The core
executable uses the same 2.17 target; the browser builds against Flatpak's own
WebKitGTK runtime.

Install the artifacts into the Flatpak application prefix:

```text
/app/bin/bokheim
/app/bin/bokheim-opds-webview
/app/lib/libpdfium.so  # from .pdfium/x86_64-unknown-linux-gnu/
/app/share/applications/se.bokheim.Bokheim.desktop
```

`PDFIUM_PACKAGED_PATH` can override the compile-time package location when a
different immutable application prefix is required. For local diagnostics,
`BOKHEIM_PDFIUM_LIBRARY_PATH` overrides the library at runtime.
