# Local releases and hosted platform builds

Linux/AppImage, Android/APK and web/Wasm builds run directly on the
workstation. Releases do not execute tests or require test-status gates.
Tests remain available as separately invoked development workflows. GitHub-hosted runners build Windows and macOS. GitHub also stores native
release downloads; it does not need to build the locally produced packages.
The Kobo device package and Linux USB installer are also built locally for
releases. Web deploys directly over SSH to our server.

Build the optimized Linux Kobo USB installer without tests:

```bash
python3 apps/kobo-installer/linux/build-release.py --output /absolute/path/to/kobo-packages
```

This reuses the local `linux-glibc217` Cargo cache, embeds the committed device
archive, and keeps the glibc 2.17 packaging checks. Upload the resulting AppImage
and checksum to the version release. The hosted Kobo workflow remains available
for Windows and optional Linux builds; dispatching it currently builds both
installers, so it is not needed to produce the local Linux package.

## Persistent dependency caches

Downloaded crates use the shared Cargo home (`~/.cargo`, or `CARGO_HOME`). Local
Linux release, Android, Kobo and web tooling keeps compiled dependencies outside
source checkouts, under `~/.cache/bokheim/builds/<platform>` on Linux. Direct Cargo
commands use Cargo's normal target directory unless `CARGO_TARGET_DIR` is set. Set
`BOKHEIM_BUILD_CACHE` to choose another local cache root, or `CARGO_TARGET_DIR` to
override a particular build. Keep these directories when removing old release
checkouts. Never use SSHFS for a build cache. Cargo checks compiler, target,
features and flags before reusing artifacts; different targets need their own
compiled dependencies.
Android release builds default to ThinLTO and 16 code-generation units, matching
the release pipeline. Explicit Cargo profile overrides remain supported; changing
them can require rebuilding dependencies.

For direct Cargo commands, use the same cache as the packaging scripts:

```bash
python3 scripts/ci/build_cache.py run android -- cargo build --release --target aarch64-linux-android -p desktop-gpui --lib --no-default-features --features android
python3 scripts/ci/build_cache.py path linux-glibc217
```

Dispatch hosted binary builds from the persistent `ci/release-builds` branch,
with the application source pinned independently. GitHub caches are scoped to
their workflow branch: creating a fresh branch for each release prevents it from
restoring caches on previous release branches. Keep `ci/release-builds` across
releases and update its workflow tooling with ordinary commits. Dependency pins
come from the selected application source, rather than the workflow branch.

```bash
python3 scripts/ci/dispatch-build.py windows --source-commit FULL_PUSHED_COMMIT
python3 scripts/ci/dispatch-build.py macos --source-commit FULL_PUSHED_COMMIT
python3 scripts/ci/dispatch-build.py kobo --source-commit FULL_PUSHED_COMMIT
```

These commands build optimized binaries with tests and automatic publication
disabled. In the Actions UI, select `ci/release-builds`, supply `source_commit`,
and enable `build_only` for the same behavior. Hosted caches are separate for
Windows desktop, both macOS architectures and both Kobo USB installers. Rust
version and dependency changes use compatible cache fallbacks; cache keys do not
include application or sibling-repository commit IDs. The Linux installer cache
also tracks the glibc 2.17 toolchain inputs. Direct version-tag workflow runs have
their own cache scope; use the persistent branch commands for cross-release reuse.

## macOS downloads

`.github/workflows/macos-release.yml` builds optimized Apple Silicon and Intel
apps on GitHub-hosted macOS runners, stages the PDF runtime, and merges
the app and PDFium binaries into `Bokheim-macOS-universal.dmg`. The disk image
includes an Applications shortcut and installation instructions. Bundles use
ad-hoc signatures, require no Apple signing credentials, and are not notarized;
users approve them through macOS Privacy & Security → Open Anyway.

Run the workflow manually with publication disabled to obtain build artifacts.
Publication runs in `JohanAnderssonOstling/bokheim-release`, the repository used
by the website download links; origin-repository builds only retain artifacts.
Version-tag runs, or manual tag runs with `publish` enabled, add the disk image,
checksum and build provenance to the existing release after the exact-commit source and server-protocol checks pass. The version tag must match the app
version. Existing assets are never overwritten. This is a website download;
macOS is not yet part of the signed automatic update feed.

## Source and tools

Use a clean release checkout, with clean sibling GPUI-Fork, HtmlEngine and
HtmlViewCore checkouts at the pins in `.github/workflows/desktop-release.yml`.
Keep this separate from the Actions runner's `_work` directory and build caches.
All platforms must use the same source commit. Do not create or move a version
tag as part of building; publication requires an existing version tag. The tag may point to older source;
the build receipts record the actual build commit.

Install the latest stable Rust (`rustup update stable`), Python 3.11+, gh, and
the platform build tools. CI and local releases use the `stable` channel. The
local command selects Rust through its environment, without changing your
default toolchain. Release builds use optimized profiles.

- Shared suite: PostgreSQL tools (`initdb`, `pg_ctl`, `createdb`), ripgrep and
  native build libraries. The suite creates an isolated temporary database when
  `SYNC_E2E_DATABASE_URL` is absent.
- Linux: native GPUI libraries, linuxdeploy and appimagetool in PATH (or set
  `LINUXDEPLOY` and `APPIMAGETOOL`). Release tooling pins remain in the Windows
  workflow environment. The desktop build automatically uses the checksum-pinned
  glibc 2.17 SDK in `scripts/linux/toolchain.json`, including Zig, native link
  libraries and matching GNU C++ headers/runtime. Install `cpio`, `zstd`,
  `pkg-config`, `file` and `binutils` on the build host. SDK files are cached in
  `.linux-toolchain/` (override with `BOKHEIM_LINUX_SDK`). The AppImage runtime
  and every bundled core library must pass the 2.17 audit. The Linux Kobo
  installer workflow uses the same SDK and checks. The optional browser helper
  also targets glibc 2.17, using system GTK/WebKit development libraries through
  an isolated linker search directory. Build it separately with
  `python3 scripts/linux/browser-toolchain.py`, or let the desktop build include
  it. Set `BOKHEIM_BROWSER_HELPER` to a separately built executable to skip its local build
  and include it in the AppImage. A compatible system GTK 3/WebKitGTK 4.1/libsoup 3
  runtime is still needed. Set `BOKHEIM_BROWSER=disabled` to skip both building and packaging
  the browser, or `BOKHEIM_BROWSER=required` to require it during packaging.
  Linux packaging also stages `install.sh` for per-user AppImage installation
  from the command line. Normal AppImage launches automatically register their
  current path for KDE/GNOME without the script or an additional AppImage copy.
  Launching a moved AppImage repairs its menu entry and command link.
  New Linux build receipts use protocol 2 and verify
  the installer alongside the AppImage; protocol 1 remains valid for existing
  releases without an installer.
- Android: Java 21, Gradle 8.14.3, Android SDK 36/build-tools 36.0.0, NDK
  29.0.13599879 and Rust target `aarch64-linux-android`. Configure production
  signing in private Gradle user properties or `BOKHEIM_ANDROID_*` environment
  variables; password `_FILE` settings are supported. Set `ANDROID_CERT_SHA256` to the production signer. Signature, package/version,
  release mode and ARM64 native-library checks run without a device. Compatibility
  metadata comes from the same build source and configuration. Device
  instrumentation is a separate, explicitly invoked development task.
- Web: Node, Rust `wasm32-unknown-unknown` and `rust-src`, Binaryen `wasm-opt`,
  wasm-bindgen-cli matching Cargo.lock, and Brotli. Browser test tooling is
  only needed when separately running browser tests.

## Local commands

```bash
# One platform at a time. Output must be a fresh directory outside the checkout.
python3 scripts/ci/local-release.py linux --output "$HOME/bokheim-builds/linux-01"
python3 scripts/ci/local-release.py android --output "$HOME/bokheim-builds/android-01"
python3 scripts/ci/local-release.py web --output "$HOME/bokheim-builds/web-01" --deploy
```

`--dry-run` prints the plan without running checks, builds, uploads or deployment.
No test suite or commit test status is required. The source and sibling pins must remain
clean through completion. Source commit and artifact hashes are recorded only
after platform verification succeeds. An incomplete output directory is retained
for diagnosis but has no verified receipt; retry using a fresh output directory.

Use `--upload --tag EXISTING_VERSION_TAG` on a native local command to upload after
verification. Or upload an already verified directory without rebuilding:

```bash
python3 scripts/ci/release_artifacts.py upload linux --directory "$HOME/bokheim-builds/linux-01" --tag EXISTING_VERSION_TAG
python3 scripts/ci/release_artifacts.py upload android --directory "$HOME/bokheim-builds/android-01" --tag EXISTING_VERSION_TAG
```

Uploads verify package hashes, the live server protocol, package version and existing version
tag. They create/update only a **draft release**, never tags or public assets.
A draft containing a different source receipt is rejected. The default release
repository is `JohanAnderssonOstling/bokheim-release`.

Web `--deploy` uses `servers/sync/deploy/deploy-web-remote.sh --dist DIRECTORY`.
It verifies the receipt and deploys those exact files, without a second build.
Existing `BOKHEIM_DEPLOY_HOST`, `BOKHEIM_DEPLOY_USER` and SSH settings apply.
To stage for manual installation instead, use that script with `--dist` and
`--stage-only`. Web does not upload a bundle to a GitHub release.

## Windows

For the selected source revision:

```bash
gh workflow run desktop-release.yml --repo JohanAnderssonOstling/bokheim-release --ref SOURCE_REF -f publish=false
```

Windows builds the optimized executable and packages the
installer, single-file standalone EXE, portable ZIP and update ZIP, probes the extracted portable executable's
runtime metadata and records artifact hashes. The portable ZIP contains the same
complete runtime as the update ZIP; extract it into a writable folder and launch
`Bokheim.exe` without running an installer. Its accompanying helper, taxonomy and
speech models must stay together. Libraries and settings use normal per-user storage.
The standalone EXE embeds the portable runtime and extracts it automatically under
`%LOCALAPPDATA%\Bokheim\Standalone` on first launch. Packaging records the app’s runtime compatibility metadata.
New Windows build receipts use protocol 2 and require both standalone downloads
and their checksums; existing protocol 1 receipts remain valid for earlier releases.
`publish=true` uploads to the version's existing tag draft after checking that
tag exists and matches the package version. The actual build commit is recorded separately. Windows platform checks also run on relevant
pull requests and pushes. No Linux, Android or web jobs are scheduled by these
two workflows. Updating their definitions does not restart or cancel old runs.

The shared-suite and platform-test workflows remain available separately; release
workflows do not dispatch or require them. The Kobo ARM package is built locally
and committed before installer builds; see `apps/kobo-installer/README.md`.

## Feed publication

Collect all three verified native platforms on the same version draft. Prepare
using `servers/updates/prepare-github-release.py --source-commit FULL_SHA` in place
of the legacy `--run-id`: it verifies individual platform receipts, package
hashes, Android signing identity, compatibility and the successful Windows run.
The old combined-run preparation mode remains supported for existing builds.

Review the prepared bundle, publish the GitHub draft, then use the existing
review/sign/publish command in `servers/updates/README.md`. Publication rechecks
the source identity and platform receipts. Signing keys stay on the trusted local
machine. Permission to upload releases/report commit statuses is the trust
boundary for local build attestations.

Before publishing native packages or deploying a verified web bundle, the pipeline probes the live login endpoint with a version-only protobuf envelope and no login fields. A protocol mismatch blocks publication until the matching server is deployed.

## Build and distribute packages

All release builds omit tests. The legacy `build_only` workflow input remains
accepted for existing dispatch clients; it does not enable tests when false.
Package signatures, hashes, versions, source receipts, server-protocol checks,
and platform compatibility checks remain required.

Completed Windows, macOS, and Kobo installer packages can be copied from workflow
artifacts to an existing draft with `distribute-release.yml`. Supply the platform,
successful packaging run ID, full build source SHA, and draft tag. It verifies
source, version, and hashes without running tests or creating tags. Existing
assets must already contain identical bytes.
