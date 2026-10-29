# Android reader

## F-Droid build mode

Use a fresh, isolated checkout with the committed `Cargo.lock`, Python, CMake,
Ninja, Git, Gradle 8.14.3, JDK 21, Android SDK 36, build-tools 36.0.0,
NDK 29.0.14206865, and Rust 1.99.1 with the `aarch64-linux-android` target.
The exact toolchain and APK normalization pins are in
`scripts/fdroid/toolchains.json`. From the isolated repository root:

```sh
python3 scripts/fdroid/prefetch.py \
  --pdfium-source-work ../.bokheim-native/pdfium \
  --speech-source-work ../.bokheim-native/speech
python3 scripts/fdroid/build.py \
  --pdfium-source-work ../.bokheim-native/pdfium \
  --speech-source-work ../.bokheim-native/speech
```

The preparation script downloads the pinned public companion sources to sibling
directories, selects the F-Droid repository configuration, and removes unused
Kobo packages, historical report archives, and binary test fixtures from this
isolated checkout. It refuses to overwrite existing developer repositories.
The pinned HtmlViewCore revision receives a small reading-line API compatibility
patch required by the committed reader; the patch and resulting file hashes are
recorded in `scripts/fdroid/sources.json`.

F-Droid mode builds PDFium 7961 and Sherpa-ONNX 1.13.8 from pinned source. It
links Sherpa against Microsoft's MIT-licensed ONNX Runtime 1.28.0 AAR, resolved
through Maven Central and checked against its pinned SHA-256. The Rustls Android
certificate verifier is compiled from the vendored upstream Kotlin source.
Alba's published model card, CC BY 4.0 dataset license, attribution, runtime
notices, and native source receipts are packaged in the APK assets.

The release APK is unsigned regardless of local production signing settings.
Google Play Services cannot be enabled. The updater is disabled in Java and
Rust, and the installer permission and update components are removed. Normal
Android storage access is retained. Output is under
`apps/desktop-gpui/android/build/outputs/apk/release/`.

Native caches and Rust outputs are separate from direct release builds.
`-Pbokheim.pdfiumSourceWork=/path/to/cache` and
`-Pbokheim.speechSourceWork=/path/to/cache` reuse verified native source work;
`-Pbokheim.nativeJobs=2` and `-Pbokheim.rustJobs=2` control compilation parallelism.
Run `verify-fdroid.py --apk <unsigned.apk> --aapt <SDK aapt> --output <report.json>`
from the Android directory to verify manifest policy, native receipts, API
architecture, 16 KiB page support, and packaged license notices.

Install `fdroidserver` separately and use `scripts/fdroid/scan.py` for source and
APK scanning. Scan the prepared sources before building, or use a separate source
snapshot, so generated libraries are not mistaken for bundled source inputs.
Android device testing was confirmed complete by the maintainer on 2026-10-26.
The pinned HtmlEngine and HtmlViewCore sources include GPL-3.0-or-later license
text and Cargo declarations. F-Droid maintainer review is still required.

For a fresh checkout, keep the source preparation away from existing developer
repositories. The following commands use the review branch until it is merged;
`gradle` must be Gradle 8.14.3. Install JDK 21, Python 3 and bubblewrap
(`bwrap`) on Linux, with unprivileged user namespaces enabled. Allow at least
30 GiB of free space for downloads and build outputs, in addition to the
installed Android SDK. The native work directories can live on a larger mounted
filesystem; use the same paths for prefetch and build:

```sh
mkdir -p /path/to/isolated-fdroid-build
cd /path/to/isolated-fdroid-build
git clone --branch fdroid/android-source-build-20261026 \
  https://github.com/JohanAnderssonOstling/bokheim.git bokheim
cd bokheim
export ANDROID_HOME="$HOME/Android/Sdk"
export JAVA_HOME=/usr/lib/jvm/java-21-openjdk
python3 scripts/fdroid/prefetch.py \
  --pdfium-source-work ../.bokheim-native/pdfium \
  --speech-source-work ../.bokheim-native/speech
python3 scripts/fdroid/build.py \
  --pdfium-source-work ../.bokheim-native/pdfium \
  --speech-source-work ../.bokheim-native/speech
```

The unsigned output is
`apps/desktop-gpui/android/build/outputs/apk/release/BokheimGpui-release-unsigned.apk`.
The preparation step changes this isolated checkout's settings and removes the
non-Android files described above; do not run it in your development checkout.

### Offline recipe and clean rebuild verification

`scripts/fdroid/metadata/se.bokheim.reader.gpui.yml` is the fdroiddata recipe.
It pins the application commit, Rustup source, Rust version, Android NDK and
upstream signing certificate. Its prebuild phase fetches dependencies; its build
phase runs the unsigned build with Gradle/Cargo downloads disabled and compiler
caches disabled. Native source archives are SHA-verified before compilation.
The draft is disabled until a developer-signed F-Droid APK is published at its
versioned `binary` URL. Publishing a release and submitting fdroiddata remain
separate steps.

For a network-enforced local check on Linux, run the build inside the provided
user/network namespace wrapper. It allows loopback for Gradle, rejects external
interfaces, probes the external network, and records the command result:

```sh
python3 scripts/fdroid/offline.py --report offline-network.json -- \
  python3 scripts/fdroid/build.py \
  --pdfium-source-work ../.bokheim-native/pdfium \
  --speech-source-work ../.bokheim-native/speech
```

For a clean rebuild, use another fresh isolated checkout and fresh Cargo/Android
outputs. Move aside the builder-owned PDFium `out/android-arm64` and speech
`build` directories before rebuilding; keep their pinned source archives and
toolchains. Never remove unrelated developer caches. Run the prefetch phase
before disabling networking. The second checkout should have a different parent path. The directory name
may differ: `build.py` mounts the app and its sibling dependencies under
`/mnt/bokheim-build` in a private mount namespace. This is required because
Cargo hashes absolute paths into external path dependency identities; Rust
source-path remapping alone does not stabilize them.

Prefetch includes Cargo's host dependencies, project dependency version
constraints, AAPT2 and the pinned lint runtime. If other Gradle builds are active,
use a separate `GRADLE_USER_HOME` for both prefetch and offline compilation to
avoid shared cache locks. Signing settings must be available in that home's
`gradle.properties` or through the `BOKHEIM_ANDROID_*` environment settings below.

The build maps Rust and speech source paths to stable paths, fixes the build
epoch, omits checkout-specific VCS metadata, and normalizes the unsigned APK with
the SHA-pinned upstream `reproducible-apk-tools` alignment script. Developer
signing preserves alignment and uses v2/v3 signatures (minimum Android API 26).
Speech debug data is stripped during linking so its ELF build ID does not retain
differences from debug paths that would survive a later strip operation.

After building the independent unsigned APK, install the verification tools in
a separate environment and compare it with the developer-signed reference:

```sh
python3 -m venv /path/to/fdroid-tools
/path/to/fdroid-tools/bin/pip install -r scripts/fdroid/requirements.txt
/path/to/fdroid-tools/bin/python scripts/fdroid/compare.py \
  --signed-apk /path/to/developer-signed.apk \
  --rebuilt-apk /path/to/clean-rebuilt-unsigned.apk \
  --output reproducibility.json
```

The comparison verifies the reference signature, copies that signature onto the
independent rebuild using `apksigcopier`, and verifies the result. Matching ZIP
entry contents alone is insufficient; the signature-copy check must pass.
F-Droid still needs to repeat this validation on its build infrastructure.

### Automated developer-signed releases

With the production signing settings described below configured, run this from
the repository root:

```sh
python3 scripts/fdroid/release.py
```

The command builds the committed `HEAD` in an isolated source snapshot, prepares
the companion sources, builds the unsigned F-Droid APK, checks its contents and
native receipts, signs it with the existing production keystore, and verifies
the certificate, alignment and unchanged APK contents. It finds JDK 21 and
Gradle 8.14.3 in the environment or standard local installation/cache locations.
Android SDK build-tools 36.0.0 and NDK 29.0.14206865 must be installed.
Uncommitted developer changes are excluded; select another committed source
with `--ref <commit-or-branch>`.

Both APKs, SHA-256 checksums, and JSON verification reports are saved in a new
timestamped directory under `artifacts/fdroid-release/`. The reports explicitly
record that clean-build reproducibility has not been verified. This command
creates local artifacts; official F-Droid submission and publication are separate.

The helper reads the existing `BOKHEIM_ANDROID_*` environment settings or user
Gradle properties, including private password files and a separate key password.
Passwords reach the signing tools through child-process environment variables,
never command-line password values. The keystore's certificate is checked against
`BOKHEIM_ANDROID_CERT_SHA256` or `ANDROID_CERT_SHA256` when either is configured.
The private key and passwords are never copied into the output directory.

To reuse a prepared checkout and native caches:

```sh
python3 scripts/fdroid/release.py --checkout /path/to/prepared/bokheim \
  --pdfium-source-work /path/to/pdfium-cache \
  --speech-source-work /path/to/speech-cache
```

To sign an existing verified unsigned build without rebuilding:

```sh
python3 scripts/fdroid/release.py --apk /path/to/BokheimGpui-release-unsigned.apk
```

Additional options include `--output-dir`, `--gradle`, `--sdk`, `--jobs`,
`--native-jobs`, and `--offline`. Existing output directories are preserved.
The normal F-Droid Gradle build continues to produce an unsigned APK without
requiring production signing credentials.

F-Droid can either sign the app with its own per-app key, or distribute an APK
signed with the existing developer key after verifying a reproducible build.
The latter preserves update compatibility with direct APKs using that key, an
unchanged application ID, and increasing version codes. The F-Droid-mode APK may
have different features from direct releases; the F-Droid rebuild must match
the developer-signed **F-Droid-mode** APK exactly, apart from signing data.
Use the clean-rebuild comparison above to verify reproducibility. See F-Droid's
[signing and reproducibility documentation](https://f-droid.org/docs/Reproducible_Builds/).

To sign the unsigned F-Droid-mode APK yourself, use the existing production
keystore and Android SDK `apksigner`; do not enable Gradle's local test signing.
The following prompts for passwords rather than putting them in shell history:

```sh
"$ANDROID_HOME/build-tools/36.0.0/apksigner" sign \
  --ks "$BOKHEIM_ANDROID_KEYSTORE" \
  --ks-key-alias "$BOKHEIM_ANDROID_KEY_ALIAS" \
  --alignment-preserved true --v1-signing-enabled false \
  --v2-signing-enabled true --v3-signing-enabled true \
  --out Bokheim-fdroid-signed.apk \
  apps/desktop-gpui/android/build/outputs/apk/release/BokheimGpui-release-unsigned.apk
"$ANDROID_HOME/build-tools/36.0.0/apksigner" verify \
  --verbose --print-certs Bokheim-fdroid-signed.apk
```

F-Droid needs the public APK and certificate fingerprint for verification; the
private signing key stays with the developer.

Build an optimized ARM64 release APK with a JDK supported by Gradle (for example JDK 21),
Android SDK 36, the configured Android NDK, and Rust's `aarch64-linux-android`
target installed:

```sh
gradle assembleRelease -Prust.ndkHome=/path/to/android/ndk
adb install -r build/outputs/apk/release/BokheimGpui-release.apk
```

Run these commands in this directory. `ANDROID_HOME` must point to the SDK.
On a memory-constrained build machine, set `CARGO_BUILD_JOBS=2` and pass
`--max-workers=2` to Gradle.

Release packaging requires `BOKHEIM_ANDROID_KEYSTORE`,
`BOKHEIM_ANDROID_STORE_PASSWORD`, `BOKHEIM_ANDROID_KEY_ALIAS`, and
`BOKHEIM_ANDROID_KEY_PASSWORD` in the build environment. Keep the keystore outside
the repository and preserve its identity across updates. These settings can also
be placed in `~/.gradle/gradle.properties`; environment values take precedence.
Use `BOKHEIM_ANDROID_STORE_PASSWORD_FILE` and optionally
`BOKHEIM_ANDROID_KEY_PASSWORD_FILE` to reference private password files instead
of storing password text in Gradle properties. The key password defaults to the
store password. For example, local user properties can contain:

```properties
BOKHEIM_ANDROID_KEYSTORE=/absolute/private/path/bokheim-release.jks
BOKHEIM_ANDROID_STORE_PASSWORD_FILE=/absolute/private/path/bokheim-release.password
BOKHEIM_ANDROID_KEY_ALIAS=bokheim
```

For an explicitly local
optimized test APK, pass `-Pbokheim.localSigning=true`; this uses the local debug
certificate while retaining the optimized Rust release build. This explicit
option overrides configured production signing for that invocation. It must not be
distributed as the production application. Unit tests require no signing key.

`versionName` comes from the desktop Cargo package. `versionCode` is
`major * 1,000,000 + minor * 1,000 + patch`, with minor/patch limited to 999 and
major limited to 2099. Stable versions must increase; publishing a correction
requires a new patch version. Existing version-code-1 installs can update only
when signed with the same certificate. Changing from a local debug certificate
to the production certificate does not create an install-compatible update.

Failed book opens return to the previous library page with a dismissible error
notification. Failed reader restoration clears the saved book so the next launch
does not repeat the failure. Native Back cancels both the opening overlay and the
reader's loading screen; the loading screen also keeps an on-screen Back button.

## Google Play build

The `play` build type produces an optimized ARM64 Google Play bundle while
`assembleRelease` continues to produce the direct-distribution APK:

```sh
gradle bundlePlay -Prust.ndkHome=/path/to/android/ndk
```

The bundle is `build/outputs/bundle/play/BokheimGpui-play.aab`. It uses the
release/upload key configured through `BOKHEIM_ANDROID_*` above. The bundle
task rejects `-Pbokheim.localSigning=true`. The project configures a 2 GiB Gradle
heap for finalizing the bundled native runtimes and offline voice. If user-level
Gradle properties override `org.gradle.jvmargs`, retain at least `-Xmx2g` for
bundle builds. Before enrolling in Play App Signing,
choose the app signing certificate deliberately: installations can update
between Play and direct APKs only with compatible signing identities and an
increasing version code. The upload key and the app signing key are separate
roles when Play manages signing. No upload or store publication runs as part of
this build.

The compiled Java `BuildConfig.PLAY_STORE` flag is read by Rust at startup;
intent extras and installer identity cannot change distribution policy. Play
never starts the direct-update coordinator, checks its feed, stages APKs or
shows its Settings controls. The native-to-Java updater entry point also
rejects all commands. The Play manifest removes the APK installation permission,
the private updater services/activity and its staging FileProvider.

Play also removes `MANAGE_EXTERNAL_STORAGE`, its launch prompt and the Manage
file access setting. New libraries, imports, downloads and synchronized libraries
use `files/libraries` in app-owned storage, even if an earlier direct installation
saved a shared-storage default. Registered libraries are never moved or deleted.
Existing shared folders need to be selected again with the system picker to
import copies; switching distributions does not grant raw filesystem access.

The system file picker opens individual books, and the folder picker imports
supported books into a managed library while preserving subfolders. The review
explains that originals stay in the selected folder. Play always uses the
provider-backed import path, including when an old all-files grant exists.
Completed imports are ordinary app-owned files and do not depend on the source
provider or a persistent URI grant when reopened. These local copies are removed
if the app is uninstalled; synchronization or separately retained originals are
needed to recover them.

Run both distributions' Android tests with:

```sh
gradle testPlayUnitTest testReleaseUnitTest
```

For a local device trial of Play storage/update behavior without a release key:

```sh
gradle assemblePlay -Pbokheim.localSigning=true
adb install -r build/outputs/apk/play/BokheimGpui-play.apk
```

Use a dedicated test profile/device if an existing installation has a different
signer; never uninstall an existing library just to bypass a signing mismatch.
Check file and folder imports (including a cloud document provider), cancellation,
downloads, reading after restart, and screen-locked audio. Local APK trials do
not test Play-delivered split APKs; validate the bundle through an internal Play
track before expanding the beta.

## Google Drive availability

Google Drive is disabled by default, including GitHub releases. These builds
omit the Google authorization SDK. Opt in for a custom build with
`-Pbokheim.googleDrive=true`, for example:

```sh
gradle assembleRelease -Pbokheim.googleDrive=true
```

The property enables both the Rust `google-drive` feature and the Google Play
services dependency. With it enabled, both Android distributions offer Google
Drive only when the Google Play services SDK reports that Play services is installed, enabled, valid, and up
to date. The launcher publishes this capability before opening the backend and
checks it again when the activity resumes. Unsupported devices hide the cloud
connection entry and reject provider operations, including requests through an
existing connection. Cached books and library metadata remain available.

Drive connects through Google Play services' native AuthorizationClient. Google
owns account selection and consent; a device browser is not required. Access
is separate from the Bokheim account. Background renewal is bound to the saved
Google account and never opens consent UI; revoked access requires reconnecting.
A rejected token is cleared from the SDK cache before renewal. Account identity
is checked against Google's verified user information before credentials are saved.

The Google Cloud project must register an Android OAuth client for each package
and signing certificate used by a distribution. That registration is deployment
configuration and is not generated by the app. Devices without usable Play
services have no browser fallback.

Authorization tests cover consent, cancellation, partial permissions, silent
renewal, account selection, activity destruction, and late callbacks.
Availability tests cover missing, disabled, outdated, invalid, and updating
services, settings changes, SDK failures, and a real SDK check without a Play
services package. Run them in both distributions with:

```sh
gradle -Pbokheim.googleDrive=true testReleaseUnitTest --tests '*GoogleDrive*Test' testPlayUnitTest --tests '*GoogleDrive*Test' -x buildRustRelease
cargo test -p cloud-storage --features google-drive --lib

# Disabled builds reject authorization and have no Google auth SDK dependency.
gradle -Pbokheim.googleDrive=false testReleaseUnitTest --tests '*GoogleDrive*Test' testPlayUnitTest --tests '*GoogleDrive*Test' -x buildRustRelease
```

## Launcher icon

Both launcher icon shapes use the Bokheim adaptive icon, with the shared Android
foreground artwork on the cream brand background. Android 13 and later also use
the mark for themed icons. Gradle stages
`apps/ui/design-tokens/assets/icon/bokheim-android-foreground-432.png` automatically;
regenerate that shared asset with `generate.py` when changing the brand icon.

## Book downloads in the app

The library's globe button and bookstore links open a GPUI browser surface inside
Bokheim. Its address/search field, back, forward, reload, close, progress, and
download feedback use the app's GPUI components, theme, and fonts. Only the webpage
is an Android WebView, positioned inside the rectangle measured by GPUI. The page
resizes with the window, safe areas, and keyboard. Store cookies and website
storage are shared between browser sessions.
HTTP and HTTPS stores are supported; unsupported URL schemes stay out of the browser.
The app's `bokheim_network_security_config.xml` permits HTTP and explicitly replaces
the network security configuration contributed by `rustls-platform-verifier`.
Android ignores `usesCleartextTraffic` when a network security configuration is present.
Magnet links and downloaded `.torrent` files start a Rust torrent transfer for
the selected library. All payload files download with the standard networking
settings. Completed content is moved into the library and found by the normal
scanner; the same files are then used for seeding.

Settings → Transfers controls automatic transfers on metered connections and is
off by default. Android's connectivity service supplies the connection cost.
Automatic uploads, cover/catalog updates and completed torrents' seeding wait
and resume when allowed. Opening and reading books, explicit downloads from the
library, catalog, OPDS or browser, and approved app updates always proceed on
metered connections. Pending user-requested downloads retain this exemption
across retries and restarts. Automatic suspension preserves the user's seeding
preference.

Torrent metadata, resume information, and book-to-torrent associations are kept
in the device's library database. Closing the browser leaves transfers running
while Bokheim is open, and reopening the app resumes saved transfers. Whole-folder
moves through Bokheim update the seeding location. A book's detail page offers
Stop/Keep seeding and Remove torrent; removing a torrent keeps its downloaded files.

Book downloads reuse the browser's cookies and user agent, follow HTTP redirects,
and stream into the selected library's hidden staging folder. Completed EPUB, PDF,
MOBI, AZW/AZW3, and M4B files enter the existing library placement/import flow.
Closing the browser lets pending downloads finish before the staging folder is
removed. Completed books are explicitly imported into the selected library on
both Android and desktop; completion notifications wait for the import result.
Failed or incomplete transfers never enter the library. Blob URLs and
downloads that require replaying a POST body are not supported by this adapter.

Download links that open a new window share the visible page's download handling,
cookies, and progress notifications.

Android and desktop use the same GPUI download notifications for HTTP and torrents.
On Android, the toast's × button cancels an active HTTP or torrent download and
removes its partial files. Completed and failed toasts simply dismiss.
Each transfer has its own progress bar, waiting state, completion, and error feedback.
A downloaded `.torrent` file hands its notification to the payload transfer;
completed torrents report whether seeding continues. Browser torrent progress
updates once per second. HTTP transfers keep updating after the browser closes;
active torrents appear again when it reopens. Android reserves space below the
native webpage while notifications are visible.

The mobile toolbar contains the address field and Close. Android's Back button
goes back through webpage history, then returns to the library. Pull down from
the top of a webpage and release to refresh; normal scrolling, sideways gestures
and pinch zoom do not trigger refresh. Page loading uses the GPUI progress bar.

Android Back buttons and gestures send a dedicated JNI request to the app's
state-based navigation handler; they never enter keyboard dispatch. The handler
dismisses one visible layer at a time: component menus/popovers and dialogs/sheets,
the embedded webpage browser, book loading, expanded audio controls, reader panels,
and library navigation. Dialog cancellation vetoes are honored. The library
switcher closes before its underlying page, and browsing follows the requested
breadcrumb trail even while a destination is loading. Browse roots return to
Home; only Home with nothing left to dismiss explicitly permits Android to exit.
Missing or stale keyboard focus cannot select a different navigation target.
Escape keeps its separate keyboard shortcuts.

GPUI touch surfaces deliver a stationary 500 ms hold as an explicit
`LongPressEvent` while the finger is still down. Reader and library context
actions bind `on_long_press` or `capture_long_press`. Moving past the touch slop,
adding a second finger, or cancelling the contact prevents a pending hold;
releasing a recognized hold does not produce a mouse click or page swipe.

In an EPUB, hold a word to select it, then drag either handle to adjust the
passage. Annotate opens its annotation editor, which stays above the keyboard
while entering a note. Back or a tap outside the selection dismisses it
before leaving the book. Handle drags claim raw touch input and never page or
open a link; word boundaries and EPUB locations come from the shared renderer.

The address field suggests previously typed sites and searches using the same
history format and matching as desktop. Tap a suggestion or select it with the
arrow keys and Enter. Suggestions reserve space above the native webpage and
are dismissed by Back or by returning focus to the page. History persists when
the browser reopens; redirects and clicked links do not fill the suggestion list.

Embedded challenge frames can load `about:srcdoc`, and browser windows accept
third-party cookies for sign-in and verification widgets. Cookies are flushed
after a page finishes loading as well as when the browser closes. The WebView's
default user agent is retained throughout the session.

Run the browser and download checks with:

```sh
gradle testReleaseUnitTest --tests se.bokheim.reader.gpui.BookBrowserTest -x buildRustRelease
cargo test -p library-opds --lib
cargo test -p desktop-gpui --lib --no-default-features --features mobile,reader-ui/native-audio book_browser
```

On a device, check a signed-in store, an EPUB/PDF download, redirect handling,
browser Back/close, and the downloaded book appearing in the selected library.

## Direct APK updates

`ApkInstaller` provides the OS handoff boundary. Before the explicit Install
action it checks the private staged path, exact signed size/hash, package identity,
version name and increasing Android version code. It grants the installer read
access only to `files/updates/staging/` through a non-exported FileProvider.
Update staging and its durable state are excluded from cloud backup and device
transfer. The installer requires Android's unknown-app-sources permission; opening
that settings page must also be an explicit user action.

The shared coordinator is connected to the conditional Settings row. Only Update
persists consent and stages downloads; Install opens Android's installer, and
Grant permission opens its unknown-app-sources settings. Returning from the
installer does not mark the update successful: the installed version on the next
launch is authoritative, and data activation waits for it. Cancellation retains
the staged package. The OS enforces package signing identity.

Before the backend opens, a non-exported `:update_migration` service prepares
private database copies in a fresh process. The caller waits for Binder death,
so helper SQLite handles and taxonomy globals are gone before promotion. A
30-minute timeout stops the helper; an absent result leaves live data untouched.
The data journal restores interrupted trials. Background work remains gated until
the first window initializes and commits the data generation. An explicit Restart
uses a private restart activity in another process; normal update polling never
restarts the application.

These adapters have host/unit coverage; the two-APK installation, helper process
lifecycle, process restart and crash recovery still require a device trial.
Android owns code installation: this data rollback does not silently downgrade a
badly behaving APK. Such code needs a corrected newer signed APK.

See Android's [versioning rules](https://developer.android.com/studio/publish/versioning).

## Read aloud

The first audiobook or read-aloud start while the app is visible offers an optional
battery optimization exemption. Continue opens Android's confirmation dialog;
Not now or cancellation allows playback and suppresses further automatic prompts.
Already-exempt apps skip the prompt. If the confirmation activity is unavailable,
Bokheim opens the system battery optimization list. Users can change the exemption
later in Android's battery settings. The exemption helps network access and CPU
wake locks during Doze; it does not remove every power restriction.

The bundled taxonomy is a standalone `taxonomy.sqlite3` APK asset, rather than
bytes embedded in the native library. Before opening library databases, the
native startup worker extracts it into `no_backup/bundled-taxonomy/<digest>/`.
Completed copies are reused, and revision directories preserve existing readers
across application updates. Reusing a completed copy skips extraction locking
and directory creation. Native startup only registers its path; it does not read
or hash the database. SQLite attaches the file read-only with a 512 KiB
page-cache target per connection. Downloaded taxonomy updates take precedence.
The isolated update migration service prepares the same resource before use.

Android enables the shared offline EPUB/PDF narration controls. The release and
debug APKs bundle Piper Alba, together with checksum-verified
ARM64 Sherpa-ONNX runtime libraries. Sherpa and ONNX Runtime load on the synthesis
worker when narration is first requested; they are packaged in the APK without
being startup link dependencies. The voice bundle retains only the English
eSpeak dictionary and English language definitions, together with shared
phoneme/voice resources.
Gradle prepares these with `shared/reader-speech/prepare_android.py`;
the first build requires downloading the runtime and voice unless they are cached.
The voice bundle is extracted from APK assets into private storage
excluded from Android backups by a background worker on first launch. A completed
copy is reused until that voice's bundled checksum changes. Extraction never blocks the
UI thread; a read-aloud request waits on the speech worker if the copy is still
running. Interrupted or failed copies are retried on the next launch or speech
request. The EPUB and PDF narration docks use Alba without a voice selector,
including when older preferences selected George. The model is loaded into the
synthesis engine when read-aloud starts. Alba uses two inference threads;
Android schedules those threads across the available cores. Later loads reuse the
extracted files and avoid RAM-backed model staging.
Playback uses Android's AAudio output through the shared narration player.
`SpeechService` registers this player with Android's media session and shows
notification/lock-screen controls with the book title. Play/pause, Previous passage,
Next passage, headset controls, and Stop act on the same Rust speech queue
and update the reader dock. Previous/next selects a paragraph or reading block;
pause/resume retains the exact PCM position and queued audio. EPUB narration
indexes readable items without pagination and loads their text on demand. Speech
starts with a short chunk of up to 120 characters, followed by sentence chunks
of up to 400 characters. The synthesis worker targets 25 seconds of queued audio
and requests upcoming text before inference, keeping at most three prepared
text chunks ahead. Playback begins with the first ready chunk. Navigation and
speed or pause-length changes discard queued work; setting changes restart
the current passage and preserve pause state. A foreground media service holds a CPU wake lock
during playback/preparation; pause, stop, focus loss, and headphone disconnection
release or pause the appropriate resources. Closing narration or the reader
removes the media session and notification. Notification intents cannot start
a new book or supply file paths.
The TTS navigation button opens narration when closed; tapping it again stops
playback, releases the session and voice model, and closes the dock. The dock's
play/pause button pauses or resumes while keeping the session available.
During a narration session, including pauses, the hardware volume buttons adjust
media volume instead of turning pages. Page-turn capture returns when narration
stops or the reader closes.
The synthesis worker refills audio from consumption notifications instead of periodic
polling. Stop releases the output device and keeps the voice warm for 60 seconds;
continued inactivity unloads the voice, and the next start reloads it. Natural completion
lets the final device buffers drain before closing output. Pause retains the voice,
source and queued PCM for exact resumption, while suspending the device stream.
Backends without hardware pause park the output callback until resume or shutdown.
Playback owns its PCM allocation directly and generates passage-gap silence without
allocating zero-filled audio buffers.
Media-session and notification state updates are signaled by Rust playback changes;
the service does not poll the native speech state periodically.
The current voice supports English books; other declared languages remain unavailable.

Validate the service lifecycle with:

```sh
gradle testReleaseUnitTest --tests se.bokheim.reader.gpui.SpeechServiceTest -x buildRustRelease
cargo test --release -p reader-speech --lib
```

On a device, verify the notification and lock-screen player during EPUB/PDF
narration, play/pause/skip controls, continued playback with the screen locked,
audio interruptions/headphone disconnection, and removal after Stop/closing.

## Audiobooks

The Android feature enables the shared audiobook reader. Open/import an M4B from
the library or an Android document provider. Android uses Media3/ExoPlayer for
decoding and playback; desktop builds retain Rodio and WSOLA. Audio codec support
on Android follows the device's Media3/platform decoders (AAC M4B is the primary
format).

`AudiobookService` owns the player and media session. Playback continues when the
screen turns off or another app opens. The notification/lock screen and headset
buttons control this same session. Audio focus and headphone disconnection are
handled by Media3. The reader's close button explicitly stops playback.

The media session publishes library cover artwork, the book title, author and
current chapter. `ChapterPlayer` exposes the current chapter's duration and
position to system controls while the decoder and persistence keep file-relative
positions. Timed skips can cross chapter boundaries; expanded system controls
also offer previous/next chapter actions. Seeks to another file are routed back
to the owning Rust playback session. Artwork arriving after playback starts
updates the existing session.

TTS publishes the same book artwork and credits with the reader's current
chapter, plus previous passage, play/pause and next passage. It has no invented
seek timeline or Stop notification action; stopping/closing narration removes
its media session and notification. Native card size, artwork presentation,
action placement and button dimensions are controlled by Android/OEM SystemUI.
The larger HTML card remains a design reference, not a custom lock-screen view.

The service sends position updates to Rust independently of the UI. Positions
are saved about every five seconds of progress and at pauses, seeks, and service
shutdown. Reopening a book uses its saved library position. Force-stopping the
process can lose the most recent unsaved interval.

Local audiobooks use an open file descriptor. Remote audiobooks use Media3 byte
reads backed by the backend's authenticated, seekable range reader, so opening
and seeking do not require a complete download or temporary playback file.
The service retains the source while background playback is active.

## Validation

```sh
cargo test --release -p audiobook-player --lib
gradle testReleaseUnitTest -x buildRustRelease
```

The Rust tests cover background persistence, stale seek events, source lifetime,
and the shared desktop playback/chapter logic. Robolectric service tests cover
restoring an initial position, metadata, seek/speed commands, replacement sessions,
cold-start commands, explicit stop, and rejection of file paths in external
service intents. Unit tests do not establish audible playback or device-specific
background behavior.

On a phone, verify an imported AAC M4B plays, seeks to chapters, changes speed
without changing pitch, continues with the screen locked, responds to notification
and headset controls, pauses on headphone disconnection/interruption, and resumes
at its saved position after closing and reopening the app.

## Native account screen and password managers

Settings opens `AccountDialog`, with native Android email/password fields for
sign-in, registration, verification, and password recovery. Requests cross JNI
to the existing Rust account client. Failed requests retain the fields;
successful sign-in commits the Autofill Framework session. Registration and
reset responses do not prove the displayed email/password pair, so those
sessions are cancelled; save confirmation waits for a successful sign-in.
Closing the dialog cancels autofill, and session IDs reject stale responses.
Credentials are not placed in Android saved-instance state.

The app declares a credential association with `https://app.bokheim.se`.
Before publishing that association, choose the production signing certificate.
Use the production APK signing certificate; never publish a local test certificate
as the production website's trusted identity.

From the repository root, generate the website file using its public SHA-256
fingerprint (repeat `--sha256` for multiple production signing certificates):

```sh
python3 apps/desktop-gpui/android/scripts/generate-assetlinks.py \
  --sha256 "$BOKHEIM_ANDROID_CERT_SHA256" \
  --output apps/web-gpui/assetlinks.json
```

Keep the generated file with the deployment sources. The web release build
copies it to `.well-known/assetlinks.json`. Deploy the Caddy route as well as the
web assets, then verify the HTTPS endpoint returns 200 and JSON without a
redirect. Until a certificate is configured, the endpoint should return 404.
This association declares credential sharing; it does not enable URL handling.

Run the native form checks using the release variant:

```sh
gradle -p apps/desktop-gpui/android testReleaseUnitTest \
  --tests se.bokheim.reader.gpui.AccountDialogTest -x buildRustRelease
```

The tests cover values filled into native fields, hints, pending requests,
errors, registration/verification/reset, successful commits, and dismissal.
They do not establish actual password-manager suggestions or save prompts.
On a device with Bitwarden selected as the autofill provider, test a vault entry
containing only `https://app.bokheim.se`, then filling, saving, failed login,
password updates, Back, rotation, and closing/reopening with an empty password.
Provider support for website/app associations must be verified; a provider may
still require the Android app URI on the same vault entry.

## PDFium

Gradle prepares checksum-verified Android PDFium through
`shared/pdfium/prepare.py` before compiling Rust, then packages `libpdfium.so`
under `lib/arm64-v8a/` in the APK. Python 3.11 or newer is required.
The Android feature enables PDF reading, search, selection, and cover extraction.
License notices are included in the shared reader. No PDFium source compilation
or runtime extraction is required. See `shared/pdfium/README.md` for offline setup.

## Startup timings

An optimized build can log startup stages without enabling dependency debug logs:

```sh
adb shell am start -W -n se.bokheim.reader.gpui/.MainActivity \
  --ez bokheim.startup_profile true
```

For repeated measurements, unlock a connected device and run:

```sh
python3 apps/desktop-gpui/scripts/benchmark-startup-android.py --serial DEVICE_SERIAL
```

The runner preserves app data, force-stops the process before each launch, and
records the first launch separately from five measured launches. Raw logcat,
Android launch timings, and stage medians/ranges go to
`target/startup-benchmark/android/results/`. It temporarily keeps the screen on
while charging and restores that setting on exit.

Native relative timings start after the initial JNI context/intent lookup.
Launch-to-log timings include earlier Android startup and use a measured device
clock offset. Frame callbacks measure GPUI rendering, not panel presentation.
Filesystem caches stay warm; these measurements do not simulate a device reboot.

Android holds library scan/sync/import maintenance until the first GPUI frame
callback. Foreground library commands remain available during startup; maintenance
resumes automatically after rendering, including when profiling is disabled.
Pass `--no-defer-maintenance` to compare immediate maintenance in the same APK
(`bokheim.startup_defer_maintenance=false` on a force-stopped launch).

Library traces include a library ID, resource construction, connection policy,
schema initialization, transaction commit, and actor startup. The runner saves
every event and per-library breakdowns in `runs.json`; `.sum_ms` metrics total a
stage across all libraries. Nested scopes overlap and must not be added together.
For a diagnostic comparison, pass `--defer-background --output ANOTHER_DIRECTORY`.
This holds backend asynchronous tasks, including library actors, until the first
GPUI frame; it does not measure when restored reader content becomes usable.

The same launch extra enables GPUI fork timings: `gpui_android_platform` records
text-system and system-font setup, `gpui_window_open` separates window creation,
the application root and its initial draw, and `gpui_renderer_*`,
`gpui_adapter_*` and `gpui_pipelines` break down GPU setup and pipeline creation.
GPUI's `since_start_ms` uses its own profiling origin; compare stage durations
across the GPUI and application profiles, not their relative timestamps.

Bokheim uses its bundled fonts and skips Android system-font loading by default.
System-font aliases resolve to bundled Raleway, registered before the first
window opens. This also omits Android's emoji and symbol fallback fonts, so
characters absent from the bundled fonts may render as missing glyphs.
To enable Android fonts for comparison, pass `--android-system-fonts` to the
benchmark runner, or add `--ez bokheim.android_system_fonts true` to a
force-stopped launch (`adb shell am start -S ...`).
The GPUI fork retains its default system-font loading; applications can opt out
with `AndroidPlatform::new_with_options` and `AndroidPlatformOptions`.

Android graphics initialization tries Vulkan before creating an OpenGL instance,
and retries with OpenGL if Vulkan initialization fails. The actual window surface
is validated instead of configuring a temporary 64×64 surface. Intermediate-path
pipelines unused by Android and the unimplemented surface pipeline are omitted.

GPU pipeline caching is enabled by default in private app storage under
`files/gpui-pipeline-cache`. Cache names include GPU/driver identity and shader
content; corrupt files are ignored and replaced atomically in the background.
Unchanged cache data is not rewritten. The first uncached launch still compiles
pipelines; subsequent launches can reuse them.

Use `--no-gpu-cache` with the benchmark runner to compare the same APK without
cache reuse, or `--force-opengl` to exercise OpenGL. These map to the cold-launch
extras `bokheim.gpu_pipeline_cache=false` and `bokheim.force_opengl=true`.
The Android OpenGL fallback uses grayscale text: Adreno GLES advertises
dual-source blending but can drop subpixel glyph draws. Vulkan retains subpixel
rendering on supported hardware.

## Input and swipe timings

Opt in for one launch with the `bokheim.input_profile` intent extra:

```sh
adb shell am force-stop se.bokheim.reader.gpui
adb shell am start -n se.bokheim.reader.gpui/.MainActivity --ez bokheim.input_profile true
adb logcat -s gpui | rg 'stage=(platform|frame)'
```

Swipe and tap the same surface several times. `event_age_ms` measures Android's
queue delay before Rust receives the event. Frame lines report synchronous
`dispatch_ms`, waiting for the next frame (`queue_ms`), view construction/layout
and prepaint (`layout_ms`), painting and frame completion, and renderer submission.
`first_input_to_submit_ms` includes the oldest coalesced input; the `latest` value
shows the age of the position actually drawn. These stop at renderer submission;
they do not measure display scanout. Tracing is off by default; force-stop and
launch normally to disable it after profiling. Desktop hosts can enable the same frame tracing with `RUST_LOG=input_frame=debug`.

## Historical F-Droid native dependency investigation

These notes record the initial investigation on 2026-10-26. The implementation
and validation described in the F-Droid sections at the start of this document
supersede the experimental status and original checklist below.

### Speech runtime

An Android ARM64 `libsherpa-onnx-c-api.so` was successfully compiled from
Sherpa-ONNX 1.13.8, commit
`11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf`, using NDK `29.0.13599879`, API 26,
and Microsoft's `com.microsoft.onnxruntime:onnxruntime-android:1.28.0` AAR from
Maven Central. The ONNX Runtime headers and `jni/arm64-v8a/libonnxruntime.so` were
extracted from the AAR and supplied through `SHERPA_ONNXRUNTIME_INCLUDE_DIR`
and `SHERPA_ONNXRUNTIME_LIB_DIR`. This avoids Sherpa's default download of a
different runtime from GitHub. Runtime version 1.28.0 is an investigated
alternative; device synthesis and release-parity tests are still required.

The important configuration was:

```text
ANDROID_ABI=arm64-v8a
ANDROID_PLATFORM=android-26
ANDROID_STL=c++_static
BUILD_SHARED_LIBS=ON
SHERPA_ONNX_ENABLE_C_API=ON
SHERPA_ONNX_ENABLE_JNI=OFF
SHERPA_ONNX_ENABLE_TTS=ON
SHERPA_ONNX_ENABLE_PORTAUDIO=OFF
SHERPA_ONNX_ENABLE_WEBSOCKET=OFF
SHERPA_ONNX_ENABLE_BINARY=OFF
SHERPA_ONNX_ENABLE_TESTS=OFF
SHERPA_ONNX_BUILD_C_API_EXAMPLES=OFF
SHERPA_ONNX_ENABLE_SPEAKER_DIARIZATION=OFF
SHERPA_ONNX_ENABLE_PYTHON=OFF
```

The build compiles Piper phonemization and eSpeak NG from pinned source too.
After fetching those source dependencies, a fresh CMake configuration also
succeeded with network access disabled, using `FETCHCONTENT_SOURCE_DIR_*`
overrides and `FETCHCONTENT_FULLY_DISCONNECTED=ON`.

F-Droid's [Anti-Vocale recipe](https://raw.githubusercontent.com/f-droid/fdroiddata/0a6edd297c5fa86add3b5e0346d8802b4d53a3de/metadata/com.antivocale.app.yml)
already builds this exact Sherpa revision. Its JNI-only flags must be adapted:
Bokheim loads the C API from Rust and requires `SHERPA_ONNX_ENABLE_C_API=ON`.
The [upstream F-Droid guide](https://github.com/k2-fsa/sherpa-onnx/blob/master/android/fdroid-reproducible-builds.md)
helps with pinning and reproducibility, but its C-API-disabled example is for
Java/JNI consumers.

F-Droid [allows some freely licensed Maven dependencies](https://f-droid.org/docs/Inclusion_Policy/#build-transparency-and-reproducibility).
Microsoft [documents using its Maven AAR from C/C++](https://onnxruntime.ai/docs/install/#cc-1).
This is a candidate ONNX Runtime sourcing route, not confirmation of F-Droid
acceptance for Bokheim. Building ONNX Runtime itself from source remains another
option if review requires it. Voice-model licensing and provenance must be
reviewed separately from the executable libraries.

### PDFium

F-Droid's [MJ PDF recipe](https://raw.githubusercontent.com/f-droid/fdroiddata/0a6edd297c5fa86add3b5e0346d8802b4d53a3de/metadata/com.gitlab.mudlej.MjPdfReader.yml)
builds PDFium from source with GN/Ninja. Its [source builder](https://gitlab.com/mudlej_android/mj_pdf_reader/-/blob/18f9a3b2f685cd26f7dc145b33f40f9e91a9c20f/build_dependencies/pdfium_source.py)
pins PDFium/depot_tools and applies the same family of shared-library and Android
patches as Bokheim's current binary provider.

The investigation checked PDFium branch 7961 at commit
`dd3583292697a0356cca1f64892892a92f0a2770`, with Chromium build dependency
`73c0fa98c5cf963c60ea685c57826fa7ba6253d8`. All four applicable patches from
`bblanchon/pdfium-binaries` commit `c6529b58791d142002f819beb46e370e668797d7`
applied with zero fuzz: `shared_library.patch`, `public_headers.patch`,
`clang_rt.patch`, and `android/build.patch`. The shared-library and Android
visibility patches provide the library and exported C API used by `pdfium-render`.
An isolated Android ARM64 source build with all four upstream patches succeeded
on 2026-10-26. It used pinned depot_tools commit
`13febbee9ece9e03df923f69d540afc63c6db93e`, PDFium's pinned Android toolchain
`30.0.14608247`, and GN/Ninja with V8/XFA and ThinLTO disabled. Compilation with
two jobs took 321 seconds; setup through build completion took about nine minutes.
The checkout, tools, caches, and outputs occupy about 9.1 GiB under
`.tmp/pdfium-source-build-7961/`.

The resulting `libpdfium.so` is 6,329,920 bytes, with SHA-256
`a743798ff9e35eff987354e9a15d0c8e154f9206537b276e651c30b4e9c64c88`.
Validation confirmed an ELF64 AArch64 library, all 461 C API exports from the
current Android binary, 16 KiB alignment on every load segment, and only
`libc.so`, `libdl.so`, and `libm.so` as shared dependencies. Source revisions,
resolved CIPD package IDs, patch hashes, license files, build logs, and the library
are saved in `artifacts/pdfium-source-build-20261026/`.

Repeat the isolated build and validation from the repository root:

```sh
python3 artifacts/pdfium-source-build-20261026/build.py
python3 artifacts/pdfium-source-build-20261026/validate.py
```

This experiment does not replace Gradle's current prebuilt input. Android device
rendering, text extraction, PDF links, APK integration, and clean-build
reproducibility remain to be tested before adopting the source-built library.

### Original implementation checklist

1. Add an explicit source-input mode to `shared/pdfium/prepare.py` and
   `shared/reader-speech/prepare_android.py`. Current preparation always fetches
   pinned prebuilt runtime archives and would overwrite separately built inputs.
2. Extend `shared/pdf-reader-core/build.rs` and the PDFium receipt format to
   validate source-built inputs. `PDFIUM_PREPARED_DIR` alone is insufficient:
   the current receipt requires the prebuilt archive's SHA-256. Record source
   commits, patches, toolchain, output hashes, and notices instead of claiming a
   locally built library came from that archive.
3. Let Gradle stage the selected source-built libraries and their notices. Keep
   production binary acquisition as a separate mode; do not substitute libraries
   silently or disable integrity checks for F-Droid.
4. Build the full F-Droid variant in a clean environment, test PDF rendering and
   TTS synthesis on-device, and then verify APK reproducibility if distributing
   with the existing developer signing key.
