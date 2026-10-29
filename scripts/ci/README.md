# Local releases and hosted platform builds

Shared tests, Linux/AppImage, Android/APK and web/Wasm run directly on the
workstation. GitHub-hosted runners build Windows and macOS. GitHub also stores native
release downloads; it does not need to build the locally produced packages.
Web deploys directly over SSH to our server.

## macOS downloads

`.github/workflows/macos-release.yml` builds optimized Apple Silicon and Intel
apps on GitHub-hosted macOS runners, tests the relocated PDF runtime, and merges
the app and PDFium binaries into `Bokheim-macOS-universal.dmg`. The disk image
includes an Applications shortcut and installation instructions. Bundles use
ad-hoc signatures, require no Apple signing credentials, and are not notarized;
users approve them through macOS Privacy & Security → Open Anyway.

Run the workflow manually with publication disabled to obtain build artifacts.
Publication runs in `JohanAnderssonOstling/bokheim-release`, the repository used
by the website download links; origin-repository builds only retain artifacts.
Version-tag runs, or manual tag runs with `publish` enabled, add the disk image,
checksum and build provenance to the existing release after the exact-commit
shared-test and server-protocol checks pass. The version tag must match the app
version. Existing assets are never overwritten. This is a website download;
macOS is not yet part of the signed automatic update feed.

## Source and tools

Use a clean release checkout, with clean sibling GPUI-Fork, HtmlEngine and
HtmlViewCore checkouts at the pins in `.github/workflows/desktop-release.yml`.
Keep this separate from the Actions runner's `_work` directory and build caches.
All platforms must use the same source commit. Do not create or move a version
tag as part of building; publication requires an existing version tag. The tag may point to older source;
the build receipts record the actual tested commit.

Install the latest stable Rust (`rustup update stable`), Python 3.11+, gh, and
the platform build tools. CI and local releases use the `stable` channel. The
local command selects Rust through its environment, without changing your
default toolchain. All builds/tests use optimized release profiles.

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
  it. Set `OPDS_BROWSER` to a separately built executable to skip its local build
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
  metadata comes from the same build source and configuration. Optionally set
  `ANDROID_SERIAL` to a dedicated ARM64 test device to additionally build/install
  instrumentation and check the running native code. The receipt distinguishes
  package verification from a device runtime test.
- Web: Node, Rust `wasm32-unknown-unknown` and `rust-src`, Binaryen `wasm-opt`,
  wasm-bindgen-cli matching Cargo.lock, and Playwright 1.61.1 with Firefox and
  Chromium installed. Set `PLAYWRIGHT_MODULE` to its absolute `index.mjs` path.

## Local commands

```bash
# Optional: run/report shared tests first. Platform commands reuse this success.
RUSTUP_TOOLCHAIN=stable RUSTC_BOOTSTRAP=1 python3 scripts/ci/shared-tests.py --report JohanAnderssonOstling/bokheim-release

# One platform at a time. Output must be a fresh directory outside the checkout.
python3 scripts/ci/local-release.py linux --output "$HOME/bokheim-builds/linux-01"
python3 scripts/ci/local-release.py android --output "$HOME/bokheim-builds/android-01"
python3 scripts/ci/local-release.py web --output "$HOME/bokheim-builds/web-01" --deploy
```

`--dry-run` prints the plan without running checks, builds, uploads or deployment.
The local command reuses the newest successful `bokheim/shared-tests-v1` status
for the exact commit; otherwise it runs/reports shared tests locally. A newer
pending or failed result blocks reuse. The source and sibling pins must remain
clean through completion. Source commit and artifact hashes are recorded only
after platform verification succeeds. An incomplete output directory is retained
for diagnosis but has no verified receipt; retry using a fresh output directory.

Use `--upload --tag EXISTING_VERSION_TAG` on a native local command to upload after
verification. Or upload an already verified directory without rebuilding:

```bash
python3 scripts/ci/release_artifacts.py upload linux --directory "$HOME/bokheim-builds/linux-01" --tag EXISTING_VERSION_TAG
python3 scripts/ci/release_artifacts.py upload android --directory "$HOME/bokheim-builds/android-01" --tag EXISTING_VERSION_TAG
```

Uploads verify package hashes, shared tests, the live server protocol, package version and existing version
tag. They create/update only a **draft release**, never tags or public assets.
A draft containing a different source receipt is rejected. The default release
repository is `JohanAnderssonOstling/bokheim-release`.

Web `--deploy` uses `servers/sync/deploy/deploy-web-remote.sh --dist DIRECTORY`.
It verifies the receipt and deploys those exact files, without a second build.
Existing `BOKHEIM_DEPLOY_HOST`, `BOKHEIM_DEPLOY_USER` and SSH settings apply.
To stage for manual installation instead, use that script with `--dist` and
`--stage-only`. Web does not upload a bundle to a GitHub release.

## Windows

After reporting local shared tests for the source revision:

```bash
gh workflow run desktop-release.yml --repo JohanAnderssonOstling/bokheim-release --ref SOURCE_REF -f publish=false
```

Windows runs its platform checks, builds the optimized executable, packages the
installer, single-file standalone EXE, portable ZIP and update ZIP, probes the extracted portable executable's
runtime metadata and records artifact hashes. The portable ZIP contains the same
complete runtime as the update ZIP; extract it into a writable folder and launch
`Bokheim.exe` without running an installer. Its accompanying helper, taxonomy and
speech models must stay together. Libraries and settings use normal per-user storage.
The standalone EXE embeds the portable runtime and extracts it automatically under
`%LOCALAPPDATA%\Bokheim\Standalone` on first launch. CI tests the launcher and
verifies both first and cached launches against the packaged app's metadata.
New Windows build receipts use protocol 2 and require both standalone downloads
and their checksums; existing protocol 1 receipts remain valid for earlier releases.
`publish=true` uploads to the version's existing tag draft after checking that
tag exists and matches the package version. The actual build commit is recorded separately. Windows platform checks also run on relevant
pull requests and pushes. No Linux, Android or web jobs are scheduled by these
two workflows. Updating their definitions does not restart or cancel old runs.

The manual shared-suite workflow remains available for operator use, but Windows
release jobs only read its commit status; they never schedule shared tests.
The Kobo workflow also reuses the local shared-suite status for its exact source
commit. The shared suite includes the Kobo installer's portable library tests;
Windows still runs its platform tests and builds the USB installer on a hosted
Windows runner. The Kobo ARM package is built locally and committed before those
installer builds; see `apps/kobo-installer/README.md`.

## Feed publication

Collect all three verified native platforms on the same version draft. Prepare
using `servers/updates/prepare-github-release.py --source-commit FULL_SHA` in place
of the legacy `--run-id`: it verifies individual platform receipts, package
hashes, Android signing identity, compatibility and the successful Windows run.
The old combined-run preparation mode remains supported for existing builds.

Review the prepared bundle, publish the GitHub draft, then use the existing
review/sign/publish command in `servers/updates/README.md`. Publication rechecks
the source gate and platform receipts. Signing keys stay on the trusted local
machine. Permission to upload releases/report commit statuses is the trust
boundary for local build attestations.

Before publishing native packages or deploying a verified web bundle, the pipeline probes the live login endpoint with a version-only protobuf envelope and no login fields. A protocol mismatch blocks publication until the matching server is deployed.

## Build packages without tests

For an explicitly requested build-only release, build locally with
`apps/desktop-gpui/scripts/build.py`, Gradle `assembleRelease`, and
`apps/web-gpui/scripts/build.sh`. These commands do not run the shared suite.
Dispatch the Windows, macOS, and Kobo workflows with `build_only=true` and
`publish=false` to produce hosted packages without tests. Download and review
the resulting workflow artifacts before uploading them to the version release.
This mode does not attest shared-test success; the normal verified publication
commands still require their existing test gates.

Completed Windows, macOS, and Kobo installer packages can be copied directly from workflow
artifacts to the existing draft with `distribute-release.yml`. Supply the
platform, successful packaging run ID, full build source SHA, and draft tag.
This avoids downloading and re-uploading hosted packages through the workstation.
It checks the build source, package version, and recorded hashes without running
tests or creating tags; existing assets must already contain identical bytes.

Windows packaging and native checks run concurrently. Publishing waits for both
jobs and requires the successful shared-suite status for the exact source commit.
