# Local releases and Windows Actions

Shared tests, Linux/AppImage, Android/APK and web/Wasm run directly on the
workstation. GitHub Actions builds and checks Windows. GitHub also stores native
release downloads; it does not need to build the locally produced packages.
Web deploys directly over SSH to our server.

## Source and tools

Use a clean release checkout, with clean sibling GPUI-Fork, HtmlEngine and
HtmlViewCore checkouts at the pins in `.github/workflows/desktop-release.yml`.
Keep this separate from the Actions runner's `_work` directory and build caches.
All platforms must use the same source commit. Do not create or move a version
tag as part of building; publication requires an existing version tag. The tag may point to older source;
the build receipts record the actual tested commit.

Install Rust 1.95.0, Python 3.11+, gh, and the platform build tools. The local
command selects Rust through its environment, without changing your default
toolchain. All builds/tests use optimized release profiles.

- Shared suite: PostgreSQL tools (`initdb`, `pg_ctl`, `createdb`), ripgrep and
  native build libraries. The suite creates an isolated temporary database when
  `SYNC_E2E_DATABASE_URL` is absent.
- Linux: native GPUI libraries, linuxdeploy and appimagetool in PATH (or set
  `LINUXDEPLOY` and `APPIMAGETOOL`). Release tooling pins remain in the Windows
  workflow environment. Build in a local Ubuntu 22.04 environment/container for
  the GLIBC 2.35 baseline. Newer host builds are accepted only if both the
  AppImage runtime and all packaged ELF files pass the same compatibility check.
- Android: Java 21, Gradle 8.14.3, Android SDK 36/build-tools 36.0.0, NDK
  29.0.13599879 and Rust target `aarch64-linux-android`. Configure production
  signing in private Gradle user properties or `BOKHEIM_ANDROID_*` environment
  variables; password `_FILE` settings are supported. Set `ANDROID_SERIAL` to a
  dedicated ARM64 test device and `ANDROID_CERT_SHA256` to the production signer.
  Verification installs the release and instrumentation APKs on that device.
- Web: Node, Rust `wasm32-unknown-unknown` and `rust-src`, Binaryen `wasm-opt`,
  wasm-bindgen-cli matching Cargo.lock, and Playwright 1.61.1 with Firefox and
  Chromium installed. Set `PLAYWRIGHT_MODULE` to its absolute `index.mjs` path.

## Local commands

```bash
# Optional: run/report shared tests first. Platform commands reuse this success.
RUSTUP_TOOLCHAIN=1.95.0 RUSTC_BOOTSTRAP=1 python3 scripts/ci/shared-tests.py --report JohanAnderssonOstling/bokheim-release

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

Uploads verify package hashes, shared tests, package version and existing version
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
installer and update ZIP, probes runtime metadata and records artifact hashes.
`publish=true` uploads to the version's existing tag draft after checking that
tag exists and matches the package version. The actual build commit is recorded separately. Windows platform checks also run on relevant
pull requests and pushes. No Linux, Android or web jobs are scheduled by these
two workflows. Updating their definitions does not restart or cancel old runs.

The manual shared-suite workflow remains available for operator use, but Windows
release jobs only read its commit status; they never schedule shared tests.
The separate Kobo workflow is outside this desktop/mobile/web change.

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
