# Bokheim GPUI for Web

This is the browser build of the same GPUI application used by the native
targets. It renders GPUI into a browser canvas and keeps browser-specific
storage behind the SQLite OPFS VFS used by the shared Rust backend.

Build the release bundle with:

```sh
./apps/web-gpui/scripts/build.sh
```

Then serve `apps/web-gpui/dist` with cross-origin isolation headers enabled.
For an isolated test bundle, set `BOKHEIM_WEB_OUTPUT_DIR` to an absolute
task-specific directory when building, then pass that path to the server's
`--directory` option. `--port` selects a separate local test port.
For local development, the included server configures the required headers:

```sh
python3 apps/web-gpui/scripts/serve.py
```

Before downloading the application module, the page checks browser version,
secure context, cross-origin isolation, shared WASM memory, workers, private
storage, and Web Locks. Unsupported browsers receive a short message with the
required browser version; technical diagnostics are logged to the console.
The version policy is Chromium desktop 109+, Chromium Android 148+, Firefox
114+, and Safari/iOS 16.4+; these are necessary version floors, not a claim that
every device running those versions has been validated. Unknown browser brands
are checked by capability. Media Session is optional.

The startup probe checks that a shared module worker can load. Tabs create the
dedicated database and CPU workers, which communicate directly with the shared
coordinator through MessagePorts. A tab lifetime lock detects a lost host; the
coordinator selects a replacement and restores database connections. An
exclusive database-worker lock prevents overlapping ownership. Interrupted
requests fail without automatically replaying writes; subscriptions reconnect.
Network requests execute asynchronously in the shared coordinator.

Startup checks:

```sh
node --test apps/web-gpui/scripts/compatibility.test.mjs
# Requires Playwright; optionally set CHROMIUM_PATH and FIREFOX_PATH:
node apps/web-gpui/scripts/compatibility.browser-test.mjs
```

After building an optimized release bundle, run the Chromium Settings async
regression from the repository root:

```sh
BOKHEIM_TEST_DIST=/absolute/path/to/release/dist \
  node apps/web-gpui/scripts/settings-async.browser-test.mjs
```

It serves that bundle with cross-origin isolation, delays database-worker
replies until after Settings opens, and checks that the account form still opens
without a GPUI borrow panic. `CHROMIUM_PATH` overrides the Chromium executable.

Browser Back dispatches the same `DismissSettings` action as native Android:
the focused page dismisses its modal/search or climbs one breadcrumb level,
the shell handles its overlays and global pages, and the application closes
the reader. An unhandled Back leaves Bokheim for the preceding browser page.
Sidebar selections do not create navigation history. Forward does not reopen
dismissed screens.

Compact browser windows and the mobile app use a shared Back button and current
page title instead of breadcrumbs. Mobile EPUB/PDF readers show that same header
above their reading controls. The header, controls, and search overlay the full
book viewport, so showing or hiding them does not repaginate the book. On Android,
the system status and navigation bars follow reader-control visibility and also
overlay the page.

The web shell arms a history guard after pointer or keyboard interaction and
returns to it with `history.forward()` while GPUI consumes Back. It does not
push replacement entries during `popstate`, which Chromium would mark as
skippable by the next toolbar Back. Reloads reuse an existing guard. Browser
history jumps can bypass it, and a directly opened tab with no preceding page
stays open when Back is unhandled. Run the Chromium/Firefox history regression:

```sh
node apps/web-gpui/scripts/browser-back.browser-test.mjs
# After building the release bundle, verify the GPUI callback in Firefox:
node apps/web-gpui/scripts/browser-back-app.browser-test.mjs
# Linux with xdotool and an isolated X11 display (for example Xvfb):
node apps/web-gpui/scripts/browser-back-toolbar.browser-test.mjs
```

The web build includes the shared audiobook player. Audiobooks open in a bottom
dock; clicking its title expands the full player, and Back minimizes it without
stopping playback. The session survives browsing and library switching while
retaining its originating library for progress saves. A browser audio element
plays M4B files, while Media Session exposes title, author, artwork, play/pause,
15-second rewind, 30-second forward, and position/seek controls where supported.
The browser and operating system choose the lock-screen layout. If automatic
playback is blocked, tap Play in the dock or full player. No PWA installation is required.

Playback position is saved from audio events (including external seeks and
pause), rather than relying on the GPUI redraw timer. Hiding the page also
requests a save. Explicitly closing the dock stops playback and releases its
source. Reload restores the active dock paused; Play opens the media source.
Closing or unloading the browser page stops playback; background
behavior and supported M4B codecs depend on the browser.

Local M4B playback uses a Blob URL backed by the stored file. Its worker
subscription retains the file generation until playback closes. Remote M4B
playback uses range requests through the audio service worker.

Document readers receive a source descriptor rather than complete file bytes.
Local sources use bounded range requests and retain their file generation for
the subscription lifetime. After a database worker replacement, reopen the
document to acquire a new source; its old capability is not restored.

Audio checks, from the repository root:

```sh
node --test apps/audiobook-player/src/web_audio.test.mjs
# Requires Playwright (or PLAYWRIGHT_MODULE) and a real M4B fixture:
node apps/audiobook-player/src/web_audio.browser-test.mjs /path/to/book.m4b
RUSTC_BOOTSTRAP=1 cargo check --release -p web-gpui --target wasm32-unknown-unknown
```

To check importing, dock controls with an import notification visible,
expand/minimize continuity, paused restoration after reload, explicit Play,
and durable Close in the complete served WASM app:

```sh
BOKHEIM_TEST_URL=http://127.0.0.1:4173 \
  node apps/web-gpui/scripts/audiobook-dock.browser-test.mjs /path/to/folder-containing-one-m4b
```

This test requires Playwright Firefox (optionally set `FIREFOX_PATH`) and a folder
containing exactly one M4B at least 90 seconds long. It uses a fresh browser context. Set `BROWSER=chromium`
and optionally `CHROMIUM_PATH` to run the same full-app check in Chromium.
Neither automated test proves which controls a physical phone displays on its
lock screen; that needs a device check after playback starts.

## PDF reading

The web build uses PDFium WASM with the regular GPUI PDF reader, including page
navigation, zoom, margin trimming, text selection, search, and saved positions.
PDFium loads lazily in one dedicated worker per tab. That worker owns PDFium's
JavaScript bindings and document handles; the UI receives page pixels, text
geometry, and search results through the application's shared Rust memory.
Closing a reader releases its document on the worker.

Library PDF thumbnails use the existing CPU worker and cover storage pipeline.
Each CPU worker lazily loads its own isolated PDFium instance when a PDF cover
is requested. The first page produces 300px browse and 600px high-density JPEG
covers.

After building the web bundle, test thumbnail extraction with:

```sh
node client/app/scripts/test-pdf-thumbnails.mjs
```

This requires Playwright Chromium and Python Pillow and accepts
`PLAYWRIGHT_MODULE`, `CHROMIUM_PATH`, and `PDF_THUMBNAIL_TEST_DIST` (the bundle
directory containing `cpu_worker_loader.js`). It checks lazy loading, recovery
from a blocked PDFium download, both cover sizes, and repeated extraction.

The current upstream WASM API opens complete byte buffers. PDFs therefore load
fully before reading, and their bytes occupy memory in both the Rust and PDFium
WASM instances. This is not PDF range streaming.

The build packages checksum-verified PDFium 7902 assets in the immutable release
bundle. The worker adapts a character-box output argument ordering bug in
`pdfium-render` 0.9.3; the Rust dependency is pinned so an upgrade cannot silently
invalidate that adapter. Native PDFium loading remains unchanged.

Run the browser worker test from the repository root:

```sh
bash apps/web-gpui/scripts/test-pdf-worker.sh
```

It builds a small harness using the production worker and PDF session code, then
checks two page rasters, selection, search, repeated open/close, invalid files,
UI responsiveness, and worker initialization failure. It requires Playwright
Chromium; `PLAYWRIGHT_MODULE` and `CHROMIUM_PATH` override their locations.
`PDF_TEST_TARGET` and `PDF_TEST_DIST` select isolated build and output directories.

To test the regular reader in a served production bundle (requires Python Pillow):

```sh
node apps/web-gpui/scripts/pdf.browser-test.mjs
```

This uses a disposable browser context and generated PDF to check import, page
colors, selection/copy, search, navigation, saved-position restoration, and close.
It defaults to Firefox (`FIREFOX_PATH`); `BROWSER=chromium` selects Chromium.
`BOKHEIM_TEST_URL` overrides the local server URL.

## Remote EPUB reading

Undownloaded EPUBs open through authenticated HTTP byte ranges. The backend
returns a transient remote-source descriptor; the EPUB parser reads ZIP entries
on demand on a GPUI background worker. Downloaded EPUBs continue to use local
bytes. Each open remote reader retains at most 64 cached blocks of 64 KiB (4 MiB),
and closing it discards that cache. Sequential cache misses grow HTTP batches
from 64 to 128 to at most 256 KiB; non-contiguous seeks reset them to 64 KiB.
Batches stop before already cached blocks and at EOF. Nearby bytes fetched in a
batch share the same 4 MiB cache budget and can serve subsequent reads.
Streaming does not mark the book downloaded; use Download to make the complete
book available offline.

Opening fetches the first 64 KiB (or the whole file if smaller), using its
Content-Range total as the file length and caching the validated prefix. The
worker response carries that prefix once; subsequent requests carry only the
file identity and requested bounds. No separate HEAD request is needed.

The server must support byte ranges, Content-Range, and an ETag matching
the book's content hash. Range reads reject changed identities or wrong bounds.
File descriptors contain only content identity and length. The backend supplies
current credentials for each range and refreshes/retries once after HTTP 401,
including the initial prefix request.
The UI thread never performs synchronous network reads. Chapter navigation,
TOC loading, and opening image resources acquire file data on parser workers;
failed chapter reads leave the current page available for retry.

The UI build uses shared WebAssembly memory and a GPUI parser-worker pool. Its
standard library and C dependencies must be built with atomics and bulk-memory
support, with the TLS exports required by wasm-bindgen; `scripts/build.sh`
supplies these flags. Backend workers remain
separate instances. The browser must support `Atomics.waitAsync`. The file bridge
uses that API to serve parser requests on the existing UI event loop, which
forwards authenticated byte reads to the backend. It creates no nested network
workers and transfers no JS objects between parser threads.

Validate the actual Rust file source across browser parser workers with:

```sh
bash client/app/scripts/test-epub-range-reader.sh
```

Set `PLAYWRIGHT_MODULE` and `CHROMIUM_PATH` if Playwright or Chromium are installed
outside their default locations.

The test builds a small shared-memory WASM harness from the production file
source. It checks seeking, cache reuse across workers, credential changes, and
rejection/cancellation of full, stale, misaligned, truncated, and oversized
responses. It also checks that slow reads keep the UI event loop responsive,
that network loss and expired credentials report errors and recover, and that
the harness discards openings and reads completed after closure. The HTTP session test below separately exercises the
real authentication refresh endpoint for both metadata and range requests.

```sh
cargo test --release -p app --no-default-features --features watcher --lib remote_file_refreshes_expiring_and_rejected_tokens_over_http
```

Rust's `rust-src` component and a matching wasm-bindgen CLI are
required. `REMOTE_FILE_TEST_TARGET`, `REMOTE_FILE_TEST_PKG`, and `WASM_BINDGEN`
can override their default locations.

To also benchmark actual EPUB opening and chapter reads in WASM parser workers:

```sh
REMOTE_FILE_BENCHMARK=1 bash client/app/scripts/test-epub-range-reader.sh
```

The benchmark injects 80 ms per HTTP request and reports elapsed time, request
count, and bytes transferred over three trials. `REMOTE_FILE_BENCHMARK_LATENCY_MS`
changes that delay; `REMOTE_FILE_BENCHMARK_REPORT` saves the detailed JSON report.
See [the benchmark fixture and recorded comparison](../../client/library-backend/crates/book-access/tests/remote-file-browser/BENCHMARK.md).

## Tabs across web deployments

Each immutable release bundle has its own coordinator URL. Tabs running the same
bundle share that coordinator. A newer bundle waits for the user to close older
tabs before it can acquire database ownership and migrate local data. Existing
tabs keep running; deployment retains their immutable assets for later requests.

Run the full-app Chromium regression after an optimized release build:

```sh
BOKHEIM_TEST_DIST=/path/to/release node apps/web-gpui/scripts/deployment.browser-test.mjs
```

It serves the compiled application under two deployment identities and checks
that the older tab keeps running, the newer tab reports the storage conflict,
and closing the older tab allows the newer deployment to open on reload.
This checks bundle isolation and ownership, not historical schema migration.
`PLAYWRIGHT_MODULE` and `CHROMIUM_PATH` select the browser test installation.

## Authentication form checks

The web Settings page opens a real HTML authentication dialog so password
managers can discover the username and password fields. It submits through the
existing Rust account client.

From the repository root, run the standalone Chromium/Firefox form checks and,
after building the optimized web release, the full-app Firefox check:

```sh
node apps/browser/web/authentication.browser-test.mjs
node apps/web-gpui/scripts/authentication.browser-test.mjs
```

These scripts accept `PLAYWRIGHT_MODULE`, `CHROMIUM_PATH` (standalone test), and
`FIREFOX_PATH` to select installed tooling. The full-app test intercepts login
requests and uses dummy credentials. These checks cover DOM autofill values and
submission, not a password manager extension's actual autofill or save prompts;
verify those separately with the extension enabled on the intended site origin.

## Local EPUB reader regression

With the optimized web app served locally, run:

```sh
node apps/web-gpui/scripts/epub-local.browser-test.mjs
```

This imports a generated EPUB through the directory chooser, verifies that its
chapter renders through a local book capability without a prefetched
remote prefix, then closes and reopens it. It uses Firefox and Python Pillow;
`PLAYWRIGHT_MODULE`, `FIREFOX_PATH`, and `BOKHEIM_TEST_URL` select the tooling and
local server. `EPUB_TEST_SCREENSHOT` optionally records the rendered reader.
